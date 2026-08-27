//! 按钮。点一下触发一次，自己不存任何状态。

use kfont::TextStyle;
use kmath::Vec2;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 一个按钮。返回它的 id，用 [`response`](Self::response) 查是否被点。
    pub fn button(&mut self, id: &str, text: impl Into<String>) -> Id {
        self.push(id, Widget::Button { text: text.into() })
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    ui.measure(text, &text_style(theme.font_size), None).size
}

/// 出几何。
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, text: &str) {
    let fill = if response.held {
        theme.active
    } else if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rounded_rect(rect, theme.radius, fill);
    ui.border(rect, theme.radius, 1.0, theme.outline);
    if response.focused {
        // 焦点框画在外面一圈，免得和边框糊在一起。
        ui.border(rect.shrink(-2.0), theme.radius + 2.0, 2.0, theme.focus);
    }
    ui.text_centered(
        rect,
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
    );
}

#[cfg(test)]
mod tests {

    use crate::WidgetUi;
    use crate::testing::{activate, activate_first, at, ui};
    use kui::{PointerButton, UiInput};

    /// 键盘也能按按钮：Tab 走到它，回车 / 空格就是一下点击。
    ///
    /// 走 `clicked` 而不是新开一个字段，是为了让所有已经写着
    /// `if response.clicked` 的地方自动支持键盘。
    #[test]
    fn a_focused_button_can_be_activated_from_the_keyboard() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let mut id = None;
        activate_first(&mut w, &mut ui, |w| {
            id = Some(w.button("a", "点我"));
        });
        let id = id.unwrap();

        assert!(w.response(id).focused);
        assert!(w.response(id).clicked, "有焦点的按钮该被回车激活");
    }

    /// 激活只算一帧。不清的话按一次回车会被当成一直按着，
    /// 「开始游戏」会连着触发到天荒地老。
    #[test]
    fn keyboard_activation_lasts_a_single_frame() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let mut id = None;
        activate_first(&mut w, &mut ui, |w| {
            id = Some(w.button("a", "点我"));
        });
        let id = id.unwrap();
        assert!(w.response(id).clicked);

        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &UiInput::default());
        assert!(!w.response(id).clicked, "松开之后不该还算按着");
    }

    /// 没焦点的按钮不该被别人的回车带着一起触发。
    #[test]
    fn an_unfocused_button_ignores_the_activate_key() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let mut ids = None;
        activate_first(&mut w, &mut ui, |w| {
            ids = Some((w.button("a", "甲"), w.button("b", "乙")));
        });
        let (a, b) = ids.unwrap();

        assert!(w.response(a).clicked);
        assert!(!w.response(b).clicked, "焦点不在乙身上");
    }

    /// 谁都没有焦点时按回车，一个按钮也不该响应。
    #[test]
    fn the_activate_key_does_nothing_without_focus() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        w.begin();
        let id = w.button("a", "点我");
        w.finish(&mut ui, &UiInput::default());

        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &activate());
        assert!(!w.response(id).clicked);
    }

    #[test]
    fn a_button_reports_hover_and_click() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        // 第一帧：声明并排版。
        w.begin();
        let id = w.button("a", "点我");
        w.finish(&mut ui, &UiInput::default());
        let rect = w.response(id).rect;
        let center = rect.center();

        // 第二帧：指针移上去。
        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &at(center.x, center.y));
        assert!(w.response(id).hovered);

        // 第三帧：按下。
        let mut input = at(center.x, center.y);
        input.pressed.push(PointerButton::Primary);
        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &input);
        assert!(w.response(id).held);
        assert!(!w.response(id).clicked);

        // 第四帧：松开。
        let mut input = at(center.x, center.y);
        input.released.push(PointerButton::Primary);
        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &input);
        assert!(w.response(id).clicked);
    }

    #[test]
    fn clicking_one_button_does_not_trigger_the_other() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.button("a", "甲");
        let b = w.button("b", "乙");
        w.finish(&mut ui, &UiInput::default());

        let center = w.response(a).rect.center();
        let mut input = at(center.x, center.y);
        input.pressed.push(PointerButton::Primary);
        w.begin();
        w.button("a", "甲");
        w.button("b", "乙");
        w.finish(&mut ui, &input);

        let mut input = at(center.x, center.y);
        input.released.push(PointerButton::Primary);
        w.begin();
        w.button("a", "甲");
        w.button("b", "乙");
        w.finish(&mut ui, &input);

        assert!(w.response(a).clicked);
        assert!(!w.response(b).clicked);
    }
}
