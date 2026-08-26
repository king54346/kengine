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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{SCREEN, at, ui};
    use kmath::Vec2;
    use kui::{Edges, Id, Style, Ui, UiInput};

    /// 一个装了 `count` 个按钮的滚动区。
    fn scroll_list(w: &mut WidgetUi, ui: &mut Ui, input: &UiInput, count: usize) -> Id {
        w.begin();
        let id = w.begin_scroll("list", 100.0);
        for i in 0..count {
            w.button(&format!("row{i}"), format!("第 {i} 行"));
        }
        w.end_scroll();
        w.finish(ui, input);
        id
    }

    /// 视口里的一个点。
    ///
    /// 不硬编码坐标：测试里没有字体，按钮宽度只剩内边距（24 px），
    /// 随手写个 x=60 就落到视口外面去了。
    fn inside_viewport(w: &WidgetUi) -> Vec2 {
        w.scroll_viewport().expect("应当有滚动区").center()
    }

    fn scrolling(x: f32, y: f32, amount: f32) -> UiInput {
        UiInput {
            pointer: Some(Vec2::new(x, y)),
            scroll: Vec2::new(0.0, amount),
            ..Default::default()
        }
    }

    #[test]
    fn scrolling_moves_the_content_up() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let list = scroll_list(&mut w, &mut ui, &UiInput::default(), 20);
        let before = w.response(Id::new("row0")).rect.min.y;

        // 滚轮向下（负 y）。指针要在视口里。
        let point = inside_viewport(&w);
        let input = scrolling(point.x, point.y, -3.0);
        scroll_list(&mut w, &mut ui, &input, 20);

        assert!(
            w.scroll_offset(list) > 0.0,
            "偏移是 {}",
            w.scroll_offset(list)
        );
        assert!(
            w.response(Id::new("row0")).rect.min.y < before,
            "内容该往上跑"
        );
    }

    #[test]
    fn scrolling_stops_at_the_top_and_bottom() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let list = scroll_list(&mut w, &mut ui, &UiInput::default(), 20);
        let p = inside_viewport(&w);

        // 往上滚到头。
        for _ in 0..50 {
            scroll_list(&mut w, &mut ui, &scrolling(p.x, p.y, 10.0), 20);
        }
        assert_eq!(w.scroll_offset(list), 0.0, "不该滚到内容上面去");

        // 往下滚到底。
        for _ in 0..200 {
            scroll_list(&mut w, &mut ui, &scrolling(p.x, p.y, -10.0), 20);
        }
        let max = w.scroll_offset(list);
        scroll_list(&mut w, &mut ui, &scrolling(p.x, p.y, -10.0), 20);
        assert_eq!(w.scroll_offset(list), max, "到底之后不该继续滚");
    }

    #[test]
    fn a_shorter_list_clamps_the_old_offset() {
        // 内容变短之后旧偏移会把内容整个顶出视口，看起来像列表空了。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let list = scroll_list(&mut w, &mut ui, &UiInput::default(), 50);
        let p = inside_viewport(&w);
        for _ in 0..100 {
            scroll_list(&mut w, &mut ui, &scrolling(p.x, p.y, -10.0), 50);
        }
        assert!(w.scroll_offset(list) > 0.0);

        // 换成只有两行。
        scroll_list(&mut w, &mut ui, &UiInput::default(), 2);
        assert_eq!(w.scroll_offset(list), 0.0, "内容变短后偏移该夹回去");
    }

    #[test]
    fn the_wheel_only_scrolls_the_area_under_the_pointer() {
        // 不判的话页面上所有滚动区会一起滚。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let list = scroll_list(&mut w, &mut ui, &UiInput::default(), 20);

        scroll_list(&mut w, &mut ui, &scrolling(700.0, 500.0, -5.0), 20);
        assert_eq!(w.scroll_offset(list), 0.0, "指针不在视口里不该滚");
    }

    #[test]
    fn hit_testing_follows_the_scrolled_position() {
        // 用滚动前的矩形判命中的话，会「点这一行，亮的是另一行」。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        scroll_list(&mut w, &mut ui, &UiInput::default(), 20);
        let p = inside_viewport(&w);
        for _ in 0..5 {
            scroll_list(&mut w, &mut ui, &scrolling(p.x, p.y, -3.0), 20);
        }

        // 指针停在视口正中，命中的那一行的矩形必须真的包含这个点。
        let point = p;
        let input = UiInput {
            pointer: Some(point),
            ..Default::default()
        };
        scroll_list(&mut w, &mut ui, &input, 20);

        let hit = (0..20)
            .map(|i| Id::new(&format!("row{i}")))
            .find(|id| w.response(*id).hovered);
        if let Some(id) = hit {
            assert!(w.response(id).rect.contains(point));
        }
    }

    #[test]
    fn offscreen_rows_produce_no_geometry() {
        // 一千行的列表每帧为看不见的九百多行生成顶点的话，
        // CPU 和带宽全花在裁剪之后会被丢掉的东西上。
        let count_for = |rows: usize| {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            scroll_list(&mut w, &mut ui, &UiInput::default(), rows);
            ui.end_frame();
            ui.draw_list().vertices().len()
        };
        let few = count_for(5);
        let many = count_for(500);
        assert!(
            many < few * 4,
            "视口高度不变，几何量不该随行数线性增长：{few} → {many}"
        );
    }

    #[test]
    fn a_scroll_area_never_extends_past_the_window() {
        // 调用方给的高度是「我想要这么高」，但滚动区的起点由排版决定。
        // 让调用方自己算准剩余空间是算不准的——漏算一次，列表最后几行
        // 就画到窗口外面去了，而且看不出是被截断还是本来就没有。
        let mut w = WidgetUi::default();
        w.begin();
        w.label("title", "Controls");
        // 故意要一个比窗口还高的滚动区。
        w.begin_scroll("s", 10_000.0);
        for i in 0..40 {
            w.label(&format!("row{i}"), "content");
        }
        w.end_scroll();

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let viewport = w.scroll_viewport().expect("该有视口");
        assert!(
            viewport.max.y <= 600.0 + 1e-3,
            "视口底边跑到窗口外面了：{}",
            viewport.max.y
        );
    }

    #[test]
    fn a_scroll_area_starting_offscreen_collapses_to_nothing() {
        // 起点已经在窗口外时，夹完高度会变成负的。收成零高度，
        // 内容整个不画，比画出一片翻转的矩形强。
        let mut w = WidgetUi::default();
        w.root_style(Style {
            margin: Edges {
                left: 0.0,
                top: 5_000.0,
                right: 0.0,
                bottom: 0.0,
            },
            ..Default::default()
        });
        w.begin();
        w.begin_scroll("s", 200.0);
        w.label("a", "x");
        w.end_scroll();

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let viewport = w.scroll_viewport().expect("该有视口");
        assert!(viewport.max.y >= viewport.min.y, "视口翻转了：{viewport:?}");
    }
}
