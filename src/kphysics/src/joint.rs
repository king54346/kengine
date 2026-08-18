//! 关节：把两个刚体按某种自由度约束在一起。
//!
//! 关节的说法统一是「锁掉哪些自由度」。六个自由度（三个平移 + 三个旋转）
//! 全锁上就是固定关节，只放开一个旋转轴就是铰链，如此类推。
//! 这也正是 rapier 内部的表示法，本模块只是给常见组合起了名字。

use crate::convert::{to_rp, to_rv};
use kmath::{Quat, Vec3};
use rapier3d::dynamics as rd;

/// 球窝关节在三个旋转轴上的活动范围，单位是弧度。
///
/// `None` 表示该轴不限位。布娃娃的关节几乎都要限位——不限的话
/// 胳膊会向反方向折过去，看起来比不做物理还糟。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SphericalLimits {
    /// 绕 X 轴（关节局部空间）的 `[最小, 最大]`。
    pub x: Option<[f32; 2]>,
    /// 绕 Y 轴的 `[最小, 最大]`。
    pub y: Option<[f32; 2]>,
    /// 绕 Z 轴的 `[最小, 最大]`。
    pub z: Option<[f32; 2]>,
}

impl SphericalLimits {
    /// 三个轴都限制在同一个对称范围内。
    pub fn symmetric(half_angle: f32) -> Self {
        let range = [-half_angle, half_angle];
        Self {
            x: Some(range),
            y: Some(range),
            z: Some(range),
        }
    }
}

/// 关节类型。
#[derive(Debug, Clone, PartialEq)]
pub enum JointKind {
    /// 固定：六个自由度全锁。焊死两个刚体。
    Fixed,
    /// 球窝：只锁平移，三个旋转轴放开（可限位）。肩、髋用这个。
    Spherical {
        /// 三轴限位。
        limits: SphericalLimits,
    },
    /// 铰链：只放开绕 `axis` 的旋转。门、轮子、肘、膝用这个。
    Revolute {
        /// 转轴，关节局部空间。
        axis: Vec3,
        /// `[最小, 最大]` 转角，弧度。
        limits: Option<[f32; 2]>,
    },
    /// 滑轨：只放开沿 `axis` 的平移。活塞、抽屉用这个。
    Prismatic {
        /// 滑动方向，关节局部空间。
        axis: Vec3,
        /// `[最小, 最大]` 位移。
        limits: Option<[f32; 2]>,
    },
}

/// 建一个关节所需的参数。
///
/// 两侧的「局部坐标系」是关节的关键：它描述关节点在各自刚体的局部空间里
/// 落在哪、朝向如何。求解器要做的就是让这两个坐标系在被锁的自由度上重合。
#[derive(Debug, Clone, PartialEq)]
pub struct JointDesc {
    /// 关节类型。
    pub kind: JointKind,
    /// 关节点在刚体 1 局部空间的位置。
    pub local_anchor1: Vec3,
    /// 关节点在刚体 2 局部空间的位置。
    pub local_anchor2: Vec3,
    /// 关节坐标系相对刚体 1 的朝向。
    pub local_basis1: Quat,
    /// 关节坐标系相对刚体 2 的朝向。
    pub local_basis2: Quat,
    /// 被关节连起来的两个刚体之间是否还做碰撞检测。
    ///
    /// 默认关闭：相连的两截肢体在关节处几乎必然互相插入，
    /// 开着的话求解器会一边拉住它们、一边把它们弹开，抖个不停。
    pub contacts_enabled: bool,
}

impl Default for JointDesc {
    fn default() -> Self {
        Self {
            kind: JointKind::Fixed,
            local_anchor1: Vec3::ZERO,
            local_anchor2: Vec3::ZERO,
            local_basis1: Quat::IDENTITY,
            local_basis2: Quat::IDENTITY,
            contacts_enabled: false,
        }
    }
}

impl JointDesc {
    /// 固定关节。
    pub fn fixed(anchor1: Vec3, anchor2: Vec3) -> Self {
        Self {
            kind: JointKind::Fixed,
            local_anchor1: anchor1,
            local_anchor2: anchor2,
            ..Self::default()
        }
    }

    /// 球窝关节。
    pub fn spherical(anchor1: Vec3, anchor2: Vec3, limits: SphericalLimits) -> Self {
        Self {
            kind: JointKind::Spherical { limits },
            local_anchor1: anchor1,
            local_anchor2: anchor2,
            ..Self::default()
        }
    }

    /// 铰链关节。
    pub fn revolute(anchor1: Vec3, anchor2: Vec3, axis: Vec3, limits: Option<[f32; 2]>) -> Self {
        Self {
            kind: JointKind::Revolute { axis, limits },
            local_anchor1: anchor1,
            local_anchor2: anchor2,
            ..Self::default()
        }
    }

    /// 滑轨关节。
    pub fn prismatic(anchor1: Vec3, anchor2: Vec3, axis: Vec3, limits: Option<[f32; 2]>) -> Self {
        Self {
            kind: JointKind::Prismatic { axis, limits },
            local_anchor1: anchor1,
            local_anchor2: anchor2,
            ..Self::default()
        }
    }

    /// 打开两端刚体之间的碰撞检测。
    pub fn with_contacts(mut self) -> Self {
        self.contacts_enabled = true;
        self
    }

    /// 指定两侧关节坐标系的朝向。
    pub fn with_bases(mut self, basis1: Quat, basis2: Quat) -> Self {
        self.local_basis1 = basis1;
        self.local_basis2 = basis2;
        self
    }

    pub(crate) fn build(&self) -> rd::GenericJoint {
        use rd::{JointAxesMask as Mask, JointAxis as Ax};

        // 铰链与滑轨在 rapier 里恒定以关节坐标系的 **X 轴**为自由轴，
        // 所以要先把用户给的轴转到 X 上，再叠加用户自己的朝向。
        let (locked, axis_align) = match &self.kind {
            JointKind::Fixed => (Mask::LOCKED_FIXED_AXES, Quat::IDENTITY),
            JointKind::Spherical { .. } => (Mask::LOCKED_SPHERICAL_AXES, Quat::IDENTITY),
            JointKind::Revolute { axis, .. } => (
                Mask::LOCKED_REVOLUTE_AXES,
                Self::align_x_to(*axis),
            ),
            JointKind::Prismatic { axis, .. } => (
                Mask::LOCKED_PRISMATIC_AXES,
                Self::align_x_to(*axis),
            ),
        };

        let mut joint = rd::GenericJointBuilder::new(locked)
            .local_frame1(to_rp(self.local_anchor1, self.local_basis1 * axis_align))
            .local_frame2(to_rp(self.local_anchor2, self.local_basis2 * axis_align))
            .contacts_enabled(self.contacts_enabled)
            .build();

        match &self.kind {
            JointKind::Fixed => {}
            JointKind::Spherical { limits } => {
                for (axis, limit) in [
                    (Ax::AngX, limits.x),
                    (Ax::AngY, limits.y),
                    (Ax::AngZ, limits.z),
                ] {
                    if let Some(limit) = limit {
                        joint.set_limits(axis, limit);
                    }
                }
            }
            JointKind::Revolute { limits, .. } => {
                if let Some(limits) = limits {
                    joint.set_limits(Ax::AngX, *limits);
                }
            }
            JointKind::Prismatic { limits, .. } => {
                if let Some(limits) = limits {
                    joint.set_limits(Ax::LinX, *limits);
                }
            }
        }

        joint
    }

    /// 构造一个把局部 X 轴旋到 `axis` 的四元数。轴退化时退回单位四元数。
    fn align_x_to(axis: Vec3) -> Quat {
        let axis = axis.normalize_or_zero();
        if axis == Vec3::ZERO {
            return Quat::IDENTITY;
        }
        // rapier 自带这个换算，直接借用可以保证与它内部的 frame 约定一致。
        crate::convert::from_rq(rd::GenericJoint::complete_ang_frame(to_rv(axis)))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use rapier3d::dynamics::{JointAxesMask as Mask, JointAxis as Ax};

    #[test]
    fn each_kind_locks_the_expected_axes() {
        assert_eq!(
            JointDesc::fixed(Vec3::ZERO, Vec3::ZERO).build().locked_axes,
            Mask::LOCKED_FIXED_AXES
        );
        assert_eq!(
            JointDesc::spherical(Vec3::ZERO, Vec3::ZERO, SphericalLimits::default())
                .build()
                .locked_axes,
            Mask::LOCKED_SPHERICAL_AXES
        );
        assert_eq!(
            JointDesc::revolute(Vec3::ZERO, Vec3::ZERO, Vec3::Y, None)
                .build()
                .locked_axes,
            Mask::LOCKED_REVOLUTE_AXES
        );
        assert_eq!(
            JointDesc::prismatic(Vec3::ZERO, Vec3::ZERO, Vec3::Y, None)
                .build()
                .locked_axes,
            Mask::LOCKED_PRISMATIC_AXES
        );
    }

    #[test]
    fn revolute_axis_ends_up_on_the_joint_frame_x() {
        // 这是最容易错的一处：给了 Y 轴却绕 X 转，门会朝着奇怪的方向开。
        let joint = JointDesc::revolute(Vec3::ZERO, Vec3::ZERO, Vec3::Y, None).build();
        let axis = crate::convert::from_rv(joint.local_axis1());

        assert!((axis - Vec3::Y).length() < 1e-5, "实际轴 {axis:?}");
    }

    #[test]
    fn prismatic_axis_ends_up_on_the_joint_frame_x() {
        let joint = JointDesc::prismatic(Vec3::ZERO, Vec3::ZERO, Vec3::Z, None).build();
        let axis = crate::convert::from_rv(joint.local_axis1());

        assert!((axis - Vec3::Z).length() < 1e-5, "实际轴 {axis:?}");
    }

    #[test]
    fn anchors_land_on_the_matching_side() {
        let joint = JointDesc::fixed(Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 2.0, 0.0)).build();

        assert_eq!(
            crate::convert::from_rv(joint.local_anchor1()),
            Vec3::new(1.0, 0.0, 0.0)
        );
        assert_eq!(
            crate::convert::from_rv(joint.local_anchor2()),
            Vec3::new(0.0, 2.0, 0.0)
        );
    }

    #[test]
    fn spherical_limits_apply_per_axis() {
        let limits = SphericalLimits {
            x: Some([-0.5, 0.5]),
            y: None,
            z: Some([0.0, 1.0]),
        };
        let joint = JointDesc::spherical(Vec3::ZERO, Vec3::ZERO, limits).build();

        assert!(joint.limits(Ax::AngX).is_some());
        assert!(joint.limits(Ax::AngY).is_none());
        let z = joint.limits(Ax::AngZ).unwrap();
        assert_eq!([z.min, z.max], [0.0, 1.0]);
    }

    #[test]
    fn contacts_are_off_by_default() {
        // 相连的两截肢体几乎必然互相插入，默认开着会抖。
        assert!(!JointDesc::fixed(Vec3::ZERO, Vec3::ZERO).build().contacts_enabled());
        assert!(
            JointDesc::fixed(Vec3::ZERO, Vec3::ZERO)
                .with_contacts()
                .build()
                .contacts_enabled()
        );
    }

    #[test]
    fn degenerate_axis_falls_back_to_identity() {
        // 零向量做转轴是调用方的错，但不该 panic 或产生 NaN 关节。
        let joint = JointDesc::revolute(Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, None).build();
        assert!(joint.local_frame1.rotation.is_finite());
    }

    #[test]
    fn symmetric_limits_cover_all_three_axes() {
        let limits = SphericalLimits::symmetric(0.4);
        assert_eq!(limits.x, Some([-0.4, 0.4]));
        assert_eq!(limits.y, Some([-0.4, 0.4]));
        assert_eq!(limits.z, Some([-0.4, 0.4]));
    }
}
