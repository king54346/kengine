//! kmath — kengine 数学库
//!
//! 基于 [glam](https://docs.rs/glam) 提供游戏引擎常用数学工具。
//!
//! # 快速上手
//! ```
//! use kmath::prelude::*;
//!
//! let t = lerp(0.0_f32, 10.0, 0.5);
//! assert!((t - 5.0).abs() < 1e-6);
//! ```

mod geometry;

pub use geometry::{Aabb, Plane};

// ── Re-exports ────────────────────────────────────────────────────────────────
pub use glam;
pub use glam::{
    // 2-D
    BVec2, BVec3, BVec4,
    IVec2, IVec3, IVec4,
    UVec2, UVec3, UVec4,
    Vec2, Vec3, Vec3A, Vec4,
    // 矩阵
    Affine2, Affine3A,
    Mat2, Mat3, Mat3A, Mat4,
    // 旋转
    Quat,
    // 双精度
    DVec2, DVec3, DVec4,
    DMat2, DMat3, DMat4,
    DQuat,
};

// ── 常量 ──────────────────────────────────────────────────────────────────────
/// π（f32）
pub const PI: f32 = std::f32::consts::PI;
/// 2π（f32）
pub const TAU: f32 = std::f32::consts::TAU;
/// π/2（f32）
pub const FRAC_PI_2: f32 = std::f32::consts::FRAC_PI_2;
/// π/4（f32）
pub const FRAC_PI_4: f32 = std::f32::consts::FRAC_PI_4;

// ── 角度转换 ──────────────────────────────────────────────────────────────────
/// 角度 → 弧度
#[inline(always)]
pub fn deg_to_rad(deg: f32) -> f32 {
    deg.to_radians()
}

/// 弧度 → 角度
#[inline(always)]
pub fn rad_to_deg(rad: f32) -> f32 {
    rad.to_degrees()
}

// ── 插值 ──────────────────────────────────────────────────────────────────────
/// 线性插值（不限定 `t` 范围）
#[inline(always)]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 线性插值，`t` 钳制到 [0, 1]
#[inline(always)]
pub fn lerp_clamped(a: f32, b: f32, t: f32) -> f32 {
    lerp(a, b, t.clamp(0.0, 1.0))
}

/// 向量线性插值
#[inline(always)]
pub fn lerp_vec3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a.lerp(b, t)
}

/// 向量线性插值（钳制 t）
#[inline(always)]
pub fn lerp_vec3_clamped(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a.lerp(b, t.clamp(0.0, 1.0))
}

/// 四元数球面线性插值（SLERP）
#[inline(always)]
pub fn slerp(a: Quat, b: Quat, t: f32) -> Quat {
    a.slerp(b, t)
}

// ── 平滑步函数 ────────────────────────────────────────────────────────────────
/// Hermite 平滑步：将 `x` 从 `[edge0, edge1]` 映射到 `[0, 1]` 并施加缓入缓出
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 更平滑的五次方版本（Ken Perlin）
#[inline]
pub fn smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

// ── 钳制 / 映射 ───────────────────────────────────────────────────────────────
/// 将 `value` 从 `[in_min, in_max]` 线性映射到 `[out_min, out_max]`
#[inline]
pub fn remap(value: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32) -> f32 {
    out_min + (value - in_min) / (in_max - in_min) * (out_max - out_min)
}

// ── 碰撞 / 几何辅助 ───────────────────────────────────────────────────────────
/// 计算给定方向（归一化）的反射向量
///
/// `normal` 必须是单位向量。
#[inline(always)]
pub fn reflect(incident: Vec3, normal: Vec3) -> Vec3 {
    incident - 2.0 * incident.dot(normal) * normal
}

/// 折射向量（Snell 定律）
///
/// `incident` 与 `normal` 均需归一化；`eta = n1/n2`（入射介质折射率 / 折射介质折射率）。
/// 返回 `None` 表示全内反射。
#[inline]
pub fn refract(incident: Vec3, normal: Vec3, eta: f32) -> Option<Vec3> {
    let n_dot_i = normal.dot(incident);
    let k = 1.0 - eta * eta * (1.0 - n_dot_i * n_dot_i);
    if k < 0.0 {
        None
    } else {
        Some(eta * incident - (eta * n_dot_i + k.sqrt()) * normal)
    }
}

/// 将方向向量投影到平面（由法线定义的平面）
#[inline(always)]
pub fn project_onto_plane(v: Vec3, plane_normal: Vec3) -> Vec3 {
    v - v.dot(plane_normal) * plane_normal
}

// ── 变换辅助 ──────────────────────────────────────────────────────────────────
/// 以给定点为中心缩放的矩阵
#[inline]
pub fn scale_around_point(scale: Vec3, pivot: Vec3) -> Mat4 {
    Mat4::from_translation(pivot) * Mat4::from_scale(scale) * Mat4::from_translation(-pivot)
}

/// 以给定轴和角度旋转（弧度），以 `pivot` 为旋转中心
#[inline]
pub fn rotate_around_point(axis: Vec3, angle_rad: f32, pivot: Vec3) -> Mat4 {
    Mat4::from_translation(pivot)
        * Mat4::from_axis_angle(axis, angle_rad)
        * Mat4::from_translation(-pivot)
}

// ── 颜色辅助 ────────────────────────────────────────────────────────────────���─
/// sRGB → 线性（Gamma 解码，单通道）
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 线性 → sRGB（Gamma 编码，单通道）
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB Vec3 → 线性 Vec3
#[inline]
pub fn srgb_vec3_to_linear(c: Vec3) -> Vec3 {
    Vec3::new(srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z))
}

// ── 快速近似 ──────────────────────────────────────────────────────────────────
/// 快速平方根倒数（Quake III 算法）
///
/// 精度约 0.175% 误差，适合对性能敏感但不需要完全精确的场景。
#[inline]
pub fn fast_inv_sqrt(x: f32) -> f32 {
    let x2 = x * 0.5;
    let mut i: u32 = x.to_bits();
    i = 0x5f37_59df - (i >> 1);
    let y = f32::from_bits(i);
    y * (1.5 - x2 * y * y) // 一次 Newton-Raphson 迭代
}

// ── Prelude ───────────────────────────────────────────────────────────────────
pub mod prelude {
    pub use super::{
        // glam 主要类型
        Affine2, Affine3A, BVec2, BVec3, BVec4,
        DVec2, DVec3, DVec4, DMat2, DMat3, DMat4, DQuat,
        IVec2, IVec3, IVec4, UVec2, UVec3, UVec4,
        Mat2, Mat3, Mat3A, Mat4, Quat,
        Vec2, Vec3, Vec3A, Vec4,
        // 常量
        PI, TAU, FRAC_PI_2, FRAC_PI_4,
        // 函数
        deg_to_rad, rad_to_deg,
        lerp, lerp_clamped, lerp_vec3, lerp_vec3_clamped, slerp,
        smoothstep, smootherstep, remap,
        reflect, refract, project_onto_plane,
        scale_around_point, rotate_around_point,
        srgb_to_linear, linear_to_srgb, srgb_vec3_to_linear,
        fast_inv_sqrt,
    };
}

// ── 测试 ──────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lerp() {
        assert!((lerp(0.0, 10.0, 0.5) - 5.0).abs() < 1e-6);
        assert!((lerp(0.0, 10.0, 0.0) - 0.0).abs() < 1e-6);
        assert!((lerp(0.0, 10.0, 1.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn test_smoothstep() {
        assert!((smoothstep(0.0, 1.0, 0.0) - 0.0).abs() < 1e-6);
        assert!((smoothstep(0.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_deg_rad() {
        assert!((deg_to_rad(180.0) - PI).abs() < 1e-5);
        assert!((rad_to_deg(PI) - 180.0).abs() < 1e-3);
    }

    #[test]
    fn test_remap() {
        assert!((remap(5.0, 0.0, 10.0, 0.0, 1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_srgb_linear_roundtrip() {
        let v = 0.5_f32;
        let linear = srgb_to_linear(v);
        let back = linear_to_srgb(linear);
        assert!((back - v).abs() < 1e-5);
    }

    #[test]
    fn test_fast_inv_sqrt() {
        let x = 4.0_f32;
        let expected = 1.0 / x.sqrt(); // 0.5
        let actual = fast_inv_sqrt(x);
        assert!((actual - expected).abs() < 0.01);
    }

    #[test]
    fn test_reflect() {
        let incident = Vec3::new(1.0, -1.0, 0.0).normalize();
        let normal = Vec3::Y;
        let r = reflect(incident, normal);
        // 反射后 y 分量应为正
        assert!(r.y > 0.0);
    }
}
