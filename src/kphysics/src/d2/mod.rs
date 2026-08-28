//! 2D 刚体物理。
//!
//! 和 3D 那套是**两个独立的世界**，互不感知。一个 2D 刚体永远不会撞到
//! 一个 3D 刚体，两个世界各自步进。同一个游戏里两个都用是可以的。
//!
//! # 为什么不用泛型和 3D 共用一份代码
//!
//! rapier 的 2D 和 3D 是两个 crate（`rapier2d` / `rapier3d`），
//! 里面每个类型都是独立定义的，没有共同的 trait 可以抽。更根本的是
//! **2D 的旋转是标量**而 3D 是四元数，角速度、锁轴、扭矩全都跟着变形。
//! 硬抽出来的泛型接口两边都不好用，还得为此发明一层抽象让读的人多学一遍。
//!
//! 所以这里是**照着 3D 抄一遍**，命名保持一致（`PhysicsWorld`、
//! `RigidBodyDesc`、`ColliderDesc`），靠模块路径区分。
//!
//! # 用法
//!
//! ```
//! use kphysics::d2::*;
//! use kmath::Vec2;
//!
//! let mut world = PhysicsWorld::new();
//!
//! // 地面
//! let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
//! world.add_collider(&ColliderDesc::cuboid(Vec2::new(50.0, 0.5)), Some(ground), 0);
//!
//! // 从 10 米高扔一个球
//! let ball = world.add_body(&RigidBodyDesc::dynamic().with_position(Vec2::new(0.0, 10.0)), 1);
//! world.add_collider(&ColliderDesc::ball(0.5), Some(ball), 1);
//!
//! for _ in 0..180 {
//!     world.step(1.0 / 60.0);
//! }
//!
//! assert!(world.body(ball).unwrap().position().y < 1.5);
//! ```

#[cfg(test)]
mod events_tests;
#[cfg(test)]
mod joint_tests;
#[cfg(test)]
mod shape_cast_tests;
#[cfg(test)]
mod tests;

mod body;
mod character;
#[cfg(test)]
mod character_tests;
mod collider;
mod convert;
mod debug;
mod joint;
mod visit;
mod world;

pub use body::{BodyMut, BodyRef, RigidBodyDesc};
pub use character::{CharacterCollision, CharacterController, CharacterMovement};
pub use collider::{ColliderDesc, ColliderMut, ColliderRef, ColliderShape};
pub use debug::PhysicsDebugOptions2d;
pub use joint::{JointDesc, JointKind};
pub use world::{
    BodyHandle, ColliderHandle, CollisionEvent2d, ContactForceEvent2d, JointHandle, PhysicsWorld,
    RayCastOptions, RayHit, ShapeCastOptions, ShapeHit,
};
