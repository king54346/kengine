//! 文本框。状态又大又贵，破例由控件自己存。

use kmath::{Vec2, Vec4};
use kfont::TextStyle;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Declared, apply_edit};
use crate::widgets::{Theme, Widget, WidgetUi, text_style};
use crate::TextEdit;

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
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, text: &str, placeholder: &str, edit: &TextEdit) {
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

    let edit = edit;
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
