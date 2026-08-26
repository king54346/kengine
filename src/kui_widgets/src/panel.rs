//! 纯容器：只画一层底色和边框，用来给一组控件做背景。

use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi};

impl WidgetUi {
    /// 一块面板底色。通常作为根容器的背景。
    pub fn panel(&mut self, id: &str) -> Id {
        let color = self.theme.panel;
        let radius = self.theme.radius + 4.0;
        self.push(id, Widget::Panel { color, radius })
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(_ui: &Ui, _theme: &Theme) -> Vec2 {
    Vec2::ZERO
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    _response: &Response,
    color: Vec4,
    radius: f32,
) {
    ui.rounded_rect(rect, radius, color);
    ui.border(rect, radius, 1.0, theme.outline);
}
