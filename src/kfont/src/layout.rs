//! 文字排版：把一串字符摆成一行行带坐标的字形。
//!
//! # 为什么排版不认识字体
//!
//! 这一层只通过 [`Metrics`] 拿宽度和行高。字体加载、光栅化、图集全在别处。
//! 好处很直接：**排版逻辑能在没有任何字体文件的情况下测**——
//! 断行、对齐、溢出、CJK 禁则这些容易错的地方，用一个「每个字都是 10 px 宽」
//! 的假字体就能钉死，不必往仓库里塞一个几十兆的字体，CI 上也不必碰系统字体。
//!
//! # 做了什么、没做什么
//!
//! 做了：断行（含 CJK 禁则）、水平对齐、行高、制表符、`\n`、超长词强断、
//! 单行省略号截断。
//!
//! **没做整形（shaping）。** 每个字符独立取字形、按 advance 累加。
//! 这对拉丁文和 CJK 是对的，对**阿拉伯文、天城文**是错的——那些书写系统
//! 需要按上下文替换字形。要支持它们得接 rustybuzz，那是另一个量级的工作。

use crate::linebreak::{BreakClass, BreakOpportunity, break_class, break_opportunities};
use kmath::Vec2;

/// 排版需要从字体那里知道的全部信息。
///
/// 抽成 trait 是为了让排版可测：测试里塞一个等宽的假实现就行。
pub trait Metrics {
    /// 一个字符占多宽（像素）。
    fn advance(&self, c: char) -> f32;

    /// 基线到行顶的距离（正数）。
    fn ascent(&self) -> f32;

    /// 基线到行底的距离（正数）。
    fn descent(&self) -> f32;

    /// 建议行距。默认是 `ascent + descent`。
    fn line_height(&self) -> f32 {
        self.ascent() + self.descent()
    }

    /// 两个字符之间的紧排微调。默认没有。
    fn kern(&self, _left: char, _right: char) -> f32 {
        0.0
    }
}

/// 水平对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// 左对齐。
    #[default]
    Left,
    /// 居中。
    Center,
    /// 右对齐。
    Right,
}

/// 装不下时怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    /// 到宽度就换行。
    #[default]
    Word,
    /// 不换行，超出部分照画（由调用方裁剪）。
    None,
    /// 不换行，超出部分用省略号截断。
    Ellipsis,
}

/// 排版参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// 字号（像素）。
    pub size: f32,
    /// 行距倍率，乘在字体建议行距上。
    pub line_height: f32,
    /// 水平对齐。
    pub align: Align,
    /// 换行策略。
    pub wrap: Wrap,
    /// 一个制表符等于几个空格。
    pub tab_size: u32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 16.0,
            line_height: 1.2,
            align: Align::Left,
            wrap: Wrap::Word,
            tab_size: 4,
        }
    }
}

/// 排好位置的一个字形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// 对应的字符。渲染时据此去图集取位图。
    pub c: char,
    /// 笔位置的 x（相对整段文本的左上角）。
    pub x: f32,
    /// **基线**的 y（相对整段文本的左上角，向下为正）。
    pub y: f32,
    /// 这个字符在原字符串里的字节偏移。光标定位用。
    pub offset: usize,
    /// 第几行。
    pub line: usize,
}

/// 一行的信息。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineInfo {
    /// 这一行的字形在 [`TextLayout::glyphs`] 里的区间。
    pub range: (usize, usize),
    /// 行宽（像素）。
    pub width: f32,
    /// 基线 y。
    pub baseline: f32,
}

/// 排版结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextLayout {
    /// 所有字形，按行、按顺序排列。空白字符**不在**其中。
    pub glyphs: Vec<PositionedGlyph>,
    /// 每一行。
    pub lines: Vec<LineInfo>,
    /// 整段文本的包围尺寸。
    pub size: Vec2,
}

impl TextLayout {
    /// 行数。空文本是 0 行。
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// 落在某个点上的字符的字节偏移。用来做点击定位光标。
    ///
    /// 点在某个字形的左半边就返回它的偏移，右半边返回下一个——
    /// 这是文本编辑器的通行手感：点在字的右侧，光标落在字之后。
    pub fn offset_at(&self, point: Vec2) -> Option<usize> {
        let line = self.lines.iter().enumerate().find(|(_, l)| {
            point.y < l.baseline + self.size.y / self.lines.len().max(1) as f32
        })?;
        let (start, end) = line.1.range;

        for glyph in &self.glyphs[start..end] {
            let advance = self.glyphs[start..end]
                .iter()
                .find(|g| g.offset > glyph.offset)
                .map_or(f32::INFINITY, |next| next.x - glyph.x);
            if point.x < glyph.x + advance * 0.5 {
                return Some(glyph.offset);
            }
        }
        self.glyphs[start..end].last().map(|g| g.offset)
    }
}

/// 省略号。用单个字符而不是三个点：三个点会被当成三个字形去查图集。
const ELLIPSIS: char = '…';

/// 把一段文本排成若干行。
///
/// `max_width` 为 `None` 表示不限宽（等价于 [`Wrap::None`]）。
pub fn layout(
    text: &str,
    style: &TextStyle,
    metrics: &dyn Metrics,
    max_width: Option<f32>,
) -> TextLayout {
    let mut out = TextLayout::default();
    if text.is_empty() {
        return out;
    }

    let line_height = metrics.line_height() * style.line_height;
    let tab_width = metrics.advance(' ') * style.tab_size.max(1) as f32;
    // 只有 Word 模式才真的按宽度断。
    let limit = match style.wrap {
        Wrap::Word => max_width,
        Wrap::None | Wrap::Ellipsis => None,
    };

    let mut cursor = Cursor {
        line: 0,
        line_start: 0,
        pen: 0.0,
        previous: None,
        line_height,
        tab_width,
    };

    // 先把文本按断行机会切成一个个**段**，再贪心地往行里装。
    //
    // 早先的写法是「先摆字形，超宽了再回头把尾巴搬到下一行」——
    // 搬运时要同步修 x、y、行号、笔位置四样东西，漏一样就错，而且错法
    // 各不相同（楼梯状下滑、包围盒对不上、一个字一行）。切段之后
    // 「装不下就换行」是一次判断，没有回溯。
    for segment in segments(text) {
        if segment.mandatory_break {
            finish_line(&mut out, &mut cursor);
        }

        // 判断能否装下时**不算行尾空白**：一行末尾的空格不该把词挤到下一行。
        let visible = measure(text, segment.visible(), metrics, &cursor);
        let fits = limit.is_none_or(|w| cursor.pen + visible <= w);
        let line_empty = out.glyphs.len() == cursor.line_start;

        if !fits && !line_empty {
            finish_line(&mut out, &mut cursor);
        }

        // 一个段自己就比整行还宽（超长的词、或者根本没有断点的一串字符），
        // 只能逐字硬断。不硬断的话它会一路画到容器外面去。
        if let Some(w) = limit
            && visible > w
        {
            for (offset, c) in text[segment.range()].char_indices() {
                let offset = segment.start + offset;
                let advance = cursor.advance_of(c, metrics);
                if cursor.pen + advance > w && out.glyphs.len() > cursor.line_start {
                    finish_line(&mut out, &mut cursor);
                }
                cursor.place(&mut out, c, offset, metrics);
            }
            continue;
        }

        for (offset, c) in text[segment.range()].char_indices() {
            cursor.place(&mut out, c, segment.start + offset, metrics);
        }
    }
    finish_line(&mut out, &mut cursor);

    if style.wrap == Wrap::Ellipsis
        && let Some(w) = max_width
    {
        truncate_with_ellipsis(&mut out, w, metrics);
    }

    let width = out.lines.iter().fold(0.0f32, |w, l| w.max(l.width));
    out.size = Vec2::new(width, line_height * out.lines.len() as f32);
    align_lines(&mut out, style.align, max_width.unwrap_or(width));
    out
}

/// 排版过程中的游标状态。
struct Cursor {
    line: usize,
    /// 当前行的第一个字形在 `glyphs` 里的下标。
    line_start: usize,
    /// 笔的水平位置。
    pen: f32,
    /// 上一个字符，用来查紧排。
    previous: Option<char>,
    line_height: f32,
    tab_width: f32,
}

impl Cursor {
    /// 一个字符要占多宽（含紧排与制表位）。
    fn advance_of(&self, c: char, metrics: &dyn Metrics) -> f32 {
        if c == '\t' {
            // 制表符是「跳到下一个制表位」，不是「加固定几个空格宽」。
            // 后者会让对齐的列在不同起点下错开。
            return ((self.pen / self.tab_width).floor() + 1.0) * self.tab_width - self.pen;
        }
        let kern = self.previous.map_or(0.0, |p| metrics.kern(p, c));
        metrics.advance(c) + kern
    }

    /// 放一个字符。空白与换行不产生字形，但笔照样前进。
    fn place(&mut self, out: &mut TextLayout, c: char, offset: usize, metrics: &dyn Metrics) {
        if c == '\n' || c == '\r' {
            // 换行符本身不占位；换行动作由段的 `mandatory_break` 触发。
            return;
        }

        let advance = self.advance_of(c, metrics);
        if break_class(c) == BreakClass::Space || c == '\t' {
            // 行首的空白直接吃掉，否则换行之后每行都缩进一格。
            if self.pen > 0.0 {
                self.pen += advance;
            }
            self.previous = Some(c);
            return;
        }

        out.glyphs.push(PositionedGlyph {
            c,
            x: self.pen,
            y: self.line as f32 * self.line_height + metrics.ascent(),
            offset,
            line: self.line,
        });
        self.pen += advance;
        self.previous = Some(c);
    }
}

/// 一个段：两个断行机会之间的一截文本。
struct Segment {
    start: usize,
    end: usize,
    /// 可见部分的结束位置（去掉行尾空白）。
    visible_end: usize,
    /// 这个段之前有没有强制换行。
    mandatory_break: bool,
}

impl Segment {
    fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }
    fn visible(&self) -> std::ops::Range<usize> {
        self.start..self.visible_end
    }
}

/// 按断行机会把文本切成段。
fn segments(text: &str) -> Vec<Segment> {
    let breaks = break_opportunities(text);
    let mut out = Vec::with_capacity(breaks.len() + 1);
    let mut start = 0usize;
    let mut mandatory = false;

    for b in breaks.iter().chain(std::iter::once(&BreakOpportunity {
        offset: text.len(),
        mandatory: false,
    })) {
        if b.offset <= start {
            continue;
        }
        let chunk = &text[start..b.offset];
        // 行尾空白不计入宽度：一行末尾的空格不该把下一个词挤到下一行。
        let visible_end = start + chunk.trim_end().len();
        out.push(Segment {
            start,
            end: b.offset,
            visible_end,
            mandatory_break: mandatory,
        });
        start = b.offset;
        mandatory = b.mandatory;
    }
    out
}

/// 量一段文本的宽度。
fn measure(text: &str, range: std::ops::Range<usize>, metrics: &dyn Metrics, cursor: &Cursor) -> f32 {
    let mut width = 0.0;
    let mut previous = cursor.previous;
    for c in text[range].chars() {
        if c == '\n' || c == '\r' {
            continue;
        }
        if c == '\t' {
            let at = cursor.pen + width;
            width += ((at / cursor.tab_width).floor() + 1.0) * cursor.tab_width - at;
            previous = Some(c);
            continue;
        }
        width += metrics.advance(c) + previous.map_or(0.0, |p| metrics.kern(p, c));
        previous = Some(c);
    }
    width
}

/// 收一行，游标移到下一行行首。
fn finish_line(out: &mut TextLayout, cursor: &mut Cursor) {
    out.lines.push(LineInfo {
        range: (cursor.line_start, out.glyphs.len()),
        width: cursor.pen,
        baseline: cursor.line as f32 * cursor.line_height,
    });
    cursor.line += 1;
    cursor.line_start = out.glyphs.len();
    cursor.pen = 0.0;
    cursor.previous = None;
}

/// 按对齐方式整体平移每一行。
fn align_lines(out: &mut TextLayout, align: Align, container_width: f32) {
    if align == Align::Left {
        return;
    }
    for line in &out.lines {
        let slack = container_width - line.width;
        let shift = match align {
            Align::Left => 0.0,
            Align::Center => slack * 0.5,
            Align::Right => slack,
        };
        for glyph in &mut out.glyphs[line.range.0..line.range.1] {
            glyph.x += shift;
        }
    }
}

/// 把超宽的行截断并补一个省略号。
fn truncate_with_ellipsis(out: &mut TextLayout, limit: f32, metrics: &dyn Metrics) {
    let ellipsis_width = metrics.advance(ELLIPSIS);

    for index in 0..out.lines.len() {
        let line = out.lines[index];
        if line.width <= limit {
            continue;
        }

        // 从后往前退，直到省略号也放得下。
        //
        // 判据是省略号的**右边缘**，不是最后一个字形的起点——按起点判的话
        // 省略号总会右溢一个字宽，截断后照样超出容器。
        let mut cut = line.range.1;
        let x = loop {
            // 一个字都放不下时也要留省略号：宁可显示「…」，
            // 也不要留一片空白让人以为是加载失败。
            let x = if cut > line.range.0 {
                let last = out.glyphs[cut - 1];
                last.x + metrics.advance(last.c)
            } else {
                0.0
            };
            if x + ellipsis_width <= limit || cut == line.range.0 {
                break x;
            }
            cut -= 1;
        };

        let ellipsis = PositionedGlyph {
            c: ELLIPSIS,
            x,
            y: out.glyphs[line.range.0].y,
            offset: out.glyphs[cut.max(line.range.0 + 1) - 1].offset,
            line: index,
        };

        out.glyphs.splice(cut..line.range.1, [ellipsis]);

        let removed = line.range.1 - cut;
        out.lines[index].range.1 = cut + 1;
        out.lines[index].width = x + ellipsis_width;
        // 后面几行的区间要跟着挪。
        for later in &mut out.lines[index + 1..] {
            later.range.0 = later.range.0 + 1 - removed;
            later.range.1 = later.range.1 + 1 - removed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个假字体：拉丁字符 10 px 宽，CJK 20 px 宽，行高 20。
    ///
    /// 宽度取整数，测试里就能直接写死期望值，不必和浮点误差纠缠。
    struct FakeFont;

    impl Metrics for FakeFont {
        fn advance(&self, c: char) -> f32 {
            if crate::linebreak::is_ideographic(c) {
                20.0
            } else {
                10.0
            }
        }
        fn ascent(&self) -> f32 {
            16.0
        }
        fn descent(&self) -> f32 {
            4.0
        }
    }

    fn plain() -> TextStyle {
        TextStyle {
            size: 20.0,
            line_height: 1.0,
            ..Default::default()
        }
    }

    /// 把排版结果按行还原成字符串，方便一眼看断在哪。
    fn lines_of(layout: &TextLayout) -> Vec<String> {
        layout
            .lines
            .iter()
            .map(|l| layout.glyphs[l.range.0..l.range.1].iter().map(|g| g.c).collect())
            .collect()
    }

    #[test]
    fn empty_text_lays_out_to_nothing() {
        let layout = layout("", &plain(), &FakeFont, None);
        assert!(layout.glyphs.is_empty());
        assert_eq!(layout.line_count(), 0);
        assert_eq!(layout.size, Vec2::ZERO);
    }

    #[test]
    fn glyphs_advance_left_to_right() {
        let layout = layout("abc", &plain(), &FakeFont, None);
        let xs: Vec<f32> = layout.glyphs.iter().map(|g| g.x).collect();
        assert_eq!(xs, vec![0.0, 10.0, 20.0]);
        assert_eq!(layout.size.x, 30.0);
    }

    #[test]
    fn the_baseline_sits_an_ascent_below_the_top() {
        // 用行顶当基线的话，所有字会整体上移一个 ascent，字号越大越明显。
        let layout = layout("a", &plain(), &FakeFont, None);
        assert_eq!(layout.glyphs[0].y, 16.0);
    }

    #[test]
    fn whitespace_produces_no_glyph_but_still_advances() {
        // 空格没有位图。进字形表只会让图集多一堆空条目。
        let layout = layout("a b", &plain(), &FakeFont, None);
        assert_eq!(layout.glyphs.len(), 2);
        assert_eq!(layout.glyphs[1].c, 'b');
        assert_eq!(layout.glyphs[1].x, 20.0, "空格的宽度还是要占的");
    }

    #[test]
    fn newline_starts_a_new_line() {
        let layout = layout("ab\ncd", &plain(), &FakeFont, None);
        assert_eq!(lines_of(&layout), vec!["ab", "cd"]);
        assert_eq!(layout.glyphs[2].x, 0.0, "第二行要从头开始");
        assert_eq!(layout.glyphs[2].y, 36.0, "第二行的基线要低一个行高");
    }

    #[test]
    fn latin_wraps_at_spaces() {
        // 100 px 装得下 "hello"（50）+ 空格（10）+ 一部分 "world"，
        // 但 "world" 要整个移到下一行。
        let layout = layout("hello world", &plain(), &FakeFont, Some(100.0));
        assert_eq!(lines_of(&layout), vec!["hello", "world"]);
    }

    #[test]
    fn a_wrapped_line_starts_at_x_zero() {
        // 换行后没把 x 归零的话，第二行会从第一行的末尾开始，
        // 整段文字像楼梯一样往右下滑。
        let layout = layout("hello world", &plain(), &FakeFont, Some(100.0));
        let second = layout.lines[1];
        assert_eq!(layout.glyphs[second.range.0].x, 0.0);
    }

    #[test]
    fn cjk_wraps_between_characters() {
        // 中文没有空格。按空格断的话整段挤成一行冲出容器。
        let layout = layout("中文换行测试", &plain(), &FakeFont, Some(60.0));
        assert_eq!(lines_of(&layout), vec!["中文换", "行测试"]);
    }

    #[test]
    fn closing_punctuation_does_not_start_a_wrapped_line() {
        // 断行禁则要一路传到排版：行首出现「。」一眼就看得出不对。
        let layout = layout("你好。世界", &plain(), &FakeFont, Some(60.0));
        for line in lines_of(&layout) {
            assert!(
                !line.starts_with('。'),
                "「。」跑到了行首：{:?}",
                lines_of(&layout)
            );
        }
    }

    #[test]
    fn an_overlong_word_is_broken_rather_than_overflowing() {
        // 一个比容器还长的词没有可断点。不硬断的话它会一路画到容器外面。
        let layout = layout("supercalifragilistic", &plain(), &FakeFont, Some(50.0));
        assert!(layout.line_count() > 1, "超长词没有被断开");
        for line in &layout.lines {
            assert!(line.width <= 50.0, "有一行宽 {}，超出了容器", line.width);
        }
    }

    #[test]
    fn leading_whitespace_after_a_wrap_is_dropped() {
        // 不吃掉的话，换行后每一行都会莫名缩进一格。
        let layout = layout("aaaaa bbbbb", &plain(), &FakeFont, Some(50.0));
        let second = layout.lines[1];
        assert_eq!(layout.glyphs[second.range.0].x, 0.0);
    }

    #[test]
    fn wrap_none_keeps_everything_on_one_line() {
        let style = TextStyle {
            wrap: Wrap::None,
            ..plain()
        };
        let layout = layout("hello world foo bar", &style, &FakeFont, Some(30.0));
        assert_eq!(layout.line_count(), 1);
    }

    #[test]
    fn ellipsis_truncates_and_fits() {
        let style = TextStyle {
            wrap: Wrap::Ellipsis,
            ..plain()
        };
        let layout = layout("hello world", &style, &FakeFont, Some(50.0));
        assert_eq!(layout.line_count(), 1);

        let text: String = layout.glyphs.iter().map(|g| g.c).collect();
        assert!(text.ends_with('…'), "截断后应当以省略号结尾：{text:?}");
        assert!(
            layout.lines[0].width <= 50.0,
            "截断后仍然超宽：{} / 文本 {text:?}",
            layout.lines[0].width
        );
    }

    #[test]
    fn ellipsis_survives_an_impossibly_narrow_container() {
        // 一个字都放不下时也要留个省略号，而不是留一片空白
        // 让人以为是加载失败。
        let style = TextStyle {
            wrap: Wrap::Ellipsis,
            ..plain()
        };
        let layout = layout("hello", &style, &FakeFont, Some(5.0));
        let text: String = layout.glyphs.iter().map(|g| g.c).collect();
        assert_eq!(text, "…");
    }

    #[test]
    fn short_text_is_not_truncated() {
        let style = TextStyle {
            wrap: Wrap::Ellipsis,
            ..plain()
        };
        let layout = layout("ab", &style, &FakeFont, Some(500.0));
        let text: String = layout.glyphs.iter().map(|g| g.c).collect();
        assert_eq!(text, "ab");
    }

    #[test]
    fn center_alignment_splits_the_slack() {
        let style = TextStyle {
            align: Align::Center,
            ..plain()
        };
        let layout = layout("ab", &style, &FakeFont, Some(100.0));
        // 内容宽 20，容器宽 100，两边各留 40。
        assert_eq!(layout.glyphs[0].x, 40.0);
    }

    #[test]
    fn right_alignment_pushes_to_the_edge() {
        let style = TextStyle {
            align: Align::Right,
            ..plain()
        };
        let layout = layout("ab", &style, &FakeFont, Some(100.0));
        assert_eq!(layout.glyphs[0].x, 80.0);
    }

    #[test]
    fn each_line_is_aligned_on_its_own() {
        // 整段一起平移的话，短行会跟着长行走，看着像左对齐。
        let style = TextStyle {
            align: Align::Right,
            ..plain()
        };
        let layout = layout("abcd\nab", &style, &FakeFont, Some(100.0));
        assert_eq!(layout.glyphs[0].x, 60.0, "第一行");
        let second = layout.lines[1];
        assert_eq!(layout.glyphs[second.range.0].x, 80.0, "第二行");
    }

    #[test]
    fn line_height_multiplies_the_font_metric() {
        let style = TextStyle {
            line_height: 2.0,
            ..plain()
        };
        let layout = layout("a\nb", &style, &FakeFont, None);
        // 字体行高 20，倍率 2 → 行距 40。
        assert_eq!(layout.glyphs[1].y - layout.glyphs[0].y, 40.0);
        assert_eq!(layout.size.y, 80.0);
    }

    #[test]
    fn tabs_jump_to_the_next_stop() {
        // 制表符是「跳到下一个制表位」，不是「加固定几个空格宽」。
        // 后者会让对齐的列在不同起点下错开。
        let layout = layout("a\tb", &plain(), &FakeFont, None);
        // 制表位每 40 px（4 个空格宽）。'a' 占到 10，跳到 40。
        assert_eq!(layout.glyphs[1].x, 40.0);
    }

    #[test]
    fn offsets_point_back_into_the_source() {
        // 光标定位、选区都靠这个偏移。多字节字符上尤其容易错。
        let text = "a中b";
        let layout = layout(text, &plain(), &FakeFont, None);
        let offsets: Vec<usize> = layout.glyphs.iter().map(|g| g.offset).collect();
        assert_eq!(offsets, vec![0, 1, 4]);
        for g in &layout.glyphs {
            assert!(text.is_char_boundary(g.offset));
        }
    }

    #[test]
    fn line_indices_are_consistent() {
        let layout = layout("ab\ncd\nef", &plain(), &FakeFont, None);
        for (index, line) in layout.lines.iter().enumerate() {
            for glyph in &layout.glyphs[line.range.0..line.range.1] {
                assert_eq!(glyph.line, index);
            }
        }
    }

    #[test]
    fn size_covers_every_glyph() {
        let layout = layout("中文 mixed\n第二行", &plain(), &FakeFont, Some(200.0));
        for g in &layout.glyphs {
            assert!(g.x >= 0.0);
            // 用这个字形自己的宽度，不是固定值——拉丁 10、CJK 20。
            let right = g.x + FakeFont.advance(g.c);
            assert!(
                right <= layout.size.x + 1e-3,
                "字形 {:?} 的右边缘 {right} 超出了包围盒 {}",
                g.c,
                layout.size.x
            );
        }
    }
}
