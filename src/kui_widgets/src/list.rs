//! 列表。选中项由调用方保管，单选多选都由它决定。
//!
//! # 控件报告动作，不报告结果
//!
//! 列表不告诉你「现在选中的是第 3 项」，而是告诉你「用户要求把选择
//! 换成第 3 项」——[`ListAction`]。差别在多选：按住 Shift 点第五行的
//! 意思是「从锚点到第五行整段都选上」，而锚点是**上一次落点**，只有
//! 控件跨帧记着。让调用方自己算的话，它得把锚点也存一份，而那份状态
//! 除了喂回给控件之外没有任何用处。
//!
//! 单选的调用方可以只看 [`ListAction::Set`]，别的两种当没有。

use std::collections::BTreeSet;

use kfont::TextStyle;
use kmath::Vec2;
use kui::{Id, NavKey, Rect, Response, Ui, UiInput};

use crate::widgets::{GroupKind, Theme, Widget, WidgetUi, text_style};

/// 列表这一帧要求怎么改选中集合。下标是行在列表里的**声明顺序**。
///
/// ```
/// use kui_widgets::list::ListAction;
/// use std::collections::BTreeSet;
///
/// let mut selection = BTreeSet::from([0, 5]);
/// ListAction::Range(1, 3).apply(&mut selection);
/// assert_eq!(selection, BTreeSet::from([1, 2, 3]));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListAction {
    /// 只选这一项，别的全清掉。普通点击、方向键。
    Set(usize),
    /// 翻转这一项，别的不动。Ctrl+点击、Ctrl+空格。
    Toggle(usize),
    /// 选中这一段（**含两端**，两个下标不分先后），别的清掉。Shift+点击。
    Range(usize, usize),
}

impl ListAction {
    /// 把这次动作应用到一个选中集合上。
    ///
    /// 只是个方便：单选的调用方直接 `match` 出 [`Set`](Self::Set) 的
    /// 下标就够了，用不着建集合。
    pub fn apply(&self, selection: &mut BTreeSet<usize>) {
        match *self {
            ListAction::Set(index) => {
                selection.clear();
                selection.insert(index);
            }
            ListAction::Toggle(index) => {
                if !selection.remove(&index) {
                    selection.insert(index);
                }
            }
            ListAction::Range(from, to) => {
                selection.clear();
                // 两端不分先后：往上拖和往下拖选出来该是同一段。
                let (low, high) = if from <= to { (from, to) } else { (to, from) };
                selection.extend(low..=high);
            }
        }
    }
}

impl WidgetUi {
    /// 开一个列表：方向键在行间走，Tab 一下跨过整个列表。
    ///
    /// 结果用 [`list_action`](Self::list_action) 查：
    ///
    /// ```no_run
    /// # use kui_widgets::WidgetUi;
    /// # use std::collections::BTreeSet;
    /// # let mut w = WidgetUi::default();
    /// # let files = ["a", "b"];
    /// # let mut selection = BTreeSet::new();
    /// let list = w.begin_list("files");
    /// for (index, name) in files.iter().enumerate() {
    ///     w.list_item(&format!("f{index}"), *name, selection.contains(&index));
    /// }
    /// w.end_list();
    ///
    /// if let Some(action) = w.list_action(list) {
    ///     action.apply(&mut selection);
    /// }
    /// ```
    ///
    /// 不开列表也能用 [`list_item`](Self::list_item)——那样每一行是
    /// 独立的一站，只认点击和回车，没有方向键。
    pub fn begin_list(&mut self, id: &str) -> Id {
        self.begin_group(id, GroupKind::List)
    }

    /// 收一个列表。
    pub fn end_list(&mut self) {
        self.end_group();
    }

    /// 一个列表这一帧要求做什么。没动静时是 [`None`]。
    ///
    /// 和别的控件一样**滞后一帧**：查到的是上一次 `finish` 的结果。
    pub fn list_action(&self, list: Id) -> Option<ListAction> {
        self.list_actions.get(&list).copied()
    }

    /// 列表里的一行。
    ///
    /// 选中状态由调用方保管。多选时就存一个集合——控件这一层不需要
    /// 知道是单选还是多选。
    pub fn list_item(&mut self, id: &str, text: impl Into<String>, selected: bool) -> Id {
        self.push(
            id,
            Widget::ListItem {
                text: text.into(),
                selected,
            },
        )
    }

    /// 一个列表的锚点：按住 Shift 选一段时从哪一项算起。
    ///
    /// 还没有锚点时就用 `fallback` 并记下——第一次操作就按住 Shift
    /// 的话，那一下等于只选它自己。
    fn list_anchor(&mut self, list: Id, fallback: usize) -> usize {
        *self.list_anchors.entry(list).or_insert(fallback)
    }

    /// 把锚点挪到这一项。
    fn set_list_anchor(&mut self, list: Id, index: usize) {
        self.list_anchors.insert(list, index);
    }
}

/// 列表的键盘与点击。
///
/// `items` 是各行的 id 与选中状态，按声明顺序。
pub(crate) fn navigate(w: &mut WidgetUi, list: Id, items: &[(Id, bool)], input: &UiInput) {
    let count = items.len();
    let current = w
        .interaction
        .focused()
        .and_then(|focused| items.iter().position(|(id, _)| *id == focused));

    // ── 方向键 ──
    if let Some(mut index) = current {
        let mut moved = false;
        for key in &input.nav {
            index = match key {
                NavKey::Up | NavKey::Left => (index + count - 1) % count,
                NavKey::Down | NavKey::Right => (index + 1) % count,
                NavKey::Home => 0,
                NavKey::End => count - 1,
                // Esc 归外面的对话框 / 菜单管，别在这里吃掉。
                NavKey::Escape => continue,
            };
            moved = true;
        }
        if moved {
            w.interaction.focus_now(items[index].0);
            // **按住 Ctrl 时只挪高亮、不改选择**。这是从一堆不相邻的
            // 行里挑几个的唯一办法：挪过去、Ctrl+空格加选、再挪。
            // 一路改选择的话，走到哪儿前面挑好的就全没了。
            if !input.ctrl {
                let action = if input.shift {
                    ListAction::Range(w.list_anchor(list, index), index)
                } else {
                    w.set_list_anchor(list, index);
                    ListAction::Set(index)
                };
                w.list_actions.insert(list, action);
            }
            // 方向键这一帧就到此为止：同一帧里既走了方向键又点了鼠标
            // 是不会发生的，硬要两边都算只会让动作互相覆盖。
            return;
        }
    }

    // ── 点击，以及有焦点时的回车 / 空格 ──
    let Some(index) = items
        .iter()
        .position(|(id, _)| w.interaction.response(*id).clicked)
    else {
        return;
    };

    let action = if input.shift {
        // Shift 段选**不挪锚点**：按住 Shift 连点几行，每一下都该
        // 从同一个起点重新量，而不是把上一次的终点当新起点。
        ListAction::Range(w.list_anchor(list, index), index)
    } else if input.ctrl {
        w.set_list_anchor(list, index);
        ListAction::Toggle(index)
    } else {
        w.set_list_anchor(list, index);
        ListAction::Set(index)
    };
    w.list_actions.insert(list, action);
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    ui.measure(text, &text_style(theme.font_size), None).size
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    selected: bool,
) {
    // 选中的整行铺主色。悬停只铺一层浅的——两者叠在
    // 一起时选中要压过悬停，否则鼠标扫过就看不出选了谁。
    if selected {
        ui.rounded_rect(rect, theme.radius * 0.5, theme.accent);
    } else if response.hovered {
        ui.rounded_rect(rect, theme.radius * 0.5, theme.hovered);
    }
    if response.focused && !selected {
        ui.border(rect, theme.radius * 0.5, 1.0, theme.focus);
    }
    ui.text(
        Vec2::new(rect.min.x, rect.center().y - theme.font_size * 0.6),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
        Some(rect.size().x),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{activate_first, click_at, ui};
    use kui::UiInput;

    /// 三行的列表。声明一次，测试里反复调用。
    fn declare(w: &mut WidgetUi, selection: &BTreeSet<usize>) -> Id {
        let list = w.begin_list("files");
        for index in 0..3 {
            w.list_item(
                &format!("f{index}"),
                format!("第 {index} 行"),
                selection.contains(&index),
            );
        }
        w.end_list();
        list
    }

    /// 跑一帧，返回列表 id。
    fn frame(
        w: &mut WidgetUi,
        ui: &mut kui::Ui,
        selection: &BTreeSet<usize>,
        input: &UiInput,
    ) -> Id {
        w.begin();
        let list = declare(w, selection);
        w.finish(ui, input);
        list
    }

    fn nav(key: NavKey) -> UiInput {
        UiInput {
            nav: vec![key],
            ..Default::default()
        }
    }

    fn tab() -> UiInput {
        UiInput {
            focus_step: 1,
            ..Default::default()
        }
    }

    #[test]
    fn apply_set_replaces_the_whole_selection() {
        let mut selection = BTreeSet::from([0, 1, 2]);
        ListAction::Set(1).apply(&mut selection);
        assert_eq!(selection, BTreeSet::from([1]));
    }

    #[test]
    fn apply_toggle_flips_one_row() {
        let mut selection = BTreeSet::from([0]);
        ListAction::Toggle(1).apply(&mut selection);
        assert_eq!(selection, BTreeSet::from([0, 1]));
        ListAction::Toggle(0).apply(&mut selection);
        assert_eq!(selection, BTreeSet::from([1]));
    }

    /// 段选两端不分先后：往上拖和往下拖选出来该是同一段。
    #[test]
    fn apply_range_is_symmetric() {
        let mut down = BTreeSet::new();
        ListAction::Range(1, 3).apply(&mut down);
        let mut up = BTreeSet::new();
        ListAction::Range(3, 1).apply(&mut up);
        assert_eq!(down, up);
        assert_eq!(down, BTreeSet::from([1, 2, 3]));
    }

    /// 有焦点的列表行认回车 / 空格。
    #[test]
    fn a_focused_list_row_can_be_selected_from_the_keyboard() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let mut id = None;
        activate_first(&mut w, &mut ui, |w| {
            id = Some(w.list_item("row", "第一行", false));
            w.list_item("row2", "第二行", false);
        });

        assert!(w.response(id.unwrap()).clicked);
    }

    /// 选中的列表行要画出底色。不画的话选了等于没选。
    #[test]
    fn a_selected_list_item_draws_a_background() {
        let count = |selected: bool| {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            w.begin();
            w.list_item("i", "一行", selected);
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();
            ui.draw_list().indices().len()
        };
        assert!(count(true) > count(false));
    }

    /// 方向键在行间走，报告成换选。
    #[test]
    fn arrow_keys_move_the_selection() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut selection = BTreeSet::new();

        // Tab 走进列表，落在第一行。
        let list = frame(&mut w, &mut ui, &selection, &tab());
        assert!(w.response(Id::new("f0")).focused);

        frame(&mut w, &mut ui, &selection, &nav(NavKey::Down));
        assert_eq!(w.list_action(list), Some(ListAction::Set(1)));
        w.list_action(list).unwrap().apply(&mut selection);

        frame(&mut w, &mut ui, &selection, &nav(NavKey::Down));
        assert_eq!(w.list_action(list), Some(ListAction::Set(2)));
    }

    /// 到底了绕回开头。列表通常不长，绕回比停在末尾好用。
    #[test]
    fn arrow_keys_wrap_around() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let selection = BTreeSet::new();

        let list = frame(&mut w, &mut ui, &selection, &tab());
        frame(&mut w, &mut ui, &selection, &nav(NavKey::Up));
        assert_eq!(
            w.list_action(list),
            Some(ListAction::Set(2)),
            "从第一行往上该绕到最后一行"
        );
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let selection = BTreeSet::new();

        let list = frame(&mut w, &mut ui, &selection, &tab());
        frame(&mut w, &mut ui, &selection, &nav(NavKey::End));
        assert_eq!(w.list_action(list), Some(ListAction::Set(2)));
        frame(&mut w, &mut ui, &selection, &nav(NavKey::Home));
        assert_eq!(w.list_action(list), Some(ListAction::Set(0)));
    }

    /// 按住 Shift 用方向键，从锚点拉出一段。
    #[test]
    fn shift_arrow_extends_a_range_from_the_anchor() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut selection = BTreeSet::new();

        let list = frame(&mut w, &mut ui, &selection, &tab());
        // 先普通选中第一行，把锚点定在 0。
        frame(&mut w, &mut ui, &selection, &nav(NavKey::Home));
        w.list_action(list).unwrap().apply(&mut selection);

        let mut input = nav(NavKey::Down);
        input.shift = true;
        frame(&mut w, &mut ui, &selection, &input);
        assert_eq!(w.list_action(list), Some(ListAction::Range(0, 1)));
    }

    /// 按住 Ctrl 用方向键**只挪高亮，不改选择**。
    ///
    /// 这是从一堆不相邻的行里挑几个的唯一办法：挪过去、Ctrl+空格加选、
    /// 再挪。一路改选择的话，走到哪儿前面挑好的就全没了。
    #[test]
    fn ctrl_arrow_moves_the_highlight_without_selecting() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let selection = BTreeSet::from([0]);

        let list = frame(&mut w, &mut ui, &selection, &tab());
        let mut input = nav(NavKey::Down);
        input.ctrl = true;
        frame(&mut w, &mut ui, &selection, &input);

        assert_eq!(w.list_action(list), None, "Ctrl+方向键不该改选择");
        assert!(w.response(Id::new("f1")).focused, "但高亮该挪过去");
    }

    /// 普通点击 = 换选。
    #[test]
    fn clicking_a_row_replaces_the_selection() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let selection = BTreeSet::new();

        let list = frame(&mut w, &mut ui, &selection, &UiInput::default());
        let target = w.response(Id::new("f1")).rect.center();
        click_at(&mut w, &mut ui, target, |w| {
            declare(w, &BTreeSet::new());
        });

        assert_eq!(w.list_action(list), Some(ListAction::Set(1)));
    }

    /// Ctrl+点击 = 加选 / 减选，别的不动。
    #[test]
    fn ctrl_clicking_toggles_one_row() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let selection = BTreeSet::from([0]);

        let list = frame(&mut w, &mut ui, &selection, &UiInput::default());
        let target = w.response(Id::new("f2")).rect.center();

        // 点击要按下、松开两帧。两帧都按着 Ctrl。
        for frame_index in 0..2 {
            let mut input = crate::testing::at(target.x, target.y);
            if frame_index == 0 {
                input.pressed.push(kui::PointerButton::Primary);
            } else {
                input.released.push(kui::PointerButton::Primary);
            }
            input.ctrl = true;
            w.begin();
            declare(&mut w, &selection);
            w.finish(&mut ui, &input);
        }

        assert_eq!(w.list_action(list), Some(ListAction::Toggle(2)));
    }

    /// Shift+点击 = 从锚点选一整段。
    #[test]
    fn shift_clicking_selects_a_range_from_the_anchor() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut selection = BTreeSet::new();

        let list = frame(&mut w, &mut ui, &selection, &UiInput::default());

        // 先普通点第一行，锚点落在 0。
        let first = w.response(Id::new("f0")).rect.center();
        click_at(&mut w, &mut ui, first, |w| {
            declare(w, &BTreeSet::new());
        });
        w.list_action(list).unwrap().apply(&mut selection);
        assert_eq!(selection, BTreeSet::from([0]));

        // 再按住 Shift 点第三行。
        let third = w.response(Id::new("f2")).rect.center();
        for frame_index in 0..2 {
            let mut input = crate::testing::at(third.x, third.y);
            if frame_index == 0 {
                input.pressed.push(kui::PointerButton::Primary);
            } else {
                input.released.push(kui::PointerButton::Primary);
            }
            input.shift = true;
            w.begin();
            declare(&mut w, &selection);
            w.finish(&mut ui, &input);
        }

        assert_eq!(w.list_action(list), Some(ListAction::Range(0, 2)));
    }

    /// 动作只报告一帧。
    ///
    /// 不清的话，一下点击会被当成一直按着——调用方每帧都以为用户又点了
    /// 一次，一个「双击打开」之类的判断会立刻误触发。
    #[test]
    fn an_action_is_reported_for_a_single_frame() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let selection = BTreeSet::new();

        let list = frame(&mut w, &mut ui, &selection, &tab());
        frame(&mut w, &mut ui, &selection, &nav(NavKey::Down));
        assert!(w.list_action(list).is_some());

        frame(&mut w, &mut ui, &selection, &UiInput::default());
        assert_eq!(w.list_action(list), None, "上一帧的动作漏到了这一帧");
    }

    /// Tab 一下跨过整个列表，不是一行一行走。
    #[test]
    fn tab_steps_over_the_whole_list() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare = |w: &mut WidgetUi| {
            w.begin_list("files");
            for index in 0..3 {
                w.list_item(&format!("f{index}"), format!("第 {index} 行"), false);
            }
            w.end_list();
            w.button("after", "之后");
        };

        w.begin();
        declare(&mut w);
        w.finish(&mut ui, &tab());
        assert!(w.response(Id::new("f0")).focused, "该先落在第一行");

        w.begin();
        declare(&mut w);
        w.finish(&mut ui, &tab());
        assert!(
            w.response(Id::new("after")).focused,
            "该一下跨过整个列表，落到后面的按钮上"
        );
    }

    /// 焦点停在列表中间某行时按 Tab，该往后走，不是跳回开头。
    #[test]
    fn tab_leaves_the_list_from_wherever_the_highlight_is() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare = |w: &mut WidgetUi| {
            w.button("before", "之前");
            w.begin_list("files");
            for index in 0..3 {
                w.list_item(&format!("f{index}"), format!("第 {index} 行"), false);
            }
            w.end_list();
            w.button("after", "之后");
        };

        // 走到列表里，再用方向键挪到第三行。
        for _ in 0..2 {
            w.begin();
            declare(&mut w);
            w.finish(&mut ui, &tab());
        }
        w.begin();
        declare(&mut w);
        w.finish(&mut ui, &nav(NavKey::End));
        assert!(w.response(Id::new("f2")).focused);

        w.begin();
        declare(&mut w);
        w.finish(&mut ui, &tab());
        assert!(
            w.response(Id::new("after")).focused,
            "焦点该继续往后走，而不是飞回开头"
        );
    }

    /// 空列表按方向键不该 panic。
    #[test]
    fn an_empty_list_survives_the_arrow_keys() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        for input in [&tab(), &nav(NavKey::Down)] {
            w.begin();
            w.begin_list("empty");
            w.end_list();
            w.finish(&mut ui, input);
        }
    }
}
