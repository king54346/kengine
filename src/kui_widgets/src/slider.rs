//! 滑条。数值由调用方保管，拖动增量自己算。

use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi};

impl WidgetUi {
    /// 一个滑条。`value` 是 0..=1。
    ///
    /// 拖动的结果要调用方自己算：
    /// `new = old + response(id).drag.x / response(id).rect.size().x`。
    /// 这么设计是因为控件不存状态（见 [`checkbox`](Self::checkbox)）。
    pub fn slider(&mut self, id: &str, value: f32) -> Id {
        self.push(
            id,
            Widget::Slider {
                value: value.clamp(0.0, 1.0),
            },
        )
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme) -> Vec2 {
    Vec2::new(120.0, theme.font_size)
}

/// 出几何。
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, value: f32) {
    let track_height = 6.0;
    let track = Rect {
        min: Vec2::new(rect.min.x, rect.center().y - track_height * 0.5),
        max: Vec2::new(rect.max.x, rect.center().y + track_height * 0.5),
    };
    ui.rounded_rect(track, track_height * 0.5, theme.surface);

    let filled = Rect {
        min: track.min,
        max: Vec2::new(track.min.x + track.size().x * value, track.max.y),
    };
    ui.rounded_rect(filled, track_height * 0.5, theme.accent);

    // 滑块。夹在轨道内，否则拖到两端时会掉出去一半。
    let knob_radius = theme.row_height * 0.32;
    let knob_x = (track.min.x + track.size().x * value)
        .clamp(track.min.x + knob_radius, track.max.x - knob_radius);
    let knob = Rect {
        min: Vec2::new(knob_x - knob_radius, rect.center().y - knob_radius),
        max: Vec2::new(knob_x + knob_radius, rect.center().y + knob_radius),
    };
    let fill = if response.held || response.hovered {
        Vec4::ONE
    } else {
        theme.text
    };
    ui.rounded_rect(knob, knob_radius, fill);
}
