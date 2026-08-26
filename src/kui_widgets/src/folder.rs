//! 可折叠分组。展开状态是纯外观，破例由控件自己存。

use kmath::Vec2;
use kfont::TextStyle;
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
pub(crate) fn paint(ui: &mut Ui, theme: &Theme, rect: Rect, response: &Response, text: &str, open: bool) {
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
