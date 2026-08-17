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
//! `Input → Update → PostUpdate → Transform → Culling → Render → FrameEnd`。
//! 插件的 `update` 挂在 `Update`，`post_update` 挂在 `PostUpdate`；
//! 需要更细的控制可用 [`App::add_system`] 直接往指定阶段挂闭包。

#![warn(missing_docs)]

mod context;
mod stage;

pub use context::Context;
pub use stage::Stage;

use kasset::ResourceManager;
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
    pub use crate::{App, Context, Plugin, Stage};
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
}

/// 应用。装载插件、注册系统，然后接管主循环。
pub struct App {
    config: WindowConfig,
    plugins: Vec<Box<dyn Plugin>>,
    systems: Vec<(Stage, System)>,
    runtime: Option<Runtime>,
    initialized: bool,
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
        }
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
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };

        let now = Instant::now();
        let dt = now.duration_since(runtime.last_frame).as_secs_f32();
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
                exit_requested: &mut exit_requested,
            };
            system(&mut context);
        }

        exit_requested
    }

    /// 对每个插件调用 `callback`，返回是否有人请求退出。
    fn dispatch(
        &mut self,
        mut callback: impl FnMut(&mut Box<dyn Plugin>, &mut Context),
    ) -> bool {
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };

        let now = Instant::now();
        let dt = now.duration_since(runtime.last_frame).as_secs_f32();
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
            resources: ResourceManager::new(),
            start_time: now,
            last_frame: now,
        });

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

        // ── Input / Update / PostUpdate ──
        exit |= self.run_systems(Stage::Input);
        exit |= self.dispatch(|plugin, ctx| plugin.update(ctx));
        exit |= self.run_systems(Stage::Update);
        exit |= self.dispatch(|plugin, ctx| plugin.post_update(ctx));
        exit |= self.run_systems(Stage::PostUpdate);

        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };
        runtime.last_frame = Instant::now();

        // ── Transform：插件可能改了层级或变换，重算世界矩阵与包围盒 ──
        runtime.scene.update();
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
