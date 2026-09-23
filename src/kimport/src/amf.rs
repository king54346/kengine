//! AMF（Additive Manufacturing File Format）：3D 打印的 XML 格式，常见的是
//! 套了一层 ZIP 的（`rook.amf` 就是）。
//!
//! 读：`object` → `mesh` → `vertices` + 若干 `volume`（每个 `volume` 一组
//! 三角形，可指定 `materialid`）；颜色可以写在 `material`、`object`、
//! `volume`、`vertex` 四个层级，**越内层越优先**；`constellation` 里的
//! `instance` 按 `deltax/y/z` + `rx/ry/rz`（度）摆放物体。
//!
//! 单位按 `unit` 属性换算成米以外的「毫米」保持原样——和 three.js 一样
//! 不做单位换算，例子自己决定缩放。
//!
//! AMF 的颜色是 sRGB 分量，这里转成线性值再交给材质 / 顶点色。

use crate::{bad, limits, loader, xml, zip};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::Material;
use kmath::{Quat, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

loader! {
    /// 读 `.amf`（纯 XML 或 ZIP 压缩的）。
    AmfLoader -> Model : ["amf"] = MODEL_TYPE_UUID, parse
}

/// sRGB 分量转线性。
pub(crate) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn read_color(element: &xml::Element) -> Option<Vec4> {
    let color = element.child("color")?;
    let channel = |name: &str, default: f32| color.child(name).and_then(|c| c.text.trim().parse().ok()).unwrap_or(default);
    Some(Vec4::new(
        srgb_to_linear(channel("r", 1.0)),
        srgb_to_linear(channel("g", 1.0)),
        srgb_to_linear(channel("b", 1.0)),
        channel("a", 1.0),
    ))
}

/// 解析 AMF。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let text = if zip::is_zip(&bytes) {
        let archive = zip::Archive::open(&bytes)?;
        let entry = archive
            .find_extension("amf")
            .or_else(|| archive.entries().first())
            .ok_or_else(|| bad("AMF 压缩包是空的"))?;
        archive.read(entry)?
    } else {
        bytes
    };
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "AMF".into());
    parse_document(&xml::parse(&text)?, &name)
}

/// 从解析好的 XML 建模型。
pub fn parse_document(root: &xml::Element, name: &str) -> Result<Model, LoadError> {
    if root.name != "amf" {
        return Err(bad("不是 AMF 文档（根元素不是 <amf>）"));
    }
    let mut materials = vec![Material::standard().with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0)).with_roughness(0.5)];
    let mut material_ids: HashMap<String, usize> = HashMap::new();
    for element in root.children_named("material") {
        let color = read_color(element).unwrap_or(Vec4::new(0.8, 0.8, 0.8, 1.0));
        let mut material = Material::standard().with_base_color(color).with_roughness(0.5);
        if let Some(name) = element.children_named("metadata").find(|m| m.attr("type") == Some("name")) {
            material.set_name(name.text.trim());
        }
        if color.w < 1.0 {
            material.set_blend_mode(kmaterial::BlendMode::Alpha);
        }
        if let Some(id) = element.attr("id") {
            material_ids.insert(id.to_string(), materials.len());
        }
        materials.push(material);
    }
    // 顶点色专用的白材质：颜色全由顶点给。
    let white = materials.len();
    materials.push(Material::standard().with_base_color(Vec4::ONE).with_roughness(0.5));

    let mut meshes = Vec::new();
    let mut nodes = vec![ModelNode {
        name: name.to_string(),
        ..Default::default()
    }];
    let mut object_nodes: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;

    for object in root.children_named("object") {
        let Some(mesh) = object.child("mesh") else { continue };
        let object_color = read_color(object);
        let mut positions = Vec::new();
        let mut vertex_colors: Vec<Option<Vec4>> = Vec::new();
        if let Some(vertices) = mesh.child("vertices") {
            for vertex in vertices.children_named("vertex") {
                let c = vertex.child("coordinates");
                let axis = |n: &str| c.and_then(|c| c.child(n)).and_then(|v| v.text.trim().parse::<f32>().ok()).unwrap_or(0.0);
                positions.push(Vec3::new(axis("x"), axis("y"), axis("z")));
                vertex_colors.push(read_color(vertex));
            }
        }
        let node_index = nodes.len();
        let mut node = ModelNode {
            name: object
                .children_named("metadata")
                .find(|m| m.attr("type") == Some("name"))
                .map_or_else(|| format!("Object{}", object.attr("id").unwrap_or("")), |m| m.text.trim().to_string()),
            ..Default::default()
        };
        for volume in mesh.children_named("volume") {
            let volume_color = read_color(volume);
            let material_id = volume.attr("materialid").and_then(|id| material_ids.get(id)).copied();
            let per_vertex = vertex_colors.iter().any(Option::is_some);
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            let mut remap: HashMap<usize, u32> = HashMap::new();
            for triangle in volume.children_named("triangle") {
                for key in ["v1", "v2", "v3"] {
                    let index: usize = triangle.child_text(key).parse().map_err(|_| bad("AMF 三角形的顶点号不是整数"))?;
                    let position = *positions.get(index).ok_or_else(|| bad("AMF 三角形引用了不存在的顶点"))?;
                    let slot = *remap.entry(index).or_insert_with(|| {
                        let color = vertex_colors[index].or(volume_color).or(object_color).unwrap_or(Vec4::ONE);
                        vertices.push(Vertex {
                            position: position.to_array(),
                            color: if per_vertex { color.truncate().to_array() } else { [1.0; 3] },
                            ..Default::default()
                        });
                        vertices.len() as u32 - 1
                    });
                    indices.push(slot);
                }
            }
            total += vertices.len();
            if total > limits::VERTICES {
                return Err(bad("AMF 顶点数超过上限"));
            }
            let mut mesh = Mesh::new(vertices, indices);
            if !mesh.is_valid() || mesh.triangle_count() == 0 {
                continue;
            }
            mesh.recompute_normals();
            // 选材质：顶点色 > 体颜色 > 物体颜色 > 材质号 > 默认。
            let material = if per_vertex {
                white
            } else if let Some(color) = volume_color.or(object_color) {
                materials.push(Material::standard().with_base_color(color).with_roughness(0.5));
                materials.len() - 1
            } else {
                material_id.unwrap_or(0)
            };
            node.parts.push(MeshPart {
                mesh: meshes.len(),
                material: Some(material),
            });
            meshes.push(mesh);
        }
        if let Some(id) = object.attr("id") {
            object_nodes.insert(id.to_string(), node_index);
        }
        nodes.push(node);
    }

    // constellation：把物体按实例摆放。有它时根只挂实例，没有时挂全部物体。
    let mut children = Vec::new();
    for constellation in root.children_named("constellation") {
        for instance in constellation.children_named("instance") {
            let Some(&source) = instance.attr("objectid").and_then(|id| object_nodes.get(id)) else { continue };
            let number = |n: &str| instance.child(n).and_then(|v| v.text.trim().parse::<f32>().ok()).unwrap_or(0.0);
            let index = nodes.len();
            let parts = nodes[source].parts.clone();
            nodes.push(ModelNode {
                name: format!("{}#{}", nodes[source].name, index),
                transform: NodeTransform {
                    position: Vec3::new(number("deltax"), number("deltay"), number("deltaz")),
                    rotation: Quat::from_euler(
                        kmath::EulerRot::XYZ,
                        number("rx").to_radians(),
                        number("ry").to_radians(),
                        number("rz").to_radians(),
                    ),
                    scale: Vec3::ONE,
                },
                parts,
                ..Default::default()
            });
            children.push(index);
        }
    }
    if children.is_empty() {
        children = (1..nodes.len()).collect();
    }
    nodes[0].children = children;
    if meshes.is_empty() {
        return Err(bad("AMF 里没有三角形"));
    }
    Ok(Model::new(meshes, materials, nodes, vec![0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colored_triangle() {
        let doc = xml::parse(
            br#"<amf unit="millimeter"><object id="0"><color><r>1</r><g>0</g><b>0</b></color><mesh><vertices>
            <vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex>
            <vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex>
            <vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex>
            </vertices><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object></amf>"#,
        )
        .unwrap();
        let model = parse_document(&doc, "t").unwrap();
        assert_eq!(model.triangle_count(), 1);
        let material = model.nodes()[1].parts[0].material.unwrap();
        assert_eq!(model.materials()[material].base_color(), Vec4::new(1.0, 0.0, 0.0, 1.0));
    }
}
