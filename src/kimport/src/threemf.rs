//! 3MF（3D Manufacturing Format）：ZIP 里装 XML 模型 + 贴图。
//!
//! # 支持
//!
//! | 部分 | |
//! |---|---|
//! | 核心 | `object`（网格 / 组件）、`build` → `item`、4×3 变换矩阵 |
//! | `basematerials` | 按三角形选材质（`displaycolor`） |
//! | 材质扩展 `m:colorgroup` | 逐面 / 逐顶点颜色 |
//! | 材质扩展 `m:texture2d` + `m:texture2dgroup` | 逐顶点 UV + 贴图（含 `tilestyle`） |
//! | 生产扩展 `p:path` | 组件引用压缩包里的另一个 `.model` 文件 |
//!
//! 不支持：`m:multiproperties`、`m:compositematerials`、切片扩展。
//!
//! 体积扩展的 `v:levelset`（`volumetric.3mf`）：形状是一棵隐式函数图，
//! 要按网格包围盒采样求值再跑 marching cubes 才能得到表面——那是一个
//! 独立的功能。这里退回到画它 `meshid` 指向的**包围网格**，并打一条警告，
//! 让你知道看到的是外壳而不是真正的形状。
//!
//! # 属性怎么落到三角形上
//!
//! 3MF 的「属性」是 `(pid, pindex)`：`pid` 指向一个属性组（材质组 / 颜色组 /
//! UV 组），`pindex` 是组里的第几项。三角形可以给三个角各自一个 `p1 p2 p3`
//! （逐顶点），也可以只给 `p1`（整面），都不给就继承物体的 `pid/pindex`。
//!
//! 这里先把每个三角形的三个角都解析成「属性组 + 项」，再按属性组分批：
//! 同一个材质组的同一项 → 同一个材质；颜色组 → 顶点色；UV 组 → 同一张贴图。
//! 角与角的属性不同，所以网格一律**不共享顶点**（逐角展开），然后由
//! `recompute_normals` 算平滑法线。
//!
//! 颜色是 sRGB 的 `#RRGGBB(AA)`，转成线性值。

use crate::{amf::srgb_to_linear, bad, limits, loader, texture_from_bytes, xml, zip};
use kasset::{LoadError, Resource, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::Material;
use kmath::{Mat4, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use ktexture::Texture;
use std::{collections::HashMap, path::PathBuf, sync::Arc};

loader! {
    /// 读 `.3mf`。
    ThreeMfLoader -> Model : ["3mf"] = MODEL_TYPE_UUID, parse
}

fn parse_color(text: &str) -> Vec4 {
    let hex = text.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(255) as f32 / 255.0;
    let alpha = if hex.len() >= 8 { byte(6) } else { 1.0 };
    Vec4::new(srgb_to_linear(byte(0)), srgb_to_linear(byte(2)), srgb_to_linear(byte(4)), alpha)
}

/// 3MF 的 `m00 m01 m02 m10 ... m32`：行向量约定的 4×3 矩阵。
fn parse_transform(text: Option<&str>) -> Mat4 {
    let Some(text) = text else { return Mat4::IDENTITY };
    let v: Vec<f32> = text.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect();
    if v.len() != 12 {
        return Mat4::IDENTITY;
    }
    // 行向量 × 矩阵 = 列向量约定下的转置，所以连续三个数就是一列。
    Mat4::from_cols_array(&[v[0], v[1], v[2], 0.0, v[3], v[4], v[5], 0.0, v[6], v[7], v[8], 0.0, v[9], v[10], v[11], 1.0])
}

/// 一个属性组。
enum Group {
    Base(Vec<(String, Vec4)>),
    Colors(Vec<Vec4>),
    Uvs { texture: String, coordinates: Vec<[f32; 2]> },
}

/// 一个 `.model` 文件里的资源（组、物体）。键是 `(文件, id)`。
struct Document {
    groups: HashMap<(String, String), Group>,
    objects: HashMap<(String, String), xml::Element>,
    textures: HashMap<(String, String), (String, bool)>,
    build: Vec<(String, String, Mat4)>,
}

/// 解析 3MF。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let archive = zip::Archive::open(&bytes)?;
    // 根模型的位置写在 `_rels/.rels` 里；找不到就用约定路径。
    let root_path = archive
        .read_named("_rels/.rels")
        .and_then(|rels| xml::parse(&rels).ok())
        .and_then(|rels| {
            rels.children_named("Relationship")
                .find(|r| r.attr("Type").is_some_and(|t| t.ends_with("/3dmodel")))
                .and_then(|r| r.attr("Target").map(|t| t.trim_start_matches('/').to_string()))
        })
        .unwrap_or_else(|| "3D/3dmodel.model".into());

    let mut document = Document {
        groups: HashMap::new(),
        objects: HashMap::new(),
        textures: HashMap::new(),
        build: Vec::new(),
    };
    let mut pending = vec![root_path.clone()];
    let mut loaded = Vec::new();
    while let Some(file) = pending.pop() {
        if loaded.contains(&file) || loaded.len() > 256 {
            continue;
        }
        let Some(data) = archive.read_named(&file) else {
            if file == root_path {
                return Err(bad(format!("3MF 里找不到根模型 {file}")));
            }
            klog::warn!("3MF 引用的模型文件 {file} 不存在");
            continue;
        };
        let root = xml::parse(&data)?;
        read_model(&root, &file, &mut document, &mut pending, file == root_path);
        loaded.push(file);
    }

    // 贴图：按路径读一次，所有 UV 组共用。
    let mut texture_cache: HashMap<String, Option<Resource<Texture>>> = HashMap::new();
    for (texture_path, _) in document.textures.values() {
        texture_cache.entry(texture_path.clone()).or_insert_with(|| {
            archive
                .read_named(texture_path)
                .and_then(|b| texture_from_bytes(&format!("{}#{texture_path}", path.display()), &b, false))
        });
    }

    let mut builder = Builder {
        document: &document,
        textures: &texture_cache,
        meshes: Vec::new(),
        materials: vec![Material::standard().with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0)).with_roughness(0.5)],
        material_keys: HashMap::new(),
        nodes: Vec::new(),
        vertex_total: 0,
    };
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "3MF".into());
    builder.nodes.push(ModelNode {
        name,
        ..Default::default()
    });
    let mut children = Vec::new();
    for (file, id, transform) in document.build.clone() {
        if let Some(node) = builder.object(&file, &id, transform, 0)? {
            children.push(node);
        }
    }
    builder.nodes[0].children = children;
    if builder.meshes.is_empty() {
        return Err(bad("3MF 里没有网格"));
    }
    Ok(Model::new(builder.meshes, builder.materials, builder.nodes, vec![0]))
}

fn read_model(root: &xml::Element, file: &str, document: &mut Document, pending: &mut Vec<String>, is_root: bool) {
    let key = |id: &str| (file.to_string(), id.to_string());
    if let Some(resources) = root.child("resources") {
        for element in &resources.children {
            let Some(id) = element.attr("id") else { continue };
            match element.name.as_str() {
                "basematerials" => {
                    let bases = element
                        .children_named("base")
                        .map(|b| (b.attr("name").unwrap_or("").to_string(), parse_color(b.attr("displaycolor").unwrap_or("#CCCCCC"))))
                        .collect();
                    document.groups.insert(key(id), Group::Base(bases));
                }
                "colorgroup" => {
                    let colors = element.children_named("color").map(|c| parse_color(c.attr("color").unwrap_or("#FFFFFF"))).collect();
                    document.groups.insert(key(id), Group::Colors(colors));
                }
                "texture2d" => {
                    let texture = element.attr("path").unwrap_or("").trim_start_matches('/').to_string();
                    let clamp = element.attr("tilestyleu") == Some("clamp") || element.attr("tilestylev") == Some("clamp");
                    document.textures.insert(key(id), (texture, clamp));
                }
                "texture2dgroup" => {
                    let texture = element.attr("texid").unwrap_or("").to_string();
                    // 3MF 的 V 朝上（原点左下），引擎朝下。
                    let coordinates = element
                        .children_named("tex2coord")
                        .map(|t| [t.attr_f32("u").unwrap_or(0.0), 1.0 - t.attr_f32("v").unwrap_or(0.0)])
                        .collect();
                    document.groups.insert(key(id), Group::Uvs { texture, coordinates });
                }
                "object" => {
                    if let Some(components) = element.child("components") {
                        for component in components.children_named("component") {
                            if let Some(path) = component.attr("path") {
                                pending.push(path.trim_start_matches('/').to_string());
                            }
                        }
                    }
                    document.objects.insert(key(id), element.clone());
                }
                _ => {}
            }
        }
    }
    if is_root && let Some(build) = root.child("build") {
        for item in build.children_named("item") {
            let file = item.attr("path").map_or_else(|| file.to_string(), |p| p.trim_start_matches('/').to_string());
            if let Some(id) = item.attr("objectid") {
                document.build.push((file, id.to_string(), parse_transform(item.attr("transform"))));
            }
        }
    }
}

/// 一个角解析出来的属性。
#[derive(Clone, Copy, PartialEq)]
enum Corner {
    None,
    /// 材质组第几项 → 材质表里的序号。
    Material(usize),
    Color(Vec4),
    Uv { material: usize, uv: [f32; 2] },
}

struct Builder<'a> {
    document: &'a Document,
    textures: &'a HashMap<String, Option<Resource<Texture>>>,
    meshes: Vec<Mesh>,
    materials: Vec<Material>,
    /// 材质去重：`材质组 (文件, id, 项)` 或 `贴图组 (文件, id)`。
    material_keys: HashMap<(String, String, usize), usize>,
    nodes: Vec<ModelNode>,
    vertex_total: usize,
}

impl Builder<'_> {
    fn material_for_base(&mut self, file: &str, id: &str, index: usize) -> usize {
        let key = (file.to_string(), id.to_string(), index);
        if let Some(&m) = self.material_keys.get(&key) {
            return m;
        }
        let (name, color) = match self.document.groups.get(&(file.to_string(), id.to_string())) {
            Some(Group::Base(bases)) => bases.get(index).cloned().unwrap_or_default(),
            _ => (String::new(), Vec4::splat(0.8)),
        };
        let mut material = Material::standard().with_base_color(color).with_roughness(0.5);
        material.set_name(name);
        if color.w < 1.0 {
            material.set_blend_mode(kmaterial::BlendMode::Alpha);
        }
        self.materials.push(material);
        self.material_keys.insert(key, self.materials.len() - 1);
        self.materials.len() - 1
    }

    fn material_for_texture(&mut self, file: &str, texture_id: &str) -> usize {
        let key = (file.to_string(), format!("tex:{texture_id}"), 0);
        if let Some(&m) = self.material_keys.get(&key) {
            return m;
        }
        let mut material = Material::standard().with_base_color(Vec4::ONE).with_roughness(0.6);
        if let Some((path, clamp)) = self.document.textures.get(&(file.to_string(), texture_id.to_string()))
            && let Some(Some(texture)) = self.textures.get(path)
        {
            let mut texture = texture.data_ref().expect("内存里建的资源一定就绪").clone();
            if *clamp {
                let mut sampler = texture.sampler();
                sampler.wrap_u = ktexture::WrapMode::ClampToEdge;
                sampler.wrap_v = ktexture::WrapMode::ClampToEdge;
                texture = texture.with_sampler(sampler);
            }
            material = material.with_base_color_texture(Resource::new_ok(format!("{path}#{clamp}"), texture));
        }
        self.materials.push(material);
        self.material_keys.insert(key, self.materials.len() - 1);
        self.materials.len() - 1
    }

    fn resolve(&mut self, file: &str, pid: Option<&str>, index: Option<usize>) -> Corner {
        let (Some(pid), Some(index)) = (pid, index) else { return Corner::None };
        match self.document.groups.get(&(file.to_string(), pid.to_string())) {
            Some(Group::Base(_)) => Corner::Material(self.material_for_base(file, pid, index)),
            Some(Group::Colors(colors)) => colors.get(index).map_or(Corner::None, |c| Corner::Color(*c)),
            Some(Group::Uvs { texture, coordinates }) => {
                let uv = coordinates.get(index).copied().unwrap_or([0.0, 0.0]);
                let texture = texture.clone();
                Corner::Uv {
                    material: self.material_for_texture(file, &texture),
                    uv,
                }
            }
            None => Corner::None,
        }
    }

    /// 实例化一个物体，返回新节点号。组件递归展开，深度超过 32 视为循环引用。
    fn object(&mut self, file: &str, id: &str, transform: Mat4, depth: usize) -> Result<Option<usize>, LoadError> {
        if depth > 32 {
            return Err(bad("3MF 组件嵌套过深（多半是循环引用）"));
        }
        let Some(object) = self.document.objects.get(&(file.to_string(), id.to_string())) else {
            klog::warn!("3MF 引用了不存在的物体 {file}#{id}");
            return Ok(None);
        };
        let (scale, rotation, position) = transform.to_scale_rotation_translation();
        let index = self.nodes.len();
        self.nodes.push(ModelNode {
            name: object.attr("name").map_or_else(|| format!("Object{id}"), str::to_string),
            transform: NodeTransform { position, rotation, scale },
            ..Default::default()
        });

        if let Some(mesh) = object.child("mesh") {
            let parts = self.mesh(file, object, mesh)?;
            self.nodes[index].parts = parts;
        } else if let Some(levelset) = object.child("levelset")
            && let Some(mesh_id) = levelset.attr("meshid")
            && let Some(source) = self.document.objects.get(&(file.to_string(), mesh_id.to_string()))
            && let Some(mesh) = source.child("mesh")
        {
            klog::warn!("3MF 物体 {id} 是隐式函数（levelset），不求值，画它的包围网格 {mesh_id}");
            let parts = self.mesh(file, source, mesh)?;
            self.nodes[index].parts = parts;
        }
        if let Some(components) = object.child("components") {
            let mut children = Vec::new();
            for component in components.children_named("component") {
                let component_file = component.attr("path").map_or_else(|| file.to_string(), |p| p.trim_start_matches('/').to_string());
                let Some(object_id) = component.attr("objectid") else { continue };
                if let Some(child) = self.object(&component_file, object_id, parse_transform(component.attr("transform")), depth + 1)? {
                    children.push(child);
                }
            }
            self.nodes[index].children = children;
        }
        Ok(Some(index))
    }

    fn mesh(&mut self, file: &str, object: &xml::Element, mesh: &xml::Element) -> Result<Vec<MeshPart>, LoadError> {
        let positions: Vec<Vec3> = mesh
            .child("vertices")
            .map(|v| {
                v.children_named("vertex")
                    .map(|p| Vec3::new(p.attr_f32("x").unwrap_or(0.0), p.attr_f32("y").unwrap_or(0.0), p.attr_f32("z").unwrap_or(0.0)))
                    .collect()
            })
            .unwrap_or_default();
        let object_pid = object.attr("pid");
        let object_index = object.attr_usize("pindex");

        // 按「材质号」分批：None 表示用顶点色 / 默认材质。
        let mut batches: HashMap<Option<usize>, (Vec<Vertex>, bool)> = HashMap::new();
        let triangles = mesh.child("triangles");
        for triangle in triangles.iter().flat_map(|t| t.children_named("triangle")) {
            let v = [triangle.attr_usize("v1"), triangle.attr_usize("v2"), triangle.attr_usize("v3")];
            let Some(v) = v.iter().map(|i| i.and_then(|i| positions.get(i).copied())).collect::<Option<Vec<_>>>() else {
                return Err(bad("3MF 三角形引用了不存在的顶点"));
            };
            let pid = triangle.attr("pid").or(object_pid);
            let p1 = triangle.attr_usize("p1").or(if triangle.attr("pid").is_none() { object_index } else { None });
            let p2 = triangle.attr_usize("p2").or(p1);
            let p3 = triangle.attr_usize("p3").or(p1);
            let corners = [self.resolve(file, pid, p1), self.resolve(file, pid, p2), self.resolve(file, pid, p3)];
            let material = match corners[0] {
                Corner::Material(m) | Corner::Uv { material: m, .. } => Some(m),
                _ => None,
            };
            let batch = batches.entry(material).or_insert_with(|| (Vec::new(), false));
            for (k, corner) in corners.iter().enumerate() {
                let (color, uv) = match *corner {
                    Corner::Color(c) => {
                        batch.1 = true;
                        (c.truncate().to_array(), [0.0, 0.0])
                    }
                    Corner::Uv { uv, .. } => ([1.0; 3], uv),
                    _ => ([1.0; 3], [0.0, 0.0]),
                };
                batch.0.push(Vertex {
                    position: v[k].to_array(),
                    color,
                    uv,
                    ..Default::default()
                });
            }
        }

        let mut parts = Vec::new();
        let mut keys: Vec<Option<usize>> = batches.keys().copied().collect();
        keys.sort();
        for key in keys {
            let (vertices, colored) = batches.remove(&key).expect("键来自这张表");
            self.vertex_total += vertices.len();
            if self.vertex_total > limits::VERTICES {
                return Err(bad("3MF 顶点数超过上限"));
            }
            let indices = (0..vertices.len() as u32).collect();
            let mut mesh = Mesh::new(vertices, indices);
            if !mesh.is_valid() || mesh.triangle_count() == 0 {
                continue;
            }
            // 逐角展开的网格先按位置焊接再算法线，否则整个模型是一片片平的。
            mesh.recompute_normals();
            let material = match key {
                Some(m) => m,
                None if colored => {
                    self.materials.push(Material::standard().with_base_color(Vec4::ONE).with_roughness(0.5));
                    self.materials.len() - 1
                }
                None => 0,
            };
            parts.push(MeshPart {
                mesh: self.meshes.len(),
                material: Some(material),
            });
            self.meshes.push(mesh);
        }
        Ok(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_are_row_vector_4x3() {
        // 平移放在最后三个数里。
        let m = parse_transform(Some("1 0 0 0 1 0 0 0 1 5 6 7"));
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(5.0, 6.0, 7.0));
        // 绕 Z 转 90°：x 轴 → y 轴。行向量约定下第一行是 x 轴的像。
        let r = parse_transform(Some("0 1 0 -1 0 0 0 0 1 0 0 0"));
        assert!((r.transform_point3(Vec3::X) - Vec3::Y).length() < 1e-6);
    }

    #[test]
    fn colors_are_srgb_hex() {
        let c = parse_color("#FF000080");
        assert_eq!(c.x, 1.0);
        assert!((c.w - 128.0 / 255.0).abs() < 1e-6);
        assert!((parse_color("#808080").x - 0.2158605).abs() < 1e-4);
    }
}
