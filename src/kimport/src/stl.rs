//! STL（立体光刻）。二进制与 ASCII 两种写法都读，含逐面颜色扩展。
//!
//! # 二进制还是 ASCII 怎么判
//!
//! **不看开头那句 `solid`**——二进制 STL 的 80 字节头部里装什么都合法，
//! 很多导出器就往里写了 `solid ...`。判据是长度：二进制 STL 必然是
//! `80 + 4 + 三角形数 × 50` 字节，对得上就是二进制。这也是 three.js
//! `STLLoader` 的做法。
//!
//! # 颜色扩展
//!
//! STL 规范里没有颜色，但有两套通行扩展，都塞在每个三角形末尾那两个
//! 「属性字节数」里：
//!
//! - **Magics**：头部含 `COLOR=rgba` 时，属性字的低 15 位是 RGB555，
//!   第 15 位为 0 表示「这一面用自己的颜色」。
//! - **VisCAM / SolidView**：反过来，第 15 位为 1 才表示有颜色。
//!
//! 两套的判别位正好相反，只能靠头部里有没有 `COLOR=` 来区分，
//! 没有别的线索。这里按 three.js 的规则实现。
//!
//! # 法线
//!
//! STL 每个面都自带法线，但**不用它**：很多导出器写的是零向量或者朝向
//! 错误的向量。一律按顶点绕序重算面法线，再由 [`Mesh::recompute_normals`]
//! 得到平滑法线。STL 没有共享顶点的概念（每个三角形都写三个独立顶点），
//! 所以先做一次位置去重，否则模型看起来会是「一片片平的」。

use crate::{bad, limits, loader, single_mesh_model};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::Material;
use kmath::{Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

loader! {
    /// 读 `.stl`（二进制或 ASCII）。
    StlLoader -> Model : ["stl"] = MODEL_TYPE_UUID, parse
}

/// 解析 STL。不需要附属文件。
pub async fn parse(
    bytes: Vec<u8>,
    path: PathBuf,
    _io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "STL".into());
    let (triangles, colored) = if is_binary(&bytes) {
        parse_binary(&bytes)?
    } else {
        (parse_ascii(&String::from_utf8_lossy(&bytes))?, false)
    };
    if triangles.is_empty() {
        return Err(bad("STL 里没有三角形"));
    }

    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // 位置（按位模式）+ 颜色一起当键：同一个点被两个不同颜色的面共用时
    // 不能合并，否则颜色会串。
    let mut lookup: HashMap<([u32; 3], [u32; 3]), u32> = HashMap::new();
    for triangle in &triangles {
        for corner in &triangle.corners {
            let key = (corner.map(f32::to_bits), triangle.color.to_array().map(f32::to_bits));
            let index = match lookup.get(&key) {
                Some(&existing) => existing,
                None => {
                    vertices.push(Vertex {
                        position: *corner,
                        normal: [0.0, 1.0, 0.0],
                        color: triangle.color.to_array(),
                        ..Default::default()
                    });
                    let fresh = (vertices.len() - 1) as u32;
                    lookup.insert(key, fresh);
                    fresh
                }
            };
            indices.push(index);
        }
    }

    let mut mesh = Mesh::new(vertices, indices);
    mesh.recompute_normals();
    let material = Material::standard()
        .with_name(&name)
        .with_base_color(if colored {
            // 有逐面颜色时基础色留白，颜色全部来自顶点色。
            Vec4::ONE
        } else {
            Vec4::new(0.75, 0.75, 0.78, 1.0)
        })
        .with_metallic(0.0)
        .with_roughness(0.55);
    Ok(single_mesh_model(&name, mesh, material))
}

struct Triangle {
    corners: [[f32; 3]; 3],
    color: Vec3,
}

/// 长度对得上 `80 + 4 + n × 50` 就是二进制。见模块文档。
fn is_binary(bytes: &[u8]) -> bool {
    if bytes.len() < 84 {
        return false;
    }
    let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
    count
        .checked_mul(50)
        .and_then(|n| n.checked_add(84))
        .is_some_and(|expected| expected == bytes.len())
}

fn parse_binary(bytes: &[u8]) -> Result<(Vec<Triangle>, bool), LoadError> {
    let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
    if count > limits::VERTICES / 3 {
        return Err(bad("STL 三角形数超过上限"));
    }
    // 头部里的 `COLOR=` 决定属性字第 15 位的含义，见模块文档。
    let header = String::from_utf8_lossy(&bytes[..80]);
    let magics = header.contains("COLOR=");
    let default = magics
        .then(|| magics_default_colour(&bytes[..80]))
        .flatten()
        .unwrap_or(Vec3::ONE);

    let mut triangles = Vec::with_capacity(count);
    let mut any_colour = false;
    for index in 0..count {
        let start = 84 + index * 50;
        let float = |offset: usize| {
            f32::from_le_bytes(bytes[start + offset..start + offset + 4].try_into().unwrap())
        };
        let corners = [
            [float(12), float(16), float(20)],
            [float(24), float(28), float(32)],
            [float(36), float(40), float(44)],
        ];
        let attribute = u16::from_le_bytes(bytes[start + 48..start + 50].try_into().unwrap());
        // Magics 的判别位是 0 有效，VisCAM 是 1 有效——两套正好相反。
        let has_colour = if magics {
            attribute & 0x8000 == 0
        } else {
            attribute & 0x8000 != 0
        };
        let color = if has_colour {
            any_colour = true;
            rgb555(attribute)
        } else {
            default
        };
        triangles.push(Triangle { corners, color });
    }
    Ok((triangles, any_colour || magics))
}

/// Magics 头部里的 `COLOR=` 后面跟四个字节的 RGBA，作为整体默认色。
fn magics_default_colour(header: &[u8]) -> Option<Vec3> {
    let position = header.windows(6).position(|w| w == b"COLOR=")?;
    let rgba = header.get(position + 6..position + 10)?;
    Some(Vec3::new(
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
    ))
}

fn rgb555(attribute: u16) -> Vec3 {
    let channel = |shift: u16| ((attribute >> shift) & 0x1f) as f32 / 31.0;
    Vec3::new(channel(0), channel(5), channel(10))
}

fn parse_ascii(text: &str) -> Result<Vec<Triangle>, LoadError> {
    let mut triangles = Vec::new();
    let mut corners: Vec<[f32; 3]> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if number > limits::LINES {
            return Err(bad("STL 行数超过上限"));
        }
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("vertex") {
            let values: Vec<f32> = rest
                .split_whitespace()
                .filter_map(|token| token.parse().ok())
                .collect();
            if values.len() < 3 {
                return Err(bad(format!("第 {} 行的 vertex 少于三个分量", number + 1)));
            }
            corners.push([values[0], values[1], values[2]]);
            if corners.len() == 3 {
                triangles.push(Triangle {
                    corners: [corners[0], corners[1], corners[2]],
                    color: Vec3::ONE,
                });
                corners.clear();
            }
        } else if line.starts_with("endfacet") {
            // 一个面不足三个顶点时丢掉半截数据，别让它污染下一个面。
            corners.clear();
        }
    }
    Ok(triangles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;

    fn load(bytes: Vec<u8>) -> Result<Model, LoadError> {
        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        ktask::block_on(parse(bytes, PathBuf::from("t.stl"), io))
    }

    fn binary(count: u32, header: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 80];
        bytes[..header.len()].copy_from_slice(header);
        bytes.extend_from_slice(&count.to_le_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(&[0u8; 12]);
            for corner in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
                for value in corner {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
            bytes.extend_from_slice(&0u16.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn reads_ascii_facets() {
        let source = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid\n";
        let model = load(source.as_bytes().to_vec()).unwrap();
        assert_eq!(model.triangle_count(), 1);
    }

    #[test]
    fn a_binary_file_starting_with_solid_is_still_binary() {
        // 这正是「不能看开头那句 solid」的理由。
        let model = load(binary(2, b"solid exported by something")).unwrap();
        assert_eq!(model.triangle_count(), 2);
    }

    #[test]
    fn shared_positions_are_welded() {
        let model = load(binary(2, b"")).unwrap();
        // 两个完全一样的三角形去重后只剩三个顶点。
        assert_eq!(model.mesh(0).unwrap().vertices().len(), 3);
    }

    #[test]
    fn a_truncated_binary_file_is_rejected() {
        let mut bytes = binary(1, b"");
        bytes.truncate(100);
        assert!(load(bytes).is_err());
    }

    #[test]
    fn magics_colours_land_on_vertices() {
        let mut bytes = binary(1, b"COLOR=\xff\x00\x00\xff solid");
        let attribute = bytes.len() - 2;
        // 第 15 位为 0 = 这一面有自己的颜色（纯红）。
        bytes[attribute..].copy_from_slice(&0x001fu16.to_le_bytes());
        let model = load(bytes).unwrap();
        let color = model.mesh(0).unwrap().vertices()[0].color;
        assert!(color[0] > 0.99 && color[1] < 0.01, "取到的颜色是 {color:?}");
    }
}
