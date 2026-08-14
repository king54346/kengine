//! 动作与轴映射：把具体按键抽象成语义化的名字。

use fxhash::FxHashMap;
use winit::{event::MouseButton, keyboard::KeyCode};

/// 一个可绑定到动作上的物理输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// 键盘按键。
    Key(KeyCode),
    /// 鼠标按键。
    Mouse(MouseButton),
}

impl From<KeyCode> for Binding {
    fn from(value: KeyCode) -> Self {
        Self::Key(value)
    }
}

impl From<MouseButton> for Binding {
    fn from(value: MouseButton) -> Self {
        Self::Mouse(value)
    }
}

/// 一个轴：正负两个方向各自绑定若干输入，读数为 `-1.0`、`0.0` 或 `1.0`。
#[derive(Debug, Default, Clone)]
pub struct AxisBinding {
    /// 使读数为正的输入。
    pub positive: Vec<Binding>,
    /// 使读数为负的输入。
    pub negative: Vec<Binding>,
}

/// 动作与轴的映射表。
///
/// 游戏逻辑里查询 `"jump"` 而不是 `KeyCode::Space`，改键位时只需改这张表。
#[derive(Debug, Default, Clone)]
pub struct Bindings {
    actions: FxHashMap<String, Vec<Binding>>,
    axes: FxHashMap<String, AxisBinding>,
}

impl Bindings {
    /// 创建空映射表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 给动作添加一个绑定。同一动作可绑定多个输入，任意一个触发即算触发。
    pub fn bind_action(&mut self, action: impl Into<String>, binding: impl Into<Binding>) {
        self.actions
            .entry(action.into())
            .or_default()
            .push(binding.into());
    }

    /// 链式版本的 [`Bindings::bind_action`]。
    pub fn with_action(mut self, action: impl Into<String>, binding: impl Into<Binding>) -> Self {
        self.bind_action(action, binding);
        self
    }

    /// 绑定一个轴的正负方向。
    pub fn bind_axis(
        &mut self,
        axis: impl Into<String>,
        positive: impl Into<Binding>,
        negative: impl Into<Binding>,
    ) {
        let entry = self.axes.entry(axis.into()).or_default();
        entry.positive.push(positive.into());
        entry.negative.push(negative.into());
    }

    /// 链式版本的 [`Bindings::bind_axis`]。
    pub fn with_axis(
        mut self,
        axis: impl Into<String>,
        positive: impl Into<Binding>,
        negative: impl Into<Binding>,
    ) -> Self {
        self.bind_axis(axis, positive, negative);
        self
    }

    /// 解除一个动作的全部绑定。
    pub fn clear_action(&mut self, action: &str) {
        self.actions.remove(action);
    }

    /// 查询某个动作绑定的输入。
    pub fn action(&self, action: &str) -> Option<&[Binding]> {
        self.actions.get(action).map(|v| v.as_slice())
    }

    /// 查询某个轴的绑定。
    pub fn axis(&self, axis: &str) -> Option<&AxisBinding> {
        self.axes.get(axis)
    }
}
