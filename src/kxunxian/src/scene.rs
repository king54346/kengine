//! 寻仙的地图场景（`scn/<场景目录>/`）：地形、水面、天空、物件摆放。
//!
//! 场景按 256 米见方的「单元」切块，`unitlist.xml` 列出有哪些单元，每个单元一个 `(x,z)` 子目录：
//!
//! | 文件 | 内容 |
//! |---|---|
//! | `height.short` | 256×256 个 `i16` 高度，**按 x 优先**排（下标 = 本地x × 256 + 本地z），× 0.02 是米 |
//! | `water.byte` | 16×16 格（每格 16 米），值是 `valley.xml` 里水面的编号，255 是没水 |
//! | `ornaments2.xml` | 小物件、花草（`.ort`）。每条 15 个数：缩放、xyz、四元数、颜色 rgba、名字号、参数号、别名号 |
//! | `sites2.xml` | 树、单件建筑（`.sit`）。每条 7 个数：xyz、转角（90° 的倍数）、名字号、参数号、别名号 |
//! | `citys2.xml` | 建筑群（`.cty`，比如整个小桑村）。格式同 `sites2.xml` |
//! | `bits_alpha.byte` | 地表分层权重：4 字节层掩码 + 每个用到的层一张 256×256，见 [`Blend`] |
//! | `color.dword` | 逐顶点明暗（D3D 颜色，B G R A），乘在地表上 |
//!
//! 还有场景根目录的 `preblend/(x,z).dds`（每个单元预先混好的地表贴图，**按 x 镜像存的**：u = 1 − 本地x/256）、
//! `sky.xml`（环境光、太阳、雾）、`valley.xml`（各片水面的高度；`haishui` 是大海，不画进 `water.byte`）、
//! `mark.xml`（NPC、出生点等标记）。
//!
//! 有的单元只有物件、没有 `height.short`（地图边上全是海的那几块），那里不铺地形。
//!
//! 物件三层嵌套：`.cty` 里是一组「摆件」（`Fitment` / `Villa`：`.sit` + 相对位置旋转，`Villa` 里还能再套），
//! `.sit` 里是几块「砖」（`Brick`：`.cct` + 相对位置旋转），`.cct` 是最终的网格和贴图（`<Plant>` / `<Building>`）。
//!
//! 碰撞：砖和 `.ort` 的 `<Phy>` 是一份简化的碰撞网格（`*_h.pmf`，只有位置），`.sit` 根上的 `<Projectee>` 是
//! 院子、台阶这类「站上去」的地面。都收进 [`SceneData::colliders`]，和地形一起交给物理。
//!
//! 坐标同角色一样是 D3D 左手系，Y 朝上。这里给出的位置、变换都已经沿 Z 镜像成引擎的右手系。

use std::collections::HashMap;

use kasset::Resource;
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use ktexture::Texture;

use crate::character::{Error, Library, convert_mesh, mirror};
use crate::xml::{self, Element};

/// 一个单元的边长（米），也是高度网格每边的采样数。
pub const UNIT_SIZE: usize = 256;
/// 高度网格的原始值 × 这个数 = 米。
const HEIGHT_SCALE: f32 = 0.02;
/// 水面格子的边长（米）：每个单元 16×16 格。
const WATER_CELL: usize = 16;

/// 一个地形单元。
#[derive(Debug, Clone)]
pub struct Unit {
    pub x: i32,
    pub z: i32,
    /// 256×256 个高度（米），按 x 优先：`heights[lx * 256 + lz]`。
    pub heights: Vec<f32>,
    /// 有没有高度图。没有的单元（海上）不铺地形，`heights` 全是 0。
    pub terrain: bool,
    /// 16×16 格的水面编号（`valley.xml` 的下标），255 是没水。同高度图一样按 x 优先：`water[ix * 16 + iz]`
    /// （拿河道核对过：这样排 93% 以上的水格子底下的地面比水面低）。
    pub water: Vec<u8>,
    /// 预混好的地表贴图。
    pub preblend: Option<Resource<Texture>>,
    /// 地表分层的混合权重（`bits_alpha.byte`），没有的单元为 `None`。见 [`Blend`]。
    pub blend: Option<Blend>,
    /// 逐顶点的明暗（`color.dword`）：256×256 个 D3D 颜色（内存里是 B、G、R、A），按 x 优先。
    /// 游戏把它乘在地表的光照上，里面烘了坡面的明暗和房子、树的影子；白色是不压暗。没有为 `None`。
    pub colors: Option<Vec<u8>>,
}

/// 一个单元的地表分层权重。
///
/// `bits_alpha.byte`：4 字节的层掩码（第 i 位 = 用到了 `layer.xml` 的第 i 层）+ 每个用到的层一张
/// 256×256 的权重（0~255），按层号从小到大排，和高度图一样按 x 优先（`[lx * 256 + lz]`）。
/// 最小的那层是底色（权重恒为 255），往上每层按自己的权重盖在前面混好的结果上。
/// 拿各层贴图的平均色这样混一遍，和预混贴图的相关系数在 0.73~0.82——预混贴图就是这么烘出来的。
#[derive(Debug, Clone)]
pub struct Blend {
    /// 用到的层号，从小到大。
    pub layers: Vec<usize>,
    /// 每层一张 256×256 的权重，顺序同 `layers`。
    pub weights: Vec<Vec<u8>>,
}

impl Blend {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let mask = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
        let layers: Vec<usize> = (0..32).filter(|i| mask >> i & 1 == 1).collect();
        let plane = UNIT_SIZE * UNIT_SIZE;
        if layers.is_empty() || bytes.len() < 4 + layers.len() * plane {
            return None;
        }
        let weights = (0..layers.len())
            .map(|k| bytes[4 + k * plane..4 + (k + 1) * plane].to_vec())
            .collect();
        Some(Self { layers, weights })
    }
}

/// `layer.xml` 里的一层地表：贴图和平铺尺寸。
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLayer {
    pub alias: String,
    /// 贴图每隔多少米重复一次（`<Scale>` 的 x）。
    pub scale: f32,
    /// 贴图的原始路径。
    pub texture: String,
}

/// 天空与光照（`sky.xml` 里第一个时段）。
#[derive(Debug, Clone, PartialEq)]
pub struct Sky {
    pub ambient: Vec3,
    pub diffuse: Vec3,
    /// 太阳方向（指向太阳，引擎右手系）。
    pub sun_direction: Vec3,
    pub fog_color: Vec3,
    pub fog_start: f32,
    pub fog_end: f32,
    /// 天顶到地平线的几种颜色（`SkyColor_0..`）。
    pub sky_colors: Vec<Vec3>,
}

impl Default for Sky {
    fn default() -> Self {
        Self {
            ambient: Vec3::splat(0.6),
            diffuse: Vec3::ONE,
            sun_direction: Vec3::new(0.4, 0.8, 0.3).normalize(),
            fog_color: Vec3::new(0.5, 0.7, 0.9),
            fog_start: 50.0,
            fog_end: 300.0,
            sky_colors: Vec::new(),
        }
    }
}

/// 场景里的一个标记点（NPC、出生点……）。
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub alias: String,
    pub kind: String,
    /// 引擎右手系的位置。
    pub position: Vec3,
    /// 朝向（度，原始值）。
    pub yaw: f32,
}

/// 一次物件摆放：哪个 `.cct`、放在哪（引擎右手系的世界变换）、什么颜色。
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// `.cct` 的原始路径。
    pub model: String,
    pub transform: Mat4,
    pub color: Vec4,
}

/// 一块碰撞网格的摆放：哪个 `.pmf`、放在哪（引擎右手系的世界变换）。
#[derive(Debug, Clone, PartialEq)]
pub struct Collider {
    pub mesh: String,
    pub transform: Mat4,
}

/// 读好的场景。
#[derive(Debug, Clone)]
pub struct SceneData {
    pub name: String,
    pub units: Vec<Unit>,
    pub sky: Sky,
    /// 各片水面的高度（米），下标就是 `water.byte` 里的编号。
    pub water_levels: Vec<f32>,
    /// 大海（`valley.xml` 里叫 `haishui` 的那片水）的编号。大海不按格子画，铺一整片，见 [`SceneData::sea_mesh`]。
    pub sea: Option<usize>,
    /// 地表分层（`layer.xml`），下标就是层号；别名是 `no` 的占位层为 `None`。
    pub layers: Vec<Option<TerrainLayer>>,
    pub marks: Vec<Mark>,
    pub placements: Vec<Placement>,
    pub colliders: Vec<Collider>,
    unit_index: HashMap<(i32, i32), usize>,
}

/// 物件 `.cct` 里的一块网格和它的材质。
#[derive(Debug, Clone)]
pub struct StaticPart {
    pub mesh: Mesh,
    pub material: Material,
}

fn parse_floats(text: &str) -> Vec<f32> {
    text.trim_matches(|c| c == '{' || c == '}')
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect()
}

fn vec3_of(text: &str) -> Option<Vec3> {
    let v = parse_floats(text);
    (v.len() >= 3).then(|| Vec3::new(v[0], v[1], v[2]))
}

fn rgb_of(text: &str) -> Option<Vec3> {
    vec3_of(text)
}

fn quat_of(text: &str) -> Option<Quat> {
    let v = parse_floats(text);
    (v.len() >= 4).then(|| {
        let q = Quat::from_xyzw(v[0], v[1], v[2], v[3]);
        if q.length_squared() > 1e-6 {
            q.normalize()
        } else {
            Quat::IDENTITY
        }
    })
}

/// 按标签取第一个元素的文字。
fn first_text<'a>(elements: &'a [Element], tag: &str) -> Option<&'a str> {
    elements
        .iter()
        .find(|e| e.tag == tag && !e.closing)
        .map(|e| e.text.as_str())
}

/// 左手系（场景原始数据）里的一个变换：先转后移，再整体缩放。
fn local(position: Vec3, rotation: Quat, scale: Vec3) -> Mat4 {
    Mat4::from_scale_rotation_translation(scale, rotation, position)
}

/// 左手系矩阵 → 引擎右手系：`S·M·S`，`S = diag(1, 1, -1)`。
fn to_engine(m: Mat4) -> Mat4 {
    let s = Mat4::from_scale(Vec3::new(1.0, 1.0, -1.0));
    s * m * s
}

/// 「90° 的倍数」转角（`sites2.xml` / `citys2.xml` 里的整数）。
fn quarter_turns(value: f32) -> Quat {
    Quat::from_rotation_y(value * std::f32::consts::FRAC_PI_2)
}

impl Library {
    fn text(&self, raw: &str) -> Option<String> {
        self.read(raw).ok().map(|(_, bytes)| xml::decode(&bytes))
    }

    /// 读 `scn/<folder>/` 下的场景。
    pub fn scene(&mut self, folder: &str) -> Result<SceneData, Error> {
        let base = format!("scn/{folder}");
        let list = self
            .text(&format!("{base}/unitlist.xml"))
            .ok_or_else(|| Error::Missing(format!("{base}/unitlist.xml")))?;
        let elements = xml::elements(&list);
        let mut coordinates = Vec::new();
        let mut pending_x = None;
        for element in &elements {
            match (element.tag.as_str(), element.closing) {
                ("XIndex", false) => pending_x = element.text.parse::<i32>().ok(),
                ("ZIndex", false) => {
                    if let (Some(x), Ok(z)) = (pending_x.take(), element.text.parse::<i32>()) {
                        coordinates.push((x, z));
                    }
                }
                _ => {}
            }
        }
        let mut units = Vec::new();
        for (x, z) in coordinates {
            let dir = format!("{base}/({x},{z})");
            let heights: Option<Vec<f32>> = match self.read(&format!("{dir}/height.short")) {
                Ok((_, bytes)) if bytes.len() >= UNIT_SIZE * UNIT_SIZE * 2 => Some(
                    bytes
                        .chunks_exact(2)
                        .take(UNIT_SIZE * UNIT_SIZE)
                        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 * HEIGHT_SCALE)
                        .collect(),
                ),
                _ => None,
            };
            let terrain = heights.is_some();
            let heights = heights.unwrap_or_else(|| vec![0.0; UNIT_SIZE * UNIT_SIZE]);
            let water = self
                .read(&format!("{dir}/water.byte"))
                .map(|(_, bytes)| bytes)
                .unwrap_or_else(|_| vec![255; (UNIT_SIZE / WATER_CELL).pow(2)]);
            let preblend = self.texture(&format!("{base}/preblend/({x},{z}).dds"));
            let colors = self
                .read(&format!("{dir}/color.dword"))
                .ok()
                .map(|(_, bytes)| bytes)
                .filter(|bytes| bytes.len() >= UNIT_SIZE * UNIT_SIZE * 4);
            let blend = self
                .read(&format!("{dir}/bits_alpha.byte"))
                .ok()
                .and_then(|(_, bytes)| Blend::parse(&bytes));
            units.push(Unit {
                x,
                z,
                heights,
                terrain,
                water,
                preblend,
                blend,
                colors,
            });
        }
        let unit_index = units
            .iter()
            .enumerate()
            .map(|(i, u)| ((u.x, u.z), i))
            .collect();

        let sky = self
            .text(&format!("{base}/sky.xml"))
            .map(|t| parse_sky(&t))
            .unwrap_or_default();
        let valleys = self
            .text(&format!("{base}/valley.xml"))
            .map(|t| xml::elements(&t))
            .unwrap_or_default();
        let water_levels = valleys
            .iter()
            .filter(|e| e.tag == "LevelHeight" && !e.closing)
            .map(|e| e.text.parse().unwrap_or(0.0))
            .collect();
        let sea = valleys
            .iter()
            .filter(|e| e.tag == "Alias" && !e.closing)
            .position(|e| e.text.to_lowercase().starts_with("haishui"));
        let marks = self
            .text(&format!("{base}/mark.xml"))
            .map(|t| parse_marks(&t))
            .unwrap_or_default();

        let mut scene = SceneData {
            name: folder.to_string(),
            units,
            sky,
            water_levels,
            sea,
            layers: self
                .text(&format!("{base}/layer.xml"))
                .map(|t| parse_layers(&t))
                .unwrap_or_default(),
            marks,
            placements: Vec::new(),
            colliders: Vec::new(),
            unit_index,
        };
        let mut out = Collected::default();
        for unit in &scene.units {
            let dir = format!("{base}/({},{})", unit.x, unit.z);
            if let Some(t) = self.text(&format!("{dir}/ornaments2.xml")) {
                self.ornament_placements(&t, &mut out);
            }
            for file in ["sites2.xml", "citys2.xml"] {
                if let Some(t) = self.text(&format!("{dir}/{file}")) {
                    self.site_placements(&t, &mut out);
                }
            }
        }
        scene.placements = out.placements;
        scene.colliders = out.colliders;
        Ok(scene)
    }

    /// 碰撞网格（`*_h.pmf`）：位置已镜像成引擎右手系，三角形绕序跟着翻过来。读不到返回 `None`。
    pub fn collision_mesh(&mut self, raw: &str) -> Option<(Vec<Vec3>, Vec<u32>)> {
        let pmf = self.mesh(raw).ok()?;
        let positions = pmf.positions.iter().map(|&p| mirror(p)).collect();
        let mut indices = pmf.indices.clone();
        for triangle in indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
        (!indices.is_empty()).then_some((positions, indices))
    }

    /// `ornaments2.xml`：名字表 + 每条 15 个数的实例。
    fn ornament_placements(&mut self, text: &str, out: &mut Collected) {
        let (names, values) = name_table_and_instances(text);
        for record in values.chunks_exact(15) {
            let Some(ort) = names.get(record[12] as usize) else {
                continue;
            };
            let Some((model, phy)) = self.ort(ort) else {
                continue;
            };
            let transform = to_engine(local(
                Vec3::new(record[1], record[2], record[3]),
                Quat::from_xyzw(record[4], record[5], record[6], record[7]).normalize(),
                Vec3::splat(record[0]),
            ));
            if let Some(mesh) = phy {
                out.colliders.push(Collider { mesh, transform });
            }
            out.placements.push(Placement {
                model,
                transform,
                color: Vec4::new(record[8], record[9], record[10], record[11]),
            });
        }
    }

    /// `sites2.xml` / `citys2.xml`：名字表 + 每条 7 个数的实例（xyz、转角、名字号……）。
    fn site_placements(&mut self, text: &str, out: &mut Collected) {
        let (names, values) = name_table_and_instances(text);
        for record in values.chunks_exact(7) {
            let Some(path) = names.get(record[4] as usize).cloned() else {
                continue;
            };
            let transform = local(
                Vec3::new(record[0], record[1], record[2]),
                quarter_turns(record[3]),
                Vec3::ONE,
            );
            self.expand(&path, transform, 0, out);
        }
    }

    /// `.ort` → 它指向的 `.cct`，和碰撞网格（有的话）。
    fn ort(&mut self, ort: &str) -> Option<(String, Option<String>)> {
        let text = self.text(ort)?;
        let elements = xml::elements(&text);
        let model = first_text(&elements, "Model")?.to_string();
        let phy = first_text(&elements, "Phy")
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        Some((model, phy))
    }

    /// 按扩展名展开一个引用：`.cty` → 一组摆件，`.sit` → 几块砖，`.ort` / `.cct` → 一次摆放。
    /// `parent` 是左手系里的父变换。
    fn expand(&mut self, path: &str, parent: Mat4, depth: usize, out: &mut Collected) {
        if depth > 8 {
            return;
        }
        let lower = path.to_lowercase();
        if lower.ends_with(".cct") {
            out.placements.push(Placement {
                model: path.to_string(),
                transform: to_engine(parent),
                color: Vec4::ONE,
            });
        } else if lower.ends_with(".ort") {
            if let Some((model, phy)) = self.ort(path) {
                let transform = to_engine(parent);
                if let Some(mesh) = phy {
                    out.colliders.push(Collider { mesh, transform });
                }
                out.placements.push(Placement {
                    model,
                    transform,
                    color: Vec4::ONE,
                });
            }
        } else if lower.ends_with(".sit") {
            let Some(text) = self.text(path) else { return };
            for (child, transform) in nested_placements(&text, &["Brick"], "Model") {
                self.expand(&child, parent * transform, depth + 1, out);
            }
            for (mesh, transform) in nested_placements(&text, &["Brick"], "Phy") {
                out.colliders.push(Collider {
                    mesh,
                    transform: to_engine(parent * transform),
                });
            }
            if let Some(mesh) =
                first_text(&xml::elements(&text), "Projectee").filter(|p| !p.is_empty())
            {
                out.colliders.push(Collider {
                    mesh: mesh.to_string(),
                    transform: to_engine(parent),
                });
            }
        } else if lower.ends_with(".cty") {
            let Some(text) = self.text(path) else { return };
            for (child, transform) in nested_placements(&text, &["Fitment", "Villa"], "Bld") {
                self.expand(&child, parent * transform, depth + 1, out);
            }
        }
    }

    /// 物件 `.cct`（`<StaticObject>` / 物件用的 `<Character>`）→ 网格和材质。
    ///
    /// `<Plant>`：网格 + 贴图，花草树叶都靠贴图 alpha 镂空，`Dual` 双面。
    /// `<Building>`：网格 + 底图（`BaseMap`）+ 光照贴图（`LightMap`，第二套 UV，暂时没用上）。
    pub fn static_object(&mut self, raw: &str) -> Vec<StaticPart> {
        let Some(text) = self.text(raw) else {
            klog::warn!("物件 {raw} 读不到");
            return Vec::new();
        };
        let elements = xml::elements(&text);
        let mut parts = Vec::new();
        let mut index = 0;
        while index < elements.len() {
            let element = &elements[index];
            index += 1;
            if element.closing || !(element.tag == "Plant" || element.tag == "Building") {
                continue;
            }
            let building = element.tag == "Building";
            let start = index;
            while index < elements.len()
                && !(elements[index].tag == element.tag && elements[index].closing)
            {
                index += 1;
            }
            let body = &elements[start..index];
            let Some(mesh_path) = first_text(body, "MeshFile") else {
                continue;
            };
            let texture_path = first_text(body, if building { "BaseMap" } else { "TextureFile" })
                .unwrap_or_default();
            // 没写 `Dual` 的植物多是单层的叶片、草片（自带背面的不到 5%），照旧双面画；写明 `No` 的才单面。
            let dual = match first_text(body, "Dual") {
                Some(flag) => flag == "Yes",
                None => !building,
            };
            let pmf = match self.mesh(mesh_path) {
                Ok(pmf) => pmf,
                Err(error) => {
                    klog::warn!("物件 {raw}：{error}");
                    continue;
                }
            };
            let Some(mesh) = convert_mesh(&pmf, 0) else {
                continue;
            };
            let mut material = Material::default().with_roughness(0.9).with_metallic(0.0);
            if let Some(texture) = self.texture(texture_path) {
                material = material.with_base_color_texture(texture);
            }
            // 植物（草、花、树叶）几乎全是镂空贴图；建筑按贴图 alpha 裁也不吃亏（不透明的 alpha 恒为 1）。
            kpbr::physical::Physical {
                alpha_cutoff: 0.5,
                ..Default::default()
            }
            .apply(&mut material);
            if !dual {
                parts.push(StaticPart { mesh, material });
                continue;
            }
            // 要双面画的部件里，布帘、屏风、晾的布这类常常是正反两面各建一份、贴在同一个平面上：再双面画，
            // 正面三角形的背面和反面三角形的正面落在同一深度上打架，镜头一动（TAA 的亚像素抖动也算）就闪。
            // 这些自带背面的三角形拆出来单面画，其余的照旧双面。
            let (single, double) = split_back_to_back(&mesh);
            if let Some(single) = single {
                parts.push(StaticPart {
                    mesh: single,
                    material: material.clone(),
                });
            }
            if let Some(double) = double {
                parts.push(StaticPart {
                    mesh: double,
                    material: material.with_double_sided(),
                });
            }
        }
        parts
    }
}

/// 把网格分成「背靠背」和其余两份，各自只留用到的顶点。哪份没有三角形就是 `None`。
///
/// 背靠背：和另一个三角形共面、朝向相反、互相盖住（一个的重心落在另一个里面）。两面的剖分常常不一样
/// （同一块布，正面沿这条对角线切、反面沿那条切），所以不能只比顶点。
fn split_back_to_back(mesh: &Mesh) -> (Option<Mesh>, Option<Mesh>) {
    /// 共面的容差（米）：两层面片离得比这还近，深度缓冲就分不清谁在前了。
    const PLANE: f32 = 0.003;
    let vertices = mesh.vertices();
    let indices = mesh.indices();
    struct Face {
        corners: [Vec3; 3],
        normal: Vec3,
        centroid: Vec3,
    }
    let faces: Vec<Option<Face>> = indices
        .chunks_exact(3)
        .map(|t| {
            let corners = [0, 1, 2].map(|k| vertices[t[k] as usize].position());
            let normal = (corners[1] - corners[0])
                .cross(corners[2] - corners[0])
                .try_normalize()?;
            let centroid = (corners[0] + corners[1] + corners[2]) / 3.0;
            Some(Face {
                corners,
                normal,
                centroid,
            })
        })
        .collect();
    // 按平面到原点的距离分桶：背靠背的两个三角形法线相反、距离也相反，绝对值一样。
    let bucket = |f: &Face| (f.normal.dot(f.corners[0]).abs() / PLANE).round() as i64;
    let mut buckets: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, face) in faces.iter().enumerate() {
        if let Some(face) = face {
            buckets.entry(bucket(face)).or_default().push(i);
        }
    }
    let contains = |f: &Face, p: Vec3| {
        (0..3).all(|k| {
            let (a, b) = (f.corners[k], f.corners[(k + 1) % 3]);
            (b - a).cross(p - a).dot(f.normal) >= -1e-6
        })
    };
    let mut paired = vec![false; faces.len()];
    for (i, face) in faces.iter().enumerate() {
        let Some(a) = face else { continue };
        let key = bucket(a);
        for j in (key - 1..=key + 1).flat_map(|k| buckets.get(&k).into_iter().flatten()) {
            let Some(b) = &faces[*j] else { continue };
            if a.normal.dot(b.normal) < -0.999
                && (a.centroid - b.corners[0]).dot(b.normal).abs() < PLANE
                && (contains(b, a.centroid) || contains(a, b.centroid))
            {
                paired[i] = true;
                break;
            }
        }
    }
    if !paired.contains(&true) {
        return (None, Some(mesh.clone()));
    }
    let subset = |keep: bool| {
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut out_vertices = Vec::new();
        let mut out_indices = Vec::new();
        for (t, &p) in indices.chunks_exact(3).zip(&paired) {
            if p != keep {
                continue;
            }
            for &i in t {
                let next = remap.len() as u32;
                let index = *remap.entry(i).or_insert_with(|| {
                    out_vertices.push(vertices[i as usize]);
                    next
                });
                out_indices.push(index);
            }
        }
        (!out_indices.is_empty()).then(|| Mesh::new(out_vertices, out_indices))
    };
    (subset(true), subset(false))
}

/// 读场景时一路收集的摆放和碰撞。
#[derive(Default)]
struct Collected {
    placements: Vec<Placement>,
    colliders: Vec<Collider>,
}

/// 名字表（`<NameTable>` 里的 `<value>`）和 `<Instance>` 里的数。
fn name_table_and_instances(text: &str) -> (Vec<String>, Vec<f32>) {
    let elements = xml::elements(text);
    let mut names = Vec::new();
    let mut in_table = false;
    let mut values = Vec::new();
    for element in &elements {
        match (element.tag.as_str(), element.closing) {
            ("NameTable", false) if !element.self_closing => in_table = true,
            ("NameTable", true) => in_table = false,
            ("value", false) if in_table && !element.self_closing => {
                names.push(element.text.clone())
            }
            ("Instance", false) => {
                values = element
                    .text
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
            }
            _ => {}
        }
    }
    (names, values)
}

/// 在嵌套的 XML 里找「摆件帧」（`frames` 里的标签），每帧的直接子元素 `Pos` / `Rot` / `reference` 组成一次摆放。
/// 帧可以嵌套（`.cty` 的 `Villa` 里还有一层 `Fitment`），里层的变换相对外层。返回 (引用路径, 左手系的局部变换)。
fn nested_placements(text: &str, frames: &[&str], reference: &str) -> Vec<(String, Mat4)> {
    struct Frame {
        depth: usize,
        position: Vec3,
        rotation: Quat,
        reference: Option<String>,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut out = Vec::new();
    let mut depth = 0usize;
    for element in xml::elements(text) {
        if element.closing {
            depth = depth.saturating_sub(1);
            if frames.contains(&element.tag.as_str())
                && stack.last().is_some_and(|frame| frame.depth == depth)
                && let Some(frame) = stack.pop()
                && let Some(path) = frame.reference
            {
                // 这一帧（和所有外层帧）的变换叠起来。
                let mut transform = local(frame.position, frame.rotation, Vec3::ONE);
                for outer in stack.iter().rev() {
                    transform = local(outer.position, outer.rotation, Vec3::ONE) * transform;
                }
                out.push((path, transform));
            }
            continue;
        }
        let here = depth;
        if !element.self_closing {
            depth += 1;
        }
        if frames.contains(&element.tag.as_str()) && !element.self_closing {
            stack.push(Frame {
                depth: here,
                position: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                reference: None,
            });
            continue;
        }
        // 只认这一帧的直接子元素：包围盒（`BB`）、灯（`Light`）里也有 `Pos` 一类的东西。
        let Some(frame) = stack.last_mut().filter(|frame| here == frame.depth + 1) else {
            continue;
        };
        match element.tag.as_str() {
            "Pos" => frame.position = vec3_of(&element.text).unwrap_or(Vec3::ZERO),
            "Rot" => frame.rotation = quat_of(&element.text).unwrap_or(Quat::IDENTITY),
            tag if tag == reference && !element.text.is_empty() => {
                frame.reference = Some(element.text.clone())
            }
            _ => {}
        }
    }
    out
}

/// `layer.xml`：每个 `<Layer>` 的 `Index` / `Alias` / `Scale` / `Texture`。
fn parse_layers(text: &str) -> Vec<Option<TerrainLayer>> {
    let mut layers: Vec<Option<TerrainLayer>> = Vec::new();
    let elements = xml::elements(text);
    let mut start = None;
    for (i, element) in elements.iter().enumerate() {
        match (element.tag.as_str(), element.closing) {
            ("Layer", false) => start = Some(i),
            ("Layer", true) => {
                let Some(begin) = start.take() else { continue };
                let body = &elements[begin..i];
                let Some(index) = first_text(body, "Index").and_then(|t| t.parse::<usize>().ok())
                else {
                    continue;
                };
                let alias = first_text(body, "Alias").unwrap_or_default().to_string();
                let texture = first_text(body, "Texture").unwrap_or_default().to_string();
                let scale = first_text(body, "Scale")
                    .map(parse_floats)
                    .and_then(|v| v.first().copied())
                    .filter(|s| *s > 0.0)
                    .unwrap_or(8.0);
                if index >= 64 {
                    continue;
                }
                if layers.len() <= index {
                    layers.resize(index + 1, None);
                }
                // `no` 是没用上的占位层（贴图都是同一张 `zy_dimian_022`）。
                if alias != "no" && !texture.is_empty() {
                    layers[index] = Some(TerrainLayer {
                        alias,
                        scale,
                        texture,
                    });
                }
            }
            _ => {}
        }
    }
    layers
}

fn parse_sky(text: &str) -> Sky {
    let elements = xml::elements(text);
    // 只取第一个时段（`Time_0`）：它在文件最前面，`first_text` 正好拿到它。
    let mut sky = Sky::default();
    if let Some(c) = first_text(&elements, "AmbientColor").and_then(rgb_of) {
        sky.ambient = c;
    }
    if let Some(c) = first_text(&elements, "DiffuseColor").and_then(rgb_of) {
        sky.diffuse = c;
    }
    if let Some(c) = first_text(&elements, "FogColor").and_then(rgb_of) {
        sky.fog_color = c;
    }
    if let Some(v) = first_text(&elements, "FogStart").and_then(|t| t.parse().ok()) {
        sky.fog_start = v;
    }
    if let Some(v) = first_text(&elements, "FogEnd").and_then(|t| t.parse().ok()) {
        sky.fog_end = v;
    }
    let pitch: Option<f32> = first_text(&elements, "DiffusePitch").and_then(|t| t.parse().ok());
    let yaw: Option<f32> = first_text(&elements, "DiffuseYaw").and_then(|t| t.parse().ok());
    if let (Some(pitch), Some(yaw)) = (pitch, yaw) {
        // 俯仰是太阳离地平线的高度角、偏航绕 Y；在左手系里算好再镜像 Z。
        let direction = Vec3::new(
            pitch.cos() * yaw.sin(),
            pitch.sin(),
            pitch.cos() * yaw.cos(),
        );
        sky.sun_direction = mirror(direction).normalize();
    }
    sky.sky_colors = (0..8)
        .filter_map(|i| first_text(&elements, &format!("SkyColor_{i}")).and_then(rgb_of))
        .collect();
    sky
}

fn parse_marks(text: &str) -> Vec<Mark> {
    let mut marks = Vec::new();
    let mut current: Option<Mark> = None;
    for element in xml::elements(text) {
        match (element.tag.as_str(), element.closing) {
            ("Alias", false) => {
                if let Some(mark) = current.take() {
                    marks.push(mark);
                }
                current = Some(Mark {
                    alias: element.text.clone(),
                    kind: String::new(),
                    position: Vec3::ZERO,
                    yaw: 0.0,
                });
            }
            ("Type", false) => {
                if let Some(mark) = current.as_mut() {
                    mark.kind = element.text.clone();
                }
            }
            ("Pos", false) => {
                if let (Some(mark), Some(p)) = (current.as_mut(), vec3_of(&element.text)) {
                    mark.position = mirror(p);
                }
            }
            ("Yaw", false) => {
                if let Some(mark) = current.as_mut() {
                    mark.yaw = element.text.parse().unwrap_or(0.0);
                }
            }
            _ => {}
        }
    }
    marks.extend(current);
    marks
}

impl SceneData {
    pub fn unit(&self, x: i32, z: i32) -> Option<&Unit> {
        self.unit_index.get(&(x, z)).map(|&i| &self.units[i])
    }

    /// 原始（左手系）坐标上的一个整数采样点的高度；跨到相邻单元去取，取不到就钳在本单元边上。
    /// 一个采样点的顶点明暗（线性空间）；跨到相邻单元去取，没有数据是白色。
    fn shade(&self, unit: &Unit, lx: i32, lz: i32) -> Vec3 {
        let size = UNIT_SIZE as i32;
        let (ux, x) = if lx >= size {
            (unit.x + 1, lx - size)
        } else {
            (unit.x, lx)
        };
        let (uz, z) = if lz >= size {
            (unit.z + 1, lz - size)
        } else {
            (unit.z, lz)
        };
        let (target, x, z) = match self.unit(ux, uz) {
            Some(target) if target.colors.is_some() => (target, x, z),
            _ => (unit, lx.min(size - 1), lz.min(size - 1)),
        };
        let Some(colors) = &target.colors else {
            return Vec3::ONE;
        };
        let at = (x as usize * UNIT_SIZE + z as usize) * 4;
        // D3D 的光照是在 gamma 空间里乘的；乘法换到线性空间仍然是乘法，按 sRGB 解码即可。
        let linear = |v: u8| (v as f32 / 255.0).powf(2.2);
        Vec3::new(
            linear(colors[at + 2]),
            linear(colors[at + 1]),
            linear(colors[at]),
        )
    }

    fn sample(&self, unit: &Unit, lx: i32, lz: i32) -> f32 {
        let (mut ux, mut uz, mut x, mut z) = (unit.x, unit.z, lx, lz);
        let size = UNIT_SIZE as i32;
        if x >= size {
            ux += 1;
            x -= size;
        }
        if z >= size {
            uz += 1;
            z -= size;
        }
        let target = self.unit(ux, uz).unwrap_or(unit);
        let (x, z) = if std::ptr::eq(target, unit) && (ux, uz) != (unit.x, unit.z) {
            (lx.min(size - 1), lz.min(size - 1))
        } else {
            (x.clamp(0, size - 1), z.clamp(0, size - 1))
        };
        target.heights[x as usize * UNIT_SIZE + z as usize]
    }

    /// 引擎坐标 `(x, -z)` 处的地面高度（双线性插值）。不在任何单元里返回 `None`。
    pub fn height_at(&self, position: Vec3) -> Option<f32> {
        let (wx, wz) = (position.x, -position.z);
        let size = UNIT_SIZE as f32;
        let unit = self.unit((wx / size).floor() as i32, (wz / size).floor() as i32)?;
        let lx = wx - unit.x as f32 * size;
        let lz = wz - unit.z as f32 * size;
        let (x0, z0) = (lx.floor() as i32, lz.floor() as i32);
        let (fx, fz) = (lx - x0 as f32, lz - z0 as f32);
        let h = |dx: i32, dz: i32| self.sample(unit, x0 + dx, z0 + dz);
        let top = h(0, 0) + (h(1, 0) - h(0, 0)) * fx;
        let bottom = h(0, 1) + (h(1, 1) - h(0, 1)) * fx;
        Some(top + (bottom - top) * fz)
    }

    /// 一个单元的地形网格：257×257 个顶点（最后一行一列取相邻单元的，接缝不留缝），贴预混贴图。
    /// `step` 是采样间隔（1 = 每米一个顶点；远处可以用 2、4 省三角形）。
    pub fn terrain_mesh(&self, unit: &Unit, step: usize) -> Mesh {
        let step = step.max(1);
        let count = UNIT_SIZE / step + 1;
        let size = UNIT_SIZE as f32;
        let mut vertices = Vec::with_capacity(count * count);
        for i in 0..count {
            for j in 0..count {
                let (lx, lz) = ((i * step) as i32, (j * step) as i32);
                let height = self.sample(unit, lx, lz);
                // 有限差分求法线（左手系里算，再镜像 Z）。
                let dx = self.sample(unit, lx + 1, lz) - self.sample(unit, (lx - 1).max(0), lz);
                let dz = self.sample(unit, lx, lz + 1) - self.sample(unit, lx, (lz - 1).max(0));
                let normal = mirror(Vec3::new(-dx, 2.0, -dz).normalize());
                let world = Vec3::new(
                    unit.x as f32 * size + lx as f32,
                    height,
                    unit.z as f32 * size + lz as f32,
                );
                // 预混贴图按 x 镜像存的：u 沿 −x、v 沿 z。拿相邻单元的贴图拼缝核对过——这样拼，
                // 接缝两边的像素平均只差 ~8（不翻是 ~86），也正好和高度图的山脊、河道对上。
                let uv = [1.0 - lx as f32 / size, lz as f32 / size];
                vertices.push(
                    Vertex::new(mirror(world), normal, uv).with_color(self.shade(unit, lx, lz)),
                );
            }
        }
        let mut indices = Vec::with_capacity((count - 1) * (count - 1) * 6);
        for i in 0..count - 1 {
            for j in 0..count - 1 {
                let a = (i * count + j) as u32;
                let b = a + count as u32;
                // 引擎里 x 沿 i 增大、z 沿 j 减小（镜像过 Z），这样排法线朝上（逆时针为正面）。
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        Mesh::new(vertices, indices)
    }

    /// 一个单元的水面：每个有水的格子一块平面，高度取 `valley.xml` 里那片水的高度。没水返回 `None`。
    pub fn water_mesh(&self, unit: &Unit) -> Option<Mesh> {
        let cells = UNIT_SIZE / WATER_CELL;
        let size = UNIT_SIZE as f32;
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for i in 0..cells {
            for j in 0..cells {
                let Some(&valley) = unit.water.get(i * cells + j) else {
                    continue;
                };
                // 大海整片单独铺（`sea_mesh`），这里再画一遍会和它打架。
                if self.sea == Some(valley as usize) {
                    continue;
                }
                let Some(&level) = self.water_levels.get(valley as usize) else {
                    continue;
                };
                let x0 = unit.x as f32 * size + (i * WATER_CELL) as f32;
                let z0 = unit.z as f32 * size + (j * WATER_CELL) as f32;
                let cell = WATER_CELL as f32;
                let base = vertices.len() as u32;
                for (dx, dz) in [(0.0, 0.0), (cell, 0.0), (0.0, cell), (cell, cell)] {
                    let p = mirror(Vec3::new(x0 + dx, level, z0 + dz));
                    vertices.push(Vertex::new(p, Vec3::Y, [(x0 + dx) / 8.0, (z0 + dz) / 8.0]));
                }
                indices.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 2,
                    base + 1,
                    base + 3,
                    base + 2,
                ]);
            }
        }
        (!vertices.is_empty()).then(|| Mesh::new(vertices, indices))
    }

    /// 大海：盖住所有单元再往外延 `margin` 米的一整块平面。没有大海返回 `None`。
    ///
    /// `water.byte` 里大海只标了零星几格（整块是海的单元也只标两格），游戏是把海平面当成一个
    /// 全局高度铺满的：地形高过它的地方自然把它挡住。
    pub fn sea_mesh(&self, margin: f32) -> Option<Mesh> {
        let level = *self.water_levels.get(self.sea?)?;
        let size = UNIT_SIZE as f32;
        let min_x = self.units.iter().map(|u| u.x).min()? as f32 * size - margin;
        let max_x = (self.units.iter().map(|u| u.x).max()? + 1) as f32 * size + margin;
        let min_z = self.units.iter().map(|u| u.z).min()? as f32 * size - margin;
        let max_z = (self.units.iter().map(|u| u.z).max()? + 1) as f32 * size + margin;
        let vertices = [
            (min_x, min_z),
            (max_x, min_z),
            (min_x, max_z),
            (max_x, max_z),
        ]
        .map(|(x, z)| Vertex::new(mirror(Vec3::new(x, level, z)), Vec3::Y, [x / 8.0, z / 8.0]))
        .to_vec();
        Some(Mesh::new(vertices, vec![0, 1, 2, 1, 3, 2]))
    }

    /// 按名字找标记（第一个）。
    pub fn mark(&self, alias: &str) -> Option<&Mark> {
        self.marks.iter().find(|m| m.alias == alias)
    }
}

/// 把摆放按 `.cct` 分组，方便一种物件建一次网格、用实例化一次画完。
pub fn group_placements(placements: &[Placement]) -> Vec<(String, Vec<(Mat4, Vec4)>)> {
    let mut groups: HashMap<String, Vec<(Mat4, Vec4)>> = HashMap::new();
    for placement in placements {
        groups
            .entry(placement.model.to_lowercase())
            .or_default()
            .push((placement.transform, placement.color));
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 地形和水面的三角形都得朝上（逆时针为正面），不然从上往下看被背面剔除，地面整个消失。
    #[test]
    fn terrain_and_water_face_up() {
        let mut heights = vec![0.0; UNIT_SIZE * UNIT_SIZE];
        heights[5] = 1.0;
        let unit = Unit {
            x: 0,
            z: 0,
            heights,
            terrain: true,
            water: vec![0; 256],
            preblend: None,
            blend: None,
            colors: None,
        };
        let scene = SceneData {
            name: String::new(),
            units: vec![unit],
            sky: Sky::default(),
            water_levels: vec![1.0],
            sea: None,
            layers: Vec::new(),
            marks: Vec::new(),
            placements: Vec::new(),
            colliders: Vec::new(),
            unit_index: [((0, 0), 0)].into_iter().collect(),
        };
        let unit = &scene.units[0];
        let sea = SceneData {
            sea: Some(0),
            ..scene.clone()
        };
        for mesh in [
            scene.terrain_mesh(unit, 32),
            scene.water_mesh(unit).unwrap(),
            sea.sea_mesh(10.0).unwrap(),
        ] {
            let vertices = mesh.vertices();
            for t in mesh.indices().chunks_exact(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| vertices[i as usize].position());
                assert!((b - a).cross(c - a).y > 0.0, "有三角形朝下");
            }
        }
    }

    /// 正反两面各建一份的三角形单独拆出来（单面画），剩下的照旧双面画；两份都只带自己用到的顶点。
    #[test]
    fn back_to_back_triangles_are_split_out() {
        let v = |x: f32, z: f32| Vertex::new(Vec3::new(x, 0.0, z), Vec3::Y, [0.0, 0.0]);
        // 0..3 正面，3..6 同样位置的反面（另存一份顶点），6..9 一片单独的叶子。
        let vertices = vec![
            v(0.0, 0.0),
            v(1.0, 0.0),
            v(0.0, 1.0),
            v(0.0, 0.0),
            v(1.0, 0.0),
            v(0.0, 1.0),
            v(5.0, 5.0),
            v(6.0, 5.0),
            v(5.0, 6.0),
        ];
        let mesh = Mesh::new(vertices, vec![0, 1, 2, 4, 3, 5, 6, 7, 8]);
        let (single, double) = split_back_to_back(&mesh);
        let (single, double) = (single.unwrap(), double.unwrap());
        assert_eq!(single.indices().len(), 6);
        assert_eq!(single.vertices().len(), 6);
        assert_eq!(double.indices(), &[0, 1, 2]);
        assert_eq!(double.vertices()[0].position(), Vec3::new(5.0, 0.0, 5.0));
        // 同一块方布，正面沿 0-3 对角线切、反面沿 1-2 切：顶点对不上，照样认得出来。
        let quad = vec![v(0.0, 0.0), v(1.0, 0.0), v(0.0, 1.0), v(1.0, 1.0)];
        let cloth = Mesh::new(quad, vec![0, 3, 1, 0, 2, 3, 0, 1, 2, 1, 3, 2]);
        let (single, double) = split_back_to_back(&cloth);
        assert_eq!(single.unwrap().indices().len(), 12);
        assert!(double.is_none());
        // 同绕序的重复不算背靠背。
        let mesh = Mesh::new(mesh.vertices().to_vec(), vec![0, 1, 2, 3, 4, 5]);
        assert!(split_back_to_back(&mesh).0.is_none());
    }

    #[test]
    fn mirroring_a_placement_matches_mirroring_its_points() {
        let m = local(
            Vec3::new(3.0, 1.0, 5.0),
            Quat::from_rotation_y(0.7),
            Vec3::splat(2.0),
        );
        let p = Vec3::new(0.5, 0.2, -1.0);
        let left = m.transform_point3(p);
        let right = to_engine(m).transform_point3(mirror(p));
        assert!((mirror(left) - right).length() < 1e-5);
    }

    /// `bits_alpha.byte`：头 4 字节是层掩码，每个置位的层一张 256×256 权重，按层号排。
    #[test]
    fn blend_reads_the_layer_mask_and_planes() {
        let plane = UNIT_SIZE * UNIT_SIZE;
        let mut bytes = (0b1_0000_0101u32).to_le_bytes().to_vec();
        for value in [255u8, 7, 9] {
            bytes.extend(std::iter::repeat_n(value, plane));
        }
        let blend = Blend::parse(&bytes).unwrap();
        assert_eq!(blend.layers, vec![0, 2, 8]);
        assert_eq!(blend.weights[1][123], 7);
        assert_eq!(blend.weights[2][plane - 1], 9);
        // 数据比掩码说的少：不认。
        assert!(Blend::parse(&bytes[..bytes.len() - 1]).is_none());
    }

    #[test]
    fn layers_skip_placeholders_and_keep_index() {
        let text = r#"<Root><Layers>
  <Layer><Index>0</Index><Alias>lc</Alias><Scale>{6,1,6}</Scale><Texture>a.dds</Texture></Layer>
  <Layer><Index>1</Index><Alias>no</Alias><Scale>{1,1,1}</Scale><Texture>z.dds</Texture></Layer>
  <Layer><Index>3</Index><Alias>gt</Alias><Scale>{8,1,8}</Scale><Texture>b.dds</Texture></Layer>
</Layers></Root>"#;
        let layers = parse_layers(text);
        assert_eq!(layers.len(), 4);
        assert_eq!(layers[0].as_ref().unwrap().scale, 6.0);
        assert!(layers[1].is_none() && layers[2].is_none());
        assert_eq!(layers[3].as_ref().unwrap().texture, "b.dds");
    }

    #[test]
    fn nested_frames_compose_and_ignore_deeper_positions() {
        let text = r#"<Root>
  <Statuary>
    <value><Fitment><Bld>a.sit</Bld><Pos>{1,0,0}</Pos><Rot>{0,0,0,1}</Rot></Fitment></value>
  </Statuary>
  <Villa>
    <value>
      <Villa>
        <Pos>{10,0,0}</Pos>
        <Rot>{0,0,0,1}</Rot>
        <Bld>house.sit</Bld>
        <InS><value><Sector><BList><value><BB><Cnt>{9,9,9}</Cnt></BB></value></BList>
          <Fit><value><Fitment><Bld>chair.sit</Bld><Pos>{0,0,2}</Pos><Rot>{0,0,0,1}</Rot></Fitment></value></Fit>
        </Sector></value></InS>
      </Villa>
    </value>
  </Villa>
</Root>"#;
        let found = nested_placements(text, &["Fitment", "Villa"], "Bld");
        let at = |name: &str| {
            found
                .iter()
                .find(|(path, _)| path == name)
                .map(|(_, m)| m.transform_point3(Vec3::ZERO))
                .unwrap()
        };
        assert_eq!(at("a.sit"), Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(at("house.sit"), Vec3::new(10.0, 0.0, 0.0));
        // 屋里的椅子相对房子摆。
        assert_eq!(at("chair.sit"), Vec3::new(10.0, 0.0, 2.0));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn reads_instance_tables() {
        let text = r#"<Root><Site><NameTable><value>a.sit</value><value>b.sit</value></NameTable>
<ParamTable><value/></ParamTable><Instance size="14">1,2,3,0,1,0,0,4,5,6,1,0,0,0,</Instance></Site></Root>"#;
        let (names, values) = name_table_and_instances(text);
        assert_eq!(names, ["a.sit", "b.sit"]);
        assert_eq!(values.len(), 14);
        assert_eq!(values[4], 1.0);
    }
}
