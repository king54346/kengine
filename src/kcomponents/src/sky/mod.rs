//! 程序化天空：瑞利 / 米氏散射大气、日盘、体积云、夜晚的星星。
//!
//! ```ignore
//! let mut sky = Sky::new(SkySettings::default(), Quality::High);
//! sky.spawn(&mut scene);
//! // 每帧：
//! sky.update(&mut scene, camera_position, dt);
//! ocean.set_lighting(&sky.lighting());
//! ```
//!
//! 一套大气模型三处用：
//!
//! - **天穹**：一个跟着相机走的大球，`sky.wgsl` 逐像素算大气、日盘、体积云。
//! - **环境光**：同一个模型在 CPU 上烘成一张 HDR 全景图交给场景当 IBL——船身、岛、
//!   海面的反射里看到的都是这片天。设置变了才重烘（有防抖，拖滑条时不卡）。
//! - **太阳光**：一盏方向光，颜色 = 太阳穿过大气后的透射率（日落时自然变红）。
//!   太阳落下去之后换成一盏暗蓝的「月光」。

pub mod atmosphere;

use crate::ocean::SceneLighting;
use crate::quality::Quality;
use atmosphere::AtmosphereParams;
use kasset::Resource;
use kcore::pool::Handle;
use klight::Light;
use kmaterial::Material;
use kmath::{Vec3, Vec4};
use kmesh::Mesh;
use kscene::{Node, Scene, Transform};

const SHADER: &str = include_str!("sky.wgsl");
const PANORAMA_SHADER: &str = include_str!("panorama.wgsl");

/// 一张全景天空照片（等距柱状投影），以及从它算出来的光照。
///
/// 照片是 LDR 的（太阳早就过曝成白色了），所以太阳光不从照片里取亮度，
/// 只取**方向**——找最亮的那一块——亮度和颜色由 [`Panorama::sun_color`] / `sun_intensity` 给。
#[derive(Clone)]
pub struct Panorama {
    /// 原图（sRGB），贴在天穹上。
    pub texture: ktexture::Texture,
    /// 缩小过的线性版本，烘环境光用。
    pub small: kpbr::hdr::HdrImage,
    /// 照片里太阳的方向（已经套过 `yaw`）。
    pub sun_direction: Vec3,
    /// 太阳光的颜色。
    pub sun_color: Vec3,
    /// 太阳光的强度。
    pub sun_intensity: f32,
    /// 天空亮度倍数。
    pub intensity: f32,
    /// 绕竖轴旋转（弧度）：把照片里的太阳转到想要的方位。
    pub yaw: f32,
}

impl Panorama {
    /// 从一张已经解码的全景图建。`intensity` 是天空亮度倍数（照片是 0–1 的 LDR，1–2 比较合适）。
    pub fn new(texture: ktexture::Texture, intensity: f32, yaw: f32) -> Self {
        let texture = texture
            .with_format(ktexture::TextureFormat::Srgb)
            .with_sampler(ktexture::Sampler {
                mag_filter: ktexture::FilterMode::Linear,
                min_filter: ktexture::FilterMode::Linear,
                wrap_u: ktexture::WrapMode::Repeat,
                wrap_v: ktexture::WrapMode::ClampToEdge,
                ..Default::default()
            });
        // 盒式滤波缩到 256×128：环境光的预滤波用不着更大的图。
        let (width, height) = (texture.width() as usize, texture.height() as usize);
        let (small_w, small_h) = (256usize, 128usize);
        let data = texture.data();
        let decode = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let lut: Vec<f32> = (0..=255u8).map(decode).collect();
        let mut pixels = vec![0.0f32; small_w * small_h * 3];
        let (step_x, step_y) = ((width / small_w).max(1), (height / small_h).max(1));
        let mut brightest = (0.0f32, 0usize, 0usize);
        for sy in 0..small_h {
            for sx in 0..small_w {
                let mut sum = [0.0f32; 3];
                let mut count = 0.0;
                // 每格只抽 4×4 个点：4K 图逐像素平均要几亿次运算，抽样足够了。
                for oy in 0..4 {
                    for ox in 0..4 {
                        let x = (sx * step_x + ox * step_x / 4).min(width - 1);
                        let y = (sy * step_y + oy * step_y / 4).min(height - 1);
                        let i = (y * width + x) * 4;
                        for c in 0..3 {
                            sum[c] += lut[data[i + c] as usize];
                        }
                        count += 1.0;
                    }
                }
                let o = (sy * small_w + sx) * 3;
                for c in 0..3 {
                    pixels[o + c] = sum[c] / count;
                }
                let luminance =
                    0.2126 * pixels[o] + 0.7152 * pixels[o + 1] + 0.0722 * pixels[o + 2];
                // 只在上半球里找太阳。
                if sy < small_h / 2 && luminance > brightest.0 {
                    brightest = (luminance, sx, sy);
                }
            }
        }
        let small = kpbr::hdr::HdrImage::from_pixels(small_w, small_h, pixels);
        // 像素 → 方向（sample_direction 的逆），再按 yaw 转回世界。
        let u = (brightest.1 as f32 + 0.5) / small_w as f32;
        let v = (brightest.2 as f32 + 0.5) / small_h as f32;
        let phi = u * std::f32::consts::TAU - std::f32::consts::PI;
        let theta = v * std::f32::consts::PI;
        let image_dir = Vec3::new(
            theta.sin() * phi.cos(),
            theta.cos(),
            theta.sin() * phi.sin(),
        );
        let (s, c) = (-yaw).sin_cos();
        let sun_direction = Vec3::new(
            c * image_dir.x - s * image_dir.z,
            image_dir.y.max(0.08),
            s * image_dir.x + c * image_dir.z,
        )
        .normalize();
        Self {
            texture,
            small,
            sun_direction,
            sun_color: Vec3::new(1.0, 0.95, 0.88),
            sun_intensity: 3.2,
            intensity,
            yaw,
        }
    }

    /// 世界方向上的天空亮度（线性，已乘亮度倍数）。
    pub fn radiance(&self, direction: Vec3) -> Vec3 {
        let (s, c) = self.yaw.sin_cos();
        let d = direction.normalize_or(Vec3::Y);
        let rotated = Vec3::new(c * d.x - s * d.z, d.y.max(0.004), s * d.x + c * d.z);
        self.small.sample_direction(rotated) * self.intensity
    }
}
/// 天穹半径（米）。要比海面网格的半径大、比相机远平面小。
pub const DOME_RADIUS: f32 = 30_000.0;

/// 天空设置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkySettings {
    /// 一天中的时刻（小时，0–24）。太阳按一条简化的日周轨迹走。
    pub time_of_day: f32,
    /// 太阳在正午时的最高仰角（度）。纬度越高越低。
    pub max_elevation: f32,
    /// 太阳轨迹的朝向（弧度）：日出的方位角。
    pub sun_azimuth: f32,
    /// 太阳亮度。
    pub sun_intensity: f32,
    /// 瑞利散射倍数（1 = 地球；大一点天更蓝更深）。
    pub rayleigh: f32,
    /// 浑浊度（米氏散射倍数，1 = 晴，5 = 雾霾）。
    pub turbidity: f32,
    /// 云量（0–1）。
    pub cloud_coverage: f32,
    /// 云的浓度。
    pub cloud_density: f32,
    /// 云飘的速度（米/秒）。
    pub cloud_speed: f32,
    /// 云飘的方向（弧度）。
    pub cloud_direction: f32,
    /// 日盘角半径（度），真实太阳约 0.27。画得大一点好看。
    pub sun_disc: f32,
    /// 夜光（月光 + 夜空底色）的亮度。
    pub night_light: f32,
    /// 地平线雾的浓度（0–1），交给海面做大气透视。
    pub haze: f32,
}

impl Default for SkySettings {
    fn default() -> Self {
        Self {
            time_of_day: 15.0,
            max_elevation: 62.0,
            sun_azimuth: 0.6,
            sun_intensity: 20.0,
            rayleigh: 1.0,
            turbidity: 1.4,
            cloud_coverage: 0.37,
            cloud_density: 1.0,
            cloud_speed: 30.0,
            cloud_direction: 0.4,
            sun_disc: 0.6,
            night_light: 1.0,
            haze: 0.3,
        }
    }
}

impl SkySettings {
    /// 按时刻算指向太阳的方向。6 点日出、18 点日落，中间按正弦升降。
    pub fn sun_direction(&self) -> Vec3 {
        let day = ((self.time_of_day - 6.0) / 12.0) * std::f32::consts::PI;
        let elevation = day.sin() * self.max_elevation.to_radians();
        // 方位角从日出的方向转半圈到日落的方向。
        let azimuth = self.sun_azimuth + day;
        Vec3::new(
            azimuth.cos() * elevation.cos(),
            elevation.sin(),
            azimuth.sin() * elevation.cos(),
        )
        .normalize()
    }

    fn atmosphere(&self) -> AtmosphereParams {
        AtmosphereParams {
            sun_direction: self.sun_direction(),
            sun_intensity: self.sun_intensity,
            rayleigh: self.rayleigh,
            mie: self.turbidity,
        }
    }

    /// 环境光要不要重烘：只有影响天空颜色的那几项算（云飘的速度之类不算）。
    fn affects_environment(&self, other: &SkySettings) -> bool {
        (self.time_of_day - other.time_of_day).abs() > 1e-3
            || self.max_elevation != other.max_elevation
            || self.sun_azimuth != other.sun_azimuth
            || self.sun_intensity != other.sun_intensity
            || self.rayleigh != other.rayleigh
            || self.turbidity != other.turbidity
            || (self.cloud_coverage - other.cloud_coverage).abs() > 1e-3
            || self.night_light != other.night_light
    }
}

/// 程序化天空。
pub struct Sky {
    settings: SkySettings,
    quality: Quality,
    dome: Handle<Node>,
    sun: Handle<Node>,
    material: Material,
    /// 云的累计漂移（米）。
    cloud_offset: [f32; 2],
    time: f32,
    /// 环境光烘的是哪一份设置；和当前不同且稳定了一会儿就重烘。
    baked: Option<SkySettings>,
    /// 设置最后一次变化后过了多久（防抖）。
    settle: f32,
    camera_altitude: f32,
    /// 全景模式：有图时天穹贴图、光照取自图，程序化大气和云都不算。
    panorama: Option<Panorama>,
    panorama_material: Material,
}

impl Sky {
    pub fn new(settings: SkySettings, quality: Quality) -> Self {
        let material = Material::standard()
            .with_shader(Resource::new_ok(
                "kcomponents/sky.wgsl",
                kshader::Shader::snippet(SHADER),
            ))
            .with_double_sided();
        Self {
            settings,
            quality,
            dome: Handle::NONE,
            sun: Handle::NONE,
            material,
            cloud_offset: [0.0, 0.0],
            time: 0.0,
            baked: None,
            settle: 0.0,
            camera_altitude: 2.0,
            panorama: None,
            panorama_material: Material::standard()
                .with_shader(Resource::new_ok(
                    "kcomponents/panorama.wgsl",
                    kshader::Shader::snippet(PANORAMA_SHADER),
                ))
                .with_double_sided(),
        }
    }

    /// 切到全景模式（`Some`）或回到程序化天空（`None`）。环境光会重烘。
    pub fn set_panorama(&mut self, scene: &mut Scene, panorama: Option<Panorama>) {
        if let Some(panorama) = &panorama {
            self.panorama_material.set(
                kmaterial::standard::BASE_COLOR_TEXTURE,
                Resource::new_ok("sky panorama", panorama.texture.clone()),
            );
        }
        self.panorama = panorama;
        self.baked = None;
        self.settle = 1.0;
        self.apply(scene);
    }

    pub fn panorama(&self) -> Option<&Panorama> {
        self.panorama.as_ref()
    }

    /// 放天穹和太阳光。
    pub fn spawn(&mut self, scene: &mut Scene) {
        self.dome = scene.add_node(
            Node::new("Sky")
                .with_mesh(Mesh::sphere(48, 96))
                .with_material(self.material.clone())
                .with_casts_shadows(false)
                .with_scale(Vec3::splat(DOME_RADIUS)),
        );
        self.sun = scene.add_node(Node::new("Sun").with_light(Light::directional().with_shadows()));
        self.apply(scene);
        self.bake(scene);
    }

    pub fn settings(&self) -> &SkySettings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: SkySettings) {
        if settings.affects_environment(&self.settings) {
            self.settle = 0.0;
        }
        self.settings = settings;
    }

    pub fn set_quality(&mut self, quality: Quality) {
        self.quality = quality;
    }

    /// 太阳光的节点（想改阴影参数时用）。
    pub fn sun_node(&self) -> Handle<Node> {
        self.sun
    }

    /// 指向主光源（白天太阳、夜里月亮）的方向。
    pub fn light_direction(&self) -> Vec3 {
        if let Some(panorama) = &self.panorama {
            return panorama.sun_direction;
        }
        let sun = self.settings.sun_direction();
        if sun.y > -0.02 {
            sun
        } else {
            // 月亮挂在太阳对面偏高一点。
            Vec3::new(-sun.x, (-sun.y).max(0.25), -sun.z).normalize()
        }
    }

    /// 太阳（或月亮）到达地面的辐射。
    pub fn light_radiance(&self) -> Vec3 {
        if let Some(panorama) = &self.panorama {
            return panorama.sun_color * panorama.sun_intensity;
        }
        let atmosphere = self.settings.atmosphere();
        let sun_light = atmosphere::sun_transmittance(&atmosphere, self.camera_altitude)
            * self.settings.sun_intensity
            * 0.18;
        let cloud_dim = 1.0 - self.settings.cloud_coverage * 0.55;
        let moon = Vec3::new(0.05, 0.07, 0.12) * self.settings.night_light;
        let night = smoothstep(0.06, -0.1, self.settings.sun_direction().y);
        (sun_light * (1.0 - night) + moon * night) * cloud_dim
    }

    /// 海面要的天光。
    pub fn lighting(&self) -> SceneLighting {
        if let Some(panorama) = &self.panorama {
            let mut sky = panorama.radiance(Vec3::Y) * 0.4;
            let mut horizon = Vec3::ZERO;
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                sky += panorama.radiance(Vec3::new(a.cos(), 0.5, a.sin())) * 0.075;
                horizon += panorama.radiance(Vec3::new(a.cos(), 0.03, a.sin())) / 8.0;
            }
            return SceneLighting {
                sun_direction: panorama.sun_direction,
                sun_radiance: self.light_radiance(),
                sky_radiance: sky,
                haze_color: horizon,
                haze_density: self.settings.haze,
            };
        }
        let atmosphere = self.settings.atmosphere();
        let altitude = self.camera_altitude;
        // 天空平均亮度：头顶一次 + 四周斜 30° 四次。
        let mut sky = atmosphere::radiance(&atmosphere, Vec3::Y, altitude) * 0.4;
        for i in 0..4 {
            let a = i as f32 * std::f32::consts::FRAC_PI_2;
            sky += atmosphere::radiance(&atmosphere, Vec3::new(a.cos(), 0.5, a.sin()), altitude)
                * 0.15;
        }
        // 地平线颜色：背对太阳那边和朝着太阳那边的平均，雾不会一侧红一侧蓝得太突兀。
        let sun = self.settings.sun_direction();
        let side = Vec3::new(sun.x, 0.0, sun.z).normalize_or(Vec3::X);
        let horizon =
            (atmosphere::radiance(&atmosphere, Vec3::new(side.x, 0.02, side.z), altitude)
                + atmosphere::radiance(&atmosphere, Vec3::new(-side.x, 0.02, -side.z), altitude)
                    * 2.0)
                / 3.0;
        let night = smoothstep(0.06, -0.1, sun.y);
        let night_floor = Vec3::new(0.004, 0.006, 0.012) * self.settings.night_light;
        let cloud = 1.0 - self.settings.cloud_coverage * 0.3;
        SceneLighting {
            sun_direction: self.light_direction(),
            sun_radiance: self.light_radiance(),
            sky_radiance: sky * cloud + night_floor * night * 4.0,
            haze_color: horizon * cloud + night_floor * night * 2.0,
            haze_density: self.settings.haze,
        }
    }

    /// 每帧：天穹跟到相机、云往前飘、太阳光跟着时刻走；设置稳定之后重烘环境光。
    pub fn update(&mut self, scene: &mut Scene, camera_position: Vec3, dt: f32) {
        self.time += dt;
        self.camera_altitude = camera_position.y.max(1.0);
        let speed = self.settings.cloud_speed * dt;
        self.cloud_offset[0] += self.settings.cloud_direction.cos() * speed;
        self.cloud_offset[1] += self.settings.cloud_direction.sin() * speed;
        if let Some(node) = scene.try_get_mut(self.dome) {
            node.transform.position = camera_position;
        }
        self.apply(scene);

        self.settle += dt;
        let stale = self.baked.is_none_or(|baked| {
            self.panorama.is_none() && baked.affects_environment(&self.settings)
        });
        if stale && self.settle > 0.3 {
            self.bake(scene);
        }
    }

    fn apply(&mut self, scene: &mut Scene) {
        let s = &self.settings;
        let sun = s.sun_direction();
        self.material
            .set_param(0, Vec4::new(sun.x, sun.y, sun.z, s.cloud_coverage));
        self.material.set_param(
            1,
            Vec4::new(s.rayleigh, s.turbidity, s.sun_intensity, s.cloud_density),
        );
        self.material.set_param(
            2,
            Vec4::new(
                self.cloud_offset[0],
                self.cloud_offset[1],
                self.quality.cloud_steps() as f32,
                s.night_light,
            ),
        );
        self.material.set_param(
            3,
            Vec4::new(self.camera_altitude, s.sun_disc, s.haze, self.time),
        );
        if let Some(panorama) = &self.panorama {
            self.panorama_material
                .set_param(0, Vec4::new(panorama.intensity, panorama.yaw, 0.6, s.haze));
        }
        let dome_material = if self.panorama.is_some() {
            &self.panorama_material
        } else {
            &self.material
        };
        if let Some(node) = scene.try_get_mut(self.dome) {
            node.set_material(dome_material.clone());
        }

        let direction = self.light_direction();
        let radiance = self.light_radiance();
        let intensity = radiance.max_element().max(1e-4);
        if let Some(node) = scene.try_get_mut(self.sun) {
            node.transform = Transform::looking_at(direction * 100.0, Vec3::ZERO, Vec3::Y);
            if let Some(light) = node.light_mut() {
                light.color = radiance / intensity;
                light.intensity = intensity;
            }
        }
    }

    /// 把当前天空烘成 HDR 环境光交给场景（漫反射球谐 + 镜面预滤波）。几十毫秒，别每帧调。
    pub fn bake(&mut self, scene: &mut Scene) {
        if let Some(panorama) = &self.panorama {
            let image =
                kpbr::hdr::HdrImage::from_fn(128, 64, |direction| panorama.radiance(direction));
            scene.set_environment_hdr(
                &image,
                kpbr::prefilter::PrefilterSettings {
                    base_width: 128,
                    levels: 5,
                    samples: 48,
                },
            );
            self.baked = Some(self.settings);
            return;
        }
        let atmosphere = self.settings.atmosphere();
        let altitude = self.camera_altitude;
        let night = smoothstep(0.06, -0.1, atmosphere.sun_direction.y);
        let night_floor = Vec3::new(0.004, 0.006, 0.012) * self.settings.night_light * night;
        let cloud = 1.0 - self.settings.cloud_coverage * 0.3;
        // 云在环境图里只做「整体压暗一点、往灰里拉」：逐像素画云的话一改云量就得重算，而反射里本来也看不清。
        let gray = Vec3::splat(0.0);
        let image = kpbr::hdr::HdrImage::from_fn(128, 64, |direction| {
            let sky = atmosphere::radiance(&atmosphere, direction, altitude) * cloud + night_floor;
            let below = direction.y < 0.0;
            if below {
                sky * (1.0 + direction.y * 0.6).max(0.2) + gray
            } else {
                sky
            }
        });
        scene.set_environment_hdr(
            &image,
            kpbr::prefilter::PrefilterSettings {
                base_width: 128,
                levels: 5,
                samples: 48,
            },
        );
        self.baked = Some(self.settings);
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_rises_in_the_morning_and_sets_in_the_evening() {
        let at = |hour: f32| {
            SkySettings {
                time_of_day: hour,
                ..Default::default()
            }
            .sun_direction()
        };
        assert!(at(12.0).y > 0.8);
        assert!(at(6.5).y > 0.0 && at(6.5).y < 0.2);
        assert!(at(22.0).y < 0.0);
    }

    #[test]
    fn sunset_light_is_warm_and_noon_light_is_white() {
        let sky = |hour: f32| {
            let mut sky = Sky::new(
                SkySettings {
                    time_of_day: hour,
                    cloud_coverage: 0.0,
                    ..Default::default()
                },
                Quality::Low,
            );
            sky.camera_altitude = 2.0;
            sky.light_radiance()
        };
        let noon = sky(12.0);
        let dusk = sky(17.8);
        assert!(dusk.x > dusk.z * 1.8, "日落偏红：{dusk:?}");
        assert!(noon.z > noon.x * 0.6, "正午接近白：{noon:?}");
    }

    #[test]
    fn the_shader_compiles_against_the_engine() {
        krender::validate_material_hook(SHADER).expect("sky.wgsl 编不过");
        krender::validate_material_hook(PANORAMA_SHADER).expect("panorama.wgsl 编不过");
    }

    #[test]
    fn the_panorama_sun_is_found_where_the_image_is_brightest() {
        // 一张 64×32 的图，左上四分之一处有一个白点。
        let (w, h) = (64usize, 32usize);
        let mut data = vec![60u8; w * h * 4];
        let (sx, sy) = (16usize, 8usize);
        for dy in 0..2 {
            for dx in 0..2 {
                let i = ((sy + dy) * w + sx + dx) * 4;
                data[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let panorama = Panorama::new(ktexture::Texture::new(w as u32, h as u32, data), 1.0, 0.0);
        // 那个点在 v ≈ 0.27 处：仰角约 40°。
        assert!(
            panorama.sun_direction.y > 0.5,
            "{:?}",
            panorama.sun_direction
        );
        // 和 sample_direction 互逆：沿算出的方向采样应该是最亮的。
        let at_sun = panorama.small.sample_direction(panorama.sun_direction);
        let elsewhere = panorama.small.sample_direction(-panorama.sun_direction);
        assert!(at_sun.x > elsewhere.x * 2.0, "{at_sun:?} vs {elsewhere:?}");
    }
}
