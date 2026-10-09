//! 无限海面。
//!
//! ```ignore
//! let mut ocean = Ocean::new(OceanSettings::default(), Quality::High);
//! ocean.spawn(&mut scene);
//! // 每帧：
//! ocean.set_lighting(&sky.lighting());
//! ocean.update(&mut scene, camera_position, dt);
//! let y = ocean.height_at(x, z);
//! ```
//!
//! # 怎么做的
//!
//! - **波浪**：JONSWAP 谱（见 [`spectrum`]）+ 三个级联的 FFT（见 [`simulation`]），CPU 上算，
//!   三个级联三条线程并行。结果编码进一张六层的 RGBA8 纹理数组。
//! - **网格**：以相机为圆心的极坐标网格，圈半径按等比增长——离相机越远越稀，屏幕上的
//!   三角形大小大致不变（连续 LOD），一直铺到几十公里外的天边。网格每帧跟着相机平移，
//!   波浪按世界坐标采样，所以浪不会跟着相机「游」。
//! - **着色**：`ocean.wgsl`。顶点阶段位移，片元阶段逐像素采样坡度和泡沫，屏幕空间折射、
//!   按水深吸收、浅滩偏青、次表面散射、屏幕空间反射、三层泡沫（见 [`FoamLayers`]）、大气透视。
//! - **浮力**：[`Buoyancy`]，CPU 上按同一份 FFT 结果查海面高度——船用多点（俯仰、横摇），
//!   浮标和漂浮物用单点（[`Buoyancy::point`]）。
//! - **尾迹**：[`WakeMap`]，一块跟着走的小波动方程网格。任何物体都能往上发尾迹，
//!   水面跟着起伏、留下白沫，几道尾迹自然叠加干涉；漂浮物也会被别的尾迹推着晃。

mod buoyancy;
pub mod simulation;
pub mod spectrum;
mod wake;

pub use buoyancy::{Buoyancy, BuoyancyProbe};
pub use simulation::{CascadeSettings, FoamSettings};
pub use spectrum::SpectrumParams;
pub use wake::WakeMap;

use crate::quality::Quality;
use kasset::Resource;
use kcore::pool::Handle;
use kmaterial::Material;
use kmath::{Vec2, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use kscene::{Node, Scene};
use ktexture::{FilterMode, Sampler, Texture, TextureFormat, WrapMode};
use simulation::Cascade;

const SHADER: &str = include_str!("ocean.wgsl");
/// 坡度编码范围：|∂h/∂x| 超过它就截断。
const SLOPE_RANGE: f32 = 3.0;

/// 一层泡沫的外观。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoamLayer {
    /// 颜色（线性），再乘 [`WaterLook::foam`]。
    pub color: Vec3,
    /// 覆盖量倍数（0–2）：1 是默认的量，0 关掉这一层。
    pub coverage: f32,
    /// 纹理平铺的边长（米，0.5–32）。
    pub scale: f32,
}

/// 三层互相独立的泡沫，各有自己的纹理（[`Ocean::set_foam_textures`]）、颜色和覆盖量。
///
/// - `whitecap`：浪尖破碎的白浪。只出现在最大那一级浪正在翻卷的浪峰上。
/// - `surface`：白浪过后留在水面上的一层薄沫，慢慢散开、变淡，跟在浪峰后面。
/// - `shore`：海岸线、浅滩拍岸浪、物体周围（船身、礁石）和尾迹的泡沫。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoamLayers {
    pub whitecap: FoamLayer,
    pub surface: FoamLayer,
    pub shore: FoamLayer,
}

impl Default for FoamLayers {
    fn default() -> Self {
        Self {
            whitecap: FoamLayer {
                color: Vec3::new(1.0, 1.0, 1.0),
                coverage: 1.0,
                scale: 7.0,
            },
            surface: FoamLayer {
                color: Vec3::new(0.86, 0.92, 0.94),
                coverage: 1.0,
                scale: 11.0,
            },
            shore: FoamLayer {
                color: Vec3::new(0.97, 0.98, 0.97),
                coverage: 1.0,
                scale: 5.0,
            },
        }
    }
}

/// 水的外观。颜色都是线性空间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterLook {
    /// 每米的吸收系数（红绿蓝）。红光吸收得最快，所以深水偏蓝。
    pub absorption: Vec3,
    /// 水体散射的颜色（深海的「本色」）。
    pub scatter: Vec3,
    /// 浅滩的颜色（沙底反上来的光）。
    pub shallow: Vec3,
    /// 泡沫颜色。
    pub foam: Vec3,
    /// 泡沫总量倍数（0–2）。
    pub foam_intensity: f32,
    /// 水面基础粗糙度（0–1，实际乘 0.25）。
    pub roughness: f32,
    /// 折射扭曲强度（0–1）。
    pub refraction: f32,
    /// 三层泡沫各自的颜色、覆盖量、纹理尺度。
    pub foam_layers: FoamLayers,
    /// 屏幕空间反射的强度（0–1）：船、岛、礁石倒映在水面上；0 只反射天空。
    pub reflections: f32,
}

impl Default for WaterLook {
    fn default() -> Self {
        Self {
            absorption: Vec3::new(0.45, 0.09, 0.06),
            scatter: Vec3::new(0.006, 0.075, 0.16),
            shallow: Vec3::new(0.06, 0.5, 0.45),
            foam: Vec3::new(0.9, 0.93, 0.95),
            foam_intensity: 0.6,
            roughness: 0.25,
            refraction: 0.5,
            foam_layers: FoamLayers::default(),
            reflections: 1.0,
        }
    }
}

/// 海洋的全部设置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OceanSettings {
    pub spectrum: SpectrumParams,
    /// 尖浪：水平位移倍数。0 是圆滚滚的正弦浪，1.5 左右浪尖已经很尖。
    pub choppiness: f32,
    pub foam: FoamSettings,
    pub look: WaterLook,
    /// 静水面高度。
    pub level: f32,
    /// 时间流速（1 = 实时）。
    pub time_scale: f32,
    /// 随机种子：同一个种子同一片海。
    pub seed: u64,
}

impl Default for OceanSettings {
    fn default() -> Self {
        Self {
            spectrum: SpectrumParams::default(),
            choppiness: 1.2,
            foam: FoamSettings::default(),
            look: WaterLook::default(),
            level: 0.0,
            time_scale: 1.0,
            seed: 20_250_101,
        }
    }
}

/// 天光：海面要知道太阳和天空有多亮，才能把水体散射算对。由 [`crate::sky::Sky::lighting`] 给。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneLighting {
    /// 指向太阳的单位向量。
    pub sun_direction: Vec3,
    /// 太阳到达海面的辐射（线性，已乘强度）。
    pub sun_radiance: Vec3,
    /// 天空的平均辐射（环境光）。
    pub sky_radiance: Vec3,
    /// 地平线附近的颜色：远处的海融进去的那个颜色。
    pub haze_color: Vec3,
    /// 雾的浓度（0–1）。
    pub haze_density: f32,
}

impl Default for SceneLighting {
    fn default() -> Self {
        Self {
            sun_direction: Vec3::new(0.3, 0.6, 0.2).normalize(),
            sun_radiance: Vec3::splat(3.0),
            sky_radiance: Vec3::new(0.3, 0.45, 0.7),
            haze_color: Vec3::new(0.6, 0.7, 0.85),
            haze_density: 0.3,
        }
    }
}

/// 水深图：一块以 `center` 为中心、边长 `extent` 米的方形区域里每格的水深（米，水面到底）。
///
/// 海面据此在浅水里压低浪高（浪跑到岸边会变矮、碎掉），并在浅滩上画朝岸边推进的拍岸浪。
/// 区域外当作深海。CPU 上查高度（浮力）也按它衰减，看到的浪和推船的浪是同一个。
#[derive(Debug, Clone)]
pub struct DepthMap {
    pub center: Vec2,
    pub extent: f32,
    pub size: usize,
    /// 行主序 `[z * size + x]`，`x` 沿世界 +X、`z` 沿世界 +Z。
    pub depth: Vec<f32>,
}

impl DepthMap {
    /// 某点的水深；区域外返回一个很大的数（深海）。
    pub fn depth_at(&self, x: f32, z: f32) -> f32 {
        let u = (x - self.center.x) / self.extent + 0.5;
        let v = (z - self.center.y) / self.extent + 0.5;
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
            return 1000.0;
        }
        let n = self.size;
        let fx = u * n as f32 - 0.5;
        let fz = v * n as f32 - 0.5;
        let (x0, z0) = (
            fx.floor().clamp(0.0, (n - 1) as f32) as usize,
            fz.floor().clamp(0.0, (n - 1) as f32) as usize,
        );
        let (x1, z1) = ((x0 + 1).min(n - 1), (z0 + 1).min(n - 1));
        let (tx, tz) = (
            (fx - x0 as f32).clamp(0.0, 1.0),
            (fz - z0 as f32).clamp(0.0, 1.0),
        );
        let at = |x: usize, z: usize| self.depth[z * n + x];
        let top = at(x0, z0) * (1.0 - tx) + at(x1, z0) * tx;
        let bottom = at(x0, z1) * (1.0 - tx) + at(x1, z1) * tx;
        top * (1.0 - tz) + bottom * tz
    }
}

/// 浅水里浪高打几折：水深 0.5 米以内只剩两成，9 米以上不衰减。和 `ocean.wgsl` 里的 `ocean_shoal` 一致。
pub fn shoal_factor(depth: f32) -> f32 {
    let t = ((depth - 0.5) / 8.5).clamp(0.0, 1.0);
    0.2 + 0.8 * t * t * (3.0 - 2.0 * t)
}

/// 海面某一点的状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterSample {
    /// 水面高度（世界 y）。
    pub height: f32,
    /// 水面法线。
    pub normal: Vec3,
    /// 这一点水面的速度（米/秒）：浪里的水是在绕圈的，浪峰处往前、浪谷处往后。
    pub velocity: Vec3,
}

/// 无限海面。
pub struct Ocean {
    settings: OceanSettings,
    quality: Quality,
    cascades: Vec<Cascade>,
    time: f32,
    lighting: SceneLighting,
    /// 六层的波浪纹理。每帧换像素、同一个 id（渲染器原地更新显存）。
    waves: Texture,
    palette: Texture,
    wake: WakeMap,
    wake_enabled: bool,
    node: Handle<Node>,
    material: Material,
    /// 设置改了，下一帧重建初始频谱。
    respectrum: bool,
    /// 泡沫贴图（灰度，平铺）。没有时用程序化噪声。
    foam_texture: bool,
    depth_map: Option<DepthMap>,
    /// 相机在不在水下（整帧一个答案：海面从下面看还是从上面看）。
    camera_submerged: bool,
}

impl Ocean {
    pub fn new(settings: OceanSettings, quality: Quality) -> Self {
        let cascades = build_cascades(&settings, quality);
        let n = quality.fft_size();
        let waves = Texture::array(n as u32, n as u32, 6, vec![128; n * n * 6 * 4])
            .with_format(TextureFormat::Linear)
            .with_sampler(repeat_linear());
        let palette = Texture::new(16, 1, vec![0; 16 * 4])
            .with_format(TextureFormat::Linear)
            .with_sampler(repeat_linear());
        // 256 格 × 320 米：一格 1.25 米，船宽 6 米压出来的尾浪看得出形状。改了尺寸要同步 ocean.wgsl 里的 WAKE_TEXELS。
        let wake = WakeMap::new(256, 320.0);
        let material = build_material(&waves, &palette, wake.texture());
        let mut ocean = Self {
            settings,
            quality,
            cascades,
            time: 0.0,
            lighting: SceneLighting::default(),
            waves,
            palette,
            wake,
            wake_enabled: true,
            node: Handle::NONE,
            material,
            respectrum: false,
            foam_texture: false,
            depth_map: None,
            camera_submerged: false,
        };
        ocean.write_palette();
        ocean
    }

    /// 往场景里放海面网格。
    pub fn spawn(&mut self, scene: &mut Scene) -> Handle<Node> {
        let (segments, growth) = self.quality.ocean_grid();
        let mesh = radial_grid(segments, growth, 0.35, 40_000.0);
        self.node = scene.add_node(
            Node::new("Ocean")
                .with_mesh(mesh)
                .with_material(self.material.clone())
                .with_casts_shadows(false)
                .with_position(Vec3::new(0.0, self.settings.level, 0.0)),
        );
        self.node
    }

    pub fn node(&self) -> Handle<Node> {
        self.node
    }

    pub fn settings(&self) -> &OceanSettings {
        &self.settings
    }

    /// 改设置。谱参数变了会在下一帧重建初始频谱（几毫秒），别每帧都改谱。
    pub fn set_settings(&mut self, settings: OceanSettings) {
        if settings.spectrum != self.settings.spectrum || settings.seed != self.settings.seed {
            self.respectrum = true;
        }
        self.settings = settings;
        self.write_palette();
    }

    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// 换画质：FFT 尺寸或网格密度变了就重建。
    pub fn set_quality(&mut self, scene: &mut Scene, quality: Quality) {
        if quality == self.quality {
            return;
        }
        let rebuild_fft = quality.fft_size() != self.quality.fft_size();
        self.quality = quality;
        if rebuild_fft {
            self.cascades = build_cascades(&self.settings, quality);
            let n = quality.fft_size();
            // 尺寸变了渲染器原地更新不了，换一张新纹理（新 id）。
            self.waves = Texture::array(n as u32, n as u32, 6, vec![128; n * n * 6 * 4])
                .with_format(TextureFormat::Linear)
                .with_sampler(repeat_linear());
        }
        if let Some(node) = scene.try_get_mut(self.node) {
            let (segments, growth) = quality.ocean_grid();
            node.set_mesh(radial_grid(segments, growth, 0.35, 40_000.0));
        }
    }

    /// 天光。每帧（或天空变了时）调。
    pub fn set_lighting(&mut self, lighting: &SceneLighting) {
        if *lighting != self.lighting {
            self.lighting = *lighting;
            self.write_palette();
        }
    }

    /// 水深图（见 [`DepthMap`]）。`None` 当作处处是深海。
    pub fn set_depth_map(&mut self, map: Option<DepthMap>) {
        // 编进一张 RGBA8：r = 水深 / 32 米。放在材质的法线贴图槽里（海面不用法线贴图）。
        if let Some(map) = &map {
            let data: Vec<u8> = map
                .depth
                .iter()
                .flat_map(|d| {
                    [
                        ((d / 32.0).clamp(0.0, 1.0) * 255.0).round() as u8,
                        0,
                        0,
                        255,
                    ]
                })
                .collect();
            let texture = Texture::new(map.size as u32, map.size as u32, data)
                .with_format(TextureFormat::Linear)
                .with_sampler(Sampler {
                    mag_filter: FilterMode::Linear,
                    min_filter: FilterMode::Linear,
                    wrap_u: WrapMode::ClampToEdge,
                    wrap_v: WrapMode::ClampToEdge,
                    ..Default::default()
                });
            self.material.set(
                kpbr::standard::NORMAL_TEXTURE,
                Resource::new_ok("ocean depth", texture),
            );
        }
        self.depth_map = map;
        self.write_palette();
    }

    /// 某点的水深（米）；没有水深图或在图外时是 1000（深海）。
    pub fn depth_at(&self, x: f32, z: f32) -> f32 {
        self.depth_map.as_ref().map_or(1000.0, |m| m.depth_at(x, z))
    }

    /// 三层泡沫用同一张纹理（灰度，会平铺）。不给的话用程序化噪声。
    pub fn set_foam_texture(&mut self, texture: Texture) {
        self.set_foam_textures(texture.clone(), texture.clone(), texture);
    }

    /// 三层泡沫（见 [`FoamLayers`]）各自的纹理：灰度、会平铺，亮处是泡沫。
    ///
    /// 白浪放基础色贴图槽（它的采样器也是海面所有贴图共用的那个），另两层放自定义贴图槽 2、3。
    pub fn set_foam_textures(&mut self, whitecap: Texture, surface: Texture, shore: Texture) {
        let prepare = |t: Texture| {
            t.with_format(TextureFormat::Linear)
                .with_sampler(repeat_linear())
        };
        self.material.set(
            kmaterial::standard::BASE_COLOR_TEXTURE,
            Resource::new_ok("ocean foam whitecap", prepare(whitecap)),
        );
        self.material
            .set_custom_texture(2, Resource::new_ok("ocean foam surface", prepare(surface)));
        self.material
            .set_custom_texture(3, Resource::new_ok("ocean foam shore", prepare(shore)));
        self.foam_texture = true;
    }

    /// 尾迹图：往上画白沫用。
    pub fn wake_mut(&mut self) -> &mut WakeMap {
        &mut self.wake
    }

    /// 开关尾迹泡沫。
    pub fn set_wake_enabled(&mut self, enabled: bool) {
        self.wake_enabled = enabled;
    }

    /// 模拟时间（秒）。
    pub fn time(&self) -> f32 {
        self.time
    }

    /// 推进一帧：FFT、编码纹理、网格跟到相机下面、更新材质。
    pub fn update(&mut self, scene: &mut Scene, camera_position: Vec3, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        self.time += dt * self.settings.time_scale;
        if self.respectrum {
            self.respectrum = false;
            for (index, cascade) in self.cascades.iter_mut().enumerate() {
                cascade.respectrum(
                    &self.settings.spectrum,
                    self.settings.seed + index as u64 * 7919,
                );
            }
        }

        // 三个级联三条线程。每个级联一帧做四次逆 FFT。
        let (time, choppiness, foam) = (self.time, self.settings.choppiness, self.settings.foam);
        let step = dt * self.settings.time_scale;
        std::thread::scope(|threads| {
            for cascade in &mut self.cascades {
                threads.spawn(move || cascade.update(time, step, choppiness, foam));
            }
        });

        self.waves = self.waves.with_pixels(encode_waves(&self.cascades));
        self.wake.update(dt);
        let submerged = camera_position.y < self.height_at(camera_position.x, camera_position.z);
        if submerged != self.camera_submerged {
            self.camera_submerged = submerged;
            self.write_palette();
        }

        let level = self.settings.level;
        let n = self.quality.fft_size() as f32;
        let lengths = Vec4::new(
            self.cascades[0].settings.length,
            self.cascades[1].settings.length,
            self.cascades[2].settings.length,
            0.0,
        );
        let ranges = Vec4::new(
            self.cascades[0].maps.displacement_range,
            self.cascades[1].maps.displacement_range,
            self.cascades[2].maps.displacement_range,
            SLOPE_RANGE,
        );
        let wake_center = self.wake.center();
        let wake = Vec4::new(
            wake_center.x,
            wake_center.y,
            self.wake.extent(),
            if self.wake_enabled { 1.0 } else { 0.0 },
        );

        self.material.set_param(
            0,
            Vec4::new(
                camera_position.x,
                camera_position.z,
                n,
                if self.foam_texture { 1.0 } else { 0.0 },
            ),
        );
        // params[1].w：水深图的边长（0 = 没有）。
        let lengths = Vec4::new(
            lengths.x,
            lengths.y,
            lengths.z,
            self.depth_map.as_ref().map_or(0.0, |m| m.extent),
        );
        self.material.set_param(1, lengths);
        self.material.set_param(2, ranges);
        self.material.set_param(3, wake);
        self.material
            .set_texture_array(Resource::new_ok("ocean waves", self.waves.clone()));
        self.material.set(
            "custom_texture0",
            Resource::new_ok("ocean palette", self.palette.clone()),
        );
        self.material.set(
            "custom_texture1",
            Resource::new_ok("ocean wake", self.wake.texture().clone()),
        );

        if let Some(node) = scene.try_get_mut(self.node) {
            node.transform.position = Vec3::new(camera_position.x, level, camera_position.z);
            node.set_material(self.material.clone());
        }
    }

    /// 某一点的水面高度（世界 y）。考虑了水平位移：浪尖被推到哪儿，高度就在哪儿。
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        self.sample(x, z).height
    }

    /// 某一点的水面高度和法线。
    pub fn sample(&self, x: f32, z: f32) -> WaterSample {
        // 纹理按「推之前」的坐标存：找 p₀ 使 p₀ + D(p₀) = p。不动点迭代三次就收敛到厘米级。
        let target = Vec2::new(x, z);
        let mut p = target;
        let shoal = shoal_factor(self.depth_at(x, z));
        for _ in 0..3 {
            let d = self.displacement(p) * shoal;
            p = target - Vec2::new(d.x, d.z);
        }
        // 浅水里浪变矮：位移、速度都打折。
        let shoal = shoal_factor(self.depth_at(x, z));
        let d = self.displacement(p) * shoal;
        // 同一个水质点（同一个 p₀）上一步在哪：位移之差 / 时间就是它的速度。
        let mut previous = Vec3::ZERO;
        let mut step = 0.0f32;
        for cascade in &self.cascades {
            let maps = &cascade.maps;
            previous.x += cascade.sample(&maps.prev_dx, p.x, p.y);
            previous.y += cascade.sample(&maps.prev_height, p.x, p.y);
            previous.z += cascade.sample(&maps.prev_dz, p.x, p.y);
            step = step.max(maps.step);
        }
        let velocity = if step > 1e-5 {
            (d - previous * shoal) / step
        } else {
            Vec3::ZERO
        };
        let mut slope = Vec2::ZERO;
        for cascade in &self.cascades {
            slope.x += cascade.sample(&cascade.maps.slope_x, p.x, p.y);
            slope.y += cascade.sample(&cascade.maps.slope_z, p.x, p.y);
        }
        // 尾迹（船划过、浮标晃出来的波纹）叠在 FFT 浪上：别的东西的尾迹也会推着漂浮物晃。
        let wake = if self.wake_enabled {
            self.wake.height_at(x, z)
        } else {
            0.0
        };
        WaterSample {
            height: self.settings.level + d.y + wake,
            normal: Vec3::new(-slope.x, 1.0, -slope.y).normalize(),
            // 夹一下：时间刚跳过一大步（换设置、重建频谱）时差分会出一个荒唐的值。
            velocity: velocity.clamp_length_max(20.0),
        }
    }

    fn displacement(&self, p: Vec2) -> Vec3 {
        let mut d = Vec3::ZERO;
        for cascade in &self.cascades {
            d.x += cascade.sample(&cascade.maps.dx, p.x, p.y);
            d.y += cascade.sample(&cascade.maps.height, p.x, p.y);
            d.z += cascade.sample(&cascade.maps.dz, p.x, p.y);
        }
        d
    }

    /// 有效波高的粗估（米）：最大位移范围之和。界面上显示用。
    pub fn wave_scale(&self) -> f32 {
        self.cascades
            .iter()
            .map(|c| c.maps.displacement_range)
            .sum()
    }

    fn write_palette(&mut self) {
        let look = &self.settings.look;
        let light = &self.lighting;
        let rgba = |v: Vec3, a: f32| {
            [
                (v.x.clamp(0.0, 1.0) * 255.0).round() as u8,
                (v.y.clamp(0.0, 1.0) * 255.0).round() as u8,
                (v.z.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        };
        let center = self.depth_map.as_ref().map_or(Vec2::ZERO, |m| m.center);
        let encode16 = |v: f32| ((v + 32768.0).round().clamp(0.0, 65535.0)) as u32;
        let (cx, cz) = (encode16(center.x), encode16(center.y));
        let wind = self.settings.spectrum.wind_direction;
        let layers = &look.foam_layers;
        let layer = |l: &FoamLayer| rgba(l.color, l.coverage / 2.0);
        let scale = |l: &FoamLayer| ((l.scale / 32.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        let texels: [[u8; 4]; 16] = [
            rgba(look.absorption / 0.6, 1.0),
            rgba(look.scatter, 1.0),
            rgba(look.shallow, 1.0),
            rgba(look.foam, 1.0),
            rgba(light.haze_color / 8.0, 1.0),
            rgba(light.sun_radiance / 8.0, 1.0),
            rgba(light.sky_radiance / 4.0, 1.0),
            rgba(
                light.sun_direction * 0.5 + Vec3::splat(0.5),
                light.haze_density,
            ),
            [
                (look.foam_intensity / 2.0 * 255.0).clamp(0.0, 255.0) as u8,
                (look.roughness * 255.0).clamp(0.0, 255.0) as u8,
                (look.refraction * 255.0).clamp(0.0, 255.0) as u8,
                255,
            ],
            [
                (cx >> 8) as u8,
                (cx & 0xff) as u8,
                (cz >> 8) as u8,
                (cz & 0xff) as u8,
            ],
            rgba(
                Vec3::new(wind.cos() * 0.5 + 0.5, wind.sin() * 0.5 + 0.5, 0.0),
                1.0,
            ),
            // 11–13：三层泡沫的颜色 + 覆盖量 / 2；14：三层的纹理尺度 / 32 米 + 反射强度。
            layer(&layers.whitecap),
            layer(&layers.surface),
            layer(&layers.shore),
            [
                scale(&layers.whitecap),
                scale(&layers.surface),
                scale(&layers.shore),
                (look.reflections.clamp(0.0, 1.0) * 255.0).round() as u8,
            ],
            // 15：相机在水下（r = 255）。
            [if self.camera_submerged { 255 } else { 0 }, 0, 0, 255],
        ];
        let mut data = vec![0u8; 16 * 4];
        for (index, texel) in texels.iter().enumerate() {
            data[index * 4..index * 4 + 4].copy_from_slice(texel);
        }
        self.palette = self.palette.with_pixels(data);
    }
}

fn repeat_linear() -> Sampler {
    // 不要 mip：波浪、调色板都按第 0 级采（textureSampleLevel），每帧重传时省得在 CPU 上重算 mip 链。
    Sampler {
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        wrap_u: WrapMode::Repeat,
        wrap_v: WrapMode::Repeat,
        mipmaps: false,
        anisotropy: 1,
    }
}

fn build_cascades(settings: &OceanSettings, quality: Quality) -> Vec<Cascade> {
    simulation::default_cascades()
        .into_iter()
        .enumerate()
        .map(|(index, cascade)| {
            Cascade::new(
                cascade,
                quality.fft_size(),
                &settings.spectrum,
                settings.seed + index as u64 * 7919,
            )
        })
        .collect()
}

fn build_material(waves: &Texture, palette: &Texture, wake: &Texture) -> Material {
    let mut material = Material::standard()
        .with_shader(Resource::new_ok(
            "kcomponents/ocean.wgsl",
            kshader::Shader::snippet(SHADER),
        ))
        .with_double_sided();
    material.set_blend_mode(kmaterial::BlendMode::Alpha);
    // 自定义贴图用基础色贴图的采样器：给一张 1×1 的白图，采样器设成线性 + 平铺。
    let white = Texture::new(1, 1, vec![255; 4])
        .with_format(TextureFormat::Linear)
        .with_sampler(repeat_linear());
    material.set(
        kmaterial::standard::BASE_COLOR_TEXTURE,
        Resource::new_ok("ocean sampler", white),
    );
    material.set_texture_array(Resource::new_ok("ocean waves", waves.clone()));
    material.set(
        "custom_texture0",
        Resource::new_ok("ocean palette", palette.clone()),
    );
    material.set(
        "custom_texture1",
        Resource::new_ok("ocean wake", wake.clone()),
    );
    material
}

/// 六层 RGBA8：每个级联「位移」和「坡度 + 泡沫」两层。
fn encode_waves(cascades: &[Cascade]) -> Vec<u8> {
    let n = cascades[0].size();
    let layer = n * n * 4;
    let mut data = vec![0u8; layer * 6];
    let unorm = |v: f32| ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
    for (index, cascade) in cascades.iter().enumerate() {
        let maps = &cascade.maps;
        let range = maps.displacement_range;
        let (displacement, rest) = data[index * 2 * layer..].split_at_mut(layer);
        let slopes = &mut rest[..layer];
        for i in 0..n * n {
            let h = ((maps.height[i] / range * 0.5 + 0.5).clamp(0.0, 1.0) * 65535.0).round() as u32;
            displacement[i * 4] = (h >> 8) as u8;
            displacement[i * 4 + 1] = (h & 0xff) as u8;
            displacement[i * 4 + 2] = unorm(maps.dx[i] / range);
            displacement[i * 4 + 3] = unorm(maps.dz[i] / range);
            slopes[i * 4] = unorm(maps.slope_x[i] / SLOPE_RANGE);
            slopes[i * 4 + 1] = unorm(maps.slope_z[i] / SLOPE_RANGE);
            slopes[i * 4 + 2] = (maps.foam[i].clamp(0.0, 1.0) * 255.0).round() as u8;
            slopes[i * 4 + 3] = 255;
        }
    }
    data
}

/// 以原点为圆心的极坐标网格：中心一个小扇形，外面一圈圈等比放大到 `outer` 米。
///
/// 相邻两圈的间距 ≈ 半径 × `growth`，所以离相机越远越稀、屏幕上的三角形大小大致不变。
pub fn radial_grid(segments: usize, growth: f32, inner: f32, outer: f32) -> Mesh {
    let rings = ((outer / inner).ln() / (1.0 + growth).ln()).ceil() as usize;
    let mut vertices = Vec::with_capacity(1 + segments * (rings + 1));
    let mut indices = Vec::with_capacity(segments * 3 + rings * segments * 6);
    let up = Vec3::Y;
    vertices.push(Vertex::new(Vec3::ZERO, up, [0.0, 0.0]));
    for ring in 0..=rings {
        let radius = inner * (1.0 + growth).powi(ring as i32);
        for segment in 0..segments {
            let angle = segment as f32 / segments as f32 * std::f32::consts::TAU;
            let (s, c) = angle.sin_cos();
            vertices.push(Vertex::new(
                Vec3::new(c * radius, 0.0, s * radius),
                up,
                [0.0, 0.0],
            ));
        }
    }
    let at = |ring: usize, segment: usize| (1 + ring * segments + segment % segments) as u32;
    // 中心扇形。逆时针（从上往下看）朝上。
    for segment in 0..segments {
        indices.extend_from_slice(&[0, at(0, segment + 1), at(0, segment)]);
    }
    for ring in 0..rings {
        for segment in 0..segments {
            let (a, b, c, d) = (
                at(ring, segment),
                at(ring, segment + 1),
                at(ring + 1, segment),
                at(ring + 1, segment + 1),
            );
            indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }
    Mesh::new(vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_radial_grid_reaches_the_horizon_and_faces_up() {
        let mesh = radial_grid(64, 0.1, 0.5, 10_000.0);
        let far = mesh
            .vertices()
            .iter()
            .map(|v| Vec3::from(v.position).length())
            .fold(0.0, f32::max);
        assert!(far >= 10_000.0);
        // 第一个三角形的法线朝上。
        let i = mesh.indices();
        let p = |k: usize| Vec3::from(mesh.vertices()[i[k] as usize].position);
        let normal = (p(1) - p(0)).cross(p(2) - p(0));
        assert!(normal.y > 0.0, "{normal:?}");
    }

    #[test]
    fn cpu_height_query_follows_the_simulation() {
        let mut ocean = Ocean::new(OceanSettings::default(), Quality::Low);
        let mut scene = Scene::new();
        ocean.spawn(&mut scene);
        ocean.update(&mut scene, Vec3::ZERO, 0.016);
        let heights: Vec<f32> = (0..50)
            .map(|i| ocean.height_at(i as f32 * 3.1, i as f32 * -1.7))
            .collect();
        let spread = heights.iter().cloned().fold(f32::MIN, f32::max)
            - heights.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread > 0.1, "海面应该有起伏：{spread}");
        assert!(heights.iter().all(|h| h.abs() < 20.0));
    }

    #[test]
    fn waves_shrink_over_shallow_water() {
        let mut ocean = Ocean::new(OceanSettings::default(), Quality::Low);
        let mut scene = Scene::new();
        ocean.spawn(&mut scene);
        // 左半边 0.3 米深的浅滩，右半边深海。
        let size = 32;
        let depth = (0..size * size)
            .map(|i| if i % size < size / 2 { 0.3 } else { 50.0 })
            .collect();
        ocean.set_depth_map(Some(DepthMap {
            center: Vec2::ZERO,
            extent: 200.0,
            size,
            depth,
        }));
        let mut shallow = Vec::new();
        let mut deep = Vec::new();
        for frame in 0..60 {
            ocean.update(&mut scene, Vec3::ZERO, 0.05);
            if frame % 3 == 0 {
                shallow.push(ocean.height_at(-60.0, 10.0));
                deep.push(ocean.height_at(60.0, 10.0));
            }
        }
        let spread = |v: &[f32]| {
            v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min)
        };
        assert!(
            spread(&shallow) < spread(&deep) * 0.5,
            "浅水的浪该矮得多：{} vs {}",
            spread(&shallow),
            spread(&deep)
        );
    }

    #[test]
    fn the_shader_compiles_against_the_engine() {
        krender::validate_material_hook(SHADER).expect("ocean.wgsl 编不过");
    }
}
