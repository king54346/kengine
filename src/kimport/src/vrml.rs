//! VRML 97（VRML 2.0）。
//!
//! 分两层：
//!
//! 1. **语法**：一个不认识任何节点类型的通用解析器，把文件读成节点树
//!    （`DEF`/`USE` 共享同一个节点）。字段的类型是按**值长什么样**推断的
//!    ——数字就连着吃数字、`[` 就读列表、类型名后面跟 `{` 就是子节点——
//!    所以不认识的节点（VRML 1 残留的 `PerspectiveCamera`、`Script`、
//!    传感器、插值器）也能被完整跳过，不会让后面的内容错位。
//! 2. **建场景**：只看认识的节点，产出 [`VrmlScene`]。
//!
//! # 支持
//!
//! | 节点 | |
//! |---|---|
//! | `Transform` `Group` `Anchor` `Collision` `Billboard` | 层级（`Transform` 的 `center`、`scaleOrientation` 都算进去） |
//! | `Switch` / `LOD` | 只取 `whichChoice` 那一个 / 最精细的第一级 |
//! | `Shape` `Appearance` `Material` | 漫反射、自发光、`shininess`→粗糙度、透明度；**没有 `Material` 时不受光**（规范如此） |
//! | `ImageTexture` `PixelTexture` `TextureTransform` | `repeatS/T`；`PixelTexture` 1~4 分量、最近邻采样 |
//! | `Box` `Sphere` `Cylinder` `Cone` | |
//! | `IndexedFaceSet` | 逐顶点 / 逐面的颜色与法线、各自的索引、`ccw`、`solid`、`convex`（非凸面走耳切）、`creaseAngle` 平滑 |
//! | `ElevationGrid` `Extrusion` | 按规范生成，`Extrusion` 的端盖可以是凹多边形 |
//! | `IndexedLineSet` `PointSet` | 分别并进一个 [`LineSet`] 与一朵 [`PointCloud`]（世界坐标） |
//! | `Background` | 天空 / 地面的渐变色（不含六面贴图） |
//!
//! # 不支持
//!
//! 灯光（例子自己打光，three.js 的 `VRMLLoader` 同样不读灯）、`Viewpoint`、
//! `Text`、`Inline`、`PROTO`（整段跳过）、`ROUTE` 与脚本驱动的交互动画
//! （`house.wrl` 里点门会开，这里门是静止的）、VRML 1.0 文件。
//!
//! # 颜色空间
//!
//! VRML 的颜色是作者在屏幕上挑出来的值，也就是 sRGB。这里一律转成线性再
//! 交给引擎，和 three.js 开启颜色管理之后的行为一致；不转的话所有颜色都会偏亮发灰。

use crate::{bad, limits, loader, path::triangulate, pcd::PointCloud, sibling};
use kasset::{LoadError, Resource, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kgizmo::{Color, LineSet, LineSetBuilder};
use kgltf::{MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::{BlendMode, Material};
use kmath::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use kpbr::unlit::UnlitMaterial;
use ktexture::{FilterMode, Sampler, Texture, TextureFormat, WrapMode};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

/// [`VrmlScene`] 的资源类型标识。
pub const VRML_TYPE_UUID: Uuid = uuid!("0f6d3c2a-8b41-4e7f-9a15-c3d8e2b74f06");

loader! {
    /// 读 `.wrl` / `.vrml`。
    VrmlLoader -> VrmlScene : ["wrl", "vrml"] = VRML_TYPE_UUID, load
}

/// 一份 VRML 读出来的全部东西。
#[derive(Debug, Clone)]
pub struct VrmlScene {
    /// 网格形体（面、基本体、高度场、挤出体）与节点层级。
    pub model: Model,
    /// 全部 `IndexedLineSet`，已经变换到世界坐标。
    pub lines: Option<LineSet>,
    /// 全部 `PointSet`，已经变换到世界坐标。
    pub points: Option<PointCloud>,
    /// 第一个 `Background` 节点。
    pub background: Option<Background>,
    /// 所有可见内容的世界包围盒，摆相机用。什么都没有时是 `(0, 0)`。
    pub bounds: (Vec3, Vec3),
}

impl ResourceData for VrmlScene {
    fn type_uuid(&self) -> Uuid {
        VRML_TYPE_UUID
    }
}

/// `Background` 的天空与地面渐变。
///
/// 规范的定义：天空是一个无穷远的球，`sky_colors[0]` 在天顶，
/// `sky_colors[i + 1]` 在离天顶 `sky_angles[i]` 弧度处，之间线性插值，
/// 最后一个角度之外一直是最后一个颜色。地面是套在里面的半球，
/// 从天底往上量，规则相同；地面盖住天空。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Background {
    /// 天空颜色（线性）。
    pub sky_colors: Vec<Vec3>,
    /// 天空颜色对应的角度（弧度，从天顶量）。
    pub sky_angles: Vec<f32>,
    /// 地面颜色（线性）。
    pub ground_colors: Vec<Vec3>,
    /// 地面颜色对应的角度（弧度，从天底量）。
    pub ground_angles: Vec<f32>,
}

impl Background {
    /// 离天顶 `polar` 弧度（0 = 正上方，π = 正下方）的方向上是什么颜色。
    pub fn color_at(&self, polar: f32) -> Vec3 {
        let from_nadir = std::f32::consts::PI - polar;
        if let Some(&last) = self.ground_angles.last()
            && self.ground_colors.len() >= 2
            && from_nadir <= last
        {
            return gradient(&self.ground_colors, &self.ground_angles, from_nadir);
        }
        gradient(&self.sky_colors, &self.sky_angles, polar)
    }

    /// 天空是不是单一颜色（这时直接当清屏色用就够，不必画球）。
    pub fn is_uniform(&self) -> bool {
        self.sky_colors.len() <= 1 && self.ground_colors.len() <= 1
    }

    /// 做一个半径为 `radius`、**朝内**、带顶点色的球，配不受光材质当天空穹顶。
    pub fn to_mesh(&self, radius: f32) -> Mesh {
        let sphere = Mesh::sphere(48, 32);
        let mut vertices = sphere.vertices().to_vec();
        for vertex in &mut vertices {
            let direction = vertex.normal();
            let polar = direction.y.clamp(-1.0, 1.0).acos();
            vertex.color = self.color_at(polar).to_array();
            vertex.position = (direction * radius).to_array();
            vertex.normal = (-direction).to_array();
        }
        // 翻转绕序：从球里面看才是正面。
        let mut indices = sphere.indices().to_vec();
        for triangle in indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
        Mesh::new(vertices, indices)
    }
}

fn gradient(colors: &[Vec3], angles: &[f32], angle: f32) -> Vec3 {
    let Some(&first) = colors.first() else {
        return Vec3::ZERO;
    };
    let mut previous_angle = 0.0;
    let mut previous_color = first;
    for (index, &next_angle) in angles.iter().enumerate() {
        let Some(&next_color) = colors.get(index + 1) else {
            break;
        };
        if angle <= next_angle {
            let span = (next_angle - previous_angle).max(1e-6);
            let t = ((angle - previous_angle) / span).clamp(0.0, 1.0);
            return previous_color.lerp(next_color, t);
        }
        previous_angle = next_angle;
        previous_color = next_color;
    }
    previous_color
}

/// [`loader!`] 要的异步签名：先同步解析，再异步把贴图读进来，最后同步建场景。
pub async fn load(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<VrmlScene, LoadError> {
    let roots = parse(&bytes)?;
    let base = crate::base_dir(&path);
    let mut images = HashMap::new();
    for url in texture_urls(&roots) {
        if let Some(texture) = read_image(&io, &base, &url).await {
            images.insert(url, texture);
        }
    }
    Ok(build(&roots, &images, &base))
}

/// 纯同步版本：不读任何外部贴图（`ImageTexture` 全部当成缺失）。测试与工具用。
pub fn parse_scene(bytes: &[u8]) -> Result<VrmlScene, LoadError> {
    Ok(build(&parse(bytes)?, &HashMap::new(), Path::new("")))
}

/// 按「原路径 → 只取文件名 → 文件名小写」依次找贴图，和 [`crate::load_texture`] 同一个规矩。
async fn read_image(io: &Arc<dyn ResourceIo>, base: &Path, url: &str) -> Option<Texture> {
    let cleaned = url.replace('\\', "/");
    let file = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
    let mut bytes = sibling(io, base, &cleaned).await;
    if bytes.is_none() {
        bytes = sibling(io, base, &file).await;
    }
    if bytes.is_none() {
        bytes = sibling(io, base, &file.to_lowercase()).await;
    }
    match Texture::from_encoded(&bytes?) {
        Ok(texture) => Some(texture),
        Err(error) => {
            klog::warn!("VRML 贴图 {url} 解码失败：{error}");
            None
        }
    }
}

// ───────────────────────────── 语法层 ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word,
    Str(String),
    Open,
    Close,
    ListOpen,
    ListClose,
}

fn tokenize(text: &str) -> Result<Vec<Token>, LoadError> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let is_break = |b: u8| {
        b.is_ascii_whitespace() || matches!(b, b',' | b'{' | b'}' | b'[' | b']' | b'"' | b'#')
    };
    while i < bytes.len() {
        if tokens.len() > limits::VERTICES * 4 {
            return Err(bad("VRML 文件过大"));
        }
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
            }
            b'{' => {
                tokens.push(Token::Open);
                i += 1;
            }
            b'}' => {
                tokens.push(Token::Close);
                i += 1;
            }
            b'[' => {
                tokens.push(Token::ListOpen);
                i += 1;
            }
            b']' => {
                tokens.push(Token::ListClose);
                i += 1;
            }
            b'"' => {
                // 字符串可以跨行（`Script` 的内联代码就是），`\"` 和 `\\` 是转义。
                i += 1;
                let mut value = Vec::new();
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if i + 1 < bytes.len() => {
                            value.push(bytes[i + 1]);
                            i += 2;
                        }
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b => {
                            value.push(b);
                            i += 1;
                        }
                    }
                }
                tokens.push(Token::Str(String::from_utf8_lossy(&value).into_owned()));
            }
            b if b.is_ascii_whitespace() || b == b',' => i += 1,
            _ => {
                let start = i;
                while i < bytes.len() && !is_break(bytes[i]) {
                    i += 1;
                }
                tokens.push(Token::Word(text[start..i].to_string()));
            }
        }
    }
    Ok(tokens)
}

/// 一个字段的值。类型是从值的样子推断的，见模块文档。
#[derive(Debug, Clone)]
enum Value {
    Numbers(Vec<f64>),
    Strings(Vec<String>),
    Bool(bool),
    Node(Arc<VNode>),
    Nodes(Vec<Arc<VNode>>),
    Word,
    Null,
}

/// 语法层的节点：类型名 + 按出现顺序的字段。
#[derive(Debug)]
struct VNode {
    kind: String,
    fields: Vec<(String, Value)>,
}

impl VNode {
    fn get(&self, name: &str) -> Option<&Value> {
        self.fields.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn numbers(&self, name: &str) -> &[f64] {
        match self.get(name) {
            Some(Value::Numbers(n)) => n,
            _ => &[],
        }
    }

    fn f32(&self, name: &str, default: f32) -> f32 {
        self.numbers(name).first().map_or(default, |&v| v as f32)
    }

    fn int(&self, name: &str, default: i64) -> i64 {
        self.numbers(name).first().map_or(default, |&v| v as i64)
    }

    fn vec2(&self, name: &str, default: Vec2) -> Vec2 {
        match self.numbers(name) {
            [x, y, ..] => Vec2::new(*x as f32, *y as f32),
            _ => default,
        }
    }

    fn vec3(&self, name: &str, default: Vec3) -> Vec3 {
        match self.numbers(name) {
            [x, y, z, ..] => Vec3::new(*x as f32, *y as f32, *z as f32),
            _ => default,
        }
    }

    /// 轴角旋转（`x y z 弧度`）。轴是零向量时当成不转。
    fn rotation(&self, name: &str) -> Quat {
        match self.numbers(name) {
            [x, y, z, angle, ..] => axis_angle(Vec3::new(*x as f32, *y as f32, *z as f32), *angle as f32),
            _ => Quat::IDENTITY,
        }
    }

    fn bool(&self, name: &str, default: bool) -> bool {
        match self.get(name) {
            Some(Value::Bool(b)) => *b,
            // `[ TRUE ]` 之类的单元素列表会被读成数字。
            Some(Value::Numbers(n)) if !n.is_empty() => n[0] != 0.0,
            _ => default,
        }
    }

    fn node(&self, name: &str) -> Option<&Arc<VNode>> {
        match self.get(name) {
            Some(Value::Node(node)) => Some(node),
            Some(Value::Nodes(nodes)) => nodes.first(),
            _ => None,
        }
    }

    fn nodes(&self, name: &str) -> &[Arc<VNode>] {
        match self.get(name) {
            Some(Value::Node(node)) => std::slice::from_ref(node),
            Some(Value::Nodes(nodes)) => nodes,
            _ => &[],
        }
    }

    fn strings(&self, name: &str) -> &[String] {
        match self.get(name) {
            Some(Value::Strings(s)) => s,
            _ => &[],
        }
    }

    fn indices(&self, name: &str) -> Vec<i64> {
        self.numbers(name).iter().map(|&v| v as i64).collect()
    }

    /// 某个子节点里的一串三元组（`coord Coordinate { point [...] }` 这种两层的写法）。
    fn vec3s(&self, child: &str, field: &str) -> Vec<Vec3> {
        self.node(child)
            .map(|node| {
                node.numbers(field)
                    .chunks_exact(3)
                    .map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn vec2s(&self, child: &str, field: &str) -> Vec<Vec2> {
        self.node(child)
            .map(|node| {
                node.numbers(field)
                    .chunks_exact(2)
                    .map(|c| Vec2::new(c[0] as f32, c[1] as f32))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn axis_angle(axis: Vec3, angle: f32) -> Quat {
    let axis = axis.normalize_or_zero();
    if axis == Vec3::ZERO || !angle.is_finite() {
        Quat::IDENTITY
    } else {
        Quat::from_axis_angle(axis, angle)
    }
}

fn number(word: &str) -> Option<f64> {
    if let Some(hex) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    let first = *word.as_bytes().first()?;
    if first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.') {
        word.parse::<f64>().ok().filter(|v| v.is_finite())
    } else {
        None
    }
}

/// 解析的递归深度上限。正常文件十几层，病态文件能把栈打爆。
const MAX_DEPTH: usize = 256;

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    defs: HashMap<String, Arc<VNode>>,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.at + offset)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn word(&mut self) -> Option<String> {
        match self.next()? {
            Token::Word(w) => Some(w),
            _ => None,
        }
    }

    /// 从当前位置跳过一个 `{...}` 或 `[...]` 块（当前 token 必须是开括号）。
    fn skip_block(&mut self) {
        let mut depth = 0usize;
        while let Some(token) = self.next() {
            match token {
                Token::Open | Token::ListOpen => depth += 1,
                Token::Close | Token::ListClose => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// `ROUTE a.b TO c.d`
    fn skip_route(&mut self) {
        self.at += 4;
    }

    /// `PROTO name [ 接口 ] { 正文 }` / `EXTERNPROTO name [ 接口 ] "url"`
    fn skip_proto(&mut self) {
        let external = matches!(self.next(), Some(Token::Word(w)) if w == "EXTERNPROTO");
        self.next(); // 名字
        if self.peek() == Some(&Token::ListOpen) {
            self.skip_block();
        }
        if external {
            match self.peek() {
                Some(Token::ListOpen) => self.skip_block(),
                Some(Token::Str(_)) => self.at += 1,
                _ => {}
            }
        } else if self.peek() == Some(&Token::Open) {
            self.skip_block();
        }
    }

    /// 顶层：一串节点，夹着 `ROUTE` / `PROTO`。
    fn scene(&mut self) -> Result<Vec<Arc<VNode>>, LoadError> {
        let mut roots = Vec::new();
        while let Some(token) = self.peek() {
            match token {
                Token::Word(w) if w == "ROUTE" => self.skip_route(),
                Token::Word(w) if w == "PROTO" || w == "EXTERNPROTO" => self.skip_proto(),
                Token::Word(_) => {
                    if let Some(node) = self.node()? {
                        roots.push(node);
                    }
                }
                Token::Open | Token::ListOpen => self.skip_block(),
                _ => self.at += 1,
            }
        }
        Ok(roots)
    }

    /// 一个节点（含 `DEF` / `USE`）。认不出来时返回 `None` 并至少前进一个 token。
    fn node(&mut self) -> Result<Option<Arc<VNode>>, LoadError> {
        let Some(word) = self.word() else {
            return Ok(None);
        };
        match word.as_str() {
            "USE" => {
                let name = self.word().unwrap_or_default();
                // 引用了没定义的名字：规范说是错误，浏览器的做法是忽略。
                Ok(self.defs.get(&name).cloned())
            }
            "DEF" => {
                let name = self.word().unwrap_or_default();
                let node = self.node()?;
                if let Some(node) = &node {
                    self.defs.insert(name, node.clone());
                }
                Ok(node)
            }
            _ => {
                if self.peek() != Some(&Token::Open) {
                    return Ok(None);
                }
                self.at += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(bad("VRML 节点嵌套过深"));
                }
                let fields = self.body()?;
                self.depth -= 1;
                Ok(Some(Arc::new(VNode { kind: word, fields })))
            }
        }
    }

    /// `{` 之后到 `}` 为止的字段。
    fn body(&mut self) -> Result<Vec<(String, Value)>, LoadError> {
        let mut fields = Vec::new();
        loop {
            match self.next() {
                None => return Err(bad("VRML 节点没有闭合")),
                Some(Token::Close) => return Ok(fields),
                Some(Token::Word(name)) => match name.as_str() {
                    "ROUTE" => self.at += 3,
                    "PROTO" | "EXTERNPROTO" => {
                        self.at -= 1;
                        self.skip_proto();
                    }
                    // 脚本 / PROTO 的接口声明：`eventIn SFTime touched` 没有值，
                    // `field SFFloat speed 1.0` 有。
                    "eventIn" | "eventOut" | "inputOnly" | "outputOnly" => self.at += 2,
                    "field" | "exposedField" | "initializeOnly" | "inputOutput" => {
                        self.next();
                        let field = self.word().unwrap_or_default();
                        let value = self.value()?;
                        fields.push((field, value));
                    }
                    _ => {
                        let value = self.value()?;
                        fields.push((name, value));
                    }
                },
                Some(Token::Open) | Some(Token::ListOpen) => {
                    self.at -= 1;
                    self.skip_block();
                }
                Some(_) => {}
            }
        }
    }

    fn value(&mut self) -> Result<Value, LoadError> {
        match self.peek().cloned() {
            Some(Token::ListOpen) => {
                self.at += 1;
                self.list()
            }
            Some(Token::Str(_)) => {
                let mut strings = Vec::new();
                while let Some(Token::Str(s)) = self.peek().cloned() {
                    strings.push(s);
                    self.at += 1;
                }
                Ok(Value::Strings(strings))
            }
            Some(Token::Word(word)) => {
                if word == "TRUE" || word == "FALSE" {
                    self.at += 1;
                    return Ok(Value::Bool(word == "TRUE"));
                }
                if word == "NULL" {
                    self.at += 1;
                    return Ok(Value::Null);
                }
                if number(&word).is_some() {
                    let mut numbers = Vec::new();
                    while let Some(Token::Word(w)) = self.peek() {
                        let Some(v) = number(w) else {
                            break;
                        };
                        numbers.push(v);
                        self.at += 1;
                    }
                    return Ok(Value::Numbers(numbers));
                }
                if word == "DEF" || word == "USE" || self.peek_at(1) == Some(&Token::Open) {
                    return Ok(self.node()?.map_or(Value::Null, Value::Node));
                }
                self.at += 1;
                Ok(Value::Word)
            }
            Some(Token::Open) => {
                self.skip_block();
                Ok(Value::Null)
            }
            _ => Ok(Value::Null),
        }
    }

    /// `[` 之后到 `]` 为止。元素类型看第一个元素。
    fn list(&mut self) -> Result<Value, LoadError> {
        let mut numbers = Vec::new();
        let mut strings = Vec::new();
        let mut nodes = Vec::new();
        loop {
            match self.peek().cloned() {
                None => return Err(bad("VRML 列表没有闭合")),
                Some(Token::ListClose) => {
                    self.at += 1;
                    break;
                }
                Some(Token::Str(s)) => {
                    strings.push(s);
                    self.at += 1;
                }
                Some(Token::Word(w)) => {
                    if let Some(v) = number(&w) {
                        numbers.push(v);
                        self.at += 1;
                    } else if w == "TRUE" || w == "FALSE" {
                        numbers.push(if w == "TRUE" { 1.0 } else { 0.0 });
                        self.at += 1;
                    } else if w == "ROUTE" {
                        self.skip_route();
                    } else if w == "PROTO" || w == "EXTERNPROTO" {
                        self.skip_proto();
                    } else if let Some(node) = self.node()? {
                        nodes.push(node);
                    }
                }
                Some(Token::Open) | Some(Token::ListOpen) => self.skip_block(),
                Some(Token::Close) => return Err(bad("VRML 列表里出现了多余的 }")),
            }
        }
        Ok(if !nodes.is_empty() {
            Value::Nodes(nodes)
        } else if !strings.is_empty() {
            Value::Strings(strings)
        } else {
            Value::Numbers(numbers)
        })
    }
}

/// 语法层入口：读成节点树的根列表。
fn parse(bytes: &[u8]) -> Result<Vec<Arc<VNode>>, LoadError> {
    let text = String::from_utf8_lossy(bytes);
    let header = text.lines().next().unwrap_or("");
    if header.starts_with("#VRML V1.0") {
        return Err(bad("只支持 VRML 2.0（VRML 97），这是 VRML 1.0 文件"));
    }
    if !header.starts_with("#VRML V2.0") {
        return Err(bad("不是 VRML 2.0 文件（缺少 `#VRML V2.0 utf8` 文件头）"));
    }
    let mut parser = Parser {
        tokens: tokenize(&text)?,
        at: 0,
        defs: HashMap::new(),
        depth: 0,
    };
    parser.scene()
}

/// 整棵树里所有 `ImageTexture` 引用的文件名（去重，跳过空串）。
fn texture_urls(roots: &[Arc<VNode>]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    let mut stack: Vec<&Arc<VNode>> = roots.iter().collect();
    while let Some(node) = stack.pop() {
        if !seen.insert(Arc::as_ptr(node)) {
            continue;
        }
        if node.kind == "ImageTexture"
            && let Some(url) = node.strings("url").iter().find(|u| !u.trim().is_empty())
            && !urls.contains(url)
        {
            urls.push(url.clone());
        }
        for (_, value) in &node.fields {
            match value {
                Value::Node(child) => stack.push(child),
                Value::Nodes(children) => stack.extend(children),
                _ => {}
            }
        }
    }
    urls
}

// ───────────────────────────── 建场景 ─────────────────────────────

fn srgb_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear(color: Vec3) -> Vec3 {
    Vec3::new(srgb_to_linear(color.x), srgb_to_linear(color.y), srgb_to_linear(color.z))
}

fn gizmo_color(color: Vec3, alpha: f32) -> Color {
    Color {
        r: color.x,
        g: color.y,
        b: color.z,
        a: alpha,
    }
}

struct Builder<'a> {
    images: &'a HashMap<String, Texture>,
    base: &'a Path,
    meshes: Vec<Mesh>,
    materials: Vec<Material>,
    nodes: Vec<ModelNode>,
    mesh_cache: HashMap<(usize, usize), Option<usize>>,
    material_cache: HashMap<(usize, bool), usize>,
    lines: LineSetBuilder,
    points: PointCloud,
    background: Option<Background>,
    min: Vec3,
    max: Vec3,
}

fn build(roots: &[Arc<VNode>], images: &HashMap<String, Texture>, base: &Path) -> VrmlScene {
    let mut builder = Builder {
        images,
        base,
        meshes: Vec::new(),
        materials: Vec::new(),
        nodes: vec![ModelNode {
            name: "VRML".to_string(),
            transform: NodeTransform::default(),
            children: Vec::new(),
            parts: Vec::new(),
            skin: None,
        }],
        mesh_cache: HashMap::new(),
        material_cache: HashMap::new(),
        lines: LineSetBuilder::default(),
        points: PointCloud::default(),
        background: None,
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };
    for root in roots {
        builder.visit(root, Mat4::IDENTITY, 0, 0);
    }

    let Builder {
        meshes,
        materials,
        nodes,
        lines,
        mut points,
        background,
        min,
        max,
        ..
    } = builder;
    points.has_color = !points.is_empty();
    VrmlScene {
        model: Model::new(meshes, materials, nodes, vec![0]),
        lines: (lines.segment_count() > 0).then(|| lines.build()),
        points: (!points.is_empty()).then_some(points),
        background,
        bounds: if min.x <= max.x { (min, max) } else { (Vec3::ZERO, Vec3::ZERO) },
    }
}

impl Builder<'_> {
    fn grow(&mut self, point: Vec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    fn add_node(&mut self, name: &str, transform: NodeTransform, parent: usize) -> Option<usize> {
        if self.nodes.len() >= limits::NODES {
            return None;
        }
        let index = self.nodes.len();
        self.nodes.push(ModelNode {
            name: name.to_string(),
            transform,
            children: Vec::new(),
            parts: Vec::new(),
            skin: None,
        });
        self.nodes[parent].children.push(index);
        Some(index)
    }

    fn visit(&mut self, node: &Arc<VNode>, world: Mat4, parent: usize, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match node.kind.as_str() {
            "Transform" => {
                let local = transform_matrix(node);
                let (scale, rotation, translation) = local.to_scale_rotation_translation();
                let transform = NodeTransform {
                    position: translation,
                    rotation,
                    scale,
                };
                let Some(index) = self.add_node("Transform", transform, parent) else {
                    return;
                };
                for child in node.nodes("children") {
                    self.visit(child, world * local, index, depth + 1);
                }
            }
            "Group" | "Anchor" | "Collision" | "Billboard" => {
                for child in node.nodes("children") {
                    self.visit(child, world, parent, depth + 1);
                }
            }
            "Switch" => {
                let choice = node.int("whichChoice", -1);
                let choices = node.nodes("choice");
                if let Ok(choice) = usize::try_from(choice)
                    && let Some(child) = choices.get(choice)
                {
                    self.visit(child, world, parent, depth + 1);
                }
            }
            "LOD" => {
                // VRML 97 叫 `level`，X3D 改名叫 `children`。只取最精细的一级。
                let levels = node.nodes("level");
                let levels = if levels.is_empty() { node.nodes("children") } else { levels };
                if let Some(first) = levels.first() {
                    self.visit(first, world, parent, depth + 1);
                }
            }
            "Shape" => self.shape(node, world, parent),
            "Background" if self.background.is_none() => {
                let colors = |name: &str| -> Vec<Vec3> {
                    node.numbers(name)
                        .chunks_exact(3)
                        .map(|c| linear(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)))
                        .collect()
                };
                let angles = |name: &str| node.numbers(name).iter().map(|&a| a as f32).collect();
                self.background = Some(Background {
                    sky_colors: colors("skyColor"),
                    sky_angles: angles("skyAngle"),
                    ground_colors: colors("groundColor"),
                    ground_angles: angles("groundAngle"),
                });
            }
            _ => {}
        }
    }

    fn shape(&mut self, shape: &Arc<VNode>, world: Mat4, parent: usize) {
        let Some(geometry) = shape.node("geometry") else {
            return;
        };
        let appearance = shape.node("appearance");
        match geometry.kind.as_str() {
            "IndexedLineSet" => return self.line_set(geometry, appearance, world),
            "PointSet" => return self.point_set(geometry, appearance, world),
            _ => {}
        }
        let texture_transform = appearance.and_then(|a| a.node("textureTransform"));
        let key = (
            Arc::as_ptr(geometry) as usize,
            texture_transform.map_or(0, |t| Arc::as_ptr(t) as usize),
        );
        let mesh = match self.mesh_cache.get(&key) {
            Some(&cached) => cached,
            None => {
                let built = build_geometry(geometry, texture_transform.map(|t| &**t)).and_then(|mesh| {
                    if mesh.triangle_count() == 0 {
                        return None;
                    }
                    self.meshes.push(mesh);
                    Some(self.meshes.len() - 1)
                });
                self.mesh_cache.insert(key, built);
                built
            }
        };
        let Some(mesh) = mesh else {
            return;
        };
        let aabb = self.meshes[mesh].aabb();
        for corner in aabb.corners() {
            self.grow(world.transform_point3(corner));
        }
        // 基本体永远是实心的；其余几何的 `solid FALSE` 意味着两面都要画。
        let double_sided = !geometry.bool("solid", true);
        let material = self.material(appearance, double_sided);
        self.nodes[parent].parts.push(MeshPart {
            mesh,
            material: Some(material),
        });
    }

    fn material(&mut self, appearance: Option<&Arc<VNode>>, double_sided: bool) -> usize {
        let key = (appearance.map_or(0, |a| Arc::as_ptr(a) as usize), double_sided);
        if let Some(&index) = self.material_cache.get(&key) {
            return index;
        }
        let mut material = self.make_material(appearance.map(|a| &**a));
        if double_sided {
            material.set_double_sided(true);
        }
        self.materials.push(material);
        let index = self.materials.len() - 1;
        self.material_cache.insert(key, index);
        index
    }

    fn make_material(&self, appearance: Option<&VNode>) -> Material {
        let Some(appearance) = appearance else {
            // 规范：没有 Appearance 的形体不受光，白色。
            return UnlitMaterial::new(Vec4::ONE).with_name("vrml-unlit");
        };
        let texture = appearance.node("texture").and_then(|t| self.texture(t));
        let mut material = match appearance.node("material") {
            None => UnlitMaterial::new(Vec4::ONE).with_name("vrml-unlit"),
            Some(node) => {
                let diffuse = linear(node.vec3("diffuseColor", Vec3::splat(0.8)));
                let emissive = linear(node.vec3("emissiveColor", Vec3::ZERO));
                // `shininess` 是 0..1，乘 128 才是 Phong 指数。换算规则和 OBJ 的 `Ns` 相同。
                let exponent = node.f32("shininess", 0.2).clamp(0.0, 1.0) * 128.0;
                let roughness = (2.0 / (exponent + 2.0)).sqrt().clamp(0.05, 1.0);
                let alpha = 1.0 - node.f32("transparency", 0.0).clamp(0.0, 1.0);
                let mut material = Material::standard()
                    .with_name("vrml")
                    .with_base_color(diffuse.extend(alpha))
                    .with_metallic(0.0)
                    .with_roughness(roughness);
                if emissive != Vec3::ZERO {
                    material.set(kpbr::standard::EMISSIVE, emissive);
                }
                material
            }
        };
        if let Some((texture, components)) = texture {
            let color = material.base_color();
            // 规范：RGB(A) 贴图**替换**漫反射色，灰度贴图**调制**漫反射色；
            // 带 alpha 的贴图替换透明度。
            let rgb = if components >= 3 { Vec3::ONE } else { color.truncate() };
            let alpha = if components == 2 || components == 4 { 1.0 } else { color.w };
            material.set_base_color(rgb.extend(alpha));
            material.set("base_color_texture", texture);
            if components == 2 || components == 4 {
                material.set_blend_mode(BlendMode::Alpha);
            }
        }
        if material.base_color().w < 1.0 {
            material.set_blend_mode(BlendMode::Alpha);
        }
        material
    }

    /// 贴图节点 → （贴图资源, 分量数）。分量数决定它是替换还是调制漫反射色。
    fn texture(&self, node: &VNode) -> Option<(Resource<Texture>, u32)> {
        let wrap = |name: &str| {
            if node.bool(name, true) { WrapMode::Repeat } else { WrapMode::ClampToEdge }
        };
        let (key, texture, components, filter) = match node.kind.as_str() {
            "ImageTexture" => {
                let url = node.strings("url").iter().find(|u| !u.trim().is_empty())?;
                let texture = self.images.get(url)?.clone();
                let has_alpha = texture.data().chunks_exact(4).any(|p| p[3] < 255);
                let key = self.base.join(url).to_string_lossy().into_owned();
                (key, texture, if has_alpha { 4 } else { 3 }, FilterMode::Linear)
            }
            "PixelTexture" => {
                let (texture, components) = pixel_texture(node.numbers("image"))?;
                // 和 three.js 的 `DataTexture` 一样默认最近邻：PixelTexture
                // 往往只有几个像素，线性过滤会把它糊成一团渐变。
                (format!("vrml-pixel-texture-{:p}", node), texture, components, FilterMode::Nearest)
            }
            _ => return None,
        };
        let sampler = Sampler {
            mag_filter: filter,
            min_filter: filter,
            wrap_u: wrap("repeatS"),
            wrap_v: wrap("repeatT"),
        };
        let texture = texture.with_format(TextureFormat::Srgb).with_sampler(sampler);
        Some((
            Resource::new_ok(format!("{key}#{:?}{:?}", sampler.wrap_u, sampler.wrap_v), texture),
            components,
        ))
    }

    fn line_set(&mut self, geometry: &VNode, appearance: Option<&Arc<VNode>>, world: Mat4) {
        let points = geometry.vec3s("coord", "point");
        let colors: Vec<Vec3> = geometry.vec3s("color", "color").into_iter().map(linear).collect();
        let (fallback, alpha) = line_color(appearance);
        let per_vertex = geometry.bool("colorPerVertex", true);
        let color_index = geometry.indices("colorIndex");
        let coord_index = geometry.indices("coordIndex");

        let mut polyline = 0usize;
        let mut previous: Option<(Vec3, Vec3)> = None;
        for (slot, &index) in coord_index.iter().enumerate() {
            if index < 0 {
                polyline += 1;
                previous = None;
                continue;
            }
            let Some(&point) = points.get(index as usize) else {
                previous = None;
                continue;
            };
            let color_slot = if per_vertex {
                if color_index.is_empty() { index } else { color_index.get(slot).copied().unwrap_or(-1) }
            } else if color_index.is_empty() {
                polyline as i64
            } else {
                color_index.get(polyline).copied().unwrap_or(-1)
            };
            let color = usize::try_from(color_slot)
                .ok()
                .and_then(|i| colors.get(i).copied())
                .unwrap_or(fallback);
            let point = world.transform_point3(point);
            self.grow(point);
            if let Some((from, from_color)) = previous {
                self.lines
                    .gradient(from, point, gizmo_color(from_color, alpha), gizmo_color(color, alpha));
            }
            previous = Some((point, color));
        }
    }

    fn point_set(&mut self, geometry: &VNode, appearance: Option<&Arc<VNode>>, world: Mat4) {
        let points = geometry.vec3s("coord", "point");
        let colors: Vec<Vec3> = geometry.vec3s("color", "color").into_iter().map(linear).collect();
        let (fallback, _) = line_color(appearance);
        for (index, &point) in points.iter().enumerate() {
            if self.points.positions.len() >= limits::VERTICES {
                break;
            }
            let point = world.transform_point3(point);
            self.grow(point);
            self.points.positions.push(point);
            self.points.colors.push(colors.get(index).copied().unwrap_or(fallback));
        }
    }
}

/// 线和点的颜色来自 `emissiveColor`（规范：它们不受光）。没有材质时是白色。
fn line_color(appearance: Option<&Arc<VNode>>) -> (Vec3, f32) {
    match appearance.and_then(|a| a.node("material")) {
        Some(material) => (
            linear(material.vec3("emissiveColor", Vec3::ZERO)),
            1.0 - material.f32("transparency", 0.0).clamp(0.0, 1.0),
        ),
        None => (Vec3::ONE, 1.0),
    }
}

/// `Transform` 的完整矩阵：`T · C · R · SR · S · -SR · -C`。
fn transform_matrix(node: &VNode) -> Mat4 {
    let translation = node.vec3("translation", Vec3::ZERO);
    let center = node.vec3("center", Vec3::ZERO);
    let rotation = node.rotation("rotation");
    let scale = node.vec3("scale", Vec3::ONE);
    let scale_orientation = node.rotation("scaleOrientation");
    Mat4::from_translation(translation + center)
        * Mat4::from_quat(rotation)
        * Mat4::from_quat(scale_orientation)
        * Mat4::from_scale(scale)
        * Mat4::from_quat(scale_orientation.inverse())
        * Mat4::from_translation(-center)
}

/// `PixelTexture` 的 `image` 字段：`宽 高 分量数 像素...`，像素从**左下角**开始逐行往上。
fn pixel_texture(numbers: &[f64]) -> Option<(Texture, u32)> {
    let [width, height, components, pixels @ ..] = numbers else {
        return None;
    };
    let (width, height, components) = (*width as usize, *height as usize, *components as u32);
    if width == 0 || height == 0 || !(1..=4).contains(&components) || width * height > 1 << 24 {
        return None;
    }
    let mut rgba = vec![0u8; width * height * 4];
    for row in 0..height {
        // 贴图内存是从上往下的，VRML 是从下往上的：翻一下。
        let target_row = height - 1 - row;
        for column in 0..width {
            let value = pixels.get(row * width + column).copied().unwrap_or(0.0) as u32;
            let channel = |shift: u32| ((value >> shift) & 0xFF) as u8;
            let pixel = match components {
                1 => [channel(0), channel(0), channel(0), 255],
                2 => [channel(8), channel(8), channel(8), channel(0)],
                3 => [channel(16), channel(8), channel(0), 255],
                _ => [channel(24), channel(16), channel(8), channel(0)],
            };
            let at = (target_row * width + column) * 4;
            rgba[at..at + 4].copy_from_slice(&pixel);
        }
    }
    Some((Texture::new(width as u32, height as u32, rgba), components))
}

// ───────────────────────────── 几何 ─────────────────────────────

fn build_geometry(node: &VNode, texture_transform: Option<&VNode>) -> Option<Mesh> {
    match node.kind.as_str() {
        "Box" => Some(scaled(Mesh::cube(), node.vec3("size", Vec3::splat(2.0)))),
        "Sphere" => Some(scaled(Mesh::sphere(24, 48), Vec3::splat(node.f32("radius", 1.0) * 2.0))),
        "Cylinder" => {
            let (radius, height) = (node.f32("radius", 1.0), node.f32("height", 2.0));
            Some(scaled(Mesh::cylinder(48), Vec3::new(radius * 2.0, height, radius * 2.0)))
        }
        "Cone" => {
            let (radius, height) = (node.f32("bottomRadius", 1.0), node.f32("height", 2.0));
            Some(scaled(Mesh::cone(48), Vec3::new(radius * 2.0, height, radius * 2.0)))
        }
        "IndexedFaceSet" => FaceSet::indexed(node).map(|f| f.build(texture_transform)),
        "ElevationGrid" => FaceSet::elevation(node).map(|f| f.build(texture_transform)),
        "Extrusion" => FaceSet::extrusion(node).map(|f| f.build(texture_transform)),
        _ => None,
    }
}

/// 把单位基本体按轴缩放。法线按逆缩放变换再归一化，非均匀缩放下才是对的。
fn scaled(mesh: Mesh, scale: Vec3) -> Mesh {
    let inverse = Vec3::new(
        if scale.x != 0.0 { 1.0 / scale.x } else { 0.0 },
        if scale.y != 0.0 { 1.0 / scale.y } else { 0.0 },
        if scale.z != 0.0 { 1.0 / scale.z } else { 0.0 },
    );
    let vertices = mesh
        .vertices()
        .iter()
        .map(|v| Vertex {
            position: (v.position() * scale).to_array(),
            normal: (v.normal() * inverse).normalize_or_zero().to_array(),
            ..*v
        })
        .collect();
    let mut mesh = Mesh::new(vertices, mesh.indices().to_vec());
    mesh.recompute_tangents();
    mesh
}

/// 多边形的一个角。
#[derive(Debug, Clone, Copy)]
struct Corner {
    position: usize,
    normal: Option<Vec3>,
    /// VRML 的 `(s, t)`，`t` 朝上。
    uv: Option<Vec2>,
    color: Option<Vec3>,
}

/// 三种面几何（`IndexedFaceSet` / `ElevationGrid` / `Extrusion`）的共同中间形态。
/// 后两种先展开成这个，再和第一种走同一条三角化、法线、UV 的路。
struct FaceSet {
    positions: Vec<Vec3>,
    faces: Vec<Vec<Corner>>,
    ccw: bool,
    convex: bool,
    crease_angle: f32,
}

/// 按索引取颜色 / 法线 / UV；`index` 越界或为负时 `None`。
fn pick<T: Copy>(values: &[T], index: i64) -> Option<T> {
    usize::try_from(index).ok().and_then(|i| values.get(i).copied())
}

impl FaceSet {
    fn indexed(node: &VNode) -> Option<Self> {
        let positions = node.vec3s("coord", "point");
        if positions.is_empty() {
            return None;
        }
        let colors: Vec<Vec3> = node.vec3s("color", "color").into_iter().map(linear).collect();
        let normals = node.vec3s("normal", "vector");
        let uvs = node.vec2s("texCoord", "point");
        let coord_index = node.indices("coordIndex");
        let color_index = node.indices("colorIndex");
        let normal_index = node.indices("normalIndex");
        let uv_index = node.indices("texCoordIndex");
        let color_per_vertex = node.bool("colorPerVertex", true);
        let normal_per_vertex = node.bool("normalPerVertex", true);

        // 「逐顶点」的属性：有自己的索引就按槽位对齐，没有就沿用 coordIndex。
        let per_vertex = |values_index: &[i64], slot: usize, coord: i64| -> i64 {
            if values_index.is_empty() { coord } else { values_index.get(slot).copied().unwrap_or(-1) }
        };
        // 「逐面」的属性：有索引就按面号取索引，没有就直接用面号。
        let per_face = |values_index: &[i64], face: usize| -> i64 {
            if values_index.is_empty() { face as i64 } else { values_index.get(face).copied().unwrap_or(-1) }
        };

        let mut faces = Vec::new();
        let mut current = Vec::new();
        for (slot, &coord) in coord_index.iter().chain(std::iter::once(&-1)).enumerate() {
            if coord < 0 {
                if current.len() >= 3 {
                    let face = faces.len();
                    let face_color = (!color_per_vertex).then(|| pick(&colors, per_face(&color_index, face))).flatten();
                    let face_normal =
                        (!normal_per_vertex).then(|| pick(&normals, per_face(&normal_index, face))).flatten();
                    for corner in current.iter_mut() {
                        let corner: &mut Corner = corner;
                        if !color_per_vertex {
                            corner.color = face_color;
                        }
                        if !normal_per_vertex {
                            corner.normal = face_normal;
                        }
                    }
                    faces.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                continue;
            }
            if coord as usize >= positions.len() {
                continue;
            }
            current.push(Corner {
                position: coord as usize,
                normal: if normal_per_vertex { pick(&normals, per_vertex(&normal_index, slot, coord)) } else { None },
                uv: pick(&uvs, per_vertex(&uv_index, slot, coord)),
                color: if color_per_vertex { pick(&colors, per_vertex(&color_index, slot, coord)) } else { None },
            });
        }
        if faces.is_empty() {
            return None;
        }
        let mut set = Self {
            positions,
            faces,
            ccw: node.bool("ccw", true),
            convex: node.bool("convex", true),
            crease_angle: node.f32("creaseAngle", 0.0),
        };
        if uvs.is_empty() {
            set.default_uvs();
        }
        Some(set)
    }

    /// 规范给 `IndexedFaceSet` 的缺省纹理坐标：包围盒最长的轴是 s、次长的是 t，
    /// 两个方向用**同一个**尺度（最长边），贴图不会被拉伸。
    fn default_uvs(&mut self) {
        let (min, max) = self
            .positions
            .iter()
            .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), &p| (a.min(p), b.max(p)));
        let size = max - min;
        let mut axes = [0usize, 1, 2];
        axes.sort_by(|&a, &b| size[b].total_cmp(&size[a]));
        let (s_axis, t_axis) = (axes[0], axes[1]);
        let extent = size[s_axis].max(1e-6);
        for face in &mut self.faces {
            for corner in face {
                let p = self.positions[corner.position] - min;
                corner.uv = Some(Vec2::new(p[s_axis] / extent, p[t_axis] / extent));
            }
        }
    }

    fn elevation(node: &VNode) -> Option<Self> {
        let columns = node.int("xDimension", 0).max(0) as usize;
        let rows = node.int("zDimension", 0).max(0) as usize;
        if columns < 2 || rows < 2 || columns * rows > limits::VERTICES {
            return None;
        }
        let (dx, dz) = (node.f32("xSpacing", 1.0), node.f32("zSpacing", 1.0));
        let heights = node.numbers("height");
        let colors: Vec<Vec3> = node.vec3s("color", "color").into_iter().map(linear).collect();
        let normals = node.vec3s("normal", "vector");
        let uvs = node.vec2s("texCoord", "point");
        let color_per_vertex = node.bool("colorPerVertex", true);
        let normal_per_vertex = node.bool("normalPerVertex", true);

        let mut positions = Vec::with_capacity(columns * rows);
        for row in 0..rows {
            for column in 0..columns {
                let height = heights.get(row * columns + column).copied().unwrap_or(0.0) as f32;
                positions.push(Vec3::new(column as f32 * dx, height, row as f32 * dz));
            }
        }
        let mut faces = Vec::with_capacity((columns - 1) * (rows - 1));
        for row in 0..rows - 1 {
            for column in 0..columns - 1 {
                let quad = row * (columns - 1) + column;
                // 从 +Y 往下看是逆时针：法线朝上。
                let corners = [(column, row), (column, row + 1), (column + 1, row + 1), (column + 1, row)];
                faces.push(
                    corners
                        .iter()
                        .map(|&(c, r)| {
                            let vertex = r * columns + c;
                            Corner {
                                position: vertex,
                                normal: if normal_per_vertex { normals.get(vertex) } else { normals.get(quad) }.copied(),
                                uv: Some(uvs.get(vertex).copied().unwrap_or(Vec2::new(
                                    c as f32 / (columns - 1) as f32,
                                    r as f32 / (rows - 1) as f32,
                                ))),
                                color: if color_per_vertex { colors.get(vertex) } else { colors.get(quad) }.copied(),
                            }
                        })
                        .collect(),
                );
            }
        }
        Some(Self {
            positions,
            faces,
            ccw: node.bool("ccw", true),
            convex: true,
            crease_angle: node.f32("creaseAngle", 0.0),
        })
    }

    fn extrusion(node: &VNode) -> Option<Self> {
        let mut section: Vec<Vec2> =
            node.numbers("crossSection").chunks_exact(2).map(|c| Vec2::new(c[0] as f32, c[1] as f32)).collect();
        if !node.fields.iter().any(|(n, _)| n == "crossSection") {
            section = vec![
                Vec2::new(1.0, 1.0),
                Vec2::new(1.0, -1.0),
                Vec2::new(-1.0, -1.0),
                Vec2::new(-1.0, 1.0),
                Vec2::new(1.0, 1.0),
            ];
        }
        let mut spine: Vec<Vec3> =
            node.numbers("spine").chunks_exact(3).map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)).collect();
        if !node.fields.iter().any(|(n, _)| n == "spine") {
            spine = vec![Vec3::ZERO, Vec3::Y];
        }
        let scales: Vec<Vec2> = node.numbers("scale").chunks_exact(2).map(|c| Vec2::new(c[0] as f32, c[1] as f32)).collect();
        let orientations: Vec<Quat> = node
            .numbers("orientation")
            .chunks_exact(4)
            .map(|c| axis_angle(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32), c[3] as f32))
            .collect();
        let (n, m) = (spine.len(), section.len());
        if n < 2 || m < 2 || n * m > limits::VERTICES {
            return None;
        }

        let frames = spine_frames(&spine);
        let mut positions = Vec::with_capacity(n * m);
        for (i, &frame) in frames.iter().enumerate() {
            let scale = scales.get(i).or(scales.last()).copied().unwrap_or(Vec2::ONE);
            let orientation = orientations.get(i).or(orientations.last()).copied().unwrap_or(Quat::IDENTITY);
            for point in &section {
                let local = orientation * Vec3::new(point.x * scale.x, 0.0, point.y * scale.y);
                positions.push(spine[i] + frame * local);
            }
        }

        // 侧面的缺省 UV：s 沿截面周长、t 沿脊线长度，各自归一化到 0..1。
        let cumulative = |lengths: Vec<f32>| {
            let total = lengths.last().copied().unwrap_or(0.0).max(1e-6);
            lengths.into_iter().map(|l| l / total).collect::<Vec<f32>>()
        };
        let s: Vec<f32> = cumulative(
            std::iter::once(0.0)
                .chain(section.windows(2).scan(0.0, |acc, w| {
                    *acc += w[0].distance(w[1]);
                    Some(*acc)
                }))
                .collect(),
        );
        let t: Vec<f32> = cumulative(
            std::iter::once(0.0)
                .chain(spine.windows(2).scan(0.0, |acc, w| {
                    *acc += w[0].distance(w[1]);
                    Some(*acc)
                }))
                .collect(),
        );

        let corner = |i: usize, k: usize, uv: Vec2| Corner {
            position: i * m + k,
            normal: None,
            uv: Some(uv),
            color: None,
        };
        let mut faces = Vec::with_capacity((n - 1) * (m - 1) + 2);
        for i in 0..n - 1 {
            for k in 0..m - 1 {
                faces.push(vec![
                    corner(i, k, Vec2::new(s[k], t[i])),
                    corner(i, k + 1, Vec2::new(s[k + 1], t[i])),
                    corner(i + 1, k + 1, Vec2::new(s[k + 1], t[i + 1])),
                    corner(i + 1, k, Vec2::new(s[k], t[i + 1])),
                ]);
            }
        }

        // 端盖：截面闭合时最后一个点和第一个重复，去掉。UV 按截面的包围盒铺。
        let closed_section = section.first() == section.last();
        let cap_points = if closed_section { m - 1 } else { m };
        if cap_points >= 3 {
            let (min, max) = section.iter().fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(a, b), &p| {
                (a.min(p), b.max(p))
            });
            let extent = (max - min).max_element().max(1e-6);
            let cap_uv = |k: usize| (section[k] - min) / extent;
            if node.bool("beginCap", true) {
                faces.push((0..cap_points).rev().map(|k| corner(0, k, cap_uv(k))).collect());
            }
            if node.bool("endCap", true) {
                faces.push((0..cap_points).map(|k| corner(n - 1, k, cap_uv(k))).collect());
            }
        }

        Some(Self {
            positions,
            faces,
            ccw: node.bool("ccw", true),
            convex: node.bool("convex", true),
            crease_angle: node.f32("creaseAngle", 0.0),
        })
    }

    /// 三角化、补法线、换算 UV，得到网格。每个角一个顶点。
    fn build(mut self, texture_transform: Option<&VNode>) -> Mesh {
        if !self.ccw {
            for face in &mut self.faces {
                face.reverse();
            }
        }
        // Newell 法：对非平面、非凸的多边形也稳定。不归一化——长度是面积的两倍，
        // 平滑时按面积加权。
        let raw: Vec<Vec3> = self
            .faces
            .iter()
            .map(|face| {
                let mut normal = Vec3::ZERO;
                for (index, corner) in face.iter().enumerate() {
                    let a = self.positions[corner.position];
                    let b = self.positions[face[(index + 1) % face.len()].position];
                    normal += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
                }
                normal
            })
            .collect();
        let unit: Vec<Vec3> = raw.iter().map(|n| n.normalize_or_zero()).collect();

        // 平滑按**位置的值**找相邻面而不是按下标：挤出体的截面首尾、
        // 手写的 IndexedFaceSet 里常有重复的坐标，按下标会在那里留一道硬缝。
        let key = |p: Vec3| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        let mut incident: HashMap<[u32; 3], Vec<usize>> = HashMap::new();
        if self.crease_angle > 0.0 {
            for (face_index, face) in self.faces.iter().enumerate() {
                for corner in face {
                    incident.entry(key(self.positions[corner.position])).or_default().push(face_index);
                }
            }
        }
        let cos_crease = self.crease_angle.min(std::f32::consts::PI).cos() - 1e-5;

        let uv_transform = texture_transform.map(TextureTransform::from_node);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (face_index, face) in self.faces.iter().enumerate() {
            let base = vertices.len() as u32;
            let face_normal = unit[face_index];
            for corner in face {
                let position = self.positions[corner.position];
                let normal = corner.normal.map(Vec3::normalize_or_zero).filter(|n| *n != Vec3::ZERO).unwrap_or_else(|| {
                    if self.crease_angle <= 0.0 {
                        return face_normal;
                    }
                    let mut sum = Vec3::ZERO;
                    for &other in incident.get(&key(position)).map_or(&[][..], Vec::as_slice) {
                        if unit[other].dot(face_normal) >= cos_crease {
                            sum += raw[other];
                        }
                    }
                    let smooth = sum.normalize_or_zero();
                    if smooth == Vec3::ZERO { face_normal } else { smooth }
                });
                let mut uv = corner.uv.unwrap_or(Vec2::ZERO);
                if let Some(transform) = &uv_transform {
                    uv = transform.apply(uv);
                }
                // VRML 的 t 朝上，引擎（glTF 口径）的 v 朝下。
                let mut vertex = Vertex::new(position, normal, [uv.x, 1.0 - uv.y]);
                if let Some(color) = corner.color {
                    vertex = vertex.with_color(color);
                }
                vertices.push(vertex);
            }
            let local = if self.convex || face.len() == 3 {
                (1..face.len() as u32 - 1).flat_map(|i| [0, i, i + 1]).collect()
            } else {
                triangulate_face(&face.iter().map(|c| self.positions[c.position]).collect::<Vec<_>>(), face_normal)
            };
            indices.extend(local.into_iter().map(|i| base + i));
        }
        let mut mesh = Mesh::new(vertices, indices);
        mesh.recompute_tangents();
        mesh
    }
}

/// 非凸多边形：投影到法线最大分量以外的两个轴上，再耳切。
fn triangulate_face(points: &[Vec3], normal: Vec3) -> Vec<u32> {
    let a = normal.abs();
    let project: fn(Vec3) -> Vec2 = if a.x >= a.y && a.x >= a.z {
        |p| Vec2::new(p.y, p.z)
    } else if a.y >= a.z {
        |p| Vec2::new(p.z, p.x)
    } else {
        |p| Vec2::new(p.x, p.y)
    };
    triangulate(&points.iter().map(|&p| project(p)).collect::<Vec<_>>())
}

/// 脊线上每一点的截面坐标系（SCP），列向量分别是 X、Y、Z。按规范第 6.18 节。
fn spine_frames(spine: &[Vec3]) -> Vec<Mat3> {
    let n = spine.len();
    let closed = spine[0].distance(spine[n - 1]) < 1e-6;
    let collinear = (1..n - 1).all(|i| (spine[i + 1] - spine[i]).cross(spine[i - 1] - spine[i]).length() < 1e-6);

    if collinear {
        // 整条脊线共线：把 +Y 转到脊线方向，XZ 平面跟着转。
        let direction = spine.windows(2).map(|w| w[1] - w[0]).find(|d| d.length() > 1e-6).unwrap_or(Vec3::Y);
        let rotation = Quat::from_rotation_arc(Vec3::Y, direction.normalize());
        return vec![Mat3::from_quat(rotation); n];
    }

    let y_axis = |i: usize| -> Vec3 {
        let d = if closed && (i == 0 || i == n - 1) {
            spine[1] - spine[n - 2]
        } else if i == 0 {
            spine[1] - spine[0]
        } else if i == n - 1 {
            spine[n - 1] - spine[n - 2]
        } else {
            spine[i + 1] - spine[i - 1]
        };
        d.normalize_or_zero()
    };
    let z_raw = |i: usize| -> Vec3 {
        if closed && (i == 0 || i == n - 1) {
            (spine[1] - spine[0]).cross(spine[n - 2] - spine[0])
        } else if i == 0 || i == n - 1 {
            Vec3::ZERO
        } else {
            (spine[i + 1] - spine[i]).cross(spine[i - 1] - spine[i])
        }
        .normalize_or_zero()
    };

    let mut zs: Vec<Vec3> = (0..n).map(z_raw).collect();
    // 开放脊线的两端、以及中间局部共线的点，Z 沿用相邻点的。
    let first = zs.iter().copied().find(|z| *z != Vec3::ZERO).unwrap_or(Vec3::Z);
    let mut previous = first;
    for z in &mut zs {
        if *z == Vec3::ZERO {
            *z = previous;
        } else if z.dot(previous) < 0.0 {
            // 相邻两个 Z 反向时翻过来，否则截面会在拐点处拧 180°。
            *z = -*z;
        }
        previous = *z;
    }
    (0..n)
        .map(|i| {
            let y = y_axis(i);
            let z = zs[i];
            let x = y.cross(z).normalize_or_zero();
            // 重新正交化 Z：Y 与 Z 只在拐点处天然正交。
            let z = x.cross(y);
            Mat3::from_cols(x, y, z)
        })
        .collect()
}

/// `TextureTransform`：`Tc' = -C · S · R · C · T · Tc`（规范原文的顺序）。
struct TextureTransform {
    center: Vec2,
    rotation: f32,
    scale: Vec2,
    translation: Vec2,
}

impl TextureTransform {
    fn from_node(node: &VNode) -> Self {
        Self {
            center: node.vec2("center", Vec2::ZERO),
            rotation: node.f32("rotation", 0.0),
            scale: node.vec2("scale", Vec2::ONE),
            translation: node.vec2("translation", Vec2::ZERO),
        }
    }

    fn apply(&self, uv: Vec2) -> Vec2 {
        let p = uv + self.translation + self.center;
        let (sin, cos) = self.rotation.sin_cos();
        let p = Vec2::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos);
        p * self.scale - self.center
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(body: &str) -> VrmlScene {
        parse_scene(format!("#VRML V2.0 utf8\n{body}").as_bytes()).expect("应当能解析")
    }

    #[test]
    fn rejects_vrml1_and_non_vrml() {
        assert!(parse_scene(b"#VRML V1.0 ascii\nSeparator {}").is_err());
        assert!(parse_scene(b"solid cube\n").is_err());
    }

    #[test]
    fn unknown_nodes_scripts_and_routes_are_skipped_without_desync() {
        let s = scene(
            r#"
            PerspectiveCamera { position 1 2 3 orientation 0 1 0 1 heightAngle 0.7 }
            DEF S Script { url "javascript:
                function f() { return \"}\"; }" eventIn SFTime touched field SFFloat speed 2 }
            DEF T TimeSensor { cycleInterval 3 loop TRUE }
            ROUTE T.fraction_changed TO S.touched
            PROTO Foo [ field SFColor c 1 0 0 ] { Shape { geometry Box {} } }
            Shape { geometry Box { size 1 2 3 } }
        "#,
        );
        assert_eq!(s.model.meshes().len(), 1, "只有最后那个 Box 是真几何");
        let (min, max) = s.bounds;
        assert!((max - min - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-4);
    }

    #[test]
    fn def_use_shares_one_mesh() {
        let s = scene(
            r#"
            Transform { translation -2 0 0 children Shape { geometry DEF B Box {} } }
            Transform { translation 2 0 0 children Shape { geometry USE B } }
        "#,
        );
        assert_eq!(s.model.meshes().len(), 1);
        let parts: usize = s.model.nodes().iter().map(|n| n.parts.len()).sum();
        assert_eq!(parts, 2);
    }

    #[test]
    fn transform_center_and_rotation_compose_per_spec() {
        // 绕 center (1,0,0) 转 180°：原点被转到 (2,0,0)。
        let node = parse(b"#VRML V2.0 utf8\nTransform { center 1 0 0 rotation 0 1 0 3.14159265 }").unwrap();
        let matrix = transform_matrix(&node[0]);
        let moved = matrix.transform_point3(Vec3::ZERO);
        assert!((moved - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4, "{moved:?}");
    }

    #[test]
    fn indexed_face_set_triangulates_and_faces_ccw() {
        let s = scene(
            r#"Shape { geometry IndexedFaceSet {
                coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0, 0 1 0 ] }
                coordIndex [ 0 1 2 3 -1 ]
            } }"#,
        );
        let mesh = &s.model.meshes()[0];
        assert_eq!(mesh.triangle_count(), 2);
        assert!(mesh.vertices().iter().all(|v| v.normal() == Vec3::Z), "逆时针 → 法线朝 +Z");
    }

    #[test]
    fn ccw_false_flips_the_face() {
        let s = scene(
            r#"Shape { geometry IndexedFaceSet { ccw FALSE
                coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0 ] } coordIndex [ 0 1 2 ] } }"#,
        );
        let mesh = &s.model.meshes()[0];
        assert!(mesh.vertices().iter().all(|v| v.normal() == Vec3::NEG_Z));
        let v = mesh.vertices();
        let i = mesh.indices();
        let n = (v[i[1] as usize].position() - v[i[0] as usize].position())
            .cross(v[i[2] as usize].position() - v[i[0] as usize].position());
        assert!(n.z < 0.0, "绕序也得跟着翻，否则背面剔除会剔错面");
    }

    #[test]
    fn crease_angle_smooths_shallow_edges_only() {
        // 两个夹角 ~11° 的面：creaseAngle 0.5 时共享边上的法线被平均。
        let body = |crease: f32| {
            format!(
                r#"Shape {{ geometry IndexedFaceSet {{ creaseAngle {crease}
                coord Coordinate {{ point [ 0 0 0, 1 0 0, 1 1 0, 0 1 0, 2 0 0.2, 2 1 0.2 ] }}
                coordIndex [ 0 1 2 3 -1, 1 4 5 2 -1 ] }} }}"#
            )
        };
        let flat = scene(&body(0.0));
        let smooth = scene(&body(0.5));
        let distinct = |s: &VrmlScene| {
            let mut normals: Vec<[i32; 3]> = s.model.meshes()[0]
                .vertices()
                .iter()
                .map(|v| (v.normal() * 1000.0).round().as_ivec3().to_array())
                .collect();
            normals.sort();
            normals.dedup();
            normals.len()
        };
        assert_eq!(distinct(&flat), 2);
        assert!(distinct(&smooth) > 2, "共享边上该出现平均过的法线");
    }

    #[test]
    fn non_convex_faces_are_ear_clipped() {
        // 一个 L 形：扇形三角化会在凹角外面多出一块。
        let s = scene(
            r#"Shape { geometry IndexedFaceSet { convex FALSE
                coord Coordinate { point [ 0 0 0, 2 0 0, 2 1 0, 1 1 0, 1 2 0, 0 2 0 ] }
                coordIndex [ 0 1 2 3 4 5 ] } }"#,
        );
        let mesh = &s.model.meshes()[0];
        let v = mesh.vertices();
        let area: f32 = mesh
            .indices()
            .chunks_exact(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|k| v[t[k] as usize].position());
                (b - a).cross(c - a).z * 0.5
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-4, "L 形面积是 3，得到 {area}");
    }

    #[test]
    fn per_face_colours_and_indices() {
        let s = scene(
            r#"Shape { geometry IndexedFaceSet { colorPerVertex FALSE
                coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0, 0 1 0 ] }
                color Color { color [ 1 0 0, 0 0 1 ] }
                colorIndex [ 1 0 ]
                coordIndex [ 0 1 2 -1 0 2 3 -1 ] } }"#,
        );
        let v = s.model.meshes()[0].vertices();
        assert_eq!(v[0].color, [0.0, 0.0, 1.0]);
        assert_eq!(v[3].color, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn elevation_grid_faces_up() {
        let s = scene(
            r#"Shape { geometry ElevationGrid { xDimension 3 zDimension 2 xSpacing 1 zSpacing 1 height [ 0 0 0 0 0 0 ] } }"#,
        );
        let mesh = &s.model.meshes()[0];
        assert_eq!(mesh.triangle_count(), 4);
        assert!(mesh.vertices().iter().all(|v| v.normal() == Vec3::Y));
    }

    #[test]
    fn default_extrusion_is_a_closed_box_with_outward_faces() {
        let s = scene("Shape { geometry Extrusion {} }");
        let mesh = &s.model.meshes()[0];
        // 4 个侧面 + 2 个端盖，每个两个三角形。
        assert_eq!(mesh.triangle_count(), 12);
        let v = mesh.vertices();
        let center = Vec3::new(0.0, 0.5, 0.0);
        for t in mesh.indices().chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| v[t[k] as usize].position());
            let n = (b - a).cross(c - a);
            assert!(n.dot((a + b + c) / 3.0 - center) > 0.0, "挤出体的面该朝外");
        }
    }

    #[test]
    fn concave_extrusion_caps_are_filled_without_spill() {
        // U 形截面，端盖必须走耳切。
        let s = scene(
            r#"Shape { geometry Extrusion { convex FALSE
                crossSection [ 0 0, 3 0, 3 3, 2 3, 2 1, 1 1, 1 3, 0 3, 0 0 ]
                spine [ 0 0 0, 0 1 0 ] } }"#,
        );
        let mesh = &s.model.meshes()[0];
        let v = mesh.vertices();
        let cap_area: f32 = mesh
            .indices()
            .chunks_exact(3)
            .map(|t| [0, 1, 2].map(|k| v[t[k] as usize].position()))
            .filter(|[a, b, c]| a.y == b.y && b.y == c.y)
            .map(|[a, b, c]| (b - a).cross(c - a).length() * 0.5)
            .sum();
        // U 形面积 9 - 2 = 7，两个端盖。
        assert!((cap_area - 14.0).abs() < 1e-3, "端盖总面积应为 14，得到 {cap_area}");
    }

    #[test]
    fn curved_spine_keeps_the_section_perpendicular() {
        let s = scene(
            r#"Shape { geometry Extrusion { beginCap FALSE endCap FALSE
                crossSection [ 0.1 0, 0 0.1, -0.1 0, 0 -0.1, 0.1 0 ]
                spine [ 0 0 0, 1 0 0, 1 1 0, 1 1 1 ] } }"#,
        );
        assert!(s.model.meshes()[0].vertices().iter().all(|v| v.position().is_finite()));
    }

    #[test]
    fn lines_follow_colour_indices_and_transforms() {
        let s = scene(
            r#"Transform { translation 0 0 5 children Shape { geometry IndexedLineSet {
                coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0 ] }
                colorPerVertex FALSE
                color Color { color [ 1 0 0, 0 1 0, 0 0 1 ] }
                coordIndex [ 0 1 -1 1 2 -1 ]
                colorIndex [ 2 1 ] } } }"#,
        );
        let lines = s.lines.expect("该有线段");
        assert_eq!(lines.segment_count(), 2);
        let v = lines.vertices();
        assert_eq!(v[0].position, [0.0, 0.0, 5.0]);
        assert_eq!(v[0].color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(v[2].color, [0.0, 1.0, 0.0, 1.0]);
        assert!(s.model.meshes().is_empty());
    }

    #[test]
    fn transparent_lines_take_emissive_colour_and_alpha() {
        let s = scene(
            r#"Shape { appearance Appearance { material Material { emissiveColor 1 0 0 transparency 0.8 } }
                geometry IndexedLineSet { coord Coordinate { point [ 0 0 0, 1 0 0 ] } coordIndex [ 0 1 ] } }"#,
        );
        let v = s.lines.unwrap().vertices()[0];
        assert_eq!(v.color[0], 1.0);
        assert!((v.color[3] - 0.2).abs() < 1e-5);
    }

    #[test]
    fn point_sets_become_a_point_cloud() {
        let s = scene(
            r#"Shape { geometry PointSet { coord Coordinate { point [ 0 0 0, 1 1 0 ] }
                color Color { color [ 1 0 0, 0 1 0 ] } } }"#,
        );
        let points = s.points.unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points.colors[1], Vec3::Y);
    }

    #[test]
    fn shapes_without_material_are_unlit_and_solid_false_is_double_sided() {
        let s = scene(
            r#"Shape { geometry IndexedFaceSet { solid FALSE coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0 ] } coordIndex [ 0 1 2 ] } }
               Shape { appearance Appearance { material Material { diffuseColor 1 0 0 transparency 0.5 } } geometry Box {} }"#,
        );
        let unlit = &s.model.materials()[0];
        assert!(unlit.shader().is_some(), "没有 Material 的形体该用不受光的钩子");
        assert!(unlit.double_sided());
        let lit = &s.model.materials()[1];
        assert!(lit.shader().is_none());
        assert_eq!(lit.blend_mode(), BlendMode::Alpha);
        assert_eq!(lit.base_color(), Vec4::new(1.0, 0.0, 0.0, 0.5));
    }

    #[test]
    fn pixel_texture_is_flipped_and_replaces_diffuse_when_rgb() {
        let (texture, components) = pixel_texture(&[1.0, 2.0, 3.0, 0xFF0000 as f64, 0x0000FF as f64]).unwrap();
        assert_eq!(components, 3);
        // 第一个像素在左下角 → 内存里的最后一行。
        assert_eq!(&texture.data()[4..8], &[255, 0, 0, 255]);
        assert_eq!(&texture.data()[0..4], &[0, 0, 255, 255]);

        let s = scene(
            r#"Shape { appearance Appearance { texture PixelTexture { image 1 1 3 0x00FF00 }
                material Material { diffuseColor 0.2 0.2 0.2 } } geometry Box {} }"#,
        );
        let material = &s.model.materials()[0];
        assert_eq!(material.base_color().truncate(), Vec3::ONE, "RGB 贴图替换漫反射色");
        assert!(material.base_color_texture().is_some());
    }

    #[test]
    fn background_gradient_interpolates_by_angle() {
        let s = scene("Background { skyColor [ 0 0 0, 1 1 1 ] skyAngle [ 1.5707963 ] groundColor [ 1 0 0, 1 0 0 ] groundAngle [ 0.5 ] }");
        let background = s.background.unwrap();
        assert_eq!(background.color_at(0.0), Vec3::ZERO);
        assert!((background.color_at(std::f32::consts::FRAC_PI_4).x - 0.5).abs() < 1e-4);
        assert_eq!(background.color_at(std::f32::consts::PI), Vec3::X, "天底被地面盖住");
        let mesh = background.to_mesh(10.0);
        assert!(mesh.vertices().iter().all(|v| (v.position().length() - 10.0).abs() < 1e-3));
    }

    #[test]
    fn broken_files_error_instead_of_panicking() {
        for text in [
            "#VRML V2.0 utf8\nShape { geometry IndexedFaceSet { coordIndex [ 0 1 2",
            "#VRML V2.0 utf8\nTransform { children [ Shape { ] }",
            "#VRML V2.0 utf8\nShape { geometry IndexedFaceSet { coord Coordinate { point [ 0 0 0 ] } coordIndex [ 0 5 9 -7 ] } }",
            "#VRML V2.0 utf8\nUSE nothing DEF",
            "#VRML V2.0 utf8\n\"unterminated",
        ] {
            let _ = parse_scene(text.as_bytes());
        }
        let deep = format!("#VRML V2.0 utf8\n{}", "Group { children [ ".repeat(1000));
        assert!(parse_scene(deep.as_bytes()).is_err());
    }
}
