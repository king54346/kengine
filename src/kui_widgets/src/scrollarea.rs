//! 滚动区：把一段内容装进固定高度的窗口里，滚轮可滚。
//!
//! 滚动区不是一个「控件」，是一段**范围**：`begin_scroll` 到 `end_scroll`
//! 之间声明的东西会被裁进一个固定高度的窗口，滚轮可以滚。所以它没有
//! 自己的绘制分支——它改的是别人怎么被裁、被摆。

use kui::Id;

use crate::widgets::ScrollFrame;
use crate::widgets::WidgetUi;

impl WidgetUi {
    /// 开一个滚动区。之后声明的控件都进这个区，直到 [`end_scroll`](Self::end_scroll)。
    ///
    /// `height` 是视口高度；内容比它高时可以滚。
    pub fn begin_scroll(&mut self, id: &str, height: f32) -> Id {
        let id = Id::new(id);
        self.scroll_frame = Some(ScrollFrame {
            id,
            height,
            first: self.declared.len(),
            last: usize::MAX,
        });
        id
    }

    /// 收一个滚动区。
    ///
    /// 滚轮由 [`finish`](Self::finish) 统一处理——那时才知道内容有多高。
    pub fn end_scroll(&mut self) {
        if let Some(frame) = self.scroll_frame.as_mut() {
            frame.last = self.declared.len();
        }
    }

    /// 一个滚动区当前滚到哪了（像素，向下为正）。
    pub fn scroll_offset(&self, id: Id) -> f32 {
        self.scroll.get(&id).copied().unwrap_or(0.0)
    }
}
