//! 基础控件。
//!
//! 这一层把前三块串起来：[`布局`](kui::solve) 算矩形、
//! [`interact`](kui::interact) 判交互、[`draw`](kui::draw) 出几何。
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

use kfont::TextStyle;
use kmath::{Vec2, Vec4};
use kui::{AlignCross, Direction, Edges, Id, Justify, LayoutNode, Length, Style};
use kui::{Rect, Response, Ui};

/// 文字样式。各控件量尺寸、出几何都用它，省得每处重写一遍。
pub(crate) fn text_style(size: f32) -> TextStyle {
    TextStyle {
        size,
        ..Default::default()
    }
}


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
    /// 模态遮罩色。半透明的黑，压暗背景但不完全盖住——
    /// 全黑的话用户会以为界面卡死了。
    pub modal: Vec4,
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
            modal: Vec4::new(0.0, 0.0, 0.0, 0.55),
            radius: 6.0,
            padding: Edges::axes(12.0, 6.0),
            font_size: 15.0,
            row_height: 30.0,
        }
    }
}

/// 一个控件：声明期贡献布局节点，绘制期出几何。
#[derive(Debug, Clone)]
pub(crate) enum Widget {
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
    /// 可折叠分组的标题条。
    Folder { text: String, open: bool },
    /// 单选按钮。和复选框的区别是它画成圆的，而且语义上「一组里只能选一个」。
    Radio { text: String, selected: bool },
    /// 列表里的一行。
    ListItem { text: String, selected: bool },
    /// 模态遮罩：铺满整屏、压暗背景、吃掉所有点击。
    Modal { color: Vec4 },
    /// 对话框的标题栏。可拖动，右端一个关闭按钮。
    DialogTitle { text: String },
    /// 滚动条。`fraction` 是滑块占轨道的比例，`offset` 是滑块起点的比例。
    Scrollbar { fraction: f32, offset: f32 },
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
pub(crate) struct Declared {
    pub(crate) id: Id,
    pub(crate) widget: Widget,
    /// 这个控件属于哪一行。[`None`] 表示直接挂在根上。
    ///
    /// 行是**扁平表示**的：声明列表仍然是一条线，行号只是个分组标记，
    /// 建树时把连号的合成一个子容器。这样 `response` / `rects` 那套
    /// 按下标索引的逻辑一个字都不用改。
    pub(crate) row: Option<usize>,
    /// 在行内是否占据剩余空间。
    ///
    /// lil-gui 那种「标签在左、控件在右」的行，靠的就是让标签把剩下的
    /// 空间全占了，把控件挤到右边。
    pub(crate) grow: bool,
}

/// 在 [`Ui`] 之上叠一层控件能力。
///
/// 每帧的用法：
///
/// ```no_run
/// # use kui::{Ui, UiInput};
/// # use kui_widgets::WidgetUi;
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
#[derive(Debug)]
pub struct WidgetUi {
    /// 配色。
    pub theme: Theme,
    /// 本帧声明的控件，按声明顺序。
    pub(crate) declared: Vec<Declared>,
    /// 根容器的样式。**跨帧保留**，`begin` 不会清它。
    root_style: Style,
    /// 交互状态，跨帧。
    pub(crate) interaction: kui::Interaction,
    /// 上一次求解的结果。
    solved: kui::Solved,
    /// 当前打开的行号；[`None`] 表示不在行里。
    pub(crate) open_row: Option<usize>,
    /// 本帧已经开过几行，用来发行号。
    rows: usize,
    /// 当前行还没有控件——下一个进来的要占满剩余宽度。
    pub(crate) row_first: bool,
    /// 本帧生效的滚动区（`begin` 时从 `open_scroll` 取过来）。
    pub(crate) scroll_frame: Option<ScrollFrame>,
    /// 各控件本帧的最终矩形（已经算上滚动偏移）。
    rects: Vec<Rect>,
    /// 每个文本框的编辑状态（光标、选区），跨帧。
    pub(crate) edits: std::collections::HashMap<Id, crate::TextEdit>,
    /// 每个滚动区的滚动位置，跨帧。
    pub(crate) scroll: std::collections::HashMap<Id, f32>,
    /// 每个折叠分组开着还是收着，跨帧。
    ///
    /// 状态放在这里而不是让调用方保管：折叠纯粹是**外观**，和游戏逻辑
    /// 无关。让调用方为每个分组存一个 bool，只会让每个面板都多出一堆
    /// 和业务无关的字段。
    pub(crate) folders: std::collections::HashMap<Id, bool>,
    /// 上一次 `finish` 时的窗口尺寸。滚动区靠它夹住自己，不越界。
    screen: Vec2,
    /// 当前折叠分组收着——接下来声明的控件直接丢弃。
    ///
    /// 丢弃而不是「声明了但不画」：不画的话它们仍然占布局空间，
    /// 收起来的分组会留下一大片空白。
    pub(crate) collapsed: bool,
}

/// 本帧的滚动区。
///
/// 只支持一个：嵌套滚动区的手感本来就很差（滚轮该滚哪个？），
/// 实现还要一整套事件冒泡。不做，也不假装做了。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollFrame {
    pub(crate) id: Id,
    /// 视口高度。
    pub(crate) height: f32,
    /// 这个滚动区是从第几个声明开始的。
    pub(crate) first: usize,
    /// 到第几个为止（不含）。`end_scroll` 之前是 `usize::MAX`。
    pub(crate) last: usize,
}

impl Default for WidgetUi {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            declared: Vec::new(),
            // 一个能直接用的根样式：竖排、留边、控件之间有间隔。
            //
            // 放在这里而不是 `begin` 里：`begin` 每帧都跑，在那儿重置的话
            // 调用方设的样式会被静默吃掉。
            root_style: Style {
                direction: Direction::Column,
                align: AlignCross::Start,
                padding: Edges::all(16.0),
                gap: 8.0,
                ..Default::default()
            },
            interaction: Default::default(),
            solved: Default::default(),
            open_row: None,
            rows: 0,
            row_first: false,
            scroll_frame: None,
            rects: Vec::new(),
            edits: Default::default(),
            scroll: Default::default(),
            folders: Default::default(),
            screen: Vec2::ZERO,
            collapsed: false,
        }
    }
}

impl WidgetUi {
    /// 开一帧，清掉上一帧的声明。
    pub fn begin(&mut self) {
        self.declared.clear();
        self.scroll_frame = None;
        self.open_row = None;
        self.rows = 0;
        self.row_first = false;
        self.collapsed = false;
        // **不重置 `root_style`**：它是配置，不是每帧的声明。
        //
        // 之前这里会把它清回默认值，于是「先 `root_style` 再 `begin`」
        // 这个再自然不过的写法会被静默丢弃——面板背景画在右边、
        // 控件却排在屏幕最左角，而且不报任何错。
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

    // ───────────────────────── 滚动区 ─────────────────────────

    pub(crate) fn push(&mut self, id: &str, widget: Widget) -> Id {
        let id = Id::new(id);
        // 收起来的分组里的控件直接不声明。声明了再藏的话它们仍然占
        // 布局空间，收起来的分组会留下一大片空白。
        if self.collapsed {
            return id;
        }
        let row = self.open_row;
        // 行内的第一个控件占据剩余空间，把后面的挤到右边。
        let grow = row.is_some() && self.row_first;
        if row.is_some() {
            self.row_first = false;
        }
        self.declared.push(Declared {
            id,
            widget,
            row,
            grow,
        });
        id
    }

    /// 开一行：接下来声明的控件横着排，第一个占满剩余宽度。
    ///
    /// 这正是 lil-gui 那种「名字在左、控件在右」的行：
    ///
    /// ```text
    /// ┌──────────────────────────────────┐
    /// │ modify time scale    [====|    ] │
    /// └──────────────────────────────────┘
    /// ```
    ///
    /// 不支持嵌套——行里再开行的排版规则很快就说不清了，而 HUD 与
    /// 调试面板用不到。重复调用等于先收上一行。
    pub fn begin_row(&mut self) {
        self.rows += 1;
        self.open_row = Some(self.rows);
        self.row_first = true;
    }

    /// 收一行。
    pub fn end_row(&mut self) {
        self.open_row = None;
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
    pub fn finish(&mut self, ui: &mut Ui, input: &kui::UiInput) {
        let screen = ui.screen();
        self.screen = screen;
        let root = self.build_tree(ui);
        self.solved = kui::solve(&root, screen);

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

        // 折叠开关在这里翻转，不在 `folder()` 里——`folder()` 跑在声明期，
        // 那时本帧的点击还没判出来。在这里翻的话下一帧就是新状态，
        // 和别的控件「响应滞后一帧」的约定一致。
        //
        // 收集到临时表再写：`declared` 正被借着。
        let toggled: Vec<Id> = self
            .declared
            .iter()
            .filter(|d| matches!(d.widget, Widget::Folder { .. }))
            .filter(|d| self.interaction.response(d.id).clicked)
            .map(|d| d.id)
            .collect();
        for id in toggled {
            let open = self.folders.entry(id).or_insert(true);
            *open = !*open;
        }

        self.paint(ui);
    }

    /// 处理滚轮、夹取偏移，并把滚动区里的矩形整体上移。
    fn apply_scroll(&mut self, input: &kui::UiInput) {
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
        let mut viewport = Rect {
            min: Vec2::new(first.min.x, first.min.y),
            max: Vec2::new(
                self.rects[frame.first..end]
                    .iter()
                    .fold(first.max.x, |w, r| w.max(r.max.x)),
                first.min.y + frame.height,
            ),
        };

        // **夹进窗口**。
        //
        // 调用方给的高度是「我想要这么高」，但滚动区的起点由排版决定
        // （标题多一行、分组多一个，起点就往下挪）。让调用方自己算准
        // 剩余空间是算不准的——漏算一次，列表最后几行就画到窗口外面去了，
        // 而且看不出是被截断还是本来就没有。
        //
        // 夹在这里，任何面板都不会越界。
        if self.screen.y > 0.0 {
            viewport.max.y = viewport.max.y.min(self.screen.y);
            viewport.max.x = viewport.max.x.min(self.screen.x);
        }
        // 夹完之后高度可能变成负的（起点已经在窗口外）。收成零高度，
        // 内容整个不画，比画出一片翻转的矩形强。
        viewport.max.y = viewport.max.y.max(viewport.min.y);
        viewport.max.x = viewport.max.x.max(viewport.min.x);

        Some(viewport)
    }

    /// 把一个声明变成叶子节点。
    fn leaf_of(&self, ui: &Ui, declared: &Declared) -> LayoutNode {
        let theme = self.theme;
        let mut style = Style {
            width: match declared.widget {
                // 滑条和文本框都要占满一行才好用。
                Widget::Slider { .. }
                | Widget::TextInput { .. }
                | Widget::ListItem { .. }
                | Widget::Modal { .. }
                | Widget::DialogTitle { .. }
                | Widget::Scrollbar { .. } => Length::Percent(1.0),
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

        // 行内的第一个控件把剩余空间吃掉，把后面的挤到右边。
        if declared.grow {
            style.grow = 1.0;
        }

        // 内容的固有尺寸。文字节点不给的话 flexbox 认为它零尺寸，
        // 整行塌陷，界面上什么都看不见。
        let content = self.content_size(ui, &declared.widget);
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
    }

    /// 把声明变成布局树。
    ///
    /// 声明列表是扁平的，但带着行号。这里把**连号**的合并成一个横排的
    /// 子容器——于是 `response` / `rects` 那套按下标索引的逻辑不用改，
    /// 而排版上多了一层。
    fn build_tree(&self, ui: &Ui) -> LayoutNode {
        let mut children: Vec<LayoutNode> = Vec::new();
        let mut index = 0;

        while index < self.declared.len() {
            let declared = &self.declared[index];
            let Some(row) = declared.row else {
                children.push(self.leaf_of(ui, declared));
                index += 1;
                continue;
            };

            // 把这一行的所有控件收进一个横排容器。
            let mut row_children = Vec::new();
            while index < self.declared.len() && self.declared[index].row == Some(row) {
                row_children.push(self.leaf_of(ui, &self.declared[index]));
                index += 1;
            }

            let style = Style {
                direction: Direction::Row,
                align: AlignCross::Center,
                width: Length::Percent(1.0),
                gap: 6.0,
                ..Default::default()
            };
            // 行容器自己也要一个 id，不然 `Solved` 里查不到它——
            // 但它不是控件，不参与交互。用行号编一个不会和用户撞的。
            children.push(
                LayoutNode::new(Id::new(&format!("__ui_row_{row}")), style)
                    .with_children(row_children),
            );
        }

        LayoutNode::new(Id::new("__ui_root"), self.root_style).with_children(children)
    }

    /// 一个控件的内容有多大。
    fn content_size(&self, ui: &Ui, widget: &Widget) -> Vec2 {
        let theme = &self.theme;
        match widget {
            Widget::Panel { .. } => crate::panel::size(ui, theme),
            Widget::Label { text, size, .. } => crate::label::size(ui, theme, text, *size),
            Widget::Button { text, .. } => crate::button::size(ui, theme, text),
            Widget::Checkbox { text, .. } => crate::checkbox::size(ui, theme, text),
            Widget::Slider { .. } => crate::slider::size(ui, theme),
            Widget::Folder { text, .. } => crate::folder::size(ui, theme, text),
            Widget::Radio { text, .. } => crate::radio::size(ui, theme, text),
            Widget::ListItem { text, .. } => crate::list::size(ui, theme, text),
            Widget::Modal { .. } => crate::modal::size(ui, theme),
            Widget::DialogTitle { text, .. } => crate::dialog::size(ui, theme, text),
            Widget::Scrollbar { .. } => crate::scrollbar::size(ui, theme),
            Widget::TextInput { text, placeholder, .. } => crate::text_input::size(ui, theme, text, placeholder),
        }

    }

    /// 走一遍求解结果，出几何。
    fn paint(&self, ui: &mut Ui) {
        let theme = &self.theme;
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
                Widget::Panel { color, radius, .. } => crate::panel::paint(ui, theme, rect, &response, *color, *radius),
                Widget::Label { text, color, size, .. } => crate::label::paint(ui, theme, rect, &response, text, *color, *size),
                Widget::Button { text, .. } => crate::button::paint(ui, theme, rect, &response, text),
                Widget::Checkbox { text, checked, .. } => crate::checkbox::paint(ui, theme, rect, &response, text, *checked),
                Widget::Slider { value, .. } => crate::slider::paint(ui, theme, rect, &response, *value),
                Widget::Folder { text, open, .. } => crate::folder::paint(ui, theme, rect, &response, text, *open),
                Widget::Radio { text, selected, .. } => crate::radio::paint(ui, theme, rect, &response, text, *selected),
                Widget::ListItem { text, selected, .. } => crate::list::paint(ui, theme, rect, &response, text, *selected),
                Widget::Modal { color, .. } => crate::modal::paint(ui, theme, rect, &response, *color, self.screen),
                Widget::DialogTitle { text, .. } => crate::dialog::paint(ui, theme, rect, &response, text),
                Widget::Scrollbar { fraction, offset, .. } => crate::scrollbar::paint(ui, theme, rect, &response, *fraction, *offset),
                Widget::TextInput { text, placeholder, .. } => crate::text_input::paint(ui, theme, rect, &response, text, placeholder, &self.text_state(declared.id)),
            }
        }

        if clipped {
            ui.pop_clip();
        }
    }

}

/// 把一个编辑动作应用到状态上。
pub(crate) fn apply_edit(edit: &mut crate::TextEdit, text: &mut String, action: kui::EditAction) {
    use kui::EditAction as A;
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
    use kui::{PointerButton, UiInput};

    /// 一个不带字体的 UI。文字量出来是零尺寸，但布局与交互照常。
    /// 测试用的窗口大小。
    const SCREEN: Vec2 = Vec2::new(800.0, 600.0);

    fn ui() -> Ui {
        let mut ui = Ui::new();
        ui.begin_frame(SCREEN, 1.0);
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

    /// 选中的单选按钮要比没选中的多画一个圆点，否则一组按钮
    /// 看上去全都一样，用户不知道自己选了哪个。
    #[test]
    fn a_selected_radio_draws_more_than_an_unselected_one() {
        let count = |selected: bool| {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            w.begin();
            w.radio("r", "选项", selected);
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();
            ui.draw_list().indices().len()
        };
        assert!(count(true) > count(false));
    }

    /// 一组单选按钮各自独立响应，点第二个不会连带点到第一个。
    #[test]
    fn radios_in_a_group_are_independent() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.radio("a", "甲", true);
        let b = w.radio("b", "乙", false);
        w.finish(&mut ui, &UiInput::default());

        // 点击要按下、松开两帧才算数。
        let target = w.response(b).rect.center();
        for frame in 0..2 {
            let mut input = at(target.x, target.y);
            if frame == 0 {
                input.pressed.push(PointerButton::Primary);
            } else {
                input.released.push(PointerButton::Primary);
            }
            w.begin();
            w.radio("a", "甲", true);
            w.radio("b", "乙", false);
            w.finish(&mut ui, &input);
        }

        assert!(w.response(b).clicked);
        assert!(!w.response(a).clicked);
    }

    /// 选中的列表行要画出底色。不画的话选了等于没选。
    #[test]
    fn a_selected_list_item_draws_a_background() {
        let count = |selected: bool| {
            let mut ui = ui();
            let mut w = WidgetUi::default();
            w.begin();
            w.list_item("i", "一行", selected);
            w.finish(&mut ui, &UiInput::default());
            ui.end_frame();
            ui.draw_list().indices().len()
        };
        assert!(count(true) > count(false));
    }

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

    /// 拖标题栏得到位移增量，调用方拿它挪对话框。
    #[test]
    fn dragging_a_dialog_title_reports_the_delta() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let t = w.dialog_title("t", "设置");
        w.finish(&mut ui, &UiInput::default());

        let start = w.response(t).rect.center();
        let mut input = at(start.x, start.y);
        input.pressed.push(PointerButton::Primary);
        w.begin();
        w.dialog_title("t", "设置");
        w.finish(&mut ui, &input);

        // 按住不放地挪：这一帧既没按下也没松开，按住的状态由内部记着。
        let input = at(start.x + 40.0, start.y + 25.0);
        w.begin();
        w.dialog_title("t", "设置");
        w.finish(&mut ui, &input);

        let drag = w.response(t).drag;
        assert!((drag.x - 40.0).abs() < 0.01, "横向位移是 {}", drag.x);
        assert!((drag.y - 25.0).abs() < 0.01, "纵向位移是 {}", drag.y);
    }

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
            edits: vec![kui::EditAction::End { select: false }],
            ..Default::default()
        };
        w.begin();
        w.text_input("t", &mut text, "", &to_end);
        w.finish(&mut ui, &to_end);

        let backspace = UiInput {
            edits: vec![kui::EditAction::Backspace],
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
            edits: vec![kui::EditAction::End { select: false }],
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

    #[test]
    fn begin_does_not_discard_the_root_style() {
        // 这条记录的是一个真实的 bug：`begin` 曾经把 `root_style` 重置回
        // 默认值，于是「先 root_style 再 begin」这个再自然不过的写法会被
        // **静默丢弃**——面板背景画在右边，控件却排在屏幕最左角。
        let mut w = WidgetUi::default();
        w.root_style(Style {
            margin: Edges {
                left: 400.0,
                top: 50.0,
                right: 0.0,
                bottom: 0.0,
            },
            ..Default::default()
        });
        w.begin();
        let label = w.label("a", "hi");

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let rect = w.response(label).rect;
        assert!(
            rect.min.x >= 400.0,
            "root_style 的左边距没生效，控件在 x={}",
            rect.min.x
        );
        assert!(rect.min.y >= 50.0, "上边距没生效，控件在 y={}", rect.min.y);
    }

    #[test]
    fn a_row_puts_widgets_side_by_side() {
        // lil-gui 那种「名字在左、控件在右」的行。
        let mut w = WidgetUi::default();
        w.begin();
        w.begin_row();
        let name = w.label("name", "time scale");
        let control = w.slider("s", 0.5);
        w.end_row();

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let name_rect = w.response(name).rect;
        let control_rect = w.response(control).rect;

        // 横着排：控件在标签右边，而且两者竖直方向重叠。
        assert!(
            control_rect.min.x >= name_rect.max.x - 1.0,
            "没横着排：标签 {name_rect:?}，控件 {control_rect:?}"
        );
        assert!(
            name_rect.min.y < control_rect.max.y && control_rect.min.y < name_rect.max.y,
            "两者不在同一行"
        );
    }

    #[test]
    fn the_first_widget_in_a_row_takes_the_slack() {
        // 第一个控件吃掉剩余空间，把后面的挤到右边——这是「右对齐」
        // 的实现方式。不这么做的话标签和控件会挤在左边。
        let mut w = WidgetUi::default();
        w.root_style(Style {
            width: Length::Px(400.0),
            padding: Edges::default(),
            ..Default::default()
        });
        w.begin();
        w.begin_row();
        w.label("name", "x");
        let control = w.button("b", "go");
        w.end_row();

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let control_rect = w.response(control).rect;
        assert!(
            control_rect.max.x > 300.0,
            "控件没被挤到右边，右边缘在 x={}",
            control_rect.max.x
        );
    }

    #[test]
    fn widgets_outside_a_row_still_stack_vertically() {
        // 不开行的控件行为不能变。
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.label("a", "one");
        let b = w.label("b", "two");

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let (ra, rb) = (w.response(a).rect, w.response(b).rect);
        assert!(rb.min.y >= ra.max.y - 1.0, "竖排被破坏了：{ra:?} / {rb:?}");
    }

    #[test]
    fn rows_and_loose_widgets_can_be_mixed() {
        let mut w = WidgetUi::default();
        w.begin();
        let title = w.label("title", "Controls");
        w.begin_row();
        let name = w.label("n", "speed");
        w.slider("s", 0.5);
        w.end_row();
        let footer = w.label("f", "end");

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let (t, n, f) = (
            w.response(title).rect,
            w.response(name).rect,
            w.response(footer).rect,
        );
        assert!(n.min.y >= t.max.y - 1.0, "行没排在标题下面");
        assert!(f.min.y >= n.max.y - 1.0, "行尾的控件没排在行下面");
    }

    #[test]
    fn an_unclosed_row_does_not_swallow_later_frames() {
        // 忘了 `end_row` 是很常见的手误。`begin` 必须把它清掉，
        // 否则下一帧所有控件都会挤进那一行。
        let mut w = WidgetUi::default();
        w.begin();
        w.begin_row();
        w.label("a", "x");
        // 故意不调 end_row

        w.begin();
        let a = w.label("a", "one");
        let b = w.label("b", "two");

        let mut ui = ui();
        w.finish(&mut ui, &kui::UiInput::default());

        let (ra, rb) = (w.response(a).rect, w.response(b).rect);
        assert!(rb.min.y >= ra.max.y - 1.0, "上一帧没收的行漏到了这一帧");
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
