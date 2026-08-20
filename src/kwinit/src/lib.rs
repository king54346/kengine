//! kwinit —— 基于 winit 的窗口后端。
//!
//! 只负责创建窗口、跑事件循环、把事件分发给上层，**不知道渲染器与场景的存在**。
//! 上层实现 [`AppHandler`] 接管这些回调。
//!
//! 这样切分的好处是将来换窗口后端（SDL、无头模式）时，上层代码一行不用改。

#![warn(missing_docs)]

use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

pub use winit::event::{ElementState, KeyEvent, MouseButton};
pub use winit::keyboard::{KeyCode, PhysicalKey};
pub use winit::window::Window as WinitWindow;
pub use winit::{event::DeviceEvent as RawDeviceEvent, event::WindowEvent as RawWindowEvent};

/// 窗口创建参数。
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// 窗口标题。
    pub title: String,
    /// 初始宽度（逻辑像素）。
    pub width: u32,
    /// 初始高度（逻辑像素）。
    pub height: u32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "kengine".to_string(),
            width: 1280,
            height: 720,
        }
    }
}

/// 一帧结束后告诉后端该继续还是退出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// 继续下一帧。
    Continue,
    /// 退出程序。
    Exit,
}

/// 窗口后端把事件回调给上层的接口。
pub trait AppHandler: 'static {
    /// 窗口创建完成，此时才能初始化图形设备。
    fn on_resume(&mut self, window: Arc<Window>);

    /// 收到窗口事件。后端已自行处理关闭请求，这里拿到的是原始事件。
    fn on_window_event(&mut self, event: &WindowEvent) {
        let _ = event;
    }

    /// 收到设备事件，主要用于鼠标原始移动量。
    fn on_device_event(&mut self, event: &DeviceEvent) {
        let _ = event;
    }

    /// 绘制一帧。
    fn on_frame(&mut self) -> FrameOutcome;

    /// 程序退出前调用一次。
    fn on_exit(&mut self) {}
}

/// 创建窗口并运行事件循环，直到窗口关闭或上层请求退出。
pub fn run<H: AppHandler>(handler: H, config: WindowConfig) {
    let event_loop = EventLoop::new().expect("无法创建事件循环");
    // Poll 而非 Wait：游戏需要持续出帧，不能等事件才醒。
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut backend = Backend {
        handler,
        config,
        window: None,
    };
    event_loop.run_app(&mut backend).expect("事件循环异常退出");
}

struct Backend<H: AppHandler> {
    handler: H,
    config: WindowConfig,
    window: Option<Arc<Window>>,
}

impl<H: AppHandler> ApplicationHandler for Backend<H> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // 移动端可能多次 resume，窗口已存在时不重复创建。
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(&self.config.title)
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.config.width,
                self.config.height,
            ));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                klog::error!("创建窗口失败：{error}");
                event_loop.exit();
                return;
            }
        };

        // 打开输入法。默认是关的——不开的话中日韩输入完全收不到，
        // 而且不会有任何报错，表现为「打中文没反应」。
        window.set_ime_allowed(true);

        self.window = Some(window.clone());
        self.handler.on_resume(window);
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        self.handler.on_device_event(&event);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.window.is_none() {
            return;
        }

        self.handler.on_window_event(&event);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if self.handler.on_frame() == FrameOutcome::Exit {
                    event_loop.exit();
                }
                // 主动请求下一帧，形成持续的绘制循环。
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.handler.on_exit();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn default_config_is_sensible() {
        let config = WindowConfig::default();

        assert!(config.width > 0 && config.height > 0);
        assert!(!config.title.is_empty());
    }

    #[test]
    fn frame_outcome_is_comparable() {
        // 事件循环靠它判断是否退出，必须能比较。
        assert_eq!(FrameOutcome::Continue, FrameOutcome::Continue);
        assert_ne!(FrameOutcome::Continue, FrameOutcome::Exit);
    }
}
