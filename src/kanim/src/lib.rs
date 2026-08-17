//! kanim —— 骨骼动画。
//!
//! 本 crate 只做**纯数据的动画运算**：采样曲线、混合姿态、跑状态机、解 IK。
//! 它不认识场景图，也不认识渲染器——[`Pose`] 是它与外界唯一的接口，
//! 由 kscene 负责把姿态写进节点的局部变换。这样整套混合逻辑都能在 CPU 上独立测试。
//!
//! # 数据流
//!
//! ```text
//! AnimationClip ──采样──> Pose ──混合──> Pose ──应用──> 场景节点的局部变换
//!      ▲                            ▲
//!   glTF 导入                 状态机 / 混合树只设置权重
//! ```
//!
//! 关键设计取自 Fyrox 的 `fyrox-animation`：**姿态是混合的中间产物**。
//! 状态机与混合树都不直接产出最终结果，它们只决定各个剪辑的权重，
//! 混合这一步全局只有一份实现。
//!
//! ```
//! use kanim::{AnimationClip, Animator, Channel, Curve, Interpolation, Track};
//! use kmath::Vec3;
//! use std::sync::Arc;
//!
//! // 一秒钟从原点走到 (10, 0, 0)。
//! let clip = AnimationClip::new(
//!     "Walk",
//!     vec![Track {
//!         target: 0,
//!         channel: Channel::Position(
//!             Curve::new(
//!                 vec![0.0, 1.0],
//!                 vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0)],
//!                 Interpolation::Linear,
//!             )
//!             .unwrap(),
//!         ),
//!     }],
//! );
//!
//! let mut animator = Animator::new(Arc::new(vec![clip]));
//! animator.play_by_name("Walk").unwrap();
//! animator.tick(0.5);
//!
//! let entry = animator.pose().entry(0).unwrap();
//! assert_eq!(entry.position, Some(Vec3::new(5.0, 0.0, 0.0)));
//! ```

#![warn(missing_docs)]

mod clip;
mod curve;
mod ik;
mod machine;
mod player;

pub use clip::{AnimationClip, Channel, MorphSample, Pose, PoseEntry, Track};
pub use curve::{Animatable, Curve, Interpolation};
pub use ik::{IkChain, solve_two_bone};
pub use machine::{BlendTree, Condition, Parameter, Parameters, State, StateMachine, Transition};
pub use player::{AnimationState, Animator};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        AnimationClip, Animator, BlendTree, Channel, Curve, Interpolation, Pose, StateMachine,
        Track,
    };
}
