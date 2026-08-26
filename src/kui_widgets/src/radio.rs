//! 单选按钮。一组里只能选一个，选中项由调用方保管。

use kfont::TextStyle;
use kmath::Vec2;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
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
    use kui::{PointerButton, UiInput};

    /// 有焦点的单选按钮认回车 / 空格。
    ///
    /// 注意这**不是**「一组里用方向键换选」——那要先有「组」这个概念，
    /// 现在还没有。这里只是让 Tab 走到的那一个能用键盘选中。
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
