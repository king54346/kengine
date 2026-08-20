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

use crate::linebreak::{BreakClass, break_class, break_opportunities};
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
pub fn layout(text: &str, style: &TextStyle, metrics: &dyn Metrics, max_width: Option<f32>) -> TextLayout {
    let mut out = TextLayout::default();
    if text.is_empty() {
        return out;
    }

    let line_height = metrics.line_height() * style.line_height;
    let space = metrics.advance(' ');
    let tab_width = space * style.tab_size.max(1) as f32;

    // 只有 Word 模式才真的按宽度断。
    let wrap_width = match style.wrap {
        Wrap::Word => max_width,
        Wrap::None | Wrap::Ellipsis => None,
    };
    let opportunities = break_opportunities(text);

    let mut line_start = 0usize;
    let mut pen = 0.0f32;
    let mut previous: Option<char> = None;
    // 最近一个可断点：(字节偏移, 断点处的笔位置, 该断点在 glyphs 里的下标)
    let mut last_break: Option<(usize, f32, usize)> = None;
    let mut line_index = 0usize;

    for (offset, c) in text.char_indices() {
        // 强制换行。
        if c == '\n' {
            finish_line(&mut out, line_start, pen, line_index, line_height);
            line_start = out.glyphs.len();
            line_index += 1;
            pen = 0.0;
            previous = None;
            last_break = None;
            continue;
        }
        if c == '\r' {
            continue;
        }

        // 走到一个断点上，记下来备用。
        if let Some(b) = opportunities.iter().find(|b| b.offset == offset)
            && !b.mandatory
        {
            last_break = Some((offset, pen, out.glyphs.len()));
        }

        let kern = previous.map_or(0.0, |p| metrics.kern(p, c));
        let advance = match c {
            '\t' => {
                // 制表符跳到下一个制表位，而不是固定加几个空格宽。
                let next = ((pen / tab_width).floor() + 1.0) * tab_width;
                let width = next - pen;
                pen = next;
                previous = Some(c);
                let _ = width;
                continue;
            }
            _ => metrics.advance(c) + kern,
        };

        // 该换行了吗？
        if let Some(limit) = wrap_width
            && pen + advance > limit
            && !out.glyphs.is_empty()
            && break_class(c) != BreakClass::Space
        {
            match last_break {
                // 有可断点：把断点之后的字形挪到新行。
                Some((_, break_pen, break_glyph)) if break_glyph > line_start => {
                    let moved: Vec<_> = out.glyphs[break_glyph..].to_vec();
                    out.glyphs.truncate(break_glyph);
                    finish_line(&mut out, line_start, break_pen, line_index, line_height);
                    line_start = out.glyphs.len();
                    line_index += 1;

                    // 重排被挪走的那批：它们的 x 要整体左移，y 要下移一行。
                    let shift = moved.first().map_or(0.0, |g| g.x);
                    pen = 0.0;
                    for mut g in moved {
                        g.x -= shift;
                        g.y = baseline_of(line_index, line_height, metrics);
                        g.line = line_index;
                        pen = g.x;
                        out.glyphs.push(g);
                    }
                    // pen 要指向最后一个字形之后，而不是它的起点。
                    pen = out
                        .glyphs
                        .last()
                        .map_or(0.0, |g| g.x + metrics.advance(g.c));
                }
                // 没有可断点：一个超长的词，只能硬断。
                // 不硬断的话它会一路画出容器外面。
                _ => {
                    finish_line(&mut out, line_start, pen, line_index, line_height);
                    line_start = out.glyphs.len();
                    line_index += 1;
                    pen = 0.0;
                }
            }
            last_break = None;
            previous = None;
        }

        // 空白不进字形表——它没有位图，进去只会让图集多一堆空条目。
        // 但笔照样要前进。
        if break_class(c) == BreakClass::Space {
            // 行首的空白直接吃掉，否则换行后每行都缩进一格。
            if pen > 0.0 {
                pen += advance;
            }
            previous = Some(c);
            continue;
        }

        out.glyphs.push(PositionedGlyph {
            c,
            x: pen,
            y: baseline_of(line_index, line_height, metrics),
            offset,
            line: line_index,
        });
        pen += advance;
        previous = Some(c);
    }

    finish_line(&mut out, line_start, pen, line_index, line_height);

    if style.wrap == Wrap::Ellipsis
        && let Some(limit) = max_width
    {
        truncate_with_ellipsis(&mut out, limit, metrics);
    }

    let width = out.lines.iter().fold(0.0f32, |w, l| w.max(l.width));
    out.size = Vec2::new(width, line_height * out.lines.len() as f32);
    align_lines(&mut out, style.align, max_width.unwrap_or(width));
    out
}

/// 第 `line` 行的基线 y。
fn baseline_of(line: usize, line_height: f32, metrics: &dyn Metrics) -> f32 {
    // 基线不是行顶：字形要挂在基线上，行顶到基线的距离是 ascent。
    // 直接用行顶的话，所有字会整体偏上一个 ascent，字号越大偏得越多。
    line as f32 * line_height + metrics.ascent()
}

/// 收一行。
fn finish_line(
    out: &mut TextLayout,
    start: usize,
    width: f32,
    line: usize,
    line_height: f32,
) {
    out.lines.push(LineInfo {
        range: (start, out.glyphs.len()),
        width,
        baseline: line as f32 * line_height,
    });
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
        let mut cut = line.range.1;
        while cut > line.range.0 {
            let last = out.glyphs[cut - 1];
            if last.x + ellipsis_width <= limit {
                break;
            }
            cut -= 1;
        }

        // 一个字都放不下时也要留省略号：宁可显示「…」，
        // 也不要显示一个空框让人以为是加载失败。
        let x = if cut > line.range.0 {
            out.glyphs[cut - 1].x + metrics.advance(out.glyphs[cut - 1].c)
        } else {
            0.0
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
        assert!(layout.lines[0].width <= 50.0, "截断后仍然超宽");
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
            assert!(
                g.x + 20.0 <= layout.size.x + 1e-3,
                "字形 {:?} 跑到包围盒外面了",
                g.c
            );
        }
    }
}
