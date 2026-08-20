//! 断行机会。
//!
//! 「哪里可以换行」和「换到哪里」是两件事，这里只管前者。
//!
//! # 为什么不能只按空格断
//!
//! 中日韩文本**没有空格**。整段中文按空格断行的结果是一整段挤成一行，
//! 一路溢出到屏幕外。所以规则至少要有两条：
//!
//! - 拉丁文按**空格**断（断在词与词之间）；
//! - CJK 按**字**断（几乎每两个字之间都能断）。
//!
//! # 禁则
//!
//! 「几乎」的那部分就是禁则（kinsoku）。中文排版里有些字符不能出现在行首
//! （`。，、」）』` 等收尾类），有些不能出现在行尾（`「（『【` 等起始类）。
//! 不处理的话，一行会以逗号开头——中文读者一眼就看得出不对。
//!
//! 这里实现的是**最小可用**的一套：行首禁则 + 行尾禁则。
//! 完整的 UAX #14 有几十个类别，绝大多数是本引擎用不到的书写系统。
//! 这一点在 [`BreakClass`] 的文档里写明了，不假装做完了。

/// 一个字符在断行上的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakClass {
    /// 空白。它自己被吃掉，两侧可以断。
    Space,
    /// 强制换行（`\n`）。
    Mandatory,
    /// 表意文字（CJK 汉字、假名、全角符号）。前后都能断。
    Ideographic,
    /// 不能出现在**行首**的字符：`。，、）」` 之类。
    NoLineStart,
    /// 不能出现在**行尾**的字符：`（「【` 之类。
    NoLineEnd,
    /// 其余（拉丁字母、数字、标点）。只能靠空格断。
    Other,
}

/// 判断一个字符属于哪一类。
///
/// **不是完整的 UAX #14。** 覆盖的是拉丁 + CJK 这两种本引擎实际会遇到的
/// 情况；阿拉伯语、天城文这类需要复杂整形的书写系统不在范围内，
/// 它们光有断行也排不出正确结果。
pub fn break_class(c: char) -> BreakClass {
    match c {
        '\n' | '\r' => BreakClass::Mandatory,
        // 不换行空格故意不算 Space：它存在的意义就是「别在这里断」。
        '\u{00A0}' | '\u{202F}' => BreakClass::Other,
        c if c.is_whitespace() => BreakClass::Space,

        // 行首禁则：收尾类标点与小书写假名。
        '。' | '、' | '，' | '．' | '：' | '；' | '？' | '！' | '）' | '】' | '》' | '」'
        | '』' | '〉' | '〕' | '｝' | '〗' | '”' | '’' | '·' | 'ー' | '～' | '…' => {
            BreakClass::NoLineStart
        }

        // 行尾禁则：起始类标点。
        '（' | '【' | '《' | '「' | '『' | '〈' | '〔' | '｛' | '〖' | '“' | '‘' => {
            BreakClass::NoLineEnd
        }

        c if is_ideographic(c) => BreakClass::Ideographic,
        _ => BreakClass::Other,
    }
}

/// 是不是「一个字占一格、可以逐字断行」的表意文字。
pub fn is_ideographic(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF      // 谚文字母
        | 0x2E80..=0x2EFF    // 康熙部首补充
        | 0x3000..=0x303F    // CJK 符号与标点
        | 0x3040..=0x309F    // 平假名
        | 0x30A0..=0x30FF    // 片假名
        | 0x3130..=0x318F    // 谚文兼容字母
        | 0x3400..=0x4DBF    // 扩展 A
        | 0x4E00..=0x9FFF    // 基本区
        | 0xAC00..=0xD7AF    // 谚文音节
        | 0xF900..=0xFAFF    // 兼容表意文字
        | 0xFF00..=0xFF60    // 全角形式
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x2FA1F  // 扩展 B~F 与兼容补充
    )
}

/// 一个断行机会。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakOpportunity {
    /// 断点在字符串里的字节偏移，断在这个位置**之前**。
    pub offset: usize,
    /// 是不是强制断（`\n`）。
    pub mandatory: bool,
}

/// 找出一段文本里所有能断行的位置。
///
/// 返回的偏移是**字节**偏移，且一定落在字符边界上。
/// 不含 0（行首不算断点），含文本末尾之前的所有机会。
pub fn break_opportunities(text: &str) -> Vec<BreakOpportunity> {
    let mut out = Vec::new();
    let mut previous: Option<(char, BreakClass)> = None;

    for (offset, c) in text.char_indices() {
        let class = break_class(c);

        if let Some((_, prev_class)) = previous {
            let can_break = match (prev_class, class) {
                // `\n` 之后必断，而且是强制的。
                (BreakClass::Mandatory, _) => {
                    out.push(BreakOpportunity {
                        offset,
                        mandatory: true,
                    });
                    previous = Some((c, class));
                    continue;
                }
                // 换行符本身之前不断——断了会把 `\n` 挤到下一行行首。
                (_, BreakClass::Mandatory) => false,

                // 空白之后可以断，断点落在空白之后（空白留在上一行行尾）。
                (BreakClass::Space, BreakClass::Space) => false,
                (BreakClass::Space, _) => true,
                // 空白之前不断：那会让行首出现一个空格。
                (_, BreakClass::Space) => false,

                // 行尾禁则优先级最高：`（` 后面绝不能断。
                (BreakClass::NoLineEnd, _) => false,
                // 行首禁则：`。` 前面绝不能断。
                (_, BreakClass::NoLineStart) => false,

                // 表意文字之间随便断。收尾类标点之后也能断
                // （`」` 后面接下一句，是正常的断点）。
                (BreakClass::Ideographic, BreakClass::Ideographic)
                | (BreakClass::Ideographic, BreakClass::NoLineEnd)
                | (BreakClass::NoLineStart, BreakClass::Ideographic)
                | (BreakClass::NoLineStart, BreakClass::NoLineEnd) => true,

                // 拉丁与 CJK 交界处也能断——中英混排时英文单词整体移到下一行。
                (BreakClass::Ideographic, BreakClass::Other)
                | (BreakClass::Other, BreakClass::Ideographic)
                | (BreakClass::NoLineStart, BreakClass::Other) => true,

                // 拉丁词内部不断。没有连字符断词——那需要词典。
                (BreakClass::Other, BreakClass::Other)
                | (BreakClass::Other, BreakClass::NoLineEnd) => false,
            };

            if can_break {
                out.push(BreakOpportunity {
                    offset,
                    mandatory: false,
                });
            }
        }

        previous = Some((c, class));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把断点位置画成一串 `|`，方便一眼看出断在哪。
    fn marked(text: &str) -> String {
        let breaks = break_opportunities(text);
        let mut out = String::new();
        for (offset, c) in text.char_indices() {
            if breaks.iter().any(|b| b.offset == offset) {
                out.push('|');
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn latin_breaks_only_at_spaces() {
        assert_eq!(marked("hello world foo"), "hello |world |foo");
    }

    #[test]
    fn a_latin_word_is_never_split() {
        // 没有词典就没法连字符断词，整词移行是唯一正确的行为。
        assert!(break_opportunities("supercalifragilistic").is_empty());
    }

    #[test]
    fn cjk_breaks_between_every_character() {
        // 中文没有空格。只按空格断的话整段会挤成一行冲出屏幕。
        assert_eq!(marked("中文换行"), "中|文|换|行");
    }

    #[test]
    fn closing_punctuation_never_starts_a_line() {
        // 行首出现「。」或「，」，中文读者一眼就看得出不对。
        assert_eq!(marked("你好。世界"), "你|好。|世|界");
        assert_eq!(marked("甲，乙"), "甲，|乙");
    }

    #[test]
    fn opening_punctuation_never_ends_a_line() {
        // 「（」不能被落在行尾。
        assert_eq!(marked("看（注）"), "看|（注）");
    }

    #[test]
    fn a_run_of_closing_punctuation_stays_together() {
        // 「。」「）」连着出现时，中间也不能断，否则第二个照样跑到行首。
        assert_eq!(marked("话）。下"), "话）。|下");
    }

    #[test]
    fn mixed_scripts_break_at_the_boundary() {
        // 中英混排：英文单词整体移到下一行，而不是从中间劈开。
        assert_eq!(marked("中文abc英文"), "中|文|abc|英|文");
    }

    #[test]
    fn newline_forces_a_break() {
        let breaks = break_opportunities("a\nb");
        assert_eq!(breaks.len(), 1);
        assert!(breaks[0].mandatory);
        // 断点在 `\n` 之后：`\n` 留在上一行，不会跑到下一行行首。
        assert_eq!(breaks[0].offset, 2);
    }

    #[test]
    fn consecutive_spaces_produce_one_opportunity() {
        // 「a   b」只该有一个断点，不是三个。
        let breaks = break_opportunities("a   b");
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0].offset, 4);
    }

    #[test]
    fn a_non_breaking_space_does_not_break() {
        // 不换行空格存在的全部意义就是「别在这里断」。
        assert!(break_opportunities("10\u{00A0}km").is_empty());
    }

    #[test]
    fn offsets_land_on_character_boundaries() {
        // 偏移落在多字节字符中间的话，切片会直接 panic。
        let text = "中文 mixed 排版。测试";
        for b in break_opportunities(text) {
            assert!(text.is_char_boundary(b.offset), "偏移 {} 不在字符边界", b.offset);
        }
    }

    #[test]
    fn no_opportunity_at_the_start() {
        // 位置 0 是行首，不是断点；混进去会产生一个空行。
        assert!(break_opportunities("中文").iter().all(|b| b.offset > 0));
        assert!(break_opportunities(" 前导空格").iter().all(|b| b.offset > 0));
    }
}
