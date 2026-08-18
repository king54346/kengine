//! ksprite —— 2D 精灵、图集与帧动画。
//!
//! **不依赖 wgpu，也不自带渲染管线。** 一个精灵最终就是一张贴了图的方片，
//! 而引擎已经有一条画「网格 + 材质」的路径了。所以这里产出的是
//! [`Sprite::quad`]（一块方片网格）和 [`Sprite::uv_scale`] / [`Sprite::uv_offset`]
//! （从图集里取哪一格），交给现成的管线去画。
//!
//! 这么做换来的是：精灵天然享有已有的批处理、实例化、剔除、光照与后处理，
//! 而渲染器一行都不用改（只加了一对标准材质参数 `uv_scale` / `uv_offset`）。
//! 代价是精灵走的是 3D 管线，逐精灵开销比专用的 2D 批处理器高一些；
//! 真到了要画几万个精灵的场合，再补一条专用路径不迟。
//!
//! ```
//! use ksprite::prelude::*;
//!
//! // 一张 4×2 格的角色表
//! let atlas = Atlas::grid(4, 2);
//! let mut walk = SpriteAnimation::new(atlas.row(0), 12.0);
//!
//! walk.tick(0.5);
//! let frame = walk.frame();
//! assert!(frame.uv_scale().x > 0.0);
//! ```

#![warn(missing_docs)]

use kcore::visitor::{Visit, VisitResult, Visitor};
use kmath::{Vec2, Vec3, Vec4};
use kmesh::{Mesh, Vertex};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{Anchor, Atlas, PlayMode, Sprite, SpriteAnimation, SpriteRegion};
}

/// 图集里的一格：一个归一化的 UV 矩形。
///
/// 用归一化坐标而不是像素：精灵不需要知道图有多大，换一张两倍分辨率的图
/// 也不用改任何数据。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteRegion {
    /// 左上角的 UV。
    pub min: Vec2,
    /// 右下角的 UV。
    pub max: Vec2,
}

impl Default for SpriteRegion {
    fn default() -> Self {
        Self::FULL
    }
}

impl SpriteRegion {
    /// 整张图。
    pub const FULL: Self = Self {
        min: Vec2::ZERO,
        max: Vec2::ONE,
    };

    /// 用两个角构造。两个角写反了会自动摆正。
    pub fn new(min: Vec2, max: Vec2) -> Self {
        Self {
            min: min.min(max),
            max: min.max(max),
        }
    }

    /// 按像素坐标构造，`size` 是整张图的尺寸。
    ///
    /// 图集是美术按像素排的，这个入口省掉调用方每次手算除法。
    pub fn from_pixels(x: f32, y: f32, width: f32, height: f32, size: Vec2) -> Self {
        // 尺寸为 0 时退回整张图，而不是产出一堆 NaN 的 UV。
        if size.x <= 0.0 || size.y <= 0.0 {
            return Self::FULL;
        }
        Self::new(
            Vec2::new(x / size.x, y / size.y),
            Vec2::new((x + width) / size.x, (y + height) / size.y),
        )
    }

    /// 采样时的 UV 缩放。
    pub fn uv_scale(&self) -> Vec2 {
        self.max - self.min
    }

    /// 采样时的 UV 偏移。
    pub fn uv_offset(&self) -> Vec2 {
        self.min
    }

    /// 宽高比（宽 / 高）。高为 0 时返回 1。
    pub fn aspect(&self) -> f32 {
        let size = self.uv_scale();
        if size.y.abs() < f32::EPSILON {
            1.0
        } else {
            size.x / size.y
        }
    }

    /// 左右翻转。
    pub fn flipped_x(self) -> Self {
        Self {
            min: Vec2::new(self.max.x, self.min.y),
            max: Vec2::new(self.min.x, self.max.y),
        }
    }

    /// 上下翻转。
    pub fn flipped_y(self) -> Self {
        Self {
            min: Vec2::new(self.min.x, self.max.y),
            max: Vec2::new(self.max.x, self.min.y),
        }
    }
}

impl Visit for SpriteRegion {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.min.visit("Min", &mut region)?;
        self.max.visit("Max", &mut region)?;
        Ok(())
    }
}

/// 方片相对节点原点的对齐方式。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Anchor {
    /// 以中心对齐。
    #[default]
    Center,
    /// 以底边中点对齐。
    ///
    /// 角色、树、建筑几乎都用这个：脚底就是它站的位置，
    /// 排序和贴地都按这一点算最省事。
    BottomCenter,
    /// 以左上角对齐，接近 2D UI 的习惯。
    TopLeft,
    /// 自定义：`(0,0)` 是左下，`(1,1)` 是右上。
    Custom(Vec2),
}

impl Anchor {
    /// 换算成 `[0,1]` 空间里的锚点坐标。
    pub fn to_uv(self) -> Vec2 {
        match self {
            Self::Center => Vec2::new(0.5, 0.5),
            Self::BottomCenter => Vec2::new(0.5, 0.0),
            Self::TopLeft => Vec2::new(0.0, 1.0),
            Self::Custom(v) => v,
        }
    }
}

/// 一个精灵。
///
/// 只描述「取图集的哪一格、画多大、怎么对齐、什么颜色」，
/// 贴图本身由材质提供。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// 取图集的哪一格。
    pub region: SpriteRegion,
    /// 方片的世界尺寸。
    pub size: Vec2,
    /// 对齐方式。
    pub anchor: Anchor,
    /// 顶点色，与材质基础色相乘。
    pub color: Vec4,
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            region: SpriteRegion::FULL,
            size: Vec2::ONE,
            anchor: Anchor::Center,
            color: Vec4::ONE,
        }
    }
}

impl Sprite {
    /// 取整张图，尺寸为 1×1。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取图集的某一格。
    pub fn from_region(region: SpriteRegion) -> Self {
        Self {
            region,
            ..Self::default()
        }
    }

    /// 换一格。
    pub fn with_region(mut self, region: SpriteRegion) -> Self {
        self.region = region;
        self
    }

    /// 指定世界尺寸。
    pub fn with_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    /// 按给定高度、并保持该格的宽高比定出尺寸。
    ///
    /// 图集里各格宽高不一时（打包器很爱这么排），手写尺寸会把精灵拉变形。
    pub fn with_height(mut self, height: f32) -> Self {
        self.size = Vec2::new(height * self.region.aspect(), height);
        self
    }

    /// 指定对齐方式。
    pub fn with_anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// 指定顶点色。
    pub fn with_color(mut self, color: Vec4) -> Self {
        self.color = color;
        self
    }

    /// 采样时的 UV 缩放，喂给材质的 `uv_scale`。
    pub fn uv_scale(&self) -> Vec2 {
        self.region.uv_scale()
    }

    /// 采样时的 UV 偏移，喂给材质的 `uv_offset`。
    pub fn uv_offset(&self) -> Vec2 {
        self.region.uv_offset()
    }

    /// 生成这个精灵的方片网格，位于 XY 平面、法线朝 +Z。
    ///
    /// **网格的 UV 恒为 0..1**，取哪一格由材质的 `uv_scale` / `uv_offset` 决定。
    /// 这样换一帧动画只要改两个数值参数，不必重建顶点缓冲——
    /// 也因此同一张图集上的所有精灵能共用一份网格、合并成一次绘制。
    pub fn quad(&self) -> Mesh {
        let anchor = self.anchor.to_uv();
        let min = -Vec2::new(self.size.x * anchor.x, self.size.y * anchor.y);
        let max = min + self.size;
        let color = Vec3::new(self.color.x, self.color.y, self.color.z);

        // UV 的 V 轴朝下（贴图习惯），所以左下角的 v 是 1。
        let vertices = vec![
            Vertex::new(Vec3::new(min.x, min.y, 0.0), Vec3::Z, [0.0, 1.0]).with_color(color),
            Vertex::new(Vec3::new(max.x, min.y, 0.0), Vec3::Z, [1.0, 1.0]).with_color(color),
            Vertex::new(Vec3::new(max.x, max.y, 0.0), Vec3::Z, [1.0, 0.0]).with_color(color),
            Vertex::new(Vec3::new(min.x, max.y, 0.0), Vec3::Z, [0.0, 0.0]).with_color(color),
        ];

        let mut mesh = Mesh::new(vertices, vec![0, 1, 2, 0, 2, 3]);
        mesh.recompute_tangents();
        mesh
    }
}

/// 一张图集：把一张图切成若干格。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Atlas {
    regions: Vec<SpriteRegion>,
    names: Vec<String>,
}

impl Atlas {
    /// 空图集。
    pub fn new() -> Self {
        Self::default()
    }

    /// 均匀网格：`columns × rows` 格，按**行主序**编号。
    ///
    /// 行主序是精灵表的通行排法（一行一个动作），[`row`](Self::row)
    /// 因此能直接取出一整条动画。
    pub fn grid(columns: usize, rows: usize) -> Self {
        let mut atlas = Self::new();
        if columns == 0 || rows == 0 {
            return atlas;
        }
        let step = Vec2::new(1.0 / columns as f32, 1.0 / rows as f32);
        for row in 0..rows {
            for column in 0..columns {
                let min = Vec2::new(column as f32 * step.x, row as f32 * step.y);
                atlas.push(format!("{row}_{column}"), SpriteRegion::new(min, min + step));
            }
        }
        atlas
    }

    /// 追加一格。
    pub fn push(&mut self, name: impl Into<String>, region: SpriteRegion) {
        self.names.push(name.into());
        self.regions.push(region);
    }

    /// 链式追加。
    pub fn with(mut self, name: impl Into<String>, region: SpriteRegion) -> Self {
        self.push(name, region);
        self
    }

    /// 格子总数。
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// 是否一格都没有。
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// 按序号取一格。
    pub fn region(&self, index: usize) -> Option<SpriteRegion> {
        self.regions.get(index).copied()
    }

    /// 按名字取一格。
    pub fn find(&self, name: &str) -> Option<SpriteRegion> {
        let index = self.names.iter().position(|n| n == name)?;
        self.regions.get(index).copied()
    }

    /// 全部格子。
    pub fn regions(&self) -> &[SpriteRegion] {
        &self.regions
    }

    /// 某一格的名字。
    pub fn name(&self, index: usize) -> Option<&str> {
        self.names.get(index).map(String::as_str)
    }

    /// 取网格图集的一整行，按列从左到右。
    ///
    /// 只对 [`grid`](Self::grid) 建出来的图集有意义；行号越界时返回空。
    pub fn row(&self, row: usize) -> Vec<SpriteRegion> {
        let prefix = format!("{row}_");
        let mut frames: Vec<(usize, SpriteRegion)> = self
            .names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                let column = name.strip_prefix(&prefix)?.parse::<usize>().ok()?;
                Some((column, self.regions[index]))
            })
            .collect();
        frames.sort_by_key(|(column, _)| *column);
        frames.into_iter().map(|(_, region)| region).collect()
    }
}

impl Visit for Atlas {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.names.visit("Names", &mut region)?;

        let mut count = self.regions.len() as u32;
        count.visit("Count", &mut region)?;
        if region.is_reading() {
            self.regions = vec![SpriteRegion::FULL; count as usize];
        }
        for (index, rect) in self.regions.iter_mut().enumerate() {
            rect.visit(&format!("Region{index}"), &mut region)?;
        }
        Ok(())
    }
}

/// 帧动画的播放方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlayMode {
    /// 放到最后一帧就停住。
    Once,
    /// 循环。
    #[default]
    Loop,
    /// 来回播：放到头再倒着放回来。
    ///
    /// 与「循环」的区别是接缝：循环从末帧跳回首帧会有一次突变，
    /// 呼吸、闪烁这类效果用来回播才连贯。
    PingPong,
}

/// 帧动画。
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteAnimation {
    frames: Vec<SpriteRegion>,
    /// 每秒几帧。
    pub fps: f32,
    /// 播放方式。
    pub mode: PlayMode,
    /// 是否暂停。
    pub paused: bool,
    /// 已播放的时间，秒。
    elapsed: f32,
}

impl Default for SpriteAnimation {
    /// 一条空动画。方便把它当结构体字段直接 `#[derive(Default)]`。
    fn default() -> Self {
        Self::new(Vec::new(), 0.0)
    }
}

impl SpriteAnimation {
    /// 用一串帧和帧率构造，默认循环播放。
    pub fn new(frames: Vec<SpriteRegion>, fps: f32) -> Self {
        Self {
            frames,
            fps: fps.max(0.0),
            mode: PlayMode::Loop,
            paused: false,
            elapsed: 0.0,
        }
    }

    /// 指定播放方式。
    pub fn with_mode(mut self, mode: PlayMode) -> Self {
        self.mode = mode;
        self
    }

    /// 帧数。
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 是否一帧都没有。
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// 全部帧。
    pub fn frames(&self) -> &[SpriteRegion] {
        &self.frames
    }

    /// 播完一轮要多久。帧率为 0 或没有帧时返回 0。
    pub fn duration(&self) -> f32 {
        if self.fps <= 0.0 || self.frames.is_empty() {
            return 0.0;
        }
        self.frames.len() as f32 / self.fps
    }

    /// 回到第一帧。
    pub fn restart(&mut self) {
        self.elapsed = 0.0;
    }

    /// 已播放的时间。
    pub fn elapsed(&self) -> f32 {
        self.elapsed
    }

    /// 推进 `dt` 秒。
    pub fn tick(&mut self, dt: f32) {
        if self.paused || !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.elapsed += dt;
    }

    /// 当前是第几帧。
    pub fn frame_index(&self) -> usize {
        let count = self.frames.len();
        if count == 0 {
            return 0;
        }
        if self.fps <= 0.0 {
            return 0;
        }

        let raw = (self.elapsed * self.fps) as usize;
        match self.mode {
            PlayMode::Once => raw.min(count - 1),
            PlayMode::Loop => raw % count,
            PlayMode::PingPong => {
                if count == 1 {
                    return 0;
                }
                // 一个来回是 2(n-1) 帧：0,1,…,n-1,n-2,…,1，两端各只出现一次，
                // 否则首尾帧会连着显示两次，看起来像卡了一拍。
                let period = 2 * (count - 1);
                let position = raw % period;
                if position < count {
                    position
                } else {
                    period - position
                }
            }
        }
    }

    /// 当前这一帧的区域。没有帧时返回整张图。
    pub fn frame(&self) -> SpriteRegion {
        self.frames
            .get(self.frame_index())
            .copied()
            .unwrap_or(SpriteRegion::FULL)
    }

    /// 一次性播放的动画是否已经播完。
    ///
    /// 循环与来回播永远返回 `false`——它们没有「完」这回事。
    pub fn is_finished(&self) -> bool {
        self.mode == PlayMode::Once && self.elapsed >= self.duration()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_full_region_covers_the_whole_texture() {
        let region = SpriteRegion::FULL;

        assert_eq!(region.uv_scale(), Vec2::ONE);
        assert_eq!(region.uv_offset(), Vec2::ZERO);
    }

    #[test]
    fn corners_written_backwards_are_straightened_out() {
        let region = SpriteRegion::new(Vec2::new(0.8, 0.9), Vec2::new(0.2, 0.1));

        assert_eq!(region.min, Vec2::new(0.2, 0.1));
        assert_eq!(region.max, Vec2::new(0.8, 0.9));
        assert!(region.uv_scale().x > 0.0 && region.uv_scale().y > 0.0);
    }

    #[test]
    fn pixel_coordinates_convert_to_normalised_uv() {
        let region = SpriteRegion::from_pixels(64.0, 32.0, 32.0, 32.0, Vec2::new(128.0, 64.0));

        assert_eq!(region.min, Vec2::new(0.5, 0.5));
        assert_eq!(region.max, Vec2::new(0.75, 1.0));
    }

    #[test]
    fn a_zero_sized_texture_does_not_produce_nan_uvs() {
        let region = SpriteRegion::from_pixels(0.0, 0.0, 10.0, 10.0, Vec2::ZERO);

        assert_eq!(region, SpriteRegion::FULL);
    }

    #[test]
    fn flipping_swaps_the_uv_direction() {
        let region = SpriteRegion::new(Vec2::new(0.0, 0.0), Vec2::new(0.5, 1.0));
        let flipped = region.flipped_x();

        // 缩放变成负数，采样方向就反过来了——不需要另做一张镜像贴图。
        assert!(flipped.uv_scale().x < 0.0);
        assert_eq!(flipped.uv_offset().x, 0.5);
        assert_eq!(flipped.uv_scale().y, region.uv_scale().y);
    }

    #[test]
    fn a_grid_atlas_is_indexed_row_major() {
        let atlas = Atlas::grid(4, 2);

        assert_eq!(atlas.len(), 8);
        assert_eq!(atlas.name(0), Some("0_0"));
        assert_eq!(atlas.name(4), Some("1_0"));

        let first = atlas.region(0).unwrap();
        assert_eq!(first.uv_scale(), Vec2::new(0.25, 0.5));
        assert_eq!(first.uv_offset(), Vec2::ZERO);
    }

    #[test]
    fn a_degenerate_grid_is_empty_rather_than_dividing_by_zero() {
        assert!(Atlas::grid(0, 3).is_empty());
        assert!(Atlas::grid(3, 0).is_empty());
    }

    #[test]
    fn a_row_comes_out_in_column_order() {
        // 一行就是一个动作，顺序错了动画会倒着放或者乱跳。
        let atlas = Atlas::grid(4, 2);
        let row = atlas.row(1);

        assert_eq!(row.len(), 4);
        for pair in row.windows(2) {
            assert!(pair[0].uv_offset().x < pair[1].uv_offset().x, "列序乱了");
        }
        // 第 1 行的 V 偏移应当是 0.5。
        assert!((row[0].uv_offset().y - 0.5).abs() < 1e-6);
    }

    #[test]
    fn an_out_of_range_row_is_empty() {
        assert!(Atlas::grid(4, 2).row(9).is_empty());
    }

    #[test]
    fn regions_can_be_looked_up_by_name() {
        let atlas = Atlas::new()
            .with("head", SpriteRegion::new(Vec2::ZERO, Vec2::splat(0.5)))
            .with("body", SpriteRegion::new(Vec2::splat(0.5), Vec2::ONE));

        assert_eq!(atlas.find("head").unwrap().uv_offset(), Vec2::ZERO);
        assert_eq!(atlas.find("body").unwrap().uv_offset(), Vec2::splat(0.5));
        assert!(atlas.find("tail").is_none());
    }

    #[test]
    fn an_atlas_survives_a_roundtrip() {
        let atlas = Atlas::grid(3, 2);

        let mut visitor = Visitor::new();
        let mut source = atlas.clone();
        source.visit("A", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut restored = Atlas::new();
        restored.visit("A", &mut visitor).unwrap();

        assert_eq!(restored, atlas);
    }

    // ── 方片 ──

    #[test]
    fn a_centred_quad_straddles_the_origin() {
        let mesh = Sprite::new().with_size(Vec2::new(2.0, 4.0)).quad();
        let aabb = mesh.aabb();

        assert_eq!(aabb.min, Vec3::new(-1.0, -2.0, 0.0));
        assert_eq!(aabb.max, Vec3::new(1.0, 2.0, 0.0));
    }

    #[test]
    fn a_bottom_anchored_quad_stands_on_the_origin() {
        // 角色、树、建筑都用这个：脚底就是它站的位置。
        let mesh = Sprite::new()
            .with_size(Vec2::new(2.0, 4.0))
            .with_anchor(Anchor::BottomCenter)
            .quad();
        let aabb = mesh.aabb();

        assert_eq!(aabb.min.y, 0.0);
        assert_eq!(aabb.max.y, 4.0);
        assert_eq!(aabb.min.x, -1.0);
    }

    #[test]
    fn a_top_left_anchored_quad_hangs_below_the_origin() {
        let mesh = Sprite::new()
            .with_anchor(Anchor::TopLeft)
            .with_size(Vec2::ONE)
            .quad();
        let aabb = mesh.aabb();

        assert_eq!(aabb.min, Vec3::new(0.0, -1.0, 0.0));
        assert_eq!(aabb.max, Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn the_quad_uv_always_spans_zero_to_one() {
        // 取哪一格由材质参数决定，不是由顶点决定——换帧才不必重建顶点缓冲，
        // 同一张图集上的精灵也才能合批。
        let sprite = Sprite::from_region(SpriteRegion::new(
            Vec2::new(0.25, 0.5),
            Vec2::new(0.5, 0.75),
        ));
        let mesh = sprite.quad();

        let us: Vec<f32> = mesh.vertices().iter().map(|v| v.uv[0]).collect();
        let vs: Vec<f32> = mesh.vertices().iter().map(|v| v.uv[1]).collect();

        assert_eq!(us.iter().cloned().fold(f32::MAX, f32::min), 0.0);
        assert_eq!(us.iter().cloned().fold(f32::MIN, f32::max), 1.0);
        assert_eq!(vs.iter().cloned().fold(f32::MAX, f32::min), 0.0);
        assert_eq!(vs.iter().cloned().fold(f32::MIN, f32::max), 1.0);

        // 而取哪一格全在这两个数值里。
        assert_eq!(sprite.uv_scale(), Vec2::new(0.25, 0.25));
        assert_eq!(sprite.uv_offset(), Vec2::new(0.25, 0.5));
    }

    #[test]
    fn the_quad_faces_the_camera_and_has_usable_tangents() {
        let mesh = Sprite::new().quad();

        for vertex in mesh.vertices() {
            assert_eq!(vertex.normal(), Vec3::Z);
            assert!(vertex.tangent().is_finite());
            assert!(vertex.normal().dot(vertex.tangent()).abs() < 1e-4);
        }
        assert_eq!(mesh.triangle_count(), 2);
        assert!(mesh.is_valid());
    }

    #[test]
    fn the_sprite_colour_reaches_the_vertices() {
        let mesh = Sprite::new()
            .with_color(Vec4::new(1.0, 0.0, 0.0, 1.0))
            .quad();

        for vertex in mesh.vertices() {
            assert_eq!(vertex.color, [1.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn sizing_by_height_preserves_the_region_aspect() {
        // 图集里各格宽高不一时，手写尺寸会把精灵拉变形。
        let sprite = Sprite::from_region(SpriteRegion::new(
            Vec2::ZERO,
            Vec2::new(0.5, 0.25),
        ))
        .with_height(2.0);

        // 该格是 2:1 的，高 2 就该宽 4。
        assert_eq!(sprite.size, Vec2::new(4.0, 2.0));
    }

    // ── 帧动画 ──

    #[test]
    fn an_animation_advances_one_frame_per_tick_at_matching_fps() {
        let mut animation = SpriteAnimation::new(Atlas::grid(4, 1).row(0), 10.0);

        assert_eq!(animation.frame_index(), 0);
        animation.tick(0.1);
        assert_eq!(animation.frame_index(), 1);
        animation.tick(0.1);
        assert_eq!(animation.frame_index(), 2);
    }

    #[test]
    fn a_looping_animation_wraps_around() {
        let mut animation = SpriteAnimation::new(Atlas::grid(3, 1).row(0), 10.0);

        animation.tick(0.3);
        assert_eq!(animation.frame_index(), 0, "一轮之后该回到第一帧");
        assert!(!animation.is_finished(), "循环动画没有「播完」这回事");
    }

    #[test]
    fn a_one_shot_animation_holds_the_last_frame() {
        let mut animation =
            SpriteAnimation::new(Atlas::grid(3, 1).row(0), 10.0).with_mode(PlayMode::Once);

        animation.tick(10.0);

        assert_eq!(animation.frame_index(), 2);
        assert!(animation.is_finished());
    }

    #[test]
    fn ping_pong_visits_each_end_exactly_once() {
        // 两端各显示两次的话，看起来像每到头就卡一拍。
        let mut animation =
            SpriteAnimation::new(Atlas::grid(4, 1).row(0), 1.0).with_mode(PlayMode::PingPong);

        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(animation.frame_index());
            animation.tick(1.0);
        }

        assert_eq!(seen, vec![0, 1, 2, 3, 2, 1]);
    }

    #[test]
    fn ping_pong_with_a_single_frame_stays_put() {
        let mut animation = SpriteAnimation::new(vec![SpriteRegion::FULL], 10.0)
            .with_mode(PlayMode::PingPong);

        animation.tick(5.0);

        assert_eq!(animation.frame_index(), 0);
    }

    #[test]
    fn a_paused_animation_does_not_advance() {
        let mut animation = SpriteAnimation::new(Atlas::grid(4, 1).row(0), 10.0);
        animation.paused = true;

        animation.tick(1.0);

        assert_eq!(animation.frame_index(), 0);
        assert_eq!(animation.elapsed(), 0.0);
    }

    #[test]
    fn an_empty_or_zero_fps_animation_is_harmless() {
        let mut empty = SpriteAnimation::new(Vec::new(), 10.0);
        empty.tick(1.0);
        assert_eq!(empty.frame_index(), 0);
        assert_eq!(empty.frame(), SpriteRegion::FULL);
        assert_eq!(empty.duration(), 0.0);

        let mut frozen = SpriteAnimation::new(Atlas::grid(4, 1).row(0), 0.0);
        frozen.tick(100.0);
        assert_eq!(frozen.frame_index(), 0);
    }

    #[test]
    fn a_bogus_delta_does_not_move_the_playhead() {
        let mut animation = SpriteAnimation::new(Atlas::grid(4, 1).row(0), 10.0);

        animation.tick(f32::NAN);
        animation.tick(-1.0);
        animation.tick(0.0);

        assert_eq!(animation.elapsed(), 0.0);
    }

    #[test]
    fn restart_rewinds_to_the_first_frame() {
        let mut animation = SpriteAnimation::new(Atlas::grid(4, 1).row(0), 10.0);
        animation.tick(0.25);
        assert_ne!(animation.frame_index(), 0);

        animation.restart();

        assert_eq!(animation.frame_index(), 0);
        assert_eq!(animation.elapsed(), 0.0);
    }

    #[test]
    fn duration_matches_the_frame_count_over_the_frame_rate() {
        let animation = SpriteAnimation::new(Atlas::grid(6, 1).row(0), 12.0);
        assert!((animation.duration() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_current_frame_tracks_the_index() {
        let atlas = Atlas::grid(4, 1);
        let mut animation = SpriteAnimation::new(atlas.row(0), 10.0);

        animation.tick(0.2);

        assert_eq!(animation.frame(), atlas.region(2).unwrap());
    }
}
