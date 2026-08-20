//! 基础控件。
//!
//! 这一层把前三块串起来：[`layout`](crate::layout) 算矩形、
//! [`interact`](crate::interact) 判交互、[`draw`](crate::draw) 出几何。
//!
//! # 三段式
//!
//! ```text
//! 1. 声明   用户建一棵 LayoutNode 树，控件只贡献节点，不画东西
//! 2. 求解   taffy 一次算出全部绝对矩形；交互按这些矩形判定
//! 3. 绘制   走一遍树，按各自的 Response 出几何
//! ```
//!
//! 分三段是被逼的：一个按钮是不是「悬停」取决于它的矩形，而矩形要等
//! 兄弟和父亲都排完才知道。想在声明的那一刻就知道结果，只有 egui 那种
//! 游标式布局做得到——那样就没法用 flexbox 了。
//!
//! 代价是**响应滞后一帧**：`ui.button(...)` 声明的按钮，要到下一次
//! `ui.finish()` 之后才能查到它被点了。对 HUD 与菜单这不构成问题；
//! 这一点在 [`WidgetUi::response`] 上写明了。

use crate::layout::{AlignCross, Direction, Edges, Id, Justify, LayoutNode, Length, Style};
use crate::{Rect, Response, Ui};
use kfont::TextStyle;
use kmath::{Vec2, Vec4};

/// 控件的配色与尺寸。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// 面板底色。
    pub panel: Vec4,
    /// 控件常态底色。
    pub surface: Vec4,
    /// 悬停时的底色。
    pub hovered: Vec4,
    /// 按下时的底色。
    pub active: Vec4,
    /// 主色（选中、滑块）。
    pub accent: Vec4,
    /// 正文颜色。
    pub text: Vec4,
    /// 次要文字。
    pub dim: Vec4,
    /// 边框颜色。
    pub outline: Vec4,
    /// 焦点框颜色。
    pub focus: Vec4,
    /// 圆角半径。
    pub radius: f32,
    /// 控件内边距。
    pub padding: Edges,
    /// 字号。
    pub font_size: f32,
    /// 控件的最小高度。
    pub row_height: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel: Vec4::new(0.10, 0.11, 0.14, 0.94),
            surface: Vec4::new(0.18, 0.19, 0.24, 1.0),
            hovered: Vec4::new(0.24, 0.26, 0.32, 1.0),
            active: Vec4::new(0.14, 0.15, 0.19, 1.0),
            accent: Vec4::new(0.25, 0.55, 1.00, 1.0),
            text: Vec4::new(0.92, 0.93, 0.96, 1.0),
            dim: Vec4::new(0.55, 0.58, 0.66, 1.0),
            outline: Vec4::new(1.0, 1.0, 1.0, 0.14),
            focus: Vec4::new(0.45, 0.70, 1.00, 0.9),
            radius: 6.0,
            padding: Edges::axes(12.0, 6.0),
            font_size: 15.0,
            row_height: 30.0,
        }
    }
}

/// 一个控件：声明期贡献布局节点，绘制期出几何。
#[derive(Debug, Clone)]
enum Widget {
    /// 纯容器，只画底色。
    Panel { color: Vec4, radius: f32 },
    /// 一段文字。
    Label {
        text: String,
        color: Vec4,
        size: f32,
    },
    /// 按钮。
    Button { text: String },
    /// 复选框。
    Checkbox { text: String, checked: bool },
    /// 滑条。`value` 已归一化到 0..=1。
    Slider { value: f32 },
}

/// 一个已经声明、等待求解的控件。
#[derive(Debug, Clone)]
struct Declared {
    id: Id,
    widget: Widget,
}

/// 在 [`Ui`] 之上叠一层控件能力。
///
/// 每帧的用法：
///
/// ```no_run
/// # use kui::{widgets::WidgetUi, Ui, UiInput};
/// # let mut ui = Ui::new();
/// # let mut widgets = WidgetUi::default();
/// # let input = UiInput::default();
/// widgets.begin();
/// let start = widgets.button("start", "开始游戏");
/// widgets.checkbox("sound", "音效", true);
/// widgets.finish(&mut ui, &input);
///
/// if widgets.response(start).clicked {
///     // 注意：这是**上一帧**声明的那个按钮的结果，见下面的说明。
/// }
/// ```
#[derive(Debug, Default)]
pub struct WidgetUi {
    /// 配色。
    pub theme: Theme,
    /// 本帧声明的控件，按声明顺序。
    declared: Vec<Declared>,
    /// 本帧的根节点样式。
    root_style: Style,
    /// 交互状态，跨帧。
    interaction: crate::Interaction,
    /// 上一次求解的结果。
    solved: crate::layout::Solved,
}

impl WidgetUi {
    /// 开一帧，清掉上一帧的声明。
    pub fn begin(&mut self) {
        self.declared.clear();
        self.root_style = Style {
            direction: Direction::Column,
            align: AlignCross::Start,
            padding: Edges::all(16.0),
            gap: 8.0,
            ..Default::default()
        };
    }

    /// 改根容器的样式（方向、间隔、内边距）。
    pub fn root_style(&mut self, style: Style) {
        self.root_style = style;
    }

    /// 一个控件本帧的交互结果。
    ///
    /// **滞后一帧**：控件的矩形要等整棵树排完才知道，而排版发生在
    /// [`finish`](Self::finish) 里。所以这里查到的是上一次 `finish`
    /// 之后的结果。对 HUD 与菜单不构成问题——按钮晚一帧响应看不出来。
    pub fn response(&self, id: Id) -> Response {
        self.interaction.response(id)
    }

    /// 指针是不是落在 UI 上。为真时游戏逻辑该吃掉这次点击。
    pub fn wants_pointer(&self) -> bool {
        self.interaction.wants_pointer()
    }

    /// 有控件正在接收键盘输入。为真时游戏不该再处理 WASD。
    pub fn wants_keyboard(&self) -> bool {
        self.interaction.wants_keyboard()
    }

    // ───────────────────────── 声明 ─────────────────────────

    /// 一段文字。
    pub fn label(&mut self, id: &str, text: impl Into<String>) -> Id {
        let color = self.theme.text;
        let size = self.theme.font_size;
        self.push(
            id,
            Widget::Label {
                text: text.into(),
                color,
                size,
            },
        )
    }

    /// 一段次要文字。
    pub fn dim_label(&mut self, id: &str, text: impl Into<String>) -> Id {
        let color = self.theme.dim;
        let size = self.theme.font_size;
        self.push(
            id,
            Widget::Label {
                text: text.into(),
                color,
                size,
            },
        )
    }

    /// 一个按钮。返回它的 id，用 [`response`](Self::response) 查是否被点。
    pub fn button(&mut self, id: &str, text: impl Into<String>) -> Id {
        self.push(id, Widget::Button { text: text.into() })
    }

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

    /// 一块面板底色。通常作为根容器的背景。
    pub fn panel(&mut self, id: &str) -> Id {
        let color = self.theme.panel;
        let radius = self.theme.radius + 4.0;
        self.push(id, Widget::Panel { color, radius })
    }

    fn push(&mut self, id: &str, widget: Widget) -> Id {
        let id = Id::new(id);
        self.declared.push(Declared { id, widget });
        id
    }

    // ───────────────────────── 求解与绘制 ─────────────────────────

    /// 排版、判交互、出几何。一帧的收尾。
    ///
    /// `origin` 由根样式的外边距决定；整棵树从屏幕左上角开始排。
    pub fn finish(&mut self, ui: &mut Ui, input: &crate::UiInput) {
        let screen = ui.screen();
        let root = self.build_tree(ui);
        self.solved = crate::layout::solve(&root, screen);

        // 交互按前序的矩形判定；后面的画在上面，命中时从后往前找。
        let hits: Vec<(Id, Rect)> = self
            .solved
            .iter()
            .filter(|(id, _)| self.declared.iter().any(|d| d.id == *id))
            .collect();
        self.interaction.update(&hits, input);

        self.paint(ui);
    }

    /// 把声明变成布局树。
    fn build_tree(&self, ui: &Ui) -> LayoutNode {
        let theme = self.theme;
        let children = self.declared.iter().map(|declared| {
            let style = Style {
                width: match declared.widget {
                    // 滑条要占满一行才好拖。
                    Widget::Slider { .. } => Length::Percent(1.0),
                    _ => Length::Auto,
                },
                min_size: Vec2::new(0.0, theme.row_height),
                padding: match declared.widget {
                    // 文字不加内边距——加了之后一行文字看着像个按钮。
                    Widget::Label { .. } => Edges::default(),
                    _ => theme.padding,
                },
                justify: Justify::Center,
                align: AlignCross::Center,
                ..Default::default()
            };

            // 内容的固有尺寸。文字节点不给的话 flexbox 认为它零尺寸，
            // 整行塌陷，界面上什么都看不见。
            let content = self.content_size(ui, &declared.widget);
            LayoutNode::leaf(declared.id, style, content)
        });

        LayoutNode::new(Id::new("__ui_root"), self.root_style).with_children(children)
    }

    /// 一个控件的内容有多大。
    fn content_size(&self, ui: &Ui, widget: &Widget) -> Vec2 {
        let theme = self.theme;
        let style = |size: f32| TextStyle {
            size,
            ..Default::default()
        };
        match widget {
            Widget::Panel { .. } => Vec2::ZERO,
            Widget::Label { text, size, .. } => ui.measure(text, &style(*size), None).size,
            Widget::Button { text } => ui.measure(text, &style(theme.font_size), None).size,
            Widget::Checkbox { text, .. } => {
                let text_size = ui.measure(text, &style(theme.font_size), None).size;
                // 勾选框本体加一点间隔。
                Vec2::new(text_size.x + theme.row_height, text_size.y)
            }
            Widget::Slider { .. } => Vec2::new(120.0, theme.font_size),
        }
    }

    /// 走一遍求解结果，出几何。
    fn paint(&self, ui: &mut Ui) {
        let theme = self.theme;
        for declared in &self.declared {
            let Some(rect) = self.solved.rect(declared.id) else {
                continue;
            };
            let response = self.interaction.response(declared.id);

            match &declared.widget {
                Widget::Panel { color, radius } => {
                    ui.rounded_rect(rect, *radius, *color);
                    ui.border(rect, *radius, 1.0, theme.outline);
                }

                Widget::Label { text, color, size } => {
                    ui.text(
                        rect.min,
                        text,
                        &TextStyle {
                            size: *size,
                            ..Default::default()
                        },
                        *color,
                        Some(rect.size().x),
                    );
                }

                Widget::Button { text } => {
                    let fill = if response.held {
                        theme.active
                    } else if response.hovered {
                        theme.hovered
                    } else {
                        theme.surface
                    };
                    ui.rounded_rect(rect, theme.radius, fill);
                    ui.border(rect, theme.radius, 1.0, theme.outline);
                    if response.focused {
                        // 焦点框画在外面一圈，免得和边框糊在一起。
                        ui.border(rect.shrink(-2.0), theme.radius + 2.0, 2.0, theme.focus);
                    }
                    ui.text_centered(
                        rect,
                        text,
                        &TextStyle {
                            size: theme.font_size,
                            ..Default::default()
                        },
                        theme.text,
                    );
                }

                Widget::Checkbox { text, checked } => {
                    let box_size = theme.row_height * 0.6;
                    let box_rect = Rect {
                        min: Vec2::new(rect.min.x, rect.center().y - box_size * 0.5),
                        max: Vec2::new(rect.min.x + box_size, rect.center().y + box_size * 0.5),
                    };
                    let fill = if *checked {
                        theme.accent
                    } else if response.hovered {
                        theme.hovered
                    } else {
                        theme.surface
                    };
                    ui.rounded_rect(box_rect, 3.0, fill);
                    ui.border(box_rect, 3.0, 1.0, theme.outline);
                    if *checked {
                        // 勾用一个内缩的实心方块表示。真的勾要画折线，
                        // 那需要非矩形图元——留给以后。
                        ui.rounded_rect(box_rect.shrink(box_size * 0.28), 1.5, Vec4::ONE);
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

                Widget::Slider { value } => {
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
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PointerButton, UiInput};

    /// 一个不带字体的 UI。文字量出来是零尺寸，但布局与交互照常。
    fn ui() -> Ui {
        let mut ui = Ui::new();
        ui.begin_frame(Vec2::new(800.0, 600.0), 1.0);
        ui
    }

    fn at(x: f32, y: f32) -> UiInput {
        UiInput {
            pointer: Some(Vec2::new(x, y)),
            ..Default::default()
        }
    }

    #[test]
    fn widgets_get_laid_out_in_a_column() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.button("a", "上");
        let b = w.button("b", "下");
        w.finish(&mut ui, &UiInput::default());

        let ra = w.response(a).rect;
        let rb = w.response(b).rect;
        assert!(rb.min.y > ra.min.y, "第二个按钮该在第一个下面");
        assert_eq!(ra.min.x, rb.min.x);
    }

    #[test]
    fn widgets_respect_the_minimum_row_height() {
        // 没字体时文字是零尺寸。不设最小高度的话按钮会塌成一条线，
        // 根本点不着。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let id = w.button("a", "");
        w.finish(&mut ui, &UiInput::default());

        assert!(w.response(id).rect.size().y >= w.theme.row_height);
    }

    #[test]
    fn a_button_reports_hover_and_click() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        // 第一帧：声明并排版。
        w.begin();
        let id = w.button("a", "点我");
        w.finish(&mut ui, &UiInput::default());
        let rect = w.response(id).rect;
        let center = rect.center();

        // 第二帧：指针移上去。
        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &at(center.x, center.y));
        assert!(w.response(id).hovered);

        // 第三帧：按下。
        let mut input = at(center.x, center.y);
        input.pressed.push(PointerButton::Primary);
        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &input);
        assert!(w.response(id).held);
        assert!(!w.response(id).clicked);

        // 第四帧：松开。
        let mut input = at(center.x, center.y);
        input.released.push(PointerButton::Primary);
        w.begin();
        w.button("a", "点我");
        w.finish(&mut ui, &input);
        assert!(w.response(id).clicked);
    }

    #[test]
    fn clicking_one_button_does_not_trigger_the_other() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.button("a", "甲");
        let b = w.button("b", "乙");
        w.finish(&mut ui, &UiInput::default());

        let center = w.response(a).rect.center();
        let mut input = at(center.x, center.y);
        input.pressed.push(PointerButton::Primary);
        w.begin();
        w.button("a", "甲");
        w.button("b", "乙");
        w.finish(&mut ui, &input);

        let mut input = at(center.x, center.y);
        input.released.push(PointerButton::Primary);
        w.begin();
        w.button("a", "甲");
        w.button("b", "乙");
        w.finish(&mut ui, &input);

        assert!(w.response(a).clicked);
        assert!(!w.response(b).clicked);
    }

    #[test]
    fn widgets_produce_geometry() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.button("a", "按钮");
        w.checkbox("b", "开关", true);
        w.slider("c", 0.5);
        w.finish(&mut ui, &UiInput::default());
        ui.end_frame();

        assert!(!ui.draw_list().is_empty(), "控件该画出东西来");
        // 没有裁剪、没有换纹理，一次绘制画完。
        assert_eq!(ui.draw_list().batches().len(), 1);
    }

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

    #[test]
    fn the_slider_knob_stays_inside_the_track() {
        // 不夹的话，拖到两端时滑块会掉出去一半。
        for value in [0.0, 1.0] {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            w.begin();
            let id = w.slider("s", value);
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();

            let track = w.response(id).rect;
            for v in ui.draw_list().vertices() {
                assert!(
                    v.position[0] >= track.min.x - 0.01 && v.position[0] <= track.max.x + 0.01,
                    "value={value} 时滑块跑出了轨道：{}",
                    v.position[0]
                );
            }
        }
    }

    #[test]
    fn ui_claims_the_pointer_only_when_it_is_over_a_widget() {
        // 鼠标在菜单上时，点击不该同时打到场景里去。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let id = w.button("a", "按钮");
        w.finish(&mut ui, &UiInput::default());
        let center = w.response(id).rect.center();

        w.begin();
        w.button("a", "按钮");
        w.finish(&mut ui, &at(center.x, center.y));
        assert!(w.wants_pointer());

        w.begin();
        w.button("a", "按钮");
        w.finish(&mut ui, &at(700.0, 500.0));
        assert!(!w.wants_pointer());
    }

    #[test]
    fn tab_moves_focus_between_widgets() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };

        w.begin();
        let a = w.button("a", "甲");
        let b = w.button("b", "乙");
        w.finish(&mut ui, &tab);
        assert!(w.response(a).focused);

        w.begin();
        w.button("a", "甲");
        w.button("b", "乙");
        w.finish(&mut ui, &tab);
        assert!(w.response(b).focused);
        assert!(w.wants_keyboard());
    }

    #[test]
    fn begin_clears_the_previous_declarations() {
        // 不清的话控件会一帧帧累积，界面越来越长。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        for _ in 0..5 {
            w.begin();
            w.button("a", "按钮");
            w.finish(&mut ui, &UiInput::default());
        }
        assert_eq!(w.declared.len(), 1);
    }

    #[test]
    fn a_row_root_lays_widgets_out_horizontally() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        w.root_style(Style {
            direction: Direction::Row,
            gap: 8.0,
            ..Default::default()
        });
        let a = w.button("a", "甲");
        let b = w.button("b", "乙");
        w.finish(&mut ui, &UiInput::default());

        assert!(w.response(b).rect.min.x > w.response(a).rect.min.x);
        assert_eq!(w.response(a).rect.min.y, w.response(b).rect.min.y);
    }

    #[test]
    fn an_undeclared_id_reports_nothing() {
        let w = WidgetUi::default();
        assert!(!w.response(Id::new("不存在")).clicked);
    }
}
