//! glam 0.29（kmath）↔ glam 0.33（rapier）的类型转换。
//!
//! rapier 0.35 已经从 nalgebra 迁到 glam，公开的数学类型就是 `glam::Vec3` /
//! `glam::Quat`——但它锁的是 glam **0.33**，而本项目其余部分用的是 **0.29**。
//! 同名不同版本的 crate 在 Rust 里是两个互不相干的类型，所以还得转一道。
//!
//! 好在两边的内存布局都是紧凑的 `f32`，转换退化成按分量搬运，优化后基本不留痕迹。
//! 真正的价值在于**这一道墙**：rapier 的类型到此为止，`kphysics` 对外只说 kmath。

use kmath::{Mat4, Quat, Vec3};
use rapier3d::math::{Pose, Rotation, Vector};

/// kmath 的向量 → rapier 的向量。
#[inline]
pub(crate) fn to_rv(v: Vec3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}

/// rapier 的向量 → kmath 的向量。
#[inline]
pub(crate) fn from_rv(v: Vector) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

/// kmath 的四元数 → rapier 的旋转。
#[inline]
pub(crate) fn to_rq(q: Quat) -> Rotation {
    Rotation::from_xyzw(q.x, q.y, q.z, q.w)
}

/// rapier 的旋转 → kmath 的四元数。
#[inline]
pub(crate) fn from_rq(q: Rotation) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// 位置 + 朝向 → rapier 的位姿。
#[inline]
pub(crate) fn to_rp(position: Vec3, rotation: Quat) -> Pose {
    Pose::from_parts(to_rv(position), to_rq(rotation))
}

/// rapier 的位姿 → 位置 + 朝向。
#[inline]
pub(crate) fn from_rp(pose: &Pose) -> (Vec3, Quat) {
    (from_rv(pose.translation), from_rq(pose.rotation))
}

/// 从 4×4 变换矩阵里取出刚体位姿，**丢弃缩放**。（内部用，公开版见 [`crate::pose_from_matrix`]）
///
/// 物理引擎里没有「被缩放的刚体」这回事——碰撞体的尺寸是形状自己的参数。
/// 场景节点上的缩放到这里必须被剥掉，否则 `to_scale_rotation_translation`
/// 分解出来的旋转会被缩放污染。Fyrox 的 `isometry_from_global_transform` 同理。
#[inline]
pub(crate) fn pose_from_mat4(m: Mat4) -> Pose {
    let (_scale, rotation, translation) = m.to_scale_rotation_translation();
    to_rp(translation, rotation)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn vector_roundtrip_is_exact() {
        // 两边都是紧凑 f32，来回一趟不该有任何位差。
        let v = Vec3::new(1.5, -2.25, 3.125);
        assert_eq!(from_rv(to_rv(v)), v);
    }

    #[test]
    fn quaternion_roundtrip_preserves_components() {
        // 分量顺序（xyzw）搞反是这类转换最典型的错法，逐分量比对才拦得住。
        let q = Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), 0.7).normalize();
        let back = from_rq(to_rq(q));

        assert!((back.x - q.x).abs() < 1e-6);
        assert!((back.y - q.y).abs() < 1e-6);
        assert!((back.z - q.z).abs() < 1e-6);
        assert!((back.w - q.w).abs() < 1e-6);
    }

    #[test]
    fn quaternion_conversion_keeps_the_rotation_it_describes() {
        // 逐分量相等还不够：真正要保的是「转出来的向量一样」。
        let q = Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 1.1);
        let v = Vec3::new(0.3, -0.7, 2.0);

        let expected = q * v;
        let actual = from_rv(to_rq(q) * to_rv(v));

        assert!(
            (actual - expected).length() < 1e-5,
            "{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn pose_from_mat4_discards_scale() {
        let rotation = Quat::from_axis_angle(Vec3::Y, 0.5);
        let translation = Vec3::new(4.0, 5.0, 6.0);
        let m = Mat4::from_scale_rotation_translation(Vec3::splat(3.0), rotation, translation);

        let (p, r) = from_rp(&pose_from_mat4(m));

        assert!((p - translation).length() < 1e-5);
        // 缩放若泄漏进来，旋转会不再是单位四元数。
        assert!((r.length() - 1.0).abs() < 1e-5);
        assert!((r * Vec3::X - rotation * Vec3::X).length() < 1e-5);
    }

    #[test]
    fn pose_from_mat4_survives_non_uniform_scale() {
        // 非均匀缩放下分解是近似的，但平移必须精确，旋转必须仍是单位四元数。
        let m = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 2.0, 4.0),
            Quat::from_axis_angle(Vec3::Z, 0.25),
            Vec3::new(-1.0, 0.0, 2.0),
        );

        let (p, r) = from_rp(&pose_from_mat4(m));

        assert!((p - Vec3::new(-1.0, 0.0, 2.0)).length() < 1e-5);
        assert!((r.length() - 1.0).abs() < 1e-4);
    }
}
