//! 标记语言解析：XML 的一个子集。
//!
//! 参照 [bevy_hui](https://github.com/Lommix/bevy_hui) 的做法，界面用
//! 标记文件写而不是用 Rust 调用堆出来：
//!
//! ```xml
//! <template>
//!     <property name="title">设置</property>
//!     <node direction="column" padding="12" gap="6">
//!         <text size="18">{title}</text>
//!         <button on_press="close">关闭</button>
//!     </node>
//! </template>
//! ```
//!
//! # 为什么值得引入一门标记语言
//!
//! 用 Rust 堆界面的代价，在移植 three.js 那个骨骼动画例子时非常直观：
//! 一个设置面板花了两百多行 Rust，而且**改一个边距要重新编译整个引擎**。
//! 标记文件是资源，改完存盘就能看到——迭代速度差一个数量级。
//!
//! # 只做 XML 的一个子集
//!
//! 支持：元素、属性、自闭合标签、文本内容、注释、五个基本实体。
//!
//! **不支持**：命名空间、DTD、CDATA、处理指令、字符实体（`&#65;`）。
//! 这些在界面模板里一个都用不上，而每一样都要一段专门的代码和一组
//! 专门的边界情况。
//!
//! # 报错必须带行号
//!
//! 模板是**运行时**加载的文件，写错了不会有编译器拦着。报错不带位置的话，
//! 一个几百行的模板里少了个引号，只能靠二分查找。

use std::fmt;

/// 一个解析出来的元素。
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// 标签名，例如 `node`、`button`。
    pub tag: String,
    /// 属性表，按出现顺序。
    ///
    /// 用 `Vec` 而不是 `HashMap`：属性通常只有几个，线性查找更快，
    /// 而且保留顺序让报错信息能指回源文件里的位置。
    pub attributes: Vec<Attribute>,
    /// 子元素。
    pub children: Vec<Element>,
    /// 直接包含的文本。
    ///
    /// 只保留**紧贴**在这个元素里的文本，不含子元素里的。混排
    /// （`<a>文字<b/>更多文字</a>`）会把两段文本拼起来——界面模板里
    /// 不该出现这种写法，拼起来比报错更宽容。
    pub text: String,
    /// 开标签在源文件里的行号，从 1 开始。报错和调试用。
    pub line: usize,
}

impl Element {
    /// 建一个只有标签名的元素，测试和程序化构造用。
    pub fn new(tag: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            attributes: Vec::new(),
            children: Vec::new(),
            text: String::new(),
            line: 0,
        }
    }

    /// 取一个属性的值，没有时返回 [`None`]。
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_str())
    }

    /// 按标签名找第一个直接子元素。
    pub fn child(&self, tag: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.tag == tag)
    }

    /// 按标签名找全部直接子元素。
    pub fn children_named<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.tag == tag)
    }
}

/// 一个属性。
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// 属性名。
    pub name: String,
    /// 属性值（已经解过实体）。
    pub value: String,
    /// 所在行号，从 1 开始。
    pub line: usize,
}

/// 解析失败。
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// 出错位置的行号，从 1 开始。
    pub line: usize,
    /// 出错位置的列号，从 1 开始（按字符数，不是字节数）。
    pub column: usize,
    /// 出了什么事。
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "第 {} 行第 {} 列：{}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

/// 解析一份模板，返回根元素。
///
/// 源文件必须**恰好有一个**根元素。零个或多个都会报错——多个根元素时
/// 「界面从哪儿开始」没有定义，而静默取第一个会让后面的内容凭空消失。
pub fn parse(source: &str) -> Result<Element, ParseError> {
    Parser::new(source).parse_document()
}

/// 嵌套深度上限。
///
/// 防的是写错的模板（比如自动生成时漏了闭合），而不是正常的界面——
/// 真实界面套到十几层已经很深了。没有这道闸的话，一个几千层的输入会
/// 在递归里爆栈，表现为**整个进程直接消失**，没有 panic 也没有日志。
///
/// 解析本身是迭代的，这个上限保护的是后续按树递归的代码。
pub const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    source: &'a str,
    /// 当前位置的**字节**偏移。
    offset: usize,
    line: usize,
    /// 当前行起点的字节偏移，用来算列号。
    line_start: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            line: 1,
            line_start: 0,
        }
    }

    fn parse_document(&mut self) -> Result<Element, ParseError> {
        self.skip_trivia();
        if self.at_end() {
            return Err(self.error("模板是空的，至少要有一个根元素"));
        }

        let root = self.parse_element()?;

        self.skip_trivia();
        if !self.at_end() {
            return Err(self.error(
                "根元素之后还有内容。一份模板只能有一个根元素——\
                 有多个的话「界面从哪儿开始」没有定义",
            ));
        }
        Ok(root)
    }

    /// 解析一个元素。**迭代**而不是递归：递归版本在 debug 构建下
    /// 几十层就会爆栈，而模板是外部文件，深度不受控。
    fn parse_element(&mut self) -> Result<Element, ParseError> {
        let mut stack: Vec<Element> = Vec::new();

        loop {
            self.skip_trivia();

            // 闭合标签。
            if self.starts_with("</") {
                let line = self.line;
                self.advance_by(2);
                let name = self.parse_name()?;
                self.skip_whitespace();
                self.expect('>')?;

                let Some(done) = stack.pop() else {
                    return Err(self.error_at(line, format!("多余的闭合标签 `</{name}>`")));
                };
                if done.tag != name {
                    return Err(self.error_at(
                        line,
                        format!(
                            "闭合标签对不上：开的是 `<{}>`，闭的是 `</{name}>`",
                            done.tag
                        ),
                    ));
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Ok(done),
                }
                continue;
            }

            // 开标签。
            if self.starts_with("<") {
                let (element, self_closing) = self.parse_open_tag()?;
                if self_closing {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(element),
                        // 自闭合的根元素：整份文档就是它。
                        None => return Ok(element),
                    }
                } else {
                    if stack.len() >= MAX_DEPTH {
                        return Err(self.error(format!(
                            "嵌套超过 {MAX_DEPTH} 层。模板写错了？\
                             （比如自动生成时漏了闭合标签）"
                        )));
                    }
                    stack.push(element);
                }
                continue;
            }

            // 文本。
            if self.at_end() {
                return match stack.last() {
                    Some(open) => {
                        Err(self.error(format!("到文件末尾了，`<{}>` 还没闭合", open.tag)))
                    }
                    None => Err(self.error("这里应该是一个元素")),
                };
            }

            let text = self.parse_text()?;
            match stack.last_mut() {
                Some(parent) => {
                    if !parent.text.is_empty() && !text.is_empty() {
                        parent.text.push(' ');
                    }
                    parent.text.push_str(&text);
                }
                None if !text.is_empty() => {
                    return Err(self.error("根元素外面不能有文本"));
                }
                None => {}
            }
        }
    }

    /// 解析开标签，返回元素与「是不是自闭合」。
    fn parse_open_tag(&mut self) -> Result<(Element, bool), ParseError> {
        let line = self.line;
        self.expect('<')?;
        let tag = self.parse_name()?;

        let mut attributes = Vec::new();
        loop {
            self.skip_whitespace();

            if self.starts_with("/>") {
                self.advance_by(2);
                return Ok((
                    Element {
                        tag,
                        attributes,
                        children: Vec::new(),
                        text: String::new(),
                        line,
                    },
                    true,
                ));
            }
            if self.starts_with(">") {
                self.advance_by(1);
                return Ok((
                    Element {
                        tag,
                        attributes,
                        children: Vec::new(),
                        text: String::new(),
                        line,
                    },
                    false,
                ));
            }
            if self.at_end() {
                return Err(self.error_at(line, format!("`<{tag}` 没有闭合")));
            }

            attributes.push(self.parse_attribute()?);
        }
    }

    fn parse_attribute(&mut self) -> Result<Attribute, ParseError> {
        let line = self.line;
        let name = self.parse_name()?;
        self.skip_whitespace();
        self.expect('=')?;
        self.skip_whitespace();

        let quote = match self.peek() {
            Some(c @ ('"' | '\'')) => c,
            // 明确拦掉不带引号的值。XML 不允许，而且 `width=100px` 这种
            // 写法在别处很常见，不报错的话会解析出莫名其妙的结果。
            _ => {
                return Err(self.error(format!("属性 `{name}` 的值要用引号括起来")));
            }
        };
        self.advance_by(quote.len_utf8());

        let start = self.offset;
        while let Some(c) = self.peek() {
            if c == quote {
                break;
            }
            self.advance_char(c);
        }
        if self.at_end() {
            return Err(self.error_at(line, format!("属性 `{name}` 的引号没有闭合")));
        }
        let raw = &self.source[start..self.offset];
        self.advance_by(quote.len_utf8());

        Ok(Attribute {
            name,
            value: decode_entities(raw),
            line,
        })
    }

    /// 读一段文本，直到下一个 `<`。
    fn parse_text(&mut self) -> Result<String, ParseError> {
        let start = self.offset;
        while let Some(c) = self.peek() {
            if c == '<' {
                break;
            }
            self.advance_char(c);
        }
        let raw = &self.source[start..self.offset];
        // 折叠空白：模板里为了排版加的换行和缩进不该出现在界面上。
        Ok(collapse_whitespace(&decode_entities(raw)))
    }

    fn parse_name(&mut self) -> Result<String, ParseError> {
        let start = self.offset;
        while let Some(c) = self.peek() {
            // 允许 `hover:background` 这种带前缀的名字。
            if c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.') {
                self.advance_char(c);
            } else {
                break;
            }
        }
        if start == self.offset {
            return Err(self.error("这里应该是一个标签名或属性名"));
        }
        Ok(self.source[start..self.offset].to_string())
    }

    /// 跳过空白与注释。
    fn skip_trivia(&mut self) {
        loop {
            self.skip_whitespace();
            if !self.starts_with("<!--") {
                return;
            }
            self.advance_by(4);
            // 找 `-->`。找不到就一路吃到末尾——未闭合的注释后面的内容
            // 本来也不该被当成界面。
            while !self.at_end() && !self.starts_with("-->") {
                if let Some(c) = self.peek() {
                    self.advance_char(c);
                }
            }
            if self.starts_with("-->") {
                self.advance_by(3);
            }
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.advance_char(c);
            } else {
                return;
            }
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), ParseError> {
        match self.peek() {
            Some(c) if c == expected => {
                self.advance_char(c);
                Ok(())
            }
            Some(c) => Err(self.error(format!("这里应该是 `{expected}`，实际是 `{c}`"))),
            None => Err(self.error(format!("这里应该是 `{expected}`，但已经到文件末尾了"))),
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.source[self.offset..].starts_with(prefix)
    }

    fn at_end(&self) -> bool {
        self.offset >= self.source.len()
    }

    /// 前进一个字符，顺带维护行号。
    fn advance_char(&mut self, c: char) {
        self.offset += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.line_start = self.offset;
        }
    }

    /// 前进固定字节数。只用在已知是 ASCII 的地方。
    fn advance_by(&mut self, bytes: usize) {
        let end = (self.offset + bytes).min(self.source.len());
        while self.offset < end {
            let Some(c) = self.peek() else { break };
            self.advance_char(c);
        }
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            line: self.line,
            column: self.column(),
            message: message.into(),
        }
    }

    fn error_at(&self, line: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            line,
            column: 1,
            message: message.into(),
        }
    }

    /// 当前列号，按**字符**数——中文标签名下按字节算会给出错的位置。
    fn column(&self) -> usize {
        self.source[self.line_start..self.offset].chars().count() + 1
    }
}

/// 解 XML 的五个基本实体。
///
/// 只做这五个：数字实体（`&#65;`）在界面模板里没有用武之地，
/// 而支持它要处理进制、越界、代理对一整套边界情况。
fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        // 绝大多数属性和文本里没有实体，跳过整趟扫描。
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];

        let replacement = [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&apos;", "'"),
        ]
        .into_iter()
        .find(|(entity, _)| rest.starts_with(entity));

        match replacement {
            Some((entity, text)) => {
                out.push_str(text);
                rest = &rest[entity.len()..];
            }
            // 不认识的实体原样保留。报错太苛刻——文案里出现一个孤零零的
            // `&` 是很正常的事。
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 把连续空白折叠成一个空格，并去掉首尾空白。
///
/// 模板里为了排版加的换行和缩进不该出现在界面上。
fn collapse_whitespace(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_space = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_element_parses() {
        let root = parse("<node/>").unwrap();
        assert_eq!(root.tag, "node");
        assert!(root.children.is_empty());
    }

    #[test]
    fn attributes_are_read_in_order() {
        let root = parse(r#"<node width="100" height="50"/>"#).unwrap();
        assert_eq!(root.attribute("width"), Some("100"));
        assert_eq!(root.attribute("height"), Some("50"));
        assert_eq!(root.attributes[0].name, "width");
        assert_eq!(root.attribute("missing"), None);
    }

    #[test]
    fn single_quotes_work_too() {
        let root = parse("<node title='hi there'/>").unwrap();
        assert_eq!(root.attribute("title"), Some("hi there"));
    }

    #[test]
    fn children_nest() {
        let root = parse("<node><button/><text/></node>").unwrap();
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].tag, "button");
        assert_eq!(root.children[1].tag, "text");
    }

    #[test]
    fn text_content_is_captured() {
        let root = parse("<text>Hello</text>").unwrap();
        assert_eq!(root.text, "Hello");
    }

    #[test]
    fn indentation_does_not_leak_into_the_text() {
        // 模板里为了排版加的换行和缩进不该出现在界面上。
        let root = parse("<text>\n    Hello\n    world\n</text>").unwrap();
        assert_eq!(root.text, "Hello world");
    }

    #[test]
    fn comments_are_skipped() {
        let root = parse("<!-- 说明 --><node><!-- 里面 --><button/></node>").unwrap();
        assert_eq!(root.tag, "node");
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn an_unclosed_comment_swallows_the_rest() {
        // 未闭合的注释后面的内容本来也不该被当成界面。
        // 这里只要求不崩、不把注释内容当元素。
        assert!(parse("<node/><!-- 没关").is_ok());
    }

    #[test]
    fn entities_are_decoded() {
        let root = parse(r#"<text title="a &lt; b &amp; c">x &gt; y</text>"#).unwrap();
        assert_eq!(root.attribute("title"), Some("a < b & c"));
        assert_eq!(root.text, "x > y");
    }

    #[test]
    fn a_lone_ampersand_is_kept() {
        // 文案里出现一个孤零零的 `&` 是很正常的事，报错太苛刻。
        let root = parse("<text>Tom &amp Jerry</text>").unwrap();
        assert!(root.text.contains('&'));
    }

    #[test]
    fn prefixed_attribute_names_are_allowed() {
        // `hover:background` 这种带前缀的名字要能读出来。
        // 用 `r##` 而不是 `r#`：颜色值里的 `"#` 会把 `r#"` 提前收掉。
        let root = parse(r##"<button hover:background="#fff"/>"##).unwrap();
        assert_eq!(root.attribute("hover:background"), Some("#fff"));
    }

    #[test]
    fn a_realistic_template_parses() {
        let source = r#"
            <template>
                <property name="title">设置</property>
                <node direction="column" padding="12" gap="6">
                    <text size="18">{title}</text>
                    <button on_press="close">关闭</button>
                </node>
            </template>
        "#;
        let root = parse(source).unwrap();

        assert_eq!(root.tag, "template");
        assert_eq!(root.child("property").unwrap().text, "设置");

        let node = root.child("node").unwrap();
        assert_eq!(node.attribute("padding"), Some("12"));
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[1].attribute("on_press"), Some("close"));
        assert_eq!(node.children[1].text, "关闭");
    }

    // ── 报错 ──

    #[test]
    fn an_unclosed_tag_is_an_error() {
        let error = parse("<node>").unwrap_err();
        assert!(error.message.contains("node"), "{}", error.message);
    }

    #[test]
    fn mismatched_tags_name_both_sides() {
        // 报错要同时说出开的和闭的是什么，不然在几百行模板里根本找不到。
        let error = parse("<node></button>").unwrap_err();
        assert!(error.message.contains("node"), "{}", error.message);
        assert!(error.message.contains("button"), "{}", error.message);
    }

    #[test]
    fn an_unquoted_attribute_value_is_rejected() {
        // `width=100px` 在别处很常见，不报错的话会解析出莫名其妙的结果。
        let error = parse("<node width=100/>").unwrap_err();
        assert!(error.message.contains("引号"), "{}", error.message);
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        assert!(parse(r#"<node width="100/>"#).is_err());
    }

    #[test]
    fn several_root_elements_are_rejected() {
        // 静默取第一个的话，后面的内容会凭空消失。
        let error = parse("<node/><node/>").unwrap_err();
        assert!(error.message.contains("一个根元素"), "{}", error.message);
    }

    #[test]
    fn an_empty_document_is_rejected() {
        assert!(parse("").is_err());
        assert!(parse("   \n  ").is_err());
        assert!(parse("<!-- 只有注释 -->").is_err());
    }

    #[test]
    fn a_stray_closing_tag_is_an_error() {
        assert!(parse("</node>").is_err());
    }

    // ── 位置信息 ──

    #[test]
    fn errors_carry_the_line_number() {
        // 模板是运行时加载的文件，写错了没有编译器拦着。报错不带位置的话，
        // 几百行里少个引号只能靠二分查找。
        let source = "<node>\n  <a/>\n  <b/>\n  <c width=1/>\n</node>";
        let error = parse(source).unwrap_err();
        assert_eq!(error.line, 4, "行号不对：{error}");
    }

    #[test]
    fn elements_remember_their_line() {
        let root = parse("<node>\n  <button/>\n</node>").unwrap();
        assert_eq!(root.line, 1);
        assert_eq!(root.children[0].line, 2);
    }

    #[test]
    fn the_column_counts_characters_not_bytes() {
        // 中文标签名下按字节算会给出错的位置。
        let error = parse("<节点 width=1/>").unwrap_err();
        // `<节点 width=` 是 11 个字符，出错位置在其后。
        assert!(error.column <= 13, "列号 {} 像是按字节算的", error.column);
    }

    // ── 退化输入 ──

    #[test]
    fn deep_nesting_is_rejected_rather_than_crashing() {
        // 没有上限的话，一个几千层的输入会在后续按树递归的代码里爆栈——
        // 表现为整个进程直接消失，没有 panic 也没有日志。
        let deep = "<n>".repeat(MAX_DEPTH + 10);
        let error = parse(&deep).unwrap_err();
        assert!(error.message.contains("嵌套"), "{}", error.message);
    }

    #[test]
    fn nesting_just_under_the_limit_still_works() {
        let depth = MAX_DEPTH - 1;
        let source = format!("{}{}", "<n>".repeat(depth), "</n>".repeat(depth));
        assert!(parse(&source).is_ok());
    }

    #[test]
    fn garbage_input_does_not_panic() {
        for source in [
            "<",
            ">",
            "<<>>",
            "<a b/>",
            "<a =\"x\"/>",
            "&",
            "<a>&",
            "<a/>>",
            "< a/>",
            "<a//>",
            "<a>\u{0}</a>",
        ] {
            // 只要求不 panic：模板来自外部文件，什么都可能。
            let _ = parse(source);
        }
    }

    #[test]
    fn text_outside_the_root_is_rejected() {
        assert!(parse("hello <node/>").is_err());
    }
}
