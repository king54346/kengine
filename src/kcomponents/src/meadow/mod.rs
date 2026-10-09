//! 风格化草地：起伏的地、几万根随风摆的草叶、零星的野花。
//!
//! 照 [three-stylized](https://github.com/) 移植（它又改编自 cortiz2894/stylized-components）。
//!
//! ```ignore
//! let mut meadow = Meadow::new(MeadowSettings::default());
//! meadow.spawn(&mut scene);
//! // 改风、改颜色不用重建：
//! meadow.set_wind(&mut scene, WindSettings { strength: 0.4, ..Default::default() });
//! ```
//!
//! # 和原版不一样的地方
//!
//! - **草叶是实例**：一份 9 个顶点的叶片网格，每根草一个 [`Instance`]（位置、朝向、宽高、深浅），
//!   按 [`GrassSettings::tile_size`] 切块、每块一个实例化节点，各自剔除。风的相位取实例矩阵的原点。
//! - **光照接的是场景里真的灯**：原版每帧把太阳的方向和颜色抄进 uniform，这里走材质的光照钩子，
//!   哪盏灯照过来就用哪盏，影子由引擎给。
//! - 草叶也可以长在别的表面上：[`SurfaceSampler`] 撒点、[`grass_instances`] 生成实例，配 [`blade_mesh`] 挂到节点上。

mod mask;
pub mod scatter;
pub mod terrain;

use kasset::Resource;
use kcore::pool::Handle;
use kmaterial::Material;
use kmath::{Aabb, Quat, Rng, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use kscene::{Instance, Node, Scene};
use ktexture::{FilterMode, Sampler, Texture, TextureFormat, WrapMode};

pub use scatter::{SurfacePoint, SurfaceSampler};
pub use terrain::TerrainSettings;

const COMMON: &str = include_str!("meadow_common.wgsl");
const GRASS: &str = include_str!("grass.wgsl");
const FLOWERS: &str = include_str!("flowers.wgsl");

/// `0xRRGGBB` → sRGB 0..1。
pub fn hex(rgb: u32) -> Vec3 {
    Vec3::new(
        ((rgb >> 16) & 0xff) as f32,
        ((rgb >> 8) & 0xff) as f32,
        (rgb & 0xff) as f32,
    ) / 255.0
}

/// sRGB → 线性。
fn srgb(c: Vec3) -> Vec3 {
    Vec3::new(
        kmath::srgb_to_linear(c.x),
        kmath::srgb_to_linear(c.y),
        kmath::srgb_to_linear(c.z),
    )
}

fn rgb_to_hsl(c: Vec3) -> Vec3 {
    let (max, min) = (c.max_element(), c.min_element());
    let l = (max + min) * 0.5;
    if max == min {
        return Vec3::new(0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == c.x {
        (c.y - c.z) / d + if c.y < c.z { 6.0 } else { 0.0 }
    } else if max == c.y {
        (c.z - c.x) / d + 2.0
    } else {
        (c.x - c.y) / d + 4.0
    };
    Vec3::new(h / 6.0, s, l)
}

fn hsl_to_rgb(hsl: Vec3) -> Vec3 {
    let (h, s, l) = (
        hsl.x.rem_euclid(1.0),
        hsl.y.clamp(0.0, 1.0),
        hsl.z.clamp(0.0, 1.0),
    );
    if s == 0.0 {
        return Vec3::splat(l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    Vec3::new(channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

/// three.js 的 `Color.offsetHSL`（在线性空间里做，和它一致）。
fn offset_hsl(c: Vec3, offset: Vec3) -> Vec3 {
    hsl_to_rgb(rgb_to_hsl(c) + offset)
}

fn offset_lightness(c: Vec3, amount: f32) -> Vec3 {
    offset_hsl(c, Vec3::new(0.0, 0.0, amount))
}

/// 草叶的形状和大小。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BladeSettings {
    pub min_width: f32,
    pub max_width: f32,
    pub min_height: f32,
    pub max_height: f32,
    /// 竖向分几段（叶片顶上再收一个尖）。
    pub segments: u32,
    /// 叶片自带的弯（米，梢部往前探多远）。
    pub lean: f32,
}

impl Default for BladeSettings {
    fn default() -> Self {
        Self {
            min_width: 0.035,
            max_width: 0.16,
            min_height: 0.705,
            max_height: 1.5,
            segments: 4,
            lean: 0.1,
        }
    }
}

/// 风。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindSettings {
    /// 摆幅（米，梢部）。
    pub strength: f32,
    pub speed: f32,
    /// 空间频率：越大一波越短。
    pub frequency: f32,
    /// 垂直方向那道小波的比例。
    pub turbulence: f32,
    /// 常驻的倾斜（米）：风一直往一边压。
    pub lean: f32,
    /// 风向（度，0 = +x，90 = +z）。
    pub direction: f32,
}

impl Default for WindSettings {
    fn default() -> Self {
        Self {
            strength: 0.22,
            speed: 1.1,
            frequency: 0.55,
            turbulence: 0.24,
            lean: 0.035,
            direction: 32.0,
        }
    }
}

impl WindSettings {
    fn params(&self) -> (Vec4, Vec3) {
        let radians = self.direction.to_radians();
        (
            Vec4::new(radians.cos(), radians.sin(), self.strength, self.speed),
            Vec3::new(self.frequency, self.turbulence, self.lean),
        )
    }
}

/// 草的颜色和光照手感。颜色都是 sRGB。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrassStyle {
    pub bottom: Vec3,
    pub top: Vec3,
    /// 逆光时透出来的颜色。
    pub backlight: Vec3,
    /// 整体亮度。
    pub brightness: f32,
    /// 逆光强度（0 = 关）。
    pub backlight_strength: f32,
    /// 逆光的集中度：越大越只在正对太阳时亮。
    pub backlight_power: f32,
    /// 逆光偏向叶梢的程度 0..1。
    pub backlight_tip: f32,
    /// 影子里还剩多少直射光（风格化的「影子不死黑」）。
    pub shadow_floor: f32,
    /// 太阳贴着地平线时漫反射的底。
    pub diffuse_floor: f32,
    /// 环境光（天空、半球光）的比例。
    pub ambient: f32,
}

impl Default for GrassStyle {
    fn default() -> Self {
        Self {
            bottom: hex(0x4f7c13),
            top: hex(0xb8da57),
            backlight: hex(0xc1e54d),
            // 原版的 0.35 是乘在「太阳强度 2.4」上的：和引擎的方向光强度同一个尺度，照抄。
            brightness: 0.35,
            backlight_strength: 2.5,
            backlight_power: 3.0,
            backlight_tip: 0.6,
            shadow_floor: 0.58,
            diffuse_floor: 0.35,
            // 原版完全不吃环境光（半球光只照地面）；留一点，背光面不至于死黑。
            ambient: 0.1,
        }
    }
}

/// 草。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrassSettings {
    /// 每平方米多少根。
    pub density: f32,
    pub seed: u32,
    pub blade: BladeSettings,
    pub style: GrassStyle,
    /// 投不投影子（几万根叶片投影，阴影图的开销不小；原版默认关）。
    pub shadows: bool,
    /// 切块的边长（米），每块一个节点、各自剔除。
    pub tile_size: f32,
}

impl Default for GrassSettings {
    fn default() -> Self {
        Self {
            density: 40.0,
            seed: 1,
            blade: BladeSettings::default(),
            style: GrassStyle::default(),
            shadows: false,
            tile_size: 10.0,
        }
    }
}

/// 野花。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WildflowerSettings {
    pub enabled: bool,
    /// 每平方米（长草的那部分）多少朵。
    pub density: f32,
    pub max_count: usize,
    pub seed: u32,
    pub brightness: f32,
}

impl Default for WildflowerSettings {
    fn default() -> Self {
        // 原版的花不受光（直接输出颜色）；这里受光，亮度给到和草同一个尺度。
        Self {
            enabled: true,
            density: 0.84,
            max_count: 240,
            seed: 29,
            brightness: 0.45,
        }
    }
}

/// 整块草地。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeadowSettings {
    pub terrain: TerrainSettings,
    pub grass: GrassSettings,
    pub wildflowers: WildflowerSettings,
    pub wind: WindSettings,
}

/// 四套野花配色（花瓣、茎叶、花心，sRGB）。
const PALETTES: [[u32; 3]; 4] = [
    [0xe9a5be, 0x467628, 0xf3c463],
    [0x9e8bd7, 0x3f7043, 0xf0d178],
    [0xe9b665, 0x58782c, 0xbf6950],
    [0x9aa9df, 0x466b50, 0xf3c7a2],
];

/// 草叶模型的一个顶点：位置、法线、uv。
type BladeVertex = (Vec3, Vec3, [f32; 2]);

/// 一根草叶的模型（宽 1、高 1，根在原点，叶面朝 +z，梢往 +z 弯）：`(位置, 法线, uv)`。
fn blade_shape(segments: u32, lean: f32) -> (Vec<BladeVertex>, Vec<u32>) {
    let segments = segments.max(1);
    let mut vertices = Vec::with_capacity(segments as usize * 2 + 1);
    let mut indices = Vec::new();
    // 叶面的切向是 (1,0,0) 和 (0,1,dz/dt)，法线是它们的叉积。
    let normal_at = |t: f32| Vec3::new(0.0, -2.0 * lean * t, 1.0).normalize();
    for row in 0..segments {
        let t = row as f32 / segments as f32;
        let width = 0.5 * (1.0 - t).powf(1.2);
        let bend = lean * t * t;
        vertices.push((Vec3::new(-width, t, bend), normal_at(t), [0.0, t]));
        vertices.push((Vec3::new(width, t, bend), normal_at(t), [1.0, t]));
        let v = row * 2;
        if row + 1 < segments {
            indices.extend_from_slice(&[v, v + 2, v + 1, v + 1, v + 2, v + 3]);
        }
    }
    vertices.push((Vec3::new(0.0, 1.0, lean), normal_at(1.0), [0.5, 1.0]));
    let last = (segments - 1) * 2;
    indices.extend_from_slice(&[last, segments * 2, last + 1]);
    (vertices, indices)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 一根草叶的网格（宽 1、高 1，根在原点）。每根草是它的一个实例（[`grass_instances`]）。
pub fn blade_mesh(blade: &BladeSettings) -> Mesh {
    let (shape, indices) = blade_shape(blade.segments, blade.lean);
    let vertices = shape
        .iter()
        .map(|&(position, normal, uv)| Vertex::new(position, normal, uv))
        .collect();
    // 包围盒按「被风吹出去」放宽：x 方向乘的是叶宽（0.1 米上下），给得大些；z 不缩放。
    Mesh::new(vertices, indices).with_bounds(Aabb::new(
        Vec3::new(-4.0, 0.0, -0.5),
        Vec3::new(4.0, 1.0, 0.5 + blade.lean),
    ))
}

/// 撒在 `points` 上的草叶实例：随机绕法线转一圈、随机宽高、±6% 的深浅。
///
/// 配 [`blade_mesh`] 和 [`grass_material`] 挂到一个节点上就是一片草（`Node::with_instances`）。
pub fn grass_instances(points: &[SurfacePoint], blade: &BladeSettings, seed: u32) -> Vec<Instance> {
    let mut rng = Rng::new(seed as u64 ^ 0x9e37_79b9);
    points
        .iter()
        .map(|point| {
            let tilt = Quat::from_rotation_arc(Vec3::Y, point.normal.normalize_or(Vec3::Y));
            let rotation = tilt * Quat::from_rotation_y(rng.next_f32() * std::f32::consts::TAU);
            let scale = Vec3::new(
                lerp(blade.min_width, blade.max_width, rng.next_f32()),
                lerp(blade.min_height, blade.max_height, rng.next_f32()),
                1.0,
            );
            let shade = lerp(0.94, 1.06, rng.next_f32());
            Instance::from_parts(point.position, rotation, scale).with_color(Vec3::splat(shade))
        })
        .collect()
}

/// 草叶材质。
pub fn grass_material(style: &GrassStyle, wind: &WindSettings) -> Material {
    let shader = format!("{COMMON}\n{GRASS}");
    let mut material = Material::standard()
        .with_shader(Resource::new_ok(
            "kcomponents/meadow_grass.wgsl",
            kshader::Shader::snippet(&shader),
        ))
        .with_base_color(Vec4::ONE)
        .with_double_sided();
    apply_grass(&mut material, style, wind);
    material
}

/// 草叶材质的全部参数（槽位见 grass.wgsl 开头）。
fn apply_grass(material: &mut Material, style: &GrassStyle, wind: &WindSettings) {
    let (wind_a, wind_b) = wind.params();
    material.set_param(0, srgb(style.bottom).extend(style.brightness));
    material.set_param(1, srgb(style.top).extend(style.backlight_strength));
    material.set_param(2, srgb(style.backlight).extend(style.backlight_power));
    material.set_param(3, wind_a);
    material.set_param(4, wind_b.extend(style.backlight_tip));
    material.set_param(
        5,
        Vec4::new(style.shadow_floor, style.diffuse_floor, style.ambient, 0.0),
    );
}

/// 野花材质里会变的那几个参数（配色在建材质时写一次）。
fn apply_flowers(
    material: &mut Material,
    flowers: &WildflowerSettings,
    style: &GrassStyle,
    wind: &WindSettings,
) {
    let (wind_a, wind_b) = wind.params();
    material.set_param(12, wind_a);
    // 花比草矮、茎硬：摆幅只要草的 45%。
    material.set_param(13, wind_b.extend(0.45));
    material.set_param(
        14,
        Vec4::new(
            flowers.brightness,
            style.shadow_floor,
            style.diffuse_floor,
            style.ambient,
        ),
    );
}

fn flower_texture() -> Texture {
    let (width, height, pixels) = mask::rasterize();
    Texture::new(width, height, pixels)
        .with_format(TextureFormat::Linear)
        .with_sampler(Sampler {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            mipmaps: false,
            anisotropy: 1,
        })
}

/// 野花材质。
pub fn flower_material(
    settings: &WildflowerSettings,
    style: &GrassStyle,
    wind: &WindSettings,
) -> Material {
    let shader = format!("{COMMON}\n{FLOWERS}");
    let mut material = Material::standard()
        .with_shader(Resource::new_ok(
            "kcomponents/meadow_flowers.wgsl",
            kshader::Shader::snippet(&shader),
        ))
        .with_base_color(Vec4::ONE)
        .with_custom_texture(
            0,
            Resource::new_ok("kcomponents/meadow_flower_mask", flower_texture()),
        )
        .with_double_sided();
    for (index, palette) in PALETTES.iter().enumerate() {
        for (part, &color) in palette.iter().enumerate() {
            material.set_param(index * 3 + part, srgb(hex(color)).extend(0.0));
        }
    }
    apply_flowers(&mut material, settings, style, wind);
    material
}

/// 野花卡片的网格：一张竖着的四边形（宽 1、高 1，底边中点在原点），每朵花是它的一个实例。
pub fn flower_card() -> Mesh {
    let mut vertices = Vec::with_capacity(4);
    for (x, t) in [(-0.5f32, 0.0f32), (0.5, 0.0), (0.5, 1.0), (-0.5, 1.0)] {
        // v = 0 是卡片顶端（图集的第 0 行）；u 在顶点钩子里按实例挑的那种花挪到图集的那一格。
        vertices.push(Vertex::new(
            Vec3::new(x, t, 0.0),
            Vec3::Z,
            [x + 0.5, 1.0 - t],
        ));
    }
    Mesh::new(vertices, vec![0, 1, 2, 0, 2, 3]).with_bounds(Aabb::new(
        Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(1.0, 1.0, 1.0),
    ))
}

/// 野花实例：图集里三种花随机挑一种、四套配色随机挑一套（`instance_data` 的 x、y），一点色差（实例颜色）。
pub fn flower_instances(points: &[SurfacePoint], seed: u32) -> Vec<Instance> {
    let mut rng = Rng::new(seed as u64 ^ 0x85eb_ca6b);
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let palette = (rng.next_f32() * PALETTES.len() as f32).floor().min(3.0);
            let variant = ((index as u32 + (rng.next_f32() * mask::VARIANTS as f32) as u32)
                % mask::VARIANTS) as f32;
            // 色差：原版对花瓣做 ±3% 色相、±5% 饱和、±4% 亮度的偏移，这里只留乘法的那份。
            let tint = Vec3::new(
                lerp(0.9, 1.1, rng.next_f32()),
                lerp(0.9, 1.1, rng.next_f32()),
                lerp(0.9, 1.1, rng.next_f32()),
            );
            let height = lerp(0.32, 0.72, rng.next_f32());
            let width = height * lerp(0.45, 0.62, rng.next_f32());
            let tilt = Quat::from_rotation_arc(Vec3::Y, point.normal.normalize_or(Vec3::Y));
            let rotation = tilt * Quat::from_rotation_y(rng.next_f32() * std::f32::consts::TAU);
            Instance::from_parts(point.position, rotation, Vec3::new(width, height, 1.0))
                .with_color(tint)
                .with_data(Vec4::new(variant, palette, 0.0, 0.0))
        })
        .collect()
}

/// 草地组件。
pub struct Meadow {
    settings: MeadowSettings,
    root: Handle<Node>,
    ground: Handle<Node>,
    grass: Vec<Handle<Node>>,
    flowers: Handle<Node>,
    blade_count: usize,
    flower_count: usize,
}

impl Meadow {
    pub fn new(settings: MeadowSettings) -> Self {
        Self {
            settings,
            root: Handle::NONE,
            ground: Handle::NONE,
            grass: Vec::new(),
            flowers: Handle::NONE,
            blade_count: 0,
            flower_count: 0,
        }
    }

    pub fn settings(&self) -> &MeadowSettings {
        &self.settings
    }

    /// 草地的根节点（地、草、花都挂在它下面）。挪它就是挪整块草地。
    pub fn node(&self) -> Handle<Node> {
        self.root
    }

    pub fn blade_count(&self) -> usize {
        self.blade_count
    }

    pub fn flower_count(&self) -> usize {
        self.flower_count
    }

    /// 地面在 (x, z)（草地节点空间）的高度。往草地上放东西用。
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        terrain::height(
            x,
            z,
            self.settings.terrain.seed,
            self.settings.terrain.relief,
        )
    }

    /// 生成地、草、花，挂到场景里。已经生成过的话先删掉旧的。
    pub fn spawn(&mut self, scene: &mut Scene) -> Handle<Node> {
        self.despawn(scene);
        let s = self.settings;
        self.root = scene.add_node(Node::new("Meadow"));
        let ground = Material::standard()
            .with_base_color(Vec4::ONE)
            .with_roughness(0.98);
        self.ground = scene.add_node_with_parent(
            Node::new("MeadowGround")
                .with_mesh(terrain::ground_mesh(&s.terrain))
                .with_material(ground),
            self.root,
        );

        let sampler = SurfaceSampler::new(terrain::grass_triangles(&s.terrain));
        let count = (sampler.area() * s.grass.density).floor() as usize;
        let points = sampler.scatter(count, s.grass.seed as u64, None);
        self.blade_count = points.len();
        let material = grass_material(&s.grass.style, &s.wind);
        // 所有块共用一份叶片网格，每块一个实例化节点（按块剔除）。
        let blade = blade_mesh(&s.grass.blade);
        for (index, tile) in tiles(&points, s.grass.tile_size).into_iter().enumerate() {
            let instances = grass_instances(
                &tile,
                &s.grass.blade,
                s.grass.seed.wrapping_add(index as u32),
            );
            self.grass.push(
                scene.add_node_with_parent(
                    Node::new(format!("MeadowGrass{index}"))
                        .with_mesh(blade.clone())
                        .with_material(material.clone())
                        .with_instances(instances)
                        .with_casts_shadows(s.grass.shadows),
                    self.root,
                ),
            );
        }

        if s.wildflowers.enabled {
            let requested = ((sampler.area() * s.wildflowers.density).round() as usize)
                .min(s.wildflowers.max_count);
            let points = sampler.scatter(requested, s.wildflowers.seed as u64, None);
            self.flower_count = points.len();
            if !points.is_empty() {
                self.flowers = scene.add_node_with_parent(
                    Node::new("MeadowFlowers")
                        .with_mesh(flower_card())
                        .with_instances(flower_instances(&points, s.wildflowers.seed))
                        .with_material(flower_material(&s.wildflowers, &s.grass.style, &s.wind))
                        .with_casts_shadows(s.grass.shadows),
                    self.root,
                );
            }
        }
        self.root
    }

    /// 从场景里删掉。
    pub fn despawn(&mut self, scene: &mut Scene) {
        if self.root.is_some() && scene.try_get(self.root).is_some() {
            scene.remove_node(self.root);
        }
        self.root = Handle::NONE;
        self.ground = Handle::NONE;
        self.grass.clear();
        self.flowers = Handle::NONE;
        self.blade_count = 0;
        self.flower_count = 0;
    }

    /// 改风（草和花一起），不重建。
    pub fn set_wind(&mut self, scene: &mut Scene, wind: WindSettings) {
        self.settings.wind = wind;
        self.write_params(scene);
    }

    /// 改草的颜色和光照手感，不重建。
    pub fn set_style(&mut self, scene: &mut Scene, style: GrassStyle) {
        self.settings.grass.style = style;
        self.write_params(scene);
    }

    fn write_params(&self, scene: &mut Scene) {
        let s = &self.settings;
        for &node in &self.grass {
            if let Some(material) = scene.try_get_mut(node).and_then(Node::material_mut) {
                apply_grass(material, &s.grass.style, &s.wind);
            }
        }
        if self.flowers.is_some()
            && let Some(material) = scene.try_get_mut(self.flowers).and_then(Node::material_mut)
        {
            apply_flowers(material, &s.wildflowers, &s.grass.style, &s.wind);
        }
    }

    /// 改其余设置（地形、密度、叶片形状……）：要重新撒，整块重建。
    pub fn set_settings(&mut self, scene: &mut Scene, settings: MeadowSettings) {
        let spawned = self.root.is_some();
        self.settings = settings;
        if spawned {
            let parent_transform = scene.try_get(self.root).map(|n| n.transform);
            self.spawn(scene);
            if let (Some(transform), Some(node)) = (parent_transform, scene.try_get_mut(self.root))
            {
                node.transform = transform;
            }
        }
    }
}

/// 按 xz 切块（`size` 米一块）。
fn tiles(points: &[SurfacePoint], size: f32) -> Vec<Vec<SurfacePoint>> {
    let size = size.max(1.0);
    let mut map: std::collections::BTreeMap<(i32, i32), Vec<SurfacePoint>> = Default::default();
    for point in points {
        let key = (
            (point.position.x / size).floor() as i32,
            (point.position.z / size).floor() as i32,
        );
        map.entry(key).or_default().push(*point);
    }
    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shaders_compile_against_the_engine() {
        krender::validate_material_hook(&format!("{COMMON}\n{GRASS}")).expect("grass.wgsl 编不过");
        krender::validate_material_hook(&format!("{COMMON}\n{FLOWERS}"))
            .expect("flowers.wgsl 编不过");
    }

    #[test]
    fn a_blade_is_one_mesh_and_every_blade_an_instance() {
        let point = SurfacePoint {
            position: Vec3::new(2.0, 0.5, -3.0),
            normal: Vec3::Y,
        };
        let blade = BladeSettings::default();
        let mesh = blade_mesh(&blade);
        assert_eq!(mesh.vertices().len(), blade.segments as usize * 2 + 1);
        assert_eq!(
            mesh.indices().len(),
            ((blade.segments as usize - 1) * 2 + 1) * 3
        );
        assert_eq!(mesh.vertices().last().unwrap().uv[1], 1.0, "梢在最后");
        let instances = grass_instances(&[point; 3], &blade, 1);
        assert_eq!(instances.len(), 3);
        for instance in &instances {
            // 根在撒的点上，高在范围里。
            let root = instance.transform.transform_point3(Vec3::ZERO);
            assert!(root.distance(point.position) < 1e-5);
            let tip = instance.transform.transform_point3(Vec3::Y);
            let height = tip.y - root.y;
            assert!(
                height >= blade.min_height - 1e-4 && height <= blade.max_height + 1e-4,
                "{height}"
            );
        }
        assert_ne!(instances[0], instances[1], "每根随机转、随机高");
    }

    #[test]
    fn the_default_meadow_has_about_ten_thousand_blades() {
        let settings = MeadowSettings::default();
        let sampler = SurfaceSampler::new(terrain::grass_triangles(&settings.terrain));
        let blades = (sampler.area() * settings.grass.density) as usize;
        assert!((6_000..16_000).contains(&blades), "{blades}");
        let flowers = ((sampler.area() * settings.wildflowers.density).round() as usize).min(240);
        assert_eq!(flowers, 240, "原版那块地野花封顶");
    }

    #[test]
    fn spawning_twice_replaces_the_old_meadow() {
        let mut scene = Scene::new();
        let mut settings = MeadowSettings::default();
        settings.terrain.size = kmath::Vec2::splat(6.0);
        let mut meadow = Meadow::new(settings);
        let first = meadow.spawn(&mut scene);
        let count = scene.descendants(first).len();
        assert!(count >= 3, "地 + 至少一块草 + 花：{count}");
        assert!(meadow.blade_count() > 0);
        let second = meadow.spawn(&mut scene);
        assert!(scene.try_get(first).is_none());
        assert_eq!(scene.descendants(second).len(), count);
    }

    #[test]
    fn hsl_round_trips() {
        let c = Vec3::new(0.3, 0.6, 0.1);
        assert!((hsl_to_rgb(rgb_to_hsl(c)) - c).length() < 1e-5);
        assert!(offset_lightness(c, -0.09).length() < c.length());
    }
}
