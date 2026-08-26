//! 按钮。点一下触发一次，自己不存任何状态。

use kmath::Vec2;
use kfont::TextStyle;
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
