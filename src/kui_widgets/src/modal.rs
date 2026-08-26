//! 模态遮罩：压暗背景、吃掉点击。

use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi};

impl WidgetUi {
    /// 一层模态遮罩：铺满整屏、压暗背景、吃掉所有点击。
    ///
    /// **必须最先声明**——它靠「后声明的画在上面」来盖住背景，而命中
    /// 测试是从后往前找的，所以它自然会把点击让给之后声明的对话框。
    ///
    /// 返回的 id 上 `clicked` 为真表示点在了遮罩上（也就是对话框外面），
    /// 通常用来关闭对话框。
    pub fn modal(&mut self, id: &str) -> Id {
        let color = self.theme.modal;
        self.push(id, Widget::Modal { color })
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(_ui: &Ui, _theme: &Theme) -> Vec2 {
    Vec2::ZERO
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    _theme: &Theme,
    _rect: Rect,
    _response: &Response,
    color: Vec4,
    screen: Vec2,
) {
    // 铺满整个窗口，不是铺满这个节点——遮罩的意义就是
    // 挡住**外面**的东西，缩在布局框里就没用了。
    ui.rect(
        Rect {
            min: Vec2::ZERO,
            max: screen,
        },
        color,
    );
}

#[cfg(test)]
mod tests {
    
    use crate::WidgetUi;
    use crate::testing::{SCREEN, at, ui};
    use kui::{PointerButton, UiInput};

    /// 遮罩铺满整个窗口，不是铺满它那个布局节点——
    /// 缩在节点里的话背景根本挡不住。
    #[test]
    fn a_modal_covers_the_whole_window() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.modal("m");
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        let vertices = ui.draw_list().vertices();
        let min_x = vertices.iter().fold(f32::MAX, |a, v| a.min(v.position[0]));
        let min_y = vertices.iter().fold(f32::MAX, |a, v| a.min(v.position[1]));
        let max_x = vertices.iter().fold(f32::MIN, |a, v| a.max(v.position[0]));
        let max_y = vertices.iter().fold(f32::MIN, |a, v| a.max(v.position[1]));
        assert_eq!((min_x, min_y), (0.0, 0.0));
        assert_eq!((max_x, max_y), (SCREEN.x, SCREEN.y));
    }

    /// 遮罩之后声明的东西要能点得到——遮罩挡的是它**下面**的，
    /// 不是它上面的对话框。命中测试从后往前找，这一条就是在钉这个顺序。
    #[test]
    fn a_modal_does_not_block_what_comes_after_it() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.modal("m");
        let b = w.button("ok", "确定");
        w.finish(&mut ui, &UiInput::default());

        let target = w.response(b).rect.center();
        for frame in 0..2 {
            let mut input = at(target.x, target.y);
            if frame == 0 {
                input.pressed.push(PointerButton::Primary);
            } else {
                input.released.push(PointerButton::Primary);
            }
            w.begin();
            w.modal("m");
            w.button("ok", "确定");
            w.finish(&mut ui, &input);
        }

        assert!(w.response(b).clicked, "对话框上的按钮被遮罩吃掉了");
    }
}
