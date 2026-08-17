//! Cook-Torrance BRDF 的 CPU 实现。
//!
//! 与 [`crate::PBR_WGSL`] 里的 WGSL 版本逐函数对应。两份实现必须保持一致——
//! 着色器无法直接单元测试，而这份 CPU 实现可以，用来断言 BRDF 的数学性质
//! （能量守恒、互易性、菲涅尔边界行为等）。改动其中一份时另一份也要跟着改。

use kmath::Vec3;

/// 电介质的垂直入射反射率，约 4%。
pub const DIELECTRIC_F0: f32 = 0.04;

/// 法线分布函数：GGX / Trowbridge-Reitz。
pub fn distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let n_dot_h = n_dot_h.max(0.0);
    let n_dot_h2 = n_dot_h * n_dot_h;

    let denominator = n_dot_h2 * (a2 - 1.0) + 1.0;
    a2 / (std::f32::consts::PI * denominator * denominator).max(1e-7)
}

/// 几何遮蔽项的单侧分量（Schlick-GGX）。
pub fn geometry_schlick_ggx(n_dot_x: f32, k: f32) -> f32 {
    let x = n_dot_x.max(0.0);
    x / (x * (1.0 - k) + k).max(1e-7)
}

/// 几何遮蔽（Smith 方法，直接光照用的 k）。
pub fn geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    geometry_schlick_ggx(n_dot_v, k) * geometry_schlick_ggx(n_dot_l, k)
}

/// 菲涅尔近似（Schlick）。
pub fn fresnel_schlick(cos_theta: f32, f0: Vec3) -> Vec3 {
    let f = (1.0 - cos_theta).clamp(0.0, 1.0).powi(5);
    f0 + (Vec3::ONE - f0) * f
}

/// 按金属度求垂直入射反射率。
pub fn f0(albedo: Vec3, metallic: f32) -> Vec3 {
    Vec3::splat(DIELECTRIC_F0).lerp(albedo, metallic.clamp(0.0, 1.0))
}

/// 一盏直接光源的出射亮度。方向向量均需已归一化。
pub fn direct_lighting(
    n: Vec3,
    v: Vec3,
    l: Vec3,
    albedo: Vec3,
    metallic: f32,
    roughness: f32,
    radiance: Vec3,
) -> Vec3 {
    let h = (v + l).normalize_or_zero();
    let n_dot_v = n.dot(v).max(0.0);
    let n_dot_l = n.dot(l).max(0.0);
    let n_dot_h = n.dot(h).max(0.0);
    let h_dot_v = h.dot(v).max(0.0);

    if n_dot_l <= 0.0 {
        return Vec3::ZERO;
    }

    let f0 = f0(albedo, metallic);
    let d = distribution_ggx(n_dot_h, roughness);
    let g = geometry_smith(n_dot_v, n_dot_l, roughness);
    let f = fresnel_schlick(h_dot_v, f0);

    let specular = f * (d * g) / (4.0 * n_dot_v * n_dot_l).max(1e-7);

    let k_diffuse = (Vec3::ONE - f) * (1.0 - metallic.clamp(0.0, 1.0));
    let diffuse = k_diffuse * albedo / std::f32::consts::PI;

    (diffuse + specular) * radiance * n_dot_l
}

#[cfg(test)]
mod test {
    use super::*;
    use std::f32::consts::{PI, TAU};

    #[test]
    fn ggx_is_non_negative_and_finite() {
        for roughness in [0.0, 0.05, 0.5, 1.0] {
            for i in 0..=20 {
                let n_dot_h = i as f32 / 20.0;
                let d = distribution_ggx(n_dot_h, roughness);
                assert!(d >= 0.0 && d.is_finite(), "D={d} 非法（r={roughness}）");
            }
        }
    }

    #[test]
    fn ggx_peaks_at_normal_incidence() {
        // 微表面法线越接近宏观法线，分布值越大。
        let r = 0.3;
        assert!(distribution_ggx(1.0, r) > distribution_ggx(0.5, r));
        assert!(distribution_ggx(0.5, r) > distribution_ggx(0.0, r));
    }

    #[test]
    fn smoother_surface_concentrates_highlight() {
        // 粗糙度越低，正对方向的分布峰值越高（高光越锐利）。
        assert!(distribution_ggx(1.0, 0.1) > distribution_ggx(1.0, 0.5));
        assert!(distribution_ggx(1.0, 0.5) > distribution_ggx(1.0, 1.0));
    }

    #[test]
    fn ggx_integrates_to_one_over_hemisphere() {
        // NDF 的归一化条件：∫ D(h) (n·h) dω = 1。
        // 这是 GGX 正确性的硬性数学要求，写错系数就会破。
        let roughness = 0.4;
        let steps = 512;
        let mut integral = 0.0;

        for i in 0..steps {
            let theta = (i as f32 + 0.5) / steps as f32 * (PI / 2.0);
            let d_theta = (PI / 2.0) / steps as f32;
            let (sin_theta, cos_theta) = theta.sin_cos();
            // dω = sinθ dθ dφ，绕方位角积分贡献 2π。
            integral += distribution_ggx(cos_theta, roughness) * cos_theta * sin_theta * d_theta * TAU;
        }

        assert!(
            (integral - 1.0).abs() < 0.02,
            "GGX 归一化积分应为 1，实得 {integral}"
        );
    }

    #[test]
    fn geometry_term_stays_within_unit_range() {
        for roughness in [0.0, 0.25, 0.7, 1.0] {
            for i in 1..=10 {
                for j in 1..=10 {
                    let g = geometry_smith(i as f32 / 10.0, j as f32 / 10.0, roughness);
                    assert!((0.0..=1.0).contains(&g), "G={g} 超出 [0,1]");
                }
            }
        }
    }

    #[test]
    fn fresnel_reaches_one_at_grazing_angle() {
        let f0 = Vec3::splat(DIELECTRIC_F0);

        // 掠射角（cosθ→0）时反射率趋近于 1，这是 PBR 观感的关键。
        let grazing = fresnel_schlick(0.0, f0);
        assert!((grazing.x - 1.0).abs() < 1e-5);

        // 垂直入射时等于 F0。
        let normal_incidence = fresnel_schlick(1.0, f0);
        assert!((normal_incidence.x - DIELECTRIC_F0).abs() < 1e-5);
    }

    #[test]
    fn fresnel_is_monotonic() {
        let f0 = Vec3::splat(DIELECTRIC_F0);
        let mut previous = fresnel_schlick(0.0, f0).x;

        for i in 1..=20 {
            let current = fresnel_schlick(i as f32 / 20.0, f0).x;
            assert!(current <= previous + 1e-6, "菲涅尔应随入射角单调递减");
            previous = current;
        }
    }

    #[test]
    fn metal_uses_albedo_as_f0() {
        let albedo = Vec3::new(0.9, 0.7, 0.3); // 类似黄金
        assert_eq!(f0(albedo, 1.0), albedo);
        assert_eq!(f0(albedo, 0.0), Vec3::splat(DIELECTRIC_F0));
    }

    #[test]
    fn metal_has_no_diffuse_contribution() {
        let n = Vec3::Y;
        let v = Vec3::new(0.0, 1.0, 1.0).normalize();
        // 光源方向偏离视线，避开高光瓣，此时纯金属应当近乎不反光。
        let l = Vec3::new(1.0, 0.2, 0.0).normalize();
        let albedo = Vec3::new(1.0, 1.0, 1.0);

        let metal = direct_lighting(n, v, l, albedo, 1.0, 0.35, Vec3::ONE);
        let dielectric = direct_lighting(n, v, l, albedo, 0.0, 0.35, Vec3::ONE);

        // 电介质有漫反射托底，金属没有，所以必然更暗。
        assert!(
            metal.length() < dielectric.length(),
            "金属 {metal:?} 不应亮于电介质 {dielectric:?}"
        );
    }

    #[test]
    fn back_facing_light_contributes_nothing() {
        let n = Vec3::Y;
        let v = Vec3::Y;
        let l = -Vec3::Y; // 光从背面来

        let result = direct_lighting(n, v, l, Vec3::ONE, 0.0, 0.5, Vec3::ONE);

        assert_eq!(result, Vec3::ZERO);
    }

    #[test]
    fn output_is_always_finite() {
        // 边界参数（粗糙度 0、正对入射）最容易产生除零。
        let n = Vec3::Y;
        for roughness in [0.0, 0.001, 1.0] {
            for metallic in [0.0, 1.0] {
                let result =
                    direct_lighting(n, n, n, Vec3::ONE, metallic, roughness, Vec3::ONE);
                assert!(
                    result.is_finite(),
                    "r={roughness} m={metallic} 产生了 NaN/inf：{result:?}"
                );
            }
        }
    }

    #[test]
    fn brdf_obeys_energy_conservation() {
        // 白炉测试的简化版：对半球上的入射方向积分，
        // 出射能量不能超过入射能量，否则表面就在凭空产生光。
        let n = Vec3::Y;
        let v = Vec3::new(0.3, 0.9, 0.0).normalize();
        let albedo = Vec3::ONE;

        for roughness in [0.15, 0.4, 0.8] {
            for metallic in [0.0, 1.0] {
                let steps = 96;
                let mut total = 0.0;

                for i in 0..steps {
                    let theta = (i as f32 + 0.5) / steps as f32 * (PI / 2.0);
                    let d_theta = (PI / 2.0) / steps as f32;
                    let (sin_theta, cos_theta) = theta.sin_cos();

                    for j in 0..steps {
                        let phi = (j as f32 + 0.5) / steps as f32 * TAU;
                        let d_phi = TAU / steps as f32;
                        let (sin_phi, cos_phi) = phi.sin_cos();

                        let l = Vec3::new(sin_theta * cos_phi, cos_theta, sin_theta * sin_phi);
                        let value =
                            direct_lighting(n, v, l, albedo, metallic, roughness, Vec3::ONE);

                        // direct_lighting 已含 n·l，这里只补立体角微元。
                        total += value.x * sin_theta * d_theta * d_phi;
                    }
                }

                assert!(
                    total <= 1.0 + 1e-2,
                    "能量不守恒：r={roughness} m={metallic} 反照率合计 {total}"
                );
            }
        }
    }

    #[test]
    fn brdf_is_reciprocal() {
        // Helmholtz 互易性：交换光源与视线方向，BRDF 值不变。
        // 注意 direct_lighting 含 n·l 因子，比较时要各自除掉。
        let n = Vec3::Y;
        let a = Vec3::new(0.4, 0.8, 0.1).normalize();
        let b = Vec3::new(-0.2, 0.6, 0.7).normalize();
        let albedo = Vec3::new(0.8, 0.8, 0.8);

        let forward = direct_lighting(n, a, b, albedo, 0.3, 0.45, Vec3::ONE) / n.dot(b);
        let backward = direct_lighting(n, b, a, albedo, 0.3, 0.45, Vec3::ONE) / n.dot(a);

        assert!(
            (forward - backward).length() < 1e-4,
            "互易性被破坏：{forward:?} vs {backward:?}"
        );
    }
}
