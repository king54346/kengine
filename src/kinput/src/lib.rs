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
mod gamepad;

pub use binding::{AxisBinding, Binding, Bindings};
pub use button::ButtonState;
pub use gamepad::{Gamepad, GamepadAxis, GamepadButton, GamepadPoller, Gamepads};

pub use winit::event::MouseButton;
pub use winit::keyboard::KeyCode;

use kmath::Vec2;
use winit::{
    event::{DeviceEvent, ElementState, MouseScrollDelta, WindowEvent},
    keyboard::PhysicalKey,
};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        Binding, Bindings, ButtonState, GamepadAxis, GamepadButton, Input, KeyCode, MouseButton,
    };
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
    /// 手柄状态（`GamepadPoller` 每帧喂进来）。
    gamepads: Gamepads,
    /// 游戏要求锁定光标（第一人称）。真正锁没锁还要看窗口有没有焦点。
    cursor_lock: bool,
    /// 窗口失焦了。存反过来的值是为了 `Default` 出来就是「有焦点」。
    unfocused: bool,
    /// 游戏要不要输入法。默认不要：开着的话中文输入法会吃掉按键（按 Shift 切中英文、
    /// 字母键进了候选框），游戏收不到。
    ime: bool,
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

    /// 要求锁定（或放开）光标：锁定时光标隐藏、不能移出窗口，视角靠 [`mouse_delta`](Self::mouse_delta) 转。
    ///
    /// 这只是记下意图，引擎在帧末交给窗口。窗口失焦时自动放开，切回来自动恢复——
    /// 游戏不用自己管 Alt+Tab。
    pub fn set_cursor_locked(&mut self, locked: bool) {
        self.cursor_lock = locked;
    }

    /// 要不要打开输入法（中日韩输入）。
    ///
    /// 默认关着：输入法开着时 Shift、字母键会先被它截走（按 Shift 切中英文、打字母弹候选框），
    /// 游戏收不到。有文本框拿到焦点时打开——用 `kui_widgets` 的话每帧
    /// `input.set_ime_allowed(widgets.wants_keyboard())` 就够了。
    ///
    /// 这只是记下意图，引擎在帧末交给窗口；只有变了才真的去改。
    pub fn set_ime_allowed(&mut self, allowed: bool) {
        self.ime = allowed;
    }

    /// 游戏现在要不要输入法。
    pub fn ime_allowed(&self) -> bool {
        self.ime
    }

    /// 游戏是否要求锁定光标（不管窗口此刻有没有焦点）。
    pub fn cursor_locked(&self) -> bool {
        self.cursor_lock
    }

    /// 光标此刻是否真的该锁着：要求锁定，且窗口有焦点。引擎据此改窗口状态。
    pub fn cursor_lock_active(&self) -> bool {
        self.cursor_lock && !self.unfocused
    }

    /// 窗口有没有焦点。
    pub fn focused(&self) -> bool {
        !self.unfocused
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

    /// 读取一个轴，`-1.0..=1.0`。
    ///
    /// 数字键（键盘、手柄按键）按着时是 `-1`、`0` 或 `1`（正负同时按下为 0）；都没按时取绑定的模拟量
    /// （[`Bindings::bind_axis_analog`]，摇杆过了死区之后的值）。轴不存在时返回 `0.0`。
    pub fn axis(&self, axis: &str) -> f32 {
        let Some(binding) = self.bindings.axis(axis) else {
            return 0.0;
        };

        let positive = binding.positive.iter().any(|b| self.binding_pressed(*b));
        let negative = binding.negative.iter().any(|b| self.binding_pressed(*b));

        match (positive, negative) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            (true, true) => 0.0,
            (false, false) => binding
                .analog
                .iter()
                .map(|axis| self.gamepads.axis(*axis))
                .fold(
                    0.0,
                    |best: f32, v| if v.abs() > best.abs() { v } else { best },
                ),
        }
    }

    /// 手柄状态（所有已连接的手柄、死区设置）。
    pub fn gamepads(&self) -> &Gamepads {
        &self.gamepads
    }

    /// 手柄状态的可变引用：改死区，或者测试 / 回放时直接喂按键。
    pub fn gamepads_mut(&mut self) -> &mut Gamepads {
        &mut self.gamepads
    }

    /// 任意一个手柄按着这个键。
    pub fn gamepad_pressed(&self, button: GamepadButton) -> bool {
        self.gamepads.pressed(button)
    }

    /// 任意一个手柄这一帧刚按下这个键。
    pub fn gamepad_just_pressed(&self, button: GamepadButton) -> bool {
        self.gamepads.just_pressed(button)
    }

    /// 一个手柄模拟量，过了死区。摇杆 −1..1（y 向上为正），扳机 0..1。
    pub fn gamepad_axis(&self, axis: GamepadAxis) -> f32 {
        self.gamepads.axis(axis)
    }

    /// 左（`left = true`）或右摇杆，径向死区，长度不超过 1。
    pub fn gamepad_stick(&self, left: bool) -> Vec2 {
        self.gamepads.stick(left)
    }

    /// 把两个轴合成一个方向向量，长度不超过 1。
    ///
    /// 斜向不比直线快；摇杆推一半就是半速（只在超过 1 时才缩回单位长度，不会把轻推放大成全速）。
    pub fn axis_vector(&self, x_axis: &str, y_axis: &str) -> Vec2 {
        let raw = Vec2::new(self.axis(x_axis), self.axis(y_axis));
        if raw.length_squared() > 1.0 {
            raw.normalize()
        } else {
            raw
        }
    }

    /// 某个具体绑定是否被按住。
    pub fn binding_pressed(&self, binding: Binding) -> bool {
        match binding {
            Binding::Key(key) => self.keys.pressed(key),
            Binding::Mouse(button) => self.mouse_buttons.pressed(button),
            Binding::Gamepad(button) => self.gamepads.pressed(button),
        }
    }

    /// 某个具体绑定是否在本帧刚被按下。
    pub fn binding_just_pressed(&self, binding: Binding) -> bool {
        match binding {
            Binding::Key(key) => self.keys.just_pressed(key),
            Binding::Mouse(button) => self.mouse_buttons.just_pressed(button),
            Binding::Gamepad(button) => self.gamepads.just_pressed(button),
        }
    }

    /// 某个具体绑定是否在本帧刚被松开。
    pub fn binding_just_released(&self, binding: Binding) -> bool {
        match binding {
            Binding::Key(key) => self.keys.just_released(key),
            Binding::Mouse(button) => self.mouse_buttons.just_released(button),
            Binding::Gamepad(button) => self.gamepads.just_released(button),
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
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32 / 120.0, p.y as f32 / 120.0),
                };
                self.scroll_delta += Vec2::new(x, y);
            }
            WindowEvent::Focused(focused) => {
                self.unfocused = !focused;
                if !focused {
                    self.reset();
                }
            }
            _ => {}
        }
    }

    /// 处理设备事件，用于获取鼠标原始移动量。由引擎调用。
    pub fn process_device_event(&mut self, event: &DeviceEvent) {
        // 原始鼠标事件不管窗口有没有焦点都会来：失焦时在别的程序里晃鼠标，
        // 不该转动游戏里的视角。
        if let DeviceEvent::MouseMotion { delta } = event
            && !self.unfocused
        {
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
        self.gamepads.end_frame();
        self.mouse_delta = Vec2::ZERO;
        self.scroll_delta = Vec2::ZERO;
    }

    /// 重置所有按键状态。窗口失焦时调用，防止按键卡住。
    pub fn reset(&mut self) {
        self.keys.reset();
        self.mouse_buttons.reset();
        self.gamepads.reset();
        self.mouse_delta = Vec2::ZERO;
        self.scroll_delta = Vec2::ZERO;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_stick_and_the_keyboard_drive_the_same_axis() {
        let mut input = Input::new();
        input
            .bindings_mut()
            .bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
        input
            .bindings_mut()
            .bind_axis_analog("horizontal", GamepadAxis::LeftStickX);
        input
            .bindings_mut()
            .bind_action("jump", GamepadButton::South);
        input.gamepads_mut().connect(0, "pad");
        input
            .gamepads_mut()
            .set_axis(0, GamepadAxis::LeftStickX, -0.575);
        // 摇杆推一半（过死区之后正好 -0.5）。
        assert!(
            (input.axis("horizontal") + 0.5).abs() < 1e-4,
            "{}",
            input.axis("horizontal")
        );
        // 键盘按着时以键盘为准。
        input.press_key(KeyCode::KeyD);
        assert_eq!(input.axis("horizontal"), 1.0);
        // 半速推不会被 axis_vector 放大成全速。
        input.release_key(KeyCode::KeyD);
        input
            .bindings_mut()
            .bind_axis("vertical", KeyCode::KeyW, KeyCode::KeyS);
        assert!((input.axis_vector("horizontal", "vertical").length() - 0.5).abs() < 1e-4);
        input.gamepads_mut().press(0, GamepadButton::South);
        assert!(input.action_just_pressed("jump"));
    }

    #[test]
    fn cursor_lock_follows_focus() {
        let mut input = Input::new();
        assert!(!input.cursor_lock_active());
        input.set_cursor_locked(true);
        assert!(input.cursor_lock_active());
        // Alt+Tab 出去：意图还在，但此刻不该锁。
        input.process_window_event(&WindowEvent::Focused(false));
        assert!(input.cursor_locked());
        assert!(!input.cursor_lock_active());
        input.process_window_event(&WindowEvent::Focused(true));
        assert!(input.cursor_lock_active());
    }

    #[test]
    fn ime_is_off_until_asked_for() {
        let mut input = Input::new();
        // 默认关：不然中文输入法会把 Shift、字母键从游戏手里截走。
        assert!(!input.ime_allowed());
        input.set_ime_allowed(true);
        assert!(input.ime_allowed());
        input.set_ime_allowed(false);
        assert!(!input.ime_allowed());
    }

    #[test]
    fn mouse_motion_while_unfocused_is_ignored() {
        let mut input = Input::new();
        input.process_device_event(&DeviceEvent::MouseMotion { delta: (3.0, 4.0) });
        assert_eq!(input.mouse_delta(), Vec2::new(3.0, 4.0));
        input.end_frame();
        input.process_window_event(&WindowEvent::Focused(false));
        input.process_device_event(&DeviceEvent::MouseMotion { delta: (3.0, 4.0) });
        assert_eq!(input.mouse_delta(), Vec2::ZERO);
    }

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
