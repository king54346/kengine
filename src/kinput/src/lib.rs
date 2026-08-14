//! kinput —— 输入采集与映射。
//!
//! 引擎每帧把 winit 事件喂给 [`Input`]，游戏逻辑从中查询状态：
//!
//! ```
//! use kinput::prelude::*;
//!
//! let mut input = Input::new();
//! input.bindings_mut()
//!     .bind_action("jump", KeyCode::Space)
//!     ;
//! input.bindings_mut()
//!     .bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
//!
//! // 引擎内部会根据窗口事件调用，这里手动模拟：
//! input.press_key(KeyCode::Space);
//! assert!(input.action_just_pressed("jump"));
//! assert!(input.action_pressed("jump"));
//!
//! // 每帧末清理「刚按下」标记。
//! input.end_frame();
//! assert!(!input.action_just_pressed("jump"));
//! assert!(input.action_pressed("jump")); // 仍按住
//! ```

#![warn(missing_docs)]

mod binding;
mod button;

pub use binding::{AxisBinding, Binding, Bindings};
pub use button::ButtonState;

pub use winit::event::MouseButton;
pub use winit::keyboard::KeyCode;

use kmath::Vec2;
use winit::{
    event::{DeviceEvent, ElementState, MouseScrollDelta, WindowEvent},
    keyboard::PhysicalKey,
};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{Binding, Bindings, ButtonState, Input, KeyCode, MouseButton};
}

/// 输入状态总入口。
#[derive(Debug, Default)]
pub struct Input {
    keys: ButtonState<KeyCode>,
    mouse_buttons: ButtonState<MouseButton>,
    cursor_position: Option<Vec2>,
    mouse_delta: Vec2,
    scroll_delta: Vec2,
    bindings: Bindings,
}

impl Input {
    /// 创建空的输入状态。
    pub fn new() -> Self {
        Self::default()
    }

    // ── 键盘 ─────────────────────────────────────────────────────────────

    /// 按键是否被按住。
    pub fn key_pressed(&self, key: KeyCode) -> bool {
        self.keys.pressed(key)
    }

    /// 按键是否在本帧刚被按下。
    pub fn key_just_pressed(&self, key: KeyCode) -> bool {
        self.keys.just_pressed(key)
    }

    /// 按键是否在本帧刚被松开。
    pub fn key_just_released(&self, key: KeyCode) -> bool {
        self.keys.just_released(key)
    }

    /// 键盘状态的完整视图。
    pub fn keys(&self) -> &ButtonState<KeyCode> {
        &self.keys
    }

    // ── 鼠标 ─────────────────────────────────────────────────────────────

    /// 鼠标键是否被按住。
    pub fn mouse_pressed(&self, button: MouseButton) -> bool {
        self.mouse_buttons.pressed(button)
    }

    /// 鼠标键是否在本帧刚被按下。
    pub fn mouse_just_pressed(&self, button: MouseButton) -> bool {
        self.mouse_buttons.just_pressed(button)
    }

    /// 鼠标键是否在本帧刚被松开。
    pub fn mouse_just_released(&self, button: MouseButton) -> bool {
        self.mouse_buttons.just_released(button)
    }

    /// 鼠标键状态的完整视图。
    pub fn mouse_buttons(&self) -> &ButtonState<MouseButton> {
        &self.mouse_buttons
    }

    /// 光标在窗口中的位置（物理像素）。光标离开窗口时为 [`None`]。
    pub fn cursor_position(&self) -> Option<Vec2> {
        self.cursor_position
    }

    /// 本帧鼠标移动增量。来自设备原始事件，不受光标是否触边影响，适合第一人称视角。
    pub fn mouse_delta(&self) -> Vec2 {
        self.mouse_delta
    }

    /// 本帧滚轮增量。
    pub fn scroll_delta(&self) -> Vec2 {
        self.scroll_delta
    }

    // ── 动作与轴 ─────────────────────────────────────────────────────────

    /// 映射表的可变引用，用于注册动作与轴。
    pub fn bindings_mut(&mut self) -> &mut Bindings {
        &mut self.bindings
    }

    /// 映射表的只读引用。
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// 动作绑定的任意一个输入被按住。
    pub fn action_pressed(&self, action: &str) -> bool {
        self.any_binding(action, |input, binding| input.binding_pressed(binding))
    }

    /// 动作绑定的任意一个输入在本帧刚被按下。
    pub fn action_just_pressed(&self, action: &str) -> bool {
        self.any_binding(action, |input, binding| input.binding_just_pressed(binding))
    }

    /// 动作绑定的任意一个输入在本帧刚被松开。
    pub fn action_just_released(&self, action: &str) -> bool {
        self.any_binding(action, |input, binding| {
            input.binding_just_released(binding)
        })
    }

    /// 读取一个轴，取值为 `-1.0`、`0.0` 或 `1.0`。
    ///
    /// 正负方向同时按下时返回 `0.0`。轴不存在时同样返回 `0.0`。
    pub fn axis(&self, axis: &str) -> f32 {
        let Some(binding) = self.bindings.axis(axis) else {
            return 0.0;
        };

        let positive = binding.positive.iter().any(|b| self.binding_pressed(*b));
        let negative = binding.negative.iter().any(|b| self.binding_pressed(*b));

        match (positive, negative) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            _ => 0.0,
        }
    }

    /// 把两个轴合成一个方向向量，长度不超过 1。
    pub fn axis_vector(&self, x_axis: &str, y_axis: &str) -> Vec2 {
        let raw = Vec2::new(self.axis(x_axis), self.axis(y_axis));
        // 斜向输入不应该比直线更快。
        raw.normalize_or_zero()
    }

    /// 某个具体绑定是否被按住。
    pub fn binding_pressed(&self, binding: Binding) -> bool {
        match binding {
            Binding::Key(key) => self.keys.pressed(key),
            Binding::Mouse(button) => self.mouse_buttons.pressed(button),
        }
    }

    /// 某个具体绑定是否在本帧刚被按下。
    pub fn binding_just_pressed(&self, binding: Binding) -> bool {
        match binding {
            Binding::Key(key) => self.keys.just_pressed(key),
            Binding::Mouse(button) => self.mouse_buttons.just_pressed(button),
        }
    }

    /// 某个具体绑定是否在本帧刚被松开。
    pub fn binding_just_released(&self, binding: Binding) -> bool {
        match binding {
            Binding::Key(key) => self.keys.just_released(key),
            Binding::Mouse(button) => self.mouse_buttons.just_released(button),
        }
    }

    fn any_binding(&self, action: &str, predicate: impl Fn(&Self, Binding) -> bool) -> bool {
        self.bindings
            .action(action)
            .is_some_and(|bindings| bindings.iter().any(|b| predicate(self, *b)))
    }

    // ── 事件接入 ─────────────────────────────────────────────────────────

    /// 处理窗口事件。由引擎调用。
    pub fn process_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                match event.state {
                    ElementState::Pressed => self.keys.press(code),
                    ElementState::Released => self.keys.release(code),
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match state {
                ElementState::Pressed => self.mouse_buttons.press(*button),
                ElementState::Released => self.mouse_buttons.release(*button),
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(Vec2::new(position.x as f32, position.y as f32));
            }
            WindowEvent::CursorLeft { .. } => self.cursor_position = None,
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (*x, *y),
                    // 像素滚动量级远大于行数，缩放到相近范围便于统一处理。
                    MouseScrollDelta::PixelDelta(p) => {
                        (p.x as f32 / 120.0, p.y as f32 / 120.0)
                    }
                };
                self.scroll_delta += Vec2::new(x, y);
            }
            WindowEvent::Focused(false) => self.reset(),
            _ => {}
        }
    }

    /// 处理设备事件，用于获取鼠标原始移动量。由引擎调用。
    pub fn process_device_event(&mut self, event: &DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.mouse_delta += Vec2::new(delta.0 as f32, delta.1 as f32);
        }
    }

    /// 手动标记按键按下，主要用于测试。
    pub fn press_key(&mut self, key: KeyCode) {
        self.keys.press(key);
    }

    /// 手动标记按键松开，主要用于测试。
    pub fn release_key(&mut self, key: KeyCode) {
        self.keys.release(key);
    }

    /// 结束一帧：清空「刚按下 / 刚松开」标记与各类增量。由引擎调用。
    pub fn end_frame(&mut self) {
        self.keys.end_frame();
        self.mouse_buttons.end_frame();
        self.mouse_delta = Vec2::ZERO;
        self.scroll_delta = Vec2::ZERO;
    }

    /// 重置所有按键状态。窗口失焦时调用，防止按键卡住。
    pub fn reset(&mut self) {
        self.keys.reset();
        self.mouse_buttons.reset();
        self.mouse_delta = Vec2::ZERO;
        self.scroll_delta = Vec2::ZERO;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn input_with_bindings() -> Input {
        let mut input = Input::new();
        input.bindings_mut().bind_action("jump", KeyCode::Space);
        input.bindings_mut().bind_action("jump", KeyCode::KeyW);
        input
            .bindings_mut()
            .bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
        input
            .bindings_mut()
            .bind_axis("vertical", KeyCode::KeyW, KeyCode::KeyS);
        input
    }

    #[test]
    fn just_pressed_lasts_one_frame_only() {
        let mut input = Input::new();

        input.press_key(KeyCode::KeyA);
        assert!(input.key_just_pressed(KeyCode::KeyA));
        assert!(input.key_pressed(KeyCode::KeyA));

        input.end_frame();
        assert!(!input.key_just_pressed(KeyCode::KeyA));
        assert!(input.key_pressed(KeyCode::KeyA));
    }

    #[test]
    fn key_repeat_does_not_retrigger_just_pressed() {
        let mut input = Input::new();

        input.press_key(KeyCode::KeyA);
        input.end_frame();
        // 系统按键重复会持续发送 Pressed，但不该再算作「刚按下」。
        input.press_key(KeyCode::KeyA);

        assert!(!input.key_just_pressed(KeyCode::KeyA));
    }

    #[test]
    fn release_sets_just_released() {
        let mut input = Input::new();

        input.press_key(KeyCode::KeyA);
        input.end_frame();
        input.release_key(KeyCode::KeyA);

        assert!(input.key_just_released(KeyCode::KeyA));
        assert!(!input.key_pressed(KeyCode::KeyA));
    }

    #[test]
    fn action_triggers_on_any_bound_key() {
        let mut input = input_with_bindings();

        input.press_key(KeyCode::KeyW);
        assert!(input.action_pressed("jump"));

        input.release_key(KeyCode::KeyW);
        input.end_frame();
        input.press_key(KeyCode::Space);
        assert!(input.action_pressed("jump"));
    }

    #[test]
    fn unknown_action_is_never_pressed() {
        let input = input_with_bindings();

        assert!(!input.action_pressed("nonexistent"));
        assert!(!input.action_just_pressed("nonexistent"));
    }

    #[test]
    fn axis_reads_positive_and_negative() {
        let mut input = input_with_bindings();

        assert_eq!(input.axis("horizontal"), 0.0);

        input.press_key(KeyCode::KeyD);
        assert_eq!(input.axis("horizontal"), 1.0);

        input.release_key(KeyCode::KeyD);
        input.press_key(KeyCode::KeyA);
        assert_eq!(input.axis("horizontal"), -1.0);
    }

    #[test]
    fn opposite_directions_cancel_out() {
        let mut input = input_with_bindings();

        input.press_key(KeyCode::KeyA);
        input.press_key(KeyCode::KeyD);

        assert_eq!(input.axis("horizontal"), 0.0);
    }

    #[test]
    fn unknown_axis_reads_zero() {
        let input = input_with_bindings();

        assert_eq!(input.axis("nonexistent"), 0.0);
    }

    #[test]
    fn diagonal_movement_is_normalized() {
        let mut input = input_with_bindings();

        input.press_key(KeyCode::KeyD);
        input.press_key(KeyCode::KeyW);

        let v = input.axis_vector("horizontal", "vertical");

        // 斜向输入不能比单方向更快。
        assert!((v.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn reset_releases_held_keys() {
        let mut input = Input::new();

        input.press_key(KeyCode::KeyA);
        input.end_frame();
        input.reset();

        assert!(!input.key_pressed(KeyCode::KeyA));
        // 失焦时按住的键应当补一个「松开」，否则逻辑会漏掉抬起事件。
        assert!(input.key_just_released(KeyCode::KeyA));
    }
}
