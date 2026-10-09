//! Fluent（`.ftl`）的一个够用的子集：解析成「键 → 带占位符的文本」。
//!
//! 支持的：
//!
//! ```ftl
//! # 注释（`#` / `##` / `###` 开头的行）
//! menu-start = 开始游戏
//! greeting = 你好，{ $name }！
//! # 值可以跨行：后续行缩进，按一个空格接上（Fluent 的规则）
//! intro =
//!     很久很久以前，
//!     在一片海上。
//! # 属性：`.名字 = 值`，查的时候用 `键.名字`
//! button-quit = 退出
//!     .tooltip = 回到桌面
//! ```
//!
//! 不支持的（遇到时原样当文本，不报错）：选择器（`{ $n -> [one] … *[other] … }`）、
//! 词条引用（`{ -brand }`）、函数（`NUMBER()`）。游戏界面里的文案绝大多数用不上；
//! 真要复数形式就写两个键。

use std::collections::HashMap;

/// 一条消息：一段段文本和占位符。
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    /// `{ $name }`
    Variable(String),
}

/// 解析出来的一张表。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Messages {
    pub(crate) entries: HashMap<String, Vec<Piece>>,
}

/// 解析错误：哪一行、什么问题。
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// 从 1 开始的行号。
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "第 {} 行：{}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

impl Messages {
    pub fn get(&self, key: &str) -> Option<&[Piece]> {
        self.entries.get(key).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 解析一段 `.ftl` 文本。
pub fn parse(source: &str) -> Result<Messages, ParseError> {
    // 先按行收成 (键, 原始值文本)：值的续行（缩进的行）接在后面。
    let mut raw: Vec<(String, String, usize)> = Vec::new();
    // 当前消息的键（属性续行要挂到它下面）。
    let mut message: Option<String> = None;
    for (index, line) in source.lines().enumerate() {
        let number = index + 1;
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented && line.starts_with('#') {
            continue;
        }
        if indented {
            let content = line.trim();
            if let Some(attribute) = content.strip_prefix('.') {
                // 属性：`.tooltip = 值` → 键「消息.tooltip」。
                let Some(parent) = &message else {
                    return Err(ParseError {
                        line: number,
                        message: "属性前面没有消息".into(),
                    });
                };
                let Some((name, value)) = attribute.split_once('=') else {
                    return Err(ParseError {
                        line: number,
                        message: "属性缺 `=`".into(),
                    });
                };
                let name = name.trim();
                if !is_identifier(name) {
                    return Err(ParseError {
                        line: number,
                        message: format!("属性名不合法：{name}"),
                    });
                }
                raw.push((format!("{parent}.{name}"), value.trim().to_string(), number));
                continue;
            }
            // 续行：接到上一条（消息或属性）的值后面。
            let Some(last) = raw.last_mut() else {
                return Err(ParseError {
                    line: number,
                    message: "缩进的行前面没有消息".into(),
                });
            };
            if !last.1.is_empty() {
                last.1.push(' ');
            }
            last.1.push_str(content);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(ParseError {
                line: number,
                message: "缺 `=`（`键 = 值`）".into(),
            });
        };
        let key = key.trim();
        if !is_identifier(key) {
            return Err(ParseError {
                line: number,
                message: format!("键不合法：{key}（字母开头，字母数字、`-`、`_`）"),
            });
        }
        message = Some(key.to_string());
        raw.push((key.to_string(), value.trim().to_string(), number));
    }

    let mut entries = HashMap::new();
    for (key, value, line) in raw {
        let pieces = parse_value(&value).map_err(|message| ParseError { line, message })?;
        if entries.insert(key.clone(), pieces).is_some() {
            return Err(ParseError {
                line,
                message: format!("键重复：{key}"),
            });
        }
    }
    Ok(Messages { entries })
}

/// 把值切成文本和 `{ $变量 }`。认不出的 `{ … }` 原样留作文本。
fn parse_value(value: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = value;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            return Err("`{` 没有配对的 `}`".into());
        };
        let inner = rest[open + 1..open + close].trim();
        match inner.strip_prefix('$') {
            Some(name) if is_identifier(name) => {
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Variable(name.to_string()));
            }
            // 字符串字面量 `{ "{" }`：Fluent 里转义花括号的写法。
            _ if inner.len() >= 2 && inner.starts_with('"') && inner.ends_with('"') => {
                text.push_str(&inner[1..inner.len() - 1]);
            }
            _ => text.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(messages: &Messages, key: &str) -> Vec<Piece> {
        messages
            .get(key)
            .unwrap_or_else(|| panic!("缺键 {key}"))
            .to_vec()
    }

    #[test]
    fn parses_messages_variables_and_comments() {
        let messages = parse(
            "# 注释\n## 分组注释\nmenu-start = 开始游戏\ngreeting = 你好，{ $name }！共 {$count} 条\n\nempty =\n",
        )
        .unwrap();
        assert_eq!(
            text(&messages, "menu-start"),
            vec![Piece::Text("开始游戏".into())]
        );
        assert_eq!(
            text(&messages, "greeting"),
            vec![
                Piece::Text("你好，".into()),
                Piece::Variable("name".into()),
                Piece::Text("！共 ".into()),
                Piece::Variable("count".into()),
                Piece::Text(" 条".into()),
            ]
        );
        assert_eq!(text(&messages, "empty"), Vec::<Piece>::new());
    }

    #[test]
    fn continuation_lines_and_attributes() {
        let messages = parse(
            "intro =\n    很久很久以前，\n    在一片海上。\nquit = 退出\n    .tooltip = 回到桌面\n",
        )
        .unwrap();
        assert_eq!(
            text(&messages, "intro"),
            vec![Piece::Text("很久很久以前， 在一片海上。".into())]
        );
        assert_eq!(text(&messages, "quit"), vec![Piece::Text("退出".into())]);
        assert_eq!(
            text(&messages, "quit.tooltip"),
            vec![Piece::Text("回到桌面".into())]
        );
    }

    #[test]
    fn unsupported_syntax_stays_as_text_and_escapes_work() {
        let messages = parse("brand = { -brand } 和 { \"{\" }括号\n").unwrap();
        assert_eq!(
            text(&messages, "brand"),
            vec![Piece::Text("{ -brand } 和 {括号".into())]
        );
    }

    #[test]
    fn errors_point_at_the_line() {
        assert_eq!(parse("ok = 1\n不是键值\n").unwrap_err().line, 2);
        assert_eq!(parse("a = 1\na = 2\n").unwrap_err().line, 2);
        assert_eq!(parse("a = { $x\n").unwrap_err().line, 1);
        assert_eq!(parse("    .tooltip = x\n").unwrap_err().line, 1);
    }
}
