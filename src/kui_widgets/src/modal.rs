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
pub(crate) fn size(ui: &Ui, theme: &Theme) -> Vec2 {
    Vec2::ZERO
}

/// 出几何。
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, color: Vec4, screen: Vec2) {
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
