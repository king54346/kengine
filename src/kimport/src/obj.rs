//! Wavefront OBJ + MTL。
//!
//! # 支持
//!
//! `v`（含非标准的顶点色扩展 `v x y z r g b`）、`vn`、`vt`、`f`（任意边数的
//! 多边形，扇形三角化；索引可为负，表示从末尾倒数）、`o` / `g` 分组、
//! `usemtl` / `mtllib`；MTL 侧读 `Kd` / `Ks` / `Ke` / `Ns` / `d` / `Tr` /
//! `map_Kd` / `map_Ks` / `map_Bump` / `map_d`。
//!
//! # 不支持
//!
//! 自由曲面（`curv` / `surf` / `trim`）、平滑组 `s`（法线一律按面法线累加，
//! 见下）、`mtllib` 里的 PBR 扩展（`Pr` / `Pm` / `Ke` 之外的那批）。
//!
//! # 分组的划分方式
//!
//! 按 **`usemtl` 切换**分组，而不是按 `o` / `g`。OBJ 里这两套是正交的：
//! 一个 `o` 可以在中途换好几次材质。引擎的一个网格只挂一个材质，所以
//! 决定网格边界的必须是材质而不是名字——否则一个换了三次材质的 `o`
//! 只能取其中一个材质，另外两份几何会画错颜色。
//!
//! # 法线
//!
//! 文件给了 `vn` 就用文件的；没给则整份几何算完之后调
//! [`Mesh::recompute_normals`]，得到的是**平滑**法线（相邻面共用顶点时
//! 会被平均）。OBJ 的平滑组 `s` 能表达「这条边是硬边」，这里忽略它，
//! 于是本该是硬边的地方会被抹圆。要精确还原就得按平滑组拆顶点，
//! 而这批例子里没有用到平滑组的模型。

use crate::{bad, base_dir, limits, loader, sibling};
use kasset::{LoadError, Resource, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::{BlendMode, Material};
use kmath::{Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use ktexture::{Texture, TextureFormat};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};

loader! {
    /// 读 `.obj`，连带同目录的 `.mtl` 与其中引用的贴图。
    ObjLoader -> Model : ["obj"] = MODEL_TYPE_UUID, parse
}

/// 解析 OBJ 文本，异步读取 `mtllib` 指向的材质库与贴图。
pub async fn parse(
    bytes: Vec<u8>,
    path: PathBuf,
    io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let text = String::from_utf8_lossy(&bytes);
    let base = base_dir(&path);

    // 先扫一遍把 mtllib 找出来，材质得在建网格之前就位。
    let mut materials: HashMap<String, Material> = HashMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("mtllib ")
            && let Some(library) = sibling(&io, &base, rest.trim()).await
        {
            let parsed = parse_mtl(&String::from_utf8_lossy(&library), &io, &base).await;
            materials.extend(parsed);
        }
    }

    let mut positions: Vec<Vec3> = Vec::new();
    let mut colors: Vec<Vec3> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();

    let mut groups: Vec<Group> = Vec::new();
    let mut current = Group::new("default".into(), None);
    let mut object_name = String::from("OBJ");

    for (number, line) in text.lines().enumerate() {
        if number > limits::LINES {
            return Err(bad("OBJ 行数超过上限"));
        }
        let line = line.trim();
        let Some((keyword, rest)) = split_keyword(line) else {
            continue;
        };
        match keyword {
            "v" => {
                let values = floats(rest);
                if values.len() < 3 {
                    return Err(bad(format!("第 {} 行的 v 少于三个分量", number + 1)));
                }
                positions.push(Vec3::new(values[0], values[1], values[2]));
                // 非标准但很常见的扩展：位置后面再跟三个数就是顶点色。
                colors.push(if values.len() >= 6 {
                    Vec3::new(values[3], values[4], values[5])
                } else {
                    Vec3::ONE
                });
            }
            "vn" => {
                let values = floats(rest);
                if values.len() >= 3 {
                    normals.push(Vec3::new(values[0], values[1], values[2]));
                }
            }
            "vt" => {
                let values = floats(rest);
                if values.len() >= 2 {
                    // OBJ 的 v 轴朝上，图片的行号朝下。
                    uvs.push([values[0], 1.0 - values[1]]);
                }
            }
            "f" => current.push_face(rest, &positions, &colors, &normals, &uvs)?,
            "usemtl" => {
                let name = rest.trim().to_string();
                if !current.is_empty() {
                    groups.push(std::mem::replace(
                        &mut current,
                        Group::new(name.clone(), Some(name.clone())),
                    ));
                } else {
                    current.name = name.clone();
                    current.material = Some(name);
                }
            }
            "o" => object_name = rest.trim().to_string(),
            _ => {}
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    if groups.is_empty() {
        return Err(bad("OBJ 里没有任何面"));
    }

    // 材质槽位按「用到的顺序」排，没写 MTL 的分组退回一个灰色标准材质。
    let mut slots: Vec<Material> = Vec::new();
    let mut slot_of: HashMap<String, usize> = HashMap::new();
    let mut parts = Vec::with_capacity(groups.len());
    for group in groups {
        let slot = group.material.as_ref().map(|name| {
            *slot_of.entry(name.clone()).or_insert_with(|| {
                slots.push(
                    materials
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| fallback_material(name)),
                );
                slots.len() - 1
            })
        });
        let had_normals = group.had_normals;
        let mut mesh = Mesh::new(group.vertices, group.indices);
        if !had_normals {
            mesh.recompute_normals();
        }
        mesh.recompute_tangents();
        parts.push((group.name, mesh, slot));
    }
    if slots.is_empty() {
        slots.push(fallback_material("default"));
    }
    Ok(crate::flat_model(&object_name, parts, slots))
}

/// 一个材质分组正在累积的几何。
struct Group {
    name: String,
    material: Option<String>,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    /// `(v, vt, vn)` 三元组 → 已生成的顶点号。OBJ 的三套索引各自独立，
    /// 而 GPU 的顶点缓冲只有一套，必须按三元组去重后展开。
    lookup: HashMap<(i64, i64, i64), u32>,
    had_normals: bool,
}

impl Group {
    fn new(name: String, material: Option<String>) -> Self {
        Self {
            name,
            material,
            vertices: Vec::new(),
            indices: Vec::new(),
            lookup: HashMap::new(),
            had_normals: true,
        }
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push_face(
        &mut self,
        rest: &str,
        positions: &[Vec3],
        colors: &[Vec3],
        normals: &[Vec3],
        uvs: &[[f32; 2]],
    ) -> Result<(), LoadError> {
        let mut corners = Vec::new();
        for token in rest.split_whitespace() {
            let mut parts = token.split('/');
            let v = index(parts.next(), positions.len())?;
            let Some(v) = v else {
                return Err(bad("面的顶点索引为空"));
            };
            let vt = index(parts.next(), uvs.len())?;
            let vn = index(parts.next(), normals.len())?;
            let key = (
                v as i64,
                vt.map_or(-1, |i| i as i64),
                vn.map_or(-1, |i| i as i64),
            );
            let vertex = match self.lookup.get(&key) {
                Some(&existing) => existing,
                None => {
                    if vn.is_none() {
                        self.had_normals = false;
                    }
                    self.vertices.push(Vertex {
                        position: positions[v].to_array(),
                        normal: vn.map_or([0.0, 1.0, 0.0], |i| normals[i].to_array()),
                        uv: vt.map_or([0.0, 0.0], |i| uvs[i]),
                        color: colors.get(v).copied().unwrap_or(Vec3::ONE).to_array(),
                        ..Default::default()
                    });
                    let fresh = (self.vertices.len() - 1) as u32;
                    self.lookup.insert(key, fresh);
                    fresh
                }
            };
            corners.push(vertex);
        }
        if corners.len() < 3 {
            // 点和线在 OBJ 里合法，这个导入器只要面。
            return Ok(());
        }
        if self.vertices.len() > limits::VERTICES {
            return Err(bad("OBJ 顶点数超过上限"));
        }
        // 扇形三角化。OBJ 的多边形按约定是平面且凸的，扇形足够；
        // 凹多边形会画出多余的三角形，那时该由建模工具先三角化。
        for i in 1..corners.len() - 1 {
            self.indices
                .extend_from_slice(&[corners[0], corners[i], corners[i + 1]]);
        }
        Ok(())
    }
}

/// 解析 OBJ 的一个索引分量：1 起计，负数表示从末尾倒数，空串表示没有。
fn index(token: Option<&str>, count: usize) -> Result<Option<usize>, LoadError> {
    let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let value: i64 = token.parse().map_err(|_| bad(format!("非法索引 {token}")))?;
    let resolved = if value > 0 {
        value - 1
    } else if value < 0 {
        count as i64 + value
    } else {
        return Err(bad("OBJ 索引不能是 0"));
    };
    if resolved < 0 || resolved as usize >= count {
        return Err(bad(format!("索引 {value} 越界")));
    }
    Ok(Some(resolved as usize))
}

fn split_keyword(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    Some(match line.split_once(char::is_whitespace) {
        Some((keyword, rest)) => (keyword, rest),
        None => (line, ""),
    })
}

fn floats(rest: &str) -> Vec<f32> {
    rest.split_whitespace()
        .filter_map(|token| token.parse().ok())
        .collect()
}

/// MTL 里没找到对应条目时的兜底：中性灰、完全粗糙。
fn fallback_material(name: &str) -> Material {
    Material::standard()
        .with_name(name)
        .with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0))
        .with_metallic(0.0)
        .with_roughness(0.85)
}

/// 解析 MTL 文本，贴图通过 `io` 读盘。
///
/// # 高光指数怎么变成粗糙度
///
/// MTL 是 Blinn-Phong 时代的格式，只有高光指数 `Ns`。用
/// `roughness = sqrt(2 / (Ns + 2))` 换算——这是 Phong 指数与 GGX
/// 粗糙度之间的常用近似，`Ns=0` 得到 1（全糙），`Ns` 越大越光滑。
/// 换算不可能精确：两套模型的高光形状本来就不一样。
async fn parse_mtl(
    text: &str,
    io: &Arc<dyn ResourceIo>,
    base: &std::path::Path,
) -> HashMap<String, Material> {
    let mut result = HashMap::new();
    let mut name = String::new();
    let mut material = Material::standard();
    let mut has_current = false;

    for line in text.lines() {
        let line = line.trim();
        let Some((keyword, rest)) = split_keyword(line) else {
            continue;
        };
        match keyword {
            "newmtl" => {
                if has_current {
                    result.insert(std::mem::take(&mut name), std::mem::replace(&mut material, Material::standard()));
                }
                name = rest.trim().to_string();
                material = fallback_material(&name);
                has_current = true;
            }
            "Kd" => {
                let v = floats(rest);
                if v.len() >= 3 {
                    let alpha = material.base_color().w;
                    material.set_base_color(Vec4::new(v[0], v[1], v[2], alpha));
                }
            }
            "Ke" => {
                let v = floats(rest);
                if v.len() >= 3 && (v[0] > 0.0 || v[1] > 0.0 || v[2] > 0.0) {
                    material.set("emissive", Vec3::new(v[0], v[1], v[2]));
                }
            }
            "Ns" => {
                if let Some(&exponent) = floats(rest).first() {
                    material.set_roughness((2.0 / (exponent.max(0.0) + 2.0)).sqrt().clamp(0.03, 1.0));
                }
            }
            "d" | "Tr" => {
                if let Some(&value) = floats(rest).first() {
                    // `Tr` 是透明度，`d` 是不透明度，互为补数。
                    let alpha = if keyword == "Tr" { 1.0 - value } else { value };
                    let color = material.base_color();
                    material.set_base_color(color.truncate().extend(alpha));
                    if alpha < 1.0 {
                        material.set_blend_mode(BlendMode::Alpha);
                    }
                }
            }
            "map_Kd" | "map_Ks" | "map_Bump" | "bump" | "norm" => {
                let file = map_filename(rest);
                if let Some(data) = sibling(io, base, file).await
                    && let Ok(texture) = Texture::from_encoded(&data)
                {
                    let (slot, texture) = match keyword {
                        "map_Kd" => ("base_color_texture", texture),
                        "map_Ks" => (
                            "metallic_roughness_texture",
                            texture.with_format(TextureFormat::Linear),
                        ),
                        _ => ("normal_texture", texture.with_format(TextureFormat::Linear)),
                    };
                    material.set(slot, Resource::new_ok(file.to_string(), texture));
                }
            }
            _ => {}
        }
    }
    if has_current {
        result.insert(name, material);
    }
    result
}

/// MTL 的贴图行可以带一串选项（`-s 1 1 1 texture.jpg`），文件名是最后一个词。
fn map_filename(rest: &str) -> &str {
    rest.split_whitespace().next_back().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;

    async fn load(source: &str) -> Result<Model, LoadError> {
        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        parse(source.as_bytes().to_vec(), PathBuf::from("t.obj"), io).await
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        ktask::block_on(future)
    }

    #[test]
    fn imports_a_quad_as_two_triangles() {
        let model = block_on(load(
            "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 2/2 3/3 4/4\n",
        ))
        .unwrap();
        assert_eq!(model.triangle_count(), 2);
        assert_eq!(model.mesh(0).unwrap().vertices().len(), 4);
    }

    #[test]
    fn negative_indices_count_back_from_the_end() {
        let model = block_on(load("v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n")).unwrap();
        assert_eq!(model.triangle_count(), 1);
    }

    #[test]
    fn a_material_switch_starts_a_new_mesh() {
        let model = block_on(load(
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl a\nf 1 2 3\nusemtl b\nf 3 2 1\n",
        ))
        .unwrap();
        assert_eq!(model.meshes().len(), 2, "两个材质应当拆成两份几何");
        assert_eq!(model.materials().len(), 2);
    }

    #[test]
    fn vertex_colours_survive_the_extension_syntax() {
        let model = block_on(load(
            "v 0 0 0 1 0 0\nv 1 0 0 0 1 0\nv 0 1 0 0 0 1\nf 1 2 3\n",
        ))
        .unwrap();
        assert_eq!(model.mesh(0).unwrap().vertices()[0].color, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn out_of_range_indices_are_rejected() {
        assert!(block_on(load("v 0 0 0\nf 1 2 3\n")).is_err());
    }

    #[test]
    fn a_file_without_faces_is_an_error() {
        assert!(block_on(load("v 0 0 0\n")).is_err());
    }
}
