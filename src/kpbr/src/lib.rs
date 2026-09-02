//! kpbr —— 基于物理的渲染。
//!
//! 提供三样东西：
//!
//! - [`PBR_WGSL`]：Cook-Torrance BRDF 的 WGSL 实现，由渲染器拼进着色器
//! - [`brdf`]：同一套公式的 CPU 实现，**着色器没法单元测试，这份可以**
//! - [`standard`] 与 [`PbrMaterial`]：PBR 材质参数的标准命名与便捷构造
//!
//! ```
//! use kpbr::prelude::*;
//! use kmath::Vec3;
//!
//! // 金属的垂直入射反射率直接取基础色，电介质则统一约 4%。
//! let gold = Vec3::new(1.0, 0.77, 0.34);
//! assert_eq!(brdf::f0(gold, 1.0), gold);
//! assert_eq!(brdf::f0(gold, 0.0), Vec3::splat(brdf::DIELECTRIC_F0));
//! ```

#![warn(missing_docs)]

pub mod brdf;
pub mod hdr;
pub mod ibl;
pub mod loader;
pub mod prefilter;
pub mod probe;
pub mod sky;

use crate::sky::Sky;
use kmaterial::Material;
use kmath::{Vec3, Vec4};

/// Cook-Torrance BRDF 的 WGSL 源码。
///
/// 渲染器把它拼接到标准着色器前面，因此其中的函数名统一带 `pbr_` 前缀以免冲突。
pub const PBR_WGSL: &str = include_str!("pbr.wgsl");

/// IBL（球谐辐照度 + 解析天空 + 环境 BRDF）的 WGSL 源码。
///
/// 定义了 `Environment` 结构，渲染器的 `Globals` 会用到它，
/// 因此拼接时必须排在标准着色器之前。
pub const IBL_WGSL: &str = include_str!("ibl.wgsl");

/// 常用项的集中导出。
pub mod prelude {
    pub use crate::{
        Environment, GpuEnvironment, IBL_WGSL, PBR_WGSL, PbrMaterial, brdf, ibl, sky::Sky, standard,
    };
}

/// PBR 材质参数的标准命名。
///
/// 与 [`kmaterial::standard`] 互补——那里是基础色与金属度粗糙度，这里是扩展通道。
pub mod standard {
    /// 自发光颜色，`Vec3`。
    pub const EMISSIVE: &str = "emissive";
    /// 环境光遮蔽系数，`Float`，取值 `[0, 1]`。
    pub const OCCLUSION: &str = "occlusion";
    /// 法线贴图，`Texture`。
    pub const NORMAL_TEXTURE: &str = "normal_texture";
    /// 金属度粗糙度贴图（glTF 约定：G 通道粗糙度、B 通道金属度），`Texture`。
    pub const METALLIC_ROUGHNESS_TEXTURE: &str = "metallic_roughness_texture";
    /// 自发光贴图，`Texture`。
    pub const EMISSIVE_TEXTURE: &str = "emissive_texture";
    /// 环境光遮蔽贴图，`Texture`。
    pub const OCCLUSION_TEXTURE: &str = "occlusion_texture";
    /// 纹理坐标缩放，`Vec2`，默认 `(1, 1)`。
    ///
    /// 与 [`UV_OFFSET`] 一起用来从图集里取一格：采样坐标 = `uv × 缩放 + 偏移`。
    /// 所有贴图槽共用同一套变换，否则法线贴图会和基础色错位。
    pub const UV_SCALE: &str = "uv_scale";
    /// 纹理坐标偏移，`Vec2`，默认 `(0, 0)`。见 [`UV_SCALE`]。
    pub const UV_OFFSET: &str = "uv_offset";
}

/// 环境光照。
///
/// 由一片程序化天空 [`Sky`] 驱动：漫反射部分预先投影成球谐系数，
/// 镜面部分在着色器里按反射方向解析求值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    /// 天空。改动后需调用 [`Environment::rebuild`] 重算球谐。
    pub sky: Sky,
    /// 环境光整体强度。
    pub intensity: f32,
    /// 距离雾。默认关闭。
    pub fog: Fog,
    /// 由天空投影出的漫反射球谐系数。
    harmonics: ibl::SphericalHarmonics,
}

impl Default for Environment {
    fn default() -> Self {
        Self::from_sky(Sky::default())
    }
}

impl kcore::visitor::Visit for Environment {
    fn visit(
        &mut self,
        name: &str,
        visitor: &mut kcore::visitor::Visitor,
    ) -> kcore::visitor::VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.sky.visit("Sky", &mut region)?;
        self.intensity.visit("Intensity", &mut region)?;

        // 球谐系数完全由天空投影而来，读回后重算而不是存下来——
        // 存一份派生数据就多一处可能和本体对不上的「真相」，
        // 而且投影一次只有几十微秒。
        if region.is_reading() {
            self.rebuild();
        }

        Ok(())
    }
}

impl Environment {
    /// 用给定天空构造，并立即投影出球谐系数。
    pub fn from_sky(sky: Sky) -> Self {
        Self {
            sky,
            intensity: 1.0,
            fog: Fog::default(),
            harmonics: ibl::SphericalHarmonics::from_sky(&sky, 48),
        }
    }

    /// 用一张实拍的 HDR 全景图当环境光。
    ///
    /// 程序化天空只有天顶、地平线、地面三个颜色加一个太阳，给不出
    /// 真实场景的光照分布——窗户的方向、树荫下的绿色反射、傍晚天空的
    /// 渐变，这些只能从实拍来。
    ///
    /// [`Environment::sky`] 仍然保留：它负责画背景（HDR 的镜面部分
    /// 还没接进着色器，见 `next.md`）。想让背景也用 HDR 得等那一步。
    ///
    /// 采样数比程序化天空高：实拍图里有窗户、灯这类很亮很小的区域，
    /// 采样不够会让它们时有时无。**这是一次 96×96 的球面积分，
    /// 不要每帧调用。**
    pub fn from_hdr(image: &hdr::HdrImage, sky: Sky) -> Self {
        Self {
            sky,
            intensity: 1.0,
            fog: Fog::default(),
            harmonics: ibl::SphericalHarmonics::from_hdr(image, 96),
        }
    }

    /// 换一张 HDR 环境图，重算球谐。
    pub fn set_hdr(&mut self, image: &hdr::HdrImage) {
        self.harmonics = ibl::SphericalHarmonics::from_hdr(image, 96);
    }

    /// 改动 [`Environment::sky`] 之后调用，重算球谐系数。
    ///
    /// 这是一次 48×48 的球面积分，不要每帧调用。
    pub fn rebuild(&mut self) {
        self.harmonics = ibl::SphericalHarmonics::from_sky(&self.sky, 48);
    }

    /// 漫反射球谐系数。
    pub fn harmonics(&self) -> &ibl::SphericalHarmonics {
        &self.harmonics
    }

    /// 某个法线方向上的漫反射辐照度，已乘上整体强度。
    pub fn irradiance(&self, normal: Vec3) -> Vec3 {
        self.harmonics.irradiance(normal) * self.intensity.max(0.0)
    }
}

/// 线性雾：距离越远，物体颜色越向雾色靠拢。
///
/// # 为什么要有雾
///
/// 远处的几何被视锥远平面**硬切**掉时，物体会在某个距离上凭空消失，
/// 一眼就能看出边界。雾让它们在到达远平面之前先融进背景里，
/// 于是可以把远平面拉近、少画很多东西。
///
/// # 线性而不是指数
///
/// 指数雾（`exp`、`exp2`）更接近真实的大气散射，但它没有「到这个距离
/// 就完全看不见了」这个硬保证——总有一点点残留。线性雾的
/// [`far`](Self::far) 就是那个保证，正好配合远平面用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fog {
    /// 雾色。通常取天空或背景色，否则远处的物体会显出一圈异色轮廓。
    pub color: Vec3,
    /// 从这个距离开始起雾。
    pub near: f32,
    /// 到这个距离完全被雾吞没。
    pub far: f32,
    /// 是否启用。
    pub enabled: bool,
}

impl Default for Fog {
    fn default() -> Self {
        Self {
            color: Vec3::new(0.5, 0.6, 0.7),
            near: 20.0,
            far: 200.0,
            enabled: false,
        }
    }
}

impl Fog {
    /// 建一团雾并启用。
    pub fn new(color: Vec3, near: f32, far: f32) -> Self {
        Self {
            color,
            near,
            far,
            enabled: true,
        }
    }

    /// 某个距离处的雾浓度，0 是完全清澈、1 是完全被吞没。
    ///
    /// 和着色器里的算法必须一致——这个函数存在的意义就是让那段
    /// 算法能在 CPU 上被测。
    pub fn density_at(&self, distance: f32) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        // `far` 不大于 `near` 时整段区间退化成一个点。返回 0（完全清澈）
        // 而不是让它除以零算出 NaN——NaN 会把整个像素染成黑色。
        let span = self.far - self.near;
        if span <= 1e-6 {
            return 0.0;
        }
        ((distance - self.near) / span).clamp(0.0, 1.0)
    }
}

/// [`Environment`] 的 GPU 表示，与 `ibl.wgsl` 的 `Environment` 结构逐字段对应。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuEnvironment {
    /// 球谐系数，xyz 有效，w 为对齐填充。
    pub sh: [[f32; 4]; ibl::SH_COEFFICIENT_COUNT],
    /// rgb = 天顶色。
    pub zenith: [f32; 4],
    /// rgb = 地平线色。
    pub horizon: [f32; 4],
    /// rgb = 地面色。
    pub ground: [f32; 4],
    /// xyz = 指向太阳的方向，w = 角半径余弦阈值。
    pub sun_direction: [f32; 4],
    /// rgb = 太阳颜色，a = 环境光整体强度。
    pub sun_color: [f32; 4],
    /// rgb = 雾色，a = 是否启用（0/1）。
    pub fog_color: [f32; 4],
    /// x = 起雾距离，y = 全雾距离，zw 保留。
    pub fog_params: [f32; 4],
}

impl Environment {
    /// 打包成 GPU uniform 数据。
    pub fn to_gpu(&self) -> GpuEnvironment {
        let mut sh = [[0.0f32; 4]; ibl::SH_COEFFICIENT_COUNT];
        for (slot, coefficient) in sh.iter_mut().zip(self.harmonics.coefficients()) {
            *slot = [coefficient.x, coefficient.y, coefficient.z, 0.0];
        }

        let sun = self.sky.sun_direction.normalize_or(Vec3::Y);
        GpuEnvironment {
            sh,
            zenith: self.sky.zenith.extend(1.0).to_array(),
            horizon: self.sky.horizon.extend(1.0).to_array(),
            ground: self.sky.ground.extend(1.0).to_array(),
            sun_direction: [sun.x, sun.y, sun.z, self.sky.sun_size],
            sun_color: self
                .sky
                .sun_color
                .extend(self.intensity.max(0.0))
                .to_array(),
            fog_color: self
                .fog
                .color
                .extend(if self.fog.enabled { 1.0 } else { 0.0 })
                .to_array(),
            fog_params: [self.fog.near, self.fog.far, 0.0, 0.0],
        }
    }
}

/// 常见 PBR 材质的便捷构造。
///
/// 这不是新类型，只是往 [`Material`] 里填标准参数的一组快捷方式。
pub struct PbrMaterial;

impl PbrMaterial {
    /// 金属材质：金属度 1，基础色即反射色。
    pub fn metal(color: Vec3, roughness: f32) -> Material {
        Material::standard()
            .with_base_color(color.extend(1.0))
            .with_metallic(1.0)
            .with_roughness(roughness.clamp(0.0, 1.0))
    }

    /// 电介质材质（塑料、木头、石头等）：金属度 0。
    pub fn dielectric(color: Vec3, roughness: f32) -> Material {
        Material::standard()
            .with_base_color(color.extend(1.0))
            .with_metallic(0.0)
            .with_roughness(roughness.clamp(0.0, 1.0))
    }

    /// 自发光材质。
    pub fn emissive(color: Vec3, emissive: Vec3) -> Material {
        Self::dielectric(color, 1.0).with(standard::EMISSIVE, emissive)
    }

    /// 抛光金属，粗糙度很低、高光锐利。
    pub fn polished_metal(color: Vec3) -> Material {
        Self::metal(color, 0.08)
    }

    /// 常见金属的基础色（线性空间）。
    pub const GOLD: Vec4 = Vec4::new(1.0, 0.766, 0.336, 1.0);
    /// 银。
    pub const SILVER: Vec4 = Vec4::new(0.972, 0.960, 0.915, 1.0);
    /// 铜。
    pub const COPPER: Vec4 = Vec4::new(0.955, 0.638, 0.538, 1.0);
    /// 铁。
    pub const IRON: Vec4 = Vec4::new(0.562, 0.565, 0.578, 1.0);
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn wgsl_source_is_valid() {
        // kpbr 导出的 WGSL 必须能通过 naga 校验，
        // 否则错误要等到渲染器建管线时才暴露。
        kshader::Shader::from_wgsl(PBR_WGSL).expect("PBR 着色器代码应当合法");
    }

    #[test]
    fn ibl_wgsl_source_is_valid() {
        kshader::Shader::from_wgsl(IBL_WGSL).expect("IBL 着色器代码应当合法");
    }

    #[test]
    fn gpu_environment_layout_is_aligned() {
        // 球谐 9 个 vec4 + 天空/太阳 5 个 vec4 + 雾 2 个 vec4 = 256 字节，
        // uniform 要求 16 字节对齐。
        assert_eq!(size_of::<GpuEnvironment>(), 16 * (9 + 5 + 2));
        assert_eq!(size_of::<GpuEnvironment>() % 16, 0);
    }

    #[test]
    fn gpu_environment_carries_intensity() {
        let env = Environment {
            intensity: 0.75,
            ..Default::default()
        };

        // 强度打包在 sun_color 的 a 分量里，着色器靠它缩放环境光。
        assert_eq!(env.to_gpu().sun_color[3], 0.75);
    }

    #[test]
    fn wgsl_exposes_expected_functions() {
        // 渲染器按名字调用这些函数，改名会导致拼接后的着色器编译失败。
        for name in [
            "pbr_direct_lighting",
            "pbr_area_lighting",
            "pbr_ambient",
            "pbr_fresnel_schlick",
            "pbr_distribution_ggx",
            "pbr_geometry_smith",
        ] {
            assert!(PBR_WGSL.contains(name), "WGSL 缺少函数 {name}");
        }

        for name in ["ibl_irradiance", "ibl_specular", "ibl_diffuse", "ibl_sky"] {
            assert!(IBL_WGSL.contains(name), "IBL WGSL 缺少函数 {name}");
        }
    }

    #[test]
    fn metal_and_dielectric_differ_in_metallic() {
        let metal = PbrMaterial::metal(Vec3::ONE, 0.3);
        let plastic = PbrMaterial::dielectric(Vec3::ONE, 0.3);

        assert_eq!(metal.metallic(), 1.0);
        assert_eq!(plastic.metallic(), 0.0);
        assert_eq!(metal.roughness(), 0.3);
    }

    #[test]
    fn roughness_is_clamped() {
        // 超范围的粗糙度会让 GGX 分母出问题，构造时就钳制掉。
        assert_eq!(PbrMaterial::metal(Vec3::ONE, 5.0).roughness(), 1.0);
        assert_eq!(PbrMaterial::metal(Vec3::ONE, -1.0).roughness(), 0.0);
    }

    #[test]
    fn emissive_material_carries_emission() {
        let material = PbrMaterial::emissive(Vec3::ZERO, Vec3::new(2.0, 0.5, 0.0));

        let emission = material
            .get(standard::EMISSIVE)
            .and_then(kmaterial::MaterialValue::as_vec3);

        assert_eq!(emission, Some(Vec3::new(2.0, 0.5, 0.0)));
    }

    #[test]
    fn environment_irradiance_scales_with_intensity() {
        let mut env = Environment::default();
        let base = env.irradiance(Vec3::Y);

        env.intensity = 2.0;
        let doubled = env.irradiance(Vec3::Y);

        assert!((doubled - base * 2.0).length() < 1e-4);
    }

    #[test]
    fn negative_intensity_yields_no_light() {
        let env = Environment {
            intensity: -1.0,
            ..Default::default()
        };

        assert_eq!(env.irradiance(Vec3::Y), Vec3::ZERO);
    }

    #[test]
    fn rebuild_picks_up_sky_changes() {
        let mut env = Environment::default();
        let before = env.irradiance(Vec3::Y);

        // 把天空调暗后必须重算，否则球谐还是旧的。
        env.sky.zenith = Vec3::ZERO;
        env.sky.horizon = Vec3::ZERO;
        env.sky.ground = Vec3::ZERO;
        env.rebuild();

        assert!(env.irradiance(Vec3::Y).length() < before.length());
    }
}

#[cfg(test)]
mod fog_test {
    use super::*;

    #[test]
    fn fog_is_off_by_default() {
        // 默认开着的话，每个现有场景的远处都会莫名其妙地褪色。
        assert!(!Fog::default().enabled);
        assert_eq!(Fog::default().density_at(1e9), 0.0);
    }

    #[test]
    fn density_ramps_from_near_to_far() {
        let fog = Fog::new(Vec3::ZERO, 10.0, 20.0);
        assert_eq!(fog.density_at(5.0), 0.0, "起雾距离之前该是清澈的");
        assert_eq!(fog.density_at(10.0), 0.0);
        assert!((fog.density_at(15.0) - 0.5).abs() < 1e-6);
        assert_eq!(fog.density_at(20.0), 1.0);
        assert_eq!(fog.density_at(100.0), 1.0, "全雾距离之后不该超过 1");
    }

    #[test]
    fn a_degenerate_range_does_not_produce_nan() {
        // far <= near 时整段区间退化成一个点。除以零算出的 NaN
        // 会把整个像素染黑，而且很难查到源头。
        for (near, far) in [(10.0, 10.0), (20.0, 10.0), (0.0, 0.0)] {
            let fog = Fog::new(Vec3::ZERO, near, far);
            for distance in [0.0, 5.0, 15.0, 1e6] {
                let density = fog.density_at(distance);
                assert!(density.is_finite(), "near={near} far={far} d={distance}");
                assert!((0.0..=1.0).contains(&density));
            }
        }
    }

    #[test]
    fn disabling_wins_over_the_range() {
        let mut fog = Fog::new(Vec3::ZERO, 1.0, 2.0);
        fog.enabled = false;
        assert_eq!(fog.density_at(1000.0), 0.0);
    }

    #[test]
    fn fog_reaches_the_gpu() {
        // 打包漏了的话着色器读到的是零，雾静默失效。
        let mut environment = Environment {
            fog: Fog::new(Vec3::new(0.1, 0.2, 0.3), 7.0, 25.0),
            ..Default::default()
        };

        let gpu = environment.to_gpu();
        assert_eq!(gpu.fog_color, [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(gpu.fog_params[0], 7.0);
        assert_eq!(gpu.fog_params[1], 25.0);

        environment.fog.enabled = false;
        assert_eq!(environment.to_gpu().fog_color[3], 0.0, "关掉了却还传了 1");
    }
}
