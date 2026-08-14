//! 按钮状态追踪。

use fxhash::FxHashSet;
use std::hash::Hash;

/// 一组按钮的当前状态。
///
/// 区分「按住」与「本帧刚按下 / 刚松开」：前者每帧都为真，后者只在状态发生变化的那一帧为真。
#[derive(Debug, Clone)]
pub struct ButtonState<T: Copy + Eq + Hash> {
    pressed: FxHashSet<T>,
    just_pressed: FxHashSet<T>,
    just_released: FxHashSet<T>,
}

impl<T: Copy + Eq + Hash> Default for ButtonState<T> {
    fn default() -> Self {
        Self {
            pressed: FxHashSet::default(),
            just_pressed: FxHashSet::default(),
            just_released: FxHashSet::default(),
        }
    }
}

impl<T: Copy + Eq + Hash> ButtonState<T> {
    /// 记录一次按下。重复按下（系统按键重复）不会再次触发 `just_pressed`。
    pub fn press(&mut self, button: T) {
        if self.pressed.insert(button) {
            self.just_pressed.insert(button);
        }
    }

    /// 记录一次松开。
    pub fn release(&mut self, button: T) {
        if self.pressed.remove(&button) {
            self.just_released.insert(button);
        }
    }

    /// 按钮当前是否被按住。
    pub fn pressed(&self, button: T) -> bool {
        self.pressed.contains(&button)
    }

    /// 按钮是否在本帧刚被按下。
    pub fn just_pressed(&self, button: T) -> bool {
        self.just_pressed.contains(&button)
    }

    /// 按钮是否在本帧刚被松开。
    pub fn just_released(&self, button: T) -> bool {
        self.just_released.contains(&button)
    }

    /// 是否有任意一个给定按钮被按住。
    pub fn any_pressed(&self, buttons: impl IntoIterator<Item = T>) -> bool {
        buttons.into_iter().any(|b| self.pressed(b))
    }

    /// 当前按住的所有按钮。
    pub fn all_pressed(&self) -> impl Iterator<Item = T> + '_ {
        self.pressed.iter().copied()
    }

    /// 清空本帧的「刚按下 / 刚松开」，由引擎在每帧末调用。
    pub fn end_frame(&mut self) {
        self.just_pressed.clear();
        self.just_released.clear();
    }

    /// 全部重置。窗口失焦时使用，避免按键卡住。
    pub fn reset(&mut self) {
        // 失焦时把按住的键视作松开，否则回到窗口后它们会一直是「按住」。
        for button in self.pressed.drain() {
            self.just_released.insert(button);
        }
        self.just_pressed.clear();
    }
}
