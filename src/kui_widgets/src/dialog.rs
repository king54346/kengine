//! 对话框标题栏：可拖动，右端一个关闭叉。

use kmath::Vec2;
use kfont::TextStyle;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 对话框的标题栏：一条可拖动的横条。
    ///
    /// 位置由调用方保管：
    ///
    /// ```ignore
    /// let title = w.dialog_title("t", "设置");
    /// self.dialog_position += w.response(title).drag;
    /// ```
    ///
    /// 拖动增量而不是绝对位置——绝对位置要求控件知道自己「本该」在哪，
    /// 而那正是调用方在管的事。
    pub fn dialog_title(&mut self, id: &str, text: impl Into<String>) -> Id {
        self.push(id, Widget::DialogTitle { text: text.into() })
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    let text_size = ui.measure(text, &text_style(theme.font_size), None).size;
    // 右端给关闭按钮留位置。
    Vec2::new(text_size.x + theme.row_height, text_size.y)
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
    ui.rect(rect, fill);
    ui.text(
        Vec2::new(
            rect.min.x + theme.padding.left,
            rect.center().y - theme.font_size * 0.6,
        ),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
        // 给关闭按钮让出宽度，否则长标题会把叉盖住。
        Some((rect.size().x - theme.row_height).max(0.0)),
    );
    // 右端的关闭叉。悬停在标题栏上时提亮，好让它看起来
    // 是能点的——虽然点它和点标题栏目前是同一个响应。
    let mark = theme.font_size * 0.4;
    let center = Vec2::new(rect.max.x - theme.row_height * 0.5, rect.center().y);
    let color = if response.hovered {
        theme.text
    } else {
        theme.dim
    };
    let half = mark * 0.5;
    let width = mark * 0.22;
    ui.segment(
        center - Vec2::splat(half),
        center + Vec2::splat(half),
        width,
        color,
    );
    ui.segment(
        center + Vec2::new(-half, half),
        center + Vec2::new(half, -half),
        width,
        color,
    );
}
