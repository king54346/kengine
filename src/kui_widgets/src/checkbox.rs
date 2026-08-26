//! 复选框。选中状态由调用方保管。

use kmath::{Vec2, Vec4};
use kfont::TextStyle;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 一个复选框。`checked` 是当前状态，由调用方保存。
    ///
    /// 不在控件里存状态：状态存在这里的话，同一个 id 在两个地方用就会
    /// 互相覆盖，而且调用方没法直接读写它。
    pub fn checkbox(&mut self, id: &str, text: impl Into<String>, checked: bool) -> Id {
        self.push(
            id,
            Widget::Checkbox {
                text: text.into(),
                checked,
            },
        )
    }
}

/// 勾的三个拐点，按方框的尺寸缩放。
///
/// 比例是照着常见字体里的勾调的：起笔偏左下、折点靠近底边、收笔在右上
/// 且比起笔高不少。三段等长的「对号」看起来像个歪掉的 V，不像勾。
fn check_points(box_rect: Rect) -> [Vec2; 3] {
    let size = box_rect.size();
    let at = |x: f32, y: f32| box_rect.min + Vec2::new(size.x * x, size.y * y);
    [at(0.22, 0.52), at(0.42, 0.72), at(0.78, 0.30)]
}


/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    let text_size = ui.measure(text, &text_style(theme.font_size), None).size;
    // 勾选框本体加一点间隔。
    Vec2::new(text_size.x + theme.row_height, text_size.y)
}

/// 出几何。
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, text: &str, checked: bool) {
    let box_size = theme.row_height * 0.6;
    let box_rect = Rect {
        min: Vec2::new(rect.min.x, rect.center().y - box_size * 0.5),
        max: Vec2::new(rect.min.x + box_size, rect.center().y + box_size * 0.5),
    };
    let fill = if checked {
        theme.accent
    } else if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rounded_rect(box_rect, 3.0, fill);
    ui.border(box_rect, 3.0, 1.0, theme.outline);
    if checked {
        ui.polyline(&check_points(box_rect), box_size * 0.15, Vec4::ONE);
    }
    if response.focused {
        ui.border(box_rect.shrink(-2.0), 5.0, 2.0, theme.focus);
    }

    ui.text(
        Vec2::new(
            box_rect.max.x + 8.0,
            rect.center().y - theme.font_size * 0.6,
        ),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
        None,
    );
}
