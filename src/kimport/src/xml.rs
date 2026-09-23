//! 一个极小的 XML DOM，给 Collada / 3MF / AMF / KML 这几个 XML 格式共用。
//!
//! 词法交给 `quick-xml`（事件流），这里只把事件搭成一棵树。这几个格式都
//! 要「按 id 找元素、往回引用」（Collada 的 `#geometry-id`、3MF 的
//! `pid`），事件流上做不了，必须先有树。
//!
//! 刻意很朴素：
//!
//! - 名字只保留**本地名**（去掉命名空间前缀）。3MF 的 `m:colorgroup` 和
//!   Collada 的默认命名空间都靠这个统一，而这几个格式里没有真正需要区分
//!   命名空间的地方。
//! - 文本是元素内所有文本段的拼接（实体已还原）。
//! - 不保留注释、处理指令、DTD。

use crate::bad;
use kasset::LoadError;
use quick_xml::events::Event;

/// 一个元素。
#[derive(Debug, Clone, Default)]
pub struct Element {
    /// 本地名（去掉命名空间前缀）。
    pub name: String,
    /// 属性，按出现顺序，键也是本地名。
    pub attributes: Vec<(String, String)>,
    /// 子元素。
    pub children: Vec<Element>,
    /// 直属文本（不含子元素里的文本）。
    pub text: String,
}

impl Element {
    /// 属性值。
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// 解析成数的属性值。
    pub fn attr_f32(&self, name: &str) -> Option<f32> {
        self.attr(name)?.trim().parse().ok()
    }

    /// 解析成整数的属性值。
    pub fn attr_usize(&self, name: &str) -> Option<usize> {
        self.attr(name)?.trim().parse().ok()
    }

    /// 第一个叫 `name` 的子元素。
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// 所有叫 `name` 的子元素。
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// 沿路径往下找：`find("a/b/c")`。
    pub fn find(&self, path: &str) -> Option<&Element> {
        path.split('/').try_fold(self, |node, name| node.child(name))
    }

    /// 子树里第一个叫 `name` 的元素（深度优先，含自己）。
    pub fn descendant(&self, name: &str) -> Option<&Element> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.descendant(name))
    }

    /// 子树里所有叫 `name` 的元素（深度优先，含自己）。
    pub fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        if self.name == name {
            out.push(self);
        }
        for child in &self.children {
            child.descendants(name, out);
        }
    }

    /// 文本按空白切开后解析成一串 `f32`。解析不了的记号当 0。
    pub fn floats(&self) -> Vec<f32> {
        self.text.split_ascii_whitespace().map(|t| t.parse().unwrap_or(0.0)).collect()
    }

    /// 文本按空白切开后解析成一串整数。
    pub fn integers(&self) -> Vec<i64> {
        self.text
            .split_ascii_whitespace()
            .map(|t| t.parse::<i64>().or_else(|_| t.parse::<f64>().map(|f| f as i64)).unwrap_or(0))
            .collect()
    }

    /// 子元素 `name` 的文本，没有时为空串。
    pub fn child_text(&self, name: &str) -> &str {
        self.child(name).map_or("", |c| c.text.trim())
    }
}

fn local(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    match name.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => name.into_owned(),
    }
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let code = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                name.strip_prefix('#')?.parse().ok()?
            };
            char::from_u32(code)?
        }
    })
}

/// 把整份文档解析成根元素。
pub fn parse(bytes: &[u8]) -> Result<Element, LoadError> {
    // 去掉 UTF-8 BOM；UTF-16 的文件这几个格式里没见过，不支持。
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| bad("XML 不是 UTF-8 编码"))?;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<Element> = vec![Element::default()];
    let open = |e: &quick_xml::events::BytesStart<'_>| -> Result<Element, LoadError> {
        let mut element = Element {
            name: local(e.name().as_ref()),
            ..Default::default()
        };
        for attribute in e.attributes().with_checks(false) {
            let attribute = attribute.map_err(|e| bad(format!("XML 属性写坏了：{e}")))?;
            let value = attribute
                .unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&attribute.value).into_owned());
            element.attributes.push((local(attribute.key.as_ref()), value));
        }
        Ok(element)
    };
    let mut depth_guard = 0usize;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| bad(format!("XML 解析失败（第 {} 字节）：{e}", reader.buffer_position())))?;
        match event {
            Event::Start(e) => {
                depth_guard += 1;
                if depth_guard > 4096 {
                    return Err(bad("XML 嵌套过深"));
                }
                stack.push(open(&e)?);
            }
            Event::Empty(e) => {
                let element = open(&e)?;
                stack.last_mut().expect("栈底是虚拟根").children.push(element);
            }
            Event::End(_) => {
                depth_guard = depth_guard.saturating_sub(1);
                if stack.len() > 1 {
                    let element = stack.pop().expect("刚检查过长度");
                    stack.last_mut().expect("栈底是虚拟根").children.push(element);
                }
            }
            Event::Text(e) => {
                let text = e.decode().map_err(|e| bad(format!("XML 文本编码错误：{e}")))?;
                stack.last_mut().expect("栈底是虚拟根").text.push_str(&text);
            }
            Event::CData(e) => {
                stack
                    .last_mut()
                    .expect("栈底是虚拟根")
                    .text
                    .push_str(&String::from_utf8_lossy(&e.into_inner()));
            }
            Event::GeneralRef(e) => {
                let name = String::from_utf8_lossy(&e.into_inner()).into_owned();
                if let Some(c) = entity(&name) {
                    stack.last_mut().expect("栈底是虚拟根").text.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // 没闭合的元素（写坏的文件）也收进树里，尽量多读出一点。
    while stack.len() > 1 {
        let element = stack.pop().expect("刚检查过长度");
        stack.last_mut().expect("栈底是虚拟根").children.push(element);
    }
    let document = stack.pop().expect("栈底是虚拟根");
    document.children.into_iter().next().ok_or_else(|| bad("XML 里没有任何元素"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_tree_with_local_names_and_entities() {
        let root = parse(
            br#"<?xml version="1.0"?><m:model xmlns:m="x" unit="mm"><m:item id="1">a &amp; b &#65;</m:item><empty v="2.5"/></m:model>"#,
        )
        .unwrap();
        assert_eq!(root.name, "model");
        assert_eq!(root.attr("unit"), Some("mm"));
        assert_eq!(root.child("item").unwrap().text, "a & b A");
        assert_eq!(root.child("empty").unwrap().attr_f32("v"), Some(2.5));
    }

    #[test]
    fn number_lists() {
        let root = parse(b"<a> 1 2.5\n -3 </a>").unwrap();
        assert_eq!(root.floats(), [1.0, 2.5, -3.0]);
        assert_eq!(root.integers(), [1, 2, -3]);
    }
}
