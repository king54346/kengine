//! 单选按钮。一组里只能选一个，选中项由调用方保管。
//!
//! # 「组」是干什么的
//!
//! 单个单选按钮画出来就是个圆点，用不着组。组管的是**键盘**：
//!
//! - 方向键在组内换选，到头绕回。
//! - Tab 一下跨过整组，不是一项一项走（[roving tabindex]）。
//!
//! 不分组也能用——那样每个单选按钮就是独立的一站，Tab 一个个走过去。
//! 一组三项还行，一组二十项就得按二十次。
//!
//! [roving tabindex]: https://www.w3.org/WAI/ARIA/apg/patterns/radio/

use kfont::TextStyle;
use kmath::Vec2;
use kui::{Id, NavKey, Rect, Response, Ui, UiInput};

use crate::widgets::{GroupKind, Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 开一个单选组：方向键在组内换选，Tab 一下跨过整组。
    ///
    /// ```no_run
    /// # use kui_widgets::WidgetUi;
    /// # let mut w = WidgetUi::default();
    /// # let mut quality = 0usize;
    /// w.begin_radio_group("quality");
    /// for (index, name) in ["低", "中", "高"].iter().enumerate() {
    ///     let r = w.radio(&format!("q{index}"), *name, quality == index);
    ///     if w.response(r).clicked {
    ///         quality = index;
    ///     }
    /// }
    /// w.end_radio_group();
    /// ```
    ///
    /// 方向键换选也走 `clicked`，所以上面这段**不用**为键盘写第二条
    /// 分支——和按钮的键盘激活是同一个道理。
    pub fn begin_radio_group(&mut self, id: &str) -> Id {
        self.begin_group(id, GroupKind::Radio)
    }

    /// 收一个单选组。
    pub fn end_radio_group(&mut self) {
        self.end_group();
    }

    /// 一个单选按钮。
    ///
    /// 和 [`checkbox`](Self::checkbox) 一样，选中状态由调用方保管——
    /// 一组单选按钮的「选中项」天然是**一个**值，存在每个按钮里反而
    /// 要维护「只有一个为真」这条不变量。
    ///
    /// ```ignore
    /// for (index, name) in ["低", "中", "高"].iter().enumerate() {
    ///     let r = w.radio(&format!("q{index}"), *name, self.quality == index);
    ///     if w.response(r).clicked {
    ///         self.quality = index;
    ///     }
    /// }
    /// ```
    ///
    /// 注意**没有**「取消选中」——点已经选中的那个不会把它取消。
    /// 这是单选组和复选框的根本区别：一组里必须有一个是选中的。
    pub fn radio(&mut self, id: &str, text: impl Into<String>, selected: bool) -> Id {
        self.push(
            id,
            Widget::Radio {
                text: text.into(),
                selected,
            },
        )
    }
}

/// 组内的方向键：换选、绕回。
///
/// 换选直接报告成「这一项被点了」，而不是新开一个「键盘选中」的通道。
/// 这样所有已经写着 `if w.response(r).clicked` 的地方**自动**支持键盘；
/// 分成两个通道的话，每一处调用都得记得处理第二个，漏掉的那些就只有
/// 鼠标能用。
///
/// `items` 是组内各项的 id 与选中状态，按声明顺序。
pub(crate) fn navigate(w: &mut WidgetUi, items: &[(Id, bool)], input: &UiInput) {
    // 焦点不在组里，这些方向键不归它管——可能是旁边的滑条在调值。
    let Some(focused) = w.interaction.focused() else {
        return;
    };
    let Some(mut current) = items.iter().position(|(id, _)| *id == focused) else {
        return;
    };

    let count = items.len();
    let mut moved = false;
    for key in &input.nav {
        current = match key {
            // 上下左右都认。竖排的单选组按左右、横排的按上下，
            // 用户不该先猜对排布方向才能用键盘。
            NavKey::Up | NavKey::Left => (current + count - 1) % count,
            NavKey::Down | NavKey::Right => (current + 1) % count,
            NavKey::Home => 0,
            NavKey::End => count - 1,
            // Esc 归外面的对话框 / 菜单管，别在这里吃掉。
            NavKey::Escape => continue,
        };
        moved = true;
    }

    if moved {
        let id = items[current].0;
        w.interaction.focus_now(id);
        w.interaction.activate(id);
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    let text_size = ui.measure(text, &text_style(theme.font_size), None).size;
    Vec2::new(text_size.x + theme.row_height, text_size.y)
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
    // 和复选框同样大小，但画成圆的。形状是单选和多选
    // 唯一的视觉区别，两者混在一起时全靠它区分。
    let size = theme.row_height * 0.6;
    let radius = size * 0.5;
    let center = Vec2::new(rect.min.x + radius, rect.center().y);
    let outer = Rect {
        min: center - Vec2::splat(radius),
        max: center + Vec2::splat(radius),
    };
    let fill = if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rounded_rect(outer, radius, fill);
    ui.border(outer, radius, 1.0, theme.outline);
    if selected {
        // 圆心一个实心点。用主色而不是白色，好让它和
        // 复选框的白勾在同一个面板里也能区分开。
        let dot = radius * 0.5;
        ui.rounded_rect(
            Rect {
                min: center - Vec2::splat(dot),
                max: center + Vec2::splat(dot),
            },
            dot,
            theme.accent,
        );
    }
    if response.focused {
        ui.border(outer.shrink(-2.0), radius + 2.0, 2.0, theme.focus);
    }
    ui.text(
        Vec2::new(outer.max.x + 8.0, rect.center().y - theme.font_size * 0.6),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
        None,
    );
}

#[cfg(test)]
mod tests {
    use crate::WidgetUi;
    use crate::testing::{activate_first, at, ui};
    use kui::{Id, NavKey, PointerButton, UiInput};

    /// 三项的单选组。
    fn declare(w: &mut WidgetUi, selected: usize) {
        w.begin_radio_group("quality");
        for index in 0..3 {
            w.radio(
                &format!("q{index}"),
                format!("第 {index} 档"),
                index == selected,
            );
        }
        w.end_radio_group();
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

    /// 方向键在组内换选，而且走的是 `clicked`。
    ///
    /// 走 `clicked` 而不是新开一个字段，是为了让所有已经写着
    /// `if response.clicked` 的地方自动支持键盘。
    #[test]
    fn arrow_keys_change_the_selection_in_a_group() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut selected = 0;

        // Tab 走进组，落在选中的那一项上。
        w.begin();
        declare(&mut w, selected);
        w.finish(&mut ui, &tab());
        assert!(w.response(Id::new("q0")).focused);

        w.begin();
        declare(&mut w, selected);
        w.finish(&mut ui, &nav(NavKey::Down));
        assert!(w.response(Id::new("q1")).clicked, "方向键该换选到第二项");
        assert!(w.response(Id::new("q1")).focused, "焦点也该跟过去");
        selected = 1;

        w.begin();
        declare(&mut w, selected);
        w.finish(&mut ui, &nav(NavKey::Up));
        assert!(w.response(Id::new("q0")).clicked);
    }

    /// 到头绕回。
    #[test]
    fn arrow_keys_wrap_around_in_a_group() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        w.begin();
        declare(&mut w, 0);
        w.finish(&mut ui, &tab());

        w.begin();
        declare(&mut w, 0);
        w.finish(&mut ui, &nav(NavKey::Up));
        assert!(
            w.response(Id::new("q2")).clicked,
            "从第一项往上该绕到最后一项"
        );
    }

    #[test]
    fn home_and_end_jump_to_the_ends_of_a_group() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        w.begin();
        declare(&mut w, 1);
        w.finish(&mut ui, &tab());

        w.begin();
        declare(&mut w, 1);
        w.finish(&mut ui, &nav(NavKey::End));
        assert!(w.response(Id::new("q2")).clicked);
    }

    /// Tab 一下跨过整组，落点是**当前选中的**那一项。
    ///
    /// 一组二十个画质选项，不这么做的话用户要按二十次 Tab 才走得过去。
    #[test]
    fn tab_steps_over_the_whole_group_landing_on_the_selected_one() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare_all = |w: &mut WidgetUi| {
            declare(w, 1);
            w.button("after", "之后");
        };

        w.begin();
        declare_all(&mut w);
        w.finish(&mut ui, &tab());
        assert!(
            w.response(Id::new("q1")).focused,
            "该落在选中的那一项上，不是第一项"
        );

        w.begin();
        declare_all(&mut w);
        w.finish(&mut ui, &tab());
        assert!(w.response(Id::new("after")).focused, "该一下跨过整组");
    }

    /// 组里没选中的那些**仍然点得到**。
    ///
    /// 「Tab 不停」和「拿不到焦点」是两件事。混成一件的话，点组里
    /// 没选中的那一项会被当成点在空白处，单选组就成了只能用键盘的控件。
    #[test]
    fn an_unselected_radio_in_a_group_can_still_be_clicked() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        w.begin();
        declare(&mut w, 0);
        w.finish(&mut ui, &UiInput::default());

        let target = w.response(Id::new("q2")).rect.center();
        for frame in 0..2 {
            let mut input = at(target.x, target.y);
            if frame == 0 {
                input.pressed.push(PointerButton::Primary);
            } else {
                input.released.push(PointerButton::Primary);
            }
            w.begin();
            declare(&mut w, 0);
            w.finish(&mut ui, &input);
        }

        assert!(w.response(Id::new("q2")).clicked);
        assert!(w.response(Id::new("q2")).focused, "点了就该拿到焦点");
    }

    /// 焦点不在组里时，方向键不该让组换选。
    ///
    /// 不挡的话，旁边一条滑条在调值的同时，单选组会跟着一起动。
    #[test]
    fn a_group_ignores_arrow_keys_when_focus_is_elsewhere() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare_all = |w: &mut WidgetUi| {
            w.button("before", "之前");
            declare(w, 0);
        };

        // Tab 一下：焦点落在按钮上。
        w.begin();
        declare_all(&mut w);
        w.finish(&mut ui, &tab());
        assert!(w.response(Id::new("before")).focused);

        w.begin();
        declare_all(&mut w);
        w.finish(&mut ui, &nav(NavKey::Down));
        for index in 0..3 {
            assert!(
                !w.response(Id::new(&format!("q{index}"))).clicked,
                "焦点不在组里，第 {index} 项不该被选中"
            );
        }
    }

    /// 空组按方向键不该 panic。
    #[test]
    fn an_empty_group_survives_the_arrow_keys() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        for input in [&tab(), &nav(NavKey::Down)] {
            w.begin();
            w.begin_radio_group("empty");
            w.end_radio_group();
            w.finish(&mut ui, input);
        }
    }

    /// 有焦点的单选按钮认回车 / 空格。
    ///
    /// 这条不依赖组：Tab 走到的那一个，回车就能选中。
    #[test]
    fn a_focused_radio_can_be_selected_from_the_keyboard() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let mut id = None;
        activate_first(&mut w, &mut ui, |w| {
            id = Some(w.radio("r", "简单", false));
        });

        assert!(w.response(id.unwrap()).clicked);
    }

    /// 选中的单选按钮要比没选中的多画一个圆点，否则一组按钮
    /// 看上去全都一样，用户不知道自己选了哪个。
    #[test]
    fn a_selected_radio_draws_more_than_an_unselected_one() {
        let count = |selected: bool| {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            w.begin();
            w.radio("r", "选项", selected);
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();
            ui.draw_list().indices().len()
        };
        assert!(count(true) > count(false));
    }

    /// 一组单选按钮各自独立响应，点第二个不会连带点到第一个。
    #[test]
    fn radios_in_a_group_are_independent() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.radio("a", "甲", true);
        let b = w.radio("b", "乙", false);
        w.finish(&mut ui, &UiInput::default());

        // 点击要按下、松开两帧才算数。
        let target = w.response(b).rect.center();
        for frame in 0..2 {
            let mut input = at(target.x, target.y);
            if frame == 0 {
                input.pressed.push(PointerButton::Primary);
            } else {
                input.released.push(PointerButton::Primary);
            }
            w.begin();
            w.radio("a", "甲", true);
            w.radio("b", "乙", false);
            w.finish(&mut ui, &input);
        }

        assert!(w.response(b).clicked);
        assert!(!w.response(a).clicked);
    }
}
