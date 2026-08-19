//! 基于图像的光照（IBL）。
//!
//! 两块内容，都能在 CPU 上算出来因而可以被测试：
//!
//! - [`SphericalHarmonics`]：把环境光的漫反射部分压成 9 个系数
//!   （Ramamoorthi & Hanrahan 的方法），着色器只需一次多项式求值
//! - [`integrate_brdf`] / [`brdf_lut`]：split-sum 近似的第二项，
//!   预计算成一张查找表供着色器采样
//!
//! 镜面部分没有做预滤波 cubemap，而是在着色器里直接解析求值天空——
//! 代价是只支持程序化天空，好处是不需要任何 GPU 预计算管线。

use crate::{brdf, sky::Sky};
use kmath::{Vec2, Vec3};
use std::f32::consts::{PI, TAU};

/// L2 球谐的系数个数。
pub const SH_COEFFICIENT_COUNT: usize = 9;

/// 环境光的球谐表示（L2，9 个系数）。
///
/// 漫反射环境光是低频信号，9 个系数就足以还原，误差通常在 1% 以内。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SphericalHarmonics {
    coefficients: [Vec3; SH_COEFFICIENT_COUNT],
}

impl Default for SphericalHarmonics {
    fn default() -> Self {
        Self {
            coefficients: [Vec3::ZERO; SH_COEFFICIENT_COUNT],
        }
    }
}

/// L2 球谐基函数在给定方向上的取值。
fn sh_basis(d: Vec3) -> [f32; SH_COEFFICIENT_COUNT] {
    [
        0.282_095,                           // Y00
        0.488_603 * d.y,                     // Y1-1
        0.488_603 * d.z,                     // Y10
        0.488_603 * d.x,                     // Y11
        1.092_548 * d.x * d.y,               // Y2-2
        1.092_548 * d.y * d.z,               // Y2-1
        0.315_392 * (3.0 * d.z * d.z - 1.0), // Y20
        1.092_548 * d.x * d.z,               // Y21
        0.546_274 * (d.x * d.x - d.y * d.y), // Y22
    ]
}

/// 余弦卷积系数：把辐射亮度转成辐照度时各阶的缩放。
///
/// 来自 Ramamoorthi & Hanrahan 2001，对 L2 而言只有三个不同取值。
const COSINE_CONVOLUTION: [f32; SH_COEFFICIENT_COUNT] = [
    PI,
    2.0 * PI / 3.0,
    2.0 * PI / 3.0,
    2.0 * PI / 3.0,
    PI / 4.0,
    PI / 4.0,
    PI / 4.0,
    PI / 4.0,
    PI / 4.0,
];

impl SphericalHarmonics {
    /// 把天空投影到球谐基上。
    ///
    /// `samples` 是每个维度的采样数，总采样量为其平方；64 已足够平滑。
    /// 太阳不计入——它通常已由一盏方向光单独表示，重复计入会过曝。
    pub fn from_sky(sky: &Sky, samples: u32) -> Self {
        let samples = samples.max(8);
        let mut coefficients = [Vec3::ZERO; SH_COEFFICIENT_COUNT];
        let mut total_weight = 0.0f32;

        for i in 0..samples {
            // 在 [-1, 1] 上均匀采 cosθ，保证球面上的采样密度一致。
            let cos_theta = 1.0 - 2.0 * (i as f32 + 0.5) / samples as f32;
            let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();

            for j in 0..samples {
                let phi = (j as f32 + 0.5) / samples as f32 * TAU;
                let (sin_phi, cos_phi) = phi.sin_cos();

                let direction = Vec3::new(sin_theta * cos_phi, cos_theta, sin_theta * sin_phi);
                let radiance = sky.sample_without_sun(direction);
                let basis = sh_basis(direction);

                for (coefficient, basis_value) in coefficients.iter_mut().zip(basis) {
                    *coefficient += radiance * basis_value;
                }
                total_weight += 1.0;
            }
        }

        // 均匀采样的立体角权重是 4π/N。
        let scale = 4.0 * PI / total_weight.max(1.0);
        for coefficient in &mut coefficients {
            *coefficient *= scale;
        }

        Self { coefficients }
    }

    /// 求某个法线方向上的辐照度（已含余弦卷积）。
    pub fn irradiance(&self, normal: Vec3) -> Vec3 {
        let basis = sh_basis(normal);
        let mut result = Vec3::ZERO;

        for ((coefficient, basis_value), convolution) in
            self.coefficients.iter().zip(basis).zip(COSINE_CONVOLUTION)
        {
            result += *coefficient * basis_value * convolution;
        }

        // 低阶球谐在高对比环境下可能出现轻微负值，钳掉以免出现负光照。
        result.max(Vec3::ZERO)
    }

    /// 全部系数，用于打包进 uniform。
    pub fn coefficients(&self) -> &[Vec3; SH_COEFFICIENT_COUNT] {
        &self.coefficients
    }
}

/// 低差异序列，用于重要性采样。
fn hammersley(index: u32, count: u32) -> Vec2 {
    // 二进制反转得到 Van der Corput 序列。
    let mut bits = index;
    bits = bits.rotate_right(16);
    bits = ((bits & 0x5555_5555) << 1) | ((bits & 0xAAAA_AAAA) >> 1);
    bits = ((bits & 0x3333_3333) << 2) | ((bits & 0xCCCC_CCCC) >> 2);
    bits = ((bits & 0x0F0F_0F0F) << 4) | ((bits & 0xF0F0_F0F0) >> 4);
    bits = ((bits & 0x00FF_00FF) << 8) | ((bits & 0xFF00_FF00) >> 8);

    Vec2::new(
        index as f32 / count.max(1) as f32,
        bits as f32 * 2.328_306_4e-10,
    )
}

/// 按 GGX 分布重要性采样半程向量（切线空间，法线为 +Z）。
fn importance_sample_ggx(xi: Vec2, roughness: f32) -> Vec3 {
    let a = roughness * roughness;

    let phi = TAU * xi.x;
    let cos_theta = ((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y).max(1e-7))
        .max(0.0)
        .sqrt();
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();

    Vec3::new(phi.cos() * sin_theta, phi.sin() * sin_theta, cos_theta)
}

/// split-sum 近似的第二项：环境 BRDF 的缩放与偏移。
///
/// 返回 `(scale, bias)`，着色器用它们组合出 `F0 * scale + bias`。
pub fn integrate_brdf(n_dot_v: f32, roughness: f32) -> Vec2 {
    const SAMPLES: u32 = 256;

    let n_dot_v = n_dot_v.clamp(1e-3, 1.0);
    // 切线空间里法线为 +Z，视线放在 XZ 平面内。
    let v = Vec3::new((1.0 - n_dot_v * n_dot_v).max(0.0).sqrt(), 0.0, n_dot_v);

    let mut scale = 0.0;
    let mut bias = 0.0;

    for index in 0..SAMPLES {
        let xi = hammersley(index, SAMPLES);
        let h = importance_sample_ggx(xi, roughness);
        let l = (2.0 * v.dot(h) * h - v).normalize_or_zero();

        let n_dot_l = l.z.max(0.0);
        if n_dot_l <= 0.0 {
            continue;
        }
        let n_dot_h = h.z.max(0.0);
        let v_dot_h = v.dot(h).max(0.0);

        // IBL 的几何项用 k = a/2，与直接光照的 (r+1)²/8 不同，混用会导致亮度偏差。
        let a = roughness * roughness;
        let k = a / 2.0;
        let g = brdf::geometry_schlick_ggx(n_dot_v, k) * brdf::geometry_schlick_ggx(n_dot_l, k);

        let g_visibility = g * v_dot_h / (n_dot_h * n_dot_v).max(1e-7);
        let fresnel = (1.0 - v_dot_h).clamp(0.0, 1.0).powi(5);

        scale += (1.0 - fresnel) * g_visibility;
        bias += fresnel * g_visibility;
    }

    Vec2::new(scale, bias) / SAMPLES as f32
}

/// 生成 BRDF 查找表。
///
/// 返回 `size × size` 个 `(scale, bias)`，行方向是粗糙度，列方向是 `n·v`。
pub fn brdf_lut(size: u32) -> Vec<Vec2> {
    let size = size.max(2);
    let mut lut = Vec::with_capacity((size * size) as usize);

    for y in 0..size {
        let roughness = (y as f32 + 0.5) / size as f32;
        for x in 0..size {
            let n_dot_v = (x as f32 + 0.5) / size as f32;
            lut.push(integrate_brdf(n_dot_v, roughness));
        }
    }

    lut
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn constant_environment_reconstructs_constant_irradiance() {
        // 全白天空：辐照度应当各向同性，且等于 π × 亮度。
        let sky = Sky {
            zenith: Vec3::ONE,
            horizon: Vec3::ONE,
            ground: Vec3::ONE,
            sun_color: Vec3::ZERO,
            ..Default::default()
        };
        let sh = SphericalHarmonics::from_sky(&sky, 64);

        let up = sh.irradiance(Vec3::Y);
        let side = sh.irradiance(Vec3::X);
        let down = sh.irradiance(-Vec3::Y);

        // 各方向应当一致。
        assert!((up - side).length() < 0.02, "{up:?} vs {side:?}");
        assert!((up - down).length() < 0.02);
        // 常量辐射亮度 L 的辐照度为 πL。
        assert!((up.x - PI).abs() < 0.05, "辐照度应约为 π，实得 {}", up.x);
    }

    #[test]
    fn irradiance_follows_environment_gradient() {
        // 天顶亮、地面暗时，朝上的法线应当比朝下的收到更多光。
        let sky = Sky {
            zenith: Vec3::ONE,
            horizon: Vec3::splat(0.5),
            ground: Vec3::ZERO,
            sun_color: Vec3::ZERO,
            ..Default::default()
        };
        let sh = SphericalHarmonics::from_sky(&sky, 64);

        assert!(sh.irradiance(Vec3::Y).x > sh.irradiance(-Vec3::Y).x);
    }

    #[test]
    fn irradiance_is_never_negative() {
        // 高对比环境最容易让低阶球谐出现负值。
        let sky = Sky {
            zenith: Vec3::splat(10.0),
            horizon: Vec3::ZERO,
            ground: Vec3::ZERO,
            sun_color: Vec3::ZERO,
            ..Default::default()
        };
        let sh = SphericalHarmonics::from_sky(&sky, 64);

        for i in 0..32 {
            let t = i as f32 / 32.0 * TAU;
            for y in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                let normal = Vec3::new(t.cos(), y, t.sin()).normalize();
                let value = sh.irradiance(normal);

                assert!(value.min_element() >= 0.0, "出现负辐照度 {value:?}");
                assert!(value.is_finite());
            }
        }
    }

    #[test]
    fn black_environment_yields_no_irradiance() {
        let sky = Sky {
            zenith: Vec3::ZERO,
            horizon: Vec3::ZERO,
            ground: Vec3::ZERO,
            sun_color: Vec3::ZERO,
            ..Default::default()
        };
        let sh = SphericalHarmonics::from_sky(&sky, 32);

        assert!(sh.irradiance(Vec3::Y).length() < 1e-4);
    }

    #[test]
    fn cosine_convolution_matches_published_values() {
        // Ramamoorthi & Hanrahan 的三个卷积系数。写错会让环境光整体偏亮或偏暗，
        // 而画面上很难看出是系数错了还是天空太亮。
        assert!((COSINE_CONVOLUTION[0] - PI).abs() < 1e-6);
        assert!((COSINE_CONVOLUTION[1] - 2.0 * PI / 3.0).abs() < 1e-6);
        assert!((COSINE_CONVOLUTION[4] - PI / 4.0).abs() < 1e-6);
        // L1 的三项与 L2 的五项各自相同。
        assert_eq!(COSINE_CONVOLUTION[1], COSINE_CONVOLUTION[3]);
        assert_eq!(COSINE_CONVOLUTION[4], COSINE_CONVOLUTION[8]);
    }

    #[test]
    fn wgsl_uses_the_same_basis_constants() {
        // 球谐基的常数在 WGSL 里是手写的，与这里必须一致。
        // 任何一边改了而另一边没跟上，环境光就会错，且不会有任何报错。
        for constant in ["0.282095", "0.488603", "1.092548", "0.315392", "0.546274"] {
            assert!(
                crate::IBL_WGSL.contains(constant),
                "IBL WGSL 缺少球谐常数 {constant}"
            );
        }
    }

    #[test]
    fn hammersley_stays_in_unit_square() {
        for i in 0..256 {
            let point = hammersley(i, 256);
            assert!((0.0..=1.0).contains(&point.x));
            assert!((0.0..=1.0).contains(&point.y));
        }
    }

    #[test]
    fn ggx_samples_are_unit_vectors_in_upper_hemisphere() {
        for roughness in [0.05, 0.5, 1.0] {
            for i in 0..64 {
                let h = importance_sample_ggx(hammersley(i, 64), roughness);

                assert!((h.length() - 1.0).abs() < 1e-4, "采样向量未归一化");
                assert!(h.z >= 0.0, "采样跑到了下半球");
            }
        }
    }

    #[test]
    fn brdf_lut_values_stay_in_unit_range() {
        for roughness in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for i in 1..=10 {
                let value = integrate_brdf(i as f32 / 10.0, roughness);

                assert!(value.is_finite(), "LUT 出现 NaN");
                assert!(
                    (0.0..=1.0).contains(&value.x) && (0.0..=1.0).contains(&value.y),
                    "LUT 值越界：{value:?}（r={roughness}）"
                );
            }
        }
    }

    #[test]
    fn smooth_surface_at_normal_incidence_preserves_f0() {
        // 完全光滑 + 正对视角时，scale 应接近 1、bias 接近 0，
        // 即反射率就等于 F0。这是 split-sum 近似的边界条件。
        let value = integrate_brdf(1.0, 0.0);

        assert!(
            (value.x - 1.0).abs() < 0.05,
            "scale 应接近 1，实得 {}",
            value.x
        );
        assert!(value.y < 0.05, "bias 应接近 0，实得 {}", value.y);
    }

    #[test]
    fn grazing_angle_increases_bias() {
        // 掠射角下菲涅尔抬升，bias 项应当更大。
        let normal = integrate_brdf(1.0, 0.5);
        let grazing = integrate_brdf(0.05, 0.5);

        assert!(grazing.y > normal.y);
    }

    #[test]
    fn lut_has_expected_dimensions() {
        let size = 16;
        let lut = brdf_lut(size);

        assert_eq!(lut.len(), (size * size) as usize);
        assert!(lut.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn lut_size_is_clamped() {
        // 尺寸为 0 会导致除零与空表。
        assert!(!brdf_lut(0).is_empty());
    }
}
