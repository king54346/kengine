//! glam 0.29（kmath）↔ glam 0.33（rapier2d）的类型转换。
//!
//! 和 3D 那边 [`crate::convert`] 同理：两边都是同名不同版本的 glam，
//! 在 Rust 里是两个互不相干的类型，得转一道。
//!
//! 2D 的旋转是**一个标量角度**（弧度），不是四元数——这是 2D 和 3D
//! 最显眼的差别，也是这个模块不能靠泛型和 3D 共用一份代码的原因之一。

use kmath::Vec2;
use rapier2d::math::{Pose, Rotation, Vector};

/// kmath 的向量 → rapier 的向量。
#[inline]
pub(crate) fn to_rv(v: Vec2) -> Vector {
    Vector::new(v.x, v.y)
}

/// rapier 的向量 → kmath 的向量。
#[inline]
pub(crate) fn from_rv(v: Vector) -> Vec2 {
    Vec2::new(v.x, v.y)
}

/// 弧度 → rapier 的旋转。
#[inline]
pub(crate) fn to_rr(angle: f32) -> Rotation {
    Rotation::from_angle(angle)
}

/// rapier 的旋转 → 弧度。
#[inline]
pub(crate) fn from_rr(rotation: Rotation) -> f32 {
    rotation.angle()
}

/// 位置 + 角度 → rapier 的位姿。
#[inline]
pub(crate) fn to_rp(position: Vec2, angle: f32) -> Pose {
    Pose::from_parts(to_rv(position), to_rr(angle))
}

/// rapier 的位姿 → 位置 + 角度。
#[inline]
pub(crate) fn from_rp(pose: &Pose) -> (Vec2, f32) {
    (from_rv(pose.translation), from_rr(pose.rotation))
}

#[cfg(test)]
mod test {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    #[test]
    fn vector_roundtrip_is_exact() {
        // 两边都是紧凑 f32，来回一趟不该有任何位差。
        let v = Vec2::new(1.5, -2.25);
        assert_eq!(from_rv(to_rv(v)), v);
    }

    #[test]
    fn angle_roundtrip_preserves_the_rotation() {
        for angle in [0.0, 0.3, FRAC_PI_2, -1.2, PI - 0.01] {
            let back = from_rr(to_rr(angle));
            assert!((back - angle).abs() < 1e-5, "{angle} → {back}");
        }
    }

    #[test]
    fn the_angle_is_wrapped_to_the_principal_range() {
        // rapier 存的是 (cos, sin)，取回来的角度必然在 (-π, π]。
        // 转了三圈的角度取回来不会还是三圈——调用方拿它累加的话
        // 会得到错的结果，所以这条得写明白。
        let three_turns = 6.0 * PI + 0.5;
        let back = from_rr(to_rr(three_turns));
        assert!(back.abs() <= PI + 1e-5, "没有归一化到主区间：{back}");
        assert!(
            (back - 0.5).abs() < 1e-4,
            "归一化后该等价于 0.5，实测 {back}"
        );
    }

    #[test]
    fn rotation_conversion_keeps_the_rotation_it_describes() {
        // 角度相等还不够：真正要保的是「转出来的向量一样」。
        let angle = 0.7_f32;
        let v = Vec2::new(0.3, -0.7);
        let (sin, cos) = angle.sin_cos();
        let expected = Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos);

        let actual = from_rv(to_rr(angle).transform_vector(to_rv(v)));
        assert!(
            (actual - expected).length() < 1e-5,
            "{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn pose_roundtrip() {
        let (position, angle) = (Vec2::new(3.0, -4.0), 0.9_f32);
        let (p, a) = from_rp(&to_rp(position, angle));
        assert!((p - position).length() < 1e-5);
        assert!((a - angle).abs() < 1e-5);
    }
}
