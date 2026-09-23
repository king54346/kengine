//! FBX（Autodesk Filmbox）：二进制（6.x / 7.x，含 7.5 的 64 位偏移）与 ASCII 都读。
//!
//! # 支持
//!
//! | 部分 | |
//! |---|---|
//! | 场景 | `Model` 节点树；完整的 FBX 变换链（平移 · 旋转偏移 · 旋转枢轴 · 预旋转 · 旋转 · 后旋转⁻¹ · 枢轴⁻¹ · 缩放偏移 · 缩放枢轴 · 缩放 · 枢轴⁻¹），六种欧拉顺序 |
//! | 几何 | 多边形网格（扇形三角化）、法线 / UV / 顶点色（`ByPolygonVertex` / `ByVertice` / `ByPolygon` / `AllSame`，`Direct` / `IndexToDirect`）、逐面材质、几何变换（`GeometricTranslation` 等） |
//! | 材质 | Lambert / Phong：漫反射、自发光、不透明度、光泽度；漫反射 / 法线 / 自发光贴图（外部文件或内嵌的 `Video.Content`） |
//! | 蒙皮 | `Skin` → `Cluster`：权重、`Transform` / `TransformLink` 算逆绑定矩阵 |
//! | 形变 | `BlendShape` → `BlendShapeChannel` → `Shape`：位置与法线增量 |
//! | 动画 | 每个 `AnimationStack` 一段剪辑；`Lcl Translation / Rotation / Scaling` 按变换链重采样成 TRS；`DeformPercent` → 形变权重 |
//! | 曲线 | `NurbsCurve`（开 / 闭 / 周期，有理 / 非有理），采样成折线，见 [`Fbx::curves`] |
//! | 坐标系 | `UpAxis = 2`（Z 朝上）时根节点绕 X 转 -90° |
//!
//! 不支持：NURBS 曲面、灯光与相机、约束、`InheritType` 不是 `RSrs` 的继承方式、
//! 曲线切线（关键帧之间一律线性插值——FBX 的三次曲线切线要按 `KeyAttrFlags`
//! 解码，而绝大多数动捕 / 烘焙过的动画是逐帧关键帧，线性足够）。
//!
//! # 单位
//!
//! 不做单位换算：FBX 默认厘米，模型读进来就是厘米尺度，和 three.js 一样。
//! `UnitScaleFactor` 可以从 [`Fbx::unit_scale`] 取到，由调用方决定要不要缩。

use crate::{bad, limits, loader};
use kanim::{AnimationClip, Channel, Curve, Interpolation, Track};
use kasset::{LoadError, Resource, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, ModelSkin, NodeTransform};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3, Vec4};
use kmesh::{Mesh, MorphDelta, MorphTarget, SkinVertex, Vertex};
use ktexture::Texture;
use std::{
    collections::{BTreeSet, HashMap},
    io::Read,
    path::PathBuf,
    sync::Arc,
};

/// [`Fbx`] 的资源类型标识。
pub const FBX_TYPE_UUID: Uuid = uuid!("f0b8e1c3-5d27-4a96-8e14-2c7b3a9d6e51");

/// 一个 FBX 文件的完整导入结果。
#[derive(Debug, Clone)]
pub struct Fbx {
    /// 场景。
    pub model: Model,
    /// NURBS 曲线：`(节点号, 节点局部空间里的折线)`。
    pub curves: Vec<(usize, Vec<Vec3>)>,
    /// `GlobalSettings.UnitScaleFactor`（1 = 厘米）。
    pub unit_scale: f32,
}

impl ResourceData for Fbx {
    fn type_uuid(&self) -> Uuid {
        FBX_TYPE_UUID
    }
}

loader! {
    /// 读 `.fbx`，产出 [`Model`]。
    FbxLoader -> Model : ["fbx"] = MODEL_TYPE_UUID, parse
}

loader! {
    /// 读 `.fbx`，产出带曲线的 [`Fbx`]。和 [`FbxLoader`] 二选一注册。
    FbxSceneLoader -> Fbx : ["fbx"] = FBX_TYPE_UUID, parse_fbx
}

// ───────────────────────────── 文档树 ─────────────────────────────

/// 一个属性值。
#[derive(Debug, Clone)]
pub enum Prop {
    /// 整数（含布尔）。
    Int(i64),
    /// 浮点。
    Float(f64),
    /// 字符串。
    Str(String),
    /// 原始字节。
    Bytes(Vec<u8>),
    /// 整数数组。
    Ints(Vec<i64>),
    /// 浮点数组。
    Floats(Vec<f64>),
}

impl Prop {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Prop::Int(v) => Some(*v as f64),
            Prop::Float(v) => Some(*v),
            Prop::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    fn as_i64(&self) -> Option<i64> {
        match self {
            Prop::Int(v) => Some(*v),
            Prop::Float(v) => Some(*v as i64),
            Prop::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }
    fn floats(&self) -> Vec<f64> {
        match self {
            Prop::Floats(v) => v.clone(),
            Prop::Ints(v) => v.iter().map(|&x| x as f64).collect(),
            other => other.as_f64().into_iter().collect(),
        }
    }
    fn ints(&self) -> Vec<i64> {
        match self {
            Prop::Ints(v) => v.clone(),
            Prop::Floats(v) => v.iter().map(|&x| x as i64).collect(),
            other => other.as_i64().into_iter().collect(),
        }
    }
}

/// 一个节点记录。
#[derive(Debug, Clone, Default)]
pub struct FbxNode {
    /// 记录名，例如 `Model`、`Vertices`。
    pub name: String,
    /// 属性。
    pub props: Vec<Prop>,
    /// 子记录。
    pub children: Vec<FbxNode>,
}

impl FbxNode {
    fn child(&self, name: &str) -> Option<&FbxNode> {
        self.children.iter().find(|c| c.name == name)
    }
    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a FbxNode> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    /// 子记录的第一个属性当数组读（ASCII 的 `*N { a: ... }` 与二进制数组都在这里）。
    fn array_f64(&self, name: &str) -> Vec<f64> {
        self.child(name).map(|c| c.props.iter().flat_map(Prop::floats).collect()).unwrap_or_default()
    }
    fn array_i64(&self, name: &str) -> Vec<i64> {
        self.child(name).map(|c| c.props.iter().flat_map(Prop::ints).collect()).unwrap_or_default()
    }
    fn child_str(&self, name: &str) -> Option<&str> {
        self.child(name)?.props.first()?.as_str()
    }
    fn child_i64(&self, name: &str) -> Option<i64> {
        self.child(name)?.props.first()?.as_i64()
    }
    fn id(&self) -> i64 {
        self.props.first().and_then(Prop::as_i64).unwrap_or(0)
    }
    /// 对象名：二进制写成 `名字\0\x01类名`，ASCII 写成 `类名::名字`。
    fn object_name(&self) -> String {
        let raw = self.props.get(1).and_then(Prop::as_str).unwrap_or("");
        if let Some((name, _)) = raw.split_once("\u{0}\u{1}") {
            return name.to_string();
        }
        raw.split_once("::").map_or(raw, |(_, n)| n).to_string()
    }
    fn subclass(&self) -> &str {
        self.props.get(2).and_then(Prop::as_str).unwrap_or("")
    }
}

/// `Properties70` / `Properties60` 里的 `P` 表：名字 → 值（去掉类型与标志）。
fn properties(node: &FbxNode) -> HashMap<String, Vec<Prop>> {
    let mut out = HashMap::new();
    for block in ["Properties70", "Properties60"] {
        let Some(table) = node.child(block) else { continue };
        let skip = if block == "Properties70" { 4 } else { 3 };
        for p in table.children.iter().filter(|c| c.name == "P" || c.name == "Property") {
            if let Some(name) = p.props.first().and_then(Prop::as_str) {
                out.insert(name.to_string(), p.props.iter().skip(skip).cloned().collect());
            }
        }
    }
    out
}

fn prop_vec3(props: &HashMap<String, Vec<Prop>>, name: &str, default: Vec3) -> Vec3 {
    match props.get(name) {
        Some(v) if v.len() >= 3 => Vec3::new(
            v[0].as_f64().unwrap_or(default.x as f64) as f32,
            v[1].as_f64().unwrap_or(default.y as f64) as f32,
            v[2].as_f64().unwrap_or(default.z as f64) as f32,
        ),
        _ => default,
    }
}

fn prop_f32(props: &HashMap<String, Vec<Prop>>, name: &str) -> Option<f32> {
    props.get(name)?.first()?.as_f64().map(|v| v as f32)
}

// ───────────────────────────── 二进制 ─────────────────────────────

const BINARY_MAGIC: &[u8] = b"Kaydara FBX Binary  \x00";

struct Binary<'a> {
    data: &'a [u8],
    wide: bool,
}

impl Binary<'_> {
    fn u32(&self, at: usize) -> Result<u32, LoadError> {
        self.data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().expect("4 字节"))).ok_or_else(|| bad("FBX 被截断"))
    }
    fn u64(&self, at: usize) -> Result<u64, LoadError> {
        self.data.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().expect("8 字节"))).ok_or_else(|| bad("FBX 被截断"))
    }
    fn offset(&self, at: usize) -> Result<(u64, usize), LoadError> {
        if self.wide { Ok((self.u64(at)?, 8)) } else { Ok((self.u32(at)? as u64, 4)) }
    }

    /// 读一个节点记录，返回 `(节点, 下一条记录的位置)`；空记录返回 `None`。
    fn node(&self, at: usize, depth: usize) -> Result<(Option<FbxNode>, usize), LoadError> {
        if depth > 64 {
            return Err(bad("FBX 节点嵌套过深"));
        }
        let (end, w) = self.offset(at)?;
        let (count, _) = self.offset(at + w)?;
        let (_list_len, _) = self.offset(at + 2 * w)?;
        let name_len = *self.data.get(at + 3 * w).ok_or_else(|| bad("FBX 被截断"))? as usize;
        if end == 0 {
            return Ok((None, at + 3 * w + 1 + name_len));
        }
        let end = end as usize;
        if end > self.data.len() || end <= at {
            return Err(bad("FBX 节点记录越界"));
        }
        let name_at = at + 3 * w + 1;
        let name = String::from_utf8_lossy(self.data.get(name_at..name_at + name_len).ok_or_else(|| bad("FBX 被截断"))?).into_owned();
        let mut cursor = name_at + name_len;
        let mut props = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            let (prop, next) = self.prop(cursor)?;
            props.push(prop);
            cursor = next;
        }
        let mut children = Vec::new();
        while cursor < end {
            let (child, next) = self.node(cursor, depth + 1)?;
            match child {
                Some(child) => children.push(child),
                None => break,
            }
            cursor = next;
        }
        Ok((Some(FbxNode { name, props, children }), end))
    }

    fn prop(&self, at: usize) -> Result<(Prop, usize), LoadError> {
        let kind = *self.data.get(at).ok_or_else(|| bad("FBX 被截断"))?;
        let body = at + 1;
        let slice = |n: usize| self.data.get(body..body + n).ok_or_else(|| bad("FBX 属性被截断"));
        Ok(match kind {
            b'C' | b'B' => (Prop::Int(slice(1)?[0] as i64), body + 1),
            b'Y' => (Prop::Int(i16::from_le_bytes(slice(2)?.try_into().expect("2")) as i64), body + 2),
            b'I' => (Prop::Int(i32::from_le_bytes(slice(4)?.try_into().expect("4")) as i64), body + 4),
            b'L' => (Prop::Int(i64::from_le_bytes(slice(8)?.try_into().expect("8"))), body + 8),
            b'F' => (Prop::Float(f32::from_le_bytes(slice(4)?.try_into().expect("4")) as f64), body + 4),
            b'D' => (Prop::Float(f64::from_le_bytes(slice(8)?.try_into().expect("8"))), body + 8),
            b'S' | b'R' => {
                let n = self.u32(body)? as usize;
                let bytes = self.data.get(body + 4..body + 4 + n).ok_or_else(|| bad("FBX 字符串被截断"))?;
                let prop = if kind == b'S' { Prop::Str(String::from_utf8_lossy(bytes).into_owned()) } else { Prop::Bytes(bytes.to_vec()) };
                (prop, body + 4 + n)
            }
            b'f' | b'd' | b'l' | b'i' | b'b' => {
                let length = self.u32(body)? as usize;
                let encoding = self.u32(body + 4)?;
                let compressed = self.u32(body + 8)? as usize;
                let raw = self.data.get(body + 12..body + 12 + compressed).ok_or_else(|| bad("FBX 数组被截断"))?;
                let width = match kind {
                    b'd' | b'l' => 8,
                    b'b' => 1,
                    _ => 4,
                };
                if length.saturating_mul(width) > limits::VERTICES * 16 {
                    return Err(bad("FBX 数组超过上限"));
                }
                let data: Vec<u8> = if encoding == 1 {
                    let mut out = Vec::with_capacity(length * width);
                    flate2::read::ZlibDecoder::new(raw)
                        .take((length * width) as u64)
                        .read_to_end(&mut out)
                        .map_err(|e| bad(format!("FBX 数组解压失败：{e}")))?;
                    out
                } else {
                    raw.to_vec()
                };
                if data.len() < length * width {
                    return Err(bad("FBX 数组长度不足"));
                }
                let prop = match kind {
                    b'f' => Prop::Floats(data.chunks_exact(4).take(length).map(|c| f32::from_le_bytes(c.try_into().expect("4")) as f64).collect()),
                    b'd' => Prop::Floats(data.chunks_exact(8).take(length).map(|c| f64::from_le_bytes(c.try_into().expect("8"))).collect()),
                    b'l' => Prop::Ints(data.chunks_exact(8).take(length).map(|c| i64::from_le_bytes(c.try_into().expect("8"))).collect()),
                    b'i' => Prop::Ints(data.chunks_exact(4).take(length).map(|c| i32::from_le_bytes(c.try_into().expect("4")) as i64).collect()),
                    _ => Prop::Ints(data.iter().take(length).map(|&b| b as i64).collect()),
                };
                (prop, body + 12 + compressed)
            }
            other => return Err(bad(format!("FBX 未知的属性类型 {:?}", other as char))),
        })
    }
}

fn parse_binary(data: &[u8]) -> Result<Vec<FbxNode>, LoadError> {
    let version = u32::from_le_bytes(data.get(23..27).ok_or_else(|| bad("FBX 头部被截断"))?.try_into().expect("4"));
    let reader = Binary { data, wide: version >= 7500 };
    let mut at = 27;
    let mut nodes = Vec::new();
    while at < data.len() {
        let (node, next) = reader.node(at, 0)?;
        match node {
            Some(node) => nodes.push(node),
            None => break,
        }
        at = next;
    }
    Ok(nodes)
}

// ───────────────────────────── ASCII ─────────────────────────────

#[derive(Debug, Clone)]
enum Token {
    Name(String),
    Value(Prop),
    Comma,
    Open,
    Close,
    Star,
    Eof,
}

struct Lexer<'a> {
    text: &'a [u8],
    at: usize,
    peeked: Option<Token>,
}

impl Lexer<'_> {
    fn next(&mut self) -> Token {
        if let Some(t) = self.peeked.take() {
            return t;
        }
        loop {
            while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.at < self.text.len() && self.text[self.at] == b';' {
                while self.at < self.text.len() && self.text[self.at] != b'\n' {
                    self.at += 1;
                }
                continue;
            }
            break;
        }
        let Some(&c) = self.text.get(self.at) else { return Token::Eof };
        self.at += 1;
        match c {
            b',' => Token::Comma,
            b'{' => Token::Open,
            b'}' => Token::Close,
            b'*' => Token::Star,
            b'"' => {
                let start = self.at;
                while self.at < self.text.len() && self.text[self.at] != b'"' {
                    self.at += 1;
                }
                let s = String::from_utf8_lossy(&self.text[start..self.at]).into_owned();
                self.at += 1;
                // ASCII 里对象名写成 `类名::名字`，二进制的分隔符在这里换算回来不必要：
                // object_name() 两种都认。
                Token::Value(Prop::Str(s.replace("&quot;", "\"")))
            }
            _ => {
                let start = self.at - 1;
                while self.at < self.text.len() && !matches!(self.text[self.at], b',' | b'{' | b'}' | b':' | b'"') && !self.text[self.at].is_ascii_whitespace() {
                    self.at += 1;
                }
                let word = String::from_utf8_lossy(&self.text[start..self.at]).into_owned();
                if self.text.get(self.at) == Some(&b':') {
                    self.at += 1;
                    return Token::Name(word);
                }
                if let Ok(i) = word.parse::<i64>() {
                    Token::Value(Prop::Int(i))
                } else if let Ok(f) = word.parse::<f64>() {
                    Token::Value(Prop::Float(f))
                } else {
                    Token::Value(Prop::Str(word))
                }
            }
        }
    }
    fn peek(&mut self) -> &Token {
        if self.peeked.is_none() {
            let t = self.next();
            self.peeked = Some(t);
        }
        self.peeked.as_ref().expect("刚放进去")
    }
}

fn ascii_nodes(lexer: &mut Lexer, depth: usize) -> Result<Vec<FbxNode>, LoadError> {
    if depth > 64 {
        return Err(bad("FBX 节点嵌套过深"));
    }
    let mut nodes = Vec::new();
    loop {
        match lexer.next() {
            Token::Name(name) => nodes.push(ascii_node(lexer, name, depth)?),
            Token::Close | Token::Eof => return Ok(nodes),
            _ => {}
        }
    }
}

fn ascii_node(lexer: &mut Lexer, name: String, depth: usize) -> Result<FbxNode, LoadError> {
    let mut node = FbxNode { name, ..Default::default() };
    loop {
        match lexer.peek().clone() {
            Token::Value(v) => {
                lexer.next();
                node.props.push(v);
            }
            Token::Comma => {
                lexer.next();
            }
            Token::Star => {
                // `*N { a: 1,2,3 }`：数组。
                lexer.next();
                let _count = lexer.next();
                if !matches!(lexer.next(), Token::Open) {
                    return Err(bad("FBX ASCII：数组后面应当是 {"));
                }
                let _a = lexer.next();
                let mut ints = Vec::new();
                let mut floats = Vec::new();
                let mut all_int = true;
                loop {
                    match lexer.next() {
                        Token::Value(Prop::Int(i)) => {
                            ints.push(i);
                            floats.push(i as f64);
                        }
                        Token::Value(Prop::Float(f)) => {
                            all_int = false;
                            floats.push(f);
                        }
                        Token::Comma | Token::Value(_) => {}
                        _ => break,
                    }
                    if floats.len() > limits::VERTICES * 4 {
                        return Err(bad("FBX 数组超过上限"));
                    }
                }
                node.props.push(if all_int { Prop::Ints(ints) } else { Prop::Floats(floats) });
            }
            Token::Open => {
                lexer.next();
                node.children = ascii_nodes(lexer, depth + 1)?;
                return Ok(node);
            }
            _ => return Ok(node),
        }
    }
}

/// 把整个文件解析成顶层记录。
pub fn parse_document(bytes: &[u8]) -> Result<Vec<FbxNode>, LoadError> {
    if bytes.starts_with(BINARY_MAGIC) {
        parse_binary(bytes)
    } else {
        let mut lexer = Lexer { text: bytes, at: 0, peeked: None };
        ascii_nodes(&mut lexer, 0)
    }
}

// ───────────────────────────── 场景 ─────────────────────────────

/// FBX 的时间单位：1 秒 = 46186158000 个 tick。
const TICKS_PER_SECOND: f64 = 46_186_158_000.0;

struct Scene<'a> {
    objects: HashMap<i64, &'a FbxNode>,
    /// `(子, 父, 属性名)`，按文件顺序。
    connections: Vec<(i64, i64, Option<String>)>,
}

impl<'a> Scene<'a> {
    fn children(&self, parent: i64) -> impl Iterator<Item = (i64, Option<&str>)> + '_ {
        self.connections.iter().filter(move |c| c.1 == parent).map(|c| (c.0, c.2.as_deref()))
    }
    fn parents(&self, child: i64) -> impl Iterator<Item = (i64, Option<&str>)> + '_ {
        self.connections.iter().filter(move |c| c.0 == child).map(|c| (c.1, c.2.as_deref()))
    }
    fn object(&self, id: i64) -> Option<&'a FbxNode> {
        self.objects.get(&id).copied()
    }
    fn children_of_kind(&self, parent: i64, kind: &str) -> Vec<&'a FbxNode> {
        self.children(parent).filter_map(|(c, _)| self.object(c)).filter(|o| o.name == kind).collect()
    }
}

fn euler(order: i64, degrees: Vec3) -> Quat {
    let r = degrees * (std::f32::consts::PI / 180.0);
    let (x, y, z) = (Quat::from_rotation_x(r.x), Quat::from_rotation_y(r.y), Quat::from_rotation_z(r.z));
    // FBX 的 eEulerABC 是「先绕 A、再绕 B、最后绕 C」（固定轴），矩阵是 R_C · R_B · R_A。
    match order {
        1 => y * z * x, // eEulerXZY
        2 => x * z * y, // eEulerYZX
        3 => z * x * y, // eEulerYXZ
        4 => y * x * z, // eEulerZXY
        5 => x * y * z, // eEulerZYX
        _ => z * y * x, // eEulerXYZ（以及 eSphericXYZ）
    }
}

/// 一个 Model 的静态变换参数。
#[derive(Clone)]
struct Transform {
    translation: Vec3,
    rotation: Vec3,
    scaling: Vec3,
    pre_rotation: Vec3,
    post_rotation: Vec3,
    rotation_offset: Vec3,
    rotation_pivot: Vec3,
    scaling_offset: Vec3,
    scaling_pivot: Vec3,
    order: i64,
}

impl Transform {
    fn read(props: &HashMap<String, Vec<Prop>>) -> Self {
        Self {
            translation: prop_vec3(props, "Lcl Translation", Vec3::ZERO),
            rotation: prop_vec3(props, "Lcl Rotation", Vec3::ZERO),
            scaling: prop_vec3(props, "Lcl Scaling", Vec3::ONE),
            pre_rotation: prop_vec3(props, "PreRotation", Vec3::ZERO),
            post_rotation: prop_vec3(props, "PostRotation", Vec3::ZERO),
            rotation_offset: prop_vec3(props, "RotationOffset", Vec3::ZERO),
            rotation_pivot: prop_vec3(props, "RotationPivot", Vec3::ZERO),
            scaling_offset: prop_vec3(props, "ScalingOffset", Vec3::ZERO),
            scaling_pivot: prop_vec3(props, "ScalingPivot", Vec3::ZERO),
            order: props.get("RotationOrder").and_then(|v| v.first()?.as_i64()).unwrap_or(0),
        }
    }

    /// FBX SDK 文档里的那条变换链。
    fn matrix(&self) -> Mat4 {
        let pre = Mat4::from_quat(euler(0, self.pre_rotation));
        let post = Mat4::from_quat(euler(0, self.post_rotation));
        let rotation = Mat4::from_quat(euler(self.order, self.rotation));
        Mat4::from_translation(self.translation)
            * Mat4::from_translation(self.rotation_offset)
            * Mat4::from_translation(self.rotation_pivot)
            * pre
            * rotation
            * post.inverse()
            * Mat4::from_translation(-self.rotation_pivot)
            * Mat4::from_translation(self.scaling_offset)
            * Mat4::from_translation(self.scaling_pivot)
            * Mat4::from_scale(self.scaling)
            * Mat4::from_translation(-self.scaling_pivot)
    }
}

fn decompose(matrix: Mat4) -> NodeTransform {
    let (scale, rotation, position) = matrix.to_scale_rotation_translation();
    NodeTransform {
        position,
        rotation: if rotation.is_finite() { rotation.normalize() } else { Quat::IDENTITY },
        scale,
    }
}

fn matrix_from_array(v: &[f64]) -> Mat4 {
    if v.len() < 16 {
        return Mat4::IDENTITY;
    }
    // FBX 的矩阵是列主序。
    Mat4::from_cols_array(&std::array::from_fn(|i| v[i] as f32))
}

/// 一个 `LayerElement*`：数据 + 映射方式。
struct Layer {
    values: Vec<f64>,
    indices: Option<Vec<i64>>,
    mapping: String,
    components: usize,
}

impl Layer {
    fn read(geometry: &FbxNode, element: &str, data: &str, index: &str, components: usize) -> Option<Self> {
        let layer = geometry.child(element)?;
        let reference = layer.child_str("ReferenceInformationType").unwrap_or("Direct");
        Some(Self {
            values: layer.array_f64(data),
            indices: (reference == "IndexToDirect" || reference == "Index").then(|| layer.array_i64(index)),
            mapping: layer.child_str("MappingInformationType").unwrap_or("ByPolygonVertex").to_string(),
            components,
        })
    }

    /// 第 `corner` 个多边形角（属于第 `polygon` 个多边形、位置下标 `vertex`）的值。
    fn get(&self, corner: usize, polygon: usize, vertex: usize) -> Option<&[f64]> {
        let slot = match self.mapping.as_str() {
            "ByVertice" | "ByVertex" | "ByControlPoint" => vertex,
            "ByPolygon" => polygon,
            "AllSame" => 0,
            _ => corner,
        };
        let slot = match &self.indices {
            Some(indices) => usize::try_from(*indices.get(slot)?).ok()?,
            None => slot,
        };
        self.values.get(slot * self.components..(slot + 1) * self.components)
    }
}

/// 建好的一块几何。
struct Built {
    mesh: Mesh,
    material_slot: usize,
    position_index: Vec<usize>,
}

fn build_geometry(geometry: &FbxNode, geometric: Mat4) -> Result<Vec<Built>, LoadError> {
    let positions = geometry.array_f64("Vertices");
    let polygon_indices = geometry.array_i64("PolygonVertexIndex");
    if positions.len() / 3 > limits::VERTICES || polygon_indices.len() > limits::VERTICES * 2 {
        return Err(bad("FBX 网格超过上限"));
    }
    let normals = Layer::read(geometry, "LayerElementNormal", "Normals", "NormalsIndex", 3)
        .or_else(|| Layer::read(geometry, "LayerElementNormal", "Normals", "NormalIndex", 3));
    let uvs = Layer::read(geometry, "LayerElementUV", "UV", "UVIndex", 2);
    let colors = Layer::read(geometry, "LayerElementColor", "Colors", "ColorIndex", 4);
    let materials = geometry.child("LayerElementMaterial").map(|l| {
        (
            l.array_i64("Materials"),
            l.child_str("MappingInformationType").unwrap_or("AllSame").to_string(),
        )
    });
    let normal_matrix = geometric.inverse().transpose();

    // 材质槽 → (顶点, 索引, 位置下标, 去重表)
    #[allow(clippy::type_complexity)]
    let mut batches: HashMap<usize, (Vec<Vertex>, Vec<u32>, Vec<usize>, HashMap<[u32; 10], u32>)> = HashMap::new();
    let mut corner = 0usize;
    let mut polygon = 0usize;
    let mut current: Vec<(usize, usize)> = Vec::new();
    for &raw in &polygon_indices {
        let last = raw < 0;
        let vertex = if last { !raw } else { raw } as usize;
        current.push((vertex, corner));
        corner += 1;
        if !last {
            continue;
        }
        let slot = match &materials {
            Some((list, mapping)) if mapping == "AllSame" => list.first().copied().unwrap_or(0),
            Some((list, _)) => list.get(polygon).copied().unwrap_or(0),
            None => 0,
        }
        .max(0) as usize;
        let batch = batches.entry(slot).or_insert_with(|| (Vec::new(), Vec::new(), Vec::new(), HashMap::new()));
        let mut corner_indices = Vec::with_capacity(current.len());
        for &(vertex, corner) in &current {
            let p = positions.get(vertex * 3..vertex * 3 + 3).ok_or_else(|| bad("FBX 多边形引用了不存在的顶点"))?;
            let position = geometric.transform_point3(Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32));
            let normal = normals
                .as_ref()
                .and_then(|l| l.get(corner, polygon, vertex))
                .map(|n| normal_matrix.transform_vector3(Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32)).normalize_or_zero());
            let uv = uvs.as_ref().and_then(|l| l.get(corner, polygon, vertex)).map(|t| [t[0] as f32, 1.0 - t[1] as f32]);
            let color = colors.as_ref().and_then(|l| l.get(corner, polygon, vertex)).map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]);
            let key = [
                vertex as u32,
                normal.map_or(0, |n| n.x.to_bits()),
                normal.map_or(0, |n| n.y.to_bits()),
                normal.map_or(0, |n| n.z.to_bits()),
                uv.map_or(0, |u| u[0].to_bits()),
                uv.map_or(0, |u| u[1].to_bits()),
                color.map_or(0, |c| c[0].to_bits()),
                color.map_or(0, |c| c[1].to_bits()),
                color.map_or(0, |c| c[2].to_bits()),
                0,
            ];
            let index = *batch.3.entry(key).or_insert_with(|| {
                batch.0.push(Vertex {
                    position: position.to_array(),
                    normal: normal.map_or([0.0, 1.0, 0.0], |n| n.to_array()),
                    uv: uv.unwrap_or([0.0, 0.0]),
                    color: color.unwrap_or([1.0; 3]),
                    ..Default::default()
                });
                batch.2.push(vertex);
                batch.0.len() as u32 - 1
            });
            corner_indices.push(index);
        }
        for k in 1..corner_indices.len().saturating_sub(1) {
            batch.1.extend_from_slice(&[corner_indices[0], corner_indices[k], corner_indices[k + 1]]);
        }
        current.clear();
        polygon += 1;
    }

    let mut out = Vec::new();
    let mut slots: Vec<usize> = batches.keys().copied().collect();
    slots.sort_unstable();
    for slot in slots {
        let (vertices, indices, position_index, _) = batches.remove(&slot).expect("键来自表本身");
        let mut mesh = Mesh::new(vertices, indices);
        if !mesh.is_valid() || mesh.triangle_count() == 0 {
            continue;
        }
        if normals.is_none() {
            mesh.recompute_normals();
        }
        mesh.recompute_tangents();
        out.push(Built { mesh, material_slot: slot, position_index });
    }
    Ok(out)
}

fn build_material(scene: &Scene, material: &FbxNode, textures: &HashMap<i64, Resource<Texture>>) -> Material {
    let props = properties(material);
    let diffuse = prop_vec3(&props, "DiffuseColor", prop_vec3(&props, "Diffuse", Vec3::splat(0.8)));
    let factor = prop_f32(&props, "DiffuseFactor").unwrap_or(1.0);
    let opacity = prop_f32(&props, "Opacity").unwrap_or_else(|| {
        let transparency = prop_f32(&props, "TransparencyFactor").unwrap_or(0.0);
        // TransparentColor 为黑时 TransparencyFactor 没有意义（很多导出器乱写）。
        let transparent = prop_vec3(&props, "TransparentColor", Vec3::ZERO);
        if transparent.max_element() > 0.0 { 1.0 - transparency } else { 1.0 }
    });
    let mut out = Material::standard()
        .with_base_color((diffuse * factor).extend(opacity.clamp(0.0, 1.0)))
        .with_metallic(0.0)
        .with_roughness(prop_f32(&props, "Shininess").or_else(|| prop_f32(&props, "ShininessExponent")).map_or(0.6, |s| (2.0 / (s.max(0.0) + 2.0)).sqrt().clamp(0.05, 1.0)));
    let emissive = prop_vec3(&props, "EmissiveColor", Vec3::ZERO) * prop_f32(&props, "EmissiveFactor").unwrap_or(1.0);
    if emissive.max_element() > 0.0 {
        out.set(kpbr::standard::EMISSIVE, emissive);
    }
    if opacity < 0.999 {
        out.set_blend_mode(kmaterial::BlendMode::Alpha);
    }
    for (texture_id, property) in scene.children(material.id()) {
        let Some(texture) = textures.get(&texture_id) else { continue };
        match property.unwrap_or("") {
            "DiffuseColor" | "Diffuse" | "Maya|TEX_color_map" | "3dsMax|maps|texmap_diffuse" => {
                out = out.with_base_color_texture(texture.clone()).with_base_color(Vec4::new(1.0, 1.0, 1.0, opacity));
            }
            "NormalMap" | "Maya|TEX_normal_map" => {
                // 贴图是按 sRGB 读进来的，法线要线性。
                let data = texture.data_ref().map(|t| t.clone().with_format(ktexture::TextureFormat::Linear));
                if let Some(data) = data {
                    out.set(kpbr::standard::NORMAL_TEXTURE, Resource::new_ok(format!("fbx-normal-{texture_id}"), data));
                    out.set("normal_scale", 1.0f32);
                }
            }
            "EmissiveColor" | "Emissive" => {
                out.set(kpbr::standard::EMISSIVE_TEXTURE, texture.clone());
                if emissive.max_element() <= 0.0 {
                    out.set(kpbr::standard::EMISSIVE, Vec3::ONE);
                }
            }
            "TransparentColor" | "TransparencyFactor" => out.set_blend_mode(kmaterial::BlendMode::Alpha),
            _ => {}
        }
    }
    out.set_name(material.object_name());
    out
}

/// 一条 NURBS 曲线采样成折线（节点局部空间）。
fn sample_nurbs(geometry: &FbxNode) -> Vec<Vec3> {
    let order = geometry.child_i64("Order").unwrap_or(4).max(2) as usize;
    let degree = order - 1;
    let raw = geometry.array_f64("Points");
    let mut knots = geometry.array_f64("KnotVector");
    let mut points: Vec<[f64; 4]> = raw.chunks_exact(4).map(|p| [p[0], p[1], p[2], if p[3] == 0.0 { 1.0 } else { p[3] }]).collect();
    if points.len() < 2 || knots.is_empty() {
        return Vec::new();
    }
    let form = geometry.child_str("Form").unwrap_or("Open");
    let (mut start, mut end) = (0usize, knots.len() - 1);
    match form {
        "Closed" => points.push(points[0]),
        "Periodic" => {
            start = degree;
            end = knots.len() - 1 - start;
            for i in 0..degree {
                points.push(points[i % points.len()]);
            }
        }
        _ => {}
    }
    // 结点数必须是「控制点 + 阶」；不够时均匀补齐，避免越界。
    while knots.len() < points.len() + order {
        let last = *knots.last().unwrap_or(&1.0);
        knots.push(last);
    }
    end = end.min(knots.len() - 1);
    let (u0, u1) = (knots[start], knots[end]);
    let samples = points.len() * 12;
    (0..=samples)
        .map(|k| {
            let u = u0 + (u1 - u0) * k as f64 / samples as f64;
            let p = crate::nurbs::de_boor(degree, &knots, &points, u);
            Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
        })
        .collect()
}

/// 解析成 [`Model`]。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    Ok(parse_fbx(bytes, path, io).await?.model)
}

/// 解析成 [`Fbx`]。
pub async fn parse_fbx(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Fbx, LoadError> {
    let document = parse_document(&bytes)?;
    // 贴图：内嵌的直接解，外部的按文件名找。先收集，异步读完再建场景。
    let objects = document.iter().find(|n| n.name == "Objects");
    let connections = connections(&document);
    let mut textures = HashMap::new();
    let base = crate::base_dir(&path);
    if let Some(objects) = objects {
        let videos: HashMap<i64, &FbxNode> = objects.children_named("Video").map(|v| (v.id(), v)).collect();
        for texture in objects.children_named("Texture") {
            let id = texture.id();
            let embedded = connections
                .iter()
                .filter(|c| c.1 == id)
                .find_map(|c| videos.get(&c.0))
                .and_then(|video| match video.child("Content")?.props.first()? {
                    Prop::Bytes(b) if !b.is_empty() => Some(b.clone()),
                    Prop::Str(s) if !s.is_empty() => base64_decode(s),
                    _ => None,
                });
            let name = texture.child_str("RelativeFilename").filter(|s| !s.is_empty()).or_else(|| texture.child_str("FileName")).unwrap_or("").to_string();
            let resource = match embedded {
                Some(bytes) => crate::texture_from_bytes(&format!("{}#{name}", path.display()), &bytes, false),
                None if !name.is_empty() => crate::load_texture(&io, &base, &name, false).await,
                None => None,
            };
            if let Some(resource) = resource {
                textures.insert(id, resource);
            } else if !name.is_empty() {
                klog::warn!("FBX 贴图 {name} 找不到");
            }
        }
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "FBX".into());
    build(&document, &textures, &name)
}

fn connections(document: &[FbxNode]) -> Vec<(i64, i64, Option<String>)> {
    document
        .iter()
        .find(|n| n.name == "Connections")
        .map(|c| {
            c.children
                .iter()
                .filter(|c| c.name == "C" || c.name == "Connect")
                .filter_map(|c| {
                    let child = c.props.get(1)?.as_i64()?;
                    let parent = c.props.get(2)?.as_i64()?;
                    Some((child, parent, c.props.get(3).and_then(Prop::as_str).map(str::to_string)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        buffer = (buffer << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// 从文档树建场景。
pub fn build(document: &[FbxNode], textures: &HashMap<i64, Resource<Texture>>, name: &str) -> Result<Fbx, LoadError> {
    let objects_node = document.iter().find(|n| n.name == "Objects").ok_or_else(|| bad("FBX 里没有 Objects 段"))?;
    let scene = Scene {
        objects: objects_node.children.iter().map(|o| (o.id(), o)).collect(),
        connections: connections(document),
    };
    let settings = document.iter().find(|n| n.name == "GlobalSettings").map(properties).unwrap_or_default();
    let up_axis = settings.get("UpAxis").and_then(|v| v.first()?.as_i64()).unwrap_or(1);
    let unit_scale = prop_f32(&settings, "UnitScaleFactor").unwrap_or(1.0);

    // ── 节点 ──
    let models: Vec<&FbxNode> = objects_node.children_named("Model").collect();
    if models.len() > limits::NODES {
        return Err(bad("FBX 节点数超过上限"));
    }
    let mut nodes = vec![ModelNode {
        name: name.to_string(),
        transform: NodeTransform {
            rotation: if up_axis == 2 { Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2) } else { Quat::IDENTITY },
            ..Default::default()
        },
        ..Default::default()
    }];
    let mut node_of: HashMap<i64, usize> = HashMap::new();
    let mut transforms: Vec<Transform> = vec![Transform::read(&HashMap::new())];
    for model in &models {
        let props = properties(model);
        let transform = Transform::read(&props);
        node_of.insert(model.id(), nodes.len());
        nodes.push(ModelNode {
            name: model.object_name(),
            transform: decompose(transform.matrix()),
            ..Default::default()
        });
        transforms.push(transform);
    }
    for model in &models {
        let index = node_of[&model.id()];
        let parent = scene
            .parents(model.id())
            .find_map(|(p, prop)| if prop.is_none() { node_of.get(&p).copied() } else { None })
            .unwrap_or(0);
        nodes[parent].children.push(index);
    }

    // ── 材质 ──
    let mut materials = vec![Material::standard().with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0)).with_roughness(0.6)];
    let mut material_of: HashMap<i64, usize> = HashMap::new();
    for material in objects_node.children_named("Material") {
        material_of.insert(material.id(), materials.len());
        materials.push(build_material(&scene, material, textures));
    }

    // ── 几何、蒙皮、形变 ──
    let mut meshes = Vec::new();
    let mut skins = Vec::new();
    let mut curves = Vec::new();
    // 形变通道 id → (节点, 形变目标序号)，给动画用。
    let mut morph_channels: HashMap<i64, Vec<(usize, usize)>> = HashMap::new();
    let mut vertex_total = 0usize;
    for model in &models {
        let node = node_of[&model.id()];
        let props = properties(model);
        let geometric = Mat4::from_translation(prop_vec3(&props, "GeometricTranslation", Vec3::ZERO))
            * Mat4::from_quat(euler(0, prop_vec3(&props, "GeometricRotation", Vec3::ZERO)))
            * Mat4::from_scale(prop_vec3(&props, "GeometricScaling", Vec3::ONE));
        let model_materials: Vec<usize> = scene
            .children(model.id())
            .filter_map(|(c, _)| material_of.get(&c).copied())
            .collect();
        for geometry in scene.children_of_kind(model.id(), "Geometry") {
            if geometry.subclass() == "NurbsCurve" || geometry.child_str("Type") == Some("NurbsCurve") {
                let points: Vec<Vec3> = sample_nurbs(geometry).into_iter().map(|p| geometric.transform_point3(p)).collect();
                if points.len() > 1 {
                    curves.push((node, points));
                }
                continue;
            }
            if geometry.child("Vertices").is_none() {
                continue;
            }
            let built = build_geometry(geometry, geometric)?;

            // 蒙皮：Skin 挂在几何上，Cluster 挂在 Skin 上，骨头 Model 挂在 Cluster 上。
            let mut skin_index = None;
            let mut per_position: HashMap<usize, Vec<(u16, f32)>> = HashMap::new();
            for skin in scene.children_of_kind(geometry.id(), "Deformer").into_iter().filter(|d| d.subclass() == "Skin") {
                let mut joints = Vec::new();
                let mut inverse_bind = Vec::new();
                for cluster in scene.children_of_kind(skin.id(), "Deformer").into_iter().filter(|d| d.subclass() == "Cluster") {
                    let Some(bone) = scene.children(cluster.id()).find_map(|(c, _)| node_of.get(&c).copied()) else { continue };
                    let joint = joints.len() as u16;
                    joints.push(bone);
                    let transform = matrix_from_array(&cluster.array_f64("Transform"));
                    let link = matrix_from_array(&cluster.array_f64("TransformLink"));
                    // 顶点在网格空间；link 是绑定时骨头的世界变换，transform 是绑定时网格的世界变换。
                    inverse_bind.push(link.inverse() * transform);
                    let indices = cluster.array_i64("Indexes");
                    let weights = cluster.array_f64("Weights");
                    for (i, w) in indices.iter().zip(&weights) {
                        per_position.entry(*i as usize).or_default().push((joint, *w as f32));
                    }
                }
                if !joints.is_empty() {
                    skins.push(ModelSkin { joints, inverse_bind, skeleton: None });
                    skin_index = Some(skins.len() - 1);
                }
                break;
            }

            // 形变：BlendShape → Channel → Shape（Geometry）。
            let mut shapes: Vec<(i64, String, f32, &FbxNode)> = Vec::new();
            for blend in scene.children_of_kind(geometry.id(), "Deformer").into_iter().filter(|d| d.subclass() == "BlendShape") {
                for channel in scene.children_of_kind(blend.id(), "Deformer").into_iter().filter(|d| d.subclass() == "BlendShapeChannel") {
                    let weight = prop_f32(&properties(channel), "DeformPercent").unwrap_or(0.0) / 100.0;
                    if let Some(shape) = scene.children_of_kind(channel.id(), "Geometry").into_iter().next() {
                        shapes.push((channel.id(), channel.object_name(), weight, shape));
                    }
                }
            }

            for piece in built {
                let mut mesh = piece.mesh;
                vertex_total += mesh.vertices().len();
                if vertex_total > limits::VERTICES {
                    return Err(bad("FBX 顶点数超过上限"));
                }
                if skin_index.is_some() {
                    let skin: Vec<SkinVertex> = piece
                        .position_index
                        .iter()
                        .map(|p| {
                            let mut list = per_position.get(p).cloned().unwrap_or_default();
                            list.sort_by(|a, b| b.1.total_cmp(&a.1));
                            list.truncate(4);
                            let mut v = SkinVertex { joints: [0; 4], weights: [0.0; 4] };
                            for (k, (j, w)) in list.into_iter().enumerate() {
                                v.joints[k] = j;
                                v.weights[k] = w;
                            }
                            if v.weights.iter().all(|w| *w == 0.0) {
                                v.weights[0] = 1.0;
                            }
                            v
                        })
                        .collect();
                    mesh = mesh.with_skin(skin);
                }
                if !shapes.is_empty() {
                    let mut targets = Vec::new();
                    let mut weights = Vec::new();
                    for (slot, (channel_id, channel_name, weight, shape)) in shapes.iter().enumerate() {
                        let indices = shape.array_i64("Indexes");
                        let deltas = shape.array_f64("Vertices");
                        let normals = shape.array_f64("Normals");
                        let mut by_position: HashMap<usize, (Vec3, Vec3)> = HashMap::new();
                        for (k, &index) in indices.iter().enumerate() {
                            let d = deltas.get(k * 3..k * 3 + 3).map_or(Vec3::ZERO, |d| Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32));
                            let n = normals.get(k * 3..k * 3 + 3).map_or(Vec3::ZERO, |n| Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32));
                            by_position.insert(index as usize, (geometric.transform_vector3(d), n));
                        }
                        let morph = piece
                            .position_index
                            .iter()
                            .map(|p| {
                                let (d, n) = by_position.get(p).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
                                MorphDelta { position: d.to_array(), normal: n.to_array(), ..Default::default() }
                            })
                            .collect();
                        targets.push(MorphTarget::new(channel_name.clone(), morph));
                        weights.push(*weight);
                        morph_channels.entry(*channel_id).or_default().push((node, slot));
                    }
                    mesh = mesh.with_morph_targets(targets, weights);
                }
                let material = model_materials.get(piece.material_slot).or_else(|| model_materials.first()).copied().unwrap_or(0);
                nodes[node].parts.push(MeshPart { mesh: meshes.len(), material: Some(material) });
                meshes.push(mesh);
            }
            if skin_index.is_some() {
                nodes[node].skin = skin_index;
            }
        }
    }
    // 同一形变通道可能被拆成的多块几何重复登记，去重。
    for list in morph_channels.values_mut() {
        list.sort_unstable();
        list.dedup();
    }

    let animations = build_animations(&scene, objects_node, &node_of, &transforms, &morph_channels);
    let mut model = Model::new(meshes, materials, nodes, vec![0]).with_skins(skins);
    if !animations.is_empty() {
        model = model.with_animations(animations);
    }
    if model.meshes().is_empty() && curves.is_empty() && model.nodes().len() <= 1 {
        return Err(bad("FBX 里没有可导入的几何"));
    }
    Ok(Fbx { model, curves, unit_scale })
}

/// 一条动画曲线：tick 时刻（换成秒）+ 值。
struct AnimCurve {
    times: Vec<f32>,
    values: Vec<f32>,
}

impl AnimCurve {
    fn sample(&self, time: f32) -> f32 {
        let n = self.times.len().min(self.values.len());
        if n == 0 {
            return 0.0;
        }
        let next = self.times[..n].partition_point(|&t| t <= time);
        if next == 0 {
            return self.values[0];
        }
        if next >= n {
            return self.values[n - 1];
        }
        let (t0, t1) = (self.times[next - 1], self.times[next]);
        let f = if t1 > t0 { (time - t0) / (t1 - t0) } else { 0.0 };
        self.values[next - 1] + (self.values[next] - self.values[next - 1]) * f
    }
}

fn build_animations(
    scene: &Scene,
    objects: &FbxNode,
    node_of: &HashMap<i64, usize>,
    transforms: &[Transform],
    morph_channels: &HashMap<i64, Vec<(usize, usize)>>,
) -> Vec<AnimationClip> {
    let curve_of = |id: i64| -> Option<AnimCurve> {
        let curve = scene.object(id).filter(|o| o.name == "AnimationCurve")?;
        let times = curve.array_i64("KeyTime").iter().map(|&t| (t as f64 / TICKS_PER_SECOND) as f32).collect();
        let values = curve.array_f64("KeyValueFloat").iter().map(|&v| v as f32).collect();
        Some(AnimCurve { times, values })
    };
    let mut clips = Vec::new();
    for stack in objects.children_named("AnimationStack") {
        let mut tracks = Vec::new();
        // 节点 → [平移, 旋转, 缩放] 各三个分量的曲线。
        let mut per_node: HashMap<usize, [[Option<AnimCurve>; 3]; 3]> = HashMap::new();
        for layer in scene.children_of_kind(stack.id(), "AnimationLayer") {
            for curve_node in scene.children_of_kind(layer.id(), "AnimationCurveNode") {
                for (target, property) in scene.parents(curve_node.id()) {
                    let property = property.unwrap_or("");
                    if let Some(&node) = node_of.get(&target) {
                        let slot = match property {
                            "Lcl Translation" => 0,
                            "Lcl Rotation" => 1,
                            "Lcl Scaling" => 2,
                            _ => continue,
                        };
                        let entry = per_node.entry(node).or_default();
                        for (curve, axis) in scene.children(curve_node.id()) {
                            let k = match axis.unwrap_or("") {
                                "d|X" => 0,
                                "d|Y" => 1,
                                "d|Z" => 2,
                                _ => continue,
                            };
                            if let Some(c) = curve_of(curve) {
                                entry[slot][k] = Some(c);
                            }
                        }
                    } else if property == "DeformPercent"
                        && let Some(targets) = morph_channels.get(&target)
                    {
                        for (curve, _) in scene.children(curve_node.id()) {
                            let Some(c) = curve_of(curve) else { continue };
                            let weights: Vec<f32> = c.values.iter().map(|v| v / 100.0).collect();
                            for &(node, index) in targets {
                                if let Some(curve) = Curve::new(c.times.clone(), weights.clone(), Interpolation::Linear) {
                                    tracks.push(Track { target: node, channel: Channel::MorphWeight { index, curve } });
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut nodes: Vec<usize> = per_node.keys().copied().collect();
        nodes.sort_unstable();
        for node in nodes {
            let channels = &per_node[&node];
            let mut times = BTreeSet::new();
            for c in channels.iter().flatten().flatten() {
                for t in &c.times {
                    times.insert(t.max(0.0).to_bits());
                }
            }
            let times: Vec<f32> = times.into_iter().map(f32::from_bits).collect();
            if times.is_empty() {
                continue;
            }
            let base = &transforms[node];
            let (mut positions, mut rotations, mut scales) = (Vec::new(), Vec::new(), Vec::new());
            for &time in &times {
                let mut t = base.clone();
                let pick = |slot: usize, default: Vec3| {
                    let mut v = default;
                    for k in 0..3 {
                        if let Some(c) = &channels[slot][k] {
                            v[k] = c.sample(time);
                        }
                    }
                    v
                };
                t.translation = pick(0, base.translation);
                t.rotation = pick(1, base.rotation);
                t.scaling = pick(2, base.scaling);
                let d = decompose(t.matrix());
                positions.push(d.position);
                rotations.push(d.rotation);
                scales.push(d.scale);
            }
            for k in 1..rotations.len() {
                if rotations[k].dot(rotations[k - 1]) < 0.0 {
                    rotations[k] = -rotations[k];
                }
            }
            if let Some(c) = Curve::new(times.clone(), positions, Interpolation::Linear) {
                tracks.push(Track { target: node, channel: Channel::Position(c) });
            }
            if let Some(c) = Curve::new(times.clone(), rotations, Interpolation::Linear) {
                tracks.push(Track { target: node, channel: Channel::Rotation(c) });
            }
            if let Some(c) = Curve::new(times, scales, Interpolation::Linear) {
                tracks.push(Track { target: node, channel: Channel::Scale(c) });
            }
        }
        if !tracks.is_empty() {
            clips.push(AnimationClip::new(stack.object_name(), tracks));
        }
    }
    clips
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII: &str = r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
    FBXVersion: 7400
}
Objects:  {
    Geometry: 10, "Geometry::Quad", "Mesh" {
        Vertices: *12 {
            a: 0,0,0,1,0,0,1,1,0,0,1,0
        }
        PolygonVertexIndex: *4 {
            a: 0,1,2,-4
        }
    }
    Model: 20, "Model::Quad", "Mesh" {
        Properties70:  {
            P: "Lcl Translation", "Lcl Translation", "", "A",1,2,3
            P: "Lcl Rotation", "Lcl Rotation", "", "A",0,0,90
        }
    }
    Material: 30, "Material::Red", "" {
        Properties70:  {
            P: "DiffuseColor", "Color", "", "A",1,0,0
        }
    }
}
Connections:  {
    C: "OO",10,20
    C: "OO",30,20
    C: "OO",20,0
}
"#;

    #[test]
    fn ascii_quad_with_transform_and_material() {
        let document = parse_document(ASCII.as_bytes()).unwrap();
        let fbx = build(&document, &HashMap::new(), "t").unwrap();
        let model = fbx.model;
        assert_eq!(model.triangle_count(), 2);
        let node = &model.nodes()[1];
        assert_eq!(node.name, "Quad");
        assert!((node.transform.position - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5);
        assert!(node.transform.rotation.dot(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)).abs() > 0.9999);
        let material = node.parts[0].material.unwrap();
        assert_eq!(model.materials()[material].base_color(), Vec4::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn euler_order_xyz_applies_x_first() {
        // eEulerXYZ：先绕 X 再绕 Y。X 轴上的点绕 X 不动，再绕 Y 转 90° 到 -Z。
        let q = euler(0, Vec3::new(90.0, 90.0, 0.0));
        let p = q * Vec3::X;
        assert!((p - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5, "{p:?}");
    }

    #[test]
    fn nurbs_curve_passes_through_clamped_endpoints() {
        let geometry = FbxNode {
            name: "Geometry".into(),
            props: vec![Prop::Int(1), Prop::Str("Geometry::".into()), Prop::Str("NurbsCurve".into())],
            children: vec![
                FbxNode { name: "Order".into(), props: vec![Prop::Int(3)], children: vec![] },
                FbxNode { name: "Form".into(), props: vec![Prop::Str("Open".into())], children: vec![] },
                FbxNode { name: "Points".into(), props: vec![Prop::Floats(vec![0., 0., 0., 1., 1., 2., 0., 1., 2., 0., 0., 1.])], children: vec![] },
                FbxNode { name: "KnotVector".into(), props: vec![Prop::Floats(vec![0., 0., 0., 1., 1., 1.])], children: vec![] },
            ],
        };
        let points = sample_nurbs(&geometry);
        assert!((points[0] - Vec3::ZERO).length() < 1e-5);
        assert!((points.last().unwrap() - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-5);
        // 二次贝塞尔的中点：(1, 1, 0)。
        let middle = points[points.len() / 2];
        assert!((middle - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-4, "{middle:?}");
    }
}
