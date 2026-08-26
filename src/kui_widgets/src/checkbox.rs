//! 复选框。选中状态由调用方保管。

use kfont::TextStyle;
use kmath::{Vec2, Vec4};
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
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    checked: bool,
) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{SCREEN, at, ui};
    use kui::{PointerButton, UiInput};

    #[test]
    fn a_checked_box_draws_more_than_an_unchecked_one() {
        // 勾没画出来的话，复选框在两个状态下长得一样，用户看不出来。
        let count = |checked: bool| {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            w.begin();
            w.checkbox("b", "开关", checked);
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();
            ui.draw_list().vertices().len()
        };
        assert!(count(true) > count(false));
    }

    /// 勾是折线画的，不是方块——方块和真正的勾在 SDF 模式上就不同。
    #[test]
    fn a_tick_is_drawn_with_strokes() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.checkbox("b", "开关", true);
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        let strokes = ui
            .draw_list()
            .vertices()
            .iter()
            .filter(|v| v.params[2] == kui::MODE_SEGMENT)
            .count();
        // 两段折线，每段一个四边形。
        assert_eq!(strokes, 2 * 4, "勾该是两段笔画");
    }

    /// 没勾选时一笔都不画。
    #[test]
    fn an_unchecked_box_draws_no_strokes() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.checkbox("b", "开关", false);
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        assert!(
            ui.draw_list()
                .vertices()
                .iter()
                .all(|v| v.params[2] != kui::MODE_SEGMENT)
        );
    }

    /// 勾要长得像勾：先往右下、再往右上，而且收笔比起笔高。
    ///
    /// 三个点如果共线就成了一根斜杠；收笔不够高就成了个歪 V。
    #[test]
    fn a_tick_actually_looks_like_a_tick() {
        let box_rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        let [start, elbow, finish] = check_points(box_rect);

        // 从左到右单调前进。
        assert!(start.x < elbow.x && elbow.x < finish.x);
        // 先下降（y 向下为正），再上升。
        assert!(elbow.y > start.y, "第一笔该往下");
        assert!(finish.y < elbow.y, "第二笔该往上");
        // 收笔明显高于起笔，这是勾和 V 的区别。
        assert!(finish.y < start.y - box_rect.size().y * 0.1, "收笔不够高");
        // 三点不共线。
        let a = elbow - start;
        let b = finish - elbow;
        assert!(
            (a.x * b.y - a.y * b.x).abs() > 1.0,
            "三点共线，画出来是根斜杠"
        );
    }

    /// 勾整个待在方框里，不出界。
    #[test]
    fn a_tick_stays_inside_its_box() {
        let box_rect = Rect::new(10.0, 40.0, 20.0, 20.0);
        for point in check_points(box_rect) {
            assert!(box_rect.contains(point), "{point:?} 跑出了 {box_rect:?}");
        }
    }

    /// 勾跟着方框缩放，不是固定尺寸——换了主题字号方框会变大。
    #[test]
    fn a_tick_scales_with_its_box() {
        let small = check_points(Rect::new(0.0, 0.0, 10.0, 10.0));
        let big = check_points(Rect::new(0.0, 0.0, 40.0, 40.0));
        let span = |p: [Vec2; 3]| p[2].x - p[0].x;
        assert!((span(big) - span(small) * 4.0).abs() < 0.01);
    }
}
