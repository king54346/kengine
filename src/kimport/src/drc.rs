//! 独立的 Draco 文件（`.drc`）。
//!
//! 解码和 glTF 的 `KHR_draco_mesh_compression` 共用 [`kgltf::draco`]，
//! 这里只负责把「位置 / 法线 / 颜色 / UV」摆进引擎的顶点格式。
//!
//! `.drc` 里没有材质，产物用一个中性灰的标准材质；有顶点色时材质取白，
//! 让顶点色原样显示。点云（没有面）不支持——引擎没有点精灵网格，
//! 点云请走 PCD / PLY 那条路。

use crate::{bad, loader, single_mesh_model};
use kasset::{LoadError, ResourceIo};
use kgltf::draco::{DracoSemantic, decode};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::Material;
use kmath::Vec4;
use kmesh::{Mesh, Vertex};
use std::{path::PathBuf, sync::Arc};

loader! {
    /// 读 `.drc`。
    DracoLoader -> Model : ["drc"] = MODEL_TYPE_UUID, parse
}

/// 解析 `.drc`。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Draco".into());
    let decoded = decode(&bytes).map_err(bad)?;
    if decoded.indices.is_empty() {
        return Err(bad("这个 Draco 文件是点云（没有面），引擎只导入三角网格"));
    }
    let position = decoded
        .by_semantic(DracoSemantic::Position)
        .ok_or_else(|| bad("Draco 网格没有位置属性"))?;
    let normal = decoded.by_semantic(DracoSemantic::Normal);
    let color = decoded.by_semantic(DracoSemantic::Color);
    let uv = decoded.by_semantic(DracoSemantic::TexCoord);
    let get = |attribute: &kgltf::draco::DracoAttribute, point: usize, k: usize| {
        if k < attribute.components {
            attribute.values[point * attribute.components + k]
        } else {
            0.0
        }
    };

    let vertices = (0..decoded.points)
        .map(|p| Vertex {
            position: [get(position, p, 0), get(position, p, 1), get(position, p, 2)],
            normal: normal.map_or([0.0, 1.0, 0.0], |n| [get(n, p, 0), get(n, p, 1), get(n, p, 2)]),
            // Draco 的 UV 和 glTF 一样，原点在左上。
            uv: uv.map_or([0.0, 0.0], |t| [get(t, p, 0), get(t, p, 1)]),
            color: color.map_or([1.0; 3], |c| [get(c, p, 0), get(c, p, 1), get(c, p, 2)]),
            ..Default::default()
        })
        .collect();
    let mut mesh = Mesh::new(vertices, decoded.indices.clone());
    if !mesh.is_valid() {
        return Err(bad("Draco 网格的索引非法"));
    }
    if normal.is_none() {
        mesh.recompute_normals();
    }
    let base = if color.is_some() { Vec4::ONE } else { Vec4::new(0.72, 0.72, 0.72, 1.0) };
    let material = Material::standard().with_base_color(base).with_roughness(0.55);
    Ok(single_mesh_model(&name, mesh, material))
}
