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

pub use context::{Context, DebugDraw};
use kinput::{KeyCode, MouseButton};
use kui::{EditAction, NavKey, PointerButton, Ui, UiInput};
pub use physics_clock::PhysicsClock;
pub use stage::Stage;

use kasset::{HotReload, ResourceIo, ResourceManager};
use kaudio::AudioDevice;
use kinput::Input;
use krender::{RenderOutcome, Renderer};
use kscene::{Node, Scene};
use kscript::{ScriptRuntime, Signal};
use kwinit::{AppHandler, FrameOutcome, WindowConfig};
use std::{sync::Arc, time::Instant};
use winit::{
    event::{DeviceEvent, WindowEvent},
    window::Window,
};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{App, Context, DebugDraw, PhysicsClock, Plugin, Stage};
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
///
/// **[`App`] 里存的是 `Box<Runtime>`**，不是它本身。这东西有几十 KB
/// （光一个 `Scene` 就一万多字节，脚本运行时还要两个），而 `App` 是靠
/// `.with_xxx(mut self) -> Self` 一路链下来的：内联的话每一环都要在栈上
/// 复制一整份，debug 构建又不会把这些复制优化掉。Windows 主线程只有 1 MB，
/// 链上七八个 `with_` 就能把它用光——症状是程序在 `resumed` 之前
/// `STATUS_STACK_OVERFLOW`，什么日志都来不及打。
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
    /// 内置调试叠加层的开关。
    debug: DebugDraw,
    /// UI 状态：字体、字形图集、本帧的绘制列表。
    ui: Ui,
    /// 本帧喂给 UI 的输入。由 `kinput` 翻译而来。
    ui_input: UiInput,
}

/// 把 `kinput` 的状态翻译成 UI 要的输入。
///
/// 指针位置要**除以** DPI 缩放：`kinput` 给的是物理像素，而 UI 全程
/// 用逻辑像素。不换算的话，高分屏上鼠标指着按钮、UI 却以为指针在
/// 屏幕右下角外面——所有控件都点不着。
fn translate_ui_input(input: &Input, scale: f32, out: &mut UiInput) {
    out.pointer = input.cursor_position().map(|p| p / scale);
    out.scroll = input.scroll_delta();

    for (winit_button, ui_button) in [
        (MouseButton::Left, PointerButton::Primary),
        (MouseButton::Right, PointerButton::Secondary),
        (MouseButton::Middle, PointerButton::Middle),
    ] {
        if input.mouse_just_pressed(winit_button) {
            out.pressed.push(ui_button);
        }
        if input.mouse_just_released(winit_button) {
            out.released.push(ui_button);
        }
    }

    let shift = input.key_pressed(KeyCode::ShiftLeft) || input.key_pressed(KeyCode::ShiftRight);
    let ctrl = input.key_pressed(KeyCode::ControlLeft) || input.key_pressed(KeyCode::ControlRight);

    // 按键翻译成编辑动作。文本框只认动作，不认按键——
    // 按键到动作的映射跟平台走（macOS 上行首是 Cmd+←），
    // 文本框不该知道这件事。
    for (key, action) in [
        (KeyCode::Backspace, EditAction::Backspace),
        (KeyCode::Delete, EditAction::Delete),
        (KeyCode::ArrowLeft, EditAction::Left { select: shift }),
        (KeyCode::ArrowRight, EditAction::Right { select: shift }),
        (KeyCode::Home, EditAction::Home { select: shift }),
        (KeyCode::End, EditAction::End { select: shift }),
        (KeyCode::Enter, EditAction::Submit),
        (KeyCode::Escape, EditAction::Cancel),
    ] {
        if input.key_just_pressed(key) {
            out.edits.push(action);
        }
    }
    if ctrl && input.key_just_pressed(KeyCode::KeyA) {
        out.edits.push(EditAction::SelectAll);
    }

    // 同一批方向键再翻译一遍，这次是**导航**的意思：滑条减一步、
    // 单选组上一项、菜单往下走。
    //
    // 和上面的编辑动作**两边都填**，不在这里判断焦点——← 在文本框里是
    // 光标左移，在滑条上是减一步，哪个生效由控件层按焦点决定。这里先
    // 挑一个的话，就得把「谁有焦点」这件事搬到输入翻译里来，而这一层
    // 根本不认识控件。
    for (key, nav) in [
        (KeyCode::ArrowUp, NavKey::Up),
        (KeyCode::ArrowDown, NavKey::Down),
        (KeyCode::ArrowLeft, NavKey::Left),
        (KeyCode::ArrowRight, NavKey::Right),
        (KeyCode::Home, NavKey::Home),
        (KeyCode::End, NavKey::End),
        (KeyCode::Escape, NavKey::Escape),
    ] {
        if input.key_just_pressed(key) {
            out.nav.push(nav);
        }
    }

    // 修饰键是持续量：列表按住 Shift 点第二下选出一个区间，
    // 而那两下之间隔着好多帧。
    out.shift = shift;
    out.ctrl = ctrl;

    if input.key_just_pressed(KeyCode::Tab) {
        // Shift+Tab 往回走，和所有桌面 UI 一致。
        out.focus_step = if shift { -1 } else { 1 };
    }

    // 回车 / 空格激活有焦点的控件。
    //
    // 这两个键同时还有别的身份——回车上面刚被翻成了 `Submit`，空格会作为
    // 一个字符走 `out.text`。**照实填两边就行**：控件层按焦点在谁身上决定
    // 谁吃掉它（文本框吃字符，按钮吃激活），这里不必先判断焦点。
    out.activate = input.key_just_pressed(KeyCode::Enter) || input.key_just_pressed(KeyCode::Space);
}

/// 应用。装载插件、注册系统，然后接管主循环。
pub struct App {
    config: WindowConfig,
    plugins: Vec<Box<dyn Plugin>>,
    systems: Vec<(Stage, System)>,
    /// 装箱的理由见 [`Runtime`] 的文档：不装的话链式构建会把主线程栈用光。
    runtime: Option<Box<Runtime>>,
    initialized: bool,
    physics_hz: f32,
    hot_reload: bool,
    /// 资源的字节从哪来。默认是本地文件系统。
    resource_io: Option<Arc<dyn ResourceIo>>,
    audio: bool,
    /// 待登记的脚本原型。运行时是在窗口就绪之后才建的，所以先攒在这里。
    prototypes: Vec<(String, Box<dyn Fn() -> Node>)>,
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
            prototypes: Vec::new(),
        }
    }

    /// 登记一个原型，脚本里用 `spawn("名字", 位置)` 生成。
    ///
    /// ```ignore
    /// App::new().with_prototype("Enemy", || {
    ///     Node::new("Enemy")
    ///         .with_mesh(Mesh::cube())
    ///         .with_script("assets/scripts/enemy.js")
    /// })
    /// ```
    ///
    /// 闭包每次生成时调用一次。让脚本直接拼网格与材质是另一条路，但那会把
    /// 整个渲染栈拖进 JS 层，而且换一套美术资源就得改脚本——名字是两边
    /// 唯一该共享的东西。
    pub fn with_prototype(
        mut self,
        name: impl Into<String>,
        factory: impl Fn() -> Node + 'static,
    ) -> Self {
        self.prototypes.push((name.into(), Box::new(factory)));
        self
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
        let dt =
            dt_override.unwrap_or_else(|| now.duration_since(runtime.last_frame).as_secs_f32());
        let elapsed = now.duration_since(runtime.start_time).as_secs_f32();
        let stats = runtime.renderer.stats();

        let mut exit_requested = false;
        // 后处理与阴影级联从渲染器取出来交给插件改，跑完再写回去。
        // 直接借渲染器的话会和 `runtime.scene` 的可变借用打架。
        let mut post = runtime.renderer.post_settings();
        let mut shadow = runtime.renderer.shadow_cascades();

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
                debug: &mut runtime.debug,
                ui: &mut runtime.ui,
                ui_input: &runtime.ui_input,
                post: &mut post,
                shadow: &mut shadow,
                exit_requested: &mut exit_requested,
            };
            system(&mut context);
        }
        if post != runtime.renderer.post_settings() {
            runtime.renderer.set_post_settings(post);
        }
        if shadow != runtime.renderer.shadow_cascades() {
            runtime.renderer.set_shadow_cascades(shadow);
        }

        exit_requested
    }

    /// 对每个插件调用 `callback`，返回是否有人请求退出。
    fn dispatch(&mut self, callback: impl FnMut(&mut Box<dyn Plugin>, &mut Context)) -> bool {
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
        let dt =
            dt_override.unwrap_or_else(|| now.duration_since(runtime.last_frame).as_secs_f32());
        let elapsed = now.duration_since(runtime.start_time).as_secs_f32();
        let stats = runtime.renderer.stats();

        // 后处理与阴影级联交给插件改，跑完写回。直接借渲染器会和
        // `runtime.scene` 的可变借用打架。
        let mut post = runtime.renderer.post_settings();
        let mut shadow = runtime.renderer.shadow_cascades();

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
                debug: &mut runtime.debug,
                ui: &mut runtime.ui,
                ui_input: &runtime.ui_input,
                post: &mut post,
                shadow: &mut shadow,
                exit_requested: &mut exit_requested,
            };
            callback(plugin, &mut context);
        }
        if post != runtime.renderer.post_settings() {
            runtime.renderer.set_post_settings(post);
        }
        if shadow != runtime.renderer.shadow_cascades() {
            runtime.renderer.set_shadow_cascades(shadow);
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

        self.runtime = Some(Box::new(Runtime {
            window,
            renderer,
            scene: Scene::new(),
            input: Input::new(),
            resources: {
                let resources = match self.resource_io.clone() {
                    Some(io) => ResourceManager::with_io(io),
                    None => ResourceManager::new(),
                };
                // 脚本加载器是引擎自己要用的：`Node::with_script` 存的是
                // 一条资源路径，运行时每帧拿它去 `request::<Script>`。
                // 不在这儿装的话，谁都想不到还得自己注册一个——症状是
                // 挂了脚本的节点安安静静地什么都不做，日志里只有一句
                // 「没有能处理 js 的加载器」。
                //
                // glTF、贴图那些不一样，那是游戏自己要的资源，由游戏注册。
                resources.add_loader(kscript::ScriptLoader);
                resources
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
            scripts: {
                let mut scripts = ScriptRuntime::new();
                for (name, factory) in self.prototypes.drain(..) {
                    scripts.register_prototype(name, factory);
                }
                scripts
            },
            script_events: Vec::new(),
            debug: DebugDraw::none(),
            ui: Ui::new(),
            ui_input: UiInput::default(),
        }));

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

        // 文本输入走事件而不是轮询键位：键位到字符的映射、修饰键组合、
        // 输入法合成全由系统做完，这里拿到的是最终结果。
        //
        // 累积到 `ui_input.text`，帧末统一清掉——一帧内可能来好几个字符。
        match event {
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if let Some(text) = &event.text {
                    // 过滤控制字符。回车、退格、Tab 也会带 text，
                    // 不滤掉的话文本框里会插进去一个看不见的字符。
                    runtime
                        .ui_input
                        .text
                        .extend(text.chars().filter(|c| !c.is_control()));
                }
            }
            // 输入法合成完成。中日韩输入走的是这条路径，不是 KeyboardInput。
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                runtime.ui_input.text.push_str(text);
            }
            _ => {}
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
                &mut runtime.input,
                &runtime.resources,
                dt,
                elapsed,
            );
        }

        // UI 开一帧。**必须排在插件 `update` 之前**——它们要往里画东西，
        // 排在后面的话这一帧画的全被清掉，屏幕上一个 UI 图元都没有。
        if let Some(runtime) = self.runtime.as_mut() {
            let size = runtime.window.inner_size();
            let scale = runtime.window.scale_factor() as f32;
            let scale = scale.max(0.01);
            runtime.ui.begin_frame(
                kmath::Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                scale,
            );
            translate_ui_input(&runtime.input, scale, &mut runtime.ui_input);
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
            let now = Instant::now()
                .duration_since(runtime.start_time)
                .as_secs_f32();
            let signals =
                runtime
                    .scripts
                    .physics_process(&mut runtime.scene, &mut runtime.input, step, now);
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

        // ── 调试叠加层 ──
        // 排在渲染的前一步：`update` 刚跑完，BVH 与包围盒都是本帧的；
        // 而且这是最后一个还来得及往缓冲里加线段的位置。
        let debug = runtime.debug;
        runtime.scene.debug_draw(debug.scene);
        runtime.scene.debug_draw_physics(debug.physics);

        // UI 一帧的收尾。插件在 `update` 里画，这里封口——
        // 不封的话最后一批图元会静默丢失。
        runtime.ui.end_frame();
        // 一帧有效的输入（刚按下、刚松开、滚轮、文本）到此为止。
        runtime.ui_input.end_frame();

        match runtime.renderer.render(&runtime.scene, &runtime.ui) {
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
            // 调试线是即时模式的：渲染器已经读走，这一帧的到此为止。
            // 不清的话线段会一帧帧累积，几秒钟就把顶点缓冲撑爆。
            runtime.scene.gizmos_mut().clear();
            // 2D 精灵同理。
            runtime.scene.clear_sprites();

            // 热重载排在帧末：这一帧的逻辑与渲染已经用完了旧数据，
            // 换在这里最不容易撞上「用到一半资源被换掉」。
            if let Some(watcher) = runtime.hot_reload.as_mut() {
                let reloaded = watcher.poll();
                for path in reloaded {
                    klog::info!("热重载：{}", path.display());
                    // 资源换了新的，但运行时手里还攥着按旧源码建的实例——
                    // 不作废的话改了文件也没反应，看起来像热重载坏了。
                    let reset = runtime.scripts.reload_path(&mut runtime.scene, &path);
                    if reset > 0 {
                        klog::info!("　└ 重建了 {reset} 个脚本实例");
                    }
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

#[cfg(test)]
mod tests {
    use super::App;

    #[test]
    fn the_app_stays_small_enough_to_pass_by_value() {
        // `App` 是靠 `.with_xxx(mut self) -> Self` 一路链下来的，每一环
        // debug 构建都会在栈上复制一整份。它里头曾经内联着整个 `Runtime`
        // （光 `Scene` 就一万多字节），于是链上七八个 `with_` 就能把 Windows
        // 主线程那 1 MB 栈耗光——程序在第一帧之前 STATUS_STACK_OVERFLOW，
        // 一行日志都来不及打。
        //
        // 这条线守的就是那件事：往 `App` 里加字段可以，但别再把大块头
        // 内联进来。
        assert!(
            size_of::<App>() < 1024,
            "App 涨到了 {} 字节，链式构建会开始啃栈",
            size_of::<App>()
        );
    }
}
