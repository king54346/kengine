//! 字体加载与字形光栅化。
//!
//! 这是 `ab_glyph` 的**唯一出口**——和引擎里别的第三方库一样，一个库只准
//! 出现在一个 crate 里。上层拿到的是 [`Metrics`] 和覆盖率位图，
//! 不知道底下是谁在光栅化。
//!
//! # 引擎不自带字体
//!
//! 一个能覆盖中文的字体动辄十几兆，塞进仓库不合适；只带拉丁字体又等于
//! 没解决问题。所以 [`Font::from_bytes`] 只认字节，字体从哪来由调用方决定。
//! [`system_font`] 提供一条方便路径：去几个常见位置找一个能显示中文的字体。
//!
//! # 回退链
//!
//! [`FontStack`] 按顺序试每个字体，谁有这个字形就用谁的。
//! 这是中英混排的实际需要：拉丁字体没有汉字，中文字体的拉丁字形又往往难看。

use crate::atlas::{AtlasError, GlyphAtlas, GlyphEntry, GlyphImage, GlyphKey};
use crate::layout::Metrics;
use ab_glyph::{Font as _, ScaleFont as _};

/// 一个已加载的字体。
pub struct Font {
    inner: ab_glyph::FontVec,
    /// 在 [`FontStack`] 里的编号，进图集的键。
    id: u32,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font").field("id", &self.id).finish()
    }
}

/// 字体加载失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontError(pub String);

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "字体加载失败：{}", self.0)
    }
}

impl std::error::Error for FontError {}

impl Font {
    /// 从 TTF / OTF 字节加载。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, FontError> {
        let inner =
            ab_glyph::FontVec::try_from_vec(bytes).map_err(|e| FontError(format!("{e:?}")))?;
        Ok(Self { inner, id: 0 })
    }

    /// 从文件加载。
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, FontError> {
        let bytes = std::fs::read(path.as_ref())
            .map_err(|e| FontError(format!("{}：{e}", path.as_ref().display())))?;
        Self::from_bytes(bytes)
    }

    /// 这个字体里有没有这个字符。
    ///
    /// 缺字时 `ab_glyph` 返回 0 号字形（`.notdef`，通常画成一个空心方框）。
    /// 回退链靠这个判断该不该往下一个字体找。
    pub fn has_glyph(&self, c: char) -> bool {
        self.inner.glyph_id(c).0 != 0
    }

    /// 光栅化一个字形。
    ///
    /// 空白字形（空格）返回零尺寸的位图，但步进仍然有效。
    fn rasterize(&self, c: char, size_px: f32) -> GlyphImage {
        let scaled = self.inner.as_scaled(size_px);
        let glyph_id = self.inner.glyph_id(c);
        let advance = scaled.h_advance(glyph_id);

        let glyph = glyph_id.with_scale(size_px);
        let Some(outlined) = self.inner.outline_glyph(glyph) else {
            // 没有轮廓 = 空白字形。步进照样要给，不然空格会没有宽度。
            return GlyphImage {
                advance,
                ..Default::default()
            };
        };

        let bounds = outlined.px_bounds();
        let width = bounds.width().ceil().max(0.0) as u32;
        let height = bounds.height().ceil().max(0.0) as u32;
        let mut coverage = vec![0u8; (width * height) as usize];

        outlined.draw(|x, y, c| {
            if x < width && y < height {
                // `c` 是 0..=1 的覆盖率。量化到 u8 时用 255 而不是 256，
                // 免得 1.0 溢出成 0。
                coverage[(y * width + x) as usize] = (c * 255.0 + 0.5) as u8;
            }
        });

        GlyphImage {
            coverage,
            width,
            height,
            bearing_x: bounds.min.x,
            // `px_bounds` 的 y 向下为正、相对基线，所以取负号换成「基线往上多少」。
            bearing_y: -bounds.min.y,
            advance,
        }
    }

    /// 字形的矢量轮廓，**字体单位**、y 朝上（TrueType 的原始坐标）。
    ///
    /// 立体文字（拉伸、倒角）和 SVG 式的矢量渲染要的是轮廓而不是位图。
    /// 曲线按书写顺序排，一段的终点不等于下一段的起点时就是开始了新的
    /// 一条闭合轮廓。缺字或空白字符返回空列表。
    pub fn glyph_outline(&self, c: char) -> Vec<OutlineCurve> {
        let id = self.inner.glyph_id(c);
        let Some(outline) = self.inner.outline(id) else {
            return Vec::new();
        };
        let p = |point: ab_glyph::Point| [point.x, point.y];
        outline
            .curves
            .iter()
            .map(|curve| match *curve {
                ab_glyph::OutlineCurve::Line(a, b) => OutlineCurve::Line(p(a), p(b)),
                ab_glyph::OutlineCurve::Quad(a, b, c) => OutlineCurve::Quad(p(a), p(b), p(c)),
                ab_glyph::OutlineCurve::Cubic(a, b, c, d) => {
                    OutlineCurve::Cubic(p(a), p(b), p(c), p(d))
                }
            })
            .collect()
    }

    /// 每 em 多少字体单位（常见 1000 或 2048）。字体没写时按 1000。
    pub fn units_per_em(&self) -> f32 {
        self.inner.units_per_em().unwrap_or(1000.0)
    }

    /// 字符的水平步进（字体单位）。
    pub fn advance_unscaled(&self, c: char) -> f32 {
        self.inner.h_advance_unscaled(self.inner.glyph_id(c))
    }

    /// 两个字符之间的字距调整（字体单位，通常是负数）。
    pub fn kern_unscaled(&self, first: char, second: char) -> f32 {
        self.inner
            .kern_unscaled(self.inner.glyph_id(first), self.inner.glyph_id(second))
    }

    /// 行高相关的三个量（字体单位）：上伸、下伸（负数）、行距。
    pub fn line_metrics_unscaled(&self) -> (f32, f32, f32) {
        (
            self.inner.ascent_unscaled(),
            self.inner.descent_unscaled(),
            self.inner.line_gap_unscaled(),
        )
    }
}

/// 轮廓里的一段曲线，点是 `[x, y]`（字体单位，y 朝上）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutlineCurve {
    /// 直线：起点、终点。
    Line([f32; 2], [f32; 2]),
    /// 二次贝塞尔：起点、控制点、终点（TrueType 字体都是这种）。
    Quad([f32; 2], [f32; 2], [f32; 2]),
    /// 三次贝塞尔：起点、两个控制点、终点（CFF / OpenType 字体）。
    Cubic([f32; 2], [f32; 2], [f32; 2], [f32; 2]),
}

impl OutlineCurve {
    /// 起点。
    pub fn start(&self) -> [f32; 2] {
        match *self {
            Self::Line(a, _) | Self::Quad(a, _, _) | Self::Cubic(a, _, _, _) => a,
        }
    }

    /// 终点。
    pub fn end(&self) -> [f32; 2] {
        match *self {
            Self::Line(_, b) | Self::Quad(_, _, b) | Self::Cubic(_, _, _, b) => b,
        }
    }
}

/// 一组字体，按顺序回退。
///
/// 中英混排的实际需要：第一个字体没有这个字形就问下一个。
/// 全都没有时用第一个字体的 `.notdef`——画一个空心方框，
/// 比什么都不画好：至少能看出「这里有个字没显示出来」。
#[derive(Debug, Default)]
pub struct FontStack {
    fonts: Vec<Font>,
}

impl FontStack {
    /// 空的字体栈。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个字体，返回它的编号。先加的优先级高。
    pub fn push(&mut self, mut font: Font) -> u32 {
        let id = self.fonts.len() as u32;
        font.id = id;
        self.fonts.push(font);
        id
    }

    /// 字体数量。
    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    /// 一个字体都没有。此时排版会得到全零的度量。
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// 实际要画的字符：栈里没有一个字体认识 `c`、而它有个常见的替身时换成替身。
    ///
    /// 界面里常用的几何符号（`▸` `✓` `▾`）很多中文字体都没有，画出来是一个
    /// 豆腐块，看着像乱码。替身取字体里几乎都有的近似字形：`›` `√` `▼`。
    /// 字体本身认识的字符原样返回——替换只兜底，不改变有字形时的样子。
    pub fn displayed(&self, c: char) -> char {
        if self.fonts.is_empty() || self.fonts.iter().any(|f| f.has_glyph(c)) {
            return c;
        }
        let substitute = match c {
            '▸' | '▶' | '►' | '⯈' | '⏵' => '›',
            '◂' | '◀' | '◄' | '⯇' | '⏴' => '‹',
            '▾' | '⯆' | '⏷' => '▼',
            '▴' | '⯅' | '⏶' => '▲',
            '✓' | '✔' | '🗸' => '√',
            '✗' | '✘' | '✕' | '✖' => '×',
            '⟩' | '〉' => '〉',
            '⟨' | '〈' => '〈',
            '•' => '·',
            _ => return c,
        };
        if self.fonts.iter().any(|f| f.has_glyph(substitute)) {
            substitute
        } else {
            c
        }
    }

    /// 挑出该用哪个字体画这个字符。
    pub fn resolve(&self, c: char) -> Option<&Font> {
        self.fonts
            .iter()
            .find(|f| f.has_glyph(c))
            .or_else(|| self.fonts.first())
    }

    /// 一个字符在图集里的键。字体栈为空时返回 `None`。
    ///
    /// 绘制时手上只有字符，图集的键却是**字形号**——这个方法是两者之间
    /// 唯一的桥。绕过它自己拿 `c as u16` 当字形号的话，取到的是别的字，
    /// 画出来是一片乱码，而且不报任何错。
    pub fn glyph_key(&self, c: char, size_px: f32) -> Option<GlyphKey> {
        let c = self.displayed(c);
        let font = self.resolve(c)?;
        Some(GlyphKey::new(font.id, font.inner.glyph_id(c).0, size_px))
    }

    /// 按某个字号取一份度量，交给排版。
    pub fn metrics(&self, size_px: f32) -> StackMetrics<'_> {
        StackMetrics {
            stack: self,
            size_px,
        }
    }

    /// 确保一个字符的字形已经在图集里，返回它的位置。
    ///
    /// 已经在的直接返回（并刷新 LRU），不在的就光栅化后插入。
    pub fn ensure_glyph(
        &self,
        atlas: &mut GlyphAtlas,
        c: char,
        size_px: f32,
    ) -> Result<(GlyphKey, GlyphEntry), AtlasError> {
        let c = self.displayed(c);
        let font = self.resolve(c).ok_or(AtlasError::Full)?;
        let key = GlyphKey::new(font.id, font.inner.glyph_id(c).0, size_px);

        if let Some(entry) = atlas.get(&key) {
            return Ok((key, entry));
        }

        let entry = atlas.insert(key, &font.rasterize(c, size_px))?;
        Ok((key, entry))
    }
}

/// 某个字号下的字体栈度量。
pub struct StackMetrics<'a> {
    stack: &'a FontStack,
    size_px: f32,
}

impl Metrics for StackMetrics<'_> {
    fn advance(&self, c: char) -> f32 {
        let c = self.stack.displayed(c);
        let Some(font) = self.stack.resolve(c) else {
            return 0.0;
        };
        font.inner
            .as_scaled(self.size_px)
            .h_advance(font.inner.glyph_id(c))
    }

    fn ascent(&self) -> f32 {
        self.stack
            .fonts
            .first()
            .map_or(0.0, |f| f.inner.as_scaled(self.size_px).ascent())
    }

    fn descent(&self) -> f32 {
        // 字体里的 descent 是负数（基线往下），这里的约定是正数。
        self.stack
            .fonts
            .first()
            .map_or(0.0, |f| -f.inner.as_scaled(self.size_px).descent())
    }

    fn line_height(&self) -> f32 {
        self.stack
            .fonts
            .first()
            .map_or(0.0, |f| f.inner.as_scaled(self.size_px).height())
    }

    fn kern(&self, left: char, right: char) -> f32 {
        // 跨字体不做紧排：两个字体的紧排表互不相干，硬凑只会更难看。
        let (left, right) = (self.stack.displayed(left), self.stack.displayed(right));
        let Some(font) = self.stack.resolve(left) else {
            return 0.0;
        };
        if !font.has_glyph(right) {
            return 0.0;
        }
        let scaled = font.inner.as_scaled(self.size_px);
        scaled.kern(font.inner.glyph_id(left), font.inner.glyph_id(right))
    }
}

/// 去系统里找一个能显示中文的字体。
///
/// 返回第一个存在的候选路径。**这只是个方便函数**，正经项目应当把字体
/// 当资源一起发布——系统装了什么字体不受控，同一份界面在两台机器上
/// 可能宽度都不一样。
pub fn system_font() -> Option<std::path::PathBuf> {
    const CANDIDATES: &[&str] = &[
        // Windows：微软雅黑、宋体、黑体。
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        // macOS。
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        // Linux：文泉驿、Noto、思源。
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        // 兜底：只有拉丁也比没有强。
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ];

    CANDIDATES
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ui_symbols_fall_back_to_a_look_alike() {
        let Some(path) = system_font() else { return };
        let Ok(font) = Font::from_file(&path) else {
            return;
        };
        let mut stack = FontStack::new();
        stack.push(font);
        // 字体认识的字符原样不动。
        assert_eq!(stack.displayed('a'), 'a');
        assert_eq!(stack.displayed('中'), '中');
        // 不认识的常见符号换成一个它认识的替身（替身本身也得认识）。
        for c in ['▸', '✓', '▾'] {
            let shown = stack.displayed(c);
            assert!(
                stack.fonts.iter().any(|f| f.has_glyph(shown)),
                "{c} → {shown} 仍然没有字形"
            );
        }
    }

    /// 加载一个系统字体。找不到就返回 `None`，调用方自行决定跳过。
    ///
    /// **这些测试依赖系统字体，在没有字体的机器上会跳过。**
    /// 真正要紧的排版逻辑不走这条路——它跑在 `layout` 模块的假字体上，
    /// 任何机器都能验。这里只验「和 ab_glyph 的接线对不对」。
    fn load() -> Option<FontStack> {
        let path = system_font()?;
        let font = Font::from_file(path).ok()?;
        let mut stack = FontStack::new();
        stack.push(font);
        Some(stack)
    }

    macro_rules! with_font {
        ($stack:ident) => {
            let Some($stack) = load() else {
                eprintln!("跳过：本机没有找到可用的系统字体");
                return;
            };
        };
    }

    #[test]
    fn garbage_bytes_are_rejected() {
        // 这条不需要字体：随便一段字节都该被拒，而不是 panic。
        let err = Font::from_bytes(vec![0, 1, 2, 3]).unwrap_err();
        assert!(!err.0.is_empty());
    }

    #[test]
    fn an_empty_stack_reports_zero_metrics() {
        // 没加载字体时排版不该 panic，只是量出来全是 0。
        let stack = FontStack::new();
        let metrics = stack.metrics(16.0);
        assert_eq!(metrics.advance('a'), 0.0);
        assert_eq!(metrics.ascent(), 0.0);
        assert!(stack.resolve('a').is_none());
    }

    #[test]
    fn latin_glyphs_have_positive_advance() {
        with_font!(stack);
        let metrics = stack.metrics(32.0);
        assert!(metrics.advance('M') > 0.0);
        assert!(metrics.ascent() > 0.0);
        assert!(metrics.descent() > 0.0, "descent 的约定是正数");
    }

    #[test]
    fn advance_scales_with_size() {
        with_font!(stack);
        let small = stack.metrics(16.0).advance('M');
        let large = stack.metrics(32.0).advance('M');
        assert!(
            (large / small - 2.0).abs() < 0.1,
            "字号翻倍，步进应当也翻倍：{small} → {large}"
        );
    }

    #[test]
    fn a_space_has_advance_but_no_bitmap() {
        with_font!(stack);
        let mut atlas = GlyphAtlas::new(256);
        let (_, entry) = stack.ensure_glyph(&mut atlas, ' ', 32.0).unwrap();
        assert!(entry.is_blank(), "空格不该有位图");
        assert!(entry.advance > 0.0, "空格照样要占宽度");
    }

    #[test]
    fn a_rasterized_glyph_has_ink() {
        // 覆盖率全零说明光栅化接错了——画出来是一片空白，而且不报错。
        with_font!(stack);
        let mut atlas = GlyphAtlas::new(256);
        let (_, entry) = stack.ensure_glyph(&mut atlas, 'M', 48.0).unwrap();

        assert!(!entry.is_blank());
        let ink: u32 = (0..entry.rect[3])
            .flat_map(|row| (0..entry.rect[2]).map(move |col| (row, col)))
            .map(|(row, col)| {
                let index = ((entry.rect[1] + row) * atlas.size() + entry.rect[0] + col) as usize;
                u32::from(atlas.pixels()[index])
            })
            .sum();
        assert!(ink > 0, "字形位图是全空的");
    }

    #[test]
    fn the_same_glyph_is_rasterized_only_once() {
        with_font!(stack);
        let mut atlas = GlyphAtlas::new(256);
        stack.ensure_glyph(&mut atlas, 'A', 24.0).unwrap();
        let version = atlas.version();

        atlas.begin_frame();
        stack.ensure_glyph(&mut atlas, 'A', 24.0).unwrap();
        assert_eq!(atlas.version(), version, "重复的字形不该重新写图集");
        assert_eq!(atlas.len(), 1);
    }

    #[test]
    fn different_sizes_are_different_entries() {
        with_font!(stack);
        let mut atlas = GlyphAtlas::new(512);
        stack.ensure_glyph(&mut atlas, 'A', 12.0).unwrap();
        stack.ensure_glyph(&mut atlas, 'A', 48.0).unwrap();
        assert_eq!(atlas.len(), 2, "两个字号该是两张位图");
    }

    #[test]
    fn bearing_y_points_up_from_the_baseline() {
        // 约定是「基线往上多少」。符号搞反的话整行字会掉到基线下面。
        with_font!(stack);
        let mut atlas = GlyphAtlas::new(256);
        let (_, entry) = stack.ensure_glyph(&mut atlas, 'M', 48.0).unwrap();
        assert!(
            entry.bearing_y > 0.0,
            "大写字母的顶端应当在基线之上，实测 {}",
            entry.bearing_y
        );
    }

    #[test]
    fn a_descender_reaches_below_the_baseline() {
        // 'g' 的下伸部分在基线以下：位图高度要大于 bearing_y。
        with_font!(stack);
        let mut atlas = GlyphAtlas::new(256);
        let (_, entry) = stack.ensure_glyph(&mut atlas, 'g', 48.0).unwrap();
        assert!(
            entry.rect[3] as f32 > entry.bearing_y,
            "'g' 没有伸到基线以下"
        );
    }

    #[test]
    fn the_fallback_chain_picks_a_font_that_has_the_glyph() {
        with_font!(stack);
        // 只有一个字体时，resolve 永远返回它——包括缺字的情况
        // （此时用 .notdef 画方框，好过什么都不画）。
        assert!(stack.resolve('a').is_some());
        assert!(stack.resolve('\u{10FFFF}').is_some());
    }
}
