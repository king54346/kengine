//! USD 文本层（`.usda`）与 USDZ 包（`.usdz`）。
//!
//! USD 是 Pixar 的场景描述格式，苹果把它打包成 USDZ（一个不压缩的 ZIP，
//! 里面一份 `.usda` / `.usdc` 加上贴图）当 AR 的交换格式。
//!
//! # 支持到哪
//!
//! - **文本层** `.usda`：完整的词法和语法（prim、属性、关系、元数据、
//!   字典、`timeSamples` 取第一帧）；
//! - `Xform` / `Scope` / `Mesh` 组成的节点树，`xformOpOrder` 里的
//!   `translate` `scale` `rotateX/Y/Z` `rotateXYZ`（及其他五种顺序）
//!   `orient` `transform`；
//! - `Mesh`：任意多边形（扇形三角化）、`normals` 和 `primvars:st` 的
//!   `vertex` / `faceVarying` / `uniform` 插值和 `:indices` 索引、
//!   `primvars:displayColor`、`orientation = "leftHanded"`、`GeomSubset`
//!   按面分材质；
//! - `UsdPreviewSurface` 材质：`diffuseColor` `emissiveColor` `metallic`
//!   `roughness` `opacity` `occlusion` `normal`，常量或者连着 `UsdUVTexture`
//!   （`scale` / `bias`、单通道输出 `r/g/b/a`）；
//! - 层元数据 `upAxis = "Z"`（转成 Y 朝上）和 `metersPerUnit`。
//!
//! # 不支持的
//!
//! - **二进制层** `.usdc`（crate 格式：LZ4 + 自定义整数压缩）——遇到会报错；
//! - 组合（`references` / `payload` / `subLayers` / `variantSets`）：只读当前这一层；
//! - 骨骼（`UsdSkel`）、动画、点实例化、曲线。

use crate::{bad, base_dir, loader, sibling, zip};
use kasset::{LoadError, Resource, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::{BlendMode, Material};
use kmath::{Mat4, Quat, Vec2, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use ktexture::{Texture, TextureFormat};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

loader! {
    /// 读 `.usdz` / `.usda`（文本层）。
    UsdLoader -> Model : ["usdz", "usda", "usd"] = MODEL_TYPE_UUID, parse
}

/// 异步加载：USDZ 从包里取层和贴图；`.usda` 的贴图按相对路径去旁边找。
pub async fn parse(
    bytes: Vec<u8>,
    path: PathBuf,
    io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "USD".into());
    if zip::is_zip(&bytes) {
        return parse_usdz(&bytes, &name);
    }
    let stage = Stage::parse(&layer_text(&bytes)?)?;
    let base = base_dir(&path);
    let mut images = HashMap::new();
    for file in stage.asset_paths() {
        if let Some(data) = sibling(&io, &base, &file).await {
            images.insert(file, data);
        }
    }
    Ok(stage.build(&name, &mut |file| images.get(file).cloned()))
}

/// 解析一个 USDZ 包（内存里的字节）。
pub fn parse_usdz(bytes: &[u8], name: &str) -> Result<Model, LoadError> {
    let archive = zip::Archive::open(bytes)?;
    // 规范：包里第一个 USD 文件是根层。
    let layer = archive
        .entries()
        .iter()
        .find(|e| {
            let lower = e.name.to_lowercase();
            lower.ends_with(".usda") || lower.ends_with(".usdc") || lower.ends_with(".usd")
        })
        .ok_or_else(|| bad("USDZ 里没有 USD 层"))?;
    let text = layer_text(&archive.read(layer)?)?;
    let stage = Stage::parse(&text)?;
    let layer_dir = layer
        .name
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/"))
        .unwrap_or_default();
    Ok(stage.build(name, &mut |file| {
        let file = file.trim_start_matches("./");
        archive
            .read_named(&format!("{layer_dir}{file}"))
            .or_else(|| archive.read_named(file))
    }))
}

fn layer_text(bytes: &[u8]) -> Result<String, LoadError> {
    if bytes.starts_with(b"PXR-USDC") {
        return Err(bad(
            "二进制 USD（usdc / crate 格式）暂不支持，请导出成 usda",
        ));
    }
    let text = String::from_utf8_lossy(bytes);
    if !text.trim_start().starts_with("#usda") {
        return Err(bad("不是 USD 文本层（缺 #usda 头）"));
    }
    Ok(text.into_owned())
}

// ── 词法 ──

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Number(f64),
    Str(String),
    Asset(String),
    Path(String),
    Punct(char),
}

fn tokenize(text: &str) -> Result<Vec<Token>, LoadError> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    // 第一行是 `#usda 1.0` 头，当注释跳过。
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let triple = bytes[i..].starts_with(if c == b'"' { b"\"\"\"" } else { b"'''" });
                let (open, close): (usize, &[u8]) = if triple {
                    (3, if c == b'"' { b"\"\"\"" } else { b"'''" })
                } else {
                    (1, if c == b'"' { b"\"" } else { b"'" })
                };
                let start = i + open;
                let mut j = start;
                let mut out = String::new();
                while j < bytes.len() && !bytes[j..].starts_with(close) {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        out.push(match bytes[j + 1] {
                            b'n' => '\n',
                            b't' => '\t',
                            other => other as char,
                        });
                        j += 2;
                        continue;
                    }
                    let ch = text[j..].chars().next().unwrap_or(' ');
                    out.push(ch);
                    j += ch.len_utf8();
                }
                tokens.push(Token::Str(out));
                i = (j + close.len()).min(bytes.len());
            }
            b'@' => {
                let triple = bytes[i..].starts_with(b"@@@");
                let (open, close): (usize, &[u8]) = if triple { (3, b"@@@") } else { (1, b"@") };
                let start = i + open;
                let mut j = start;
                while j < bytes.len() && !bytes[j..].starts_with(close) {
                    j += 1;
                }
                tokens.push(Token::Asset(text[start..j].to_string()));
                i = (j + close.len()).min(bytes.len());
            }
            b'<' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'>' {
                    j += 1;
                }
                tokens.push(Token::Path(text[start..j].to_string()));
                i = j + 1;
            }
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'=' | b',' | b';' => {
                tokens.push(Token::Punct(c as char));
                i += 1;
            }
            b'-' | b'+' | b'.' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'-' | b'+')
                {
                    i += 1;
                }
                let raw = &text[start..i];
                // `-inf` / `-nan`
                if raw == "-" && text[i..].starts_with("inf") {
                    tokens.push(Token::Number(f64::NEG_INFINITY));
                    i += 3;
                    continue;
                }
                let value = raw
                    .parse::<f64>()
                    .map_err(|_| bad(format!("USD 数字写坏了：{raw}")))?;
                tokens.push(Token::Number(value));
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b':' | b'.'))
                {
                    i += 1;
                }
                // `float3[]` 的 `[]` 属于类型名。
                let mut ident = text[start..i].to_string();
                if bytes[i..].starts_with(b"[]") {
                    ident.push_str("[]");
                    i += 2;
                }
                match ident.as_str() {
                    "inf" => tokens.push(Token::Number(f64::INFINITY)),
                    "nan" => tokens.push(Token::Number(f64::NAN)),
                    _ => tokens.push(Token::Ident(ident)),
                }
            }
            _ => i += 1,
        }
    }
    Ok(tokens)
}

// ── 语法 ──

/// 属性值。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 数字。
    Number(f64),
    /// 字符串或 token。
    Str(String),
    /// 资源路径 `@...@`。
    Asset(String),
    /// 场景路径 `<...>`。
    Path(String),
    /// 元组 `(...)`。
    Tuple(Vec<Value>),
    /// 数组 `[...]`。
    List(Vec<Value>),
    /// 字典或 `timeSamples` 之类没有展开的东西。
    Opaque,
    /// `None`。
    None,
}

impl Value {
    fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Number(v) => Some(*v as f32),
            Value::Tuple(items) | Value::List(items) => items.first().and_then(Value::as_f32),
            _ => None,
        }
    }

    fn floats(&self) -> Vec<f32> {
        let mut out = Vec::new();
        fn walk(value: &Value, out: &mut Vec<f32>) {
            match value {
                Value::Number(v) => out.push(*v as f32),
                Value::Tuple(items) | Value::List(items) => items.iter().for_each(|i| walk(i, out)),
                _ => {}
            }
        }
        walk(self, &mut out);
        out
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Asset(s) | Value::Path(s) => Some(s),
            _ => None,
        }
    }
}

/// 一个属性或关系。
#[derive(Debug, Clone)]
pub struct Attribute {
    /// 类型名（`point3f[]`、`token`、`rel`……）。
    pub type_name: String,
    /// 值（可能没有）。
    pub value: Option<Value>,
    /// `.connect` 目标。
    pub connect: Option<String>,
    /// 元数据里的 `interpolation`。
    pub interpolation: Option<String>,
}

/// 一个 prim。
#[derive(Debug, Clone, Default)]
pub struct Prim {
    /// 名字。
    pub name: String,
    /// 类型（`Xform`、`Mesh`……），`over` 或无类型时为空。
    pub type_name: String,
    /// 属性和关系。
    pub attributes: HashMap<String, Attribute>,
    /// 子 prim。
    pub children: Vec<Prim>,
}

impl Prim {
    fn attr(&self, name: &str) -> Option<&Value> {
        self.attributes.get(name).and_then(|a| a.value.as_ref())
    }

    fn attr_f32(&self, name: &str) -> Option<f32> {
        self.attr(name).and_then(Value::as_f32)
    }

    fn attr_str(&self, name: &str) -> Option<&str> {
        self.attr(name).and_then(Value::as_str)
    }

    fn connection(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).and_then(|a| a.connect.as_deref())
    }
}

/// 一个解析好的 USD 层。
#[derive(Debug, Clone, Default)]
pub struct Stage {
    /// 顶层 prim。
    pub prims: Vec<Prim>,
    /// `upAxis`，默认 `"Y"`。
    pub up_axis: String,
    /// `metersPerUnit`，默认 0.01（USD 的默认单位是厘米）。
    pub meters_per_unit: f32,
    /// `defaultPrim`。
    pub default_prim: Option<String>,
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Token::Punct(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), LoadError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(bad(format!(
                "USD 语法错误：第 {} 个记号处应为 `{c}`，实际是 {:?}",
                self.at,
                self.peek()
            )))
        }
    }

    fn value(&mut self) -> Result<Value, LoadError> {
        match self.next() {
            Some(Token::Number(v)) => Ok(Value::Number(v)),
            Some(Token::Str(s)) => Ok(Value::Str(s)),
            Some(Token::Asset(s)) => Ok(Value::Asset(s)),
            Some(Token::Path(s)) => Ok(Value::Path(s)),
            Some(Token::Ident(s)) => Ok(match s.as_str() {
                "None" => Value::None,
                "true" => Value::Number(1.0),
                "false" => Value::Number(0.0),
                _ => Value::Str(s),
            }),
            Some(Token::Punct(open @ ('(' | '['))) => {
                let close = if open == '(' { ')' } else { ']' };
                let mut items = Vec::new();
                while !self.eat(close) {
                    if self.peek().is_none() {
                        return Err(bad("USD 数组没有闭合"));
                    }
                    items.push(self.value()?);
                    self.eat(',');
                }
                Ok(if open == '(' {
                    Value::Tuple(items)
                } else {
                    Value::List(items)
                })
            }
            Some(Token::Punct('{')) => {
                // 字典 / timeSamples：先整体跳过，timeSamples 由调用方另外处理。
                self.skip_braces()?;
                Ok(Value::Opaque)
            }
            other => Err(bad(format!("USD 语法错误：意外的 {other:?}"))),
        }
    }

    /// 已经吃掉了 `{`，跳到配对的 `}` 之后。
    fn skip_braces(&mut self) -> Result<(), LoadError> {
        let mut depth = 1;
        while depth > 0 {
            match self.next() {
                Some(Token::Punct('{')) => depth += 1,
                Some(Token::Punct('}')) => depth -= 1,
                None => return Err(bad("USD 花括号没有闭合")),
                _ => {}
            }
        }
        Ok(())
    }

    /// `timeSamples` 的第一帧：`{ 0: value, 1: value }`。
    fn first_time_sample(&mut self) -> Result<Option<Value>, LoadError> {
        self.expect('{')?;
        let mut first = None;
        while !self.eat('}') {
            // 时间码后面跟一个冒号，词法里冒号不单独成记号——它被吞在了
            // 数字后面的空白里，所以这里只看「数字 然后 值」。
            match self.next() {
                Some(Token::Number(_)) => {}
                None => return Err(bad("USD timeSamples 没有闭合")),
                _ => continue,
            }
            let value = self.value()?;
            if first.is_none() {
                first = Some(value);
            }
            self.eat(',');
        }
        Ok(first)
    }

    /// `( key = value ... )` 元数据。返回其中的 `interpolation`，其余丢弃。
    fn metadata(&mut self) -> Result<HashMap<String, Value>, LoadError> {
        let mut out = HashMap::new();
        self.expect('(')?;
        while !self.eat(')') {
            match self.next() {
                Some(Token::Str(_)) => {}
                Some(Token::Ident(mut key)) => {
                    // `prepend references = ...`、`dictionary customData = {...}`
                    if let Some(Token::Ident(real)) = self.peek().cloned() {
                        self.at += 1;
                        key = real;
                    }
                    if self.eat('=') {
                        let value = self.value()?;
                        out.insert(key, value);
                    }
                }
                Some(Token::Punct(';' | ',')) => {}
                None => return Err(bad("USD 元数据没有闭合")),
                _ => {}
            }
        }
        Ok(out)
    }

    fn prim(&mut self) -> Result<Prim, LoadError> {
        // 已经吃掉了 def / over / class。
        let mut prim = Prim::default();
        if let Some(Token::Ident(type_name)) = self.peek().cloned() {
            self.at += 1;
            prim.type_name = type_name;
        }
        match self.next() {
            Some(Token::Str(name)) => prim.name = name,
            other => return Err(bad(format!("USD prim 缺名字：{other:?}"))),
        }
        if self.peek() == Some(&Token::Punct('(')) {
            self.metadata()?;
        }
        self.expect('{')?;
        while !self.eat('}') {
            let Some(token) = self.next() else {
                return Err(bad("USD prim 没有闭合"));
            };
            match token {
                Token::Ident(word) => match word.as_str() {
                    "def" | "over" | "class" => prim.children.push(self.prim()?),
                    "variantSet" => {
                        // variantSet "name" = { "a" { ... } ... }
                        self.next();
                        self.expect('=')?;
                        self.expect('{')?;
                        self.skip_braces()?;
                    }
                    "reorder" => {
                        // reorder nameChildren = [...]
                        self.next();
                        self.expect('=')?;
                        self.value()?;
                    }
                    _ => self.property(word, &mut prim)?,
                },
                Token::Punct(';') => {}
                other => return Err(bad(format!("USD prim 里意外的 {other:?}"))),
            }
        }
        Ok(prim)
    }

    fn property(&mut self, first: String, prim: &mut Prim) -> Result<(), LoadError> {
        // [custom] [uniform|varying] [prepend|append|delete|add] type name
        let mut word = first;
        while matches!(
            word.as_str(),
            "custom" | "uniform" | "varying" | "prepend" | "append" | "delete" | "add"
        ) {
            match self.next() {
                Some(Token::Ident(next)) => word = next,
                other => return Err(bad(format!("USD 属性声明写坏了：{other:?}"))),
            }
        }
        let type_name = word;
        let Some(Token::Ident(mut name)) = self.next() else {
            return Err(bad(format!("USD 属性缺名字（类型 {type_name}）")));
        };
        let mut attribute = Attribute {
            type_name: type_name.clone(),
            value: None,
            connect: None,
            interpolation: None,
        };
        let mut connect = false;
        let mut samples = false;
        if let Some(stripped) = name.strip_suffix(".connect") {
            name = stripped.to_string();
            connect = true;
        } else if let Some(stripped) = name.strip_suffix(".timeSamples") {
            name = stripped.to_string();
            samples = true;
        }
        if self.eat('=') {
            if samples {
                attribute.value = self.first_time_sample()?;
            } else {
                let value = self.value()?;
                if connect || type_name == "rel" {
                    // 连接可以是单个路径或者路径数组，取第一个。
                    let target = match &value {
                        Value::Path(p) => Some(p.clone()),
                        Value::List(items) => {
                            items.iter().find_map(|v| v.as_str().map(str::to_string))
                        }
                        _ => None,
                    };
                    if connect {
                        attribute.connect = target;
                    } else {
                        attribute.value = target.map(Value::Path);
                    }
                } else {
                    attribute.value = Some(value);
                }
            }
        }
        if self.peek() == Some(&Token::Punct('(')) {
            let metadata = self.metadata()?;
            attribute.interpolation = metadata
                .get("interpolation")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        // 同名属性（值和 .connect 分两行写）合并。
        let entry = prim.attributes.entry(name).or_insert(Attribute {
            type_name: attribute.type_name.clone(),
            value: None,
            connect: None,
            interpolation: None,
        });
        if attribute.value.is_some() {
            entry.value = attribute.value;
        }
        if attribute.connect.is_some() {
            entry.connect = attribute.connect;
        }
        if attribute.interpolation.is_some() {
            entry.interpolation = attribute.interpolation;
        }
        Ok(())
    }
}

impl Stage {
    /// 解析一份 `.usda` 文本。
    pub fn parse(text: &str) -> Result<Stage, LoadError> {
        let mut parser = Parser {
            tokens: tokenize(text)?,
            at: 0,
        };
        let mut stage = Stage {
            up_axis: "Y".into(),
            meters_per_unit: 0.01,
            ..Stage::default()
        };
        if parser.peek() == Some(&Token::Punct('(')) {
            let metadata = parser.metadata()?;
            if let Some(axis) = metadata.get("upAxis").and_then(Value::as_str) {
                stage.up_axis = axis.to_string();
            }
            if let Some(scale) = metadata.get("metersPerUnit").and_then(Value::as_f32) {
                stage.meters_per_unit = scale;
            }
            stage.default_prim = metadata
                .get("defaultPrim")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        while let Some(token) = parser.next() {
            match token {
                Token::Ident(word) if matches!(word.as_str(), "def" | "over" | "class") => {
                    let prim = parser.prim()?;
                    if word != "class" {
                        stage.prims.push(prim);
                    }
                }
                _ => {}
            }
        }
        Ok(stage)
    }

    /// 按绝对路径找 prim（`/a/b/c`）。
    pub fn prim_at(&self, path: &str) -> Option<&Prim> {
        let path = path.split('.').next().unwrap_or(path);
        let mut parts = path
            .trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty());
        let first = parts.next()?;
        let mut prim = self.prims.iter().find(|p| p.name == first)?;
        for part in parts {
            prim = prim.children.iter().find(|p| p.name == part)?;
        }
        Some(prim)
    }

    /// 所有 `asset inputs:file` 引用的文件（去重）。
    pub fn asset_paths(&self) -> Vec<String> {
        fn walk(prim: &Prim, out: &mut Vec<String>) {
            for attribute in prim.attributes.values() {
                if let Some(Value::Asset(path)) = &attribute.value
                    && !out.contains(path)
                {
                    out.push(path.clone());
                }
            }
            prim.children.iter().for_each(|c| walk(c, out));
        }
        let mut out = Vec::new();
        self.prims.iter().for_each(|p| walk(p, &mut out));
        out
    }

    /// 展开成模型。`images` 按资源路径（`@...@` 里那段）给字节。
    pub fn build(&self, name: &str, images: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Model {
        let mut builder = ModelBuilder {
            stage: self,
            images,
            meshes: Vec::new(),
            materials: Vec::new(),
            material_index: HashMap::new(),
            textures: HashMap::new(),
            nodes: Vec::new(),
        };
        // 根节点：统一成 Y 朝上、米为单位。
        let mut rotation = Quat::IDENTITY;
        if self.up_axis.eq_ignore_ascii_case("z") {
            rotation = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        }
        builder.nodes.push(ModelNode {
            name: name.to_string(),
            transform: NodeTransform {
                position: Vec3::ZERO,
                rotation,
                scale: Vec3::splat(self.meters_per_unit),
            },
            children: Vec::new(),
            parts: Vec::new(),
            skin: None,
        });
        for prim in &self.prims {
            if let Some(child) = builder.prim(prim, &format!("/{}", prim.name)) {
                builder.nodes[0].children.push(child);
            }
        }
        Model::new(builder.meshes, builder.materials, builder.nodes, vec![0])
    }
}

// ── 展开 ──

struct ModelBuilder<'a> {
    stage: &'a Stage,
    images: &'a mut dyn FnMut(&str) -> Option<Vec<u8>>,
    meshes: Vec<Mesh>,
    materials: Vec<Material>,
    material_index: HashMap<String, usize>,
    /// (文件, 是否线性) → 贴图。
    textures: HashMap<(String, bool), Option<Texture>>,
    nodes: Vec<ModelNode>,
}

/// 一个 `xformOp` 的矩阵。
fn xform_op(prim: &Prim, op: &str) -> Mat4 {
    let Some(value) = prim.attr(op) else {
        return Mat4::IDENTITY;
    };
    let v = value.floats();
    let v3 = || {
        Vec3::new(
            v.first().copied().unwrap_or(0.0),
            v.get(1).copied().unwrap_or(0.0),
            v.get(2).copied().unwrap_or(0.0),
        )
    };
    let kind = op.trim_start_matches("xformOp:");
    let kind = kind.split(':').next().unwrap_or(kind);
    let rad = |d: f32| d.to_radians();
    match kind {
        "translate" => Mat4::from_translation(v3()),
        "scale" => {
            if v.len() == 1 {
                Mat4::from_scale(Vec3::splat(v[0]))
            } else {
                Mat4::from_scale(v3())
            }
        }
        "rotateX" => Mat4::from_rotation_x(rad(v.first().copied().unwrap_or(0.0))),
        "rotateY" => Mat4::from_rotation_y(rad(v.first().copied().unwrap_or(0.0))),
        "rotateZ" => Mat4::from_rotation_z(rad(v.first().copied().unwrap_or(0.0))),
        "orient" if v.len() == 4 => {
            // USD 的四元数是 (实部, 虚部)。
            Mat4::from_quat(Quat::from_xyzw(v[1], v[2], v[3], v[0]).normalize())
        }
        "transform" if v.len() == 16 => {
            // USD 是行向量约定：它的行就是列向量约定下的列。
            Mat4::from_cols_array(&std::array::from_fn(|i| v[i]))
        }
        rotate if rotate.starts_with("rotate") && rotate.len() == 9 => {
            // rotateXYZ：先绕 X、再 Y、再 Z（列向量约定下矩阵是 Z·Y·X）。
            let a = v3();
            let axis = |c: char, angle: f32| match c {
                'X' => Mat4::from_rotation_x(rad(angle)),
                'Y' => Mat4::from_rotation_y(rad(angle)),
                _ => Mat4::from_rotation_z(rad(angle)),
            };
            let order: Vec<char> = rotate[6..].chars().collect();
            let angle_of = |c: char| match c {
                'X' => a.x,
                'Y' => a.y,
                _ => a.z,
            };
            axis(order[2], angle_of(order[2]))
                * axis(order[1], angle_of(order[1]))
                * axis(order[0], angle_of(order[0]))
        }
        _ => Mat4::IDENTITY,
    }
}

fn local_transform(prim: &Prim) -> NodeTransform {
    let Some(Value::List(order)) = prim.attr("xformOpOrder") else {
        return NodeTransform::default();
    };
    let mut matrix = Mat4::IDENTITY;
    for op in order.iter().filter_map(Value::as_str) {
        if let Some(inverse) = op.strip_prefix("!invert!") {
            matrix *= xform_op(prim, inverse).inverse();
        } else if op == "!resetXformStack!" {
            matrix = Mat4::IDENTITY;
        } else {
            matrix *= xform_op(prim, op);
        }
    }
    let (scale, rotation, position) = matrix.to_scale_rotation_translation();
    NodeTransform {
        position,
        rotation,
        scale,
    }
}

fn vec3s(value: Option<&Value>) -> Vec<Vec3> {
    let floats = value.map(Value::floats).unwrap_or_default();
    floats
        .chunks_exact(3)
        .map(|c| Vec3::new(c[0], c[1], c[2]))
        .collect()
}

fn vec2s(value: Option<&Value>) -> Vec<Vec2> {
    let floats = value.map(Value::floats).unwrap_or_default();
    floats
        .chunks_exact(2)
        .map(|c| Vec2::new(c[0], c[1]))
        .collect()
}

fn ints(value: Option<&Value>) -> Vec<usize> {
    value
        .map(Value::floats)
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.max(0.0) as usize)
        .collect()
}

/// 一个 primvar：值、插值方式、可选的索引。按「第几个面、第几个角、
/// 顶点号」取值。
struct Primvar<T> {
    values: Vec<T>,
    indices: Option<Vec<usize>>,
    interpolation: String,
}

impl<T: Copy> Primvar<T> {
    fn read(prim: &Prim, name: &str, parse: fn(Option<&Value>) -> Vec<T>) -> Option<Self> {
        let attribute = prim.attributes.get(name)?;
        let values = parse(attribute.value.as_ref());
        if values.is_empty() {
            return None;
        }
        let indices = prim
            .attributes
            .get(&format!("{name}:indices"))
            .map(|a| ints(a.value.as_ref()));
        Some(Self {
            values,
            indices,
            interpolation: attribute
                .interpolation
                .clone()
                .unwrap_or_else(|| "vertex".into()),
        })
    }

    fn get(&self, face: usize, corner: usize, point: usize) -> Option<T> {
        let slot = match self.interpolation.as_str() {
            "faceVarying" => corner,
            "uniform" => face,
            "constant" => 0,
            _ => point,
        };
        let slot = match &self.indices {
            Some(indices) => *indices.get(slot)?,
            None => slot,
        };
        self.values.get(slot).copied()
    }
}

impl ModelBuilder<'_> {
    fn prim(&mut self, prim: &Prim, path: &str) -> Option<usize> {
        if matches!(
            prim.type_name.as_str(),
            "Material" | "Shader" | "GeomSubset" | "NodeGraph"
        ) {
            return None;
        }
        let index = self.nodes.len();
        self.nodes.push(ModelNode {
            name: prim.name.clone(),
            transform: local_transform(prim),
            children: Vec::new(),
            parts: Vec::new(),
            skin: None,
        });
        if prim.type_name == "Mesh" {
            let parts = self.mesh(prim, path);
            self.nodes[index].parts = parts;
        }
        for child in &prim.children {
            if let Some(child_index) = self.prim(child, &format!("{path}/{}", child.name)) {
                self.nodes[index].children.push(child_index);
            }
        }
        Some(index)
    }

    fn mesh(&mut self, prim: &Prim, path: &str) -> Vec<MeshPart> {
        let points = vec3s(prim.attr("points"));
        let counts = ints(prim.attr("faceVertexCounts"));
        let indices = ints(prim.attr("faceVertexIndices"));
        if points.is_empty() || counts.is_empty() {
            return Vec::new();
        }
        let normals = Primvar::read(prim, "normals", vec3s)
            .or_else(|| Primvar::read(prim, "primvars:normals", vec3s));
        let uvs = Primvar::read(prim, "primvars:st", vec2s)
            .or_else(|| Primvar::read(prim, "primvars:UVMap", vec2s))
            .or_else(|| Primvar::read(prim, "primvars:st0", vec2s));
        let colors = Primvar::read(prim, "primvars:displayColor", vec3s);
        let left_handed = prim.attr_str("orientation") == Some("leftHanded");

        // 面 → 所属子集（GeomSubset 按面分材质）。
        let subsets: Vec<&Prim> = prim
            .children
            .iter()
            .filter(|c| {
                c.type_name == "GeomSubset" && c.attr_str("elementType").unwrap_or("face") == "face"
            })
            .collect();
        let mut face_group = vec![0usize; counts.len()];
        for (group, subset) in subsets.iter().enumerate() {
            for face in ints(subset.attr("indices")) {
                if let Some(slot) = face_group.get_mut(face) {
                    *slot = group + 1;
                }
            }
        }

        // 每组一个网格：(顶点, 索引, 去重表)。
        let groups = subsets.len() + 1;
        // 每组：(顶点, 索引, 「点号 + 法线 + UV」→ 顶点号 的去重表)。
        type Group = (Vec<Vertex>, Vec<u32>, HashMap<(usize, [u32; 5]), u32>);
        let mut built: Vec<Group> = (0..groups)
            .map(|_| (Vec::new(), Vec::new(), HashMap::new()))
            .collect();
        let mut corner = 0;
        for (face, &count) in counts.iter().enumerate() {
            let (vertices, out_indices, lookup) = &mut built[face_group[face]];
            let mut face_vertices = Vec::with_capacity(count);
            for k in 0..count {
                let c = corner + k;
                let Some(&point) = indices.get(c) else { break };
                let Some(&position) = points.get(point) else {
                    continue;
                };
                let normal = normals
                    .as_ref()
                    .and_then(|n| n.get(face, c, point))
                    .unwrap_or(Vec3::ZERO);
                // USD 的 st 原点在左下，引擎的 uv 原点在左上。
                let uv = uvs
                    .as_ref()
                    .and_then(|u| u.get(face, c, point))
                    .map_or(Vec2::ZERO, |uv| Vec2::new(uv.x, 1.0 - uv.y));
                let color = colors
                    .as_ref()
                    .and_then(|col| col.get(face, c, point))
                    .unwrap_or(Vec3::ONE);
                let key = (
                    point,
                    [
                        normal.x.to_bits(),
                        normal.y.to_bits(),
                        normal.z.to_bits(),
                        uv.x.to_bits(),
                        uv.y.to_bits(),
                    ],
                );
                let index = *lookup.entry(key).or_insert_with(|| {
                    vertices.push(Vertex {
                        position: position.to_array(),
                        normal: normal.to_array(),
                        uv: uv.to_array(),
                        color: color.to_array(),
                        ..Default::default()
                    });
                    (vertices.len() - 1) as u32
                });
                face_vertices.push(index);
            }
            corner += count;
            for k in 1..face_vertices.len().saturating_sub(1) {
                let tri = [face_vertices[0], face_vertices[k], face_vertices[k + 1]];
                if left_handed {
                    out_indices.extend_from_slice(&[tri[0], tri[2], tri[1]]);
                } else {
                    out_indices.extend_from_slice(&tri);
                }
            }
        }

        let default_binding = prim.attr_str("material:binding").map(str::to_string);
        let double_sided = prim.attr_f32("doubleSided").is_some_and(|v| v > 0.5);
        let mut parts = Vec::new();
        for (group, (vertices, indices, _)) in built.into_iter().enumerate() {
            if indices.is_empty() {
                continue;
            }
            let mut mesh = Mesh::new(vertices, indices);
            if normals.is_none() {
                mesh.recompute_normals();
            }
            if uvs.is_some() {
                mesh.recompute_tangents();
            }
            self.meshes.push(mesh);
            let binding = if group == 0 {
                default_binding.clone()
            } else {
                subsets[group - 1]
                    .attr_str("material:binding")
                    .map(str::to_string)
                    .or_else(|| default_binding.clone())
            };
            let material = binding
                .map(|b| self.material(&b, double_sided))
                .or_else(|| {
                    // 没绑材质：用 displayColor 常量（或白）。
                    let base = colors
                        .as_ref()
                        .filter(|c| c.values.len() == 1)
                        .map_or(Vec3::splat(0.8), |c| c.values[0]);
                    let mut material = Material::standard()
                        .with_name(format!("{path} displayColor"))
                        .with_base_color(base.extend(1.0));
                    if double_sided {
                        material.set_double_sided(true);
                    }
                    self.materials.push(material);
                    Some(self.materials.len() - 1)
                });
            parts.push(MeshPart {
                mesh: self.meshes.len() - 1,
                material,
            });
        }
        parts
    }

    fn texture(&mut self, file: &str, linear: bool) -> Option<Texture> {
        let key = (file.to_string(), linear);
        if let Some(cached) = self.textures.get(&key) {
            return cached.clone();
        }
        let loaded = (self.images)(file).and_then(|bytes| match Texture::from_encoded(&bytes) {
            Ok(texture) => Some(texture.with_format(if linear {
                TextureFormat::Linear
            } else {
                TextureFormat::Srgb
            })),
            Err(error) => {
                klog::warn!("USD 贴图 {file} 解码失败：{error}");
                None
            }
        });
        if loaded.is_none() {
            klog::warn!("USD 贴图 {file} 读不到");
        }
        self.textures.insert(key, loaded.clone());
        loaded
    }

    /// 一个 `UsdPreviewSurface` 输入：常量，或者连着的 `UsdUVTexture`
    /// （返回贴图文件、取哪个输出、scale、bias）。
    fn input<'s>(&'s self, shader: &'s Prim, name: &str) -> Input<'s> {
        let key = format!("inputs:{name}");
        if let Some(target) = shader.connection(&key)
            && let Some(texture) = self.stage.prim_at(target)
            && texture.attr_str("info:id") == Some("UsdUVTexture")
            && let Some(file) = texture.attr_str("inputs:file")
        {
            let output = target.rsplit_once(".outputs:").map_or("rgb", |(_, o)| o);
            let scale = texture
                .attr("inputs:scale")
                .map(Value::floats)
                .unwrap_or_default();
            let bias = texture
                .attr("inputs:bias")
                .map(Value::floats)
                .unwrap_or_default();
            return Input::Texture {
                file,
                output,
                scale: Vec4::from_slice(&pad4(&scale, 1.0)),
                bias: Vec4::from_slice(&pad4(&bias, 0.0)),
            };
        }
        match shader.attr(&key) {
            Some(value) => Input::Constant(value.floats()),
            None => Input::Missing,
        }
    }

    fn material(&mut self, binding: &str, double_sided: bool) -> usize {
        let key = format!("{binding}#{double_sided}");
        if let Some(&index) = self.material_index.get(&key) {
            return index;
        }
        let material = self.build_material(binding, double_sided);
        self.materials.push(material);
        let index = self.materials.len() - 1;
        self.material_index.insert(key, index);
        index
    }

    fn build_material(&mut self, binding: &str, double_sided: bool) -> Material {
        let mut material =
            Material::standard().with_name(binding.rsplit('/').next().unwrap_or(binding));
        if double_sided {
            material.set_double_sided(true);
        }
        let stage = self.stage;
        let Some(prim) = stage.prim_at(binding) else {
            klog::warn!("USD 材质 {binding} 不存在");
            return material;
        };
        let surface = prim
            .connection("outputs:surface")
            .or_else(|| prim.connection("outputs:mtlx:surface"))
            .and_then(|target| stage.prim_at(target));
        let Some(shader) = surface.filter(|s| s.attr_str("info:id") == Some("UsdPreviewSurface"))
        else {
            klog::warn!("USD 材质 {binding} 不是 UsdPreviewSurface，按默认材质");
            return material;
        };

        // 基础色。
        let mut base = Vec4::new(0.18, 0.18, 0.18, 1.0);
        match self.input(shader, "diffuseColor") {
            Input::Constant(v) => base = pad3(&v, 0.18).extend(1.0),
            Input::Texture {
                file, scale, bias, ..
            } => {
                let file = file.to_string();
                if let Some(texture) = self.texture(&file, false) {
                    material.set(
                        kmaterial::standard::BASE_COLOR_TEXTURE,
                        Resource::new_ok(format!("usd:{file}"), texture),
                    );
                    // 常见的 scale = 1、bias = 0；其他值按系数近似。
                    base = (scale + bias).truncate().extend(1.0);
                }
            }
            Input::Missing => {}
        }
        match self.input(shader, "opacity") {
            Input::Constant(v) => {
                base.w = v.first().copied().unwrap_or(1.0);
            }
            Input::Texture { .. } => {
                // 常见写法是连到基础色贴图的 a 通道：透明度由贴图 alpha 给。
                material.set_blend_mode(BlendMode::Alpha);
            }
            Input::Missing => {}
        }
        if base.w < 1.0 {
            material.set_blend_mode(BlendMode::Alpha);
        }
        material.set_base_color(base);

        // 金属度 / 粗糙度：常量直接用；连着贴图时拼成 glTF 的 G/B 通道。
        let metallic = self.input(shader, "metallic");
        let roughness = self.input(shader, "roughness");
        let mut metallic_factor = match &metallic {
            Input::Constant(v) => v.first().copied().unwrap_or(0.0),
            _ => 0.0,
        };
        let mut roughness_factor = match &roughness {
            Input::Constant(v) => v.first().copied().unwrap_or(0.5),
            _ => 0.5,
        };
        let metallic_texture = match &metallic {
            Input::Texture { file, output, .. } => Some((file.to_string(), channel(output))),
            _ => None,
        };
        let roughness_texture = match &roughness {
            Input::Texture { file, output, .. } => Some((file.to_string(), channel(output))),
            _ => None,
        };
        if metallic_texture.is_some() || roughness_texture.is_some() {
            let m = metallic_texture
                .as_ref()
                .and_then(|(f, c)| self.texture(f, true).map(|t| (t, *c)));
            let r = roughness_texture
                .as_ref()
                .and_then(|(f, c)| self.texture(f, true).map(|t| (t, *c)));
            if let Some(packed) = pack_metallic_roughness(m.as_ref(), r.as_ref()) {
                if m.is_some() {
                    metallic_factor = 1.0;
                }
                if r.is_some() {
                    roughness_factor = 1.0;
                }
                material.set(
                    kpbr::standard::METALLIC_ROUGHNESS_TEXTURE,
                    Resource::new_ok(format!("usd-mr:{binding}"), packed),
                );
            }
        }
        material.set_metallic(metallic_factor);
        material.set_roughness(roughness_factor);

        match self.input(shader, "emissiveColor") {
            Input::Constant(v) => {
                let e = pad3(&v, 0.0);
                if e != Vec3::ZERO {
                    material.set(kpbr::standard::EMISSIVE, e);
                }
            }
            Input::Texture { file, .. } => {
                let file = file.to_string();
                if let Some(texture) = self.texture(&file, false) {
                    material.set(kpbr::standard::EMISSIVE, Vec3::ONE);
                    material.set(
                        kpbr::standard::EMISSIVE_TEXTURE,
                        Resource::new_ok(format!("usd:{file}"), texture),
                    );
                }
            }
            Input::Missing => {}
        }
        if let Input::Texture { file, .. } = self.input(shader, "normal") {
            let file = file.to_string();
            if let Some(texture) = self.texture(&file, true) {
                material.set(
                    kpbr::standard::NORMAL_TEXTURE,
                    Resource::new_ok(format!("usd-n:{file}"), texture),
                );
            }
        }
        if let Input::Texture { file, .. } = self.input(shader, "occlusion") {
            let file = file.to_string();
            if let Some(texture) = self.texture(&file, true) {
                material.set(
                    kpbr::standard::OCCLUSION_TEXTURE,
                    Resource::new_ok(format!("usd-ao:{file}"), texture),
                );
            }
        }
        material
    }
}

enum Input<'a> {
    Constant(Vec<f32>),
    Texture {
        file: &'a str,
        output: &'a str,
        scale: Vec4,
        bias: Vec4,
    },
    Missing,
}

fn pad3(v: &[f32], fill: f32) -> Vec3 {
    let p = pad4(v, fill);
    Vec3::new(p[0], p[1], p[2])
}

fn pad4(v: &[f32], fill: f32) -> [f32; 4] {
    std::array::from_fn(|i| {
        v.get(i)
            .copied()
            .or_else(|| {
                if v.len() == 1 {
                    v.first().copied()
                } else {
                    None
                }
            })
            .unwrap_or(fill)
    })
}

fn channel(output: &str) -> usize {
    match output {
        "g" => 1,
        "b" => 2,
        "a" => 3,
        _ => 0,
    }
}

/// 把金属度、粗糙度两张（可能是同一张）单通道图拼成 glTF 约定的一张：
/// G 粗糙度、B 金属度。缺的那路填满。
fn pack_metallic_roughness(
    metallic: Option<&(Texture, usize)>,
    roughness: Option<&(Texture, usize)>,
) -> Option<Texture> {
    let (width, height) = metallic
        .or(roughness)
        .map(|(t, _)| (t.width(), t.height()))?;
    let sample = |source: Option<&(Texture, usize)>, x: u32, y: u32| -> u8 {
        let Some((texture, channel)) = source else {
            return 255;
        };
        // 两张尺寸不同时按最近邻缩放。
        let sx = (x as u64 * texture.width() as u64 / width.max(1) as u64) as usize;
        let sy = (y as u64 * texture.height() as u64 / height.max(1) as u64) as usize;
        texture.data()[(sy * texture.width() as usize + sx) * 4 + channel]
    };
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[255, sample(roughness, x, y), sample(metallic, x, y), 255]);
        }
    }
    Some(Texture::new(width, height, data).with_format(TextureFormat::Linear))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
    metersPerUnit = 1
    upAxis = "Z"
)

def Xform "Root" (
    kind = "component"
)
{
    double3 xformOp:translate = (1, 2, 3)
    float xformOp:rotateZ = 90
    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateZ"]

    def Mesh "Quad"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1)] (
            interpolation = "faceVarying"
        )
        int[] primvars:st:indices = [0, 1, 2, 1]
        rel material:binding = </Root/Materials/Red>
    }

    def Scope "Materials"
    {
        def Material "Red"
        {
            token outputs:surface.connect = </Root/Materials/Red/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (1, 0, 0)
                float inputs:roughness = 0.25
                float inputs:metallic.timeSamples = {
                    0: 0.75,
                    10: 1,
                }
                token outputs:surface
            }
        }
    }
}
"#;

    fn build(text: &str) -> Model {
        Stage::parse(text).unwrap().build("test", &mut |_| None)
    }

    #[test]
    fn stage_metadata_and_hierarchy() {
        let stage = Stage::parse(CUBE).unwrap();
        assert_eq!(stage.up_axis, "Z");
        assert_eq!(stage.meters_per_unit, 1.0);
        assert_eq!(stage.default_prim.as_deref(), Some("Root"));
        let quad = stage.prim_at("/Root/Quad").unwrap();
        assert_eq!(quad.type_name, "Mesh");
        assert_eq!(
            quad.attributes["primvars:st"].interpolation.as_deref(),
            Some("faceVarying")
        );
    }

    #[test]
    fn xform_ops_compose_in_order() {
        let stage = Stage::parse(CUBE).unwrap();
        let t = local_transform(stage.prim_at("/Root").unwrap());
        assert!((t.position - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5);
        let x = t.rotation * Vec3::X;
        assert!(
            (x - Vec3::Y).length() < 1e-5,
            "绕 Z 转 90° 把 X 转到 Y，实际 {x}"
        );
    }

    #[test]
    fn a_quad_becomes_two_triangles_with_indexed_face_varying_uvs() {
        let model = build(CUBE);
        let mesh = &model.meshes()[0];
        assert_eq!(mesh.indices().len(), 6);
        // 第四个角的 st 索引是 1 → (1, 0)，翻 v 之后是 (1, 1)。
        let uv = mesh.vertices()[3].uv;
        assert_eq!(uv, [1.0, 1.0]);
    }

    #[test]
    fn preview_surface_constants_and_time_samples() {
        let model = build(CUBE);
        let material = &model.materials()[0];
        assert_eq!(material.base_color(), Vec4::new(1.0, 0.0, 0.0, 1.0));
        assert!((material.roughness() - 0.25).abs() < 1e-6);
        assert!(
            (material.metallic() - 0.75).abs() < 1e-6,
            "timeSamples 取第一帧"
        );
    }

    #[test]
    fn z_up_layers_are_rotated_to_y_up() {
        let model = build(CUBE);
        let up = model.nodes()[0].transform.rotation * Vec3::Z;
        assert!((up - Vec3::Y).length() < 1e-5);
    }

    #[test]
    fn subsets_split_the_mesh_by_material() {
        let text = r#"#usda 1.0
def Mesh "M"
{
    int[] faceVertexCounts = [3, 3]
    int[] faceVertexIndices = [0, 1, 2, 0, 2, 3]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    def GeomSubset "second"
    {
        uniform token elementType = "face"
        int[] indices = [1]
    }
}
"#;
        let model = build(text);
        assert_eq!(model.meshes().len(), 2);
        assert_eq!(model.nodes()[1].parts.len(), 2);
    }

    #[test]
    fn binary_layers_are_rejected_with_a_clear_message() {
        let error = layer_text(b"PXR-USDC\0\0\0\0").unwrap_err().to_string();
        assert!(error.contains("usdc"), "{error}");
    }
}
