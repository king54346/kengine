//! 寻仙配置文件（`.cct` / `.cmf`）用的极简 XML 读取。
//!
//! 文件是带 BOM 的 UTF-16LE，结构只有两层：顶层一串元素，有的带属性（`<Model .../>`），
//! 有的只有文字（`<Skeleton>路径</Skeleton>`），`.cmf` 里的 `<Material>` 再包一层子元素。
//! 所以这里不建树，只按出现顺序吐出一串扁平的元素；需要层级的地方由调用方按「开始 / 结束」自己跟踪。

/// 一个元素：标签、属性、紧跟在开始标签后面的文字。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    /// 开始标签和下一个标签之间的文字（去掉首尾空白）。
    pub text: String,
    /// 是否是结束标签（`</Material>`）。
    pub closing: bool,
}

impl Element {
    /// 按名取属性。
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// 把文件字节解码成字符串：有 UTF-16LE 的 BOM 就按 UTF-16 解，否则按 UTF-8（容错）。
pub fn decode(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 按出现顺序列出所有元素（跳过声明和注释）。
pub fn elements(text: &str) -> Vec<Element> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        if rest.starts_with('?') || rest.starts_with('!') {
            let end = if rest.starts_with("!--") {
                rest.find("-->").map(|i| i + 3)
            } else {
                rest.find('>').map(|i| i + 1)
            };
            rest = &rest[end.unwrap_or(rest.len())..];
            continue;
        }
        let Some(end) = rest.find('>') else { break };
        let inner = &rest[..end];
        rest = &rest[end + 1..];
        let closing = inner.starts_with('/');
        let inner = inner.trim_start_matches('/').trim_end_matches('/');
        let tag_end = inner
            .find(|c: char| c.is_whitespace())
            .unwrap_or(inner.len());
        let mut element = Element {
            tag: inner[..tag_end].to_string(),
            closing,
            ..Default::default()
        };
        let mut attrs = &inner[tag_end..];
        while let Some(eq) = attrs.find('=') {
            let key = attrs[..eq].trim().to_string();
            let after = attrs[eq + 1..].trim_start();
            let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let Some(close) = after[1..].find(quote) else {
                break;
            };
            element.attrs.push((key, unescape(&after[1..1 + close])));
            attrs = &after[close + 2..];
        }
        if !closing && !rest.is_empty() {
            let text_end = rest.find('<').unwrap_or(rest.len());
            element.text = unescape(rest[..text_end].trim());
        }
        out.push(element);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_attributes_text_and_nesting() {
        let text = r#"<?xml version="1.0"?>
<Character>
  <Scaling>1.0000</Scaling>
  <Skeleton/>
  <Model MatName="c_a" MeshFile="$(res)\a.pmf" Name="m_a"/>
  <Material Name="x"><BaseMap0>p&amp;q.dds</BaseMap0></Material>
</Character>"#;
        let list = elements(text);
        let tags: Vec<_> = list.iter().map(|e| (e.tag.as_str(), e.closing)).collect();
        assert_eq!(
            tags,
            [
                ("Character", false),
                ("Scaling", false),
                ("Scaling", true),
                ("Skeleton", false),
                ("Model", false),
                ("Material", false),
                ("BaseMap0", false),
                ("BaseMap0", true),
                ("Material", true),
                ("Character", true),
            ]
        );
        assert_eq!(list[1].text, "1.0000");
        assert_eq!(list[4].attr("MeshFile"), Some("$(res)\\a.pmf"));
        assert_eq!(list[6].text, "p&q.dds");
    }

    #[test]
    fn decodes_utf16_with_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "<A b=\"中\"/>".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let list = elements(&decode(&bytes));
        assert_eq!(list[0].attr("b"), Some("中"));
    }
}
