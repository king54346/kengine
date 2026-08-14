//! 插件：游戏逻辑的挂载点。
//!
//! 引擎负责窗口、事件循环和渲染，你的游戏实现 [`Plugin`]，
//! 在各个回调里操作场景。

use crate::{RenderStats, scene::Scene};
use kasset::ResourceManager;
use kinput::Input;
use winit::{event::WindowEvent, window::Window};

/// 传给插件回调的引擎上下文。
pub struct PluginContext<'a> {
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
    pub(crate) exit_requested: &'a mut bool,
}

impl PluginContext<'_> {
    /// 请求退出程序，本帧结束后生效。
    pub fn request_exit(&mut self) {
        *self.exit_requested = true;
    }
}

/// 游戏逻辑插件。所有方法都有默认空实现，按需覆盖即可。
pub trait Plugin: 'static {
    /// 引擎与渲染器就绪后调用一次，适合在这里搭建场景。
    fn init(&mut self, context: &mut PluginContext) {
        let _ = context;
    }

    /// 每帧调用，在场景世界变换重算与渲染之前。
    fn update(&mut self, context: &mut PluginContext) {
        let _ = context;
    }

    /// 收到窗口事件时调用。引擎已处理关闭与尺寸变化，这里拿到的是原始事件。
    fn on_os_event(&mut self, event: &WindowEvent, context: &mut PluginContext) {
        let _ = (event, context);
    }

    /// 程序退出前调用一次。
    fn on_deinit(&mut self, context: &mut PluginContext) {
        let _ = context;
    }
}
