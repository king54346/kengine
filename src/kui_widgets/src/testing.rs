//! 测试用的小工具。各控件模块的测试都从这里拿。
//!
//! 只在 `cfg(test)` 下编译，不进发布产物。

use kmath::Vec2;
use kui::{PointerButton, Ui, UiInput};

use crate::WidgetUi;

/// 测试用的窗口大小。
pub(crate) const SCREEN: Vec2 = Vec2::new(800.0, 600.0);

/// 一个不带字体的 UI。文字量出来是零尺寸，但布局与交互照常。
pub(crate) fn ui() -> Ui {
    let mut ui = Ui::new();
    ui.begin_frame(SCREEN, 1.0);
    ui
}

/// 指针停在某处，没有按键。
pub(crate) fn at(x: f32, y: f32) -> UiInput {
    UiInput {
        pointer: Some(Vec2::new(x, y)),
        ..Default::default()
    }
}

/// 本帧按下了激活键（回车 / 空格），指针不在窗口里。
pub(crate) fn activate() -> UiInput {
    UiInput {
        activate: true,
        ..Default::default()
    }
}

/// Tab 一次，把焦点落到第一个能聚焦的控件上，然后按一下激活键。
///
/// 焦点是**跨帧**的，而控件的矩形要等 `finish` 排完才知道，所以这必须
/// 是两帧：第一帧走焦点，第二帧才轮得到激活。写在一处免得每个控件的
/// 测试各踩一次。
pub(crate) fn activate_first(w: &mut WidgetUi, ui: &mut Ui, mut declare: impl FnMut(&mut WidgetUi)) {
    let tab = UiInput {
        focus_step: 1,
        ..Default::default()
    };
    for input in [&tab, &activate()] {
        w.begin();
        declare(w);
        w.finish(ui, input);
    }
}

/// 在某处完成一次完整的点击。
///
/// 点击要**按下、松开两帧**才算数：只发松开的话，控件从没见过按下，
/// 自然不认这一下。这个陷阱值得包一层，免得每个测试各踩一次。
pub(crate) fn click_at(
    w: &mut WidgetUi,
    ui: &mut Ui,
    point: Vec2,
    mut declare: impl FnMut(&mut WidgetUi),
) {
    for frame in 0..2 {
        let mut input = at(point.x, point.y);
        if frame == 0 {
            input.pressed.push(PointerButton::Primary);
        } else {
            input.released.push(PointerButton::Primary);
        }
        w.begin();
        declare(w);
        w.finish(ui, &input);
    }
}
