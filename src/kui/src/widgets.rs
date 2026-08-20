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
    /// 文本框。
    TextInput {
        /// 当前内容的一份快照。绘制期要用。
        text: String,
        /// 没内容时显示的提示。
        placeholder: String,
    },
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
    /// 本帧生效的滚动区（`begin` 时从 `open_scroll` 取过来）。
    scroll_frame: Option<ScrollFrame>,
    /// 各控件本帧的最终矩形（已经算上滚动偏移）。
    rects: Vec<Rect>,
    /// 每个文本框的编辑状态（光标、选区），跨帧。
    edits: std::collections::HashMap<Id, crate::TextEdit>,
    /// 每个滚动区的滚动位置，跨帧。
    scroll: std::collections::HashMap<Id, f32>,
}

/// 本帧的滚动区。
///
/// 只支持一个：嵌套滚动区的手感本来就很差（滚轮该滚哪个？），
/// 实现还要一整套事件冒泡。不做，也不假装做了。
#[derive(Debug, Clone, Copy)]
struct ScrollFrame {
    id: Id,
    /// 视口高度。
    height: f32,
    /// 这个滚动区是从第几个声明开始的。
    first: usize,
    /// 到第几个为止（不含）。`end_scroll` 之前是 `usize::MAX`。
    last: usize,
}

impl WidgetUi {
    /// 开一帧，清掉上一帧的声明。
    pub fn begin(&mut self) {
        self.declared.clear();
        self.scroll_frame = None;
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

    /// 一个文本框。
    ///
    /// **直接改传进来的 `text`**：编辑动作在声明期就应用了，不像别的控件
    /// 那样滞后一帧——打字滞后一帧会让人以为按键丢了。
    ///
    /// 只在这个文本框**有焦点**时才处理输入。
    pub fn text_input(
        &mut self,
        id: &str,
        text: &mut String,
        placeholder: impl Into<String>,
        input: &crate::UiInput,
    ) -> Id {
        let id = Id::new(id);
        let focused = self.interaction.focused() == Some(id);

        let edit = self.edits.entry(id).or_default();
        // 文本可能被外部改过（读档、重置）。不夹的话光标停在旧位置，
        // 下一次切片直接 panic。
        edit.clamp(text);

        if focused {
            for action in &input.edits {
                apply_edit(edit, text, *action);
            }
            if !input.text.is_empty() {
                edit.insert(text, &input.text);
            }
        }

        let snapshot = text.clone();
        self.declared.push(Declared {
            id,
            widget: Widget::TextInput {
                text: snapshot,
                placeholder: placeholder.into(),
            },
        });
        id
    }

    /// 一个文本框的光标与选区。
    pub fn text_state(&self, id: Id) -> crate::TextEdit {
        self.edits.get(&id).copied().unwrap_or_default()
    }

    // ───────────────────────── 滚动区 ─────────────────────────

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

    /// 某个声明是不是在滚动区里。
    fn scrolled(&self, index: usize) -> Option<ScrollFrame> {
        self.scroll_frame
            .filter(|frame| index >= frame.first && index < frame.last)
    }

    // ───────────────────────── 求解与绘制 ─────────────────────────

    /// 排版、判交互、出几何。一帧的收尾。
    ///
    /// `origin` 由根样式的外边距决定；整棵树从屏幕左上角开始排。
    pub fn finish(&mut self, ui: &mut Ui, input: &crate::UiInput) {
        let screen = ui.screen();
        let root = self.build_tree(ui);
        self.solved = crate::layout::solve(&root, screen);

        self.rects = self
            .declared
            .iter()
            .map(|d| self.solved.rect(d.id).unwrap_or_default())
            .collect();

        self.apply_scroll(input);

        // 交互按前序的矩形判定；后面的画在上面，命中时从后往前找。
        //
        // 用**滚动之后**的矩形：不然滚下去之后，点击命中的还是原位置，
        // 表现为「点这一行，亮的是另一行」。
        let hits: Vec<(Id, Rect)> = self
            .declared
            .iter()
            .zip(&self.rects)
            .map(|(d, rect)| (d.id, *rect))
            .collect();
        self.interaction.update(&hits, input);

        self.paint(ui);
    }

    /// 处理滚轮、夹取偏移，并把滚动区里的矩形整体上移。
    fn apply_scroll(&mut self, input: &crate::UiInput) {
        let Some(frame) = self.scroll_frame else {
            return;
        };
        let Some(viewport) = self.scroll_viewport() else {
            return;
        };

        // 内容总高：从第一个被滚的控件顶端到最后一个的底端。
        let end = frame.last.min(self.rects.len());
        let content = self.rects[frame.first..end]
            .iter()
            .fold(0.0f32, |h, r| h.max(r.max.y))
            - viewport.min.y;
        let max_offset = (content - frame.height).max(0.0);

        let offset = self.scroll.entry(frame.id).or_insert(0.0);
        // 指针在视口里才响应滚轮，否则页面上所有滚动区会一起滚。
        if input.pointer.is_some_and(|p| viewport.contains(p)) {
            *offset -= input.scroll.y * 40.0;
        }
        // 夹取要在**每帧**做，不只是滚的时候：内容变短之后，
        // 旧偏移会把内容整个顶出视口，看起来像列表空了。
        *offset = offset.clamp(0.0, max_offset);
        let offset = *offset;

        for rect in &mut self.rects[frame.first..end] {
            rect.min.y -= offset;
            rect.max.y -= offset;
        }
    }

    /// 滚动区的视口矩形。
    fn scroll_viewport(&self) -> Option<Rect> {
        let frame = self.scroll_frame?;
        let end = frame.last.min(self.rects.len());
        let first = self.rects.get(frame.first)?;
        Some(Rect {
            min: Vec2::new(first.min.x, first.min.y),
            max: Vec2::new(
                self.rects[frame.first..end]
                    .iter()
                    .fold(first.max.x, |w, r| w.max(r.max.x)),
                first.min.y + frame.height,
            ),
        })
    }

    /// 把声明变成布局树。
    fn build_tree(&self, ui: &Ui) -> LayoutNode {
        let theme = self.theme;
        let children = self.declared.iter().map(|declared| {
            let style = Style {
                width: match declared.widget {
                    // 滑条要占满一行才好拖。
                    // 滑条和文本框都要占满一行才好用。
                    Widget::Slider { .. } | Widget::TextInput { .. } => Length::Percent(1.0),
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
            let mut style = style;
            // 最小宽度也按内容兜底。
            //
            // 固有尺寸只在宽度是 `Auto` 时生效，而滑条和文本框用的是
            // `Percent(1.0)`——在宽度不确定的父容器里，百分比解析成 0，
            // 于是一个空文本框会塌成零宽，根本点不进去。
            style.min_size.x = style
                .min_size
                .x
                .max(content.x + theme.padding.left + theme.padding.right);
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
            Widget::TextInput { text, placeholder } => {
                // 按内容和提示里较宽的那个量，但至少留出一段可打字的宽度——
                // 空文本框宽度为零的话根本点不进去。
                let shown = if text.is_empty() { placeholder } else { text };
                let size = ui.measure(shown, &style(theme.font_size), None).size;
                Vec2::new(size.x.max(160.0), theme.font_size)
            }
        }
    }

    /// 走一遍求解结果，出几何。
    fn paint(&self, ui: &mut Ui) {
        let theme = self.theme;
        let viewport = self.scroll_viewport();
        let mut clipped = false;

        for (index, declared) in self.declared.iter().enumerate() {
            let rect = self.rects[index];

            // 进滚动区时压一层裁剪，出来时弹掉。
            let inside = self.scrolled(index).is_some();
            if inside != clipped {
                match (inside, viewport) {
                    (true, Some(v)) => ui.push_clip(v),
                    (true, None) => {}
                    (false, _) => ui.pop_clip(),
                }
                clipped = inside;
            }

            // 滚出视口的控件连几何都不生成。
            //
            // 不跳的话，一个一千行的列表每帧都要为看不见的九百多行
            // 生成顶点——CPU 和带宽全花在裁剪之后会被丢掉的东西上。
            if inside
                && let Some(v) = viewport
                && rect.intersect(&v).is_empty()
            {
                continue;
            }

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

                Widget::TextInput { text, placeholder } => {
                    let fill = if response.focused {
                        theme.active
                    } else if response.hovered {
                        theme.hovered
                    } else {
                        theme.surface
                    };
                    ui.rounded_rect(rect, theme.radius, fill);
                    ui.border(
                        rect,
                        theme.radius,
                        if response.focused { 2.0 } else { 1.0 },
                        if response.focused {
                            theme.focus
                        } else {
                            theme.outline
                        },
                    );

                    let inner = rect.shrink(theme.padding.left.min(theme.padding.top).max(6.0));
                    let text_style = TextStyle {
                        size: theme.font_size,
                        wrap: kfont::Wrap::None,
                        ..Default::default()
                    };
                    let baseline = Vec2::new(inner.min.x, rect.center().y - theme.font_size * 0.62);

                    // 内容会比框长。裁剪掉溢出的部分，否则文字会画到
                    // 框外面、盖住旁边的控件。
                    ui.push_clip(inner);

                    let edit = self.text_state(declared.id);
                    // 选区先画，文字盖在上面。反过来的话高亮会糊住文字。
                    if edit.has_selection() {
                        let range = edit.selection();
                        let before = ui.measure(&text[..range.start], &text_style, None).size.x;
                        let width = ui.measure(&text[range.clone()], &text_style, None).size.x;
                        ui.rect(
                            Rect {
                                min: Vec2::new(baseline.x + before, inner.min.y),
                                max: Vec2::new(baseline.x + before + width, inner.max.y),
                            },
                            theme.accent * Vec4::new(1.0, 1.0, 1.0, 0.45),
                        );
                    }

                    if text.is_empty() {
                        ui.text(baseline, placeholder, &text_style, theme.dim, None);
                    } else {
                        ui.text(baseline, text, &text_style, theme.text, None);
                    }

                    // 光标。只在有焦点时画，否则每个文本框里都杵着一根竖线。
                    if response.focused {
                        let before = ui
                            .measure(&text[..edit.cursor().min(text.len())], &text_style, None)
                            .size
                            .x;
                        ui.rect(
                            Rect {
                                min: Vec2::new(baseline.x + before, inner.min.y),
                                max: Vec2::new(baseline.x + before + 1.5, inner.max.y),
                            },
                            theme.text,
                        );
                    }

                    ui.pop_clip();
                }
            }
        }

        if clipped {
            ui.pop_clip();
        }
    }
}

/// 把一个编辑动作应用到状态上。
fn apply_edit(edit: &mut crate::TextEdit, text: &mut String, action: crate::EditAction) {
    use crate::EditAction as A;
    match action {
        A::Backspace => edit.backspace(text),
        A::Delete => edit.delete(text),
        A::Left { select } => edit.move_left(text, select),
        A::Right { select } => edit.move_right(text, select),
        A::Home { select } => edit.move_home(select),
        A::End { select } => edit.move_end(text, select),
        A::SelectAll => edit.select_all(text),
        // 提交与取消由调用方处理——文本框不知道回车该干什么。
        A::Submit | A::Cancel => {}
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

    /// 让某个控件拿到焦点：Tab 一次就走到第一个。
    fn focus_first(w: &mut WidgetUi, ui: &mut Ui, declare: impl Fn(&mut WidgetUi)) {
        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        w.begin();
        declare(w);
        w.finish(ui, &tab);
    }

    #[test]
    fn typing_into_a_focused_text_input_changes_the_text() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::new();

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::new();
            w.text_input("name", &mut scratch, "名字", &UiInput::default());
        });

        let input = UiInput {
            text: "中文".to_string(),
            ..Default::default()
        };
        w.begin();
        w.text_input("name", &mut text, "名字", &input);
        w.finish(&mut ui, &input);

        assert_eq!(text, "中文");
    }

    #[test]
    fn an_unfocused_text_input_ignores_typing() {
        // 不判焦点的话，界面上每个文本框都会同时收到同一串字。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut a = String::new();
        let mut b = String::new();

        // 先让第一个拿到焦点。
        focus_first(&mut w, &mut ui, |w| {
            let mut s1 = String::new();
            let mut s2 = String::new();
            w.text_input("a", &mut s1, "", &UiInput::default());
            w.text_input("b", &mut s2, "", &UiInput::default());
        });

        let input = UiInput {
            text: "x".to_string(),
            ..Default::default()
        };
        w.begin();
        w.text_input("a", &mut a, "", &input);
        w.text_input("b", &mut b, "", &input);
        w.finish(&mut ui, &input);

        assert_eq!(a, "x");
        assert_eq!(b, "", "没有焦点的文本框不该收到输入");
    }

    #[test]
    fn backspace_in_a_text_input_removes_a_whole_character() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::from("中文");

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::from("中文");
            w.text_input("t", &mut scratch, "", &UiInput::default());
        });
        // 光标要先到末尾。
        let to_end = UiInput {
            edits: vec![crate::EditAction::End { select: false }],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &to_end);
        w.finish(&mut ui, &to_end);

        let backspace = UiInput {
            edits: vec![crate::EditAction::Backspace],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &backspace);
        w.finish(&mut ui, &backspace);

        assert_eq!(text, "中");
    }

    #[test]
    fn a_text_input_survives_the_text_being_replaced_externally() {
        // 读档、重置会把文本整个换掉。光标不夹回去的话下一次切片就 panic。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::from("很长的一段内容");

        focus_first(&mut w, &mut ui, |w| {
            let mut scratch = String::from("很长的一段内容");
            w.text_input("t", &mut scratch, "", &UiInput::default());
        });
        let to_end = UiInput {
            edits: vec![crate::EditAction::End { select: false }],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &to_end);
        w.finish(&mut ui, &to_end);

        // 外部换成短的。
        text = String::from("短");
        w.begin();
        w.text_input("t", &mut text, "", &UiInput::default());
        w.finish(&mut ui, &UiInput::default());

        assert!(w.text_state(Id::new("t")).cursor() <= text.len());
    }

    #[test]
    fn an_empty_text_input_still_has_a_clickable_width() {
        // 宽度按内容算的话，空文本框会塌成零宽，根本点不进去。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut text = String::new();
        w.begin();
        let id = w.text_input("t", &mut text, "", &UiInput::default());
        w.finish(&mut ui, &UiInput::default());

        assert!(w.response(id).rect.size().x >= 160.0);
    }

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
    fn an_undeclared_id_reports_nothing() {
        let w = WidgetUi::default();
        assert!(!w.response(Id::new("不存在")).clicked);
    }
}
