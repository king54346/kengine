//! kphysics —— 刚体物理。
//!
//! 全项目**唯一**依赖 rapier 的 crate，作用与 `krender` 之于 wgpu 相同：
//! 把第三方引擎关在一层墙后面。对外的接口只说 kmath 的 `Vec3` / `Quat`，
//! rapier 的类型一个都不外泄。
//!
//! # 为什么是 Rapier 而不是 Jolt
//!
//! Jolt 是 C++ 库，Rust 侧要经 FFI，绑定生态尚不成熟，且会给整个工作区
//! 引入 C++ 工具链依赖——本项目从 kcore 到 kparticle 都是纯 Rust，
//! `cargo test` 一条命令跑完，这个性质不值得为物理放弃。
//!
//! Rapier 是纯 Rust，Fyrox 用的也是它（`scene/graph/physics/`），
//! 参考实现可以直接对照。0.35 起 Rapier 的公开数学类型已从 nalgebra
//! 换成 glam，与本项目同源，转换层因此薄得多。
//!
//! # 用法
//!
//! ```
//! use kphysics::*;
//! use kmath::Vec3;
//!
//! let mut world = PhysicsWorld::new();
//!
//! // 地面
//! let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
//! world.add_collider(&ColliderDesc::cuboid(Vec3::new(50.0, 0.5, 50.0)), Some(ground), 0);
//!
//! // 从 10 米高扔一个球
//! let ball = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec3::Y * 10.0), 1);
//! world.add_collider(&ColliderDesc::ball(0.5), Some(ball), 1);
//!
//! for _ in 0..180 {
//!     world.step(1.0 / 60.0);
//! }
//!
//! assert!(world.body(ball).unwrap().position().y < 1.5);
//! ```
//!
//! # 分工
//!
//! 本 crate 只管**模拟**，不认识场景图。「哪个节点对应哪个刚体」「世界变换
//! 怎么同步回去」由 `kscene` 负责——刚体上的 `user_data`（一个 `u128`）
//! 就是为此留的洞，`kscene` 往里塞节点句柄。

#![warn(missing_docs)]

mod body;
mod collider;
mod convert;
mod debug;
mod events;
mod joint;
mod query;
mod visit;
mod world;

pub use body::{BodyMut, BodyRef, RigidBodyDesc, RigidBodyType};
pub use collider::{
    Axis, CoefficientCombineRule, ColliderDesc, ColliderMut, ColliderRef, ColliderShape,
    InteractionGroups, TriMeshData,
};
pub use debug::PhysicsDebugOptions;
pub use events::{CollisionEvent, ContactForceEvent};
pub use joint::{JointDesc, JointKind, SphericalLimits};
pub use query::{PointProjection, RayCastOptions, RayHit, ShapeCastOptions, ShapeHit};
pub use world::{IntegrationParameters, PhysicsStats, PhysicsWorld};

/// 从 4×4 世界变换矩阵里取出刚体位姿，返回 `(位置, 朝向)`，**丢弃缩放**。
///
/// 物理引擎里没有「被缩放的刚体」这回事——碰撞体的尺寸是形状自己的参数。
/// 场景节点的世界矩阵送进物理世界之前必须先剥掉缩放，否则分解出来的
/// 旋转会被缩放污染。Fyrox 的 `isometry_from_global_transform` 同理。
pub fn pose_from_matrix(matrix: kmath::Mat4) -> (kmath::Vec3, kmath::Quat) {
    convert::from_rp(&convert::pose_from_mat4(matrix))
}

/// 刚体句柄。
///
/// 不透明的新类型：外面拿不到 rapier 的句柄，也就没法绕过 [`PhysicsWorld`]
/// 直接动原生数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BodyHandle(pub(crate) rapier3d::dynamics::RigidBodyHandle);

/// 碰撞体句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColliderHandle(pub(crate) rapier3d::geometry::ColliderHandle);

/// 关节句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JointHandle(pub(crate) rapier3d::dynamics::ImpulseJointHandle);

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        BodyHandle, ColliderDesc, ColliderHandle, ColliderShape, CollisionEvent, InteractionGroups,
        JointDesc, JointHandle, JointKind, PhysicsWorld, RayCastOptions, RayHit, RigidBodyDesc,
        RigidBodyType, SphericalLimits,
    };
}
