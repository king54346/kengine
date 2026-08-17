//! kengine —— 基于 wgpu 的小型游戏引擎。
//!
//! 本 crate 是**门面**：把各个子 crate 组装起来并统一导出，自身不含实现。
//!
//! ```no_run
//! use kengine::prelude::*;
//!
//! #[derive(Default)]
//! struct Game {
//!     cube: Handle<Node>,
//! }
//!
//! impl Plugin for Game {
//!     fn init(&mut self, ctx: &mut Context) {
//!         ctx.scene.add_node(
//!             Node::new("Camera")
//!                 .with_camera(Camera::default())
//!                 .with_transform(Transform::looking_at(
//!                     Vec3::new(0.0, 2.0, 5.0),
//!                     Vec3::ZERO,
//!                     Vec3::Y,
//!                 )),
//!         );
//!         self.cube = ctx.scene.add_node(Node::new("Cube").with_mesh(Mesh::cube()));
//!     }
//!
//!     fn update(&mut self, ctx: &mut Context) {
//!         ctx.scene[self.cube].transform.rotate_y(ctx.dt);
//!     }
//! }
//!
//! App::new().with_title("我的游戏").add_plugin(Game::default()).run();
//! ```
//!
//! # 模块地图
//!
//! | crate | 职责 | 依赖 wgpu |
//! |---|---|---|
//! | `kapp` | 应用生命周期、插件、阶段调度 | ❌ |
//! | `kwinit` | 窗口与事件循环 | ❌ |
//! | `krender` | 渲染后端 | ✅ **唯一** |
//! | `kscene` | 场景图 | ❌ |
//! | `kcamera` | 相机与视锥剔除 | ❌ |
//! | `kasset` | 异步资源加载 | ❌ |
//! | `kmesh` / `ktexture` / `kmaterial` / `kshader` / `kgltf` / `kpbr` | 资源与渲染数据 | ❌ |
//! | `kparticle` | 粒子模拟（列式存储 + 并行） | ❌ |
//! | `kinput` | 输入采集与映射 | ❌ |
//! | `klight` | 光源与衰减 | ❌ |
//! | `kmath` / `kcore` / `ktask` / `klog` | 基础设施 | ❌ |

#![warn(missing_docs)]

pub use kapp;
pub use kasset;
pub use kcamera;
pub use kcore;
pub use kgltf;
pub use kinput;
pub use klight;
pub use klog;
pub use kmaterial;
pub use kmath;
pub use kmesh;
pub use kparticle;
pub use kpbr;
pub use krender;
pub use kscene;
pub use kshader;
pub use ktask;
pub use ktexture;
pub use kwinit;

mod task;

/// 常用类型的集中导出。
pub mod prelude {
    pub use kapp::{App, Context, Plugin, Stage};
    pub use kasset::{
        BoxedLoaderFuture, LoadError, Resource, ResourceData, ResourceIo, ResourceLoader,
        ResourceManager,
    };
    pub use kcamera::{Camera, Frustum, Projection};
    pub use kcore::pool::Handle;
    pub use kgltf::{GltfLoader, Model};
    pub use kinput::{Binding, Input, KeyCode, MouseButton};
    pub use kmaterial::{Material, MaterialValue};
    pub use kmath::{Aabb, Mat4, Quat, Vec2, Vec3, Vec4};
    pub use kmesh::{Mesh, Vertex};
    pub use kparticle::{BlendMode, ColorGradient, Curve, Emitter, EmitterShape, ParticleSystem, Space};
    pub use kpbr::{Environment, PbrMaterial};
    pub use krender::RenderStats;
    pub use klight::{Light, LightKind};
    pub use kscene::{Node, Scene, Transform};
    pub use kshader::{Shader, ShaderLoader};
    pub use ktexture::{Sampler, Texture, TextureLoader};
    pub use winit::event::WindowEvent;
}
