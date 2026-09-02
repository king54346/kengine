//! 传给插件与系统的引擎上下文。

use kasset::ResourceManager;
use kaudio::AudioDevice;
use kinput::Input;
use krender::RenderStats;
use kscene::{PhysicsDebugOptions, Scene, SceneDebugOptions};
use kscript::Signal;
use kui::{Ui, UiInput};
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
    /// **2D** 物理，画在 XY 平面上。
    ///
    /// 和上面那个分开：两个物理世界互相独立，绝大多数游戏只用其中一个。
    pub physics2d: kscene::d2::PhysicsDebugOptions2d,
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
            physics2d: kscene::d2::PhysicsDebugOptions2d::none(),
        }
    }

    /// 一项都没开。
    pub fn is_empty(&self) -> bool {
        self.scene.is_empty() && self.physics.is_empty() && self.physics2d.is_empty()
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
    /// 本帧的 UI。即时模式：每帧重新画，不画就没有。
    ///
    /// 引擎在 `update` 之前 `begin_frame`、渲染之前 `end_frame`，
    /// 所以插件里直接画即可，不必自己管这两步。
    pub ui: &'a mut Ui,
    /// 本帧翻译好的 UI 输入（指针、按键、滚轮、Tab）。
    ///
    /// 交给 [`WidgetUi::finish`](kui_widgets::WidgetUi::finish)。位置已经换算成
    /// 逻辑像素，和 `ui.screen()` 同一套坐标。
    pub ui_input: &'a UiInput,
    /// 后处理设置：Bloom、色调映射、抗锯齿。
    ///
    /// 改了立刻生效，下一帧就是新的。移植别的引擎的场景时经常要动它——
    /// 那些例子多半没开辉光，而这边默认开着。
    pub post: &'a mut krender::PostSettings,
    /// 计算着色器的入口，和渲染器**共用**同一台 GPU 设备。
    ///
    /// 共用是要紧的：算出来的缓冲能直接被渲染管线拿去用。
    /// 自己开一台（[`ComputeContext::headless`](krender::ComputeContext::headless)）
    /// 得到的缓冲和渲染那边互不相通。
    ///
    /// 克隆很便宜（内部是 `Arc`），可以自行保存副本。
    pub compute: krender::ComputeContext,
    /// 本帧要画的 GPU 粒子。**即时模式：不提交就不画**，和精灵一样。
    ///
    /// 粒子数据是一块由 [`compute`](Self::compute) 建、由计算着色器填的
    /// storage buffer；渲染器直接拿它当顶点数据源，不经过 CPU。
    ///
    /// ```ignore
    /// ctx.gpu_particles.push(krender::GpuParticles {
    ///     particles: self.buffer.clone(),
    ///     count: self.alive,
    ///     texture: None,
    ///     blend: BlendMode::Additive,
    ///     bounds: self.bounds,
    /// });
    /// ```
    ///
    /// 缓冲里的结构必须和 [`kparticle::PARTICLE_STRUCT_WGSL`] 一致，
    /// 排序的限制见 [`krender::GpuParticles`]。
    pub gpu_particles: &'a mut Vec<krender::GpuParticles>,
    /// 阴影级联的划分参数。改了下一帧生效。
    ///
    /// 场景尺度和默认那套差得远时一定要调：默认按几十米的户外场景配，
    /// 照一个 60 厘米高的模型时，影子会因为精度全撒在空地上而糊成一团。
    pub shadow: &'a mut krender::CascadeSettings,
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
