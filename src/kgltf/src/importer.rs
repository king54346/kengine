//! glTF 导入实现。

use crate::{
    model::{MeshPart, Model, ModelNode, NodeTransform},
    uri,
};
use kasset::{LoadError, Resource, ResourceIo};
use kmaterial::Material;
use kmath::{Quat, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use ktexture::Texture;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// 导入一份 glTF / GLB。
pub(crate) async fn import(
    bytes: Vec<u8>,
    path: PathBuf,
    io: Arc<dyn ResourceIo>,
) -> Result<Model, LoadError> {
    let gltf = gltf::Gltf::from_slice(&bytes).map_err(LoadError::custom)?;
    let base = uri::base_dir(&path);

    let buffers = load_buffers(&gltf, &base, &io).await?;
    let textures = load_textures(&gltf, &base, &io, &buffers, &path).await;
    let materials = import_materials(&gltf, &textures);
    let (meshes, primitive_ranges) = import_meshes(&gltf, &buffers)?;
    let (nodes, roots) = import_nodes(&gltf, &primitive_ranges);

    klog::debug!(
        "glTF 已导入：{}（{} 网格 / {} 材质 / {} 节点）",
        path.display(),
        meshes.len(),
        materials.len(),
        nodes.len()
    );

    Ok(Model::new(meshes, materials, nodes, roots))
}

/// 加载全部缓冲区。GLB 的内嵌 BIN 块、data URI 与外部 .bin 都在这里统一。
async fn load_buffers(
    gltf: &gltf::Gltf,
    base: &Path,
    io: &Arc<dyn ResourceIo>,
) -> Result<Vec<Vec<u8>>, LoadError> {
    let mut buffers = Vec::with_capacity(gltf.buffers().len());

    for buffer in gltf.buffers() {
        let data = match buffer.source() {
            gltf::buffer::Source::Bin => gltf
                .blob
                .clone()
                .ok_or_else(|| LoadError::message("glTF 引用了 BIN 块，但文件里没有"))?,
            gltf::buffer::Source::Uri(source) => uri::read_uri(source, base, io).await?,
        };

        // 缓冲允许多出几个字节做对齐，但不能少。
        if data.len() < buffer.length() {
            return Err(LoadError::message(format!(
                "缓冲区长度不足：声明 {} 字节，实际 {} 字节",
                buffer.length(),
                data.len()
            )));
        }

        buffers.push(data);
    }

    Ok(buffers)
}

/// 加载全部贴图。单张贴图失败不影响整个模型，记日志后跳过。
async fn load_textures(
    gltf: &gltf::Gltf,
    base: &Path,
    io: &Arc<dyn ResourceIo>,
    buffers: &[Vec<u8>],
    model_path: &Path,
) -> Vec<Option<Resource<Texture>>> {
    let mut textures = Vec::with_capacity(gltf.images().len());

    for image in gltf.images() {
        let bytes = match image.source() {
            gltf::image::Source::Uri { uri: source, .. } => {
                match uri::read_uri(source, base, io).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        klog::warn!("贴图加载失败（{source}）：{error}");
                        textures.push(None);
                        continue;
                    }
                }
            }
            gltf::image::Source::View { view, .. } => {
                let Some(buffer) = buffers.get(view.buffer().index()) else {
                    klog::warn!("贴图引用了不存在的缓冲区");
                    textures.push(None);
                    continue;
                };
                let start = view.offset();
                let end = start + view.length();
                if end > buffer.len() {
                    klog::warn!("贴图的缓冲区视图越界");
                    textures.push(None);
                    continue;
                }
                buffer[start..end].to_vec()
            }
        };

        match Texture::from_encoded(&bytes) {
            Ok(texture) => {
                // 贴图不是独立文件时也需要一个路径做资源键，用模型路径加索引区分。
                let key = format!("{}#image{}", model_path.display(), image.index());
                textures.push(Some(Resource::new_ok(key, texture)));
            }
            Err(error) => {
                klog::warn!("贴图解码失败：{error}");
                textures.push(None);
            }
        }
    }

    textures
}

fn import_materials(
    gltf: &gltf::Gltf,
    textures: &[Option<Resource<Texture>>],
) -> Vec<Material> {
    gltf.materials()
        .map(|source| {
            let pbr = source.pbr_metallic_roughness();
            let color = pbr.base_color_factor();

            let mut material = Material::standard()
                .with_base_color(Vec4::from_array(color))
                .with_metallic(pbr.metallic_factor())
                .with_roughness(pbr.roughness_factor());

            if let Some(info) = pbr.base_color_texture() {
                let index = info.texture().source().index();
                if let Some(Some(texture)) = textures.get(index) {
                    material = material.with_base_color_texture(texture.clone());
                }
            }

            material
        })
        .collect()
}

/// 导入所有网格。
///
/// glTF 的一个 mesh 含多个 primitive，而引擎的 [`Mesh`] 对应单个 primitive，
/// 因此返回值第二项记录了每个 glTF mesh 展开成了哪几块。
fn import_meshes(
    gltf: &gltf::Gltf,
    buffers: &[Vec<u8>],
) -> Result<(Vec<Mesh>, Vec<Vec<MeshPart>>), LoadError> {
    let mut meshes = Vec::new();
    let mut ranges = Vec::with_capacity(gltf.meshes().len());

    for source in gltf.meshes() {
        let mut parts = Vec::new();

        for primitive in source.primitives() {
            // 只处理三角形；点、线等模式暂不支持。
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                klog::warn!("跳过非三角形图元：{:?}", primitive.mode());
                continue;
            }

            let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));

            let Some(positions) = reader.read_positions() else {
                klog::warn!("图元缺少 POSITION 属性，已跳过");
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();

            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
            let uvs: Option<Vec<[f32; 2]>> = reader
                .read_tex_coords(0)
                .map(|tc| tc.into_f32().collect());
            let colors: Option<Vec<[f32; 4]>> = reader
                .read_colors(0)
                .map(|c| c.into_rgba_f32().collect());
            let tangents: Option<Vec<[f32; 4]>> = reader.read_tangents().map(Iterator::collect);

            let vertices: Vec<Vertex> = positions
                .iter()
                .enumerate()
                .map(|(index, position)| Vertex {
                    position: *position,
                    normal: normals
                        .as_ref()
                        .and_then(|n| n.get(index).copied())
                        .unwrap_or([0.0, 1.0, 0.0]),
                    uv: uvs
                        .as_ref()
                        .and_then(|u| u.get(index).copied())
                        .unwrap_or([0.0, 0.0]),
                    color: colors
                        .as_ref()
                        .and_then(|c| c.get(index).copied())
                        .map(|c| [c[0], c[1], c[2]])
                        .unwrap_or([1.0; 3]),
                    tangent: tangents
                        .as_ref()
                        .and_then(|t| t.get(index).copied())
                        .unwrap_or([1.0, 0.0, 0.0, 1.0]),
                })
                .collect();

            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                // 没有索引缓冲时按顶点顺序组成三角形。
                None => (0..vertices.len() as u32).collect(),
            };

            let mut mesh = Mesh::new(vertices, indices);
            if !mesh.is_valid() {
                klog::warn!("图元的索引数据非法，已跳过");
                continue;
            }
            // glTF 允许省略法线，此时需要自行生成，否则光照全黑。
            if normals.is_none() {
                mesh.recompute_normals();
            }
            // 同样，缺少 TANGENT 时法线贴图无法工作，按 UV 反推一份。
            if tangents.is_none() {
                mesh.recompute_tangents();
            }

            parts.push(MeshPart {
                mesh: meshes.len(),
                material: primitive.material().index(),
            });
            meshes.push(mesh);
        }

        ranges.push(parts);
    }

    Ok((meshes, ranges))
}

fn import_nodes(
    gltf: &gltf::Gltf,
    primitive_ranges: &[Vec<MeshPart>],
) -> (Vec<ModelNode>, Vec<usize>) {
    let nodes: Vec<ModelNode> = gltf
        .nodes()
        .map(|source| {
            let (position, rotation, scale) = source.transform().decomposed();

            ModelNode {
                name: source.name().unwrap_or_default().to_string(),
                transform: NodeTransform {
                    position: Vec3::from_array(position),
                    // glTF 的四元数是 [x, y, z, w] 顺序。
                    rotation: Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
                    scale: Vec3::from_array(scale),
                },
                children: source.children().map(|child| child.index()).collect(),
                parts: source
                    .mesh()
                    .and_then(|mesh| primitive_ranges.get(mesh.index()))
                    .cloned()
                    .unwrap_or_default(),
            }
        })
        .collect();

    // 优先用默认场景的根节点；没有场景定义时，把所有非子节点当作根。
    let roots = match gltf.default_scene().or_else(|| gltf.scenes().next()) {
        Some(scene) => scene.nodes().map(|node| node.index()).collect(),
        None => {
            let mut is_child = vec![false; nodes.len()];
            for node in &nodes {
                for &child in &node.children {
                    if let Some(flag) = is_child.get_mut(child) {
                        *flag = true;
                    }
                }
            }
            is_child
                .iter()
                .enumerate()
                .filter(|(_, child)| !**child)
                .map(|(index, _)| index)
                .collect()
        }
    };

    (nodes, roots)
}
