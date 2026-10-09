//! 手柄：按键、摇杆、扳机的状态，以及从系统读手柄的轮询器（gilrs）。
//!
//! 状态部分（[`Gamepads`]）不依赖 gilrs，测试和回放可以直接往里喂；[`GamepadPoller`] 每帧把系统的
//! 手柄事件翻译进来（kapp 自动调，游戏不用管）。
//!
//! 按键按位置命名（`South` 是 Xbox 的 A、PlayStation 的 ×、Switch 的 B），和 gilrs、Godot、Bevy 一样——
//! 同一份绑定在三家手柄上都按「下面那个键」理解。

use crate::ButtonState;
use kmath::Vec2;

/// 手柄按键（按位置命名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadButton {
    /// 右边四个键的下面那个（Xbox A / PS × / Switch B）。
    South,
    /// 右边那个（Xbox B / PS ○ / Switch A）。
    East,
    /// 上面那个（Xbox Y / PS △ / Switch X）。
    North,
    /// 左边那个（Xbox X / PS □ / Switch Y）。
    West,
    /// 左肩键（LB / L1）。
    LeftBumper,
    /// 右肩键（RB / R1）。
    RightBumper,
    /// 左扳机按到底（LT / L2）。模拟量见 [`GamepadAxis::LeftTrigger`]。
    LeftTrigger,
    /// 右扳机按到底（RT / R2）。
    RightTrigger,
    /// 选择 / 视图 / Share。
    Select,
    /// 开始 / 菜单 / Options。
    Start,
    /// 中间的徽标键。
    Mode,
    /// 按下左摇杆（L3）。
    LeftStick,
    /// 按下右摇杆（R3）。
    RightStick,
    /// 十字键上。
    DPadUp,
    /// 十字键下。
    DPadDown,
    /// 十字键左。
    DPadLeft,
    /// 十字键右。
    DPadRight,
}

/// 手柄的模拟量。摇杆 −1..1（**y 向上为正**），扳机 0..1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadAxis {
    /// 左摇杆左右。
    LeftStickX,
    /// 左摇杆上下（向上为正）。
    LeftStickY,
    /// 右摇杆左右。
    RightStickX,
    /// 右摇杆上下（向上为正）。
    RightStickY,
    /// 左扳机，0..1。
    LeftTrigger,
    /// 右扳机，0..1。
    RightTrigger,
}

impl GamepadAxis {
    const ALL: [GamepadAxis; 6] = [
        Self::LeftStickX,
        Self::LeftStickY,
        Self::RightStickX,
        Self::RightStickY,
        Self::LeftTrigger,
        Self::RightTrigger,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|axis| *axis == self).unwrap_or(0)
    }
}

/// 一个手柄此刻的状态。
#[derive(Debug, Clone)]
pub struct Gamepad {
    /// 系统给的编号（拔了再插可能换号）。
    pub id: usize,
    /// 系统报的名字（「Xbox Wireless Controller」之类）。
    pub name: String,
    buttons: ButtonState<GamepadButton>,
    /// 原始值（没过死区）。
    axes: [f32; 6],
}

impl Gamepad {
    /// 按键状态。
    pub fn buttons(&self) -> &ButtonState<GamepadButton> {
        &self.buttons
    }

    /// 原始模拟量（没过死区）。
    pub fn raw_axis(&self, axis: GamepadAxis) -> f32 {
        self.axes[axis.index()]
    }
}

/// 所有已连接手柄的状态，以及死区设置。
#[derive(Debug, Clone)]
pub struct Gamepads {
    pads: Vec<Gamepad>,
    /// 摇杆死区（按摇杆**半径**算，不是逐轴）：推得比这小当没推，之外重新拉伸到 0..1，
    /// 推一点点也是从 0 平滑起步，不会在死区边缘跳一下。默认 0.15。
    pub stick_deadzone: f32,
    /// 扳机死区，默认 0.05。
    pub trigger_deadzone: f32,
}

impl Default for Gamepads {
    fn default() -> Self {
        Self {
            pads: Vec::new(),
            stick_deadzone: 0.15,
            trigger_deadzone: 0.05,
        }
    }
}

impl Gamepads {
    /// 已连接的手柄，按接入顺序。
    pub fn iter(&self) -> impl Iterator<Item = &Gamepad> {
        self.pads.iter()
    }

    /// 有没有手柄。
    pub fn is_empty(&self) -> bool {
        self.pads.is_empty()
    }

    /// 按编号找。
    pub fn get(&self, id: usize) -> Option<&Gamepad> {
        self.pads.iter().find(|pad| pad.id == id)
    }

    fn get_mut(&mut self, id: usize) -> &mut Gamepad {
        if let Some(index) = self.pads.iter().position(|pad| pad.id == id) {
            return &mut self.pads[index];
        }
        self.pads.push(Gamepad {
            id,
            name: String::new(),
            buttons: ButtonState::default(),
            axes: [0.0; 6],
        });
        self.pads.last_mut().expect("刚放进去")
    }

    /// 接入一个手柄（已经有了就只改名字）。
    pub fn connect(&mut self, id: usize, name: impl Into<String>) {
        self.get_mut(id).name = name.into();
    }

    /// 拔掉一个手柄。它按住的键不再算按着。
    pub fn disconnect(&mut self, id: usize) {
        self.pads.retain(|pad| pad.id != id);
    }

    /// 按下一个键。
    pub fn press(&mut self, id: usize, button: GamepadButton) {
        self.get_mut(id).buttons.press(button);
    }

    /// 松开一个键。
    pub fn release(&mut self, id: usize, button: GamepadButton) {
        self.get_mut(id).buttons.release(button);
    }

    /// 写一个模拟量的原始值。
    pub fn set_axis(&mut self, id: usize, axis: GamepadAxis, value: f32) {
        let range = if matches!(axis, GamepadAxis::LeftTrigger | GamepadAxis::RightTrigger) {
            0.0..=1.0
        } else {
            -1.0..=1.0
        };
        self.get_mut(id).axes[axis.index()] = value.clamp(*range.start(), *range.end());
    }

    /// 任意一个手柄按着这个键。
    pub fn pressed(&self, button: GamepadButton) -> bool {
        self.pads.iter().any(|pad| pad.buttons.pressed(button))
    }

    /// 任意一个手柄这一帧刚按下这个键。
    pub fn just_pressed(&self, button: GamepadButton) -> bool {
        self.pads.iter().any(|pad| pad.buttons.just_pressed(button))
    }

    /// 任意一个手柄这一帧刚松开这个键。
    pub fn just_released(&self, button: GamepadButton) -> bool {
        self.pads
            .iter()
            .any(|pad| pad.buttons.just_released(button))
    }

    /// 一个模拟量，过了死区之后的值。几个手柄时取推得最多的那个（单人游戏插两个手柄也能用）。
    pub fn axis(&self, axis: GamepadAxis) -> f32 {
        self.pads
            .iter()
            .map(|pad| self.filtered(pad, axis))
            .fold(
                0.0,
                |best: f32, v| if v.abs() > best.abs() { v } else { best },
            )
    }

    /// 一根摇杆（左 / 右），过了径向死区，长度不超过 1。
    pub fn stick(&self, left: bool) -> Vec2 {
        let (x, y) = if left {
            (GamepadAxis::LeftStickX, GamepadAxis::LeftStickY)
        } else {
            (GamepadAxis::RightStickX, GamepadAxis::RightStickY)
        };
        self.pads
            .iter()
            .map(|pad| self.radial(Vec2::new(pad.raw_axis(x), pad.raw_axis(y))))
            .fold(Vec2::ZERO, |best, v| {
                if v.length_squared() > best.length_squared() {
                    v
                } else {
                    best
                }
            })
    }

    fn filtered(&self, pad: &Gamepad, axis: GamepadAxis) -> f32 {
        match axis {
            GamepadAxis::LeftTrigger | GamepadAxis::RightTrigger => {
                let v = pad.raw_axis(axis);
                if v <= self.trigger_deadzone {
                    0.0
                } else {
                    (v - self.trigger_deadzone) / (1.0 - self.trigger_deadzone)
                }
            }
            // 单轴读数也按整根摇杆的径向死区算：斜着推时 x 分量不会被逐轴死区吃掉一截。
            GamepadAxis::LeftStickX => {
                self.radial(Vec2::new(
                    pad.raw_axis(axis),
                    pad.raw_axis(GamepadAxis::LeftStickY),
                ))
                .x
            }
            GamepadAxis::LeftStickY => {
                self.radial(Vec2::new(
                    pad.raw_axis(GamepadAxis::LeftStickX),
                    pad.raw_axis(axis),
                ))
                .y
            }
            GamepadAxis::RightStickX => {
                self.radial(Vec2::new(
                    pad.raw_axis(axis),
                    pad.raw_axis(GamepadAxis::RightStickY),
                ))
                .x
            }
            GamepadAxis::RightStickY => {
                self.radial(Vec2::new(
                    pad.raw_axis(GamepadAxis::RightStickX),
                    pad.raw_axis(axis),
                ))
                .y
            }
        }
    }

    fn radial(&self, raw: Vec2) -> Vec2 {
        let length = raw.length();
        if length <= self.stick_deadzone {
            return Vec2::ZERO;
        }
        let scaled = ((length - self.stick_deadzone) / (1.0 - self.stick_deadzone)).min(1.0);
        raw / length * scaled
    }

    /// 帧末：清「刚按下 / 刚松开」。
    pub fn end_frame(&mut self) {
        for pad in &mut self.pads {
            pad.buttons.end_frame();
        }
    }

    /// 失焦时：所有键当松开（摇杆值保留——下一次事件会更新它）。
    pub fn reset(&mut self) {
        for pad in &mut self.pads {
            pad.buttons.reset();
        }
    }
}

/// 从系统读手柄（gilrs：Windows 上是 Windows.Gaming.Input，Linux 上是 evdev，macOS 上是 IOKit）。
pub struct GamepadPoller {
    gilrs: gilrs::Gilrs,
}

impl std::fmt::Debug for GamepadPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GamepadPoller")
    }
}

impl GamepadPoller {
    /// 打开系统的手柄接口。没有可用后端（无头服务器、沙盒）时返回 `None`，游戏照常跑，只是没有手柄。
    pub fn new() -> Option<Self> {
        match gilrs::Gilrs::new() {
            Ok(gilrs) => Some(Self { gilrs }),
            Err(gilrs::Error::NotImplemented(gilrs)) => {
                // 这个平台没有手柄后端：拿到的是个空实现，留着也无妨，但没意义。
                drop(gilrs);
                None
            }
            Err(_) => None,
        }
    }

    /// 把积压的系统事件翻译进 `gamepads`。每帧开头调一次（kapp 自动做）。
    pub fn poll(&mut self, gamepads: &mut Gamepads) {
        // 第一次调用前已经插着的手柄不发 Connected 事件，补一次。
        for (id, pad) in self.gilrs.gamepads() {
            if gamepads.get(usize::from(id)).is_none() {
                gamepads.connect(usize::from(id), pad.name());
            }
        }
        while let Some(gilrs::Event { id: gid, event, .. }) = self.gilrs.next_event() {
            let id = usize::from(gid);
            match event {
                gilrs::EventType::Connected => {
                    let name = self.gilrs.gamepad(gid).name().to_string();
                    gamepads.connect(id, name);
                }
                gilrs::EventType::Disconnected => gamepads.disconnect(id),
                gilrs::EventType::ButtonPressed(button, _) => {
                    if let Some(button) = map_button(button) {
                        gamepads.press(id, button);
                    }
                }
                gilrs::EventType::ButtonReleased(button, _) => {
                    if let Some(button) = map_button(button) {
                        gamepads.release(id, button);
                    }
                }
                // 扳机在多数手柄上是「带模拟量的按键」。
                gilrs::EventType::ButtonChanged(gilrs::Button::LeftTrigger2, value, _) => {
                    gamepads.set_axis(id, GamepadAxis::LeftTrigger, value)
                }
                gilrs::EventType::ButtonChanged(gilrs::Button::RightTrigger2, value, _) => {
                    gamepads.set_axis(id, GamepadAxis::RightTrigger, value)
                }
                gilrs::EventType::AxisChanged(axis, value, _) => {
                    let axis = match axis {
                        gilrs::Axis::LeftStickX => Some(GamepadAxis::LeftStickX),
                        gilrs::Axis::LeftStickY => Some(GamepadAxis::LeftStickY),
                        gilrs::Axis::RightStickX => Some(GamepadAxis::RightStickX),
                        gilrs::Axis::RightStickY => Some(GamepadAxis::RightStickY),
                        gilrs::Axis::LeftZ => Some(GamepadAxis::LeftTrigger),
                        gilrs::Axis::RightZ => Some(GamepadAxis::RightTrigger),
                        _ => None,
                    };
                    if let Some(axis) = axis {
                        gamepads.set_axis(id, axis, value);
                    }
                }
                _ => {}
            }
        }
    }
}

fn map_button(button: gilrs::Button) -> Option<GamepadButton> {
    use gilrs::Button as B;
    Some(match button {
        B::South => GamepadButton::South,
        B::East => GamepadButton::East,
        B::North => GamepadButton::North,
        B::West => GamepadButton::West,
        B::LeftTrigger => GamepadButton::LeftBumper,
        B::RightTrigger => GamepadButton::RightBumper,
        B::LeftTrigger2 => GamepadButton::LeftTrigger,
        B::RightTrigger2 => GamepadButton::RightTrigger,
        B::Select => GamepadButton::Select,
        B::Start => GamepadButton::Start,
        B::Mode => GamepadButton::Mode,
        B::LeftThumb => GamepadButton::LeftStick,
        B::RightThumb => GamepadButton::RightStick,
        B::DPadUp => GamepadButton::DPadUp,
        B::DPadDown => GamepadButton::DPadDown,
        B::DPadLeft => GamepadButton::DPadLeft,
        B::DPadRight => GamepadButton::DPadRight,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stick_deadzone_is_radial_and_rescaled() {
        let mut pads = Gamepads::default();
        pads.connect(0, "test");
        // 死区里：当没推。
        pads.set_axis(0, GamepadAxis::LeftStickX, 0.1);
        assert_eq!(pads.stick(true), Vec2::ZERO);
        // 推满：长度 1。
        pads.set_axis(0, GamepadAxis::LeftStickX, 1.0);
        assert!((pads.stick(true).length() - 1.0).abs() < 1e-5);
        // 刚出死区：从 0 平滑起步，不是一下跳到 0.15。
        pads.set_axis(0, GamepadAxis::LeftStickX, 0.16);
        assert!(pads.axis(GamepadAxis::LeftStickX) < 0.02);
        // 斜着推 0.12 + 0.12（长度 0.17 > 死区）：单轴读数不被逐轴死区吃掉。
        pads.set_axis(0, GamepadAxis::LeftStickX, 0.12);
        pads.set_axis(0, GamepadAxis::LeftStickY, 0.12);
        assert!(pads.axis(GamepadAxis::LeftStickX) > 0.0);
    }

    #[test]
    fn buttons_have_edges_and_disconnect_releases_them() {
        let mut pads = Gamepads::default();
        pads.press(3, GamepadButton::South);
        assert!(pads.just_pressed(GamepadButton::South) && pads.pressed(GamepadButton::South));
        pads.end_frame();
        assert!(!pads.just_pressed(GamepadButton::South) && pads.pressed(GamepadButton::South));
        pads.disconnect(3);
        assert!(!pads.pressed(GamepadButton::South));
    }

    #[test]
    fn triggers_are_zero_to_one_with_their_own_deadzone() {
        let mut pads = Gamepads::default();
        pads.set_axis(0, GamepadAxis::RightTrigger, 0.03);
        assert_eq!(pads.axis(GamepadAxis::RightTrigger), 0.0);
        pads.set_axis(0, GamepadAxis::RightTrigger, 2.0);
        assert!((pads.axis(GamepadAxis::RightTrigger) - 1.0).abs() < 1e-5);
    }
}
