//! kapp —— 应用生命周期与插件系统。
//!
//! 把窗口（kwinit）、渲染（krender）、场景（kscene）、资源（kasset）、
//! 输入（kinput）组装成一个可运行的应用。
//!
//! ```no_run
//! use kapp::prelude::*;
//!
//! #[derive(Default)]
//! struct Game;
//!
//! impl Plugin for Game {
//!     fn init(&mut self, ctx: &mut Context) {
//!         // 搭建场景
//!     }
//!     fn update(&mut self, ctx: &mut Context) {
//!         // 每帧逻辑
//!     }
//! }
//!
//! App::new().with_title("我的游戏").add_plugin(Game).run();
//! ```
//!
//! # 阶段
//!
//! 一帧的执行顺序是固定的（见 [`Stage`]）：
//! `Input → Update → PostUpdate → Physics → Transform → Culling → Render → FrameEnd`。
//! 插件的 `update` 挂在 `Update`，`post_update` 挂在 `PostUpdate`；
//! 需要更细的控制可用 [`App::add_system`] 直接往指定阶段挂闭包。

#![warn(missing_docs)]

mod context;
mod physics_clock;
mod stage;

pub use context::Context;
pub use physics_clock::PhysicsClock;
pub use stage::Stage;

use kasset::{HotReload, ResourceIo, ResourceManager};
use kaudio::AudioDevice;
use kscript::{ScriptRuntime, Signal};
use kinput::Input;
use krender::{RenderOutcome, Renderer};
use kscene::Scene;
use kwinit::{AppHandler, FrameOutcome, WindowConfig};
use std::{sync::Arc, time::Instant};
use winit::{
    event::{DeviceEvent, WindowEvent},
    window::Window,
};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{App, Context, PhysicsClock, Plugin, Stage};
}

/// 挂在某个阶段上的一段逻辑。
type System = Box<dyn FnMut(&mut Context<'_>)>;

/// 游戏逻辑插件。所有方法都有默认空实现，按需覆盖即可。
pub trait Plugin: 'static {
    /// 引擎与渲染器就绪后调用一次，适合在这里搭建场景。
    fn init(&mut self, ctx: &mut Context) {
        let _ = ctx;
    }

    /// 每帧调用，对应 [`Stage::Update`]。
    fn update(&mut self, ctx: &mut Context) {
        let _ = ctx;
    }

    /// 每帧调用，对应 [`Stage::PostUpdate`]，在所有插件的 `update` 之后。
    fn post_update(&mut self, ctx: &mut Context) {
        let _ = ctx;
    }

    /// **定长**调用，对应 [`Stage::FixedUpdate`]，每个物理子步之前一次。
    ///
    /// 一帧可能调 0 次（帧率高于物理步频）、1 次或多次（掉帧后追帧）。
    /// `ctx.dt` 在这里**恒等于物理步长**，不是帧间隔。
    ///
    /// 施力、驱动角色控制器、任何「结果不该随帧率变化」的逻辑都该写在这里；
    /// 读输入、改 UI 那些每帧一次就够的，仍然写在 [`update`](Self::update)。
    fn fixed_update(&mut self, ctx: &mut Context) {
        let _ = ctx;
    }

    /// 收到窗口事件。引擎已处理关闭与尺寸变化，这里拿到的是原始事件。
    fn on_os_event(&mut self, event: &WindowEvent, ctx: &mut Context) {
        let _ = (event, ctx);
    }

    /// 程序退出前调用一次。
    fn on_deinit(&mut self, ctx: &mut Context) {
        let _ = ctx;
    }
}

/// 运行期状态。窗口与渲染器要等 `resumed` 之后才能创建，故与 [`App`] 分开。
struct Runtime {
    window: Arc<Window>,
    renderer: Renderer,
    scene: Scene,
    input: Input,
    resources: ResourceManager,
    start_time: Instant,
    last_frame: Instant,
    physics_clock: PhysicsClock,
    /// 资源热重载看门人。关掉时为 `None`。
    hot_reload: Option<HotReload>,
    audio: AudioDevice,
    scripts: ScriptRuntime,
    /// 本帧脚本抛出的信号，供插件在 `update` 里读。
    script_events: Vec<Signal>,
}

/// 应用。装载插件、注册系统，然后接管主循环。
pub struct App {
    config: WindowConfig,
    plugins: Vec<Box<dyn Plugin>>,
    systems: Vec<(Stage, System)>,
    runtime: Option<Runtime>,
    initialized: bool,
    physics_hz: f32,
    hot_reload: bool,
    /// 资源的字节从哪来。默认是本地文件系统。
    resource_io: Option<Arc<dyn ResourceIo>>,
    audio: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// 创建应用。
    pub fn new() -> Self {
        Self {
            config: WindowConfig::default(),
            plugins: Vec::new(),
            systems: Vec::new(),
            runtime: None,
            initialized: false,
            physics_hz: 60.0,
            hot_reload: true,
            resource_io: None,
            audio: true,
        }
    }

    /// 开关音频输出。默认开启。
    ///
    /// 关掉之后引擎仍然会同步声源与听者，只是没有人来取样本——
    /// 与「机器上没有声卡」是同一条路径，游戏逻辑不必区分。
    pub fn with_audio(mut self, enabled: bool) -> Self {
        self.audio = enabled;
        self
    }

    /// 开关资源热重载。默认开启。
    ///
    /// 发布版通常要关掉：资源都在包里，轮询磁盘既查不到东西也没有意义。
    pub fn with_hot_reload(mut self, enabled: bool) -> Self {
        self.hot_reload = enabled;
        self
    }

    /// 指定资源的字节来源，例如一个资源包。
    ///
    /// 不指定时读本地文件系统。想要「散文件优先、包兜底」的话，
    /// 传一个 [`kasset::LayeredResourceIo`]。
    pub fn with_resource_io(mut self, io: Arc<dyn ResourceIo>) -> Self {
        self.resource_io = Some(io);
        self
    }

    /// 设置物理的步频，单位是每秒步数。默认 60。
    ///
    /// 调高更稳（快速运动更不容易穿模），代价是 CPU 线性增长。
    pub fn with_physics_hz(mut self, hz: f32) -> Self {
        self.physics_hz = hz;
        self
    }

    /// 设置窗口标题。
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.config.title = title.into();
        self
    }

    /// 设置窗口初始尺寸。
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.config.width = width;
        self.config.height = height;
        self
    }

    /// 装载一个插件。可多次调用，回调按装载顺序执行。
    pub fn add_plugin(mut self, plugin: impl Plugin) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    /// 往指定阶段挂一段逻辑。
    ///
    /// 适合不需要完整插件的小功能，或物理、动画这类要精确控制执行时机的系统。
    pub fn add_system(
        mut self,
        stage: Stage,
        system: impl FnMut(&mut Context<'_>) + 'static,
    ) -> Self {
        self.systems.push((stage, Box::new(system)));
        self
    }

    /// 运行，直到窗口关闭或有人请求退出。
    pub fn run(self) {
        let config = self.config.clone();
        kwinit::run(self, config);
    }

    /// 在给定阶段执行所有注册到该阶段的系统。
    fn run_systems(&mut self, stage: Stage) -> bool {
        self.run_systems_with_dt(stage, None)
    }

    /// 在给定阶段执行系统，可覆盖 `ctx.dt`。
    ///
    /// 定长阶段要覆盖成**固定步长**：`FixedUpdate` 里写 `v * ctx.dt` 的代码
    /// 拿到帧间隔的话，定长调度就白做了——那正是它要消除的东西。
    fn run_systems_with_dt(&mut self, stage: Stage, dt_override: Option<f32>) -> bool {
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };

        let now = Instant::now();
        let dt = dt_override
            .unwrap_or_else(|| now.duration_since(runtime.last_frame).as_secs_f32());
        let elapsed = now.duration_since(runtime.start_time).as_secs_f32();
        let stats = runtime.renderer.stats();

        let mut exit_requested = false;
        for (system_stage, system) in &mut self.systems {
            if *system_stage != stage {
                continue;
            }
            let mut context = Context {
                scene: &mut runtime.scene,
                input: &mut runtime.input,
                resources: &runtime.resources,
                dt,
                elapsed,
                window: &runtime.window,
                stats,
                audio: &runtime.audio,
                script_events: &runtime.script_events,
                exit_requested: &mut exit_requested,
            };
            system(&mut context);
        }

        exit_requested
    }

    /// 对每个插件调用 `callback`，返回是否有人请求退出。
    fn dispatch(
        &mut self,
        callback: impl FnMut(&mut Box<dyn Plugin>, &mut Context),
    ) -> bool {
        self.dispatch_with_dt(callback, None)
    }

    /// 对每个插件调用 `callback`，可覆盖 `ctx.dt`。
    fn dispatch_with_dt(
        &mut self,
        mut callback: impl FnMut(&mut Box<dyn Plugin>, &mut Context),
        dt_override: Option<f32>,
    ) -> bool {
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };

        let now = Instant::now();
        let dt = dt_override
            .unwrap_or_else(|| now.duration_since(runtime.last_frame).as_secs_f32());
        let elapsed = now.duration_since(runtime.start_time).as_secs_f32();
        let stats = runtime.renderer.stats();

        let mut exit_requested = false;
        for plugin in &mut self.plugins {
            let mut context = Context {
                scene: &mut runtime.scene,
                input: &mut runtime.input,
                resources: &runtime.resources,
                dt,
                elapsed,
                window: &runtime.window,
                stats,
                audio: &runtime.audio,
                script_events: &runtime.script_events,
                exit_requested: &mut exit_requested,
            };
            callback(plugin, &mut context);
        }

        exit_requested
    }
}

impl AppHandler for App {
    fn on_resume(&mut self, window: Arc<Window>) {
        if self.runtime.is_some() {
            return;
        }

        let renderer = pollster::block_on(Renderer::new(window.clone()));
        let now = Instant::now();

        self.runtime = Some(Runtime {
            window,
            renderer,
            scene: Scene::new(),
            input: Input::new(),
            resources: match self.resource_io.clone() {
                Some(io) => ResourceManager::with_io(io),
                None => ResourceManager::new(),
            },
            start_time: now,
            last_frame: now,
            physics_clock: PhysicsClock::new(self.physics_hz),
            hot_reload: None,
            audio: if self.audio {
                AudioDevice::open()
            } else {
                AudioDevice::silent()
            },
            scripts: ScriptRuntime::new(),
            script_events: Vec::new(),
        });

        // 看门人要在资源管理器建好之后再建，它一上来就要把现有资源的
        // 修改时间记成基线。
        if self.hot_reload
            && let Some(runtime) = self.runtime.as_mut()
        {
            runtime.hot_reload = Some(HotReload::new(&runtime.resources));
        }

        // 渲染器就绪后才初始化插件，这样 `init` 里可以安全地假定引擎可用。
        if !self.initialized {
            self.initialized = true;
            self.dispatch(|plugin, ctx| plugin.init(ctx));
        }
    }

    fn on_window_event(&mut self, event: &WindowEvent) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };

        // 输入状态先于插件更新，这样插件回调里读到的就是本次事件之后的状态。
        runtime.input.process_window_event(event);

        if let WindowEvent::Resized(size) = event {
            runtime.renderer.resize(*size);
        }

        self.dispatch(|plugin, ctx| plugin.on_os_event(event, ctx));
    }

    fn on_device_event(&mut self, event: &DeviceEvent) {
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.input.process_device_event(event);
        }
    }

    fn on_frame(&mut self) -> FrameOutcome {
        let mut exit = false;

        // ── Script ──
        // 排在插件 `update` **之前**：脚本发出的事件这一帧就能被插件读到
        // （`ctx.script_events`），不必等下一帧。
        // 脚本读到的仍然是上一帧末的变换——快照语义本来如此，见 `kscript`。
        if let Some(runtime) = self.runtime.as_mut() {
            let now = Instant::now();
            let dt = now.duration_since(runtime.last_frame).as_secs_f32();
            let elapsed = now.duration_since(runtime.start_time).as_secs_f32();
            runtime.script_events = runtime.scripts.process(
                &mut runtime.scene,
                &runtime.resources,
                dt,
                elapsed,
            );
        }

        // ── Input / Update / PostUpdate ──
        exit |= self.run_systems(Stage::Input);
        exit |= self.dispatch(|plugin, ctx| plugin.update(ctx));
        exit |= self.run_systems(Stage::Update);
        exit |= self.dispatch(|plugin, ctx| plugin.post_update(ctx));
        exit |= self.run_systems(Stage::PostUpdate);

        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };
        // 帧间隔要在重置计时点之前取，重置之后拿到的就恒等于 0 了。
        let now = Instant::now();
        let dt = now.duration_since(runtime.last_frame).as_secs_f32();
        runtime.last_frame = now;

        // ── Animation：动画改的是局部变换，必须排在世界变换重算之前 ──
        runtime.scene.tick_animations(dt);

        // ── Physics：定长步进 ──
        // 排在动画之后：未激活的布娃娃要跟着这一帧的动画姿态走。
        // 排在世界变换之前：物理写的也是局部变换。
        let steps = runtime.physics_clock.accumulate(dt);
        let step = runtime.physics_clock.step();

        // 每个子步都完整走一遍「FixedUpdate → 步进 → Physics」。
        //
        // `Physics` 必须**跟着子步**而不是每帧一次：`PhysicsWorld::step` 每次
        // 开头都清空事件队列，一帧跑多个子步时，除最后一个之外的碰撞事件
        // 会全部丢失——一次穿过传感器的完整「进入 + 离开」可能一个都收不到。
        for _ in 0..steps {
            exit |= self.run_systems_with_dt(Stage::FixedUpdate, Some(step));
            exit |= self.dispatch_with_dt(|plugin, ctx| plugin.fixed_update(ctx), Some(step));

            let Some(runtime) = self.runtime.as_mut() else {
                return FrameOutcome::Continue;
            };
            // 脚本的 `_physics_process`：与 `FixedUpdate` 同一条定长节拍。
            let now = Instant::now().duration_since(runtime.start_time).as_secs_f32();
            let signals = runtime
                .scripts
                .physics_process(&mut runtime.scene, step, now);
            runtime.script_events.extend(signals);

            runtime.scene.step_physics(step);

            exit |= self.run_systems_with_dt(Stage::Physics, Some(step));
        }

        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };

        // ── Transform：插件可能改了层级或变换，重算世界矩阵与包围盒 ──
        runtime.scene.update();
        // 粒子紧跟其后：世界空间的粒子出生时要用节点的世界变换，
        // 放在 update 之前的话，第一批粒子会出现在原点。
        runtime.scene.tick_particles(dt);

        // 音频排在世界变换之后：声源的位置取自节点的世界变换，
        // 排在前面的话声音会比画面慢一帧。
        runtime.scene.tick_audio(&runtime.audio);

        exit |= self.run_systems(Stage::Transform);

        // ── Culling + Render：剔除在渲染器内部完成 ──
        exit |= self.run_systems(Stage::Culling);
        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };
        match runtime.renderer.render(&runtime.scene) {
            RenderOutcome::Ok | RenderOutcome::Skip => {}
            RenderOutcome::Reconfigure => {
                let size = runtime.renderer.size();
                runtime.renderer.resize(size);
            }
            RenderOutcome::Fatal => exit = true,
        }
        exit |= self.run_systems(Stage::Render);

        // ── FrameEnd：清掉「刚按下 / 刚松开」与各类增量 ──
        exit |= self.run_systems(Stage::FrameEnd);
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.input.end_frame();

            // 热重载排在帧末：这一帧的逻辑与渲染已经用完了旧数据，
            // 换在这里最不容易撞上「用到一半资源被换掉」。
            if let Some(watcher) = runtime.hot_reload.as_mut() {
                for path in watcher.poll() {
                    klog::info!("热重载：{}", path.display());
                }
            }
        }

        if exit {
            FrameOutcome::Exit
        } else {
            FrameOutcome::Continue
        }
    }

    fn on_exit(&mut self) {
        self.dispatch(|plugin, ctx| plugin.on_deinit(ctx));
    }
}
