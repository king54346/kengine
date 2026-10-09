//! LDraw（`.ldr` / `.mpd` / `.dat`）：乐高积木的开放 CAD 格式。
//!
//! 一个 LDraw 文件是一行一条命令，行首数字是类型：
//!
//! | 类型 | 含义 |
//! |---|---|
//! | 0 | 注释或元命令（`!COLOUR`、`BFC`、`STEP`、`FILE`） |
//! | 1 | 引用另一个文件（子零件），带颜色和 3×4 变换 |
//! | 2 | 一条边线 |
//! | 3 / 4 | 三角形 / 四边形 |
//! | 5 | 条件边线（只在轮廓处画） |
//!
//! 一块积木由几十层子零件拼成（「4×2 砖」→「砖壳」→「凸点」→「圆柱面」），
//! 整个模型展开下来常常是几十万个三角形。
//!
//! # 支持到哪
//!
//! - MPD 多文件包（`0 FILE`）和官方的 **Packed** 格式（零件全部内联）；
//!   不内联的零件按 `parts/`、`p/`、`models/` 的库目录结构去找，找不到跳过；
//! - 颜色表 `0 !COLOUR`（`VALUE`、`EDGE`、`ALPHA`、`LUMINANCE`、
//!   `CHROME` / `PEARLESCENT` / `RUBBER` / `METAL` / `MATTE_METALLIC`）、
//!   继承色 16 和边线色 24、直接色 `0x2RRGGBB`；
//! - BFC 背面剔除约定（`CERTIFY CCW/CW`、`INVERTNEXT`、镜像变换翻转绕序）；
//!   没有认证的几何两面都画；
//! - 顶层模型的 `0 STEP`：每一步一个节点，可以逐步显示拼装过程；
//! - 边线（类型 2）单独输出成线段，和 three.js 一样可以开关。
//!
//! 法线按**折角**平滑：两个面夹角小于 [`CREASE_ANGLE`] 的共享顶点取平均，
//! 大于的保持锐边——积木的直角棱不会被抹圆，圆柱面又是光滑的。
//!
//! # 不支持的
//!
//! - 条件边线（类型 5）：需要按屏幕空间判断的专用着色器；
//! - `!TEXMAP` 贴图投影、`!DATA` 内嵌图片；
//! - `GLITTER` / `SPECKLE` 这类颗粒材质（按底色处理）。
//!
//! # 坐标
//!
//! LDraw 的 Y 轴朝**下**，单位是 LDU（一个凸点间距 = 20 LDU）。根节点绕
//! X 轴转 180°（three.js 的做法），单位原样保留。

use crate::{base_dir, loader, sibling};
use kasset::{LoadError, ResourceIo};
use kgizmo::{Color, LineSet, LineSetBuilder};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::{BlendMode, Material};
use kmath::{Mat3, Quat, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

loader! {
    /// 读 `.ldr` / `.mpd` / `.dat`（只出三角形；要边线用 [`parse_str`]）。
    LDrawLoader -> Model : ["ldr", "mpd", "dat"] = MODEL_TYPE_UUID, parse
}

/// 平滑法线的折角阈值（度）。
pub const CREASE_ANGLE: f32 = 40.0;

/// 解析结果：模型 + 每一步的边线。
pub struct LDrawModel {
    /// 三角形部分。根节点下每个拼装步骤一个子节点，名字是 `Step N`（从 0 数）。
    pub model: Model,
    /// 每一步的边线，坐标和对应的 `Step N` 节点同一个空间。
    pub edges: Vec<LineSet>,
    /// 拼装步骤数（至少 1）。
    pub steps: usize,
    /// 缺失的子文件名。
    pub missing: Vec<String>,
}

/// 异步加载入口：主文件之外，缺的子零件按库目录结构去找。
pub async fn parse(
    bytes: Vec<u8>,
    path: PathBuf,
    io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let base = base_dir(&path);
    let mut library = Library::default();
    library.add_document(&text, &main_name(&path));
    // 一层层补：读到的零件又会引用新的零件。
    let mut tried = HashSet::new();
    loop {
        let missing: Vec<String> = library
            .unresolved()
            .into_iter()
            .filter(|n| tried.insert(n.clone()))
            .collect();
        if missing.is_empty() {
            break;
        }
        for name in missing {
            for candidate in library_candidates(&name) {
                if let Some(bytes) = sibling(&io, &base, &candidate).await {
                    library.add_document(&String::from_utf8_lossy(&bytes), &name);
                    break;
                }
            }
        }
    }
    Ok(library.build(&main_name(&path))?.model)
}

/// 同步解析一份文本（Packed 文件自带全部零件）。`name` 是主文件名，
/// 只在文档里没有 `0 FILE` 时用来给它起名。
pub fn parse_str(text: &str, name: &str) -> Result<LDrawModel, LoadError> {
    let mut library = Library::default();
    library.add_document(text, name);
    library.build(name)
}

/// 同步读一个文件。缺的零件按库目录结构（文件所在目录及其上级的
/// `parts/`、`p/`、`models/`）去找。
pub fn load(path: impl AsRef<Path>) -> Result<LDrawModel, LoadError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source: Arc::new(source),
    })?;
    let name = main_name(path);
    let mut library = Library::default();
    library.add_document(&text, &name);
    let mut roots = vec![base_dir(path)];
    if let Some(parent) = roots[0].parent() {
        roots.push(parent.to_path_buf());
    }
    let mut tried = HashSet::new();
    loop {
        let missing: Vec<String> = library
            .unresolved()
            .into_iter()
            .filter(|n| tried.insert(n.clone()))
            .collect();
        if missing.is_empty() {
            break;
        }
        for name in missing {
            let found = roots
                .iter()
                .flat_map(|root| {
                    library_candidates(&name)
                        .into_iter()
                        .map(move |c| root.join(c))
                })
                .find_map(|file| std::fs::read_to_string(file).ok());
            if let Some(text) = found {
                library.add_document(&text, &name);
            }
        }
    }
    library.build(&name)
}

fn main_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "main.ldr".into())
}

fn normalize_name(name: &str) -> String {
    name.trim().replace('\\', "/").to_lowercase()
}

fn library_candidates(name: &str) -> Vec<String> {
    ["", "parts/", "p/", "models/", "parts/s/", "p/48/"]
        .iter()
        .map(|dir| format!("{dir}{name}"))
        .collect()
}

// ── 颜色 ──

#[derive(Debug, Clone, Copy)]
struct Colour {
    /// 线性 RGBA。
    value: Vec4,
    edge: Vec3,
    luminance: f32,
    finish: Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    Plastic,
    Chrome,
    Pearlescent,
    Rubber,
    Metal,
    MatteMetallic,
}

fn srgb_hex(text: &str) -> Option<Vec3> {
    let hex = text.trim_start_matches('#');
    let value = u32::from_str_radix(hex, 16).ok()?;
    let channel = |shift: u32| {
        let c = ((value >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(Vec3::new(channel(16), channel(8), channel(0)))
}

fn parse_colour(tokens: &[&str]) -> Option<(u32, Colour)> {
    let mut code = None;
    let mut colour = Colour {
        value: Vec4::new(0.5, 0.5, 0.5, 1.0),
        edge: Vec3::ZERO,
        luminance: 0.0,
        finish: Finish::Plastic,
    };
    let mut i = 0;
    while i < tokens.len() {
        let next = tokens.get(i + 1).copied().unwrap_or("");
        match tokens[i].to_ascii_uppercase().as_str() {
            "CODE" => {
                code = next.parse().ok();
                i += 1;
            }
            "VALUE" => {
                if let Some(v) = srgb_hex(next) {
                    colour.value = v.extend(colour.value.w);
                }
                i += 1;
            }
            "EDGE" => {
                if let Some(v) = srgb_hex(next) {
                    colour.edge = v;
                }
                i += 1;
            }
            "ALPHA" => {
                colour.value.w = next.parse::<f32>().unwrap_or(255.0) / 255.0;
                i += 1;
            }
            "LUMINANCE" => {
                colour.luminance = next.parse::<f32>().unwrap_or(0.0) / 255.0;
                i += 1;
            }
            "CHROME" => colour.finish = Finish::Chrome,
            "PEARLESCENT" => colour.finish = Finish::Pearlescent,
            "RUBBER" => colour.finish = Finish::Rubber,
            "METAL" => colour.finish = Finish::Metal,
            "MATTE_METALLIC" => colour.finish = Finish::MatteMetallic,
            _ => {}
        }
        i += 1;
    }
    code.map(|code| (code, colour))
}

/// 颜色表里没有时的回退：LDraw 的几个基本色。
fn fallback_colour(code: u32) -> Colour {
    let hex = match code {
        0 => "#05131D",
        1 => "#0055BF",
        2 => "#257A3E",
        4 => "#C91A09",
        14 => "#F2CD37",
        15 => "#FFFFFF",
        71 => "#A0A5A9",
        72 => "#6C6E68",
        _ => "#A0A5A9",
    };
    Colour {
        value: srgb_hex(hex).unwrap_or(Vec3::splat(0.5)).extend(1.0),
        edge: if code == 0 {
            Vec3::splat(0.1)
        } else {
            Vec3::ZERO
        },
        luminance: 0.0,
        finish: Finish::Plastic,
    }
}

/// 解析后的颜色引用：继承（16）、边线色（24）或确定的颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ColourRef {
    Code(u32),
    /// `0x2RRGGBB` 直接色。
    Direct(u32),
}

fn parse_colour_ref(token: &str) -> Option<i64> {
    if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok().map(|v| -v - 1)
    } else {
        token.parse().ok()
    }
}

// ── 文件 ──

#[derive(Debug, Clone)]
enum Command {
    Sub {
        colour: i64,
        matrix: [f32; 12],
        name: String,
        invert: bool,
    },
    Line {
        colour: i64,
        points: [Vec3; 2],
    },
    Triangle {
        colour: i64,
        points: [Vec3; 3],
        ccw: bool,
        certified: bool,
    },
    Quad {
        colour: i64,
        points: [Vec3; 4],
        ccw: bool,
        certified: bool,
    },
    Step,
}

#[derive(Debug, Default)]
struct File {
    commands: Vec<Command>,
}

#[derive(Default)]
struct Library {
    files: HashMap<String, File>,
    colours: HashMap<u32, Colour>,
    /// 文档里第一个 `0 FILE`（MPD 的主模型）。
    first: Option<String>,
}

impl Library {
    fn add_document(&mut self, text: &str, default_name: &str) {
        let mut current = normalize_name(default_name);
        let mut file = File::default();
        let mut certified = false;
        let mut ccw = true;
        let mut invert_next = false;
        let mut started = false;
        for raw in text.lines() {
            let line = raw.trim();
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let Some(&kind) = tokens.first() else {
                continue;
            };
            match kind {
                "0" => {
                    let meta = tokens
                        .get(1)
                        .map(|t| t.to_ascii_uppercase())
                        .unwrap_or_default();
                    match meta.as_str() {
                        "FILE" => {
                            if started || !file.commands.is_empty() {
                                self.files.insert(
                                    std::mem::take(&mut current),
                                    std::mem::take(&mut file),
                                );
                            }
                            current = normalize_name(&line[line.find("FILE").unwrap_or(0) + 4..]);
                            if self.first.is_none() {
                                self.first = Some(current.clone());
                            }
                            started = true;
                            certified = false;
                            ccw = true;
                        }
                        "NOFILE" => {}
                        "!COLOUR" => {
                            if let Some((code, colour)) = parse_colour(&tokens[2..]) {
                                self.colours.insert(code, colour);
                            }
                        }
                        "BFC" => {
                            for token in &tokens[2..] {
                                match token.to_ascii_uppercase().as_str() {
                                    "CERTIFY" => certified = true,
                                    "NOCERTIFY" => certified = false,
                                    "CW" => ccw = false,
                                    "CCW" => ccw = true,
                                    "INVERTNEXT" => invert_next = true,
                                    _ => {}
                                }
                            }
                        }
                        "STEP" => file.commands.push(Command::Step),
                        _ => {}
                    }
                }
                "1" if tokens.len() >= 15 => {
                    let numbers: Vec<f32> = tokens[2..14]
                        .iter()
                        .map(|t| t.parse().unwrap_or(0.0))
                        .collect();
                    let mut matrix = [0.0; 12];
                    matrix.copy_from_slice(&numbers);
                    // 文件名可以带空格：取第 14 个记号起的原文。
                    let name = nth_rest(line, 14);
                    file.commands.push(Command::Sub {
                        colour: parse_colour_ref(tokens[1]).unwrap_or(16),
                        matrix,
                        name: normalize_name(name),
                        invert: std::mem::take(&mut invert_next),
                    });
                }
                "2" if tokens.len() >= 8 => {
                    let p = points::<2>(&tokens[2..]);
                    file.commands.push(Command::Line {
                        colour: parse_colour_ref(tokens[1]).unwrap_or(24),
                        points: p,
                    });
                }
                "3" if tokens.len() >= 11 => {
                    file.commands.push(Command::Triangle {
                        colour: parse_colour_ref(tokens[1]).unwrap_or(16),
                        points: points::<3>(&tokens[2..]),
                        ccw,
                        certified,
                    });
                }
                "4" if tokens.len() >= 14 => {
                    file.commands.push(Command::Quad {
                        colour: parse_colour_ref(tokens[1]).unwrap_or(16),
                        points: points::<4>(&tokens[2..]),
                        ccw,
                        certified,
                    });
                }
                _ => {}
            }
        }
        self.files.entry(current).or_insert(file);
    }

    /// 被引用了、但还没读到的文件名。
    fn unresolved(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .files
            .values()
            .flat_map(|file| file.commands.iter())
            .filter_map(|command| match command {
                Command::Sub { name, .. } if !self.files.contains_key(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn colour(&self, code: u32) -> Colour {
        self.colours
            .get(&code)
            .copied()
            .unwrap_or_else(|| fallback_colour(code))
    }

    fn build(&self, main: &str) -> Result<LDrawModel, LoadError> {
        let main = normalize_name(main);
        let root_name = if self.files.contains_key(&main) && !self.files[&main].commands.is_empty()
        {
            main
        } else {
            self.first
                .clone()
                .ok_or_else(|| crate::bad("LDraw 文件里没有任何内容"))?
        };
        let mut builder = Builder {
            library: self,
            buckets: HashMap::new(),
            edges: Vec::new(),
            missing: HashSet::new(),
            step: 0,
            depth: 0,
        };
        let root = &self.files[&root_name];
        builder.walk(root, ColourRef::Code(7), Frame::IDENTITY, false, true, true);
        let steps = builder.step + 1;
        Ok(builder.finish(steps))
    }
}

fn nth_rest(line: &str, n: usize) -> &str {
    let mut rest = line;
    for _ in 0..n {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = &rest[end..];
    }
    rest.trim()
}

fn points<const N: usize>(tokens: &[&str]) -> [Vec3; N] {
    std::array::from_fn(|i| {
        let v = |k: usize| {
            tokens
                .get(i * 3 + k)
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0)
        };
        Vec3::new(v(0), v(1), v(2))
    })
}

// ── 展开 ──

#[derive(Clone, Copy)]
struct Frame {
    linear: Mat3,
    translation: Vec3,
}

impl Frame {
    const IDENTITY: Self = Self {
        linear: Mat3::IDENTITY,
        translation: Vec3::ZERO,
    };

    fn apply(&self, p: Vec3) -> Vec3 {
        self.linear * p + self.translation
    }

    fn then(&self, matrix: &[f32; 12]) -> Self {
        let [x, y, z, a, b, c, d, e, f, g, h, i] = *matrix;
        // LDraw 按行给：[a b c; d e f; g h i]，glam 按列构造。
        let local = Mat3::from_cols(Vec3::new(a, d, g), Vec3::new(b, e, h), Vec3::new(c, f, i));
        Self {
            linear: self.linear * local,
            translation: self.apply(Vec3::new(x, y, z)),
        }
    }
}

/// 一个「步骤 × 颜色」桶里的三角形（已经展开到模型空间、绕序已经理顺）。
#[derive(Default)]
struct Bucket {
    triangles: Vec<[Vec3; 3]>,
}

struct Builder<'a> {
    library: &'a Library,
    buckets: HashMap<(usize, ColourRef), Bucket>,
    edges: Vec<(usize, Vec3, Vec3, Vec3)>,
    missing: HashSet<String>,
    step: usize,
    depth: usize,
}

impl Builder<'_> {
    fn resolve(&self, token: i64, inherited: ColourRef, edge: bool) -> ColourRef {
        match token {
            16 => inherited,
            24 => match inherited {
                // 边线色另外编码：用一个不会和正常颜色码冲突的偏移。
                ColourRef::Code(code) if edge => ColourRef::Code(code | 0x8000_0000),
                other => other,
            },
            t if t < 0 => ColourRef::Direct((-(t + 1)) as u32),
            t => ColourRef::Code(t as u32),
        }
    }

    fn walk(
        &mut self,
        file: &File,
        colour: ColourRef,
        frame: Frame,
        inverted: bool,
        certified_chain: bool,
        top: bool,
    ) {
        self.depth += 1;
        if self.depth > 64 {
            self.depth -= 1;
            return;
        }
        let mirrored = frame.linear.determinant() < 0.0;
        for command in &file.commands {
            match command {
                Command::Step => {
                    if top {
                        self.step += 1;
                    }
                }
                Command::Sub {
                    colour: c,
                    matrix,
                    name,
                    invert,
                } => {
                    let Some(child) = self.library.files.get(name) else {
                        self.missing.insert(name.clone());
                        continue;
                    };
                    let child_colour = self.resolve(*c, colour, false);
                    self.walk(
                        child,
                        child_colour,
                        frame.then(matrix),
                        inverted ^ invert,
                        certified_chain,
                        false,
                    );
                }
                Command::Line { colour: c, points } => {
                    let edge = self.edge_colour(self.resolve(*c, colour, true));
                    self.edges.push((
                        self.step,
                        frame.apply(points[0]),
                        frame.apply(points[1]),
                        edge,
                    ));
                }
                Command::Triangle {
                    colour: c,
                    points,
                    ccw,
                    certified,
                } => {
                    let key = (self.step, self.resolve(*c, colour, false));
                    let p = points.map(|p| frame.apply(p));
                    self.push(
                        key,
                        &[p],
                        *ccw,
                        inverted ^ mirrored,
                        certified_chain && *certified,
                    );
                }
                Command::Quad {
                    colour: c,
                    points,
                    ccw,
                    certified,
                } => {
                    let key = (self.step, self.resolve(*c, colour, false));
                    let p = points.map(|p| frame.apply(p));
                    self.push(
                        key,
                        &[[p[0], p[1], p[2]], [p[0], p[2], p[3]]],
                        *ccw,
                        inverted ^ mirrored,
                        certified_chain && *certified,
                    );
                }
            }
        }
        self.depth -= 1;
    }

    fn push(
        &mut self,
        key: (usize, ColourRef),
        triangles: &[[Vec3; 3]],
        ccw: bool,
        inverted: bool,
        certified: bool,
    ) {
        let bucket = self.buckets.entry(key).or_default();
        for &[a, b, c] in triangles {
            // 引擎的正面是逆时针；文件声明顺时针、或者累计翻转过一次，都要反过来。
            let reverse = !ccw ^ inverted;
            let front = if reverse { [a, c, b] } else { [a, b, c] };
            bucket.triangles.push(front);
            if !certified {
                // 没有认证的几何不知道哪面朝外：两面都画。
                bucket.triangles.push([front[0], front[2], front[1]]);
            }
        }
    }

    fn edge_colour(&self, colour: ColourRef) -> Vec3 {
        match colour {
            ColourRef::Code(code) if code & 0x8000_0000 != 0 => {
                self.library.colour(code & 0x7fff_ffff).edge
            }
            ColourRef::Code(code) => self.library.colour(code).value.truncate(),
            ColourRef::Direct(rgb) => {
                srgb_hex(&format!("{:06x}", rgb & 0xff_ffff)).unwrap_or(Vec3::ZERO)
            }
        }
    }

    fn material(&self, colour: ColourRef) -> Material {
        let (colour, name) = match colour {
            ColourRef::Code(code) => (
                self.library.colour(code & 0x7fff_ffff),
                format!("LDraw {}", code & 0x7fff_ffff),
            ),
            ColourRef::Direct(rgb) => (
                Colour {
                    value: srgb_hex(&format!("{:06x}", rgb & 0xff_ffff))
                        .unwrap_or(Vec3::splat(0.5))
                        .extend(1.0),
                    edge: Vec3::ZERO,
                    luminance: 0.0,
                    finish: Finish::Plastic,
                },
                format!("LDraw #{:06x}", rgb & 0xff_ffff),
            ),
        };
        // 和 three.js LDrawLoader 的取值一致。
        let (metallic, roughness) = match colour.finish {
            Finish::Plastic => (0.0, 0.3),
            Finish::Chrome => (1.0, 0.0),
            Finish::Pearlescent => (0.25, 0.3),
            Finish::Rubber => (0.0, 0.9),
            Finish::Metal => (1.0, 0.4),
            Finish::MatteMetallic => (0.4, 0.8),
        };
        let mut material = Material::standard()
            .with_name(name)
            .with_base_color(colour.value)
            .with_metallic(metallic)
            .with_roughness(roughness);
        if colour.value.w < 1.0 {
            material.set_blend_mode(BlendMode::Alpha);
        }
        if colour.luminance > 0.0 {
            material.set(
                kpbr::standard::EMISSIVE,
                colour.value.truncate() * colour.luminance,
            );
        }
        material
    }

    fn finish(self, steps: usize) -> LDrawModel {
        let mut keys: Vec<(usize, ColourRef)> = self.buckets.keys().copied().collect();
        keys.sort_by_key(|(step, colour)| {
            (
                *step,
                match colour {
                    ColourRef::Code(c) => (0, *c),
                    ColourRef::Direct(c) => (1, *c),
                },
            )
        });
        let mut material_index: HashMap<ColourRef, usize> = HashMap::new();
        let mut materials = Vec::new();
        let mut meshes = Vec::new();
        let mut nodes = vec![ModelNode {
            name: "LDraw".into(),
            transform: NodeTransform {
                rotation: Quat::from_rotation_x(std::f32::consts::PI),
                ..NodeTransform::default()
            },
            children: (1..=steps).collect(),
            parts: Vec::new(),
            skin: None,
        }];
        for step in 0..steps {
            nodes.push(ModelNode {
                name: format!("Step {step}"),
                transform: NodeTransform::default(),
                children: Vec::new(),
                parts: Vec::new(),
                skin: None,
            });
        }
        for key in keys {
            let bucket = &self.buckets[&key];
            if bucket.triangles.is_empty() {
                continue;
            }
            let material = *material_index.entry(key.1).or_insert_with(|| {
                materials.push(self.material(key.1));
                materials.len() - 1
            });
            meshes.push(crease_mesh(&bucket.triangles, CREASE_ANGLE));
            nodes[1 + key.0].parts.push(MeshPart {
                mesh: meshes.len() - 1,
                material: Some(material),
            });
        }
        let mut edge_builders: Vec<LineSetBuilder> =
            (0..steps).map(|_| LineSetBuilder::default()).collect();
        for (step, a, b, colour) in &self.edges {
            edge_builders[*step].line(*a, *b, Color::rgb(colour.x, colour.y, colour.z));
        }
        let mut missing: Vec<String> = self.missing.into_iter().collect();
        missing.sort();
        if !missing.is_empty() {
            klog::warn!("LDraw 缺 {} 个子文件，例如 {}", missing.len(), missing[0]);
        }
        LDrawModel {
            model: Model::new(meshes, materials, nodes, vec![0]),
            edges: edge_builders
                .into_iter()
                .map(LineSetBuilder::build)
                .collect(),
            steps,
            missing,
        }
    }
}

/// 按折角平滑法线：共享位置、且和本面夹角小于 `angle` 度的面才参与平均。
pub fn crease_mesh(triangles: &[[Vec3; 3]], angle: f32) -> Mesh {
    let quantize = |p: Vec3| {
        [
            (p.x * 1000.0).round() as i64,
            (p.y * 1000.0).round() as i64,
            (p.z * 1000.0).round() as i64,
        ]
    };
    let face_normals: Vec<Vec3> = triangles
        .iter()
        .map(|[a, b, c]| (*b - *a).cross(*c - *a))
        .collect();
    // 位置 → 用到它的 (面, 角)。
    let mut shared: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (face, triangle) in triangles.iter().enumerate() {
        for p in triangle {
            shared.entry(quantize(*p)).or_default().push(face);
        }
    }
    let threshold = angle.to_radians().cos();
    let mut vertices = Vec::with_capacity(triangles.len() * 3);
    let mut indices = Vec::with_capacity(triangles.len() * 3);
    let mut lookup: HashMap<([i64; 3], [i32; 3]), u32> = HashMap::new();
    for (face, triangle) in triangles.iter().enumerate() {
        let own = face_normals[face].normalize_or_zero();
        for p in triangle {
            let key = quantize(*p);
            let mut sum = Vec3::ZERO;
            for &other in &shared[&key] {
                let n = face_normals[other];
                // 面积加权（叉积长度就是两倍面积）。
                if n.normalize_or_zero().dot(own) >= threshold {
                    sum += n;
                }
            }
            let normal = sum.normalize_or(own);
            let normal_key = [
                (normal.x * 1000.0) as i32,
                (normal.y * 1000.0) as i32,
                (normal.z * 1000.0) as i32,
            ];
            let index = *lookup.entry((key, normal_key)).or_insert_with(|| {
                vertices.push(Vertex::new(*p, normal, [0.0, 0.0]));
                (vertices.len() - 1) as u32
            });
            indices.push(index);
        }
    }
    Mesh::new(vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRICK: &str = "\
0 !COLOUR Red CODE 4 VALUE #C91A09 EDGE #333333
0 !COLOUR Trans_Clear CODE 47 VALUE #FCFCFC EDGE #C3C3C3 ALPHA 128
0 FILE main.ldr
1 4 0 0 0 1 0 0 0 1 0 0 0 1 quad.dat
0 STEP
0 BFC INVERTNEXT
1 47 0 -10 0 1 0 0 0 1 0 0 0 1 quad.dat
0 FILE quad.dat
0 BFC CERTIFY CCW
4 16 0 0 0 10 0 0 10 0 10 0 0 10
2 24 0 0 0 10 0 0
";

    #[test]
    fn a_packed_document_expands_into_steps_and_colours() {
        let parsed = parse_str(BRICK, "main.ldr").unwrap();
        assert_eq!(parsed.steps, 2);
        assert!(parsed.missing.is_empty());
        let model = &parsed.model;
        // 两步，各一个颜色。
        assert_eq!(model.nodes()[1].name, "Step 0");
        assert_eq!(model.nodes()[1].parts.len(), 1);
        assert_eq!(model.nodes()[2].parts.len(), 1);
        assert_eq!(model.materials().len(), 2);
        let transparent = model
            .materials()
            .iter()
            .find(|m| m.name() == Some("LDraw 47"))
            .unwrap();
        assert_eq!(transparent.blend_mode(), BlendMode::Alpha);
        // 边线：色 24 取的是「继承色（红）」的边线色。
        assert_eq!(parsed.edges[0].segment_count(), 1);
    }

    #[test]
    fn invertnext_flips_the_winding() {
        let parsed = parse_str(BRICK, "main.ldr").unwrap();
        let normal_of = |step: usize| {
            let part = &parsed.model.nodes()[1 + step].parts[0];
            let mesh = &parsed.model.meshes()[part.mesh];
            let v = mesh.vertices();
            let i = mesh.indices();
            let [a, b, c] = [0, 1, 2].map(|k| v[i[k] as usize].position());
            (b - a).cross(c - a).normalize()
        };
        assert!(
            normal_of(0).dot(normal_of(1)) < -0.9,
            "INVERTNEXT 之后应当朝反方向"
        );
    }

    #[test]
    fn uncertified_geometry_is_double_sided() {
        let text = "0 FILE a.ldr\n3 4 0 0 0 1 0 0 0 0 1\n";
        let parsed = parse_str(text, "a.ldr").unwrap();
        assert_eq!(parsed.model.meshes()[0].indices().len(), 6);
    }

    #[test]
    fn missing_parts_are_reported_not_fatal() {
        let text = "1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n3 4 0 0 0 1 0 0 0 0 1\n";
        let parsed = parse_str(text, "a.ldr").unwrap();
        assert_eq!(parsed.missing, vec!["3001.dat".to_string()]);
    }

    #[test]
    fn crease_keeps_right_angles_sharp_and_shallow_angles_smooth() {
        // 两个直角相接的面：不共享法线，顶点要分开。
        let right = [
            [Vec3::ZERO, Vec3::X, Vec3::Y],
            [Vec3::ZERO, Vec3::Z, Vec3::X],
        ];
        assert_eq!(crease_mesh(&right, 40.0).vertices().len(), 6);
        // 夹角很小的两个面：共享边上的两个顶点合并。
        let shallow = [
            [Vec3::ZERO, Vec3::X, Vec3::new(0.0, 1.0, 0.0)],
            [
                Vec3::ZERO,
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(-1.0, 0.0, 0.2),
            ],
        ];
        assert_eq!(crease_mesh(&shallow, 40.0).vertices().len(), 4);
    }

    #[test]
    fn file_names_with_spaces_survive() {
        assert_eq!(
            nth_rest("1 16 0 0 0 1 0 0 0 1 0 0 0 1 My Part.dat", 14),
            "My Part.dat"
        );
    }
}
