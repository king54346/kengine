//! 列表行。选中项由调用方保管，单选多选都由它决定。

use kmath::Vec2;
use kfont::TextStyle;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 列表里的一行。
    ///
    /// 选中项同样由调用方保管。多选时就存一个集合——控件这一层
    /// 不需要知道是单选还是多选。
    pub fn list_item(&mut self, id: &str, text: impl Into<String>, selected: bool) -> Id {
        self.push(
            id,
            Widget::ListItem {
                text: text.into(),
                selected,
            },
        )
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    ui.measure(text, &text_style(theme.font_size), None).size
}

/// 出几何。
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, text: &str, selected: bool) {
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
