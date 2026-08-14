//! kengine —— 基于 wgpu 的小型游戏引擎。
//!
//! 用法：实现 [`Plugin`](plugin::Plugin) 写游戏逻辑，交给
//! [`Executor`](engine::Executor) 运行。
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
//!     fn init(&mut self, ctx: &mut PluginContext) {
//!         ctx.scene.add_node(
//!             Node::new("Camera")
//!                 .with_camera(Camera::default())
//!                 .with_transform(Transform::looking_at(
//!                     Vec3::new(0.0, 1.5, 3.0),
//!                     Vec3::ZERO,
//!                     Vec3::Y,
//!                 )),
//!         );
//!         self.cube = ctx.scene.add_node(Node::new("Cube").with_mesh(Mesh::cube()));
//!     }
//!
//!     fn update(&mut self, ctx: &mut PluginContext) {
//!         ctx.scene[self.cube].transform.rotate_y(ctx.dt);
//!     }
//! }
//!
//! let mut executor = Executor::new();
//! executor.add_plugin(Game::default());
//! executor.run();
//! ```

pub mod engine;
pub mod plugin;
mod renderer;
pub mod scene;

pub use renderer::RenderStats;

pub use kgltf;
pub use kmaterial;
pub use kmesh;
pub use kshader;
pub use ktexture;

mod task;

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::engine::Executor;
    pub use crate::plugin::{Plugin, PluginContext};
    pub use crate::scene::{Aabb, Camera, Lighting, Mesh, Node, Scene, Transform, Vertex};
    pub use crate::RenderStats;
    pub use kcamera::{Frustum, Projection};
    pub use kgltf::{GltfLoader, Model};
    pub use kmaterial::{Material, MaterialValue};
    pub use kshader::{Shader, ShaderLoader};
    pub use ktexture::{Sampler, Texture, TextureLoader};
    pub use kasset::{
        BoxedLoaderFuture, LoadError, Resource, ResourceData, ResourceIo, ResourceLoader,
        ResourceManager,
    };
    pub use kcore::pool::Handle;
    pub use kinput::{Binding, Input, KeyCode, MouseButton};
    pub use kmath::{Mat4, Quat, Vec2, Vec3, Vec4};
    pub use winit::event::WindowEvent;
}
