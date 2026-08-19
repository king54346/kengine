//! 传给插件与系统的引擎上下文。

use kasset::ResourceManager;
use kaudio::AudioDevice;
use kinput::Input;
use krender::RenderStats;
use kscene::Scene;
use kscript::Signal;
use winit::window::Window;

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
