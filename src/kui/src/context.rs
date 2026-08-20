//! UI 上下文：把字体、图集、绘制列表攒在一起。
//!
//! 存在的理由是**封掉一个坑**。直接用底层 API 画一段文字要两步：
//! 先 `ensure_glyph` 把字形塞进图集，再 `DrawList::text` 去查。
//! 顺序反了不会报错——只是那一帧的文字不显示，下一帧又好了，
//! 表现为「文字偶尔闪一下」，极难查。[`Ui::text`] 把两步并成一步。

use crate::{DrawList, Rect};
use kfont::{FontStack, GlyphAtlas, TextLayout, TextStyle};
use kmath::{Vec2, Vec4};
use ktexture::Texture;

/// 一帧 UI 的全部状态。
pub struct Ui {
    /// 字体栈。按顺序回退，先加的优先。
    pub fonts: FontStack,
    atlas: GlyphAtlas,
    list: DrawList,
    screen: Vec2,
    scale: f32,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    /// 字形图集的边长。
    ///
    /// 1024² 在中文下大约能装两三千个 16 px 的字形——够一整屏有余。
    /// 装不下会按 LRU 驱逐，不会缺字。
    const ATLAS_SIZE: u32 = 1024;

    /// 空的上下文。**没有字体**，得先 [`Ui::add_font`]。
    pub fn new() -> Self {
        Self {
            fonts: FontStack::new(),
            atlas: GlyphAtlas::new(Self::ATLAS_SIZE),
            list: DrawList::default(),
            screen: Vec2::ZERO,
            scale: 1.0,
        }
    }

    /// 加一个字体。
    pub fn add_font(&mut self, font: kfont::Font) -> u32 {
        // 换字体之后旧字形的字体编号还留在图集里，会和新字体的编号撞上。
        // 全清一遍最稳，反正只在加载时发生。
        self.atlas.clear();
        self.fonts.push(font)
    }

    /// 一个字体都没有。此时所有文字调用都是空操作。
    pub fn has_font(&self) -> bool {
        !self.fonts.is_empty()
    }

    /// 界面尺寸（逻辑像素）。
    pub fn screen(&self) -> Vec2 {
        self.screen
    }

    /// 当前的绘制列表。
    pub fn draw_list(&self) -> &DrawList {
        &self.list
    }

    /// 字形图集的版本号。渲染器靠它判断要不要重传纹理。
    pub fn atlas_version(&self) -> u64 {
        self.atlas.version()
    }

    /// 把图集取成纹理。**只在版本号变了之后调**——它要展开成 RGBA，
    /// 1024² 就是 4 MB 的分配，每帧做一次是实打实的浪费。
    pub fn atlas_texture(&self) -> Texture {
        self.atlas.to_texture()
    }

    /// 开一帧。
    ///
    /// `screen` 是逻辑像素尺寸，`scale` 是 DPI 缩放（用来把剪刀矩形
    /// 换算成物理像素）。
    pub fn begin_frame(&mut self, screen: Vec2, scale: f32) {
        self.screen = screen;
        self.scale = scale.max(0.01);
        self.atlas.begin_frame();
        self.list.begin(screen, self.atlas.white_uv());
    }

    /// 收一帧。**必须调**，否则最后一批图元会静默丢失。
    pub fn end_frame(&mut self) {
        self.list.end();
    }

    /// DPI 缩放。
    pub fn scale(&self) -> f32 {
        self.scale
    }

    // ───────────────────────── 绘制 ─────────────────────────

    /// 实心矩形。
    pub fn rect(&mut self, rect: Rect, color: Vec4) {
        self.list.rect(rect, color);
    }

    /// 圆角矩形。
    pub fn rounded_rect(&mut self, rect: Rect, radius: f32, color: Vec4) {
        self.list.rounded_rect(rect, radius, color);
    }

    /// 边框。
    pub fn border(&mut self, rect: Rect, radius: f32, thickness: f32, color: Vec4) {
        self.list.border(rect, radius, thickness, color);
    }

    /// 压一层裁剪。
    pub fn push_clip(&mut self, rect: Rect) {
        self.list.push_clip(rect);
    }

    /// 弹一层裁剪。
    pub fn pop_clip(&mut self) {
        self.list.pop_clip();
    }

    /// 量一段文字排完之后有多大，但不画。
    ///
    /// 布局要用：得先知道文字多宽，才能决定容器多宽。
    pub fn measure(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        kfont::layout(text, style, &self.fonts.metrics(style.size), max_width)
    }

    /// 画一段文字，`origin` 是文本框的左上角。返回排版结果。
    ///
    /// 字形的准备与绘制在这里一次做完——分两步调用的话，顺序反了
    /// 会让那一帧的文字不显示，而且不报任何错。
    pub fn text(
        &mut self,
        origin: Vec2,
        text: &str,
        style: &TextStyle,
        color: Vec4,
        max_width: Option<f32>,
    ) -> TextLayout {
        let layout = self.measure(text, style, max_width);
        if layout.glyphs.is_empty() {
            return layout;
        }

        // 先把这一帧要用的字形都塞进图集。
        //
        // 插不进去（图集满了且无可驱逐）时只是这个字形画不出来，
        // 不影响别的字——所以忽略错误，而不是整段文字放弃。
        for glyph in &layout.glyphs {
            let _ = self
                .fonts
                .ensure_glyph(&mut self.atlas, glyph.c, style.size);
        }

        self.list
            .text(origin, &layout, &self.fonts, &self.atlas, style.size, color);
        layout
    }

    /// 在一个矩形里居中画一行文字。按钮的文字用它。
    pub fn text_centered(
        &mut self,
        rect: Rect,
        text: &str,
        style: &TextStyle,
        color: Vec4,
    ) -> TextLayout {
        let layout = self.measure(text, style, None);
        let origin = rect.center() - layout.size * 0.5;
        self.text(origin, text, style, color, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> Ui {
        let mut ui = Ui::new();
        ui.begin_frame(Vec2::new(800.0, 600.0), 1.0);
        ui
    }

    fn white() -> Vec4 {
        Vec4::ONE
    }

    /// 加载一个系统字体。没有就返回 `None`。
    fn with_font() -> Option<Ui> {
        let path = kfont::system_font()?;
        let font = kfont::Font::from_file(path).ok()?;
        let mut ui = Ui::new();
        ui.add_font(font);
        ui.begin_frame(Vec2::new(800.0, 600.0), 1.0);
        Some(ui)
    }

    #[test]
    fn shapes_work_without_any_font() {
        // 没字体时纯色图形照样能画——否则「字体还没加载完」的那几帧
        // 会整个界面消失。
        let mut ui = ui();
        assert!(!ui.has_font());
        ui.rect(Rect::new(0.0, 0.0, 100.0, 50.0), white());
        ui.end_frame();
        assert_eq!(ui.draw_list().vertices().len(), 4);
    }

    #[test]
    fn text_without_a_font_is_a_no_op() {
        let mut ui = ui();
        let layout = ui.text(Vec2::ZERO, "没有字体", &TextStyle::default(), white(), None);
        ui.end_frame();

        // 排版层不认识字体，照样会产出字形——只是度量全为零。
        // 要紧的是**不产生几何**：画不出来好过画出一堆错位的方块。
        assert_eq!(layout.size, Vec2::ZERO);
        assert!(ui.draw_list().is_empty());
    }

    #[test]
    fn drawing_text_produces_geometry() {
        let Some(mut ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        ui.text(
            Vec2::new(10.0, 10.0),
            "Hello",
            &TextStyle::default(),
            white(),
            None,
        );
        ui.end_frame();

        // 五个字母，每个一个四边形。
        assert_eq!(ui.draw_list().vertices().len(), 5 * 4);
        assert_eq!(ui.draw_list().batches().len(), 1, "文字该和纯色同批");
    }

    #[test]
    fn text_and_shapes_share_one_batch() {
        // 白点的意义就在这里：一整个界面一次绘制画完。
        let Some(mut ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        ui.rect(Rect::new(0.0, 0.0, 200.0, 40.0), white());
        ui.text(
            Vec2::new(8.0, 8.0),
            "按钮",
            &TextStyle::default(),
            white(),
            None,
        );
        ui.rounded_rect(Rect::new(0.0, 60.0, 200.0, 40.0), 8.0, white());
        ui.end_frame();

        assert_eq!(ui.draw_list().batches().len(), 1);
    }

    #[test]
    fn glyphs_are_in_the_atlas_before_they_are_drawn() {
        // 顺序反了不会报错，只是那一帧文字不显示——表现为「文字偶尔闪一下」。
        let Some(mut ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        // 第一帧就该画出来，不能等到第二帧。
        ui.text(Vec2::ZERO, "A", &TextStyle::default(), white(), None);
        ui.end_frame();
        assert!(!ui.draw_list().is_empty(), "第一帧就该有几何");
    }

    #[test]
    fn cjk_text_lays_out_and_draws() {
        let Some(mut ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        let style = TextStyle {
            size: 18.0,
            ..Default::default()
        };
        // 限宽取 60：18 px 的雅黑一个汉字约 18 px，四个字加 "mixed"
        // 无论如何都塞不进 60 px。取 100 的话实测宽 98.26，真的放得下——
        // 那样测的就不是换行了。
        let layout = ui.text(Vec2::ZERO, "中文界面 mixed", &style, white(), Some(60.0));
        ui.end_frame();

        assert!(
            layout.line_count() > 1,
            "60 px 装不下这么多字，该换行；实测 {} 行，宽 {}",
            layout.line_count(),
            layout.size.x
        );
        assert!(!ui.draw_list().is_empty());
    }

    #[test]
    fn the_atlas_version_only_moves_when_new_glyphs_appear() {
        let Some(mut ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        ui.text(Vec2::ZERO, "abc", &TextStyle::default(), white(), None);
        ui.end_frame();
        let version = ui.atlas_version();

        // 同样的字第二帧不该让渲染器重传 4 MB 的纹理。
        ui.begin_frame(Vec2::new(800.0, 600.0), 1.0);
        ui.text(Vec2::ZERO, "abc", &TextStyle::default(), white(), None);
        ui.end_frame();
        assert_eq!(ui.atlas_version(), version);

        // 新字则要重传。
        ui.begin_frame(Vec2::new(800.0, 600.0), 1.0);
        ui.text(Vec2::ZERO, "xyz", &TextStyle::default(), white(), None);
        ui.end_frame();
        assert!(ui.atlas_version() > version);
    }

    #[test]
    fn measure_does_not_touch_the_atlas() {
        // 布局阶段会量很多次文字。每次都往图集里塞字形的话，
        // 一次布局就能把图集冲爆。
        let Some(ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        let version = ui.atlas_version();
        for _ in 0..100 {
            ui.measure("量一下", &TextStyle::default(), None);
        }
        assert_eq!(ui.atlas_version(), version);
    }

    #[test]
    fn centered_text_sits_in_the_middle() {
        let Some(mut ui) = with_font() else {
            eprintln!("跳过：本机没有找到可用的系统字体");
            return;
        };
        let rect = Rect::new(100.0, 100.0, 200.0, 60.0);
        let layout = ui.text_centered(rect, "OK", &TextStyle::default(), white());
        ui.end_frame();

        let expected = rect.center() - layout.size * 0.5;
        let first = ui.draw_list().vertices()[0].position;
        // 第一个字形的左上角在期望原点附近（差一个字形的左偏移）。
        assert!((first[0] - expected.x).abs() < 5.0, "水平没居中");
    }

    #[test]
    fn begin_frame_resets_the_list() {
        let mut ui = ui();
        ui.rect(Rect::new(0.0, 0.0, 10.0, 10.0), white());
        ui.end_frame();
        assert!(!ui.draw_list().is_empty());

        ui.begin_frame(Vec2::new(800.0, 600.0), 1.0);
        assert!(ui.draw_list().is_empty(), "上一帧的图元没清掉会一直累积");
    }
}
