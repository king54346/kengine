//! Quake II 的 MD2：逐帧顶点动画的角色模型。
//!
//! # 支持
//!
//! 顶点帧、纹理坐标、三角形、皮肤名单，以及按帧名前缀切出来的动画片段。
//!
//! # 顶点动画，不是骨骼动画
//!
//! MD2 没有骨骼：每一帧都是一整套顶点坐标（压成每轴一个字节 + 一组
//! 缩放平移还原）。播放就是在相邻两帧之间线性插值。这和引擎里已有的
//! 蒙皮动画是两条完全不同的路——那条路每帧算骨骼矩阵，这条路每帧直接
//! 改顶点缓冲（[`Md2::pose`]）。
//!
//! MD2 的模型只有几百个顶点，每帧在 CPU 上重写一遍顶点缓冲完全不是问题；
//! 真正的代价在显存上传，而引擎的 [`Mesh`] 在独占几何时是原地覆写、
//! 只 bump 版本号，不会每帧新建缓冲。
//!
//! # 法线是 162 个方向的量化值
//!
//! MD2 的每个顶点存的不是法线向量，而是一张 162 项的固定方向表里的下标
//! （Quake 的 `anorms.h`）。表原样抄在 [`NORMALS`] 里。量化到 162 个方向
//! 的后果是光照在曲面上会有轻微的台阶感，这是格式自带的，不是实现问题。
//!
//! # 绕序靠法线定，不靠猜
//!
//! Y 轴朝上的转换要把 y 和 z 对调，那是一次**镜像**，会让三角形的绕序
//! 反过来。这里不写死「一定要反过来」，而是拿第一帧算出来的面法线和
//! 文件自带的顶点法线对一下点积，为负才翻——写死的话，换一个导出器
//! 写的 MD2 就可能整个翻到里面去，而且只表现为「模型看不见」。

use crate::{bad, loader};
use kasset::{LoadError, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kmath::{Vec2, Vec3};
use kmesh::{Mesh, Vertex};
use std::{path::PathBuf, sync::Arc};

/// [`Md2`] 的资源类型标识。
pub const MD2_TYPE_UUID: Uuid = uuid!("1f0a9c3e-7b52-4a61-9d84-3e5c6a20b7f1");

loader! {
    /// 读 `.md2`。
    Md2Loader -> Md2 : ["md2"] = MD2_TYPE_UUID, parse
}

/// 一帧的顶点坐标与法线，已经换算到引擎坐标系。
#[derive(Debug, Clone)]
pub struct Frame {
    /// 帧名，例如 `stand01`。
    pub name: String,
    /// 每个顶点的位置。
    pub positions: Vec<Vec3>,
    /// 每个顶点的法线。
    pub normals: Vec<Vec3>,
}

/// 一段动画：帧名前缀相同的一串连续帧。
#[derive(Debug, Clone)]
pub struct Animation {
    /// 动画名（帧名去掉末尾数字）。
    pub name: String,
    /// 起始帧号。
    pub start: usize,
    /// 结束帧号（含）。
    pub end: usize,
}

impl Animation {
    /// 帧数。至少为 1。
    pub fn len(&self) -> usize {
        self.end - self.start + 1
    }

    /// 恒为 `false`：一段动画至少有一帧。有它只是为了配 `len`。
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// 一个 MD2 模型。
#[derive(Debug)]
pub struct Md2 {
    /// 三角形索引，指向下面那套展开后的顶点。
    pub indices: Vec<u32>,
    /// 每个展开顶点对应的**帧内顶点号**。
    ///
    /// MD2 的位置索引和 UV 索引是两套，展开成 GPU 顶点后要记住每个顶点
    /// 原本取的是哪个位置，[`Md2::pose`] 靠它把帧数据搬进网格。
    pub vertex_map: Vec<u32>,
    /// 每个展开顶点的纹理坐标。
    pub uvs: Vec<Vec2>,
    /// 所有帧。
    pub frames: Vec<Frame>,
    /// 所有动画片段。
    pub animations: Vec<Animation>,
    /// 文件里登记的皮肤名单（相对路径，可能指向不存在的文件）。
    pub skins: Vec<String>,
}

impl ResourceData for Md2 {
    fn type_uuid(&self) -> Uuid {
        MD2_TYPE_UUID
    }
}

impl Md2 {
    /// 用第一帧建一份可渲染的几何。
    pub fn to_mesh(&self) -> Mesh {
        let frame = &self.frames[0];
        let vertices = self
            .vertex_map
            .iter()
            .zip(&self.uvs)
            .map(|(&source, uv)| Vertex {
                position: frame.positions[source as usize].to_array(),
                normal: frame.normals[source as usize].to_array(),
                uv: uv.to_array(),
                ..Default::default()
            })
            .collect();
        let mut mesh = Mesh::new(vertices, self.indices.clone());
        mesh.recompute_tangents();
        mesh
    }

    /// 把 `a`、`b` 两帧按 `t` 插值写进网格。
    ///
    /// `t` 会被夹到 `[0, 1]`，帧号越界时夹到最后一帧而不是 panic——
    /// 播放循环里一个 off-by-one 让整个游戏崩掉是不可接受的。
    pub fn pose(&self, mesh: &mut Mesh, a: usize, b: usize, t: f32) {
        let last = self.frames.len() - 1;
        let (first, second) = (&self.frames[a.min(last)], &self.frames[b.min(last)]);
        let t = t.clamp(0.0, 1.0);
        // `vertices_mut` 借走了网格，`vertex_map` 在 self 上，两个借用不冲突，
        // 但要先把切片拿出来，免得闭包里同时借 self 和 mesh。
        let map = &self.vertex_map;
        for (vertex, &source) in mesh.vertices_mut().iter_mut().zip(map) {
            let source = source as usize;
            vertex.position = first.positions[source]
                .lerp(second.positions[source], t)
                .to_array();
            // 两个单位向量的线性插值不再是单位向量，必须重新归一化，
            // 否则插值中段的光照会整体偏暗。
            vertex.normal = first.normals[source]
                .lerp(second.normals[source], t)
                .normalize_or_zero()
                .to_array();
        }
        mesh.recompute_bounds();
    }

    /// 按名字找动画。
    pub fn find_animation(&self, name: &str) -> Option<&Animation> {
        self.animations.iter().find(|a| a.name == name)
    }
}

/// 解析 MD2。
pub async fn parse(
    bytes: Vec<u8>,
    _path: PathBuf,
    _io: Arc<dyn ResourceIo>,
) -> Result<Md2, LoadError> {
    if bytes.len() < 68 || &bytes[..4] != b"IDP2" {
        return Err(bad("不是 MD2 文件"));
    }
    let field =
        |index: usize| -> i32 { i32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap()) };
    if field(1) != 8 {
        return Err(bad("只支持 MD2 版本 8"));
    }
    let (skin_width, skin_height) = (field(2) as f32, field(3) as f32);
    let count = |index: usize| -> Result<usize, LoadError> {
        let value = field(index);
        if !(0..=1 << 22).contains(&value) {
            return Err(bad("MD2 的计数字段不合法"));
        }
        Ok(value as usize)
    };
    let (num_skins, num_vertices, num_st, num_tris, num_frames) =
        (count(5)?, count(6)?, count(7)?, count(8)?, count(10)?);
    let offset = |index: usize| -> Result<usize, LoadError> {
        let value = field(index);
        if value < 0 || value as usize > bytes.len() {
            return Err(bad("MD2 的偏移字段越界"));
        }
        Ok(value as usize)
    };
    let (offset_skins, offset_st, offset_tris, offset_frames) =
        (offset(11)?, offset(12)?, offset(13)?, offset(14)?);
    if num_frames == 0 || num_vertices == 0 || num_tris == 0 {
        return Err(bad("MD2 里没有几何或帧"));
    }

    let read_u16 = |at: usize| -> Result<u16, LoadError> {
        Ok(u16::from_le_bytes(
            bytes
                .get(at..at + 2)
                .ok_or_else(|| bad("MD2 被截断"))?
                .try_into()
                .unwrap(),
        ))
    };
    let read_f32 = |at: usize| -> Result<f32, LoadError> {
        Ok(f32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(|| bad("MD2 被截断"))?
                .try_into()
                .unwrap(),
        ))
    };

    // ── 皮肤名单 ──
    let skins = (0..num_skins)
        .map(|index| {
            let at = offset_skins + index * 64;
            let raw = bytes.get(at..at + 64).unwrap_or(&[]);
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).into_owned()
        })
        .collect();

    // ── 纹理坐标表 ──
    let mut texcoords = Vec::with_capacity(num_st);
    for index in 0..num_st {
        let at = offset_st + index * 4;
        let u = read_u16(at)? as i16 as f32 / skin_width.max(1.0);
        let v = read_u16(at + 2)? as i16 as f32 / skin_height.max(1.0);
        texcoords.push(Vec2::new(u, 1.0 - v));
    }

    // ── 三角形：位置索引与 UV 索引是两套，展开成一套 GPU 顶点 ──
    let mut vertex_map = Vec::with_capacity(num_tris * 3);
    let mut uvs = Vec::with_capacity(num_tris * 3);
    for index in 0..num_tris {
        let at = offset_tris + index * 12;
        for corner in 0..3 {
            let position = read_u16(at + corner * 2)? as usize;
            let texcoord = read_u16(at + 6 + corner * 2)? as usize;
            if position >= num_vertices {
                return Err(bad("MD2 的顶点索引越界"));
            }
            vertex_map.push(position as u32);
            uvs.push(texcoords.get(texcoord).copied().unwrap_or(Vec2::ZERO));
        }
    }

    // ── 帧 ──
    let frame_size = 40 + num_vertices * 4;
    let mut frames = Vec::with_capacity(num_frames);
    for index in 0..num_frames {
        let base = offset_frames + index * frame_size;
        let scale = Vec3::new(read_f32(base)?, read_f32(base + 4)?, read_f32(base + 8)?);
        let translate = Vec3::new(
            read_f32(base + 12)?,
            read_f32(base + 16)?,
            read_f32(base + 20)?,
        );
        let raw = bytes
            .get(base + 24..base + 40)
            .ok_or_else(|| bad("MD2 的帧名被截断"))?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name = String::from_utf8_lossy(&raw[..end]).into_owned();

        let mut positions = Vec::with_capacity(num_vertices);
        let mut normals = Vec::with_capacity(num_vertices);
        for vertex in 0..num_vertices {
            let at = base + 40 + vertex * 4;
            let packed = bytes
                .get(at..at + 4)
                .ok_or_else(|| bad("MD2 的帧数据被截断"))?;
            let decoded =
                Vec3::new(packed[0] as f32, packed[1] as f32, packed[2] as f32) * scale + translate;
            let normal = NORMALS
                .get(packed[3] as usize)
                .copied()
                .unwrap_or([0.0, 0.0, 1.0]);
            // Z 朝上 → Y 朝上：y 和 z 对调。这是一次镜像，绕序在下面处理。
            positions.push(Vec3::new(decoded.x, decoded.z, decoded.y));
            normals.push(Vec3::new(normal[0], normal[2], normal[1]));
        }
        frames.push(Frame {
            name,
            positions,
            normals,
        });
    }

    let animations = split_animations(&frames);
    let mut md2 = Md2 {
        indices: (0..vertex_map.len() as u32).collect(),
        vertex_map,
        uvs,
        frames,
        animations,
        skins,
    };
    if winding_is_inside_out(&md2) {
        for triangle in md2.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    Ok(md2)
}

/// 拿文件自带的顶点法线给绕序做裁判，见模块文档。
fn winding_is_inside_out(md2: &Md2) -> bool {
    let frame = &md2.frames[0];
    let mut votes = 0i32;
    // 取前若干个三角形投票就够了；退化三角形（面积为零）没有意见。
    for triangle in md2.indices.chunks_exact(3).take(256) {
        let corner = |slot: usize| md2.vertex_map[triangle[slot] as usize] as usize;
        let (a, b, c) = (
            frame.positions[corner(0)],
            frame.positions[corner(1)],
            frame.positions[corner(2)],
        );
        let face = (b - a).cross(c - a);
        if face.length_squared() < 1e-12 {
            continue;
        }
        let stored =
            frame.normals[corner(0)] + frame.normals[corner(1)] + frame.normals[corner(2)];
        votes += if face.dot(stored) >= 0.0 { 1 } else { -1 };
    }
    votes < 0
}

/// 按帧名前缀切动画：`stand01`…`stand40` 是一段。
fn split_animations(frames: &[Frame]) -> Vec<Animation> {
    let prefix = |name: &str| {
        name.trim_end_matches(|c: char| c.is_ascii_digit())
            .to_string()
    };
    let mut animations: Vec<Animation> = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let name = prefix(&frame.name);
        match animations.last_mut() {
            // 只有**相邻**的同名帧才并进同一段：同一个名字在文件里分两处
            // 出现时那是两段动画，而不是一段中间带空洞的。
            Some(last) if last.name == name && last.end + 1 == index => last.end = index,
            _ => animations.push(Animation {
                name,
                start: index,
                end: index,
            }),
        }
    }
    animations
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;

    /// 拼一个最小的 MD2：一个三角形、若干帧。
    fn file(frame_names: &[&str]) -> Vec<u8> {
        let (num_vertices, num_st, num_tris) = (3usize, 3usize, 1usize);
        let offset_skins = 68usize;
        let offset_st = offset_skins;
        let offset_tris = offset_st + num_st * 4;
        let offset_frames = offset_tris + num_tris * 12;
        let frame_size = 40 + num_vertices * 4;
        let offset_end = offset_frames + frame_names.len() * frame_size;

        let mut header = Vec::new();
        header.extend_from_slice(b"IDP2");
        for value in [
            8i32,
            64,
            64,
            frame_size as i32,
            0,
            num_vertices as i32,
            num_st as i32,
            num_tris as i32,
            0,
            frame_names.len() as i32,
            offset_skins as i32,
            offset_st as i32,
            offset_tris as i32,
            offset_frames as i32,
            offset_end as i32,
            offset_end as i32,
        ] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        // 纹理坐标
        for uv in [[0i16, 0], [64, 0], [0, 64]] {
            header.extend_from_slice(&uv[0].to_le_bytes());
            header.extend_from_slice(&uv[1].to_le_bytes());
        }
        // 三角形
        for index in [0u16, 1, 2, 0, 1, 2] {
            header.extend_from_slice(&index.to_le_bytes());
        }
        // 帧
        for (number, name) in frame_names.iter().enumerate() {
            for value in [1.0f32, 1.0, 1.0, number as f32, 0.0, 0.0] {
                header.extend_from_slice(&value.to_le_bytes());
            }
            let mut label = [0u8; 16];
            label[..name.len()].copy_from_slice(name.as_bytes());
            header.extend_from_slice(&label);
            for vertex in [[0u8, 0, 0, 0], [10, 0, 0, 0], [0, 10, 0, 0]] {
                header.extend_from_slice(&vertex);
            }
        }
        header
    }

    fn load(bytes: Vec<u8>) -> Result<Md2, LoadError> {
        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        ktask::block_on(parse(bytes, PathBuf::from("t.md2"), io))
    }

    #[test]
    fn reads_frames_and_geometry() {
        let md2 = load(file(&["stand01", "stand02"])).unwrap();
        assert_eq!(md2.frames.len(), 2);
        assert_eq!(md2.indices.len(), 3);
        assert_eq!(md2.to_mesh().triangle_count(), 1);
    }

    #[test]
    fn frame_names_group_into_animations() {
        let md2 = load(file(&["stand01", "stand02", "run01", "run02", "run03"])).unwrap();
        let names: Vec<_> = md2.animations.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["stand", "run"]);
        assert_eq!(md2.find_animation("run").unwrap().len(), 3);
    }

    /// 同一个名字分两处出现时是两段动画，不是一段带空洞的。
    #[test]
    fn a_repeated_name_after_a_gap_starts_a_new_animation() {
        let md2 = load(file(&["a01", "b01", "a02"])).unwrap();
        assert_eq!(md2.animations.len(), 3);
    }

    #[test]
    fn posing_interpolates_between_two_frames() {
        let md2 = load(file(&["f1", "f2"])).unwrap();
        let mut mesh = md2.to_mesh();
        // 两帧的位移分别是 0 和 1，取中点应当是 0.5。
        md2.pose(&mut mesh, 0, 1, 0.5);
        assert!((mesh.vertices()[0].position[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn an_out_of_range_frame_clamps_instead_of_panicking() {
        let md2 = load(file(&["f1"])).unwrap();
        let mut mesh = md2.to_mesh();
        md2.pose(&mut mesh, 99, 99, 2.0);
    }

    #[test]
    fn a_wrong_magic_number_is_rejected() {
        let mut bytes = file(&["f1"]);
        bytes[..4].copy_from_slice(b"NOPE");
        assert!(load(bytes).is_err());
    }
}

/// MD2 用来量化法线的 162 个固定方向（Quake 的 `anorms.h`），原始的 Z 轴朝上坐标系。
pub const NORMALS: &[[f32; 3]] = &[
    [0.525731,0.000000,0.850651], [0.442863,0.238856,0.864188], [0.295242,0.000000,0.955423],
    [0.309017,0.500000,0.809017], [0.162460,0.262866,0.951056], [0.000000,0.000000,1.000000],
    [0.000000,0.850651,0.525731], [0.147621,0.716567,0.681718], [0.147621,0.716567,0.681718],
    [0.000000,0.525731,0.850651], [0.309017,0.500000,0.809017], [0.525731,0.000000,0.850651],
    [0.295242,0.000000,0.955423], [0.442863,0.238856,0.864188], [0.162460,0.262866,0.951056],
    [0.681718,0.147621,0.716567], [0.809017,0.309017,0.500000], [0.587785,0.425325,0.688191],
    [0.850651,0.525731,0.000000], [0.864188,0.442863,0.238856], [0.716567,0.681718,0.147621],
    [0.688191,0.587785,0.425325], [0.500000,0.809017,0.309017], [0.238856,0.864188,0.442863],
    [0.425325,0.688191,0.587785], [0.716567,0.681718,0.147621], [0.500000,0.809017,0.309017],
    [0.525731,0.850651,0.000000], [0.000000,0.850651,0.525731], [0.238856,0.864188,0.442863],
    [0.000000,0.955423,0.295242], [0.262866,0.951056,0.162460], [0.000000,1.000000,0.000000],
    [0.000000,0.955423,0.295242], [0.262866,0.951056,0.162460], [0.238856,0.864188,0.442863],
    [0.262866,0.951056,0.162460], [0.500000,0.809017,0.309017], [0.238856,0.864188,0.442863],
    [0.262866,0.951056,0.162460], [0.500000,0.809017,0.309017], [0.850651,0.525731,0.000000],
    [0.716567,0.681718,0.147621], [0.716567,0.681718,0.147621], [0.525731,0.850651,0.000000],
    [0.425325,0.688191,0.587785], [0.864188,0.442863,0.238856], [0.688191,0.587785,0.425325],
    [0.809017,0.309017,0.500000], [0.681718,0.147621,0.716567], [0.587785,0.425325,0.688191],
    [0.955423,0.295242,0.000000], [1.000000,0.000000,0.000000], [0.951056,0.162460,0.262866],
    [0.850651,0.525731,0.000000], [0.955423,0.295242,0.000000], [0.864188,0.442863,0.238856],
    [0.951056,0.162460,0.262866], [0.809017,0.309017,0.500000], [0.681718,0.147621,0.716567],
    [0.850651,0.000000,0.525731], [0.864188,0.442863,0.238856], [0.809017,0.309017,0.500000],
    [0.951056,0.162460,0.262866], [0.525731,0.000000,0.850651], [0.681718,0.147621,0.716567],
    [0.681718,0.147621,0.716567], [0.850651,0.000000,0.525731], [0.809017,0.309017,0.500000],
    [0.864188,0.442863,0.238856], [0.951056,0.162460,0.262866], [0.147621,0.716567,0.681718],
    [0.309017,0.500000,0.809017], [0.425325,0.688191,0.587785], [0.442863,0.238856,0.864188],
    [0.587785,0.425325,0.688191], [0.688191,0.587785,0.425325], [0.147621,0.716567,0.681718],
    [0.309017,0.500000,0.809017], [0.000000,0.525731,0.850651], [0.525731,0.000000,0.850651],
    [0.442863,0.238856,0.864188], [0.295242,0.000000,0.955423], [0.162460,0.262866,0.951056],
    [0.000000,0.000000,1.000000], [0.295242,0.000000,0.955423], [0.162460,0.262866,0.951056],
    [0.442863,0.238856,0.864188], [0.309017,0.500000,0.809017], [0.162460,0.262866,0.951056],
    [0.000000,0.850651,0.525731], [0.147621,0.716567,0.681718], [0.147621,0.716567,0.681718],
    [0.000000,0.525731,0.850651], [0.309017,0.500000,0.809017], [0.442863,0.238856,0.864188],
    [0.162460,0.262866,0.951056], [0.238856,0.864188,0.442863], [0.500000,0.809017,0.309017],
    [0.425325,0.688191,0.587785], [0.716567,0.681718,0.147621], [0.688191,0.587785,0.425325],
    [0.587785,0.425325,0.688191], [0.000000,0.955423,0.295242], [0.000000,1.000000,0.000000],
    [0.262866,0.951056,0.162460], [0.000000,0.850651,0.525731], [0.000000,0.955423,0.295242],
    [0.238856,0.864188,0.442863], [0.262866,0.951056,0.162460], [0.500000,0.809017,0.309017],
    [0.716567,0.681718,0.147621], [0.525731,0.850651,0.000000], [0.238856,0.864188,0.442863],
    [0.500000,0.809017,0.309017], [0.262866,0.951056,0.162460], [0.850651,0.525731,0.000000],
    [0.716567,0.681718,0.147621], [0.716567,0.681718,0.147621], [0.525731,0.850651,0.000000],
    [0.500000,0.809017,0.309017], [0.238856,0.864188,0.442863], [0.262866,0.951056,0.162460],
    [0.864188,0.442863,0.238856], [0.809017,0.309017,0.500000], [0.688191,0.587785,0.425325],
    [0.681718,0.147621,0.716567], [0.442863,0.238856,0.864188], [0.587785,0.425325,0.688191],
    [0.309017,0.500000,0.809017], [0.147621,0.716567,0.681718], [0.425325,0.688191,0.587785],
    [0.162460,0.262866,0.951056], [0.442863,0.238856,0.864188], [0.162460,0.262866,0.951056],
    [0.309017,0.500000,0.809017], [0.147621,0.716567,0.681718], [0.000000,0.525731,0.850651],
    [0.425325,0.688191,0.587785], [0.587785,0.425325,0.688191], [0.688191,0.587785,0.425325],
    [0.955423,0.295242,0.000000], [0.951056,0.162460,0.262866], [1.000000,0.000000,0.000000],
    [0.850651,0.000000,0.525731], [0.955423,0.295242,0.000000], [0.951056,0.162460,0.262866],
    [0.864188,0.442863,0.238856], [0.951056,0.162460,0.262866], [0.809017,0.309017,0.500000],
    [0.864188,0.442863,0.238856], [0.951056,0.162460,0.262866], [0.809017,0.309017,0.500000],
    [0.681718,0.147621,0.716567], [0.681718,0.147621,0.716567], [0.850651,0.000000,0.525731],
    [0.688191,0.587785,0.425325], [0.587785,0.425325,0.688191], [0.425325,0.688191,0.587785],
    [0.425325,0.688191,0.587785], [0.587785,0.425325,0.688191], [0.688191,0.587785,0.425325]
];
