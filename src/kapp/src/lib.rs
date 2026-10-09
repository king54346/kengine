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
//!
//! # 环境变量
//!
//! 量性能、截图对比、复现问题时用，不用改代码：
//!
//! | 变量 | 作用 |
//! |---|---|
//! | `KENGINE_PROFILE=1` | 一启动就打开剖析面板（平时按 F3） |
//! | `KENGINE_PRESENT=immediate` | 出帧方式：`immediate` / `mailbox` / `fifo`（量性能时关掉垂直同步） |
//! | `KENGINE_FIXED_DT=1/60` | 固定帧间隔：时间不看墙上的钟，每帧前进这么多；还会等在途的资源加载完再走（见 [`App::with_fixed_dt`]） |
//! | `KENGINE_SEED=1` | 脚本里的 `Math.random` 用这个种子，每次跑出同一串数 |
//! | `KENGINE_SCREENSHOT=路径` | 第 `KENGINE_SCREENSHOT_FRAME`（默认 90）帧截图；`KENGINE_SCREENSHOT_EXIT=1` 截完退出；`KENGINE_SCREENSHOT_WIDTH` 缩到这个宽度 |
//! | `KENGINE_PIPELINE_CACHE=0` | 不读写磁盘上的管线缓存（量冷启动时用） |
//!
//! 截图回归测试（`tests/example_screenshots.rs`）就是把后面四个组合起来用的。
//!
//! # Windows 上要把主线程栈开大（用脚本时）
//!
//! Windows 的主线程栈只有 1 MB，而脚本引擎（boa）解析 JavaScript 时递归很深：
//! 一个三层嵌套的调用 `a.f(b.g(c.h(x)))` 就要七八百 KB。栈不够时进程直接
//! `STATUS_STACK_OVERFLOW` 退出，连一行日志都来不及打。
//!
//! 脚本运行时是场景里第一次出现脚本时才建的，**不用脚本的程序不受影响**。
//! 用脚本的游戏在自己包的 `build.rs` 里加上这几行（和本仓库根目录的
//! `build.rs` 一样），把主线程栈开到和 Linux 一样的 8 MB：
//!
//! ```ignore
//! // build.rs
//! fn main() {
//!     let target = std::env::var("TARGET").unwrap_or_default();
//!     if target.contains("windows") {
//!         let stack = 8 * 1024 * 1024;
//!         let arg = if target.contains("msvc") { format!("/STACK:{stack}") } else { format!("-Wl,--stack,{stack}") };
//!         println!("cargo:rustc-link-arg-bins={arg}");
//!     }
//! }
//! ```

#![warn(missing_docs)]

mod context;
mod physics_clock;
mod profiler;
mod stage;

pub use context::{Context, DebugDraw};
use kinput::{KeyCode, MouseButton};
use kui::{EditAction, NavKey, PointerButton, Ui, UiInput};
pub use physics_clock::PhysicsClock;
pub use profiler::Profiler;
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

    /// 每帧调用，**动画写完姿态之后**、物理和世界变换重算之前。
    ///
    /// 动画在 `update` / `post_update` 之后才推进，那两处改骨骼会被这一帧的动画姿态盖掉。
    /// 要在动画之上再改一笔——按体型缩放骨骼、IK、头部看向目标、程序化的抖动——写在这里。
    /// 这时节点的**局部**变换已经是这一帧的动画姿态，世界变换还是上一帧的。
    fn post_animate(&mut self, ctx: &mut Context) {
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
/// 场景里有脚本、而脚本运行时还没建时把它建起来，交出去；没有脚本时返回 `None`。
///
/// 自由函数而不是 `Runtime` 的方法：调用方同时要借 `runtime.scene`。
/// 登记过的脚本原型：`(名字, 造节点的函数)`。
type Prototypes = Vec<(String, Box<dyn Fn() -> Node>)>;

fn ensure_scripts<'a>(
    scripts: &'a mut Option<ScriptRuntime>,
    prototypes: &mut Prototypes,
    scene: &kscene::Scene,
) -> Option<&'a mut ScriptRuntime> {
    if scripts.is_none() && (!scene.script_nodes().is_empty() || !prototypes.is_empty()) {
        let mut runtime = ScriptRuntime::new();
        for (name, factory) in prototypes.drain(..) {
            runtime.register_prototype(name, factory);
        }
        *scripts = Some(runtime);
    }
    scripts.as_mut()
}

struct Runtime {
    /// 手柄轮询（gilrs）。平台没有手柄后端时是 `None`，游戏照常跑。
    gamepads: Option<kinput::GamepadPoller>,
    window: Arc<Window>,
    renderer: Renderer,
    scene: Scene,
    input: Input,
    resources: ResourceManager,
    start_time: Instant,
    last_frame: Instant,
    /// 固定帧间隔（秒）。设了就不看墙上时钟：每帧恰好前进这么多，截图回归测试靠它。
    fixed_dt: Option<f32>,
    /// 这一帧的帧间隔和开场以来的时间。每帧开头定一次，这一帧里所有人读同一份。
    frame_dt: f32,
    frame_elapsed: f32,
    physics_clock: PhysicsClock,
    /// 资源热重载看门人。关掉时为 `None`。
    hot_reload: Option<HotReload>,
    audio: AudioDevice,
    /// 脚本运行时，**场景里第一次出现脚本时才建**。
    ///
    /// 建它要把两段前奏脚本编译一遍，而 boa 的解析器极吃栈（一个三层嵌套的调用
    /// 就要七八百 KB）——Windows 主线程只有 1 MB，不用脚本的程序没理由去冒这个险，
    /// 也没理由在启动时多花这段时间。见 [`ensure_scripts`]。
    scripts: Option<ScriptRuntime>,
    /// 游戏登记的原型，运行时建出来时一并交给它。
    pending_prototypes: Vec<(String, Box<dyn Fn() -> Node>)>,
    /// 本帧脚本抛出的信号，供插件在 `update` 里读。
    script_events: Vec<Signal>,
    /// 剖析面板（F3）。
    profiler: Profiler,
    /// 窗口上光标此刻是不是锁着。和 `input.cursor_lock_active()` 不一致时改窗口。
    cursor_locked: bool,
    /// 窗口上输入法现在开没开（kwinit 建窗口时是关的）。
    ime_allowed: bool,
    /// 内置调试叠加层的开关。
    debug: DebugDraw,
    /// UI 状态：字体、字形图集、本帧的绘制列表。
    ui: Ui,
    /// 本帧喂给 UI 的输入。由 `kinput` 翻译而来。
    ui_input: UiInput,
    /// 本帧提交的 GPU 粒子。即时模式：每帧清空，不提交就不画。
    ///
    /// 放在这里而不是 `Scene` 里，是因为它引用 `krender::StorageBuffer`
    /// ——而 `kscene` 在 `krender` 的下游，反过来依赖就成环了。
    /// 精灵能待在 `Scene` 里是因为它只带一个纹理 id。
    gpu_particles: Vec<krender::GpuParticles>,
    /// 自定义后处理。插件往里挂效果，渲染器每帧照着跑。
    post_effects: krender::PostStack,
    /// 画了几帧。
    frames: u64,
    /// 自动截图：环境变量 `KENGINE_SCREENSHOT` 给了路径时，
    /// 第 `KENGINE_SCREENSHOT_FRAME`（默认 90）帧截一张；
    /// `KENGINE_SCREENSHOT_EXIT` 非空时截完就退出。
    ///
    /// 给不看屏幕的场合用：CI 里对画面、批量检查例子有没有画出东西。
    auto_screenshot: Option<AutoScreenshot>,
}

impl Runtime {
    /// 定下这一帧的帧间隔和时间。每帧开头调一次。
    fn advance_clock(&mut self, now: Instant) {
        match self.fixed_dt {
            Some(step) => {
                self.frame_dt = step;
                self.frame_elapsed += step;
                // 资源是后台线程异步加载的：同一份脚本这次第 2 帧就绪、下次第 3 帧就绪，
                // 后面的一切（刷怪时机、物理）就整体错开一帧。固定步长要的是可复现，
                // 所以这里等在途的加载做完再往下走（最多等十秒，免得坏资源卡死）。
                let deadline = Instant::now() + std::time::Duration::from_secs(10);
                while !self.resources.is_idle() && Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            None => {
                self.frame_dt = now.duration_since(self.last_frame).as_secs_f32();
                self.frame_elapsed = now.duration_since(self.start_time).as_secs_f32();
            }
        }
        self.last_frame = now;
        // 着色器里的时间（水面、流光）也跟着走，不然固定步长下画面照样每次不同。
        self.renderer
            .set_time_override(self.fixed_dt.map(|_| (self.frame_elapsed, self.frame_dt)));
    }
}

/// `KENGINE_FIXED_DT`：秒数（`0.016667`）或分数（`1/60`）。
fn fixed_dt_from_env() -> Option<f32> {
    let text = std::env::var("KENGINE_FIXED_DT").ok()?;
    let value = match text.split_once('/') {
        Some((a, b)) => a.trim().parse::<f32>().ok()? / b.trim().parse::<f32>().ok()?,
        None => text.trim().parse::<f32>().ok()?,
    };
    (value > 0.0 && value.is_finite()).then_some(value)
}

struct AutoScreenshot {
    path: std::path::PathBuf,
    frame: u64,
    exit: bool,
    requested: bool,
}

impl AutoScreenshot {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("KENGINE_SCREENSHOT")?;
        let frame = std::env::var("KENGINE_SCREENSHOT_FRAME")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90);
        Some(Self {
            path: path.into(),
            frame,
            exit: std::env::var_os("KENGINE_SCREENSHOT_EXIT").is_some_and(|v| !v.is_empty()),
            requested: false,
        })
    }
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
    /// 一启动就打开剖析面板。
    profiler: bool,
    /// 出帧方式（垂直同步开关）。`None` 用渲染器的默认（垂直同步）。
    present_mode: Option<krender::PresentMode>,
    /// 固定帧间隔。见 [`App::with_fixed_dt`]。
    fixed_dt: Option<f32>,
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
            present_mode: None,
            fixed_dt: None,
            profiler: false,
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

    /// 出帧方式：垂直同步（默认）、Mailbox 或 Immediate。量性能时关掉垂直同步，
    /// 不然每帧都被锁在显示器刷新率上，看不出余量。环境变量 `KENGINE_PRESENT` 优先。
    pub fn with_present_mode(mut self, mode: krender::PresentMode) -> Self {
        self.present_mode = Some(mode);
        self
    }

    /// 固定帧间隔：每帧的 `ctx.dt` 恰好是 `seconds`，时间不看墙上的钟。
    ///
    /// 动画、物理、脚本、补间、着色器里的时间全从它来，于是「第 N 帧长什么样」
    /// 和机器快慢无关——截图对比、录制回放要的就是这个。环境变量
    /// `KENGINE_FIXED_DT=0.016667`（或 `1/60`）效果相同。正常游戏别开：机器卡的时候
    /// 游戏会跟着变慢，而不是丢帧追上。
    pub fn with_fixed_dt(mut self, seconds: f32) -> Self {
        self.fixed_dt = (seconds > 0.0 && seconds.is_finite()).then_some(seconds);
        self
    }

    /// 一启动就打开剖析面板（平时按 F3 开关）。
    pub fn with_profiler(mut self, visible: bool) -> Self {
        self.profiler = visible;
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

        let dt = dt_override.unwrap_or(runtime.frame_dt);
        let elapsed = runtime.frame_elapsed;
        let stats = runtime.renderer.stats();

        let mut exit_requested = false;
        // 后处理与阴影级联从渲染器取出来交给插件改，跑完再写回去。
        // 直接借渲染器的话会和 `runtime.scene` 的可变借用打架。
        let mut post = runtime.renderer.post_settings();
        let mut shadow = runtime.renderer.shadow_cascades();
        let mut ssao = runtime.renderer.ssao();
        let mut clusters = runtime.renderer.clusters();
        let cluster_stats = (
            runtime.renderer.cluster_average(),
            runtime.renderer.cluster_peak(),
            runtime.renderer.cluster_overflow(),
        );

        for (system_stage, system) in &mut self.systems {
            if *system_stage != stage {
                continue;
            }
            let compute = krender::ComputeContext::from_renderer(&runtime.renderer);
            let mut context = Context {
                scene: &mut runtime.scene,
                input: &mut runtime.input,
                resources: &runtime.resources,
                dt,
                elapsed,
                window: &runtime.window,
                stats,
                cluster_stats,
                audio: &runtime.audio,
                script_events: &runtime.script_events,
                debug: &mut runtime.debug,
                profiler: &mut runtime.profiler,
                ui: &mut runtime.ui,
                ui_input: &runtime.ui_input,
                post: &mut post,
                // 先算好再进结构体：字面量里同时 `&runtime.renderer` 和
                // `&mut runtime.renderer` 会被借用检查器拒绝。
                compute,
                gpu_particles: &mut runtime.gpu_particles,
                post_effects: &mut runtime.post_effects,
                shadow: &mut shadow,
                clusters: &mut clusters,
                ssao: &mut ssao,
                renderer: &mut runtime.renderer,
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
        if ssao != runtime.renderer.ssao() {
            runtime.renderer.set_ssao(ssao);
        }
        if clusters != runtime.renderer.clusters() {
            runtime.renderer.set_clusters(clusters);
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

        let dt = dt_override.unwrap_or(runtime.frame_dt);
        let elapsed = runtime.frame_elapsed;
        let stats = runtime.renderer.stats();

        // 后处理与阴影级联交给插件改，跑完写回。直接借渲染器会和
        // `runtime.scene` 的可变借用打架。
        let mut post = runtime.renderer.post_settings();
        let mut shadow = runtime.renderer.shadow_cascades();
        let mut ssao = runtime.renderer.ssao();
        let mut clusters = runtime.renderer.clusters();
        let cluster_stats = (
            runtime.renderer.cluster_average(),
            runtime.renderer.cluster_peak(),
            runtime.renderer.cluster_overflow(),
        );

        let mut exit_requested = false;
        for plugin in &mut self.plugins {
            let compute = krender::ComputeContext::from_renderer(&runtime.renderer);
            let mut context = Context {
                scene: &mut runtime.scene,
                input: &mut runtime.input,
                resources: &runtime.resources,
                dt,
                elapsed,
                window: &runtime.window,
                stats,
                cluster_stats,
                audio: &runtime.audio,
                script_events: &runtime.script_events,
                debug: &mut runtime.debug,
                profiler: &mut runtime.profiler,
                ui: &mut runtime.ui,
                ui_input: &runtime.ui_input,
                post: &mut post,
                // 先算好再进结构体：字面量里同时 `&runtime.renderer` 和
                // `&mut runtime.renderer` 会被借用检查器拒绝。
                compute,
                gpu_particles: &mut runtime.gpu_particles,
                post_effects: &mut runtime.post_effects,
                shadow: &mut shadow,
                clusters: &mut clusters,
                ssao: &mut ssao,
                renderer: &mut runtime.renderer,
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
        if ssao != runtime.renderer.ssao() {
            runtime.renderer.set_ssao(ssao);
        }
        if clusters != runtime.renderer.clusters() {
            runtime.renderer.set_clusters(clusters);
        }

        exit_requested
    }
}

impl AppHandler for App {
    fn on_resume(&mut self, window: Arc<Window>) {
        if self.runtime.is_some() {
            return;
        }

        let mut renderer = pollster::block_on(Renderer::new(window.clone()));
        if let Some(mode) = self.present_mode {
            renderer.set_present_mode(mode);
        }
        let now = Instant::now();

        self.runtime = Some(Box::new(Runtime {
            gamepads: kinput::GamepadPoller::new(),
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
            fixed_dt: self.fixed_dt.or_else(fixed_dt_from_env),
            frame_dt: 0.0,
            frame_elapsed: 0.0,
            last_frame: now,
            physics_clock: PhysicsClock::new(self.physics_hz),
            hot_reload: None,
            audio: if self.audio {
                AudioDevice::open()
            } else {
                AudioDevice::silent()
            },
            scripts: None,
            pending_prototypes: std::mem::take(&mut self.prototypes),
            script_events: Vec::new(),
            debug: DebugDraw::none(),
            cursor_locked: false,
            ime_allowed: false,
            profiler: {
                let mut profiler = Profiler::default();
                // 环境变量 `KENGINE_PROFILE=1` 一启动就打开面板（量性能、截图时用）。
                if self.profiler || std::env::var("KENGINE_PROFILE").is_ok_and(|v| v != "0") {
                    profiler.set_visible(true);
                }
                profiler
            },
            ui: Ui::new(),
            gpu_particles: Vec::new(),
            post_effects: krender::PostStack::default(),
            frames: 0,
            auto_screenshot: AutoScreenshot::from_env(),
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

        // F3 开关剖析面板。按键状态是上一轮事件攒下的，`end_frame` 在帧末才清。
        if let Some(runtime) = self.runtime.as_mut()
            && runtime.input.key_just_pressed(KeyCode::F3)
        {
            let visible = !runtime.profiler.is_visible();
            runtime.profiler.set_visible(visible);
        }
        let frame_start = Instant::now();
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.advance_clock(frame_start);
        }
        let frame_scope = klog::profile!("帧");
        let mut section = klog::profile::Sequence::new();
        section.next("脚本");

        // ── Script ──
        // 排在插件 `update` **之前**：脚本发出的事件这一帧就能被插件读到
        // （`ctx.script_events`），不必等下一帧。
        // 脚本读到的仍然是上一帧末的变换——快照语义本来如此，见 `kscript`。
        if let Some(runtime) = self.runtime.as_mut() {
            let (dt, elapsed) = (runtime.frame_dt, runtime.frame_elapsed);
            let scripts = ensure_scripts(
                &mut runtime.scripts,
                &mut runtime.pending_prototypes,
                &runtime.scene,
            );
            runtime.script_events = match scripts {
                Some(scripts) => scripts.process(
                    &mut runtime.scene,
                    &mut runtime.input,
                    &runtime.resources,
                    dt,
                    elapsed,
                ),
                None => Vec::new(),
            };
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
            // 手柄事件在插件 `update` 之前收进来，和键鼠同一帧可见。
            if let Some(poller) = runtime.gamepads.as_mut() {
                poller.poll(runtime.input.gamepads_mut());
            }
            translate_ui_input(&runtime.input, scale, &mut runtime.ui_input);
        }

        // ── Input / Update / PostUpdate ──
        section.next("逻辑");
        exit |= self.run_systems(Stage::Input);
        exit |= self.dispatch(|plugin, ctx| plugin.update(ctx));
        exit |= self.run_systems(Stage::Update);
        exit |= self.dispatch(|plugin, ctx| plugin.post_update(ctx));
        exit |= self.run_systems(Stage::PostUpdate);

        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };
        let dt = runtime.frame_dt;

        // ── Animation：动画改的是局部变换，必须排在世界变换重算之前 ──
        section.next("动画");
        runtime.scene.tick_animations(dt);
        // 动画之上的程序化修改（IK、按体型缩放骨骼……）：姿态刚写完，世界变换还没重算。
        exit |= self.dispatch(|plugin, ctx| plugin.post_animate(ctx));
        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };
        section.next("物理");

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
            let now = runtime.frame_elapsed;
            if let Some(scripts) = runtime.scripts.as_mut() {
                let signals =
                    scripts.physics_process(&mut runtime.scene, &mut runtime.input, step, now);
                runtime.script_events.extend(signals);
            }

            runtime.scene.step_physics(step);

            exit |= self.run_systems_with_dt(Stage::Physics, Some(step));
        }

        let Some(runtime) = self.runtime.as_mut() else {
            return FrameOutcome::Continue;
        };

        // ── Transform：插件可能改了层级或变换，重算世界矩阵与包围盒 ──
        section.next("变换");
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
        runtime.scene.debug_draw_physics_2d(debug.physics2d);

        // 剖析面板画在所有插件之后、UI 封口之前，盖在最上面。
        runtime.profiler.draw(&mut runtime.ui);

        // UI 一帧的收尾。插件在 `update` 里画，这里封口——
        // 不封的话最后一批图元会静默丢失。
        runtime.ui.end_frame();
        // 一帧有效的输入（刚按下、刚松开、滚轮、文本）到此为止。
        runtime.ui_input.end_frame();

        runtime.frames += 1;
        if let Some(shot) = runtime.auto_screenshot.as_mut()
            && !shot.requested
            && runtime.frames >= shot.frame
        {
            runtime.renderer.request_screenshot(shot.path.clone());
            shot.requested = true;
        }

        // 光标锁定：插件和脚本这一帧可能改了意图，失焦 / 回焦也会改，统一在这里落到窗口上。
        let want = runtime.input.cursor_lock_active();
        if want != runtime.cursor_locked {
            runtime.cursor_locked = apply_cursor_lock(&runtime.window, want);
        }
        // 输入法同理：游戏要打字时才开，平时关着，免得中文输入法截走 Shift 和字母键。
        let want = runtime.input.ime_allowed();
        if want != runtime.ime_allowed {
            runtime.window.set_ime_allowed(want);
            runtime.ime_allowed = want;
        }

        section.next("渲染提交");
        match runtime.renderer.render_with_effects(
            &runtime.scene,
            &runtime.ui,
            &runtime.gpu_particles,
            &runtime.post_effects,
        ) {
            RenderOutcome::Ok | RenderOutcome::Skip => {}
            RenderOutcome::Reconfigure => {
                let size = runtime.renderer.size();
                runtime.renderer.resize(size);
            }
            RenderOutcome::Fatal => exit = true,
        }
        if let Some(shot) = runtime.auto_screenshot.as_ref()
            && shot.requested
            && shot.exit
            && !runtime.renderer.screenshot_pending()
        {
            exit = true;
        }
        exit |= self.run_systems(Stage::Render);

        // ── FrameEnd：清掉「刚按下 / 刚松开」与各类增量 ──
        section.next("帧尾");
        exit |= self.run_systems(Stage::FrameEnd);
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.input.end_frame();
            // 调试线是即时模式的：渲染器已经读走，这一帧的到此为止。
            // 不清的话线段会一帧帧累积，几秒钟就把顶点缓冲撑爆。
            runtime.scene.gizmos_mut().clear();
            // 2D 精灵同理。
            runtime.scene.clear_sprites();
            // GPU 粒子也是即时模式的。清的只是这一帧的提交列表，
            // 缓冲本身由游戏保管，不会被丢掉。
            runtime.gpu_particles.clear();

            // 热重载排在帧末：这一帧的逻辑与渲染已经用完了旧数据，
            // 换在这里最不容易撞上「用到一半资源被换掉」。
            if let Some(watcher) = runtime.hot_reload.as_mut() {
                let reloaded = watcher.poll();
                for path in reloaded {
                    klog::info!("热重载：{}", path.display());
                    // 资源换了新的，但运行时手里还攥着按旧源码建的实例——
                    // 不作废的话改了文件也没反应，看起来像热重载坏了。
                    let reset = match runtime.scripts.as_mut() {
                        Some(scripts) => scripts.reload_path(&mut runtime.scene, &path),
                        None => 0,
                    };
                    if reset > 0 {
                        klog::info!("　└ 重建了 {reset} 个脚本实例");
                    }
                }
            }
        }

        // 剖析：这一帧到此结束，收下 CPU 与 GPU 两边的数据。
        section.end();
        drop(frame_scope);
        if let Some(runtime) = self.runtime.as_mut() {
            let frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
            let cpu = klog::profile::end_frame();
            let gpu = runtime.renderer.gpu_profile().to_vec();
            runtime.profiler.record(frame_ms, cpu, &gpu);
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

/// 把光标锁定状态落到窗口上，返回实际是否锁住了。
///
/// 先要 `Locked`（光标原地不动，macOS / Wayland 支持），不行退到 `Confined`
/// （关在窗口里，Windows / X11）。两种都拿得到原始鼠标增量，第一人称够用。
fn apply_cursor_lock(window: &Window, lock: bool) -> bool {
    use winit::window::CursorGrabMode;
    if !lock {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        return false;
    }
    let grabbed = window
        .set_cursor_grab(CursorGrabMode::Locked)
        .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
    match grabbed {
        Ok(()) => {
            window.set_cursor_visible(false);
            true
        }
        Err(error) => {
            // 记成「锁着」，免得每帧重试刷屏；放开时照常放。
            klog::warn!("锁定光标失败：{error}");
            true
        }
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
