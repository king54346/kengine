//! 引擎与执行器：拥有窗口、渲染器和场景，驱动整个主循环。

use crate::{
    plugin::{Plugin, PluginContext},
    renderer::{RenderOutcome, Renderer},
    scene::Scene,
};
use kasset::ResourceManager;
use kinput::Input;
use std::{sync::Arc, time::Instant};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

/// 运行期状态：窗口与渲染器要等 `resumed` 事件后才能创建，故与 [`Executor`] 分开。
struct Engine {
    window: Arc<Window>,
    renderer: Renderer,
    scene: Scene,
    input: Input,
    resources: ResourceManager,
    start_time: Instant,
    last_frame: Instant,
}

/// 引擎执行器：装载插件并接管主循环。
///
/// ```no_run
/// use kengine::prelude::*;
///
/// #[derive(Default)]
/// struct Game;
/// impl Plugin for Game {}
///
/// let mut executor = Executor::new();
/// executor.add_plugin(Game::default());
/// executor.run();
/// ```
pub struct Executor {
    title: String,
    size: (u32, u32),
    plugins: Vec<Box<dyn Plugin>>,
    engine: Option<Engine>,
    initialized: bool,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor {
    /// 创建执行器。
    pub fn new() -> Self {
        Self {
            title: "kengine".to_string(),
            size: (1280, 720),
            plugins: Vec::new(),
            engine: None,
            initialized: false,
        }
    }

    /// 设置窗口标题。
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// 设置窗口初始尺寸。
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.size = (width, height);
        self
    }

    /// 装载一个插件。可多次调用，回调按装载顺序执行。
    pub fn add_plugin(&mut self, plugin: impl Plugin) {
        self.plugins.push(Box::new(plugin));
    }

    /// 创建事件循环并阻塞运行，直到窗口关闭。
    pub fn run(mut self) {
        let event_loop = EventLoop::new().expect("无法创建事件循环");
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.run_app(&mut self).expect("事件循环异常退出");
    }

    /// 对每个插件调用 `callback`，并在其后处理退出请求。
    fn dispatch(
        &mut self,
        event_loop: &ActiveEventLoop,
        mut callback: impl FnMut(&mut Box<dyn Plugin>, &mut PluginContext),
    ) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };

        let now = Instant::now();
        let dt = now.duration_since(engine.last_frame).as_secs_f32();
        let elapsed = now.duration_since(engine.start_time).as_secs_f32();

        let stats = engine.renderer.stats();
        let mut exit_requested = false;
        for plugin in &mut self.plugins {
            let mut context = PluginContext {
                scene: &mut engine.scene,
                input: &mut engine.input,
                resources: &engine.resources,
                dt,
                elapsed,
                window: &engine.window,
                stats,
                exit_requested: &mut exit_requested,
            };
            callback(plugin, &mut context);
        }

        if exit_requested {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler for Executor {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.engine.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(self.size.0, self.size.1));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("无法创建窗口"),
        );

        let renderer = pollster::block_on(Renderer::new(window.clone()));
        let now = Instant::now();

        self.engine = Some(Engine {
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
            self.dispatch(event_loop, |plugin, context| plugin.init(context));
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        // 鼠标原始移动量只在设备事件里有，第一人称视角依赖它。
        if let Some(engine) = self.engine.as_mut() {
            engine.input.process_device_event(&event);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };

        // 输入状态先于插件更新，这样插件回调里读到的就是本次事件之后的状态。
        engine.input.process_window_event(&event);

        // 再把原始事件转给插件，最后执行引擎自身的默认处理。
        self.dispatch(event_loop, |plugin, context| {
            plugin.on_os_event(&event, context)
        });

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(new_size) => {
                if let Some(engine) = self.engine.as_mut() {
                    engine.renderer.resize(new_size);
                }
            }
            WindowEvent::RedrawRequested => {
                self.dispatch(event_loop, |plugin, context| plugin.update(context));

                let Some(engine) = self.engine.as_mut() else {
                    return;
                };
                engine.last_frame = Instant::now();

                // 插件可能改动了节点层级或变换，重算世界矩阵后再绘制。
                engine.scene.update();

                match engine.renderer.render(&engine.scene) {
                    RenderOutcome::Ok | RenderOutcome::Skip => {}
                    RenderOutcome::Reconfigure => {
                        let size = engine.renderer.size();
                        engine.renderer.resize(size);
                    }
                    RenderOutcome::Fatal => event_loop.exit(),
                }

                // 一帧结束，清掉「刚按下/刚松开」与各类增量。
                engine.input.end_frame();
                engine.window.request_redraw();
            }
            _ => {}
        }
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.dispatch(event_loop, |plugin, context| plugin.on_deinit(context));
    }
}
