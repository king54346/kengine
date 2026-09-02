//! 文本框。状态又大又贵，破例由控件自己存。

use kfont::TextStyle;
use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::TextEdit;
use crate::widgets::{Declared, apply_edit};
use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 一个文本框。
    ///
    /// **直接改传进来的 `text`**：编辑动作在声明期就应用了，不像别的控件
    /// 那样滞后一帧——打字滞后一帧会让人以为按键丢了。
    ///
    /// 只在这个文本框**有焦点**时才处理输入。
    pub fn text_input(
        &mut self,
        id: &str,
        text: &mut String,
        placeholder: impl Into<String>,
        input: &kui::UiInput,
    ) -> Id {
        let id = Id::new(id);
        let focused = self.interaction.focused() == Some(id);

        let edit = self.edits.entry(id).or_default();
        // 文本可能被外部改过（读档、重置）。不夹的话光标停在旧位置，
        // 下一次切片直接 panic。
        edit.clamp(text);

        if focused {
            for action in &input.edits {
                apply_edit(edit, text, *action);
            }
            if !input.text.is_empty() {
                edit.insert(text, &input.text);
            }
        }

        let snapshot = text.clone();
        let row = self.open_row;
        let grow = row.is_some() && self.row_first;
        if row.is_some() {
            self.row_first = false;
        }
        self.declared.push(Declared {
            id,
            widget: Widget::TextInput {
                text: snapshot,
                placeholder: placeholder.into(),
            },
            row,
            grow,
            tab_stop: true,
        });
        id
    }

    /// 一个文本框的光标与选区。
    pub fn text_state(&self, id: Id) -> crate::TextEdit {
        self.edits.get(&id).copied().unwrap_or_default()
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str, placeholder: &str) -> Vec2 {
    // 按内容和提示里较宽的那个量，但至少留出一段可打字的宽度——
    // 空文本框宽度为零的话根本点不进去。
    let shown = if text.is_empty() { placeholder } else { text };
    let size = ui.measure(shown, &text_style(theme.font_size), None).size;
    Vec2::new(size.x.max(160.0), theme.font_size)
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    placeholder: &str,
    edit: &TextEdit,
) {
    let fill = if response.focused {
        theme.active
    } else if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rounded_rect(rect, theme.radius, fill);
    ui.border(
        rect,
        theme.radius,
        if response.focused { 2.0 } else { 1.0 },
        if response.focused {
            theme.focus
        } else {
            theme.outline
        },
    );

    let inner = rect.shrink(theme.padding.left.min(theme.padding.top).max(6.0));
    let text_style = TextStyle {
        size: theme.font_size,
        wrap: kfont::Wrap::None,
        ..Default::default()
    };
    let baseline = Vec2::new(inner.min.x, rect.center().y - theme.font_size * 0.62);

    // 内容会比框长。裁剪掉溢出的部分，否则文字会画到
    // 框外面、盖住旁边的控件。
    ui.push_clip(inner);

    // 选区先画，文字盖在上面。反过来的话高亮会糊住文字。
    if edit.has_selection() {
        let range = edit.selection();
        let before = ui.measure(&text[..range.start], &text_style, None).size.x;
        let width = ui.measure(&text[range.clone()], &text_style, None).size.x;
        ui.rect(
            Rect {
                min: Vec2::new(baseline.x + before, inner.min.y),
                max: Vec2::new(baseline.x + before + width, inner.max.y),
            },
            theme.accent * Vec4::new(1.0, 1.0, 1.0, 0.45),
        );
    }

    if text.is_empty() {
        ui.text(baseline, placeholder, &text_style, theme.dim, None);
    } else {
        ui.text(baseline, text, &text_style, theme.text, None);
    }

    // 光标。只在有焦点时画，否则每个文本框里都杵着一根竖线。
    if response.focused {
        let before = ui
            .measure(&text[..edit.cursor().min(text.len())], &text_style, None)
            .size
            .x;
        ui.rect(
            Rect {
                min: Vec2::new(baseline.x + before, inner.min.y),
                max: Vec2::new(baseline.x + before + 1.5, inner.max.y),
            },
            theme.text,
        );
    }

    ui.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::ui;
    use kui::UiInput;

    /// 让某个控件拿到焦点：Tab 一次就走到第一个。
    fn focus_first(w: &mut WidgetUi, ui: &mut Ui, declare: impl Fn(&mut WidgetUi)) {
        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        w.begin();
        declare(w);
        w.finish(ui, &tab);
    }

    /// 空格在文本框里就是一个空格，**不是**一次激活。
    ///
    /// 两边都认的话，敲一个空格会既打出空格又触发一次「点击」——
    /// 那个 `clicked` 会一路传到调用方的「确定」按钮逻辑上去。
    /// 所以 `kapp` 照实把空格同时填进 `text` 和 `activate`，
    /// 由控件层按焦点在谁身上决定谁吃掉它。
    #[test]
    fn a_space_in_a_text_input_is_typed_not_an_activation() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::new();

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::new();
            w.text_input("name", &mut scratch, "名字", &UiInput::default());
        });

        let input = UiInput {
            text: " ".to_string(),
            activate: true,
            ..Default::default()
        };
        w.begin();
        let id = w.text_input("name", &mut text, "名字", &input);
        w.finish(&mut ui, &input);

        assert_eq!(text, " ", "空格该被打进文本框");
        assert!(!w.response(id).clicked, "文本框不该被空格激活");
    }

    /// 回车同理：它在文本框里是提交，不该顺带算一次点击。
    #[test]
    fn enter_in_a_text_input_is_not_an_activation() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::from("ab");

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::from("ab");
            w.text_input("name", &mut scratch, "名字", &UiInput::default());
        });

        let input = UiInput {
            edits: vec![kui::EditAction::Submit],
            activate: true,
            ..Default::default()
        };
        w.begin();
        let id = w.text_input("name", &mut text, "名字", &input);
        w.finish(&mut ui, &input);

        assert!(!w.response(id).clicked);
    }

    #[test]
    fn typing_into_a_focused_text_input_changes_the_text() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::new();

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::new();
            w.text_input("name", &mut scratch, "名字", &UiInput::default());
        });

        let input = UiInput {
            text: "中文".to_string(),
            ..Default::default()
        };
        w.begin();
        w.text_input("name", &mut text, "名字", &input);
        w.finish(&mut ui, &input);

        assert_eq!(text, "中文");
    }

    #[test]
    fn an_unfocused_text_input_ignores_typing() {
        // 不判焦点的话，界面上每个文本框都会同时收到同一串字。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut a = String::new();
        let mut b = String::new();

        // 先让第一个拿到焦点。
        focus_first(&mut w, &mut ui, |w| {
            let mut s1 = String::new();
            let mut s2 = String::new();
            w.text_input("a", &mut s1, "", &UiInput::default());
            w.text_input("b", &mut s2, "", &UiInput::default());
        });

        let input = UiInput {
            text: "x".to_string(),
            ..Default::default()
        };
        w.begin();
        w.text_input("a", &mut a, "", &input);
        w.text_input("b", &mut b, "", &input);
        w.finish(&mut ui, &input);

        assert_eq!(a, "x");
        assert_eq!(b, "", "没有焦点的文本框不该收到输入");
    }

    #[test]
    fn backspace_in_a_text_input_removes_a_whole_character() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::from("中文");

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::from("中文");
            w.text_input("t", &mut scratch, "", &UiInput::default());
        });
        // 光标要先到末尾。
        let to_end = UiInput {
            edits: vec![kui::EditAction::End { select: false }],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &to_end);
        w.finish(&mut ui, &to_end);

        let backspace = UiInput {
            edits: vec![kui::EditAction::Backspace],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &backspace);
        w.finish(&mut ui, &backspace);

        assert_eq!(text, "中");
    }

    #[test]
    fn a_text_input_survives_the_text_being_replaced_externally() {
        // 读档、重置会把文本整个换掉。光标不夹回去的话下一次切片就 panic。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::from("很长的一段内容");

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::from("很长的一段内容");
            w.text_input("t", &mut scratch, "", &UiInput::default());
        });
        let to_end = UiInput {
            edits: vec![kui::EditAction::End { select: false }],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &to_end);
        w.finish(&mut ui, &to_end);

        // 外部换成短的。
        text = String::from("短");
        w.begin();
        w.text_input("t", &mut text, "", &UiInput::default());
        w.finish(&mut ui, &UiInput::default());

        assert!(w.text_state(Id::new("t")).cursor() <= text.len());
    }

    #[test]
    fn an_empty_text_input_still_has_a_clickable_width() {
        // 宽度按内容算的话，空文本框会塌成零宽，根本点不进去。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::new();
        w.begin();
        let id = w.text_input("t", &mut text, "", &UiInput::default());
        w.finish(&mut ui, &UiInput::default());

        assert!(w.response(id).rect.size().x >= 160.0);
    }
}
