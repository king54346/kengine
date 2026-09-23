//! 3DS（Autodesk 3D Studio）。一种二进制分块格式：每块是 `u16 id + u32 长度`，
//! 块里再嵌子块。
//!
//! # 读哪些块
//!
//! | 块 | 内容 |
//! |---|---|
//! | `0x4000` 物体 → `0x4100` 三角网格 | 顶点 `0x4110`、面 `0x4120`（含材质分组 `0x4130`）、UV `0x4140` |
//! | `0xAFFF` 材质 | 名字、漫反射 / 高光颜色、光泽度、透明度、双面、漫反射贴图 `0xA200`、凹凸贴图 `0xA230`、不透明贴图 `0xA210` |
//!
//! 关键帧段（`0xB000`）、灯光、相机不读：3DS 的关键帧层级几乎没有工具
//! 正确导出，而 three.js 的 `TDSLoader` 同样忽略它。
//!
//! # 局部矩阵
//!
//! 3DS 的顶点已经在**世界空间**里，`0x4160` 那个局部矩阵只是告诉你「物体
//! 的轴心在哪」。three.js 先用它的逆把几何搬回局部、再把矩阵分解给
//! 物体——两步抵消，顶点的世界位置不变。这里直接不做这两步。
//!
//! # 凹凸贴图
//!
//! 3DS 的「凹凸贴图」在现代导出器里装的其实是法线贴图（portal gun 那个
//! 样本就是），按法线贴图接入。
//!
//! # 平滑组
//!
//! 3DS 的法线由平滑组（`0x4150`）决定，这里不读平滑组，按位置焊接后
//! 算平滑法线——硬边会被抹圆，这是已知的简化。

use crate::{bad, flat_model, limits, load_texture, loader, base_dir};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::Material;
use kmath::{Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

loader! {
    /// 读 `.3ds`，连带同目录（或 `textures/` 子目录）里的贴图。
    TdsLoader -> Model : ["3ds"] = MODEL_TYPE_UUID, parse
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u16(&self, at: usize) -> Result<u16, LoadError> {
        self.data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| bad("3DS 被截断"))
    }
    fn u32(&self, at: usize) -> Result<u32, LoadError> {
        self.data
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| bad("3DS 被截断"))
    }
    fn f32(&self, at: usize) -> Result<f32, LoadError> {
        self.u32(at).map(f32::from_bits)
    }
    /// 以 0 结尾的字符串，返回（字符串, 占用字节数）。
    fn cstr(&self, at: usize) -> (String, usize) {
        let rest = self.data.get(at..).unwrap_or(&[]);
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        (String::from_utf8_lossy(&rest[..end]).into_owned(), end + 1)
    }
    /// `[start, end)` 范围内的子块：`(id, 数据起点, 块终点)`。
    fn chunks(&self, start: usize, end: usize) -> Vec<(u16, usize, usize)> {
        let mut out = Vec::new();
        let mut at = start;
        let end = end.min(self.data.len());
        while at + 6 <= end {
            let (Ok(id), Ok(length)) = (self.u16(at), self.u32(at + 2)) else { break };
            let length = length as usize;
            if length < 6 || at + length > end {
                break;
            }
            out.push((id, at + 6, at + length));
            at += length;
        }
        out
    }
}

#[derive(Default)]
struct TdsMaterial {
    name: String,
    diffuse: Vec3,
    specular: f32,
    shininess: f32,
    transparency: f32,
    two_sided: bool,
    diffuse_map: Option<(String, [f32; 4])>,
    bump_map: Option<String>,
    opacity_map: Option<String>,
}

struct Object {
    name: String,
    positions: Vec<Vec3>,
    uvs: Vec<[f32; 2]>,
    faces: Vec<[u16; 3]>,
    /// 材质名 → 用这个材质的面序号。
    groups: Vec<(String, Vec<u16>)>,
}

fn color(reader: &Reader, start: usize, end: usize) -> Option<Vec3> {
    for (id, at, _) in reader.chunks(start, end) {
        match id {
            0x0010 | 0x0013 => {
                return Some(Vec3::new(reader.f32(at).ok()?, reader.f32(at + 4).ok()?, reader.f32(at + 8).ok()?));
            }
            0x0011 | 0x0012 => {
                let b = reader.data.get(at..at + 3)?;
                return Some(Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32) / 255.0);
            }
            _ => {}
        }
    }
    None
}

fn percent(reader: &Reader, start: usize, end: usize) -> Option<f32> {
    for (id, at, _) in reader.chunks(start, end) {
        match id {
            0x0030 => return Some(reader.u16(at).ok()? as f32 / 100.0),
            0x0031 => return Some(reader.f32(at).ok()? / 100.0),
            _ => {}
        }
    }
    None
}

fn map(reader: &Reader, start: usize, end: usize) -> Option<(String, [f32; 4])> {
    let mut name = None;
    let mut transform = [1.0, 1.0, 0.0, 0.0];
    for (id, at, _) in reader.chunks(start, end) {
        match id {
            0xA300 => name = Some(reader.cstr(at).0),
            0xA354 => transform[1] = reader.f32(at).unwrap_or(1.0),
            0xA356 => transform[0] = reader.f32(at).unwrap_or(1.0),
            0xA358 => transform[2] = reader.f32(at).unwrap_or(0.0),
            0xA35A => transform[3] = reader.f32(at).unwrap_or(0.0),
            _ => {}
        }
    }
    name.map(|n| (n, transform))
}

fn read_material(reader: &Reader, start: usize, end: usize) -> TdsMaterial {
    let mut material = TdsMaterial {
        diffuse: Vec3::splat(0.8),
        ..Default::default()
    };
    for (id, at, chunk_end) in reader.chunks(start, end) {
        match id {
            0xA000 => material.name = reader.cstr(at).0,
            0xA020 => material.diffuse = color(reader, at, chunk_end).unwrap_or(material.diffuse),
            0xA030 => material.specular = color(reader, at, chunk_end).map_or(0.0, |c| c.max_element()),
            0xA040 => material.shininess = percent(reader, at, chunk_end).unwrap_or(0.0),
            0xA050 => material.transparency = percent(reader, at, chunk_end).unwrap_or(0.0),
            0xA081 => material.two_sided = true,
            0xA200 => material.diffuse_map = map(reader, at, chunk_end),
            0xA230 => material.bump_map = map(reader, at, chunk_end).map(|m| m.0),
            0xA210 => material.opacity_map = map(reader, at, chunk_end).map(|m| m.0),
            _ => {}
        }
    }
    material
}

fn read_mesh(reader: &Reader, name: String, start: usize, end: usize) -> Result<Object, LoadError> {
    let mut object = Object {
        name,
        positions: Vec::new(),
        uvs: Vec::new(),
        faces: Vec::new(),
        groups: Vec::new(),
    };
    for (id, at, chunk_end) in reader.chunks(start, end) {
        match id {
            0x4110 => {
                let count = reader.u16(at)? as usize;
                object.positions = (0..count)
                    .map(|i| {
                        let p = at + 2 + i * 12;
                        Ok(Vec3::new(reader.f32(p)?, reader.f32(p + 4)?, reader.f32(p + 8)?))
                    })
                    .collect::<Result<_, LoadError>>()?;
            }
            0x4140 => {
                let count = reader.u16(at)? as usize;
                object.uvs = (0..count)
                    .map(|i| {
                        let p = at + 2 + i * 8;
                        // 3DS 的 V 朝上，引擎和 glTF 一样朝下。
                        Ok([reader.f32(p)?, 1.0 - reader.f32(p + 4)?])
                    })
                    .collect::<Result<_, LoadError>>()?;
            }
            0x4120 => {
                let count = reader.u16(at)? as usize;
                object.faces = (0..count)
                    .map(|i| {
                        let p = at + 2 + i * 8;
                        Ok([reader.u16(p)?, reader.u16(p + 2)?, reader.u16(p + 4)?])
                    })
                    .collect::<Result<_, LoadError>>()?;
                // 面列表之后是它的子块：材质分组。
                for (sub, sub_at, _) in reader.chunks(at + 2 + count * 8, chunk_end) {
                    if sub == 0x4130 {
                        let (material, used) = reader.cstr(sub_at);
                        let n = reader.u16(sub_at + used)? as usize;
                        let faces = (0..n)
                            .map(|k| reader.u16(sub_at + used + 2 + k * 2))
                            .collect::<Result<Vec<_>, _>>()?;
                        object.groups.push((material, faces));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(object)
}

/// 解析 3DS。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let reader = Reader { data: &bytes };
    if reader.u16(0)? != 0x4D4D {
        return Err(bad("不是 3DS 文件（缺少 0x4D4D 主块）"));
    }
    let main_end = reader.u32(2)? as usize;
    let mut materials: Vec<TdsMaterial> = Vec::new();
    let mut objects: Vec<Object> = Vec::new();
    let mut vertex_total = 0usize;
    for (id, at, end) in reader.chunks(6, main_end) {
        if id != 0x3D3D {
            continue;
        }
        for (sub, sub_at, sub_end) in reader.chunks(at, end) {
            match sub {
                0xAFFF => materials.push(read_material(&reader, sub_at, sub_end)),
                0x4000 => {
                    let (name, used) = reader.cstr(sub_at);
                    for (kind, mesh_at, mesh_end) in reader.chunks(sub_at + used, sub_end) {
                        if kind == 0x4100 {
                            let object = read_mesh(&reader, name.clone(), mesh_at, mesh_end)?;
                            vertex_total += object.faces.len() * 3;
                            if vertex_total > limits::VERTICES {
                                return Err(bad("3DS 顶点数超过上限"));
                            }
                            objects.push(object);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if objects.is_empty() {
        return Err(bad("3DS 里没有三角网格"));
    }

    // 贴图常放在模型旁边，或者 `textures/` 子目录（three.js 例子里就是这样）。
    let base = base_dir(&path);
    let mut engine_materials = Vec::with_capacity(materials.len() + 1);
    for source in &materials {
        let mut material = Material::standard()
            .with_base_color(source.diffuse.extend(1.0 - source.transparency))
            .with_metallic(0.0)
            .with_roughness((1.0 - source.shininess).clamp(0.05, 1.0));
        material.set_name(&source.name);
        material.set_double_sided(source.two_sided);
        if source.transparency > 0.01 || source.opacity_map.is_some() {
            material.set_blend_mode(kmaterial::BlendMode::Alpha);
        }
        let texture = |name: &str, linear: bool| {
            let io = io.clone();
            let base = base.clone();
            let name = name.to_string();
            async move {
                match load_texture(&io, &base, &name, linear).await {
                    Some(t) => Some(t),
                    None => load_texture(&io, &base.join("textures"), &name, linear).await,
                }
            }
        };
        if let Some((name, [su, sv, ou, ov])) = &source.diffuse_map
            && let Some(t) = texture(name, false).await
        {
            material = material.with_base_color_texture(t).with_base_color(Vec4::new(1.0, 1.0, 1.0, 1.0 - source.transparency));
            if (*su, *sv, *ou, *ov) != (1.0, 1.0, 0.0, 0.0) {
                material.set(kpbr::standard::UV_SCALE, kmath::Vec2::new(*su, *sv));
                material.set(kpbr::standard::UV_OFFSET, kmath::Vec2::new(*ou, *ov));
            }
        }
        if let Some(name) = &source.bump_map
            && let Some(t) = texture(name, true).await
        {
            material.set(kpbr::standard::NORMAL_TEXTURE, t);
            material.set("normal_scale", 1.0f32);
        }
        engine_materials.push(material);
    }
    let fallback = engine_materials.len();
    engine_materials.push(Material::standard().with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0)).with_roughness(0.6));
    let index_of: HashMap<&str, usize> = materials.iter().enumerate().map(|(i, m)| (m.name.as_str(), i)).collect();

    let mut parts = Vec::new();
    for object in &objects {
        // 面按材质分组；没进任何组的面用兜底材质。
        let mut assigned = vec![fallback; object.faces.len()];
        for (name, faces) in &object.groups {
            let material = index_of.get(name.as_str()).copied().unwrap_or(fallback);
            for &face in faces {
                if let Some(slot) = assigned.get_mut(face as usize) {
                    *slot = material;
                }
            }
        }
        let mut by_material: Vec<usize> = assigned.clone();
        by_material.sort_unstable();
        by_material.dedup();
        for material in by_material {
            let mut remap: HashMap<u16, u32> = HashMap::new();
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            for (face, corners) in object.faces.iter().enumerate() {
                if assigned[face] != material {
                    continue;
                }
                for &corner in corners {
                    let Some(position) = object.positions.get(corner as usize) else {
                        return Err(bad("3DS 的面引用了不存在的顶点"));
                    };
                    let index = *remap.entry(corner).or_insert_with(|| {
                        vertices.push(Vertex {
                            position: position.to_array(),
                            uv: object.uvs.get(corner as usize).copied().unwrap_or([0.0, 0.0]),
                            ..Default::default()
                        });
                        vertices.len() as u32 - 1
                    });
                    indices.push(index);
                }
            }
            let mut mesh = Mesh::new(vertices, indices);
            if !mesh.is_valid() {
                continue;
            }
            mesh.recompute_normals();
            mesh.recompute_tangents();
            parts.push((object.name.clone(), mesh, Some(material)));
        }
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "3DS".into());
    Ok(flat_model(&name, parts, engine_materials))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;

    fn chunk(id: u16, body: &[u8]) -> Vec<u8> {
        let mut out = id.to_le_bytes().to_vec();
        out.extend_from_slice(&((body.len() + 6) as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn one_triangle_with_a_material() {
        let mut vertices = 3u16.to_le_bytes().to_vec();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in p {
                vertices.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mut group = b"Red\0".to_vec();
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&0u16.to_le_bytes());
        let mut faces = 1u16.to_le_bytes().to_vec();
        for v in [0u16, 1, 2, 0] {
            faces.extend_from_slice(&v.to_le_bytes());
        }
        faces.extend(chunk(0x4130, &group));
        let mesh = [chunk(0x4110, &vertices), chunk(0x4120, &faces)].concat();
        let mut object = b"Tri\0".to_vec();
        object.extend(chunk(0x4100, &mesh));
        let material = [chunk(0xA000, b"Red\0"), chunk(0xA020, &chunk(0x0011, &[255, 0, 0]))].concat();
        let editor = [chunk(0xAFFF, &material), chunk(0x4000, &object)].concat();
        let file = chunk(0x4D4D, &chunk(0x3D3D, &editor));

        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        let model = ktask::block_on(parse(file, PathBuf::from("t.3ds"), io)).unwrap();
        assert_eq!(model.triangle_count(), 1);
        assert_eq!(model.materials()[0].base_color(), Vec4::new(1.0, 0.0, 0.0, 1.0));
    }
}
