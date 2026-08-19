//! 传给插件与系统的引擎上下文。

use kasset::ResourceManager;
use kaudio::AudioDevice;
use kinput::Input;
use krender::RenderStats;
use kscene::{PhysicsDebugOptions, Scene, SceneDebugOptions};
use kscript::Signal;
use winit::window::Window;

/// 引擎每帧自动画哪些调试信息。
///
/// 这些开关只管**内置的**那几套叠加层。想画自己的东西直接用
/// [`Scene::gizmos_mut`]，不必经过这里。
///
/// 总开关是 [`Gizmos::set_enabled`](kscene::Gizmos::set_enabled)：它关着的时候
/// 这里的开关一个都不生效，连数据都不会去收集。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebugDraw {
    /// 场景结构：包围盒、BVH、骨架、坐标轴、光源、相机。
    pub scene: SceneDebugOptions,
    /// 物理：碰撞体线框、刚体坐标轴、关节、接触点。
    pub physics: PhysicsDebugOptions,
}

impl DebugDraw {
    /// 什么都不自动画。这是默认值。
    ///
    /// `PhysicsDebugOptions::default()` 本身是「画碰撞体和关节」，所以
    /// 不能直接 `derive(Default)` 就完事——那样一打开总开关就会莫名
    /// 冒出一堆物理线框。
    pub fn none() -> Self {
        Self {
            scene: SceneDebugOptions::default(),
            physics: PhysicsDebugOptions::none(),
        }
    }

    /// 一项都没开。
    pub fn is_empty(&self) -> bool {
        self.scene.is_empty() && self.physics.is_empty()
    }
}

/// 引擎在回调中交给用户的一组引用。
pub struct Context<'a> {
    /// 当前场景，增删节点、改变换都在这里。
    pub scene: &'a mut Scene,
    /// 输入状态。可在 `init` 里注册动作与轴映射。
    pub input: &'a mut Input,
    /// 资源管理器。克隆廉价，可自行保存副本。
    pub resources: &'a ResourceManager,
    /// 距上一帧的间隔（秒）。
    pub dt: f32,
    /// 自引擎启动以来的总时长（秒）。
    pub elapsed: f32,
    /// 主窗口。
    pub window: &'a Window,
    /// 上一帧的渲染统计（绘制数、剔除数、三角形数）。
    pub stats: RenderStats,
    /// 音频输出。改总音量、直接播一次性音效都走它。
    ///
    /// 场景里挂了 [`SoundSource`](kscene::SoundSource) 的节点会由引擎自动
    /// 同步，一般不必碰这个。
    pub audio: &'a AudioDevice,
    /// 本帧脚本抛出的信号（JS 里的 `emit(name, value)`）。
    ///
    /// 脚本排在插件的 `update` 之前跑，所以这里拿到的是**本帧**的信号。
    pub script_events: &'a [Signal],
    /// 内置调试叠加层的开关，改了下一帧生效。
    pub debug: &'a mut DebugDraw,
    pub(crate) exit_requested: &'a mut bool,
}

impl Context<'_> {
    /// 请求退出程序，本帧结束后生效。
    pub fn request_exit(&mut self) {
        *self.exit_requested = true;
    }

    /// 是否已经有人请求退出。
    pub fn exit_requested(&self) -> bool {
        *self.exit_requested
    }
}
