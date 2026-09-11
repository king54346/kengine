//! MagicaVoxel `.vox`。
//!
//! # 支持
//!
//! `SIZE` / `XYZI` / `RGBA` / `PACK`。多个模型会各成一个节点。
//!
//! # 不支持
//!
//! 场景图块（`nTRN` / `nGRP` / `nSHP`）——模型一律摆在原点，不施加
//! 场景图里的位移与旋转；材质块（`MATL`，金属/发光/玻璃参数）；
//! 图层（`LAYR`）。这和 three.js 的 `VOXLoader` 是同一个取舍。
//!
//! # 贪心合并（greedy meshing）
//!
//! 一个 126³ 的模型如果每个体素都老老实实画六个面，会是几百万个三角形，
//! 其中绝大多数还被邻居挡着。这里做两件事：
//!
//! 1. **剔除内部面**：只有「一侧实一侧空」的面才需要画。
//! 2. **合并共面同色的矩形**：把一整片朝向相同、颜色相同的面并成一个
//!    大四边形。
//!
//! monu10 这个样本从三百多万个三角形降到两万出头。算法和 three.js
//! `VOXLoader` 的 `buildMesh` 是同一个，两边的三角形数可以对着看。
//!
//! # 坐标系
//!
//! MagicaVoxel 是 Z 轴朝上、Y 轴朝前；引擎和 three.js 都是 Y 轴朝上。
//! 转换是 `(x, z, -y)`，并且减去尺寸的一半让模型居中。

use crate::{bad, limits, loader, single_mesh_model};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::Material;
use kmath::Vec4;
use kmesh::{Mesh, Vertex};
use std::{path::PathBuf, sync::Arc};

loader! {
    /// 读 `.vox`。
    VoxLoader -> Model : ["vox"] = MODEL_TYPE_UUID, parse
}

/// 一个体素模型：尺寸、体素表、调色板。
#[derive(Debug, Clone)]
pub struct VoxelModel {
    /// X / Y / Z 三个方向的体素数。
    pub size: [u32; 3],
    /// 每个体素的 `(x, y, z, 调色板下标)`。下标从 1 起，0 表示空。
    pub voxels: Vec<[u8; 4]>,
    /// 256 色调色板，RGBA。
    pub palette: Vec<[u8; 4]>,
}

/// 解析 `.vox`。
pub async fn parse(
    bytes: Vec<u8>,
    path: PathBuf,
    _io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "VOX".into());
    let models = read_chunks(&bytes)?;
    if models.is_empty() {
        return Err(bad("VOX 里没有模型"));
    }

    let material = Material::standard()
        .with_name(&name)
        .with_base_color(Vec4::ONE)
        .with_metallic(0.0)
        .with_roughness(0.9);

    if models.len() == 1 {
        return Ok(single_mesh_model(&name, build_mesh(&models[0]), material));
    }
    let parts = models
        .iter()
        .enumerate()
        .map(|(index, model)| (format!("{name}{index}"), build_mesh(model), Some(0)))
        .collect();
    Ok(crate::flat_model(&name, parts, vec![material]))
}

/// 走一遍 RIFF 式的块结构，把 `SIZE` / `XYZI` / `RGBA` 挑出来。
fn read_chunks(bytes: &[u8]) -> Result<Vec<VoxelModel>, LoadError> {
    if !bytes.starts_with(b"VOX ") || bytes.len() < 20 {
        return Err(bad("不是 VOX 文件"));
    }
    // MAIN 块的头 12 字节跳过，之后是一串子块。
    let mut cursor = 20usize;
    let mut sizes: Vec<[u32; 3]> = Vec::new();
    let mut voxel_sets: Vec<Vec<[u8; 4]>> = Vec::new();
    let mut palette: Option<Vec<[u8; 4]>> = None;

    while cursor + 12 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let content = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let children =
            u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into().unwrap()) as usize;
        let start = cursor + 12;
        let body = bytes
            .get(start..start + content)
            .ok_or_else(|| bad("VOX 块被截断"))?;
        match id {
            b"SIZE" if body.len() >= 12 => {
                let axis = |index: usize| {
                    u32::from_le_bytes(body[index * 4..index * 4 + 4].try_into().unwrap())
                };
                let size = [axis(0), axis(1), axis(2)];
                if size.iter().any(|&n| n == 0 || n > 2048) {
                    return Err(bad("VOX 的模型尺寸为零或超过 2048"));
                }
                sizes.push(size);
            }
            b"XYZI" if body.len() >= 4 => {
                let count = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
                if count > limits::VERTICES / 8 || body.len() < 4 + count * 4 {
                    return Err(bad("VOX 的体素数超过上限或数据被截断"));
                }
                voxel_sets.push(
                    body[4..4 + count * 4]
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                );
            }
            b"RGBA" if body.len() >= 1024 => {
                // 调色板里第 i 个条目对应体素里的下标 i+1，前面补一个空位。
                let mut colors = vec![[0u8; 4]];
                colors.extend(body[..1024].chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]));
                palette = Some(colors);
            }
            _ => {}
        }
        // 块头本身 12 字节，所以即使内容和子块都是空的，cursor 也在前进。
        cursor = start + content + children;
    }

    let palette = palette.unwrap_or_else(|| vec![[255, 255, 255, 255]; 257]);
    Ok(sizes
        .into_iter()
        .zip(voxel_sets)
        .map(|(size, voxels)| VoxelModel {
            size,
            voxels,
            palette: palette.clone(),
        })
        .collect())
}

/// 贪心合并出网格，见模块文档。
pub fn build_mesh(model: &VoxelModel) -> Mesh {
    let [sx, sy, sz] = model.size.map(|n| n as usize);
    let mut volume = vec![0u8; sx * sy * sz];
    for voxel in &model.voxels {
        let (x, y, z) = (voxel[0] as usize, voxel[1] as usize, voxel[2] as usize);
        if x < sx && y < sy && z < sz {
            volume[x + y * sx + z * sx * sy] = voxel[3];
        }
    }

    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let dims = [sx, sy, sz];
    // MagicaVoxel 的 Z 朝上、Y 朝前，引擎 Y 朝上：`(x, z, -y)`，并居中。
    let to_engine = |p: [usize; 3]| {
        [
            p[0] as f32 - sx as f32 / 2.0,
            p[2] as f32 - sz as f32 / 2.0,
            -(p[1] as f32) + sy as f32 / 2.0,
        ]
    };

    for d in 0..3 {
        let u = (d + 1) % 3;
        let v = (d + 2) % 3;
        let (dim_d, dim_u, dim_v) = (dims[d], dims[u], dims[v]);
        let mut normal_step = [0usize; 3];
        normal_step[d] = 1;
        let mut mask = vec![0i16; dim_u * dim_v];

        for slice in 0..=dim_d {
            // 先把这一层的「哪儿有面」填出来：一侧实一侧空才有面，
            // 符号表示朝向。
            for vv in 0..dim_v {
                for uu in 0..dim_u {
                    let mut position = [0usize; 3];
                    position[d] = slice;
                    position[u] = uu;
                    position[v] = vv;
                    let behind = if slice > 0 {
                        let back = [
                            position[0] - normal_step[0],
                            position[1] - normal_step[1],
                            position[2] - normal_step[2],
                        ];
                        volume[back[0] + back[1] * sx + back[2] * sx * sy]
                    } else {
                        0
                    };
                    let front = if slice < dim_d {
                        volume[position[0] + position[1] * sx + position[2] * sx * sy]
                    } else {
                        0
                    };
                    mask[uu + vv * dim_u] = match (behind > 0, front > 0) {
                        (true, false) => behind as i16,
                        (false, true) => -(front as i16),
                        _ => 0,
                    };
                }
            }

            // 再把同色同朝向的相邻格子并成尽量大的矩形。
            let mut vv = 0;
            while vv < dim_v {
                let mut uu = 0;
                while uu < dim_u {
                    let cell = mask[uu + vv * dim_u];
                    if cell == 0 {
                        uu += 1;
                        continue;
                    }
                    let mut width = 1;
                    while uu + width < dim_u && mask[uu + width + vv * dim_u] == cell {
                        width += 1;
                    }
                    let mut height = 1;
                    'grow: while vv + height < dim_v {
                        for k in 0..width {
                            if mask[uu + k + (vv + height) * dim_u] != cell {
                                break 'grow;
                            }
                        }
                        height += 1;
                    }

                    let mut origin = [0usize; 3];
                    origin[d] = slice;
                    origin[u] = uu;
                    origin[v] = vv;
                    let mut along_u = [0usize; 3];
                    along_u[u] = width;
                    let mut along_v = [0usize; 3];
                    along_v[v] = height;
                    let offset = |a: [usize; 3], b: [usize; 3]| {
                        [origin[0] + a[0] + b[0], origin[1] + a[1] + b[1], origin[2] + a[2] + b[2]]
                    };
                    let corners = [
                        to_engine(origin),
                        to_engine(offset(along_u, [0; 3])),
                        to_engine(offset(along_u, along_v)),
                        to_engine(offset([0; 3], along_v)),
                    ];

                    let color = model
                        .palette
                        .get(cell.unsigned_abs() as usize)
                        .copied()
                        .unwrap_or([255; 4]);
                    let color = [
                        color[0] as f32 / 255.0,
                        color[1] as f32 / 255.0,
                        color[2] as f32 / 255.0,
                    ];

                    let base = vertices.len() as u32;
                    // 正朝向和负朝向的绕序相反，否则一半的面会被背面剔除掉。
                    let order: [usize; 4] = if cell > 0 { [0, 1, 2, 3] } else { [0, 3, 2, 1] };
                    for &corner in &order {
                        vertices.push(Vertex {
                            position: corners[corner],
                            normal: [0.0, 1.0, 0.0],
                            color,
                            ..Default::default()
                        });
                    }
                    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

                    // 并进来的格子要清掉，否则会被重复输出。
                    for dv in 0..height {
                        for du in 0..width {
                            mask[uu + du + (vv + dv) * dim_u] = 0;
                        }
                    }
                    uu += width;
                }
                vv += 1;
            }
        }
    }

    let mut mesh = Mesh::new(vertices, indices);
    mesh.recompute_normals();
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;

    /// 拼一个最小的 VOX：一个 `sx×sy×sz` 的盒子，指定哪些体素是实的。
    fn file(size: [u32; 3], voxels: &[[u8; 4]]) -> Vec<u8> {
        let chunk = |id: &[u8; 4], body: Vec<u8>| {
            let mut out = id.to_vec();
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&body);
            out
        };
        let mut size_body = Vec::new();
        for axis in size {
            size_body.extend_from_slice(&axis.to_le_bytes());
        }
        let mut xyzi = (voxels.len() as u32).to_le_bytes().to_vec();
        for voxel in voxels {
            xyzi.extend_from_slice(voxel);
        }
        let mut palette = Vec::new();
        for index in 0..256u32 {
            palette.extend_from_slice(&[index as u8, 0, 0, 255]);
        }

        let mut children = chunk(b"SIZE", size_body);
        children.extend(chunk(b"XYZI", xyzi));
        children.extend(chunk(b"RGBA", palette));

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(b"MAIN");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(children.len() as u32).to_le_bytes());
        bytes.extend(children);
        bytes
    }

    fn load(bytes: Vec<u8>) -> Result<Model, LoadError> {
        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        ktask::block_on(parse(bytes, PathBuf::from("t.vox"), io))
    }

    #[test]
    fn a_single_voxel_has_six_faces() {
        let model = load(file([1, 1, 1], &[[0, 0, 0, 1]])).unwrap();
        assert_eq!(model.triangle_count(), 12, "六个面，每面两个三角形");
    }

    /// 贪心合并的整个意义所在：两个同色体素并排，接缝那两个面消失，
    /// 而外侧共面的面要并成一个。
    #[test]
    fn adjacent_same_colour_voxels_merge_into_larger_quads() {
        let merged = load(file([2, 1, 1], &[[0, 0, 0, 1], [1, 0, 0, 1]])).unwrap();
        // 不合并的话是 2×6 面减去 2 个内部面 = 10 个面 = 20 个三角形。
        // 合并之后上下前后四个方向各并成一个大四边形，只剩 6 个面。
        assert_eq!(merged.triangle_count(), 12);
    }

    #[test]
    fn different_colours_do_not_merge() {
        let model = load(file([2, 1, 1], &[[0, 0, 0, 1], [1, 0, 0, 2]])).unwrap();
        assert_eq!(model.triangle_count(), 20);
    }

    #[test]
    fn palette_colours_land_on_vertices() {
        let model = load(file([1, 1, 1], &[[0, 0, 0, 2]])).unwrap();
        // 体素里的下标 i 取的是 RGBA 块里的第 i−1 项——这个偏移是格式
        // 规定的，写成 i 的话整个模型的颜色会整体错开一格。
        let color = model.mesh(0).unwrap().vertices()[0].color;
        assert!((color[0] - 1.0 / 255.0).abs() < 1e-6, "取到的是 {color:?}");
    }

    #[test]
    fn a_file_without_the_magic_number_is_rejected() {
        assert!(load(b"not a vox file at all............".to_vec()).is_err());
    }
}
