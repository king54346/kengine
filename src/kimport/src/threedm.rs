//! 3DM（Rhino / openNURBS）：二进制分块格式。
//!
//! three.js 用的是 rhino3dm（openNURBS 编译成 WASM）。这里是纯 Rust 的
//! **子集**：按 openNURBS 源码（`opennurbs_archive.cpp`、`opennurbs_mesh.cpp`
//! 等）的写法读出「能直接画」的东西。
//!
//! # 读什么
//!
//! | 对象 | 怎么读 |
//! |---|---|
//! | `ON_Mesh` | 顶点 / 法线 / UV / 顶点色（zlib 压缩缓冲）、三角形与四边形 |
//! | `ON_Brep` / `ON_Extrusion` | **读它们缓存的显示网格**（Rhino 存盘时附带的 `ON_Mesh`），不求值曲面 |
//! | `ON_SubD` | 缓存网格；没有时读第 0 层控制网格，做 3 次带折边 / 角点的 Catmull–Clark 细分 |
//! | `ON_NurbsCurve` / `ON_LineCurve` / `ON_PolylineCurve` / `ON_PolyCurve` | 采样成折线，见 [`Rhino3dm::curves`] |
//! | `ON_PointCloud` | 点，见 [`Rhino3dm::points`] |
//! | 图层表 | 名字、颜色、可见性；每个图层一个节点，物体挂在所属图层下 |
//! | 物体属性 | 所属图层、名字、颜色（按图层 / 按物体）、隐藏 |
//!
//! # 不读什么
//!
//! 没有缓存网格的 Brep（「保存为小文件」存出来的）画不出来——那需要修剪
//! NURBS 曲面的细分，是 openNURBS 本身的规模。SubD 的细分用标准
//! Catmull–Clark 规则，和 Rhino 的极限曲面在 dart 顶点附近略有差别。
//! 遇到时打一条警告。材质、贴图、灯光、注释、文字点也不读。
//!
//! Rhino 是 Z 朝上，根节点绕 X 转 -90°。

use crate::{bad, limits, loader};
use kasset::{LoadError, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::Material;
use kmath::{Quat, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use std::{io::Read, path::PathBuf, sync::Arc};

/// [`Rhino3dm`] 的资源类型标识。
pub const RHINO_TYPE_UUID: Uuid = uuid!("3d0f5a7e-1c42-4b8e-9a6d-7e2b5c9f1a03");

/// 一个图层。
#[derive(Debug, Clone)]
pub struct Layer {
    /// 名字。
    pub name: String,
    /// 显示颜色（线性）。
    pub color: Vec4,
    /// 是否可见。
    pub visible: bool,
}

/// 一条曲线：所属图层、颜色、折线点（Rhino 坐标，Z 朝上）。
#[derive(Debug, Clone)]
pub struct Curve {
    /// 图层号（[`Rhino3dm::layers`] 的下标）。
    pub layer: usize,
    /// 颜色（线性）。
    pub color: Vec4,
    /// 折线。
    pub points: Vec<Vec3>,
}

/// 一个 3DM 文件的导入结果。
#[derive(Debug, Clone)]
pub struct Rhino3dm {
    /// 网格部分。根节点下每个图层一个子节点（顺序同 [`layers`](Self::layers)），物体挂在图层下。
    pub model: Model,
    /// 图层表。
    pub layers: Vec<Layer>,
    /// 曲线。
    pub curves: Vec<Curve>,
    /// 点云里的点：`(图层, 位置, 颜色)`。
    pub points: Vec<(usize, Vec3, Vec4)>,
}

impl ResourceData for Rhino3dm {
    fn type_uuid(&self) -> Uuid {
        RHINO_TYPE_UUID
    }
}

loader! {
    /// 读 `.3dm`，只要网格部分。
    Rhino3dmLoader -> Model : ["3dm"] = MODEL_TYPE_UUID, parse
}

loader! {
    /// 读 `.3dm`，带图层、曲线和点。和 [`Rhino3dmLoader`] 二选一注册。
    Rhino3dmSceneLoader -> Rhino3dm : ["3dm"] = RHINO_TYPE_UUID, parse_scene
}

// ── openNURBS 的分块 ──

const TCODE_SHORT: u32 = 0x8000_0000;
const TCODE_CRC: u32 = 0x8000;
const TCODE_LAYER_TABLE: u32 = 0x1000_0011;
const TCODE_OBJECT_TABLE: u32 = 0x1000_0013;
const TCODE_LAYER_RECORD: u32 = 0x2000_8050;
const TCODE_OBJECT_RECORD: u32 = 0x2000_8070;
const TCODE_OBJECT_RECORD_ATTRIBUTES: u32 = 0x0200_8072;
const TCODE_OPENNURBS_CLASS: u32 = 0x0002_7FFA;
const TCODE_OPENNURBS_CLASS_UUID: u32 = 0x0002_FFFB;
const TCODE_OPENNURBS_CLASS_DATA: u32 = 0x0002_FFFC;

fn class(text: &str) -> [u8; 16] {
    Uuid::parse_str(text).expect("常量写对了").to_bytes_le()
}

#[derive(Clone, Copy)]
struct Chunk {
    code: u32,
    start: usize,
    /// 数据终点，**不含**末尾的 CRC。
    end: usize,
}

struct Archive<'a> {
    data: &'a [u8],
    /// 分块长度的字节数：V5 之后是 8。
    wide: bool,
}

impl Archive<'_> {
    fn chunks(&self, start: usize, end: usize) -> Vec<Chunk> {
        let header = if self.wide { 12 } else { 8 };
        let mut out = Vec::new();
        let mut at = start;
        while at + header <= end.min(self.data.len()) {
            let code = u32::from_le_bytes(self.data[at..at + 4].try_into().expect("4"));
            let value = if self.wide {
                i64::from_le_bytes(self.data[at + 4..at + 12].try_into().expect("8"))
            } else {
                i32::from_le_bytes(self.data[at + 4..at + 8].try_into().expect("4")) as i64
            };
            if code & TCODE_SHORT != 0 {
                out.push(Chunk { code, start: at + header, end: at + header });
                at += header;
                continue;
            }
            if value < 0 || at + header + value as usize > end {
                break;
            }
            let body_end = at + header + value as usize;
            let crc = if code & TCODE_CRC != 0 { 4 } else { 0 };
            out.push(Chunk { code, start: at + header, end: body_end.saturating_sub(crc).max(at + header) });
            at = body_end;
        }
        out
    }

    /// 一个 `TCODE_OPENNURBS_CLASS` 块 → `(类 UUID, 数据块范围)`。
    fn class(&self, chunk: Chunk) -> Option<([u8; 16], usize, usize)> {
        let inner = self.chunks(chunk.start, chunk.end + 4);
        let id = inner.iter().find(|c| c.code == TCODE_OPENNURBS_CLASS_UUID)?;
        let data = inner.iter().find(|c| c.code == TCODE_OPENNURBS_CLASS_DATA)?;
        Some((self.data.get(id.start..id.start + 16)?.try_into().ok()?, data.start, data.end))
    }
}

/// 顺序读取的游标。越界一律报错。
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
    end: usize,
    wide: bool,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        if self.at + n > self.end {
            return Err(bad("3DM 对象数据被截断"));
        }
        let s = &self.data[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }
    fn i32(&mut self) -> Result<i32, LoadError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn f64(&mut self) -> Result<f64, LoadError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }
    fn skip(&mut self, n: usize) -> Result<(), LoadError> {
        self.take(n).map(|_| ())
    }
    fn count(&mut self, limit: usize) -> Result<usize, LoadError> {
        let n = self.i32()?;
        if n < 0 || n as usize > limit {
            return Err(bad("3DM 里的数量不合理"));
        }
        Ok(n as usize)
    }
    /// 跳过一个嵌套的大块（网格参数、曲率统计）。
    fn skip_chunk(&mut self) -> Result<(), LoadError> {
        self.skip(4)?;
        let length = if self.wide { i64::from_le_bytes(self.take(8)?.try_into().expect("8")) } else { self.i32()? as i64 };
        if length < 0 {
            return Err(bad("3DM 块长度为负"));
        }
        self.skip(length as usize)
    }
    /// `ON_wString`：UTF-16 元素数（含结尾 0）+ 元素。
    fn string(&mut self) -> Result<String, LoadError> {
        let n = self.count(1 << 20)?;
        let raw = self.take(n * 2)?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        Ok(String::from_utf16_lossy(&units))
    }
    /// `ON_Color`：`R | G<<8 | B<<16 | A<<24`，A 是**透明度**。
    fn color(&mut self) -> Result<Vec4, LoadError> {
        let b = self.take(4)?;
        let s = crate::amf::srgb_to_linear;
        Ok(Vec4::new(s(b[0] as f32 / 255.0), s(b[1] as f32 / 255.0), s(b[2] as f32 / 255.0), 1.0 - b[3] as f32 / 255.0))
    }
    /// `ReadCompressedBuffer`：CRC、方式（0 原样 / 1 zlib 块）、数据。
    fn compressed(&mut self, size: usize) -> Result<Vec<u8>, LoadError> {
        if size == 0 {
            return Ok(Vec::new());
        }
        if size > limits::VERTICES * 64 {
            return Err(bad("3DM 压缩缓冲过大"));
        }
        let _crc = self.i32()?;
        match self.u8()? {
            0 => Ok(self.take(size)?.to_vec()),
            1 => {
                self.skip(4)?;
                let length = if self.wide { i64::from_le_bytes(self.take(8)?.try_into().expect("8")) } else { self.i32()? as i64 };
                let body = self.take(length.max(0) as usize)?;
                let stream = &body[..body.len().saturating_sub(4)];
                let mut out = Vec::with_capacity(size);
                if flate2::read::ZlibDecoder::new(stream).take(size as u64).read_to_end(&mut out).is_err() || out.len() < size {
                    out.clear();
                    flate2::read::DeflateDecoder::new(stream)
                        .take(size as u64)
                        .read_to_end(&mut out)
                        .map_err(|e| bad(format!("3DM 压缩缓冲解压失败：{e}")))?;
                }
                if out.len() < size {
                    return Err(bad("3DM 压缩缓冲长度不足"));
                }
                Ok(out)
            }
            other => Err(bad(format!("3DM 未知的压缩方式 {other}"))),
        }
    }
}

/// `ON_Mesh::Read`（只读到顶点色为止，后面的字段画图用不到）。
fn read_mesh(c: &mut Cursor) -> Result<Mesh, LoadError> {
    let version = c.u8()?;
    let major = version >> 4;
    if major != 3 {
        return Err(bad(format!("3DM 网格版本 {major}.x 不支持（只读压缩格式 3.x）")));
    }
    let vcount = c.count(limits::VERTICES)?;
    let fcount = c.count(limits::VERTICES)?;
    c.skip(8 * 4 + 8 * 4 + 8 * 2)?; // 贴图 / 曲面定义域、曲面缩放
    c.skip(4 * 6 + 4 * 6 + 4 * 4)?; // 包围盒、法线盒、UV 盒
    c.skip(4)?; // 是否闭合
    for _ in 0..5 {
        // 网格参数 + 四份曲率统计，各自「有 / 无」一个字节，有的话是一个块。
        if c.u8()? != 0 {
            c.skip_chunk()?;
        }
    }
    let width = c.i32()?;
    let mut faces = Vec::with_capacity(fcount);
    for _ in 0..fcount {
        let mut vi = [0u32; 4];
        for v in &mut vi {
            *v = match width {
                1 => c.u8()? as u32,
                2 => u16::from_le_bytes(c.take(2)?.try_into().expect("2")) as u32,
                _ => c.i32()? as u32,
            };
        }
        faces.push(vi);
    }
    let mut buffer = |element: usize| -> Result<Vec<u8>, LoadError> {
        let size = c.i32()?.max(0) as usize;
        let data = c.compressed(size)?;
        Ok(if data.len() == vcount * element { data } else { Vec::new() })
    };
    let positions = buffer(12)?;
    let normals = buffer(12)?;
    let uvs = buffer(8)?;
    let _curvature = buffer(16)?;
    let colors = buffer(4)?;
    if positions.is_empty() {
        return Err(bad("3DM 网格没有顶点"));
    }
    let f = |b: &[u8], i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().expect("4"));
    let vertices = (0..vcount)
        .map(|i| Vertex {
            position: [f(&positions, i * 3), f(&positions, i * 3 + 1), f(&positions, i * 3 + 2)],
            normal: if normals.is_empty() { [0.0, 0.0, 1.0] } else { [f(&normals, i * 3), f(&normals, i * 3 + 1), f(&normals, i * 3 + 2)] },
            // Rhino 的 V 朝上。
            uv: if uvs.is_empty() { [0.0, 0.0] } else { [f(&uvs, i * 2), 1.0 - f(&uvs, i * 2 + 1)] },
            color: if colors.is_empty() {
                [1.0; 3]
            } else {
                let s = crate::amf::srgb_to_linear;
                [s(colors[i * 4] as f32 / 255.0), s(colors[i * 4 + 1] as f32 / 255.0), s(colors[i * 4 + 2] as f32 / 255.0)]
            },
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let mut indices = Vec::with_capacity(fcount * 6);
    for vi in faces {
        if vi.iter().any(|&v| v as usize >= vcount) {
            continue;
        }
        indices.extend_from_slice(&[vi[0], vi[1], vi[2]]);
        // 第三、四个下标相同表示三角形。
        if vi[2] != vi[3] {
            indices.extend_from_slice(&[vi[0], vi[2], vi[3]]);
        }
    }
    let mut mesh = Mesh::new(vertices, indices);
    if !mesh.is_valid() {
        return Err(bad("3DM 网格索引非法"));
    }
    if normals.is_empty() {
        mesh.recompute_normals();
    }
    Ok(mesh)
}

/// `ON_NurbsCurve::Read` → 折线。
fn read_nurbs_curve(c: &mut Cursor) -> Result<Vec<Vec3>, LoadError> {
    let _version = c.u8()?;
    let dim = c.i32()?.clamp(1, 3) as usize;
    let rational = c.i32()? != 0;
    let order = c.i32()?.max(2) as usize;
    let cv_count = c.count(1 << 20)?;
    c.skip(8 + 48)?; // 两个保留整数 + 包围盒
    let knot_count = c.count(1 << 22)?;
    let mut knots: Vec<f64> = (0..knot_count).map(|_| c.f64()).collect::<Result<_, _>>()?;
    let _cv_array = c.i32()?;
    let stride = dim + rational as usize;
    let mut points = Vec::with_capacity(cv_count);
    for _ in 0..cv_count {
        let v: Vec<f64> = (0..stride).map(|_| c.f64()).collect::<Result<_, _>>()?;
        // openNURBS 存的是齐次坐标（x·w, y·w, z·w, w）。
        let w = if rational { v[dim] } else { 1.0 };
        let w = if w.abs() < 1e-12 { 1.0 } else { w };
        let g = |k: usize| if k < dim { v[k] / w } else { 0.0 };
        points.push([g(0), g(1), g(2), w]);
    }
    if points.len() < 2 || knots.is_empty() {
        return Ok(Vec::new());
    }
    // openNURBS 的结点向量少两端各一个「多余」结点，补上成标准形式。
    knots.insert(0, knots[0]);
    knots.push(*knots.last().expect("非空"));
    let degree = order - 1;
    let (u0, u1) = (knots[degree], knots[knots.len() - 1 - degree]);
    let samples = if degree == 1 { cv_count - 1 } else { (cv_count * 12).min(4096) };
    Ok((0..=samples)
        .map(|k| {
            let u = u0 + (u1 - u0) * k as f64 / samples.max(1) as f64;
            let p = crate::nurbs::de_boor(degree, &knots, &points, u);
            Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
        })
        .collect())
}

/// `ON_3dmObjectAttributes::Internal_ReadV5` 的前几项：`(图层, 名字, 颜色, 颜色来源, 可见)`。
fn read_attributes(c: &mut Cursor) -> Result<(usize, String, Option<Vec4>, u8, bool), LoadError> {
    let version = c.u8()?;
    if version >> 4 != 2 {
        return Ok((0, String::new(), None, 0, true));
    }
    c.skip(16)?;
    let layer = c.i32()?.max(0) as usize;
    let (mut name, mut color, mut source, mut visible) = (String::new(), None, 0u8, true);
    let mut item = c.u8()?;
    // 各项按编号递增出现，只写非默认值；读到 0 结束。遇到不认识的就停。
    while item != 0 {
        match item {
            1 => name = c.string()?,
            2 => {
                c.string()?;
            }
            3 | 4 | 10 => c.skip(4)?,
            6 => color = Some(c.color()?),
            7 => c.skip(4)?,
            8 => c.skip(8)?,
            9 => c.skip(1)?,
            11 => visible = c.u8()? != 0,
            12 => {
                // 1 = 隐藏。
                if c.u8()? == 1 {
                    visible = false;
                }
            }
            13 => source = c.u8()?,
            _ => break,
        }
        item = c.u8()?;
    }
    Ok((layer, name, color, source, visible))
}

/// `ON_Layer::Read`。
fn read_layer(c: &mut Cursor) -> Result<Layer, LoadError> {
    let version = c.u8()?;
    if version >> 4 != 1 {
        return Err(bad("3DM 图层版本不认识"));
    }
    let mode = c.i32()?;
    c.skip(4 * 4)?; // 图层号、IGES 层、材质号、废弃值
    let color = c.color()?;
    c.skip(2 + 2 + 8 + 8)?; // 废弃的线型
    let name = c.string()?;
    let mut visible = mode != 1;
    if version & 0x0f >= 1 {
        visible &= c.u8()? != 0;
    }
    Ok(Layer { name, color: Vec4::new(color.x, color.y, color.z, 1.0), visible })
}

/// 在一段字节里找嵌套的 `ON_Mesh` 类块（Brep / SubD 的缓存显示网格）。
fn find_cached_mesh(archive: &Archive, start: usize, end: usize) -> Option<Mesh> {
    let mesh_class = class("4ED7D4E4-E947-11d3-BFE5-0010830122F0");
    let header = if archive.wide { 12 } else { 8 };
    let region = &archive.data[start..end];
    let mut from = 0;
    while let Some(offset) = region[from..].windows(16).position(|w| w == mesh_class) {
        let at = start + from + offset;
        from += offset + 16;
        // UUID 块头在它前面 `header` 字节，类块头再往前 `header` 字节。
        let (Some(uuid_header), Some(class_header)) = (at.checked_sub(header), at.checked_sub(2 * header)) else { continue };
        let code = |p: usize| u32::from_le_bytes(archive.data[p..p + 4].try_into().expect("4"));
        if code(uuid_header) != TCODE_OPENNURBS_CLASS_UUID || code(class_header) != TCODE_OPENNURBS_CLASS {
            continue;
        }
        let Some(chunk) = archive.chunks(class_header, end).first().copied() else { continue };
        let Some((_, data_start, data_end)) = archive.class(chunk) else { continue };
        let mut cursor = Cursor { data: archive.data, at: data_start, end: data_end, wide: archive.wide };
        if let Ok(mesh) = read_mesh(&mut cursor) {
            return Some(mesh);
        }
    }
    None
}

/// 解析成 [`Model`]。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    Ok(parse_scene(bytes, path, io).await?.model)
}

/// 解析成 [`Rhino3dm`]。
pub async fn parse_scene(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Rhino3dm, LoadError> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Rhino".into());
    parse_bytes(&bytes, &name)
}

/// 从字节解析。
pub fn parse_bytes(bytes: &[u8], name: &str) -> Result<Rhino3dm, LoadError> {
    if bytes.len() < 32 || !bytes.starts_with(b"3D Geometry File Format ") {
        return Err(bad("不是 3DM 文件"));
    }
    let version: u32 = std::str::from_utf8(&bytes[24..32]).ok().and_then(|s| s.trim().parse().ok()).ok_or_else(|| bad("3DM 版本号读不出来"))?;
    let archive = Archive { data: bytes, wide: version >= 50 };
    let top = archive.chunks(32, bytes.len());

    let mut layers = Vec::new();
    for table in top.iter().filter(|c| c.code == TCODE_LAYER_TABLE) {
        for record in archive.chunks(table.start, table.end + 4).into_iter().filter(|c| c.code == TCODE_LAYER_RECORD) {
            for chunk in archive.chunks(record.start, record.end).into_iter().filter(|c| c.code == TCODE_OPENNURBS_CLASS) {
                if let Some((_, start, end)) = archive.class(chunk) {
                    let mut cursor = Cursor { data: bytes, at: start, end, wide: archive.wide };
                    match read_layer(&mut cursor) {
                        Ok(layer) => layers.push(layer),
                        Err(error) => klog::warn!("3DM 图层读不出来：{error}"),
                    }
                }
            }
        }
    }
    if layers.is_empty() {
        layers.push(Layer { name: "Default".into(), color: Vec4::ONE, visible: true });
    }

    let mesh_class = class("4ED7D4E4-E947-11d3-BFE5-0010830122F0");
    let nurbs_curves = [class("4ED7D4DD-E947-11d3-BFE5-0010830122F0"), class("5EAF1119-0B51-11d4-BFFE-0010830122F0"), class("76A709D5-1550-11d4-8000-0010830122F0")];
    let line_curve = class("4ED7D4DB-E947-11d3-BFE5-0010830122F0");
    let polyline_curve = class("4ED7D4E6-E947-11d3-BFE5-0010830122F0");
    let poly_curve = class("4ED7D4E0-E947-11d3-BFE5-0010830122F0");
    let point_cloud = class("2488F347-F8FA-11d3-BFEC-0010830122F0");
    let subd_class = class("F09BA4D9-455B-42C3-BA3B-E6CCACEF853B");

    let mut meshes = Vec::new();
    let mut materials = Vec::new();
    let mut nodes = vec![ModelNode {
        name: name.to_string(),
        transform: NodeTransform { rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2), ..Default::default() },
        children: (1..=layers.len()).collect(),
        ..Default::default()
    }];
    for layer in &layers {
        nodes.push(ModelNode { name: layer.name.clone(), ..Default::default() });
    }
    let mut curves = Vec::new();
    let mut points = Vec::new();
    let mut skipped = 0usize;
    let mut vertex_total = 0usize;

    for table in top.iter().filter(|c| c.code == TCODE_OBJECT_TABLE) {
        for record in archive.chunks(table.start, table.end + 4).into_iter().filter(|c| c.code == TCODE_OBJECT_RECORD) {
            let inner = archive.chunks(record.start, record.end);
            let (layer, object_name, object_color, source, visible) = inner
                .iter()
                .find(|c| c.code == TCODE_OBJECT_RECORD_ATTRIBUTES)
                .and_then(|c| read_attributes(&mut Cursor { data: bytes, at: c.start, end: c.end, wide: archive.wide }).ok())
                .unwrap_or((0, String::new(), None, 0, true));
            if !visible {
                continue;
            }
            let layer = layer.min(layers.len() - 1);
            let color = if source == 1 { object_color.unwrap_or(layers[layer].color) } else { layers[layer].color };
            let Some(chunk) = inner.iter().find(|c| c.code == TCODE_OPENNURBS_CLASS) else { continue };
            let Some((id, start, end)) = archive.class(*chunk) else { continue };
            let mut cursor = Cursor { data: bytes, at: start, end, wide: archive.wide };

            let mesh = if id == mesh_class {
                read_mesh(&mut cursor).ok()
            } else if nurbs_curves.contains(&id) || id == line_curve || id == polyline_curve || id == poly_curve {
                let polyline = if nurbs_curves.contains(&id) {
                    read_nurbs_curve(&mut cursor).unwrap_or_default()
                } else if id == line_curve {
                    (|| -> Result<Vec<Vec3>, LoadError> {
                        cursor.u8()?;
                        let v: Vec<f64> = (0..6).map(|_| cursor.f64()).collect::<Result<_, _>>()?;
                        Ok(vec![Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32), Vec3::new(v[3] as f32, v[4] as f32, v[5] as f32)])
                    })()
                    .unwrap_or_default()
                } else if id == polyline_curve {
                    (|| -> Result<Vec<Vec3>, LoadError> {
                        cursor.u8()?;
                        let n = cursor.count(1 << 22)?;
                        (0..n).map(|_| Ok(Vec3::new(cursor.f64()? as f32, cursor.f64()? as f32, cursor.f64()? as f32))).collect()
                    })()
                    .unwrap_or_default()
                } else {
                    // ON_PolyCurve：段是嵌套的曲线对象，逐个找出 NURBS 段拼起来。
                    let mut all = Vec::new();
                    for segment in archive.chunks(start, end).into_iter().filter(|c| c.code == TCODE_OPENNURBS_CLASS) {
                        if let Some((sid, s0, s1)) = archive.class(segment)
                            && nurbs_curves.contains(&sid)
                        {
                            all.extend(read_nurbs_curve(&mut Cursor { data: bytes, at: s0, end: s1, wide: archive.wide }).unwrap_or_default());
                        }
                    }
                    all
                };
                if polyline.len() > 1 {
                    curves.push(Curve { layer, color, points: polyline });
                }
                continue;
            } else if id == point_cloud {
                let read = (|| -> Result<Vec<(Vec3, Vec4)>, LoadError> {
                    cursor.u8()?;
                    let n = cursor.count(limits::VERTICES)?;
                    let positions: Vec<Vec3> = (0..n).map(|_| Ok(Vec3::new(cursor.f64()? as f32, cursor.f64()? as f32, cursor.f64()? as f32))).collect::<Result<_, LoadError>>()?;
                    cursor.skip(16 * 8 + 48 + 4)?; // 平面、包围盒、标志
                    let normals = cursor.count(limits::VERTICES)?;
                    cursor.skip(normals * 24)?;
                    let colors = cursor.count(limits::VERTICES)?;
                    let colors: Vec<Vec4> = (0..colors).map(|_| cursor.color()).collect::<Result<_, _>>()?;
                    Ok(positions.into_iter().enumerate().map(|(i, p)| (p, colors.get(i).copied().unwrap_or(color))).collect())
                })();
                for (p, c) in read.unwrap_or_default() {
                    points.push((layer, p, Vec4::new(c.x, c.y, c.z, 1.0)));
                }
                continue;
            } else if id == subd_class {
                // SubD：先找缓存网格，没有就自己从控制网格细分。
                let found = find_cached_mesh(&archive, start, end).or_else(|| match read_subd(&mut cursor, version) {
                    Ok(net) => subd_mesh(net, 3),
                    Err(error) => {
                        klog::warn!("3DM SubD 读不出来：{error}");
                        None
                    }
                });
                if found.is_none() {
                    skipped += 1;
                }
                found
            } else {
                // Brep / Extrusion：用它们缓存的显示网格。
                let found = find_cached_mesh(&archive, start, end);
                if found.is_none() {
                    skipped += 1;
                }
                found
            };
            let Some(mesh) = mesh else { continue };
            vertex_total += mesh.vertices().len();
            if vertex_total > limits::VERTICES {
                return Err(bad("3DM 顶点数超过上限"));
            }
            let has_colors = mesh.vertices().iter().any(|v| v.color != [1.0; 3]);
            let mut material = Material::standard()
                .with_base_color(if has_colors { Vec4::ONE } else { color })
                .with_roughness(0.45)
                .with_metallic(0.0);
            material.set_double_sided(true);
            if color.w < 0.999 {
                material.set_blend_mode(kmaterial::BlendMode::Alpha);
            }
            let node = nodes.len();
            nodes.push(ModelNode {
                name: if object_name.is_empty() { format!("Object{node}") } else { object_name },
                parts: vec![MeshPart { mesh: meshes.len(), material: Some(materials.len()) }],
                ..Default::default()
            });
            nodes[1 + layer].children.push(node);
            meshes.push(mesh);
            materials.push(material);
        }
    }
    if skipped > 0 {
        klog::warn!("3DM：{skipped} 个 Brep / SubD 没有缓存网格，画不出来（需要曲面求值）");
    }
    if meshes.is_empty() && curves.is_empty() && points.is_empty() {
        return Err(bad("3DM 里没有能画的东西"));
    }
    Ok(Rhino3dm { model: Model::new(meshes, materials, nodes, vec![0]), layers, curves, points })
}

// ───────────────────────────── SubD ─────────────────────────────

/// SubD 的第 0 层控制网格。
#[derive(Debug, Default)]
struct ControlNet {
    positions: Vec<Vec3>,
    faces: Vec<Vec<usize>>,
    creases: std::collections::HashSet<(usize, usize)>,
    corners: std::collections::HashSet<usize>,
}

fn edge_key(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

impl Cursor<'_> {
    fn u16(&mut self) -> Result<u16, LoadError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("2")))
    }
    fn u32(&mut self) -> Result<u32, LoadError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn vec3(&mut self) -> Result<Vec3, LoadError> {
        Ok(Vec3::new(self.f64()? as f32, self.f64()? as f32, self.f64()? as f32))
    }
    /// 带版本号的块头：`typecode + 长度 + major(i32) + minor(i32)`。
    fn versioned_chunk(&mut self) -> Result<(i32, i32), LoadError> {
        self.skip(4 + if self.wide { 8 } else { 4 })?;
        Ok((self.i32()?, self.i32()?))
    }
    /// 「附加字段」结尾：逐个跳过，直到 255。
    fn finish_additions(&mut self) -> Result<(), LoadError> {
        loop {
            match self.u8()? {
                255 => return Ok(()),
                254 => self.skip_chunk()?,
                size => self.skip(size as usize)?,
            }
        }
    }
    /// 一个附加字段：`Some(true)` 有数据、`Some(false)` 空、`None` 读到了结尾。
    fn addition(&mut self, expected: u8) -> Result<Option<bool>, LoadError> {
        match self.u8()? {
            255 => Ok(None),
            0 => Ok(Some(false)),
            s if s == expected => Ok(Some(true)),
            other => Err(bad(format!("3DM SubD 附加字段长度 {other} 不对"))),
        }
    }
    /// 组件公共部分（v7 起的格式）：档案号、id、层号 + 三个附加字段。
    fn subd_base(&mut self) -> Result<(), LoadError> {
        self.skip(4 + 4 + 2)?;
        for size in [24u8, 4, 5] {
            match self.addition(size)? {
                None => return Ok(()),
                Some(true) => self.skip(size as usize)?,
                Some(false) => {}
            }
        }
        self.finish_additions()
    }
    /// `(档案号, 标志)` 列表。
    fn subd_ptrs(&mut self) -> Result<Vec<(u32, u8)>, LoadError> {
        let n = self.u16()?;
        (0..n).map(|_| Ok((self.u32()?, self.u8()?))).collect()
    }
}

/// `ON_SubD::Read`，只取第 0 层（控制网格）。
fn read_subd(c: &mut Cursor, version: u32) -> Result<ControlNet, LoadError> {
    if version < 70 {
        return Err(bad("只读 Rhino 7 及以后的 SubD 存档"));
    }
    if c.u8()? != 1 {
        return Err(bad("空的 SubD"));
    }
    let (major, _) = c.versioned_chunk()?;
    if major != 1 {
        return Err(bad("SubD 版本不认识"));
    }
    if c.u32()? == 0 {
        return Err(bad("SubD 没有层"));
    }
    c.skip(12 + 48)?;
    let (major, _) = c.versioned_chunk()?;
    if major != 1 {
        return Err(bad("SubD 层版本不认识"));
    }
    c.skip(2 + 3 + 48)?;
    let partition = [c.u32()?, c.u32()?, c.u32()?, c.u32()?];
    let vertex_count = partition[1].saturating_sub(partition[0]) as usize;
    let edge_count = partition[2].saturating_sub(partition[1]) as usize;
    let face_count = partition[3].saturating_sub(partition[2]) as usize;
    if vertex_count + edge_count + face_count > limits::VERTICES {
        return Err(bad("SubD 太大"));
    }
    let mut net = ControlNet::default();
    for _ in 0..vertex_count {
        c.subd_base()?;
        let tag = c.u8()?;
        let p = c.vec3()?;
        c.skip(4)?; // 边数、面数（下面的列表自己带长度）
        if c.u8()? != 0 {
            // 旧版本存的极限点，读了也不用。
            let n = c.u32()? as usize;
            c.skip(n * (12 * 8 + 5))?;
        }
        c.subd_ptrs()?;
        c.subd_ptrs()?;
        c.finish_additions()?;
        if tag == 3 {
            net.corners.insert(net.positions.len());
        }
        net.positions.push(p);
    }
    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        c.subd_base()?;
        let tag = c.u8()?;
        c.skip(2 + 16 + 8)?;
        let v = c.subd_ptrs()?;
        c.subd_ptrs()?;
        if version >= 80 {
            match c.u8()? {
                255 => {}
                8 => {
                    c.skip(8)?;
                    c.finish_additions()?;
                }
                _ => return Err(bad("SubD 边的第二锐度字段不对")),
            }
        } else {
            c.finish_additions()?;
        }
        let index = |p: &(u32, u8)| p.0.saturating_sub(partition[0]) as usize;
        let (a, b) = (v.first().map(index).unwrap_or(0), v.get(1).map(index).unwrap_or(0));
        if tag == 2 {
            net.creases.insert(edge_key(a, b));
        }
        edges.push((a, b));
    }
    for _ in 0..face_count {
        c.subd_base()?;
        c.skip(4 + 4 + 2)?; // 第 0 层面号、废弃的父面号、边数（列表自己还带一个）
        let list = c.subd_ptrs()?;
        let loop_vertices: Vec<usize> = list
            .iter()
            .filter_map(|&(id, flags)| {
                let (a, b) = *edges.get(id.checked_sub(partition[1])? as usize)?;
                // 方向位为 1 表示这条边在面上是反着用的。
                Some(if flags & 1 == 0 { a } else { b })
            })
            .collect();
        // 附加字段：纹理域（34）、材质通道、逐面颜色、打包号、自定义贴图点。
        let mut done = false;
        for size in [34u8, 4, 4, 4] {
            match c.addition(size)? {
                None => {
                    done = true;
                    break;
                }
                Some(true) => c.skip(size as usize)?,
                Some(false) => {}
            }
        }
        if !done {
            match c.addition(4)? {
                None => done = true,
                Some(true) => {
                    let chunks = c.u32()? as usize;
                    for _ in 0..chunks {
                        c.skip(1 + 240)?;
                    }
                    let left = list.len() % 10;
                    if left > 0 {
                        c.skip(1 + left * 24)?;
                    }
                }
                Some(false) => {}
            }
        }
        if !done {
            c.finish_additions()?;
        }
        if loop_vertices.len() >= 3 && loop_vertices.iter().all(|&v| v < net.positions.len()) {
            net.faces.push(loop_vertices);
        }
    }
    Ok(net)
}

/// 一次 Catmull–Clark 细分（带折边与角点）。
fn catmull_clark(net: &ControlNet) -> ControlNet {
    use std::collections::HashMap;
    let n = net.positions.len();
    let mut edge_index: HashMap<(usize, usize), usize> = HashMap::new();
    let mut edges: Vec<((usize, usize), Vec<usize>)> = Vec::new();
    for (f, face) in net.faces.iter().enumerate() {
        for k in 0..face.len() {
            let key = edge_key(face[k], face[(k + 1) % face.len()]);
            let e = *edge_index.entry(key).or_insert_with(|| {
                edges.push((key, Vec::new()));
                edges.len() - 1
            });
            edges[e].1.push(f);
        }
    }
    let crease = |e: &((usize, usize), Vec<usize>)| e.1.len() != 2 || net.creases.contains(&e.0);
    let face_points: Vec<Vec3> = net
        .faces
        .iter()
        .map(|f| f.iter().map(|&v| net.positions[v]).sum::<Vec3>() / f.len() as f32)
        .collect();
    let edge_points: Vec<Vec3> = edges
        .iter()
        .map(|e| {
            let (a, b) = e.0;
            if crease(e) {
                (net.positions[a] + net.positions[b]) * 0.5
            } else {
                (net.positions[a] + net.positions[b] + face_points[e.1[0]] + face_points[e.1[1]]) * 0.25
            }
        })
        .collect();

    let mut vertex_faces = vec![Vec::new(); n];
    for (f, face) in net.faces.iter().enumerate() {
        for &v in face {
            vertex_faces[v].push(f);
        }
    }
    let mut vertex_edges = vec![Vec::new(); n];
    for (e, edge) in edges.iter().enumerate() {
        vertex_edges[edge.0 .0].push(e);
        vertex_edges[edge.0 .1].push(e);
    }
    let mut positions: Vec<Vec3> = (0..n)
        .map(|v| {
            let p = net.positions[v];
            if net.corners.contains(&v) || vertex_edges[v].is_empty() {
                return p;
            }
            let sharp: Vec<usize> = vertex_edges[v].iter().copied().filter(|&e| crease(&edges[e])).collect();
            match sharp.len() {
                // 光滑点与 dart（只有一条折边）：标准规则。
                0 | 1 => {
                    let k = vertex_edges[v].len() as f32;
                    let f = vertex_faces[v].iter().map(|&f| face_points[f]).sum::<Vec3>() / vertex_faces[v].len().max(1) as f32;
                    let r = vertex_edges[v]
                        .iter()
                        .map(|&e| (net.positions[edges[e].0 .0] + net.positions[edges[e].0 .1]) * 0.5)
                        .sum::<Vec3>()
                        / k;
                    (f + r * 2.0 + p * (k - 3.0)) / k
                }
                // 折边上的点：沿折线做 1-6-1 的三次 B 样条细分。
                2 => {
                    let other = |e: usize| if edges[e].0 .0 == v { edges[e].0 .1 } else { edges[e].0 .0 };
                    (p * 6.0 + net.positions[other(sharp[0])] + net.positions[other(sharp[1])]) / 8.0
                }
                // 三条以上折边汇聚：当角点。
                _ => p,
            }
        })
        .collect();
    let face_base = positions.len();
    positions.extend(face_points);
    let edge_base = positions.len();
    positions.extend(edge_points);

    let mut out = ControlNet {
        positions,
        corners: net.corners.clone(),
        ..Default::default()
    };
    for (e, edge) in edges.iter().enumerate() {
        if crease(edge) {
            out.creases.insert(edge_key(edge.0 .0, edge_base + e));
            out.creases.insert(edge_key(edge_base + e, edge.0 .1));
        }
    }
    for (f, face) in net.faces.iter().enumerate() {
        let m = face.len();
        for k in 0..m {
            let prev = edge_index[&edge_key(face[(k + m - 1) % m], face[k])];
            let next = edge_index[&edge_key(face[k], face[(k + 1) % m])];
            out.faces.push(vec![face[k], edge_base + next, face_base + f, edge_base + prev]);
        }
    }
    out
}

/// SubD 控制网格细分 `levels` 次后的网格。
fn subd_mesh(mut net: ControlNet, levels: usize) -> Option<Mesh> {
    for _ in 0..levels {
        if net.faces.len() * 4 > limits::VERTICES / 8 {
            break;
        }
        net = catmull_clark(&net);
    }
    let vertices = net.positions.iter().map(|p| Vertex { position: p.to_array(), ..Default::default() }).collect();
    let mut indices = Vec::new();
    for face in &net.faces {
        for k in 1..face.len() - 1 {
            indices.extend_from_slice(&[face[0] as u32, face[k] as u32, face[k + 1] as u32]);
        }
    }
    let mut mesh = Mesh::new(vertices, indices);
    if !mesh.is_valid() || mesh.triangle_count() == 0 {
        return None;
    }
    mesh.recompute_normals();
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdividing_a_cube_keeps_it_closed_and_rounds_it() {
        let mut net = ControlNet::default();
        for i in 0..8 {
            net.positions.push(Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32) * 2.0 - Vec3::ONE);
        }
        net.faces = vec![vec![0, 2, 3, 1], vec![4, 5, 7, 6], vec![0, 1, 5, 4], vec![2, 6, 7, 3], vec![0, 4, 6, 2], vec![1, 3, 7, 5]];
        let once = catmull_clark(&net);
        assert_eq!(once.faces.len(), 24);
        assert_eq!(once.positions.len(), 8 + 6 + 12);
        // 光滑细分会把角往里收：原来的角点 (1,1,1) 移到了 5/9。
        let corner = once.positions[7];
        assert!((corner - Vec3::splat(5.0 / 9.0)).length() < 1e-5, "{corner:?}");
        // 标成角点就不动。
        net.corners.insert(7);
        assert_eq!(catmull_clark(&net).positions[7], Vec3::ONE);
    }
}
