//! 文字：正文与次要文字。

use kfont::TextStyle;
use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 一段文字。
    pub fn label(&mut self, id: &str, text: impl Into<String>) -> Id {
        let color = self.theme.text;
        let size = self.theme.font_size;
        self.push(
            id,
            Widget::Label {
                text: text.into(),
                color,
                size,
            },
        )
    }

    /// 一段次要文字。
    pub fn dim_label(&mut self, id: &str, text: impl Into<String>) -> Id {
        let color = self.theme.dim;
        let size = self.theme.font_size;
        self.push(
            id,
            Widget::Label {
                text: text.into(),
                color,
                size,
            },
        )
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, _theme: &Theme, text: &str, size: f32) -> Vec2 {
    ui.measure(text, &text_style(size), None).size
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    _theme: &Theme,
    rect: Rect,
    _response: &Response,
    text: &str,
    color: Vec4,
    size: f32,
) {
    ui.text(
        rect.min,
        text,
        &TextStyle {
            size: size,
            ..Default::default()
        },
        color,
        Some(rect.size().x),
    );
}
