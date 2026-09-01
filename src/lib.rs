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
//! | `ksprite` | 2D 精灵 / 图集 / 帧动画 | ❌ |
//! | `kaudio` | 混音 / 3D 音频 / 解码（cpal 出声） | ❌，独占 cpal |
//! | `kscript` | JavaScript 脚本（boa） | ❌，独占 boa |
//! | `kphysics` | 刚体物理（rapier 封装） | ❌，独占 rapier |
//! | `kanim` | 骨骼动画（曲线 / 姿态 / 状态机 / IK） | ❌ |
//! | `kinput` | 输入采集与映射 | ❌ |
//! | `klight` | 光源与衰减 | ❌ |
//! | `kmath` / `kcore` / `ktask` / `klog` | 基础设施 | ❌ |

#![warn(missing_docs)]

pub use kanim;
pub use kapp;
pub use kasset;
pub use kaudio;
pub use kcamera;
pub use kcore;
pub use kfont;
pub use kgizmo;
pub use kgltf;
pub use kinput;
pub use klight;
pub use klog;
pub use kmaterial;
pub use kmath;
pub use kmesh;
pub use kparticle;
pub use kpbr;
pub use kphysics;
pub use krender;
pub use kscene;
pub use kscript;
pub use kshader;
pub use ksprite;
pub use ktask;
pub use kterrain;
pub use ktexture;
pub use kui;
pub use kui_widgets;
pub use kwinit;

mod task;

/// 常用类型的集中导出。
pub mod prelude {
    pub use kapp::{App, Context, DebugDraw, PhysicsClock, Plugin, Stage};
    pub use kasset::{
        BoxedLoaderFuture, LoadError, Resource, ResourceData, ResourceIo, ResourceLoader,
        ResourceManager,
    };
    pub use kcamera::{
        Camera, FlyCamera, Frustum, OrbitCamera, PanCamera, Projection, ScreenShake,
    };
    pub use kcore::pool::Handle;
    // `Color` 在引擎里只有调试线用得上，导出时冠上来源，免得和材质的
    // 颜色向量混起来。
    pub use kgizmo::{Color as GizmoColor, Gizmos, Layer as GizmoLayer};
    pub use kgltf::{GltfLoader, Model};
    pub use kinput::{Binding, Input, KeyCode, MouseButton};
    pub use kmaterial::{Material, MaterialValue};
    // `Plane` 出现在 `kparticle::Collision::planes` 的签名里，
    // 不放进 prelude 的话用起来要额外写一行 `use kengine::kmath::Plane`。
    // 几何图元（`Circle`、`Sphere`…）**有意不放进来**：那些名字太通用，
    // 放进 prelude 容易和游戏自己的类型撞。要用就显式
    // `use kengine::kmath::Circle`。
    pub use kmath::{Aabb, EulerRot, Mat4, Plane, Quat, Rng, Vec2, Vec3, Vec4};
    // 动画状态机的 `State` 与阶段调度的 `Stage` 容易混淆，这里按原名导出，
    // 用的时候看得见它来自哪个体系。
    pub use kanim::{
        AnimationClip, Animator, BlendTree, Condition, IkChain, Parameters, State, StateMachine,
        Transition,
    };
    pub use kaudio::{
        Attenuation, AudioBuffer, AudioDevice, AudioLoader, Listener, Mixer, Sound, Spatial, Status,
    };
    pub use kfont::{Align as TextAlign, Font, TextStyle, Wrap as TextWrap};
    pub use klight::cascade::{Cascade, CascadeSettings};
    pub use klight::{Light, LightKind};
    pub use kmesh::{Mesh, SkinVertex, Vertex};
    pub use kparticle::{
        BlendMode, Collision, CollisionResponse, ColorGradient, Curve, Emitter, EmitterShape,
        ParticleSystem, Space, SurfaceHit,
    };
    pub use kpbr::{
        Environment, PbrMaterial,
        hdr::HdrImage,
        loader::HdrLoader,
        prefilter::{PrefilterSettings, prefilter},
    };
    pub use kphysics::{
        BodyHandle, ColliderDesc, ColliderHandle, ColliderShape, CollisionEvent, InteractionGroups,
        JointDesc, JointHandle, JointKind, PhysicsDebugOptions, PhysicsWorld, RayCastOptions,
        RayHit, RigidBodyDesc, RigidBodyType, ShapeCastOptions, SphericalLimits,
    };
    pub use krender::{AntiAlias, PostSettings, RenderStats};
    pub use kscene::{
        AnimationPlayer, Cell, Collider, Joint, LimbDesc, Node, Ragdoll, RagdollBuilder,
        RagdollLimb, RigidBody, Scene, SceneDebugOptions, SceneRayHit, ScriptSlot, Skin, SortMode,
        SoundSource, SpriteInstance, Streaming, Terrain, Transform, hinge_limits,
    };
    pub use kscript::{Script, ScriptLoader, ScriptRuntime, ScriptStats, Signal};
    pub use kshader::{Shader, ShaderLoader};
    pub use ksprite::{
        Anchor, Atlas, PlayMode, Slices, Sprite, SpriteAnimation, SpriteRegion, TileMap,
    };
    pub use ktexture::{Sampler, Texture, TextureLoader};
    // 核心层：布局、样式、绘制图元。
    pub use kui::{
        AlignCross, Direction as UiDirection, Edges, Id as UiId, Justify, Length, NavKey,
        Rect as UiRect, Ui, UiInput,
    };
    // 控件层。滑条和列表要用到各自的配置 / 动作类型，一并带上。
    pub use kui_widgets::{ListAction, Slider, Theme as UiTheme, TrackClick, WidgetUi};
    pub use winit::event::WindowEvent;
}
