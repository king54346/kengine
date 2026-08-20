//! 事件路由：谁被指到了、谁被按下了、谁有焦点。
//!
//! # 跨帧状态按 id 存
//!
//! 悬停、按下、焦点这些都要跨帧记住，但 UI 树每帧重建。所以状态存在
//! 一张按 [`Id`] 索引的表里，而不是挂在节点上。
//!
//! id 由调用方给的字符串生成而不是自动编号，理由见 [`Id`]：自动编号会
//! 随树结构变化整体错位，插一个节点就能让焦点跑到别人身上。
//!
//! # 「按下」要配对
//!
//! 一次点击不是「鼠标抬起时指着谁」，而是**按下和抬起指的是同一个**。
//! 不配对的话，在别处按下、拖到按钮上松手也会触发点击——
//! 这正是所有 UI 都允许用户「按下后拖开取消」的原因。

use crate::{Id, Rect};
use kmath::Vec2;
use std::collections::HashMap;

/// 鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// 左键。
    Primary,
    /// 右键。
    Secondary,
    /// 中键。
    Middle,
}

/// 一次编辑动作。文本框认这些，不认具体按键。
///
/// 抽一层是因为按键到动作的映射跟平台走（macOS 上行首是 Cmd+←），
/// 而文本框不该知道这件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    /// 退格。
    Backspace,
    /// 删除。
    Delete,
    /// 左移。`select` 为真时扩展选区。
    Left { select: bool },
    /// 右移。
    Right { select: bool },
    /// 跳到行首。
    Home { select: bool },
    /// 跳到行尾。
    End { select: bool },
    /// 全选。
    SelectAll,
    /// 提交（回车）。
    Submit,
    /// 放弃焦点（Esc）。
    Cancel,
}

/// 一帧的输入。由调用方（通常是 `kapp`）填。
#[derive(Debug, Clone, Default)]
pub struct UiInput {
    /// 指针位置（逻辑像素）。指针不在窗口里时为 `None`。
    pub pointer: Option<Vec2>,
    /// 本帧刚按下的键。
    pub pressed: Vec<PointerButton>,
    /// 本帧刚松开的键。
    pub released: Vec<PointerButton>,
    /// 滚轮增量。
    pub scroll: Vec2,
    /// 本帧输入的文本（已经过输入法合成）。
    pub text: String,
    /// 本帧按下的 Tab 数：正数往后走焦点，负数（Shift+Tab）往前。
    pub focus_step: i32,
    /// 本帧的编辑动作，按发生顺序。
    pub edits: Vec<EditAction>,
}

impl UiInput {
    /// 清掉「刚按下 / 刚松开」这类一帧有效的量，保留指针位置。
    pub fn end_frame(&mut self) {
        self.pressed.clear();
        self.released.clear();
        self.scroll = Vec2::ZERO;
        self.text.clear();
        self.focus_step = 0;
        self.edits.clear();
    }

    /// 某个键本帧刚按下。
    pub fn just_pressed(&self, button: PointerButton) -> bool {
        self.pressed.contains(&button)
    }

    /// 某个键本帧刚松开。
    pub fn just_released(&self, button: PointerButton) -> bool {
        self.released.contains(&button)
    }
}

/// 一个控件本帧的交互结果。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Response {
    /// 控件的矩形。
    pub rect: Rect,
    /// 指针正指着它。
    pub hovered: bool,
    /// 正被按住（在它身上按下且还没松手）。
    pub held: bool,
    /// 本帧完成了一次点击：按下和松开都在它身上。
    pub clicked: bool,
    /// 它有键盘焦点。
    pub focused: bool,
    /// 本帧被拖动了多少。没在拖时为零。
    pub drag: Vec2,
}

/// 跨帧的交互状态。
#[derive(Debug, Default)]
pub struct Interaction {
    /// 本帧指针指着的控件。
    hovered: Option<Id>,
    /// 被按住的控件，以及按下那一刻的指针位置。
    active: Option<(Id, Vec2)>,
    /// 有键盘焦点的控件。
    focused: Option<Id>,
    /// 上一帧的指针位置，用来算拖动增量。
    last_pointer: Option<Vec2>,
    /// 本帧算出的各控件结果。
    responses: HashMap<Id, Response>,
    /// 本帧参与命中的控件，按前序——焦点用 Tab 走的就是这个顺序。
    focusable: Vec<Id>,
}

impl Interaction {
    /// 空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前有焦点的控件。
    pub fn focused(&self) -> Option<Id> {
        self.focused
    }

    /// 直接设置焦点。
    pub fn focus(&mut self, id: Option<Id>) {
        self.focused = id;
    }

    /// 指针指着的控件。
    pub fn hovered(&self) -> Option<Id> {
        self.hovered
    }

    /// 取一个控件的结果。没参与本帧布局的返回默认值（什么都没发生）。
    pub fn response(&self, id: Id) -> Response {
        self.responses.get(&id).copied().unwrap_or_default()
    }

    /// 本帧有没有任何控件被指着。
    ///
    /// 游戏逻辑靠它决定要不要吃掉这次点击——鼠标在菜单上时，
    /// 点击不该同时打到场景里去。
    pub fn wants_pointer(&self) -> bool {
        self.hovered.is_some() || self.active.is_some()
    }

    /// 有控件正在接收键盘输入（文本框）。
    ///
    /// 为真时游戏不该再处理 WASD——否则打字会让角色乱跑。
    pub fn wants_keyboard(&self) -> bool {
        self.focused.is_some()
    }

    /// 按本帧的布局与输入更新一遍状态。
    ///
    /// `hit_order` 是可交互控件按**前序**排列的 (id, 矩形)；
    /// 命中判定从后往前（后画的在上面）。
    pub fn update(&mut self, hit_order: &[(Id, Rect)], input: &UiInput) {
        self.responses.clear();
        self.focusable.clear();
        self.focusable.extend(hit_order.iter().map(|(id, _)| *id));

        // 命中：从后往前找第一个包含指针的。
        // 从前往后的话，点在按钮上会命中它底下的面板。
        self.hovered = input.pointer.and_then(|p| {
            hit_order
                .iter()
                .rev()
                .find(|(_, rect)| rect.contains(p))
                .map(|(id, _)| *id)
        });

        // 按下：记住是在谁身上按的。
        if input.just_pressed(PointerButton::Primary)
            && let Some(pointer) = input.pointer
        {
            self.active = self.hovered.map(|id| (id, pointer));
            // 点在空白处要清掉焦点，否则文本框永远抢着键盘。
            self.focused = self.hovered.filter(|id| self.focusable.contains(id));
        }

        // Tab 切焦点。
        if input.focus_step != 0 && !self.focusable.is_empty() {
            let current = self
                .focused
                .and_then(|id| self.focusable.iter().position(|f| *f == id));
            let count = self.focusable.len() as i32;
            let next = match current {
                Some(index) => (index as i32 + input.focus_step).rem_euclid(count),
                // 还没有焦点时，Shift+Tab 从末尾进，Tab 从开头进。
                None if input.focus_step > 0 => 0,
                None => count - 1,
            };
            self.focused = Some(self.focusable[next as usize]);
        }

        let released = input.just_released(PointerButton::Primary);
        let drag = match (input.pointer, self.last_pointer) {
            (Some(now), Some(before)) => now - before,
            _ => Vec2::ZERO,
        };

        for (id, rect) in hit_order {
            let hovered = self.hovered == Some(*id);
            let held = self.active.map(|(a, _)| a) == Some(*id);
            self.responses.insert(
                *id,
                Response {
                    rect: *rect,
                    hovered,
                    held,
                    // 点击要求按下和松开在**同一个**控件上。不配对的话，
                    // 在别处按下、拖到按钮上松手也会触发。
                    clicked: held && released && hovered,
                    focused: self.focused == Some(*id),
                    drag: if held { drag } else { Vec2::ZERO },
                },
            );
        }

        if released {
            self.active = None;
        }
        self.last_pointer = input.pointer;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> Id {
        Id::new(name)
    }

    /// 两个不重叠的按钮，外加一个盖住它们的面板（排在最前 = 最底下）。
    fn layout() -> Vec<(Id, Rect)> {
        vec![
            (id("panel"), Rect::new(0.0, 0.0, 200.0, 100.0)),
            (id("a"), Rect::new(10.0, 10.0, 80.0, 30.0)),
            (id("b"), Rect::new(10.0, 50.0, 80.0, 30.0)),
        ]
    }

    fn at(x: f32, y: f32) -> UiInput {
        UiInput {
            pointer: Some(Vec2::new(x, y)),
            ..Default::default()
        }
    }

    #[test]
    fn hovering_picks_the_topmost() {
        // 从前往后找的话，点在按钮上会命中它底下的面板。
        let mut ui = Interaction::new();
        ui.update(&layout(), &at(20.0, 20.0));
        assert_eq!(ui.hovered(), Some(id("a")));

        ui.update(&layout(), &at(150.0, 90.0));
        assert_eq!(ui.hovered(), Some(id("panel")));
    }

    #[test]
    fn nothing_is_hovered_outside_the_tree() {
        let mut ui = Interaction::new();
        ui.update(&layout(), &at(500.0, 500.0));
        assert_eq!(ui.hovered(), None);
        assert!(!ui.wants_pointer());
    }

    #[test]
    fn a_missing_pointer_hovers_nothing() {
        // 鼠标移出窗口时不该还留着上一帧的悬停高亮。
        let mut ui = Interaction::new();
        ui.update(&layout(), &at(20.0, 20.0));
        ui.update(&layout(), &UiInput::default());
        assert_eq!(ui.hovered(), None);
    }

    #[test]
    fn a_full_press_and_release_is_a_click() {
        let mut ui = Interaction::new();

        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);
        assert!(ui.response(id("a")).held);
        assert!(!ui.response(id("a")).clicked, "还没松手，不算点击");

        let mut input = at(20.0, 20.0);
        input.released.push(PointerButton::Primary);
        ui.update(&layout(), &input);
        assert!(ui.response(id("a")).clicked);
    }

    #[test]
    fn dragging_away_before_release_cancels_the_click() {
        // 所有 UI 都允许「按下后拖开取消」。不配对的话用户没法反悔。
        let mut ui = Interaction::new();

        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        // 拖到另一个控件上再松手。
        let mut input = at(20.0, 60.0);
        input.released.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        assert!(!ui.response(id("a")).clicked, "拖开之后不该算点击");
        assert!(!ui.response(id("b")).clicked, "也不该算到拖到的那个头上");
    }

    #[test]
    fn pressing_elsewhere_and_releasing_on_a_button_is_not_a_click() {
        let mut ui = Interaction::new();

        let mut input = at(150.0, 90.0); // 面板空白处
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        let mut input = at(20.0, 20.0); // 松在按钮上
        input.released.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        assert!(!ui.response(id("a")).clicked);
    }

    #[test]
    fn holding_survives_the_pointer_leaving() {
        // 按住滑条把鼠标拖出控件外，滑条应当继续跟手。
        let mut ui = Interaction::new();

        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        ui.update(&layout(), &at(500.0, 500.0));
        assert!(ui.response(id("a")).held, "拖出去之后仍然该是按住状态");
        assert!(ui.wants_pointer(), "按住期间指针仍然归 UI");
    }

    #[test]
    fn drag_reports_the_delta_only_while_held() {
        let mut ui = Interaction::new();

        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        ui.update(&layout(), &at(35.0, 28.0));
        assert_eq!(ui.response(id("a")).drag, Vec2::new(15.0, 8.0));
        assert_eq!(ui.response(id("b")).drag, Vec2::ZERO, "没被按的不该有拖动");
    }

    #[test]
    fn clicking_a_widget_focuses_it() {
        let mut ui = Interaction::new();
        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);
        assert_eq!(ui.focused(), Some(id("a")));
    }

    #[test]
    fn clicking_empty_space_clears_focus() {
        // 不清的话文本框会永远抢着键盘，游戏里再也走不动路。
        let mut ui = Interaction::new();
        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);
        assert!(ui.wants_keyboard());

        let mut input = at(500.0, 500.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);
        assert_eq!(ui.focused(), None);
        assert!(!ui.wants_keyboard());
    }

    #[test]
    fn tab_walks_focus_forward_and_wraps() {
        let mut ui = Interaction::new();
        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };

        ui.update(&layout(), &tab);
        assert_eq!(ui.focused(), Some(id("panel")));
        ui.update(&layout(), &tab);
        assert_eq!(ui.focused(), Some(id("a")));
        ui.update(&layout(), &tab);
        assert_eq!(ui.focused(), Some(id("b")));
        ui.update(&layout(), &tab);
        assert_eq!(ui.focused(), Some(id("panel")), "该绕回开头");
    }

    #[test]
    fn shift_tab_walks_backward() {
        let mut ui = Interaction::new();
        let back = UiInput {
            focus_step: -1,
            ..Default::default()
        };

        // 还没有焦点时 Shift+Tab 从末尾进。
        ui.update(&layout(), &back);
        assert_eq!(ui.focused(), Some(id("b")));
        ui.update(&layout(), &back);
        assert_eq!(ui.focused(), Some(id("a")));
    }

    #[test]
    fn focus_survives_across_frames() {
        // UI 树每帧重建，状态却要留住。挂在节点上就做不到。
        let mut ui = Interaction::new();
        let mut input = at(20.0, 20.0);
        input.pressed.push(PointerButton::Primary);
        ui.update(&layout(), &input);

        for _ in 0..10 {
            ui.update(&layout(), &UiInput::default());
        }
        assert_eq!(ui.focused(), Some(id("a")));
    }

    #[test]
    fn an_unknown_id_reports_nothing_happened() {
        let ui = Interaction::new();
        let response = ui.response(id("从未存在"));
        assert!(!response.hovered && !response.clicked && !response.held);
    }

    #[test]
    fn end_frame_clears_one_shot_input_but_keeps_the_pointer() {
        let mut input = at(10.0, 10.0);
        input.pressed.push(PointerButton::Primary);
        input.text.push('a');
        input.edits.push(EditAction::Backspace);
        input.scroll = Vec2::new(0.0, 3.0);
        input.focus_step = 1;

        input.end_frame();

        assert!(input.pressed.is_empty());
        assert!(input.text.is_empty());
        assert!(input.edits.is_empty());
        assert_eq!(input.scroll, Vec2::ZERO);
        assert_eq!(input.focus_step, 0);
        assert_eq!(
            input.pointer,
            Some(Vec2::new(10.0, 10.0)),
            "指针位置是持续量"
        );
    }
}
