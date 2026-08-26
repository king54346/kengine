//! 可折叠分组。展开状态是纯外观，破例由控件自己存。

use kfont::TextStyle;
use kmath::Vec2;
use kui::{Id, Rect, Response, Ui};

use crate::widgets::Declared;
use crate::widgets::{Theme, Widget, WidgetUi, text_style};

impl WidgetUi {
    /// 开一个可折叠分组，返回标题条的 id。
    ///
    /// 点标题条切换展开 / 收起。收起时**里面的控件根本不会被声明**——
    /// 不占布局、不参与交互、不出几何。
    ///
    /// ```no_run
    /// # let mut w = kui_widgets::WidgetUi::default();
    /// w.begin();
    /// if w.folder("visibility", "Visibility") {
    ///     w.checkbox("a", "show model", true);
    /// }
    /// w.end_folder();
    /// ```
    ///
    /// 返回值是「现在开着吗」，方便用 `if` 把内容包起来——虽然收起时
    /// 声明了也会被丢弃，但用 `if` 能顺带省掉构造那些字符串的开销。
    ///
    /// 不支持嵌套：嵌套折叠的缩进规则和点击热区很快就说不清了，
    /// 而 HUD 与调试面板用不到。
    pub fn folder(&mut self, id: &str, text: impl Into<String>) -> bool {
        let key = Id::new(id);
        // 默认展开：一个所有分组都收着的面板，第一眼看不出能点开。
        let open = *self.folders.entry(key).or_insert(true);

        // 标题条本身不受折叠影响——它是那个开关。
        let was_collapsed = self.collapsed;
        self.collapsed = false;
        let text = text.into();
        self.declared.push(Declared {
            id: key,
            widget: Widget::Folder { text, open },
            row: None,
            grow: false,
            tab_stop: true,
        });
        self.collapsed = was_collapsed;

        // 上一层已经收起来时，里面的分组一律当收起处理。
        let effective = open && !was_collapsed;
        self.collapsed = !effective;
        effective
    }

    /// 收一个折叠分组。
    pub fn end_folder(&mut self) {
        self.collapsed = false;
    }

    /// 一个折叠分组现在开着吗。
    pub fn folder_open(&self, id: Id) -> bool {
        self.folders.get(&id).copied().unwrap_or(true)
    }

    /// 直接设置某个折叠分组的开合。
    pub fn set_folder_open(&mut self, id: Id, open: bool) {
        self.folders.insert(id, open);
    }
}

/// 量内容尺寸。布局据此决定这个控件要占多大。
pub(crate) fn size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    let size = ui.measure(text, &text_style(theme.font_size), None).size;
    // 左边给三角形留位置。
    Vec2::new(size.x + theme.row_height, size.y)
}

/// 出几何。
pub(crate) fn paint(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    open: bool,
) {
    // 标题条：一条比面板稍亮的横杠，左端一个三角形。
    let fill = if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rect(rect, fill);

    // 三角形用两条不同长度的短横线拼——引擎的 UI 图元只有
    // 矩形和圆角矩形，画不出真三角形。收起时是 `>`（两段
    // 竖向错开），展开时是 `v`（两段横向错开）。
    let mark = theme.row_height * 0.28;
    let center = Vec2::new(rect.min.x + theme.row_height * 0.5, rect.center().y);
    let arm = mark * 0.5;
    if open {
        // ▼：上面一横宽、下面一横窄。
        ui.rect(
            Rect {
                min: Vec2::new(center.x - mark, center.y - arm * 0.5),
                max: Vec2::new(center.x + mark, center.y + arm * 0.5),
            },
            theme.dim,
        );
        ui.rect(
            Rect {
                min: Vec2::new(center.x - arm, center.y + arm * 0.5),
                max: Vec2::new(center.x + arm, center.y + arm * 1.5),
            },
            theme.dim,
        );
    } else {
        // ▶：左边一竖长、右边一竖短。
        ui.rect(
            Rect {
                min: Vec2::new(center.x - arm * 0.5, center.y - mark),
                max: Vec2::new(center.x + arm * 0.5, center.y + mark),
            },
            theme.dim,
        );
        ui.rect(
            Rect {
                min: Vec2::new(center.x + arm * 0.5, center.y - arm),
                max: Vec2::new(center.x + arm * 1.5, center.y + arm),
            },
            theme.dim,
        );
    }

    ui.text(
        Vec2::new(rect.min.x + theme.row_height, rect.min.y),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
        Some(rect.size().x - theme.row_height),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{activate_first, ui};

    /// 键盘能展开 / 收起分组。
    ///
    /// 折叠的翻转排在 `finish` 里读 `clicked` 的那一步，所以这条同时也
    /// 钉住了「键盘激活要发生在读 `clicked` 之前」——顺序反了这里就不过。
    #[test]
    fn a_focused_folder_toggles_from_the_keyboard() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        activate_first(&mut w, &mut ui, |w| {
            w.folder("f", "Section");
            w.end_folder();
        });

        assert!(!w.folder_open(Id::new("f")), "回车该把展开着的分组收起来");
    }

    #[test]
    fn a_folder_starts_open() {
        // 所有分组都收着的面板，第一眼看不出能点开。
        let mut w = WidgetUi::default();
        w.begin();
        assert!(w.folder("f", "Section"));
        w.end_folder();
    }

    #[test]
    fn a_collapsed_folder_declares_nothing_inside() {
        // 收起时里面的控件**根本不声明**。声明了再藏的话它们仍然占布局
        // 空间，收起来的分组会留下一大片空白。
        let mut w = WidgetUi::default();
        w.begin();
        let header = w.folder("f", "Section");
        assert!(header);
        w.label("inside", "hi");
        w.end_folder();

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());
        let open_height = w.response(Id::new("inside")).rect.size().y;
        assert!(open_height > 0.0, "展开时里面的控件该有高度");

        // 收起来。
        w.set_folder_open(Id::new("f"), false);
        w.begin();
        w.folder("f", "Section");
        w.label("inside", "hi");
        w.end_folder();
        w.finish(&mut ui, &kui::UiInput::default());

        assert_eq!(
            w.response(Id::new("inside")).rect.size(),
            Vec2::ZERO,
            "收起来了，里面的控件却还占着地方"
        );
    }

    #[test]
    fn a_collapsed_folder_shrinks_the_panel() {
        // 端到端：收起之后整体高度必须变小，否则折叠没有意义。
        let mut ui = ui();

        let mut w = WidgetUi::default();
        w.begin();
        w.folder("f", "Section");
        for i in 0..8 {
            w.label(&format!("row{i}"), "content");
        }
        w.end_folder();
        let tail_open = w.label("tail", "after");
        w.finish(&mut ui, &kui::UiInput::default());
        let open_bottom = w.response(tail_open).rect.max.y;

        w.set_folder_open(Id::new("f"), false);
        w.begin();
        w.folder("f", "Section");
        for i in 0..8 {
            w.label(&format!("row{i}"), "content");
        }
        w.end_folder();
        let tail_closed = w.label("tail", "after");
        w.finish(&mut ui, &kui::UiInput::default());
        let closed_bottom = w.response(tail_closed).rect.max.y;

        assert!(
            closed_bottom < open_bottom,
            "收起来之后面板没变矮：{closed_bottom} vs {open_bottom}"
        );
    }

    #[test]
    fn the_folder_header_itself_is_always_declared() {
        // 标题条是那个开关，收起时它自己必须还在，否则再也点不开了。
        let mut w = WidgetUi::default();
        w.set_folder_open(Id::new("f"), false);
        w.begin();
        let header = w.folder("f", "Section");
        assert!(!header);
        w.label("inside", "hi");
        w.end_folder();

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());
        assert!(
            w.response(Id::new("f")).rect.size().y > 0.0,
            "收起来之后标题条也没了，再也点不开"
        );
    }

    #[test]
    fn end_folder_restores_declaring() {
        // 收完之后的控件不受影响。
        let mut w = WidgetUi::default();
        w.set_folder_open(Id::new("f"), false);
        w.begin();
        w.folder("f", "Section");
        w.label("inside", "hidden");
        w.end_folder();
        let after = w.label("after", "visible");

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());
        assert!(
            w.response(after).rect.size().y > 0.0,
            "分组之后的控件被吞了"
        );
    }

    #[test]
    fn a_forgotten_end_folder_does_not_leak_into_the_next_frame() {
        // 忘了 `end_folder` 是常见手误。`begin` 必须把折叠状态清掉，
        // 否则下一帧整个面板都是空的——而且看不出原因。
        let mut w = WidgetUi::default();
        w.set_folder_open(Id::new("f"), false);
        w.begin();
        w.folder("f", "Section");
        w.label("inside", "hidden");
        // 故意不调 end_folder

        w.begin();
        let normal = w.label("normal", "visible");

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());
        assert!(
            w.response(normal).rect.size().y > 0.0,
            "上一帧没收的折叠漏到了这一帧，整个面板是空的"
        );
    }
}
