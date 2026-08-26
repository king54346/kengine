//! 滚动条：看得见、拖得动的那根条。

use kmath::{Vec2, Vec4};
use kui::{Id, Rect, Response, Ui};

use crate::widgets::{Theme, Widget, WidgetUi};

/// 滚动条的厚度。
const SCROLLBAR_THICKNESS: f32 = 10.0;

/// 滑块的最短长度。内容极长时滑块会算得很短，短到抓不住。
const SCROLLBAR_MIN_THUMB: f32 = 24.0;

impl WidgetUi {
    /// 一根滚动条。
    ///
    /// `fraction` 是可见部分占全部内容的比例（滑块多长），
    /// `offset` 是已经滚过的比例（滑块在哪）。两者都是 0..=1。
    ///
    /// 滚轮由 [`begin_scroll`](Self::begin_scroll) 处理；这根条是给
    /// **鼠标拖动**用的，也让「内容还有多少没看到」变得可见。
    pub fn scrollbar(&mut self, id: &str, fraction: f32, offset: f32) -> Id {
        self.push(
            id,
            Widget::Scrollbar {
                fraction: fraction.clamp(0.0, 1.0),
                offset: offset.clamp(0.0, 1.0),
            },
        )
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(_ui: &Ui, _theme: &Theme) -> Vec2 {
    Vec2::new(0.0, SCROLLBAR_THICKNESS)
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    fraction: f32,
    offset: f32,
) {
    // 竖着还是横着看布局给的形状，不额外加参数——
    // 一根又高又窄的条只可能是竖的。
    let size = rect.size();
    let vertical = size.y >= size.x;
    let track_len = if vertical { size.y } else { size.x };
    // 滑块再短也要能点中。太短的话内容一多就变成一根
    // 抓不住的线。
    let thumb_len = (track_len * fraction).max(SCROLLBAR_MIN_THUMB.min(track_len));
    // 滑块缩短了多少，可走的行程就少多少，否则滑到底时
    // 滑块会探出轨道。
    let travel = (track_len - thumb_len).max(0.0);
    let start = travel * offset;

    ui.rounded_rect(rect, size.min_element() * 0.5, theme.surface);

    let thumb = if vertical {
        Rect {
            min: Vec2::new(rect.min.x, rect.min.y + start),
            max: Vec2::new(rect.max.x, rect.min.y + start + thumb_len),
        }
    } else {
        Rect {
            min: Vec2::new(rect.min.x + start, rect.min.y),
            max: Vec2::new(rect.min.x + start + thumb_len, rect.max.y),
        }
    };
    let fill = if response.held {
        Vec4::ONE
    } else if response.hovered {
        theme.text
    } else {
        theme.dim
    };
    ui.rounded_rect(thumb, thumb.size().min_element() * 0.5, fill);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{SCREEN, at, ui};
    use kui::{PointerButton, UiInput};

    /// 内容越多滑块越短，但短到一定程度就不再短了——
    /// 一根抓不住的线等于没有滚动条。
    #[test]
    fn a_scrollbar_thumb_never_gets_too_short() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        // 万分之一的可见比例：朴素算法会得到零点几像素。
        let bar = w.scrollbar("s", 0.0001, 0.0);
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        let track = w.response(bar).rect;
        // 找出画在轨道之外的最右一点，也就是滑块的末端。
        let vertices = ui.draw_list().vertices();
        let widest = vertices.iter().fold(f32::MIN, |a, v| a.max(v.position[0]));
        let thumb_len = widest - track.min.x;
        assert!(
            thumb_len >= SCROLLBAR_MIN_THUMB.min(track.size().x) - 0.01,
            "滑块只有 {thumb_len} 长",
        );
    }

    /// 滑到底时滑块的末端正好贴着轨道末端，不探出去。
    ///
    /// 这一条盯着的是「行程 = 轨道长 − 滑块长」：忘了减滑块长度的话，
    /// offset 为 1 会把滑块整个推出轨道。
    #[test]
    fn a_scrollbar_thumb_stops_at_the_end_of_the_track() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let bar = w.scrollbar("s", 0.25, 1.0);
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        let track = w.response(bar).rect;
        let vertices = ui.draw_list().vertices();
        let widest = vertices.iter().fold(f32::MIN, |a, v| a.max(v.position[0]));
        assert!(
            widest <= track.max.x + 0.01,
            "滑块探出轨道 {} 像素",
            widest - track.max.x,
        );
    }
}
