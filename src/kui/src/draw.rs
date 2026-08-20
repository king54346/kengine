//! 屏幕空间的绘制列表。
//!
//! 攒一帧的 UI 图元，输出成顶点数组加若干批次，渲染器直接上传就能画。
//!
//! # 一张纹理画完一整个界面
//!
//! 字形图集左上角留了一块**纯白**（[`GlyphAtlas::white_uv`]）。纯色矩形
//! 采样那一点，文字采样自己的字形——两者用同一张纹理，于是整个界面
//! 通常只要**一次绘制**。这是 Dear ImGui 的老办法，省下的不是显存而是
//! 绘制调用：UI 图元又小又多，每个矩形一次 draw 的话，瓶颈全在提交上。
//!
//! # 圆角和描边在着色器里算
//!
//! 顶点上带着「这个矩形的中心、半尺寸、圆角半径、描边宽度」，片元着色器
//! 用一个圆角矩形的 SDF 决定覆盖率。好处是**圆角不增加顶点**——
//! 用三角扇去逼近圆角的话，一个按钮就要几十个顶点，而且半径一变就得重建。
//!
//! # 裁剪用剪刀矩形
//!
//! 裁剪不进着色器，走 `set_scissor_rect`：滚动区里的内容动辄上千个图元，
//! 逐片元判断远不如让光栅化阶段直接跳过。代价是裁剪区变化要断批。

use kfont::{FontStack, GlyphAtlas, TextLayout};
use kmath::{Vec2, Vec4};

/// 一个屏幕空间矩形，单位是**逻辑像素**，原点在左上角，y 向下。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// 左上角。
    pub min: Vec2,
    /// 右下角。
    pub max: Vec2,
}

impl Rect {
    /// 由左上角与尺寸构造。
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            min: Vec2::new(x, y),
            max: Vec2::new(x + width, y + height),
        }
    }

    /// 由两个角点构造，自动取最小/最大。
    pub fn from_corners(a: Vec2, b: Vec2) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    /// 宽高。
    pub fn size(&self) -> Vec2 {
        (self.max - self.min).max(Vec2::ZERO)
    }

    /// 中心点。
    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    /// 宽或高为零。
    pub fn is_empty(&self) -> bool {
        self.max.x <= self.min.x || self.max.y <= self.min.y
    }

    /// 点在矩形内（含左上边界，不含右下边界）。
    ///
    /// 边界的取舍是有讲究的：两个相邻的控件共享一条边时，
    /// 两边都算「含」会让那一列像素同时命中两个控件。
    pub fn contains(&self, point: Vec2) -> bool {
        point.x >= self.min.x
            && point.y >= self.min.y
            && point.x < self.max.x
            && point.y < self.max.y
    }

    /// 交集。不相交时返回一个空矩形。
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        }
    }

    /// 四边各向内收缩。
    pub fn shrink(&self, amount: f32) -> Self {
        Self {
            min: self.min + Vec2::splat(amount),
            max: (self.max - Vec2::splat(amount)).max(self.min + Vec2::splat(amount)),
        }
    }
}

/// UI 的一个顶点，对应 `ui.wgsl` 的顶点输入。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiVertex {
    /// 屏幕坐标（逻辑像素）。
    pub position: [f32; 2],
    /// 纹理坐标。
    pub uv: [f32; 2],
    /// 线性 RGBA，直接乘在采样结果上。
    pub color: [f32; 4],
    /// 所属矩形：`[中心 x, 中心 y, 半宽, 半高]`。圆角 SDF 用它。
    pub rect: [f32; 4],
    /// `[圆角半径, 描边宽度]`。描边宽度为 0 表示实心。
    pub params: [f32; 2],
}

/// 一批可以一次画完的图元。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawBatch {
    /// 在索引数组里的区间。
    pub first_index: u32,
    /// 索引数量。
    pub index_count: u32,
    /// 用哪张纹理。`None` 表示用字形图集。
    pub texture: Option<kcore::uuid::Uuid>,
    /// 剪刀矩形。
    pub clip: Rect,
}

/// 一帧的 UI 绘制列表。
#[derive(Debug, Clone, Default)]
pub struct DrawList {
    vertices: Vec<UiVertex>,
    indices: Vec<u32>,
    batches: Vec<DrawBatch>,
    /// 裁剪栈。栈顶是当前生效的裁剪区。
    clips: Vec<Rect>,
    /// 整个界面的尺寸，也是最外层的裁剪区。
    screen: Vec2,
    /// 当前批次用的纹理。
    texture: Option<kcore::uuid::Uuid>,
    /// 当前批次的起始索引。
    batch_start: u32,
    /// 图集里那块纯白的 UV。纯色图元全都采样这一点。
    white_uv: [f32; 2],
}

impl DrawList {
    /// 开一帧。会清空上一帧的内容，但保留已分配的容量。
    ///
    /// `white_uv` 来自 [`GlyphAtlas::white_uv`]：纯色图元采样那一点，
    /// 于是纯色和文字共用一张纹理，整个界面一次绘制画完。
    pub fn begin(&mut self, screen: Vec2, white_uv: [f32; 2]) {
        self.vertices.clear();
        self.indices.clear();
        self.batches.clear();
        self.clips.clear();
        self.screen = screen;
        self.texture = None;
        self.batch_start = 0;
        self.white_uv = white_uv;
    }

    /// 纯色图元用的 UV：四个角都取白点，采样结果恒为白。
    fn solid_uv(&self) -> [[f32; 2]; 2] {
        [self.white_uv, self.white_uv]
    }

    /// 收尾。把最后一批提交掉。**画完必须调，否则最后一批会丢。**
    pub fn end(&mut self) {
        self.flush();
    }

    /// 顶点。
    pub fn vertices(&self) -> &[UiVertex] {
        &self.vertices
    }

    /// 索引。
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// 批次，按提交顺序。
    pub fn batches(&self) -> &[DrawBatch] {
        &self.batches
    }

    /// 一个图元都没有。
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// 当前生效的裁剪区。
    pub fn clip(&self) -> Rect {
        self.clips
            .last()
            .copied()
            .unwrap_or(Rect::new(0.0, 0.0, self.screen.x, self.screen.y))
    }

    /// 压一层裁剪。新的裁剪区会与外层**求交**——
    /// 不求交的话，子控件能画到父容器外面去，滚动区就漏了。
    pub fn push_clip(&mut self, rect: Rect) {
        let clipped = rect.intersect(&self.clip());
        self.flush();
        self.clips.push(clipped);
    }

    /// 弹一层裁剪。
    pub fn pop_clip(&mut self) {
        self.flush();
        self.clips.pop();
    }

    // ───────────────────────── 图元 ─────────────────────────

    /// 实心矩形。
    pub fn rect(&mut self, rect: Rect, color: Vec4) {
        self.rounded_rect(rect, 0.0, color);
    }

    /// 圆角矩形。
    pub fn rounded_rect(&mut self, rect: Rect, radius: f32, color: Vec4) {
        let uv = self.solid_uv();
        self.quad(rect, radius, 0.0, color, None, uv);
    }

    /// 只画边框，中间镂空。
    pub fn border(&mut self, rect: Rect, radius: f32, thickness: f32, color: Vec4) {
        if thickness <= 0.0 {
            return;
        }
        let uv = self.solid_uv();
        self.quad(rect, radius, thickness, color, None, uv);
    }

    /// 贴一张图。`uv` 是 `[[u0, v0], [u1, v1]]`。
    ///
    /// 会另起一批：换纹理必须断批。
    pub fn image(&mut self, rect: Rect, texture: kcore::uuid::Uuid, uv: [[f32; 2]; 2], tint: Vec4) {
        self.quad(rect, 0.0, 0.0, tint, Some(texture), uv);
    }

    /// 画一段已经排好版的文字。
    ///
    /// `origin` 是文本框的**左上角**，不是基线——排版结果里的 y 已经是
    /// 相对左上角的基线位置了。
    ///
    /// 字形必须**先**通过 [`FontStack::ensure_glyph`] 进过图集；这里只查不建，
    /// 查不到的字形直接跳过（画不出来好过画错）。
    pub fn text(
        &mut self,
        origin: Vec2,
        layout: &TextLayout,
        fonts: &FontStack,
        atlas: &GlyphAtlas,
        size_px: f32,
        color: Vec4,
    ) {
        let atlas_size = atlas.size();
        for glyph in &layout.glyphs {
            // 字符到字形号的映射只能问字体要。自己拿 `c as u16` 当字形号
            // 会取到别的字，画出来是一片乱码而且不报错。
            let Some(key) = fonts.glyph_key(glyph.c, size_px) else {
                continue;
            };
            let Some(entry) = atlas.peek(&key) else {
                continue;
            };
            if entry.is_blank() {
                continue;
            }

            // 字形位图挂在基线上：左上角 = 笔位置 + 左偏移，基线 - 上偏移。
            // bearing_y 的符号搞反的话整行字会掉到基线下面。
            let min = Vec2::new(
                origin.x + glyph.x + entry.bearing_x,
                origin.y + glyph.y - entry.bearing_y,
            );
            let rect = Rect {
                min,
                max: min + Vec2::new(entry.rect[2] as f32, entry.rect[3] as f32),
            };
            let uv = entry.uv(atlas_size);
            self.quad(
                rect,
                0.0,
                0.0,
                color,
                None,
                [[uv[0], uv[1]], [uv[2], uv[3]]],
            );
        }
    }

    /// 推一个四边形。所有图元最终都走这里。
    fn quad(
        &mut self,
        rect: Rect,
        radius: f32,
        border: f32,
        color: Vec4,
        texture: Option<kcore::uuid::Uuid>,
        uv: [[f32; 2]; 2],
    ) {
        if rect.is_empty() {
            return;
        }
        // 完全被裁掉的图元连顶点都不生成。滚动区里绝大多数内容是看不见的，
        // 全部生成顶点再靠剪刀丢弃，白白多传几十倍的数据。
        if rect.intersect(&self.clip()).is_empty() {
            return;
        }
        if texture != self.texture {
            self.flush();
            self.texture = texture;
        }

        let half = rect.size() * 0.5;
        let center = rect.center();
        // 圆角半径超过半尺寸就成了胶囊，再大就该退化成圆。夹一下，
        // 不夹的话 SDF 会给出负的内圆半径，边缘出现一圈亮线。
        let radius = radius.min(half.x).min(half.y).max(0.0);
        let shared = ([center.x, center.y, half.x, half.y], [radius, border]);

        let base = self.vertices.len() as u32;
        let color = color.to_array();
        for (corner, texcoord) in [
            (rect.min, [uv[0][0], uv[0][1]]),
            (Vec2::new(rect.max.x, rect.min.y), [uv[1][0], uv[0][1]]),
            (rect.max, [uv[1][0], uv[1][1]]),
            (Vec2::new(rect.min.x, rect.max.y), [uv[0][0], uv[1][1]]),
        ] {
            self.vertices.push(UiVertex {
                position: corner.to_array(),
                uv: texcoord,
                color,
                rect: shared.0,
                params: shared.1,
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// 把当前累积的索引封成一批。没有新索引时什么都不做。
    fn flush(&mut self) {
        let end = self.indices.len() as u32;
        if end == self.batch_start {
            return;
        }
        self.batches.push(DrawBatch {
            first_index: self.batch_start,
            index_count: end - self.batch_start,
            texture: self.texture,
            clip: self.clip(),
        });
        self.batch_start = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> Vec4 {
        Vec4::new(1.0, 0.0, 0.0, 1.0)
    }

    /// 假的白点 UV。真实值来自图集，这里只要一个固定值就够。
    const WHITE: [f32; 2] = [0.001, 0.001];

    fn list() -> DrawList {
        let mut list = DrawList::default();
        list.begin(Vec2::new(800.0, 600.0), WHITE);
        list
    }

    #[test]
    fn a_rect_is_two_triangles() {
        let mut list = list();
        list.rect(Rect::new(10.0, 10.0, 100.0, 50.0), red());
        list.end();

        assert_eq!(list.vertices().len(), 4);
        assert_eq!(list.indices().len(), 6);
        assert_eq!(list.batches().len(), 1);
    }

    #[test]
    fn corners_are_in_clockwise_order_from_top_left() {
        let mut list = list();
        list.rect(Rect::new(0.0, 0.0, 10.0, 20.0), red());
        list.end();

        let p: Vec<[f32; 2]> = list.vertices().iter().map(|v| v.position).collect();
        assert_eq!(p, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 20.0], [0.0, 20.0]]);
    }

    #[test]
    fn an_empty_rect_draws_nothing() {
        let mut list = list();
        list.rect(Rect::new(10.0, 10.0, 0.0, 50.0), red());
        list.rect(Rect::new(10.0, 10.0, 100.0, -5.0), red());
        list.end();
        assert!(list.is_empty());
    }

    #[test]
    fn same_texture_stays_in_one_batch() {
        // UI 图元又小又多。一个矩形一次绘制的话，瓶颈全在提交上。
        let mut list = list();
        for i in 0..50 {
            list.rect(Rect::new(i as f32 * 10.0, 0.0, 8.0, 8.0), red());
        }
        list.end();

        assert_eq!(list.batches().len(), 1, "同一张纹理不该断批");
        assert_eq!(list.indices().len(), 50 * 6);
    }

    #[test]
    fn switching_texture_splits_the_batch() {
        let mut list = list();
        let a = kcore::uuid::Uuid::new_v4();
        let b = kcore::uuid::Uuid::new_v4();

        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), red());
        list.image(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            a,
            [[0.0, 0.0], [1.0, 1.0]],
            red(),
        );
        list.image(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            b,
            [[0.0, 0.0], [1.0, 1.0]],
            red(),
        );
        list.end();

        assert_eq!(list.batches().len(), 3);
        assert_eq!(list.batches()[0].texture, None);
        assert_eq!(list.batches()[1].texture, Some(a));
        assert_eq!(list.batches()[2].texture, Some(b));
    }

    #[test]
    fn batches_cover_every_index_exactly_once() {
        let mut list = list();
        let texture = kcore::uuid::Uuid::new_v4();
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), red());
        list.image(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            texture,
            [[0.0, 0.0], [1.0, 1.0]],
            red(),
        );
        list.push_clip(Rect::new(0.0, 0.0, 5.0, 5.0));
        list.rect(Rect::new(0.0, 0.0, 4.0, 4.0), red());
        list.pop_clip();
        list.end();

        let mut covered = 0;
        let mut cursor = 0;
        for batch in list.batches() {
            assert_eq!(batch.first_index, cursor, "批次之间有缝或者重叠");
            cursor += batch.index_count;
            covered += batch.index_count as usize;
        }
        assert_eq!(covered, list.indices().len());
    }

    #[test]
    fn a_nested_clip_intersects_with_the_outer_one() {
        // 不求交的话子控件能画到父容器外面，滚动区就漏了。
        let mut list = list();
        list.push_clip(Rect::new(100.0, 100.0, 200.0, 200.0));
        list.push_clip(Rect::new(0.0, 0.0, 1000.0, 1000.0));

        let clip = list.clip();
        assert_eq!(clip.min, Vec2::new(100.0, 100.0));
        assert_eq!(clip.max, Vec2::new(300.0, 300.0));
    }

    #[test]
    fn popping_restores_the_outer_clip() {
        let mut list = list();
        let outer = list.clip();
        list.push_clip(Rect::new(10.0, 10.0, 20.0, 20.0));
        list.pop_clip();
        assert_eq!(list.clip(), outer);
    }

    #[test]
    fn fully_clipped_primitives_produce_no_vertices() {
        // 滚动区里绝大多数内容看不见。全生成顶点再靠剪刀丢，
        // 白白多传几十倍数据。
        let mut list = list();
        list.push_clip(Rect::new(0.0, 0.0, 50.0, 50.0));
        list.rect(Rect::new(500.0, 500.0, 20.0, 20.0), red());
        list.pop_clip();
        list.end();

        assert!(list.is_empty(), "被裁掉的图元不该生成顶点");
    }

    #[test]
    fn a_partially_clipped_primitive_is_kept_whole() {
        // 部分可见的图元交给剪刀矩形处理，不在 CPU 上切几何。
        let mut list = list();
        list.push_clip(Rect::new(0.0, 0.0, 50.0, 50.0));
        list.rect(Rect::new(40.0, 40.0, 20.0, 20.0), red());
        list.pop_clip();
        list.end();

        assert_eq!(list.vertices().len(), 4);
        assert_eq!(list.batches()[0].clip, Rect::new(0.0, 0.0, 50.0, 50.0));
    }

    #[test]
    fn the_corner_radius_is_clamped_to_half_the_size() {
        // 半径超过半尺寸时 SDF 会算出负的内圆半径，边缘会出现一圈亮线。
        let mut list = list();
        list.rounded_rect(Rect::new(0.0, 0.0, 20.0, 10.0), 999.0, red());
        list.end();

        assert_eq!(list.vertices()[0].params[0], 5.0, "半径该被夹到半高");
    }

    #[test]
    fn rounded_corners_do_not_add_vertices() {
        // 用三角扇逼近圆角的话，一个按钮就要几十个顶点，半径一变还得重建。
        let mut list = list();
        list.rounded_rect(Rect::new(0.0, 0.0, 100.0, 40.0), 8.0, red());
        list.end();
        assert_eq!(list.vertices().len(), 4);
    }

    #[test]
    fn a_zero_thickness_border_draws_nothing() {
        let mut list = list();
        list.border(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, 0.0, red());
        list.end();
        assert!(list.is_empty());
    }

    #[test]
    fn every_vertex_carries_its_rect() {
        // 圆角 SDF 要知道自己属于哪个矩形。四个顶点必须带同一份。
        let mut list = list();
        list.rounded_rect(Rect::new(10.0, 20.0, 100.0, 40.0), 6.0, red());
        list.end();

        for v in list.vertices() {
            assert_eq!(v.rect, [60.0, 40.0, 50.0, 20.0]);
            assert_eq!(v.params, [6.0, 0.0]);
        }
    }

    #[test]
    fn solid_primitives_sample_the_white_texel() {
        // 采样整张图集的话，纯色矩形上会印出一片字形。
        let mut list = list();
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), red());
        list.end();

        for v in list.vertices() {
            assert_eq!(v.uv, WHITE, "纯色图元应当四个角都取白点");
        }
    }

    #[test]
    fn begin_clears_but_keeps_capacity() {
        // 即时模式每帧重填，容量丢了就是每帧重新分配。
        let mut list = list();
        for i in 0..200 {
            list.rect(Rect::new(i as f32, 0.0, 5.0, 5.0), red());
        }
        list.end();
        let capacity = list.vertices.capacity();

        list.begin(Vec2::new(800.0, 600.0), WHITE);
        assert!(list.is_empty());
        assert_eq!(list.vertices.capacity(), capacity);
    }

    #[test]
    fn end_flushes_the_last_batch() {
        // 不 flush 的话最后一批图元会静默丢失——画面上表现为
        // 「最后画的那个控件不见了」。
        let mut list = list();
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), red());
        assert!(list.batches().is_empty(), "flush 之前不该有批次");
        list.end();
        assert_eq!(list.batches().len(), 1);
    }

    #[test]
    fn the_vertex_layout_matches_the_shader() {
        // 顶点字段和 WGSL 的 `@location` 对不上就是满屏乱码。
        let module = naga::front::wgsl::parse_str(crate::UI_WGSL).expect("着色器应当能解析");
        let vs = module
            .entry_points
            .iter()
            .find(|e| e.name == "ui_vs")
            .expect("应当有顶点入口");

        let locations: Vec<_> = vs
            .function
            .arguments
            .iter()
            .filter_map(|a| match a.binding {
                Some(naga::Binding::Location { location, .. }) => Some(location),
                _ => None,
            })
            .collect();
        assert_eq!(locations, vec![0, 1, 2, 3, 4]);
        // 2 + 2 + 4 + 4 + 2 个 f32。
        assert_eq!(size_of::<UiVertex>(), 14 * 4);
    }

    #[test]
    fn rect_contains_excludes_the_far_edges() {
        // 相邻控件共享一条边时，两边都算「含」会让那一列像素同时命中两个。
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(rect.contains(Vec2::new(0.0, 0.0)));
        assert!(rect.contains(Vec2::new(9.99, 9.99)));
        assert!(!rect.contains(Vec2::new(10.0, 5.0)));
        assert!(!rect.contains(Vec2::new(5.0, 10.0)));
    }

    #[test]
    fn disjoint_rects_intersect_to_something_empty() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(100.0, 100.0, 10.0, 10.0);
        assert!(a.intersect(&b).is_empty());
    }

    #[test]
    fn shrinking_past_the_middle_collapses_instead_of_inverting() {
        // 收缩过头得到 max < min 的话，后续的尺寸计算会出负数。
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0).shrink(100.0);
        assert!(rect.size().x >= 0.0 && rect.size().y >= 0.0);
    }
}
