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

mod bounding;
mod bvh;
mod curve;
mod geometry;
mod primitives;
mod rng;
mod visit;

pub use primitives::{
    Annulus, Arc2d, Bounded2d, Bounded3d, Capsule2d, Capsule3d, Circle, CircularSector,
    CircularSegment, Cone, ConicalFrustum, Cuboid, Cylinder, Ellipse, Rectangle, RegularPolygon,
    Segment2d, Segment3d, ShapeSample, Sphere, Tetrahedron, Torus, Triangle2d, Triangle3d,
};
pub use rng::Rng;

pub use bounding::{Aabb2d, BoundingCircle, Ray2d, Ray3d};
pub use bvh::Bvh;
// 曲线类型一律带 `Cubic` 前缀：`kanim` 里已经有一个 `Curve`（动画通道的
// 关键帧序列），两者在 `kengine::prelude` 里会碰面。
pub use curve::{
    CubicBSpline, CubicBezier, CubicCardinalSpline, CubicCurve, CubicHermite, CubicSegment,
    NotEnoughPoints, Point as CurvePoint,
};
pub use geometry::{Aabb, Intersection, Plane};

// ── Re-exports ────────────────────────────────────────────────────────────────
pub use glam;
pub use glam::{
    // 矩阵
    Affine2,
    Affine3A,
    // 2-D
    BVec2,
    BVec3,
    BVec4,
    DMat2,
    DMat3,
    DMat4,
    DQuat,
    // 双精度
    DVec2,
    DVec3,
    DVec4,
    // 欧拉角的轴序。`Quat::from_euler` / `to_euler` 的第一个参数，
    // 少了它那两个函数在外面就没法调。
    EulerRot,
    IVec2,
    IVec3,
    IVec4,
    Mat2,
    Mat3,
    Mat3A,
    Mat4,
    // 旋转
    Quat,
    UVec2,
    UVec3,
    UVec4,
    Vec2,
    Vec3,
    Vec3A,
    Vec4,
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

/// 帧率无关的指数逼近系数。
///
/// # 为什么需要它
///
/// 相机跟随、数值缓动最常见的写法是 `current = lerp(current, target, 0.1)`。
/// 这行代码**跟着帧率走**：60 帧下每秒逼近 60 次，144 帧下逼近 144 次，
/// 于是同一个游戏在高刷屏上跟得更紧。改成乘 `dt` 也不对——那只是让它
/// 从「每帧固定比例」变成「每帧固定比例乘时间」，仍然不是指数曲线。
///
/// 正确的系数是 `1 - e^(-decay·dt)`：不管一帧多长，走完同样的时间就
/// 逼近同样的比例。`decay` 是「每秒的急迫程度」，越大跟得越紧；
/// 常用范围 2~20。
///
/// ```
/// # use kmath::{exp_decay, lerp};
/// // 同样的 0.1 秒，拆成一大步还是若干小步，结果几乎一样。
/// let one_step = lerp(0.0, 10.0, exp_decay(8.0, 0.1));
///
/// let mut many = 0.0_f32;
/// for _ in 0..10 {
///     many = lerp(many, 10.0, exp_decay(8.0, 0.01));
/// }
///
/// assert!((one_step - many).abs() < 0.05);
/// ```
#[inline]
pub fn exp_decay(decay: f32, dt: f32) -> f32 {
    if !(decay.is_finite() && dt.is_finite()) || dt <= 0.0 {
        return 0.0;
    }
    1.0 - (-decay * dt).exp()
}

/// 帧率无关地把 `current` 朝 `target` 推一步。
///
/// 就是 [`lerp`] 配上 [`exp_decay`] 的系数，见那里的说明。
#[inline]
pub fn smooth_nudge(current: f32, target: f32, decay: f32, dt: f32) -> f32 {
    lerp(current, target, exp_decay(decay, dt))
}

/// [`smooth_nudge`] 的三维版本。相机跟随最常用的那个。
#[inline]
pub fn smooth_nudge_vec3(current: Vec3, target: Vec3, decay: f32, dt: f32) -> Vec3 {
    current.lerp(target, exp_decay(decay, dt))
}

/// [`smooth_nudge`] 的二维版本。
#[inline]
pub fn smooth_nudge_vec2(current: Vec2, target: Vec2, decay: f32, dt: f32) -> Vec2 {
    current.lerp(target, exp_decay(decay, dt))
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
    Vec3::new(
        srgb_to_linear(c.x),
        srgb_to_linear(c.y),
        srgb_to_linear(c.z),
    )
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
        Affine2,
        Affine3A,
        BVec2,
        BVec3,
        BVec4,
        DMat2,
        DMat3,
        DMat4,
        DQuat,
        DVec2,
        DVec3,
        DVec4,
        EulerRot,
        FRAC_PI_2,
        FRAC_PI_4,
        IVec2,
        IVec3,
        IVec4,
        Mat2,
        Mat3,
        Mat3A,
        Mat4,
        // 常量
        PI,
        Quat,
        TAU,
        UVec2,
        UVec3,
        UVec4,
        Vec2,
        Vec3,
        Vec3A,
        Vec4,
        // 函数
        deg_to_rad,
        exp_decay,
        fast_inv_sqrt,
        lerp,
        lerp_clamped,
        lerp_vec3,
        lerp_vec3_clamped,
        linear_to_srgb,
        project_onto_plane,
        rad_to_deg,
        reflect,
        refract,
        remap,
        rotate_around_point,
        scale_around_point,
        slerp,
        smooth_nudge,
        smooth_nudge_vec2,
        smooth_nudge_vec3,
        smootherstep,
        smoothstep,
        srgb_to_linear,
        srgb_vec3_to_linear,
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
    fn exp_decay_is_frame_rate_independent() {
        // 这正是它存在的理由：同样的时间跨度，拆成一大步还是许多小步，
        // 结果必须几乎一样。用 `lerp(cur, target, 0.1)` 那种写法的话，
        // 60 帧和 144 帧下跟随速度会差一大截。
        let big = lerp(0.0, 100.0, exp_decay(6.0, 0.5));

        let mut small = 0.0_f32;
        for _ in 0..50 {
            small = lerp(small, 100.0, exp_decay(6.0, 0.01));
        }

        assert!((big - small).abs() < 0.5, "一大步 {big}，许多小步 {small}");
    }

    #[test]
    fn exp_decay_never_overshoots() {
        // 系数必须落在 [0, 1]：超过 1 会冲过头再弹回来。
        //
        // 取到 1 是允许的，而且是对的——一帧长到 100 秒时本来就该
        // 直接到达目标（`1 - e^-2000` 在 f32 下正是 1.0）。
        for dt in [0.0, 0.001, 0.016, 1.0, 100.0] {
            let k = exp_decay(20.0, dt);
            assert!((0.0..=1.0).contains(&k), "dt={dt} 给出了 {k}");
        }
    }

    #[test]
    fn exp_decay_survives_a_bad_dt() {
        // 第一帧的 dt 有时是 0 或者奇怪的值。返回 0 表示「这一帧不动」，
        // 比让 NaN 顺着位置传下去强——那会让相机再也回不来。
        assert_eq!(exp_decay(5.0, f32::NAN), 0.0);
        assert_eq!(exp_decay(f32::NAN, 0.016), 0.0);
        assert_eq!(exp_decay(5.0, -1.0), 0.0);
    }

    #[test]
    fn smooth_nudge_approaches_but_does_not_pass() {
        let mut value = 0.0_f32;
        for _ in 0..600 {
            value = smooth_nudge(value, 10.0, 8.0, 1.0 / 60.0);
            assert!(value <= 10.0, "冲过头了：{value}");
        }
        assert!((value - 10.0).abs() < 0.01, "十秒之后还差 {}", 10.0 - value);
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
