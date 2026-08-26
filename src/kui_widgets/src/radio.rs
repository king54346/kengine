//! 单选按钮。一组里只能选一个，选中项由调用方保管。

use kmath::Vec2;
use kfont::TextStyle;
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
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, text: &str, selected: bool) {
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
