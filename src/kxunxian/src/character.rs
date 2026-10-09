//! 把一个寻仙角色拼成引擎的 [`Model`]：骨架 + 身上穿的部件 + 挂在挂点上的装备。
//!
//! 节点排布：前 `骨骼数` 个节点就是骨骼（序号 = `.psf` 里的骨骼号，动作轨道直接用它当目标号），
//! 后面是身体部件（蒙皮网格，挂 0 号骨架）和装备（刚体网格，作为挂点骨骼的子节点）。
//!
//! **换手系**：寻仙是 D3D 的左手系（Y 朝上），引擎是右手系。所有位置、四元数、矩阵统一沿 Z 镜像
//! （`S = diag(1, 1, -1)`，`M' = S·M·S`），三角形绕序随之反过来。镜像是自己的逆，所以骨骼、蒙皮、
//! 挂点、动作只要各自镜像一遍，组合起来就仍然一致。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanim::{AnimationClip, Channel, Curve, Interpolation, Track};
use kasset::Resource;
use kgltf::{MeshPart, Model, ModelNode, ModelSkin, NodeTransform};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec2, Vec3, Vec4};
use kmesh::{Mesh, SkinVertex, Vertex};
use ktexture::Texture;

use crate::config::{CharacterDef, EquipDef, MaterialDef, parse_materials};
use crate::formats::{Bone, FormatError, PafAnimation, PmfMesh, parse_paf, parse_pmf, parse_psf};

/// 读资源时的错误。
#[derive(Debug, Clone)]
pub enum Error {
    /// 文件找不到（给的是原始路径）。
    Missing(String),
    Io(PathBuf, String),
    Format(PathBuf, FormatError),
    /// 配置里没有这个名字。
    Unknown(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing(path) => write!(f, "找不到 {path}"),
            Error::Io(path, message) => write!(f, "读 {} 失败：{message}", path.display()),
            Error::Format(path, error) => write!(f, "{}：{error}", path.display()),
            Error::Unknown(name) => write!(f, "配置里没有 {name}"),
        }
    }
}

impl std::error::Error for Error {}

/// 沿 Z 镜像（左手系 ↔ 右手系）。
fn mirror(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, -v.z)
}

fn mirror_rotation(q: Quat) -> Quat {
    Quat::from_xyzw(-q.x, -q.y, q.z, q.w)
}

/// 一套穿戴：角色 `.cct` 里的部件名（`<Model Name>`）和装备名（`<Equip Name>`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outfit {
    pub parts: Vec<String>,
    pub equips: Vec<String>,
}

/// 一个已经读好的角色：配置 + 骨架。
#[derive(Debug, Clone)]
pub struct Character {
    pub name: String,
    pub def: Arc<CharacterDef>,
    pub bones: Arc<Vec<Bone>>,
}

impl Character {
    /// 按名找骨骼号。
    pub fn bone(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|bone| bone.name == name)
    }
}

/// 寻仙资源库：资源根目录（解包出来的 `cha/`、`obj/` 等所在目录）+ 各种缓存。
///
/// 衣柜很大（一个 zj 角色几千个部件），所以一切都按需读、读过就缓存：配置、材质表、网格、贴图。
pub struct Library {
    root: PathBuf,
    characters: HashMap<String, Arc<CharacterDef>>,
    skeletons: HashMap<PathBuf, Arc<Vec<Bone>>>,
    material_tables: HashMap<PathBuf, Arc<HashMap<String, MaterialDef>>>,
    meshes: HashMap<PathBuf, Arc<PmfMesh>>,
    textures: HashMap<PathBuf, Option<Resource<Texture>>>,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            characters: HashMap::new(),
            skeletons: HashMap::new(),
            material_tables: HashMap::new(),
            meshes: HashMap::new(),
            textures: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 把配置里的路径（`$(res)\cha\...`）解析成磁盘上的文件。
    ///
    /// 源数据有大小写和拼写不一致（`textrue`、`.DDS`、`skelton`），按「原样 → 小写 → 修正拼写」依次试。
    pub fn resolve(&self, raw: &str) -> Option<PathBuf> {
        let relative = raw
            .trim()
            .trim_start_matches("$(res)")
            .trim_start_matches(['\\', '/'])
            .replace('\\', "/");
        if relative.is_empty() {
            return None;
        }
        let candidates = [
            relative.clone(),
            relative.to_lowercase(),
            relative.to_lowercase().replace("/textrue/", "/texture/"),
        ];
        candidates
            .into_iter()
            .map(|candidate| self.root.join(candidate))
            .find(|path| path.is_file())
    }

    fn read(&self, raw: &str) -> Result<(PathBuf, Vec<u8>), Error> {
        let path = self
            .resolve(raw)
            .ok_or_else(|| Error::Missing(raw.to_string()))?;
        let bytes =
            std::fs::read(&path).map_err(|error| Error::Io(path.clone(), error.to_string()))?;
        Ok((path, bytes))
    }

    /// 读一个 `.cct`（按原始路径）。
    pub fn config(&mut self, raw: &str) -> Result<Arc<CharacterDef>, Error> {
        let key = raw.to_lowercase();
        if let Some(def) = self.characters.get(&key) {
            return Ok(def.clone());
        }
        let (_, bytes) = self.read(raw)?;
        let def = Arc::new(CharacterDef::parse(&bytes));
        self.characters.insert(key, def.clone());
        Ok(def)
    }

    /// 读 `cha/special/<name>/` 下的角色。
    pub fn character(&mut self, name: &str) -> Result<Character, Error> {
        let def = self.config(&format!("cha/special/{name}/config/{name}.cct"))?;
        let raw = def
            .skeleton
            .clone()
            .ok_or_else(|| Error::Unknown(format!("{name} 的骨架")))?;
        let (path, bytes) = self.read(&raw)?;
        let bones = match self.skeletons.get(&path) {
            Some(bones) => bones.clone(),
            None => {
                let bones = Arc::new(
                    parse_psf(&bytes).map_err(|error| Error::Format(path.clone(), error))?,
                );
                self.skeletons.insert(path, bones.clone());
                bones
            }
        };
        Ok(Character {
            name: name.to_string(),
            def,
            bones,
        })
    }

    fn material_table(&mut self, raw: &str) -> Option<Arc<HashMap<String, MaterialDef>>> {
        let path = self.resolve(raw)?;
        if let Some(table) = self.material_tables.get(&path) {
            return Some(table.clone());
        }
        let bytes = std::fs::read(&path).ok()?;
        let table = Arc::new(parse_materials(&bytes));
        self.material_tables.insert(path, table.clone());
        Some(table)
    }

    fn mesh(&mut self, raw: &str) -> Result<Arc<PmfMesh>, Error> {
        let path = self
            .resolve(raw)
            .ok_or_else(|| Error::Missing(raw.to_string()))?;
        if let Some(mesh) = self.meshes.get(&path) {
            return Ok(mesh.clone());
        }
        let bytes =
            std::fs::read(&path).map_err(|error| Error::Io(path.clone(), error.to_string()))?;
        let mesh = Arc::new(parse_pmf(&bytes).map_err(|error| Error::Format(path.clone(), error))?);
        self.meshes.insert(path, mesh.clone());
        Ok(mesh)
    }

    fn texture(&mut self, raw: &str) -> Option<Resource<Texture>> {
        // 文件名前缀写错的（`m_yf637hw_001_h.dds` 实际只有 `c_` / `f_` 版本）换个前缀再找。
        let path = self.resolve(raw).or_else(|| {
            prefix_variants(raw)
                .into_iter()
                .find_map(|variant| self.resolve(&variant))
        })?;
        if let Some(texture) = self.textures.get(&path) {
            return texture.clone();
        }
        let texture =
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| match Texture::from_encoded(&bytes) {
                    Ok(texture) => Some(Resource::new_ok(
                        path.to_string_lossy().into_owned(),
                        texture.with_format(ktexture::TextureFormat::Srgb),
                    )),
                    Err(error) => {
                        klog::warn!("贴图 {} 解码失败：{error}", path.display());
                        None
                    }
                });
        self.textures.insert(path, texture.clone());
        texture
    }

    /// 按材质名在这几张材质表里找定义，做成引擎材质。
    fn material(&mut self, tables: &[String], name: &str, double_sided: bool) -> Material {
        let own: Vec<_> = tables
            .iter()
            .filter_map(|raw| self.material_table(raw))
            .collect();
        // 自己的表找不到，再去男女两张 zj 共享表里找：有些部件引用的材质只写在另一个性别的表里。
        let def = lookup_material(&own, name).or_else(|| {
            let shared: Vec<_> = SHARED_MATERIAL_TABLES
                .iter()
                .filter_map(|raw| self.material_table(raw))
                .collect();
            lookup_material(&shared, name)
        });
        let mut material = Material::default().with_roughness(0.85).with_metallic(0.0);
        if double_sided {
            material = material.with_double_sided();
        }
        let Some(def) = def else {
            klog::warn!("材质表里没有 {name}");
            return material;
        };
        if let Some(texture) = self.texture(&def.base_map) {
            material = material.with_base_color_texture(texture.clone());
            // D3D 固定管线的自发光是加在光照上再乘贴图的：`贴图 × (光照 + 自发光)`。
            // 用「自发光贴图 = 底图、强度 = 自发光色」就是同一个式子。
            let emissive = def.emissive.truncate();
            if emissive.max_element() > 0.0 {
                material.set(kpbr::standard::EMISSIVE_TEXTURE, texture);
                material.set(kpbr::standard::EMISSIVE, emissive);
            }
        }
        material =
            material.with_base_color(Vec4::new(def.diffuse.x, def.diffuse.y, def.diffuse.z, 1.0));
        // 头发、衣摆、法宝的镂空靠贴图 alpha：按 0.5 裁掉。整张不透明的贴图 alpha 恒为 1，裁不掉什么。
        kpbr::physical::Physical {
            alpha_cutoff: 0.5,
            ..Default::default()
        }
        .apply(&mut material);
        material
    }

    /// 读一段动作并合并上下半身。
    ///
    /// 一个完整动作分成两个文件：`<code>_tk.paf` 驱动上半身，`<code>_ca.paf` 驱动下半身（根、骨盆、腿），
    /// 两边的骨骼不重叠。只有一半的动作（比如表情）就只用那一半。
    pub fn animation(&mut self, character: &Character, code: &str) -> Result<AnimationClip, Error> {
        let mut merged: Option<PafAnimation> = None;
        for half in ["tk", "ca"] {
            let name = format!("{code}_{half}");
            let raw = match character.def.animations.get(&name) {
                Some(raw) => raw.clone(),
                None => format!("cha/special/{}/animation/{name}.paf", character.name),
            };
            let Ok((path, bytes)) = self.read(&raw) else {
                continue;
            };
            let animation = parse_paf(&bytes).map_err(|error| Error::Format(path, error))?;
            match merged.as_mut() {
                None => merged = Some(animation),
                Some(merged) => {
                    for (bone, track) in animation.tracks {
                        merged.tracks.entry(bone).or_insert(track);
                    }
                    merged.duration = merged.duration.max(animation.duration);
                }
            }
        }
        let animation = merged.ok_or_else(|| Error::Unknown(format!("动作 {code}")))?;
        Ok(clip(code, &animation, character.bones.len()))
    }

    /// 拼出角色模型。读不到的部件跳过并记日志（源数据里确实有几个坏文件），不让整个角色失败。
    pub fn build(&mut self, character: &Character, outfit: &Outfit) -> Model {
        let bones = &character.bones;
        let def = &character.def;
        let mut nodes: Vec<ModelNode> = bones
            .iter()
            .map(|bone| ModelNode {
                name: bone.name.clone(),
                transform: NodeTransform {
                    position: mirror(bone.bind_translation),
                    rotation: mirror_rotation(bone.bind_rotation),
                    scale: Vec3::ONE,
                },
                ..Default::default()
            })
            .collect();
        let mut roots = Vec::new();
        for (index, bone) in bones.iter().enumerate() {
            match usize::try_from(bone.parent)
                .ok()
                .filter(|&parent| parent < bones.len() && parent != index)
            {
                Some(parent) => nodes[parent].children.push(index),
                None => roots.push(index),
            }
        }
        let inverse_bind = bones
            .iter()
            .map(|bone| {
                Mat4::from_rotation_translation(
                    mirror_rotation(bone.inverse_rotation),
                    mirror(bone.inverse_translation),
                )
            })
            .collect();
        let skins = vec![ModelSkin {
            joints: (0..bones.len()).collect(),
            inverse_bind,
            skeleton: None,
        }];

        let mut meshes = Vec::new();
        let mut materials = Vec::new();

        // ---- 身上穿的部件：蒙皮网格 ----
        for name in &outfit.parts {
            let Some(part) = def.model(name).cloned() else {
                klog::warn!("{} 没有部件 {name}", character.name);
                continue;
            };
            let pmf = match self.mesh(&part.mesh) {
                Ok(pmf) => pmf,
                Err(error) => {
                    klog::warn!("部件 {name}：{error}");
                    continue;
                }
            };
            let Some(mesh) = convert_mesh(&pmf, bones.len()) else {
                continue;
            };
            let material = self.material(&def.materials, &part.material, part.double_sided);
            let index = nodes.len();
            nodes.push(ModelNode {
                name: name.clone(),
                parts: vec![MeshPart {
                    mesh: meshes.len(),
                    material: Some(materials.len()),
                }],
                skin: Some(0),
                ..Default::default()
            });
            meshes.push(mesh);
            materials.push(material);
            roots.push(index);
        }

        // ---- 装备：刚体网格，挂在挂点上 ----
        for equip_name in &outfit.equips {
            let Some(equip) = def.equips.get(equip_name).cloned() else {
                klog::warn!("{} 没有装备 {equip_name}", character.name);
                continue;
            };
            self.attach_equip(
                character,
                &equip,
                &mut nodes,
                &mut roots,
                &mut meshes,
                &mut materials,
            );
        }

        Model::new(meshes, materials, nodes, roots).with_skins(skins)
    }

    fn attach_equip(
        &mut self,
        character: &Character,
        equip: &EquipDef,
        nodes: &mut Vec<ModelNode>,
        roots: &mut Vec<usize>,
        meshes: &mut Vec<Mesh>,
        materials: &mut Vec<Material>,
    ) {
        let template = match self.config(&equip.template) {
            Ok(template) => template,
            Err(error) => {
                klog::warn!("装备 {}：{error}", equip.name);
                return;
            }
        };
        // 挂点优先在角色自己身上找（`equip_weapen` 这类），角色没有再找模板里的。
        let hinge = character
            .def
            .hinges
            .get(&equip.hinge)
            .or_else(|| template.hinges.get(&equip.hinge))
            .copied();
        let transform = match hinge {
            Some(hinge) => NodeTransform {
                position: mirror(hinge.translation),
                rotation: mirror_rotation(hinge.rotation),
                scale: Vec3::splat(equip.scaling),
            },
            None => NodeTransform {
                scale: Vec3::splat(equip.scaling),
                ..Default::default()
            },
        };
        let bone = hinge
            .and_then(|hinge| usize::try_from(hinge.bone).ok())
            .filter(|&bone| bone < character.bones.len());
        let mount = nodes.len();
        nodes.push(ModelNode {
            name: equip.name.clone(),
            transform,
            ..Default::default()
        });
        match bone {
            Some(bone) => nodes[bone].children.push(mount),
            None => roots.push(mount),
        }
        for item in &equip.items {
            // `p_` 是粒子特效（`.gfx`），这里还不支持，跳过。
            let Some(model) = template.model(item).cloned() else {
                continue;
            };
            let pmf = match self.mesh(&model.mesh) {
                Ok(pmf) => pmf,
                Err(error) => {
                    klog::warn!("装备 {item}：{error}");
                    continue;
                }
            };
            let Some(mesh) = convert_mesh(&pmf, 0) else {
                continue;
            };
            let material = self.material(&template.materials, &model.material, model.double_sided);
            let index = nodes.len();
            nodes.push(ModelNode {
                name: item.clone(),
                parts: vec![MeshPart {
                    mesh: meshes.len(),
                    material: Some(materials.len()),
                }],
                ..Default::default()
            });
            meshes.push(mesh);
            materials.push(material);
            nodes[mount].children.push(index);
        }
    }
}

/// zj 衣柜共用的两张材质表（男、女）。
const SHARED_MATERIAL_TABLES: [&str; 2] = [
    "cha/share/config/zj_nanxing_001.cmf",
    "cha/share/config/zj_nvxing_001.cmf",
];

/// 材质 / 贴图名的前缀（`c_` `f_` `m_` `t_`，大概是不同品质或来源）换成别的几种。
/// 路径的话只换最后一段文件名。
fn prefix_variants(name: &str) -> Vec<String> {
    let split = name.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let (dir, file) = name.split_at(split);
    let Some(rest) = file
        .get(..2)
        .filter(|head| head.ends_with('_') && "cfmtCFMT".contains(&head[..1]))
        .map(|_| &file[2..])
    else {
        return Vec::new();
    };
    ["c_", "f_", "m_", "t_"]
        .iter()
        .filter(|prefix| !file.to_lowercase().starts_with(*prefix))
        .map(|prefix| format!("{dir}{prefix}{rest}"))
        .collect()
}

/// 按名在几张材质表里找材质，容忍源数据的笔误，依次试：
/// 1. 精确匹配；
/// 2. 不分大小写；
/// 3. 去掉 `_h` 后面多出来的数字（`c_mz609mza_609_h0` 其实是 `c_mz609mza_609_h`）；
/// 4. 以上各形式换前缀（`c_mz609mz_609_h` 表里只有 `f_` / `m_` 版本）。
fn lookup_material(
    tables: &[Arc<HashMap<String, MaterialDef>>],
    name: &str,
) -> Option<MaterialDef> {
    if let Some(def) = tables.iter().find_map(|table| table.get(name)) {
        return Some(def.clone());
    }
    let trimmed = name
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .to_string();
    let mut candidates = vec![name.to_string(), trimmed.clone()];
    candidates.extend(prefix_variants(name));
    candidates.extend(prefix_variants(&trimmed));
    candidates
        .iter()
        .map(|c| c.to_lowercase())
        .find_map(|wanted| {
            tables.iter().find_map(|table| {
                table
                    .iter()
                    .find(|(key, _)| key.to_lowercase() == wanted)
                    .map(|(_, def)| def.clone())
            })
        })
}

/// `.pmf` → 引擎网格（换手系、反绕序、补法线）。`joint_count` 为 0 表示当刚体用，忽略蒙皮数据。
fn convert_mesh(pmf: &PmfMesh, joint_count: usize) -> Option<Mesh> {
    let positions: Vec<Vec3> = pmf.positions.iter().map(|&p| mirror(p)).collect();
    let mut indices = pmf.indices.clone();
    for triangle in indices.chunks_exact_mut(3) {
        triangle.swap(1, 2);
    }
    if indices.is_empty() {
        return None;
    }
    // 文件法线长度不对（个别格式变体的布局没对上）就整份按几何重算。
    let file_normals_ok = pmf.normals.len() == positions.len()
        && pmf.normals.iter().all(|n| (n.length() - 1.0).abs() < 0.1);
    let normals: Vec<Vec3> = if file_normals_ok {
        pmf.normals.iter().map(|&n| mirror(n)).collect()
    } else {
        let mut sums = vec![Vec3::ZERO; positions.len()];
        for t in indices.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            let face = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
            for i in [a, b, c] {
                sums[i] += face;
            }
        }
        sums.into_iter().map(|n| n.normalize_or(Vec3::Y)).collect()
    };
    let vertices: Vec<Vertex> = (0..positions.len())
        .map(|i| {
            let uv = pmf.uvs.get(i).copied().unwrap_or(Vec2::ZERO);
            Vertex::new(positions[i], normals[i], [uv.x, uv.y])
        })
        .collect();
    let mut mesh = Mesh::new(vertices, indices);
    if joint_count > 0 && pmf.is_skinned() {
        let skin = pmf
            .weights
            .iter()
            .zip(&pmf.joints)
            .map(|(&weights, &joints)| {
                // 越界的骨骼号（坏数据）钳到 0 号，宁可这一点不动也不能取错矩阵。
                let joints = joints.map(|j| if (j as usize) < joint_count { j } else { 0 });
                if weights.iter().sum::<f32>() > 1e-6 {
                    SkinVertex { joints, weights }
                } else {
                    // 权重全 0：刚性绑在第一个骨骼上。
                    SkinVertex {
                        joints,
                        weights: [1.0, 0.0, 0.0, 0.0],
                    }
                }
            })
            .collect();
        mesh = mesh.with_skin(skin);
    }
    Some(mesh)
}

/// `.paf` → 剪辑。轨道目标号就是骨骼号（[`Library::build`] 让骨骼占模型的前几个节点）。
fn clip(name: &str, animation: &PafAnimation, bone_count: usize) -> AnimationClip {
    let step = 1.0 / animation.sample_rate as f32;
    let times = |count: usize| (0..count).map(|i| i as f32 * step).collect::<Vec<_>>();
    let mut tracks = Vec::new();
    let mut bones: Vec<_> = animation.tracks.iter().collect();
    bones.sort_by_key(|(bone, _)| **bone);
    for (&bone, track) in bones {
        let Some(target) = usize::try_from(bone).ok().filter(|&b| b < bone_count) else {
            continue;
        };
        if !track.rotations.is_empty() {
            let values = track
                .rotations
                .iter()
                .map(|&q| mirror_rotation(q))
                .collect();
            if let Some(curve) =
                Curve::new(times(track.rotations.len()), values, Interpolation::Linear)
            {
                tracks.push(Track {
                    target,
                    channel: Channel::Rotation(curve),
                });
            }
        }
        if !track.translations.is_empty() {
            let values = track.translations.iter().map(|&t| mirror(t)).collect();
            if let Some(curve) = Curve::new(
                times(track.translations.len()),
                values,
                Interpolation::Linear,
            ) {
                tracks.push(Track {
                    target,
                    channel: Channel::Position(curve),
                });
            }
        }
    }
    AnimationClip::new(name, tracks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_lookup_tolerates_source_typos() {
        let def = |map: &str| MaterialDef {
            base_map: map.to_string(),
            diffuse: Vec4::ONE,
            emissive: Vec4::ZERO,
        };
        let table: HashMap<String, MaterialDef> = [
            ("c_mz609mza_609_h".to_string(), def("hat.dds")),
            ("C_Upper_h".to_string(), def("upper.dds")),
        ]
        .into_iter()
        .collect();
        let tables = [Arc::new(table)];
        assert_eq!(
            lookup_material(&tables, "c_mz609mza_609_h0")
                .unwrap()
                .base_map,
            "hat.dds"
        );
        assert_eq!(
            lookup_material(&tables, "c_upper_H").unwrap().base_map,
            "upper.dds"
        );
        assert!(lookup_material(&tables, "nothing").is_none());
        // 前缀写错：表里是 `c_`，引用写成 `f_`。
        assert_eq!(
            lookup_material(&tables, "f_mz609mza_609_h")
                .unwrap()
                .base_map,
            "hat.dds"
        );
    }

    #[test]
    fn prefix_variants_swap_only_the_file_name() {
        let variants = prefix_variants(r"$(res)\cha\t_dir\m_yf637hw_001_h.dds");
        assert!(variants.contains(&r"$(res)\cha\t_dir\c_yf637hw_001_h.dds".to_string()));
        assert!(!variants.iter().any(|v| v.contains("m_yf637")));
        assert!(prefix_variants("zj_zjst_007_h").is_empty());
    }

    #[test]
    fn mirroring_keeps_composition_consistent() {
        // 左手系里「先旋转再平移」组合出来的点，镜像后在右手系里组合出来应该就是镜像点。
        let q = Quat::from_euler(kmath::EulerRot::XYZ, 0.3, -0.7, 1.1);
        let t = Vec3::new(0.2, -0.4, 0.9);
        let p = Vec3::new(1.0, 2.0, 3.0);
        let left = Mat4::from_rotation_translation(q, t).transform_point3(p);
        let right = Mat4::from_rotation_translation(mirror_rotation(q), mirror(t))
            .transform_point3(mirror(p));
        assert!((mirror(left) - right).length() < 1e-5);
    }

    #[test]
    fn resolves_paths_with_source_typos() {
        let root = std::env::temp_dir().join("kxunxian_resolve_test");
        let dir = root.join("cha/share/texture");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.dds"), b"x").unwrap();
        let library = Library::new(&root);
        assert!(
            library
                .resolve("$(res)\\cha\\share\\texture\\a.dds")
                .is_some()
        );
        assert!(
            library
                .resolve("$(res)\\cha\\share\\textrue\\A.DDS")
                .is_some()
        );
        assert!(library.resolve("$(res)\\cha\\nope.dds").is_none());
    }
}
