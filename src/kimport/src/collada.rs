//! Collada（`.dae`）：XML 的场景交换格式。
//!
//! # 支持
//!
//! | 部分 | |
//! |---|---|
//! | 几何 | `triangles` / `polylist` / `polygons`（多边形按扇形三角化），位置 / 法线 / 多套 UV / 顶点色 |
//! | 材质 | `profile_COMMON` 的 `phong` / `blinn` / `lambert` / `constant`：漫反射颜色或贴图、自发光、透明度、光泽度、双面 |
//! | 场景 | 节点树，`matrix` / `translate` / `rotate` / `scale` 变换栈，`instance_node`，`bind_material` |
//! | 蒙皮 | `controller` → `skin`：`bind_shape_matrix`、逆绑定矩阵、逐顶点权重（取最大的 4 个并归一化） |
//! | 动画 | 任意变换元素（整矩阵或单分量，如 `rotateZ.ANGLE`）上的通道，重采样成 TRS 轨道 |
//! | 运动学 | `kinematics_model` 的关节（旋转 / 平移、轴、限位）+ `bind_joint_axis` 绑到变换元素，见 [`Collada::joints`] |
//! | 坐标系 | `up_axis`（Z 朝上的文件整体绕 X 转 -90°）、`unit` 缩放 |
//!
//! 不支持：`tristrips` / `trifans`、形变控制器（`morph`）、灯光与相机、
//! `profile_GLSL` 等着色器 profile、物理段。
//!
//! # 动画为什么要重采样
//!
//! Collada 的动画通道挂在**变换元素**上：一个节点可以是
//! `translate · rotateZ · rotateY · rotateX · scale`，而动画只动其中的
//! `rotateY.ANGLE`。引擎的轨道是 TRS。所以对每个被动画的节点：取它所有
//! 通道关键帧时刻的并集，在每个时刻按变换栈重新算一遍局部矩阵，再分解成
//! TRS。这样整矩阵动画、单分量动画、混着来的都走同一条路。
//!
//! # 运动学怎么对上节点
//!
//! 规范里的绑定链是 `bind_joint_axis` → 运动学场景的参数 → 关节系统的
//! SIDREF → 运动学模型的关节，四层间接。这里走捷径：
//!
//! - `target` 的**最后一段**是变换元素的 sid（例如 `node_joint_1_axis0`），
//!   全场景里找带这个 sid 的变换元素，它所在的节点就是被驱动的节点；
//! - 轴参数名以 `{关节 sid}_{轴 sid}` 结尾，据此在运动学模型里找关节。
//!
//! URDF / OpenRAVE / Blender 导出的文件都满足这两条（three.js 的样本就是
//! OpenRAVE 导出的）；不满足时这个关节被跳过并打警告。

use crate::{bad, limits, loader, xml::Element};
use kanim::{AnimationClip, Channel, Curve, Interpolation, Track};
use kasset::{LoadError, Resource, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kgltf::{MODEL_TYPE_UUID, MeshPart, Model, ModelNode, ModelSkin, NodeTransform};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3, Vec4};
use kmesh::{Mesh, SkinVertex, Vertex};
use ktexture::Texture;
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};

/// [`Collada`] 的资源类型标识。
pub const COLLADA_TYPE_UUID: Uuid = uuid!("c0a11ada-2e0f-4b7a-9d31-7f5e9b8c4a10");

/// 运动学模型里的一个关节，已经绑到了模型的某个节点上。
#[derive(Debug, Clone)]
pub struct KinematicJoint {
    /// 关节名（`kinematics_model` 里的 `name`，没有时用 sid）。
    pub name: String,
    /// 旋转关节（值是角度，单位度）还是平移关节（值是距离）。
    pub revolute: bool,
    /// 关节轴（节点局部空间）。
    pub axis: Vec3,
    /// 下限。
    pub min: f32,
    /// 上限。
    pub max: f32,
    /// 被驱动的节点（[`Model::nodes`] 的下标）。
    pub node: usize,
    /// 变换栈里被绑定元素**之前**的部分。
    pub before: Mat4,
    /// 变换栈里被绑定元素**之后**的部分。
    pub after: Mat4,
    /// 文件里写的初始值。
    pub zero: f32,
}

impl KinematicJoint {
    /// 关节值是 `value` 时节点的局部变换。
    ///
    /// 限位之外的值会被夹回范围内——机械臂转不过限位，three.js 的
    /// `setJointValue` 同样拒绝越界值。
    pub fn transform(&self, value: f32) -> NodeTransform {
        let value = if self.min < self.max { value.clamp(self.min, self.max) } else { value };
        let axis = self.axis.try_normalize().unwrap_or(Vec3::Z);
        let motion = if self.revolute {
            Mat4::from_axis_angle(axis, value.to_radians())
        } else {
            Mat4::from_translation(axis * value)
        };
        let (scale, rotation, position) = (self.before * motion * self.after).to_scale_rotation_translation();
        NodeTransform { position, rotation, scale }
    }

    /// 这个关节能不能动（上下限相同的是固定关节）。
    pub fn is_static(&self) -> bool {
        self.min >= self.max || self.axis.length_squared() < 1e-12
    }
}

/// 一个 Collada 文件的完整导入结果：模型 + 运动学关节。
#[derive(Debug, Clone)]
pub struct Collada {
    /// 场景。
    pub model: Model,
    /// 运动学关节，没有运动学段时为空。
    pub joints: Vec<KinematicJoint>,
}

impl ResourceData for Collada {
    fn type_uuid(&self) -> Uuid {
        COLLADA_TYPE_UUID
    }
}

loader! {
    /// 读 `.dae`，产出 [`Model`]。连带同目录的贴图。
    ColladaLoader -> Model : ["dae"] = MODEL_TYPE_UUID, parse
}

loader! {
    /// 读 `.dae`，产出带运动学关节的 [`Collada`]。和 [`ColladaLoader`] 扩展名
    /// 相同，二者注册一个即可——要驱动机械臂关节时用这个。
    ColladaKinematicsLoader -> Collada : ["dae"] = COLLADA_TYPE_UUID, parse_collada
}

/// 解析成 [`Model`]。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    Ok(parse_collada(bytes, path, io).await?.model)
}

/// 解析成 [`Collada`]。
pub async fn parse_collada(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Collada, LoadError> {
    let root = crate::xml::parse(&bytes)?;
    let base = crate::base_dir(&path);
    let mut images = HashMap::new();
    for file in image_paths(&root) {
        if let Some(texture) = crate::load_texture(&io, &base, &file, false).await {
            images.insert(file, texture);
        }
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Collada".into());
    build(&root, &images, &name)
}

/// 文档里引用的全部图片路径（已经去掉 `file://` 前缀、还原 `%20`）。
pub fn image_paths(root: &Element) -> Vec<String> {
    let mut images = Vec::new();
    if let Some(library) = root.child("library_images") {
        for image in library.children_named("image") {
            if let Some(path) = image_path(image) {
                images.push(path);
            }
        }
    }
    images
}

fn image_path(image: &Element) -> Option<String> {
    // 1.4：<init_from>path</init_from>；1.5：<init_from><ref>path</ref></init_from>。
    let init = image.descendant("init_from")?;
    let raw = init.child("ref").map_or(init.text.trim(), |r| r.text.trim());
    if raw.is_empty() {
        return None;
    }
    let raw = raw.strip_prefix("file:///").or_else(|| raw.strip_prefix("file://")).unwrap_or(raw);
    Some(percent_decode(raw))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 按 id 索引全部元素。
fn index_ids<'a>(element: &'a Element, out: &mut HashMap<&'a str, &'a Element>) {
    if let Some(id) = element.attr("id") {
        out.entry(id).or_insert(element);
    }
    for child in &element.children {
        index_ids(child, out);
    }
}

fn url(value: Option<&str>) -> &str {
    value.unwrap_or("").trim_start_matches('#')
}

/// 一个 `<source>`：数据 + 步长。
struct Source {
    floats: Vec<f32>,
    names: Vec<String>,
    stride: usize,
}

fn read_source(element: &Element) -> Source {
    let floats = element.child("float_array").map(Element::floats).unwrap_or_default();
    let names = element
        .child("Name_array")
        .or_else(|| element.child("IDREF_array"))
        .map(|a| a.text.split_ascii_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let stride = element
        .find("technique_common/accessor")
        .and_then(|a| a.attr_usize("stride"))
        .unwrap_or(1)
        .max(1);
    Source { floats, names, stride }
}

/// Collada 的矩阵是**行主序**写的。
fn matrix_from_row_major(v: &[f32]) -> Mat4 {
    if v.len() < 16 {
        return Mat4::IDENTITY;
    }
    Mat4::from_cols_array(&[
        v[0], v[4], v[8], v[12], v[1], v[5], v[9], v[13], v[2], v[6], v[10], v[14], v[3], v[7], v[11], v[15],
    ])
}

/// 一个变换元素的矩阵。`values` 可以被动画覆盖。
fn transform_matrix(kind: &str, values: &[f32]) -> Option<Mat4> {
    let get = |i: usize| values.get(i).copied().unwrap_or(0.0);
    Some(match kind {
        "matrix" => matrix_from_row_major(values),
        "translate" => Mat4::from_translation(Vec3::new(get(0), get(1), get(2))),
        "rotate" => {
            let axis = Vec3::new(get(0), get(1), get(2));
            match axis.try_normalize() {
                Some(axis) => Mat4::from_axis_angle(axis, get(3).to_radians()),
                None => Mat4::IDENTITY,
            }
        }
        "scale" => Mat4::from_scale(Vec3::new(get(0), get(1), get(2))),
        "lookat" => {
            let eye = Vec3::new(get(0), get(1), get(2));
            let target = Vec3::new(get(3), get(4), get(5));
            let up = Vec3::new(get(6), get(7), get(8));
            Mat4::look_at_rh(eye, target, up).inverse()
        }
        _ => return None,
    })
}

fn decompose(matrix: Mat4) -> NodeTransform {
    let (scale, rotation, position) = matrix.to_scale_rotation_translation();
    NodeTransform {
        position,
        rotation: if rotation.is_finite() { rotation.normalize() } else { Quat::IDENTITY },
        scale,
    }
}

/// 一个待建的网格块：顶点、索引、每个顶点来自哪个位置下标（蒙皮权重按位置下标给）。
struct Primitive {
    symbol: Option<String>,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    position_index: Vec<usize>,
    has_normals: bool,
}

fn read_geometry(geometry: &Element) -> Result<Vec<Primitive>, LoadError> {
    let Some(mesh) = geometry.child("mesh") else { return Ok(Vec::new()) };
    let sources: HashMap<&str, Source> = mesh
        .children_named("source")
        .filter_map(|s| Some((s.attr("id")?, read_source(s))))
        .collect();
    // <vertices> 把若干输入打包成一个 VERTEX 语义。
    let mut vertex_inputs: Vec<(String, String, usize)> = Vec::new();
    if let Some(vertices) = mesh.child("vertices") {
        for input in vertices.children_named("input") {
            vertex_inputs.push((
                input.attr("semantic").unwrap_or("").to_string(),
                url(input.attr("source")).to_string(),
                0,
            ));
        }
    }

    let mut primitives = Vec::new();
    for element in &mesh.children {
        let kind = element.name.as_str();
        if !matches!(kind, "triangles" | "polylist" | "polygons") {
            if matches!(kind, "tristrips" | "trifans" | "lines" | "linestrips") {
                klog::warn!("Collada：跳过不支持的图元 <{kind}>");
            }
            continue;
        }
        // (语义, 源, 偏移, 集合号)
        let mut inputs: Vec<(String, String, usize, usize)> = Vec::new();
        for input in element.children_named("input") {
            let semantic = input.attr("semantic").unwrap_or("");
            let offset = input.attr_usize("offset").unwrap_or(0);
            let set = input.attr_usize("set").unwrap_or(0);
            if semantic == "VERTEX" {
                for (s, source, _) in &vertex_inputs {
                    inputs.push((s.clone(), source.clone(), offset, set));
                }
            } else {
                inputs.push((semantic.to_string(), url(input.attr("source")).to_string(), offset, set));
            }
        }
        let stride = inputs.iter().map(|i| i.2).max().unwrap_or(0) + 1;
        // 每个多边形的角数。
        let mut polygons: Vec<Vec<i64>> = Vec::new();
        match kind {
            "triangles" => {
                let p = element.child("p").map(Element::integers).unwrap_or_default();
                polygons.extend(p.chunks_exact(stride * 3).map(<[i64]>::to_vec));
            }
            "polylist" => {
                let counts = element.child("vcount").map(Element::integers).unwrap_or_default();
                let p = element.child("p").map(Element::integers).unwrap_or_default();
                let mut at = 0;
                for count in counts {
                    let n = count.max(0) as usize * stride;
                    if at + n > p.len() {
                        break;
                    }
                    polygons.push(p[at..at + n].to_vec());
                    at += n;
                }
            }
            _ => {
                for p in element.children_named("p") {
                    polygons.push(p.integers());
                }
            }
        }

        let find = |semantic: &str, set: usize| {
            inputs
                .iter()
                .filter(|i| i.0 == semantic)
                .find(|i| i.3 == set)
                .or_else(|| inputs.iter().find(|i| i.0 == semantic && set == 0))
                .and_then(|i| sources.get(i.1.as_str()).map(|s| (s, i.2)))
        };
        let position = find("POSITION", 0).ok_or_else(|| bad("Collada 网格没有 POSITION"))?;
        let normal = find("NORMAL", 0);
        let mut texcoord_sets: Vec<usize> = inputs.iter().filter(|i| i.0 == "TEXCOORD").map(|i| i.3).collect();
        texcoord_sets.sort_unstable();
        texcoord_sets.dedup();
        let uv0 = texcoord_sets.first().and_then(|&s| find("TEXCOORD", s));
        let uv1 = texcoord_sets.get(1).and_then(|&s| find("TEXCOORD", s));
        let color = find("COLOR", 0);

        let mut primitive = Primitive {
            symbol: element.attr("material").map(str::to_string),
            vertices: Vec::new(),
            indices: Vec::new(),
            position_index: Vec::new(),
            has_normals: normal.is_some(),
        };
        let mut dedupe: HashMap<Vec<i64>, u32> = HashMap::new();
        let fetch = |source: &Source, index: i64, k: usize| {
            source.floats.get(index.max(0) as usize * source.stride + k).copied().unwrap_or(0.0)
        };
        for polygon in polygons {
            let corners: Vec<u32> = polygon
                .chunks_exact(stride)
                .map(|corner| {
                    if let Some(&existing) = dedupe.get(corner) {
                        return existing;
                    }
                    let pi = corner[position.1];
                    let vertex = Vertex {
                        position: [fetch(position.0, pi, 0), fetch(position.0, pi, 1), fetch(position.0, pi, 2)],
                        normal: normal.map_or([0.0, 1.0, 0.0], |(s, o)| [fetch(s, corner[o], 0), fetch(s, corner[o], 1), fetch(s, corner[o], 2)]),
                        // Collada 的 V 朝上。
                        uv: uv0.map_or([0.0, 0.0], |(s, o)| [fetch(s, corner[o], 0), 1.0 - fetch(s, corner[o], 1)]),
                        uv1: uv1.map_or([0.0, 0.0], |(s, o)| [fetch(s, corner[o], 0), 1.0 - fetch(s, corner[o], 1)]),
                        color: color.map_or([1.0; 3], |(s, o)| [fetch(s, corner[o], 0), fetch(s, corner[o], 1), fetch(s, corner[o], 2)]),
                        ..Default::default()
                    };
                    primitive.vertices.push(vertex);
                    primitive.position_index.push(pi.max(0) as usize);
                    let index = primitive.vertices.len() as u32 - 1;
                    dedupe.insert(corner.to_vec(), index);
                    index
                })
                .collect();
            // 扇形三角化：凸多边形正确，凹多边形会有错面（Collada 导出器几乎只写凸的）。
            for k in 1..corners.len().saturating_sub(1) {
                primitive.indices.extend_from_slice(&[corners[0], corners[k], corners[k + 1]]);
            }
            if primitive.vertices.len() > limits::VERTICES {
                return Err(bad("Collada 顶点数超过上限"));
            }
        }
        primitives.push(primitive);
    }
    Ok(primitives)
}

/// 一个 effect 读出来的外观。
fn read_effect(effect: &Element, images: &HashMap<String, Resource<Texture>>, ids: &HashMap<&str, &Element>) -> Material {
    let mut material = Material::standard().with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0)).with_metallic(0.0).with_roughness(0.6);
    let Some(profile) = effect.child("profile_COMMON") else { return material };
    // newparam：sampler2D → surface → image。
    let params: HashMap<&str, &Element> = profile
        .children_named("newparam")
        .filter_map(|p| Some((p.attr("sid")?, p)))
        .collect();
    let resolve_texture = |name: &str| -> Option<Resource<Texture>> {
        let mut image_id = name.to_string();
        if let Some(param) = params.get(name) {
            if let Some(sampler) = param.child("sampler2D") {
                if let Some(instance) = sampler.child("instance_image") {
                    image_id = url(instance.attr("url")).to_string();
                } else {
                    let surface_name = sampler.child_text("source");
                    image_id = params
                        .get(surface_name)
                        .and_then(|s| s.child("surface"))
                        .and_then(|s| s.child("init_from"))
                        .map_or(surface_name.to_string(), |i| i.text.trim().to_string());
                }
            }
        }
        let path = ids.get(image_id.as_str()).and_then(|image| image_path(image))?;
        images.get(&path).cloned()
    };
    let Some(technique) = profile.child("technique") else { return material };
    let Some(shading) = technique
        .children
        .iter()
        .find(|c| matches!(c.name.as_str(), "phong" | "blinn" | "lambert" | "constant"))
    else {
        return material;
    };
    let color_of = |name: &str| -> Option<Vec4> {
        let color = shading.child(name)?.child("color")?.floats();
        (color.len() >= 3).then(|| Vec4::new(color[0], color[1], color[2], color.get(3).copied().unwrap_or(1.0)))
    };
    let float_of = |name: &str| -> Option<f32> { shading.child(name)?.child("float")?.text.trim().parse().ok() };

    let mut base = Vec4::new(0.8, 0.8, 0.8, 1.0);
    if let Some(diffuse) = shading.child("diffuse") {
        if let Some(color) = color_of("diffuse") {
            base = color;
        }
        if let Some(texture) = diffuse.child("texture").and_then(|t| resolve_texture(t.attr("texture")?)) {
            material = material.with_base_color_texture(texture);
            base = Vec4::ONE;
        }
    }
    if shading.name == "constant" {
        // constant 没有漫反射，颜色写在 emission 里。
        if let Some(color) = color_of("emission") {
            base = color;
        }
    } else if let Some(emission) = color_of("emission")
        && emission.truncate().max_element() > 0.0
    {
        material.set(kpbr::standard::EMISSIVE, emission.truncate());
    }
    // 透明度：`transparent` 颜色 × `transparency` 系数（A_ONE 约定）。
    let transparency = float_of("transparency").unwrap_or(1.0);
    let transparent = color_of("transparent").map_or(1.0, |c| c.w);
    let opacity = (transparent * transparency).clamp(0.0, 1.0);
    // 有些导出器把 transparency 写反（0 = 不透明）；全透明几乎一定是写反了。
    let opacity = if opacity <= 0.001 { 1.0 } else { opacity };
    base.w *= opacity;
    if base.w < 0.999 {
        material.set_blend_mode(kmaterial::BlendMode::Alpha);
    }
    material = material.with_base_color(base);
    if let Some(shininess) = float_of("shininess") {
        // Blinn-Phong 指数 → GGX 粗糙度的常用换算。
        material.set_roughness((2.0 / (shininess.max(0.0) + 2.0)).sqrt().clamp(0.05, 1.0));
    }
    let mut double_sided = Vec::new();
    effect.descendants("double_sided", &mut double_sided);
    if double_sided.iter().any(|d| d.text.trim() == "1") {
        material.set_double_sided(true);
    }
    material
}

/// 建模型用到的全部状态。
struct Builder<'a> {
    ids: HashMap<&'a str, &'a Element>,
    images: &'a HashMap<String, Resource<Texture>>,
    meshes: Vec<Mesh>,
    materials: Vec<Material>,
    material_index: HashMap<String, usize>,
    nodes: Vec<ModelNode>,
    skins: Vec<ModelSkin>,
    /// 节点序号 → 它的变换栈 `(元素名, sid, 值)`。
    stacks: Vec<Vec<(String, Option<String>, Vec<f32>)>>,
    /// 按 sid / id / name 找节点。
    by_sid: HashMap<String, usize>,
    by_id: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
    /// 待挂骨架：`(网格所在节点, 控制器, skeleton 根)`。
    pending_skins: Vec<(usize, &'a Element, Vec<String>, Vec<usize>)>,
    geometry_cache: HashMap<String, Vec<(Option<String>, usize, Vec<usize>)>>,
    fallback_material: usize,
}

impl<'a> Builder<'a> {
    fn material(&mut self, id: &str) -> usize {
        if let Some(&index) = self.material_index.get(id) {
            return index;
        }
        let Some(material) = self.ids.get(id) else { return self.fallback_material };
        let effect = self.ids.get(url(material.child("instance_effect").and_then(|e| e.attr("url"))));
        let mut built = effect.map_or_else(Material::standard, |e| read_effect(e, self.images, &self.ids));
        built.set_name(material.attr("name").unwrap_or(id));
        self.materials.push(built);
        let index = self.materials.len() - 1;
        self.material_index.insert(id.to_string(), index);
        index
    }

    /// 读一个 geometry，缓存成 `(材质符号, 网格号, 位置下标表)`。
    fn geometry(&mut self, id: &str, bind_shape: Option<Mat4>) -> Result<Vec<(Option<String>, usize, Vec<usize>)>, LoadError> {
        let key = format!("{id}#{}", bind_shape.is_some());
        if let Some(cached) = self.geometry_cache.get(&key) {
            return Ok(cached.clone());
        }
        let Some(&geometry) = self.ids.get(id) else {
            klog::warn!("Collada 引用了不存在的几何 {id}");
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for mut primitive in read_geometry(geometry)? {
            if let Some(matrix) = bind_shape {
                // 蒙皮网格先乘 bind_shape_matrix：之后的逆绑定矩阵是相对它的。
                let normal_matrix = matrix.inverse().transpose();
                for v in &mut primitive.vertices {
                    v.position = matrix.transform_point3(Vec3::from_array(v.position)).to_array();
                    v.normal = normal_matrix.transform_vector3(Vec3::from_array(v.normal)).normalize_or_zero().to_array();
                }
            }
            let mut mesh = Mesh::new(primitive.vertices, primitive.indices);
            if !mesh.is_valid() || mesh.triangle_count() == 0 {
                continue;
            }
            if !primitive.has_normals {
                mesh.recompute_normals();
            }
            mesh.recompute_tangents();
            self.meshes.push(mesh);
            out.push((primitive.symbol, self.meshes.len() - 1, primitive.position_index));
        }
        self.geometry_cache.insert(key, out.clone());
        Ok(out)
    }

    /// `bind_material` 里的「符号 → 材质 id」。
    fn bindings(instance: &Element) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let mut found = Vec::new();
        instance.descendants("instance_material", &mut found);
        for m in found {
            if let (Some(symbol), Some(target)) = (m.attr("symbol"), m.attr("target")) {
                map.insert(symbol.to_string(), url(Some(target)).to_string());
            }
        }
        map
    }

    fn node(&mut self, element: &'a Element, depth: usize) -> Result<usize, LoadError> {
        if depth > 256 || self.nodes.len() > limits::NODES {
            return Err(bad("Collada 节点树过深或过大"));
        }
        let index = self.nodes.len();
        let mut stack = Vec::new();
        let mut local = Mat4::IDENTITY;
        for child in &element.children {
            let values = child.floats();
            if let Some(matrix) = transform_matrix(&child.name, &values) {
                local *= matrix;
                stack.push((child.name.clone(), child.attr("sid").map(str::to_string), values));
            }
        }
        let name = element.attr("name").or(element.attr("id")).unwrap_or("").to_string();
        self.nodes.push(ModelNode {
            name: name.clone(),
            transform: decompose(local),
            ..Default::default()
        });
        self.stacks.push(stack);
        if let Some(sid) = element.attr("sid") {
            self.by_sid.entry(sid.to_string()).or_insert(index);
        }
        if let Some(id) = element.attr("id") {
            self.by_id.entry(id.to_string()).or_insert(index);
        }
        self.by_name.entry(name).or_insert(index);

        let mut parts = Vec::new();
        let mut children = Vec::new();
        for child in &element.children {
            match child.name.as_str() {
                "instance_geometry" => {
                    let bindings = Self::bindings(child);
                    for (symbol, mesh, _) in self.geometry(url(child.attr("url")), None)? {
                        let material = symbol
                            .as_ref()
                            .and_then(|s| bindings.get(s).cloned())
                            .map_or(self.fallback_material, |id| self.material(&id));
                        parts.push(MeshPart { mesh, material: Some(material) });
                    }
                }
                "instance_controller" => {
                    let Some(&controller) = self.ids.get(url(child.attr("url"))) else { continue };
                    let Some(skin) = controller.child("skin") else {
                        klog::warn!("Collada：跳过不支持的控制器（morph）");
                        continue;
                    };
                    let bind_shape = skin.child("bind_shape_matrix").map(|m| matrix_from_row_major(&m.floats()));
                    let bindings = Self::bindings(child);
                    let skeletons: Vec<String> = child.children_named("skeleton").map(|s| url(Some(s.text.trim())).to_string()).collect();
                    let mut meshes = Vec::new();
                    for (symbol, mesh, positions) in self.geometry(url(skin.attr("source")), Some(bind_shape.unwrap_or(Mat4::IDENTITY)))? {
                        let material = symbol
                            .as_ref()
                            .and_then(|s| bindings.get(s).cloned())
                            .map_or(self.fallback_material, |id| self.material(&id));
                        parts.push(MeshPart { mesh, material: Some(material) });
                        let _ = positions;
                        meshes.push(mesh);
                    }
                    self.pending_skins.push((index, controller, skeletons, meshes));
                }
                "node" => children.push(self.node(child, depth + 1)?),
                "instance_node" => {
                    if let Some(&library) = self.ids.get(url(child.attr("url"))) {
                        children.push(self.node(library, depth + 1)?);
                    }
                }
                _ => {}
            }
        }
        self.nodes[index].parts = parts;
        self.nodes[index].children = children;
        Ok(index)
    }

    /// 在 `roots` 的子树里按 sid 找节点（蒙皮关节名在 skeleton 范围内唯一）。
    fn find_in(&self, roots: &[usize], sid_or_name: &str, sid_map: &HashMap<usize, String>) -> Option<usize> {
        let mut stack: Vec<usize> = roots.to_vec();
        while let Some(n) = stack.pop() {
            if sid_map.get(&n).is_some_and(|s| s == sid_or_name) {
                return Some(n);
            }
            stack.extend(self.nodes[n].children.iter().copied());
        }
        None
    }

    fn attach_skins(&mut self, sid_map: &HashMap<usize, String>) -> Result<(), LoadError> {
        for (node, controller, skeletons, meshes) in std::mem::take(&mut self.pending_skins) {
            let Some(skin) = controller.child("skin") else { continue };
            let sources: HashMap<&str, Source> = skin
                .children_named("source")
                .filter_map(|s| Some((s.attr("id")?, read_source(s))))
                .collect();
            let joints_element = skin.child("joints");
            fn input<'e>(element: Option<&'e Element>, semantic: &str) -> Option<&'e Element> {
                element.and_then(|e| e.children.iter().find(|i| i.name == "input" && i.attr("semantic") == Some(semantic)))
            }
            let joint_names = input(joints_element, "JOINT")
                .and_then(|i| sources.get(url(i.attr("source"))))
                .map(|s| s.names.clone())
                .unwrap_or_default();
            let inverse_binds: Vec<Mat4> = input(joints_element, "INV_BIND_MATRIX")
                .and_then(|i| sources.get(url(i.attr("source"))))
                .map(|s| s.floats.chunks_exact(16).map(matrix_from_row_major).collect())
                .unwrap_or_default();
            let roots: Vec<usize> = skeletons.iter().filter_map(|s| self.by_id.get(s).copied()).collect();
            let all_roots: Vec<usize> = if roots.is_empty() { (0..self.nodes.len()).collect() } else { roots };
            let joints: Vec<usize> = joint_names
                .iter()
                .map(|name| {
                    self.find_in(&all_roots, name, sid_map)
                        .or_else(|| self.by_id.get(name).copied())
                        .or_else(|| self.by_name.get(name).copied())
                        .unwrap_or_else(|| {
                            klog::warn!("Collada 蒙皮关节 {name} 找不到对应节点");
                            0
                        })
                })
                .collect();

            // 逐位置的权重。
            let weights_element = skin.child("vertex_weights");
            let weight_source = input(weights_element, "WEIGHT").and_then(|i| sources.get(url(i.attr("source"))));
            let joint_offset = input(weights_element, "JOINT").and_then(|i| i.attr_usize("offset")).unwrap_or(0);
            let weight_offset = input(weights_element, "WEIGHT").and_then(|i| i.attr_usize("offset")).unwrap_or(1);
            let stride = weights_element.map_or(2, |w| w.children_named("input").count().max(1));
            let counts = weights_element.and_then(|w| w.child("vcount")).map(Element::integers).unwrap_or_default();
            let v = weights_element.and_then(|w| w.child("v")).map(Element::integers).unwrap_or_default();
            let mut per_position: Vec<SkinVertex> = Vec::with_capacity(counts.len());
            let mut at = 0usize;
            for count in counts {
                let mut influences: Vec<(u16, f32)> = Vec::new();
                for _ in 0..count.max(0) {
                    let joint = v.get(at + joint_offset).copied().unwrap_or(0);
                    let weight_index = v.get(at + weight_offset).copied().unwrap_or(0).max(0) as usize;
                    let weight = weight_source.and_then(|s| s.floats.get(weight_index)).copied().unwrap_or(0.0);
                    // 关节号 -1 表示「绑定形状本身」，权重不属于任何骨头。
                    if joint >= 0 {
                        influences.push((joint as u16, weight));
                    }
                    at += stride;
                }
                influences.sort_by(|a, b| b.1.total_cmp(&a.1));
                influences.truncate(4);
                let mut skin_vertex = SkinVertex { joints: [0; 4], weights: [0.0; 4] };
                for (k, (joint, weight)) in influences.into_iter().enumerate() {
                    skin_vertex.joints[k] = joint;
                    skin_vertex.weights[k] = weight;
                }
                per_position.push(skin_vertex);
            }

            let skin_index = self.skins.len();
            let mut inverse_bind = inverse_binds;
            inverse_bind.resize(joints.len(), Mat4::IDENTITY);
            self.skins.push(ModelSkin { joints, inverse_bind, skeleton: None });
            for mesh_index in meshes {
                // 网格顶点 → 位置下标 → 权重。
                let Some((_, _, positions)) = self
                    .geometry_cache
                    .values()
                    .flatten()
                    .find(|(_, m, _)| *m == mesh_index)
                    .cloned()
                else {
                    continue;
                };
                let skin: Vec<SkinVertex> = positions
                    .iter()
                    .map(|&p| per_position.get(p).copied().unwrap_or(SkinVertex { joints: [0; 4], weights: [1.0, 0.0, 0.0, 0.0] }))
                    .collect();
                self.meshes[mesh_index] = self.meshes[mesh_index].clone().with_skin(skin);
            }
            self.nodes[node].skin = Some(skin_index);
        }
        Ok(())
    }
}

/// 从解析好的 XML 建出 [`Collada`]。`images` 是 [`image_paths`] 里的路径 → 贴图。
pub fn build(root: &Element, images: &HashMap<String, Resource<Texture>>, name: &str) -> Result<Collada, LoadError> {
    if root.name != "COLLADA" {
        return Err(bad("不是 Collada 文档（根元素不是 <COLLADA>）"));
    }
    let mut ids = HashMap::new();
    index_ids(root, &mut ids);
    let mut builder = Builder {
        ids,
        images,
        meshes: Vec::new(),
        materials: vec![Material::standard().with_base_color(Vec4::new(0.8, 0.8, 0.8, 1.0)).with_roughness(0.6)],
        material_index: HashMap::new(),
        nodes: Vec::new(),
        skins: Vec::new(),
        stacks: Vec::new(),
        by_sid: HashMap::new(),
        by_id: HashMap::new(),
        by_name: HashMap::new(),
        pending_skins: Vec::new(),
        geometry_cache: HashMap::new(),
        fallback_material: 0,
    };

    // 根节点：坐标系换算放在这里，子树保持文件原样（动画、蒙皮都按原坐标算）。
    let up = root.find("asset/up_axis").map_or("Y_UP", |u| u.text.trim());
    let meter = root.find("asset/unit").and_then(|u| u.attr_f32("meter")).filter(|m| *m > 0.0).unwrap_or(1.0);
    let rotation = match up {
        "Z_UP" => Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        "X_UP" => Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        _ => Quat::IDENTITY,
    };
    builder.nodes.push(ModelNode {
        name: name.to_string(),
        transform: NodeTransform {
            position: Vec3::ZERO,
            rotation,
            scale: Vec3::splat(meter),
        },
        ..Default::default()
    });
    builder.stacks.push(Vec::new());

    let scene_url = root.find("scene/instance_visual_scene").and_then(|s| s.attr("url"));
    let scene = match scene_url {
        Some(u) => builder.ids.get(url(Some(u))).copied(),
        None => root.find("library_visual_scenes/visual_scene"),
    }
    .ok_or_else(|| bad("Collada 里没有可视场景"))?;
    let mut children = Vec::new();
    for node in scene.children_named("node") {
        children.push(builder.node(node, 0)?);
    }
    builder.nodes[0].children = children;

    // 节点号 → sid（蒙皮关节按 sid 找）。
    let mut sid_map = HashMap::new();
    for (sid, &node) in &builder.by_sid {
        sid_map.insert(node, sid.clone());
    }
    builder.attach_skins(&sid_map)?;

    let animations = read_animations(root, &builder);
    let joints = read_kinematics(root, &builder);

    let mut model = Model::new(builder.meshes, builder.materials, builder.nodes, vec![0]).with_skins(builder.skins);
    if !animations.is_empty() {
        model = model.with_animations(animations);
    }
    if model.meshes().is_empty() && model.nodes().len() <= 1 {
        return Err(bad("Collada 场景是空的"));
    }
    Ok(Collada { model, joints })
}

/// 一条动画通道。
struct AnimationChannel {
    node: usize,
    /// 变换元素的 sid。
    sid: String,
    /// 分量：`None` 整个元素，`Some(k)` 第 k 个值。
    member: Option<usize>,
    times: Vec<f32>,
    values: Vec<f32>,
    stride: usize,
    step: bool,
}

fn member_index(kind: &str, member: &str) -> Option<usize> {
    let member = member.to_ascii_uppercase();
    // `(i)(j)` 形式：矩阵的第 i 行第 j 列（行主序），或数组下标。
    if member.starts_with('(') {
        let numbers: Vec<usize> = member
            .split(['(', ')'])
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        return match numbers.as_slice() {
            [i] => Some(*i),
            [row, column] => Some(row * 4 + column),
            _ => None,
        };
    }
    Some(match (kind, member.as_str()) {
        ("rotate", "ANGLE") => 3,
        (_, "X") => 0,
        (_, "Y") => 1,
        (_, "Z") => 2,
        (_, "W") | (_, "ANGLE") => 3,
        _ => return None,
    })
}

fn read_animations(root: &Element, builder: &Builder) -> Vec<AnimationClip> {
    let Some(library) = root.child("library_animations") else { return Vec::new() };
    let mut channels = Vec::new();
    let mut animations = Vec::new();
    library.descendants("animation", &mut animations);
    for animation in animations {
        let sources: HashMap<&str, Source> = animation
            .children_named("source")
            .filter_map(|s| Some((s.attr("id")?, read_source(s))))
            .collect();
        let samplers: HashMap<&str, &Element> = animation.children_named("sampler").filter_map(|s| Some((s.attr("id")?, s))).collect();
        for channel in animation.children_named("channel") {
            let Some(sampler) = samplers.get(url(channel.attr("source"))) else { continue };
            let input = |semantic: &str| {
                sampler
                    .children_named("input")
                    .find(|i| i.attr("semantic") == Some(semantic))
                    .and_then(|i| sources.get(url(i.attr("source"))))
            };
            let (Some(times), Some(values)) = (input("INPUT"), input("OUTPUT")) else { continue };
            let step = input("INTERPOLATION").is_some_and(|s| s.names.first().is_some_and(|n| n == "STEP"));
            let target = channel.attr("target").unwrap_or("");
            let Some((node_id, rest)) = target.split_once('/') else { continue };
            let Some(&node) = builder.by_id.get(node_id) else { continue };
            let (sid, member) = match rest.find(['.', '(']) {
                Some(at) => (&rest[..at], Some(rest[at..].trim_start_matches('.'))),
                None => (rest, None),
            };
            let kind = builder.stacks[node]
                .iter()
                .find(|(_, s, _)| s.as_deref() == Some(sid))
                .map_or("", |(k, _, _)| k.as_str());
            channels.push(AnimationChannel {
                node,
                sid: sid.to_string(),
                member: member.and_then(|m| member_index(kind, m)),
                times: times.floats.clone(),
                stride: values.stride,
                values: values.floats.clone(),
                step,
            });
        }
    }
    if channels.is_empty() {
        return Vec::new();
    }

    let mut by_node: HashMap<usize, Vec<&AnimationChannel>> = HashMap::new();
    for channel in &channels {
        by_node.entry(channel.node).or_default().push(channel);
    }
    let mut tracks = Vec::new();
    let mut nodes: Vec<usize> = by_node.keys().copied().collect();
    nodes.sort_unstable();
    for node in nodes {
        let list = &by_node[&node];
        let mut times: BTreeSet<u32> = BTreeSet::new();
        for c in list {
            for t in &c.times {
                times.insert(t.max(0.0).to_bits());
            }
        }
        let times: Vec<f32> = times.into_iter().map(f32::from_bits).collect();
        let step = list.iter().all(|c| c.step);
        let mut positions = Vec::with_capacity(times.len());
        let mut rotations = Vec::with_capacity(times.len());
        let mut scales = Vec::with_capacity(times.len());
        for &time in &times {
            let mut local = Mat4::IDENTITY;
            for (kind, sid, values) in &builder.stacks[node] {
                let mut values = values.clone();
                for channel in list.iter().filter(|c| Some(c.sid.as_str()) == sid.as_deref()) {
                    let sampled = sample(channel, time);
                    match channel.member {
                        Some(k) => {
                            if let (Some(slot), Some(v)) = (values.get_mut(k), sampled.first()) {
                                *slot = *v;
                            }
                        }
                        None => {
                            for (slot, v) in values.iter_mut().zip(&sampled) {
                                *slot = *v;
                            }
                        }
                    }
                }
                if let Some(matrix) = transform_matrix(kind, &values) {
                    local *= matrix;
                }
            }
            let t = decompose(local);
            positions.push(t.position);
            rotations.push(t.rotation);
            scales.push(t.scale);
        }
        // 四元数逐帧取与前一帧同半球，免得线性插值绕远路。
        for k in 1..rotations.len() {
            if rotations[k].dot(rotations[k - 1]) < 0.0 {
                rotations[k] = -rotations[k];
            }
        }
        let interpolation = if step { Interpolation::Step } else { Interpolation::Linear };
        if let Some(c) = Curve::new(times.clone(), positions, interpolation) {
            tracks.push(Track { target: node, channel: Channel::Position(c) });
        }
        if let Some(c) = Curve::new(times.clone(), rotations, interpolation) {
            tracks.push(Track { target: node, channel: Channel::Rotation(c) });
        }
        if let Some(c) = Curve::new(times, scales, interpolation) {
            tracks.push(Track { target: node, channel: Channel::Scale(c) });
        }
    }
    vec![AnimationClip::new("default", tracks)]
}

/// 线性采样一条通道，返回这一时刻的值（`stride` 个）。
fn sample(channel: &AnimationChannel, time: f32) -> Vec<f32> {
    let frames = channel.times.len().min(channel.values.len() / channel.stride.max(1));
    let value = |k: usize| channel.values[k * channel.stride..(k + 1) * channel.stride].to_vec();
    if frames == 0 {
        return Vec::new();
    }
    let next = channel.times[..frames].partition_point(|&t| t <= time);
    if next == 0 {
        return value(0);
    }
    if next >= frames {
        return value(frames - 1);
    }
    let (t0, t1) = (channel.times[next - 1], channel.times[next]);
    let a = value(next - 1);
    if channel.step || t1 <= t0 {
        return a;
    }
    let b = value(next);
    let f = (time - t0) / (t1 - t0);
    a.iter().zip(&b).map(|(x, y)| x + (y - x) * f).collect()
}

fn read_kinematics(root: &Element, builder: &Builder) -> Vec<KinematicJoint> {
    let mut models = Vec::new();
    root.descendants("kinematics_model", &mut models);
    let mut joints: Vec<(String, String, bool, Vec3, f32, f32)> = Vec::new();
    for model in &models {
        let Some(technique) = model.child("technique_common") else { continue };
        for joint in technique.children_named("joint") {
            let joint_sid = joint.attr("sid").unwrap_or("");
            for motion in joint.children.iter().filter(|c| c.name == "revolute" || c.name == "prismatic") {
                let axis_sid = motion.attr("sid").unwrap_or("axis0");
                let axis = motion.child("axis").map(Element::floats).unwrap_or_default();
                let limit = |name: &str| motion.find(&format!("limits/{name}")).and_then(|v| v.text.trim().parse::<f32>().ok());
                joints.push((
                    joint.attr("name").unwrap_or(joint_sid).to_string(),
                    format!("{joint_sid}_{axis_sid}"),
                    motion.name == "revolute",
                    Vec3::new(*axis.first().unwrap_or(&0.0), *axis.get(1).unwrap_or(&0.0), *axis.get(2).unwrap_or(&0.0)),
                    limit("min").unwrap_or(0.0),
                    limit("max").unwrap_or(0.0),
                ));
            }
        }
    }
    if joints.is_empty() {
        return Vec::new();
    }
    let mut binds = Vec::new();
    root.descendants("bind_joint_axis", &mut binds);
    let mut out = Vec::new();
    for bind in binds {
        let target = bind.attr("target").unwrap_or("");
        let element_sid = target.rsplit('/').next().unwrap_or("");
        let axis_param = bind.find("axis/param").map_or("", |p| p.text.trim());
        let value = bind.find("value/float").and_then(|v| v.text.trim().parse::<f32>().ok()).unwrap_or(0.0);
        // 最长后缀匹配：`joint_6-tool0_axis0` 不能被 `joint_6_axis0`……抢走。
        let Some(joint) = joints
            .iter()
            .filter(|j| axis_param.ends_with(&j.1))
            .max_by_key(|j| j.1.len())
        else {
            klog::warn!("Collada 运动学：{axis_param} 对不上任何关节");
            continue;
        };
        let Some((node, position)) = builder.stacks.iter().enumerate().find_map(|(n, stack)| {
            stack.iter().position(|(_, sid, _)| sid.as_deref() == Some(element_sid)).map(|p| (n, p))
        }) else {
            klog::warn!("Collada 运动学：找不到 sid 为 {element_sid} 的变换元素");
            continue;
        };
        let stack = &builder.stacks[node];
        let product = |range: &[(String, Option<String>, Vec<f32>)]| {
            range.iter().fold(Mat4::IDENTITY, |m, (k, _, v)| m * transform_matrix(k, v).unwrap_or(Mat4::IDENTITY))
        };
        out.push(KinematicJoint {
            name: joint.0.clone(),
            revolute: joint.2,
            axis: joint.3,
            min: joint.4,
            max: joint.5,
            node,
            before: product(&stack[..position]),
            after: product(&stack[position + 1..]),
            zero: value,
        });
    }
    out
}

/// 解析 Collada 字节，贴图从 `resolve` 里取（KMZ 从压缩包里取）。
pub(crate) fn build_with(
    bytes: &[u8],
    name: &str,
    mut resolve: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Collada, LoadError> {
    let root = crate::xml::parse(bytes)?;
    let mut images = HashMap::new();
    for file in image_paths(&root) {
        if let Some(data) = resolve(&file)
            && let Some(texture) = crate::texture_from_bytes(&format!("{name}#{file}"), &data, false)
        {
            images.insert(file, texture);
        }
    }
    build(&root, &images, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIANGLE: &str = r##"<COLLADA><asset><up_axis>Z_UP</up_axis></asset>
    <library_effects><effect id="fx"><profile_COMMON><technique sid="t"><phong>
      <diffuse><color>1 0 0 1</color></diffuse><shininess><float>50</float></shininess>
    </phong></technique></profile_COMMON></effect></library_effects>
    <library_materials><material id="red" name="Red"><instance_effect url="#fx"/></material></library_materials>
    <library_geometries><geometry id="g"><mesh>
      <source id="p"><float_array count="12">0 0 0 1 0 0 1 1 0 0 1 0</float_array>
        <technique_common><accessor stride="3"/></technique_common></source>
      <vertices id="v"><input semantic="POSITION" source="#p"/></vertices>
      <polylist material="m" count="1"><input semantic="VERTEX" source="#v" offset="0"/>
        <vcount>4</vcount><p>0 1 2 3</p></polylist>
    </mesh></geometry></library_geometries>
    <library_visual_scenes><visual_scene id="s"><node id="n" name="Quad">
      <translate sid="t">0 0 5</translate><rotate sid="r">0 0 1 0</rotate>
      <instance_geometry url="#g"><bind_material><technique_common>
        <instance_material symbol="m" target="#red"/></technique_common></bind_material></instance_geometry>
    </node></visual_scene></library_visual_scenes>
    <library_animations><animation><source id="t_in"><float_array>0 1</float_array><technique_common><accessor stride="1"/></technique_common></source>
      <source id="t_out"><float_array>0 90</float_array><technique_common><accessor stride="1"/></technique_common></source>
      <sampler id="smp"><input semantic="INPUT" source="#t_in"/><input semantic="OUTPUT" source="#t_out"/></sampler>
      <channel source="#smp" target="n/r.ANGLE"/></animation></library_animations>
    <scene><instance_visual_scene url="#s"/></scene></COLLADA>"##;

    #[test]
    fn quad_material_and_rotate_animation() {
        let root = crate::xml::parse(TRIANGLE.as_bytes()).unwrap();
        let collada = build(&root, &HashMap::new(), "t").unwrap();
        let model = collada.model;
        // 四边形扇形三角化成两个三角形。
        assert_eq!(model.triangle_count(), 2);
        let part = model.nodes()[1].parts[0];
        assert_eq!(model.materials()[part.material.unwrap()].base_color(), Vec4::new(1.0, 0.0, 0.0, 1.0));
        // Z 朝上：根节点绕 X 转了 -90°。
        assert!(model.nodes()[0].transform.rotation.angle_between(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)) < 1e-3);
        // 动画只动 rotateZ 的角度，平移保持 (0,0,5)。
        let pose = model.animations()[0].sample(1.0);
        let entry = pose.entry(1).unwrap();
        assert!((entry.position.unwrap() - Vec3::new(0.0, 0.0, 5.0)).length() < 1e-5);
        assert!(entry.rotation.unwrap().angle_between(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)) < 1e-4);
    }

    #[test]
    fn row_major_matrices() {
        let m = matrix_from_row_major(&[1.0, 0.0, 0.0, 7.0, 0.0, 1.0, 0.0, 8.0, 0.0, 0.0, 1.0, 9.0, 0.0, 0.0, 0.0, 1.0]);
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(7.0, 8.0, 9.0));
    }
}
