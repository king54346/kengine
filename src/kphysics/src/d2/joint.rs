//! 2D 关节：把两个刚体按某种约束连起来。
//!
//! # 和 3D 那套的差别
//!
//! 少一种、多一种，都是维度带来的：
//!
//! - **没有球铰**。球铰是「位置锁死、三个转轴都自由」，而 2D 一共只有一个
//!   转轴——那就是铰链（[`revolute`](JointDesc::revolute)）本身。
//! - **铰链不需要给轴**。2D 的旋转轴恒定垂直于平面，没有第二种选择；
//!   3D 那边要传的 `axis` 在这里根本无从谈起。
//! - **多了绳索**（[`rope`](JointDesc::rope)）：只限制最大距离，绳子松着的
//!   时候两端互不影响。3D 那边也有这个约束，只是当时没接。
//!
//! # 三种典型用法
//!
//! | 要做什么 | 用哪个 |
//! |---|---|
//! | 车轮 | [`revolute`](JointDesc::revolute)，锚点在轮心 |
//! | 绳索 / 锁链 | 一串 [`rope`](JointDesc::rope)，或一串带限位的铰链 |
//! | 布娃娃的关节 | [`revolute`](JointDesc::revolute) 加角度限位，别让肘部反折 |

use super::convert::to_rp;
use kmath::Vec2;
use rapier2d::dynamics as rd;

/// 关节的种类。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointKind {
    /// 完全固定：相对位置与朝向都锁死。
    ///
    /// 用来把几块拼成一个刚体的替代品——比直接做一个复合碰撞体更灵活，
    /// 因为随时可以断开。
    Fixed,
    /// 铰链：位置锁死，可以自由转动。车轮、钟摆、布娃娃的关节。
    Revolute {
        /// 转角限位（弧度），`None` 表示能转满一圈。
        ///
        /// 布娃娃全靠它：不给限位的话肘和膝会朝着两边任意反折。
        limits: Option<[f32; 2]>,
    },
    /// 滑轨：只能沿一个方向平移，不能转。电梯、活塞、推拉门。
    Prismatic {
        /// 滑动方向（局部空间）。
        axis: Vec2,
        /// 行程限位，`None` 表示无限长。
        limits: Option<[f32; 2]>,
    },
    /// 绳索：只限制**最大距离**。
    ///
    /// 和上面几种的根本区别是它**只在绷紧时起作用**——松着的时候两端
    /// 各走各的。吊桥、锁链、抓钩都是这个。
    Rope {
        /// 最大距离。
        max_distance: f32,
    },
}

/// 一个关节的描述。
///
/// 两个锚点分别是**各自刚体的局部坐标**。以车轮为例：车身那侧的锚点在
/// 轮子该装的位置，轮子那侧的锚点在它自己的圆心（也就是零）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointDesc {
    /// 种类。
    pub kind: JointKind,
    /// 第一个刚体上的锚点（局部坐标）。
    pub local_anchor1: Vec2,
    /// 第二个刚体上的锚点（局部坐标）。
    pub local_anchor2: Vec2,
    /// 被连起来的两个刚体之间还要不要做碰撞检测。
    ///
    /// 默认**关闭**：连在一起的两块通常是重叠的（车轮陷进轮拱里），
    /// 开着的话它们会一直互相推，关节和碰撞打架，整个东西抖个不停。
    pub contacts_enabled: bool,
}

impl JointDesc {
    /// 固定关节。
    pub fn fixed(anchor1: Vec2, anchor2: Vec2) -> Self {
        Self::new(JointKind::Fixed, anchor1, anchor2)
    }

    /// 铰链。`limits` 是转角范围（弧度），`None` 表示能转满一圈。
    pub fn revolute(anchor1: Vec2, anchor2: Vec2, limits: Option<[f32; 2]>) -> Self {
        Self::new(JointKind::Revolute { limits }, anchor1, anchor2)
    }

    /// 滑轨。
    pub fn prismatic(anchor1: Vec2, anchor2: Vec2, axis: Vec2, limits: Option<[f32; 2]>) -> Self {
        Self::new(JointKind::Prismatic { axis, limits }, anchor1, anchor2)
    }

    /// 绳索，只限制最大距离。
    pub fn rope(anchor1: Vec2, anchor2: Vec2, max_distance: f32) -> Self {
        Self::new(
            JointKind::Rope {
                max_distance: max_distance.max(0.0),
            },
            anchor1,
            anchor2,
        )
    }

    fn new(kind: JointKind, local_anchor1: Vec2, local_anchor2: Vec2) -> Self {
        Self {
            kind,
            local_anchor1,
            local_anchor2,
            contacts_enabled: false,
        }
    }

    /// 让被连起来的两个刚体之间仍然做碰撞检测。
    pub fn with_contacts(mut self) -> Self {
        self.contacts_enabled = true;
        self
    }

    pub(crate) fn build(&self) -> rd::GenericJoint {
        use rd::{JointAxesMask as Mask, JointAxis as Ax};

        // 滑轨在 rapier 里恒定以关节坐标系的 **X 轴**为自由轴，所以要先把
        // 用户给的方向转到 X 上。其余几种没有方向可言。
        let (locked, angle) = match &self.kind {
            JointKind::Fixed => (Mask::LOCKED_FIXED_AXES, 0.0),
            JointKind::Revolute { .. } => (Mask::LOCKED_REVOLUTE_AXES, 0.0),
            JointKind::Prismatic { axis, .. } => {
                (Mask::LOCKED_PRISMATIC_AXES, Self::angle_of(*axis))
            }
            // 绳索什么轴都不锁：它靠的是下面那条 LinX 上限，
            // 而不是把某个自由度焊死。
            JointKind::Rope { .. } => (Mask::empty(), 0.0),
        };

        let mut builder = rd::GenericJointBuilder::new(locked)
            .local_frame1(to_rp(self.local_anchor1, angle))
            .local_frame2(to_rp(self.local_anchor2, angle))
            .contacts_enabled(self.contacts_enabled);

        // 绳索要限制的是**欧氏距离**，不是某一根轴上的投影。把两个线性轴
        // 耦合起来，下面那条 `LinX` 限位才代表「两点之间有多远」——
        // 少了这一句，绳子只会限制横向分量，重物照样一路掉下去。
        if matches!(self.kind, JointKind::Rope { .. }) {
            builder = builder.coupled_axes(Mask::LIN_AXES);
        }

        let mut joint = builder.build();

        match &self.kind {
            JointKind::Fixed => {}
            JointKind::Revolute { limits } => {
                if let Some(limits) = limits {
                    joint.set_limits(Ax::AngX, *limits);
                }
            }
            JointKind::Prismatic { limits, .. } => {
                if let Some(limits) = limits {
                    joint.set_limits(Ax::LinX, *limits);
                }
            }
            JointKind::Rope { max_distance } => {
                // 下限给 0：绳子能松到两个锚点重合，只是不能拉得比
                // `max_distance` 更长。
                joint.set_limits(Ax::LinX, [0.0, *max_distance]);
            }
        }

        joint
    }

    /// 方向向量对应的角度。退化成零向量时返回 0。
    fn angle_of(axis: Vec2) -> f32 {
        let axis = axis.normalize_or_zero();
        if axis == Vec2::ZERO {
            return 0.0;
        }
        axis.y.atan2(axis.x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapier2d::dynamics::{JointAxesMask as Mask, JointAxis as Ax};

    #[test]
    fn each_kind_locks_the_expected_axes() {
        assert_eq!(
            JointDesc::fixed(Vec2::ZERO, Vec2::ZERO).build().locked_axes,
            Mask::LOCKED_FIXED_AXES
        );
        assert_eq!(
            JointDesc::revolute(Vec2::ZERO, Vec2::ZERO, None)
                .build()
                .locked_axes,
            Mask::LOCKED_REVOLUTE_AXES
        );
        assert_eq!(
            JointDesc::prismatic(Vec2::ZERO, Vec2::ZERO, Vec2::X, None)
                .build()
                .locked_axes,
            Mask::LOCKED_PRISMATIC_AXES
        );
        assert_eq!(
            JointDesc::rope(Vec2::ZERO, Vec2::ZERO, 3.0)
                .build()
                .locked_axes,
            Mask::empty(),
            "绳索不该锁死任何自由度"
        );
    }

    #[test]
    fn limits_reach_the_joint() {
        let hinge = JointDesc::revolute(Vec2::ZERO, Vec2::ZERO, Some([-0.5, 0.5])).build();
        assert_eq!(
            hinge.limits(Ax::AngX).map(|l| [l.min, l.max]),
            Some([-0.5, 0.5])
        );

        let rail = JointDesc::prismatic(Vec2::ZERO, Vec2::ZERO, Vec2::Y, Some([0.0, 2.0])).build();
        assert_eq!(
            rail.limits(Ax::LinX).map(|l| [l.min, l.max]),
            Some([0.0, 2.0])
        );
    }

    #[test]
    fn a_rope_limits_only_its_length() {
        let rope = JointDesc::rope(Vec2::ZERO, Vec2::ZERO, 4.0).build();
        let limit = rope.limits(Ax::LinX).expect("绳索该有长度上限");

        assert_eq!(limit.max, 4.0);
        assert_eq!(limit.min, 0.0, "绳子能松到两端重合");
    }

    #[test]
    fn a_negative_rope_length_is_clamped_to_zero() {
        // 负长度会让求解器去满足一个不可能的约束，两端被无限拉近。
        let rope = JointDesc::rope(Vec2::ZERO, Vec2::ZERO, -2.0);
        assert_eq!(rope.kind, JointKind::Rope { max_distance: 0.0 });
    }

    #[test]
    fn contacts_are_off_by_default() {
        // 连在一起的两块通常重叠着，开着碰撞会让它们互相推、抖个不停。
        assert!(!JointDesc::fixed(Vec2::ZERO, Vec2::ZERO).contacts_enabled);
        assert!(
            JointDesc::fixed(Vec2::ZERO, Vec2::ZERO)
                .with_contacts()
                .contacts_enabled
        );
    }

    #[test]
    fn a_prismatic_axis_becomes_a_frame_angle() {
        // 滑轨的自由轴在 rapier 里恒定是 X，所以方向是靠转关节坐标系表达的。
        assert!((JointDesc::angle_of(Vec2::X) - 0.0).abs() < 1e-6);
        assert!((JointDesc::angle_of(Vec2::Y) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert_eq!(JointDesc::angle_of(Vec2::ZERO), 0.0, "退化的轴不该产生 NaN");
    }
}
