//! 布局：把一棵声明式的节点树喂给 taffy，拿回一组绝对矩形。
//!
//! # 为什么不自己写
//!
//! flexbox 的语义细节极多——`flex-basis` 与 `width` 的优先级、
//! `min-content` 的求法、百分比在何时相对何物解析、边距折叠……
//! 而这部分**完全没有引擎特异性**。taffy 有一整套 CSS 一致性测试，
//! 自己写一遍只能得到一个测试更少的版本。
//!
//! # 即时的 API、保留的内部
//!
//! 绘制层是即时模式的，但布局天生要成树：一个节点多宽取决于兄弟和父亲。
//! 所以这里的流程是**先声明、再求解、最后绘制**——
//! 调用方每帧建一棵轻量的 [`LayoutNode`] 树（就是些 `Vec` 和结构体，
//! 不碰 taffy），[`solve`] 里一次性喂给 taffy，拿回绝对坐标的矩形。
//!
//! 每帧重建而不是增量更新：UI 树通常只有几十到几百个节点，重建的代价
//! 远小于「维护一份持久树并保证它和声明同步」的复杂度——后者是
//! 保留模式 UI 里最主要的一类 bug 来源。

use crate::Rect;
use kmath::Vec2;
use taffy::prelude::*;

/// 允许的最大嵌套深度。
///
/// **超过就整棵树不布局**，而不是让它崩掉。
///
/// 这个上限来自 taffy：它的 `compute_layout` 是递归的，而 debug 构建的
/// 栈帧很大——实测 20~25 层就爆栈，表现为**整个进程直接消失**，
/// 没有 panic、没有日志、没有退出码可查。一个界面嵌套太深是显示问题，
/// 不该把游戏带走。
///
/// 24 对真实界面是够的：面板套列表套行套按钮套文字通常在 10 层以内。
/// release 构建能扛的远不止这个数，但上限按最紧的那个定，
/// 免得「debug 能跑 release 崩」或者反过来。
pub const MAX_DEPTH: usize = 24;

/// 主轴方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// 从上到下排。
    #[default]
    Column,
    /// 从左到右排。
    Row,
}

/// 主轴上怎么分配剩余空间。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    /// 挤在起点。
    #[default]
    Start,
    /// 挤在终点。
    End,
    /// 居中。
    Center,
    /// 两端对齐，间隔均分。
    SpaceBetween,
    /// 每个元素两侧的间隔相等。
    SpaceAround,
}

/// 交叉轴上怎么对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignCross {
    /// 拉伸铺满。
    #[default]
    Stretch,
    /// 靠起点。
    Start,
    /// 靠终点。
    End,
    /// 居中。
    Center,
}

/// 一个节点按什么规则排它的孩子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// 弹性盒：孩子沿主轴排成一条。绝大多数界面都是这个。
    #[default]
    Flex,
    /// 网格：孩子按 [`Style::grid_columns`] 折行。
    ///
    /// 和「一堆 `Row` 套在 `Column` 里」的区别是**跨行对齐**：
    /// 网格的第二列在每一行里都是同一个宽度，而一行行手排的话
    /// 每行各按各的内容宽，列会对不齐。物品栏、关卡选择、键位表
    /// 都要的是网格。
    Grid,
    /// 不参与布局，也不占位置，连同整棵子树一起消失。
    ///
    /// 和「画的时候跳过」不是一回事：那样它仍然占着位置，
    /// 后面的东西不会补上来。
    None,
}

/// 网格的一条轨道（一列或一行）有多宽。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Track {
    /// 由这一列里最宽的那个孩子决定。
    #[default]
    Auto,
    /// 固定像素。
    Px(f32),
    /// 按比例分剩余空间。`Fr(1.0)` 各占一份，等宽。
    Fr(f32),
}

impl Track {
    fn to_taffy(self) -> TrackSizingFunction {
        match self {
            Track::Auto => auto(),
            Track::Px(v) => length(v),
            Track::Fr(v) => fr(v),
        }
    }
}

/// 一个长度：固定像素、百分比，或者由内容决定。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Length {
    /// 由内容和 flex 决定。
    #[default]
    Auto,
    /// 固定像素。
    Px(f32),
    /// 相对父容器的百分比，`1.0` 表示 100%。
    Percent(f32),
}

impl Length {
    fn to_dimension(self) -> Dimension {
        match self {
            Length::Auto => auto(),
            Length::Px(v) => length(v),
            Length::Percent(v) => percent(v),
        }
    }
}

/// 四边的内边距或外边距。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    /// 上。
    pub top: f32,
    /// 右。
    pub right: f32,
    /// 下。
    pub bottom: f32,
    /// 左。
    pub left: f32,
}

impl Edges {
    /// 四边相同。
    pub fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    /// 水平与垂直分别指定。
    pub fn axes(horizontal: f32, vertical: f32) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }

    fn to_rect(self) -> taffy::Rect<LengthPercentage> {
        taffy::Rect {
            left: length(self.left),
            right: length(self.right),
            top: length(self.top),
            bottom: length(self.bottom),
        }
    }
}

/// 一个节点的布局样式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// 子节点的排列方向。
    pub direction: Direction,
    /// 主轴分配。
    pub justify: Justify,
    /// 交叉轴对齐。
    pub align: AlignCross,
    /// 宽。
    pub width: Length,
    /// 高。
    pub height: Length,
    /// 最小宽 / 高（像素）。`0` 表示不限。
    pub min_size: Vec2,
    /// 最大宽 / 高（像素）。`0` 表示不限。
    ///
    /// 用 `0` 而不是 `Option<Vec2>` 当「不限」，是为了让 [`Style`] 保持
    /// `Copy` 且能 `..Default::default()` 一路写下来。宽度为 0 的界面
    /// 元素没有意义，所以这个哨兵值不会和真实取值撞上。
    ///
    /// 最大宽和 [`grow`](Self::grow) 配合起来才是常见的那个需求：
    /// 「铺满，但别超过 600 像素」——正文栏宽度、对话框、提示条都是这样。
    pub max_size: Vec2,
    /// 内边距。
    pub padding: Edges,
    /// 外边距。
    pub margin: Edges,
    /// 子节点之间的间隔。
    pub gap: f32,
    /// 主轴上占剩余空间的比例。0 表示不伸展。
    pub grow: f32,
    /// 空间不够时收缩的比例。
    pub shrink: f32,
    /// 脱离父容器的流式布局，**不占位置**。
    ///
    /// 浮层要的就是这个：菜单弹出来时不该把它下面的控件往下顶。
    /// 节点仍然会被排（拿得到自己的尺寸），只是兄弟们当它不存在。
    ///
    /// 摆到哪里由调用方在求解**之后**平移——CSS 那套 `top`/`left`
    /// 在这里没有意义，因为浮层的目标位置要先知道锚点排在哪，
    /// 而那要等这一轮求解出来。
    pub absolute: bool,
    /// 这个节点按什么规则排它的孩子。
    pub display: Display,
    /// [`Display::Grid`] 时有几列，以及每列多宽。
    ///
    /// 只用前 `grid_column_count` 条。列数为 0 时退化成一列。
    ///
    /// # 为什么是定长数组而不是 `Vec`
    ///
    /// [`Style`] 是 `Copy` 的——它在整个布局层里按值传来传去，
    /// 每帧每个节点一份。为了网格把它改成 `Clone` 会让所有调用点
    /// 多一次分配，而**游戏界面里超过 12 列的网格基本不存在**
    /// （物品栏 8~10 列，键位表 3 列）。真要更多就嵌一层。
    pub grid_columns: [Track; MAX_GRID_COLUMNS],
    /// 上面那个数组里有几条是有效的。
    pub grid_column_count: u8,
    /// 这个**孩子**横跨几列。`0` 和 `1` 都表示一列。
    ///
    /// 网格里的小标题、分隔线要它：一条占满整行的标题，
    /// 而不是被挤在第一列。
    pub grid_span: u8,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            direction: Direction::Column,
            justify: Justify::Start,
            align: AlignCross::Stretch,
            width: Length::Auto,
            height: Length::Auto,
            min_size: Vec2::ZERO,
            max_size: Vec2::ZERO,
            padding: Edges::default(),
            margin: Edges::default(),
            gap: 0.0,
            grow: 0.0,
            // flexbox 的默认收缩比是 1：空间不够时元素会缩，而不是溢出。
            shrink: 1.0,
            absolute: false,
            display: Display::Flex,
            grid_columns: [Track::Auto; MAX_GRID_COLUMNS],
            grid_column_count: 0,
            grid_span: 1,
        }
    }
}

/// 一个网格最多能有几列。理由见 [`Style::grid_columns`]。
pub const MAX_GRID_COLUMNS: usize = 12;

impl Style {
    /// 把这个节点变成 `columns` 列等宽的网格。
    ///
    /// 最常用的那种：物品栏、关卡选择、颜色板。每列 `Fr(1.0)`，
    /// 行高由内容决定。
    ///
    /// ```
    /// # use kui::{Style, Display};
    /// let grid = Style::default().with_uniform_grid(4);
    /// assert_eq!(grid.display, Display::Grid);
    /// ```
    ///
    /// 超过 [`MAX_GRID_COLUMNS`] 的部分会被丢掉——静默截断比 panic 好，
    /// 但会记一条日志，因为「列数不对」在画面上表现为整个网格错位。
    pub fn with_uniform_grid(self, columns: usize) -> Self {
        self.with_grid(&[Track::Fr(1.0)].repeat(columns.max(1)))
    }

    /// 把这个节点变成网格，逐列指定宽度。
    ///
    /// 键位表那种「名字一列自适应、按键一列固定宽」用它。
    pub fn with_grid(mut self, columns: &[Track]) -> Self {
        if columns.len() > MAX_GRID_COLUMNS {
            klog::warn!(
                "网格要 {} 列，但最多只支持 {MAX_GRID_COLUMNS} 列，多的被丢掉了",
                columns.len()
            );
        }
        self.display = Display::Grid;
        self.grid_column_count = columns.len().min(MAX_GRID_COLUMNS) as u8;
        for (slot, track) in self.grid_columns.iter_mut().zip(columns) {
            *slot = *track;
        }
        self
    }

    /// 让这个**孩子**横跨几列。网格里的标题行用它。
    pub fn spanning(mut self, columns: usize) -> Self {
        self.grid_span = columns.clamp(1, MAX_GRID_COLUMNS) as u8;
        self
    }

    /// 限制最大宽度。「铺满，但别超过这么宽」。
    pub fn with_max_width(mut self, width: f32) -> Self {
        self.max_size.x = width;
        self
    }

    /// 限制最大高度。
    pub fn with_max_height(mut self, height: f32) -> Self {
        self.max_size.y = height;
        self
    }
}

impl Style {
    fn to_taffy(self) -> taffy::Style {
        // 网格的列。`grid_column_count` 为 0 时留空，taffy 会退化成一列。
        let template: Vec<GridTemplateComponent<String>> = self.grid_columns
            [..self.grid_column_count as usize]
            .iter()
            .map(|track| GridTemplateComponent::Single(track.to_taffy()))
            .collect();

        taffy::Style {
            display: match self.display {
                Display::Flex => taffy::Display::Flex,
                Display::Grid => taffy::Display::Grid,
                Display::None => taffy::Display::None,
            },
            grid_template_columns: template,
            // 行数不指定：孩子放不下就自动开新行，行高由内容决定。
            // 这正是「一格一格往里塞」想要的行为——固定行数反而会让
            // 多出来的物品消失。
            grid_column: if self.grid_span > 1 {
                // CSS 的 `grid-column: span N` 展开成
                // `grid-column-start: span N; grid-column-end: auto`。
                // 写反成 `start: auto, end: span N` 的话 taffy 不认，
                // 表现为跨列静默失效——标题挤在第一列里。
                Line {
                    start: span(self.grid_span as u16),
                    end: GridPlacement::Auto,
                }
            } else {
                Line::AUTO
            },
            flex_direction: match self.direction {
                Direction::Column => FlexDirection::Column,
                Direction::Row => FlexDirection::Row,
            },
            justify_content: Some(match self.justify {
                Justify::Start => JustifyContent::FLEX_START,
                Justify::End => JustifyContent::FLEX_END,
                Justify::Center => JustifyContent::CENTER,
                Justify::SpaceBetween => JustifyContent::SPACE_BETWEEN,
                Justify::SpaceAround => JustifyContent::SPACE_AROUND,
            }),
            align_items: Some(match self.align {
                AlignCross::Stretch => AlignItems::STRETCH,
                AlignCross::Start => AlignItems::FLEX_START,
                AlignCross::End => AlignItems::FLEX_END,
                AlignCross::Center => AlignItems::CENTER,
            }),
            size: Size {
                width: self.width.to_dimension(),
                height: self.height.to_dimension(),
            },
            min_size: Size {
                width: if self.min_size.x > 0.0 {
                    length(self.min_size.x)
                } else {
                    auto()
                },
                height: if self.min_size.y > 0.0 {
                    length(self.min_size.y)
                } else {
                    auto()
                },
            },
            max_size: Size {
                width: if self.max_size.x > 0.0 {
                    length(self.max_size.x)
                } else {
                    auto()
                },
                height: if self.max_size.y > 0.0 {
                    length(self.max_size.y)
                } else {
                    auto()
                },
            },
            padding: self.padding.to_rect(),
            margin: taffy::Rect {
                left: length(self.margin.left),
                right: length(self.margin.right),
                top: length(self.margin.top),
                bottom: length(self.margin.bottom),
            },
            gap: Size {
                width: length(self.gap),
                height: length(self.gap),
            },
            flex_grow: self.grow,
            flex_shrink: self.shrink,
            position: if self.absolute {
                Position::Absolute
            } else {
                Position::Relative
            },
            ..Default::default()
        }
    }
}

/// 节点的标识。用来在求解之后找回自己的矩形。
///
/// 用调用方给的字符串哈希而不是自动编号：自动编号会随着树结构变化而
/// 整体错位——插入一个节点，后面所有节点的编号都变了，跨帧的状态
/// （焦点、拖动、滚动位置）会跟着跑到别人身上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(pub u64);

impl Id {
    /// 由任意字符串生成。
    pub fn new(name: &str) -> Self {
        // FNV-1a。要的是稳定和快，不是抗碰撞。
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        Self(hash)
    }

    /// 在父 id 的基础上再套一层。列表里的重复控件用它区分。
    pub fn child(self, name: &str) -> Self {
        let mut hash = self.0;
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        Self(hash)
    }

    /// 由下标生成子 id。
    pub fn index(self, index: usize) -> Self {
        self.child(&index.to_string())
    }
}

/// 一棵待求解的节点树。
#[derive(Debug, Clone)]
pub struct LayoutNode {
    /// 标识。求解之后靠它取矩形。
    pub id: Id,
    /// 布局样式。
    pub style: Style,
    /// 子节点。
    pub children: Vec<LayoutNode>,
    /// 内容的固有尺寸（例如一段文字排完之后有多大）。
    ///
    /// 有值时这个节点被当作叶子的**测量结果**：宽高为 `Auto` 时用它。
    /// 文字节点必须给，否则 flexbox 会认为它零尺寸，整行塌陷。
    pub content_size: Option<Vec2>,
}

impl LayoutNode {
    /// 一个容器。
    pub fn new(id: Id, style: Style) -> Self {
        Self {
            id,
            style,
            children: Vec::new(),
            content_size: None,
        }
    }

    /// 一个有固有尺寸的叶子（文字、图标）。
    pub fn leaf(id: Id, style: Style, content: Vec2) -> Self {
        Self {
            id,
            style,
            children: Vec::new(),
            content_size: Some(content),
        }
    }

    /// 追加一个子节点。
    pub fn with_child(mut self, child: LayoutNode) -> Self {
        self.children.push(child);
        self
    }

    /// 追加一组子节点。
    pub fn with_children(mut self, children: impl IntoIterator<Item = LayoutNode>) -> Self {
        self.children.extend(children);
        self
    }

    /// 这棵树里一共多少个节点。
    pub fn count(&self) -> usize {
        let mut count = 0;
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            count += 1;
            stack.extend(node.children.iter());
        }
        count
    }

    /// 树高，根算 1 层。
    ///
    /// 迭代实现：递归版本在深树上会先于 taffy 爆栈，那样连
    /// 「深度超限」这个错误都报不出来。
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = vec![(self, 1usize)];
        while let Some((node, depth)) = stack.pop() {
            deepest = deepest.max(depth);
            // 已经超了就没必要继续走——深树本身可能非常大。
            if deepest > MAX_DEPTH {
                return deepest;
            }
            stack.extend(node.children.iter().map(|c| (c, depth + 1)));
        }
        deepest
    }
}

/// 求解结果：每个节点的绝对矩形。
#[derive(Debug, Clone, Default)]
pub struct Solved {
    /// 按**深度优先前序**排列，与绘制顺序一致。
    entries: Vec<(Id, Rect)>,
}

impl Solved {
    /// 查一个节点的矩形。
    ///
    /// id 重复时返回第一个。重复 id 是调用方的错——跨帧状态会串，
    /// [`Solved::duplicate_ids`] 可以查出来。
    pub fn rect(&self, id: Id) -> Option<Rect> {
        self.entries.iter().find(|(i, _)| *i == id).map(|(_, r)| *r)
    }

    /// 按前序遍历所有节点。
    pub fn iter(&self) -> impl Iterator<Item = (Id, Rect)> + '_ {
        self.entries.iter().copied()
    }

    /// 节点数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空树。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 找出重复的 id。
    ///
    /// 重复 id 会让焦点、拖动、滚动位置串到别的控件上，而且症状很怪
    /// （「点 A 却是 B 亮了」）。调试时值得查一下。
    pub fn duplicate_ids(&self) -> Vec<Id> {
        let mut seen: Vec<Id> = self.entries.iter().map(|(i, _)| *i).collect();
        seen.sort_unstable();
        let mut out = Vec::new();
        for pair in seen.windows(2) {
            if pair[0] == pair[1] && out.last() != Some(&pair[0]) {
                out.push(pair[0]);
            }
        }
        out
    }

    /// 最上面的命中者。
    ///
    /// 从**后往前**找：前序遍历里后面的节点画在上面，所以后面的先命中。
    /// 从前往后找的话，点在按钮上会命中它底下的面板。
    pub fn hit(&self, point: Vec2) -> Option<Id> {
        self.entries
            .iter()
            .rev()
            .find(|(_, r)| r.contains(point))
            .map(|(i, _)| *i)
    }
}

/// 求解一棵树。`available` 是根节点可用的空间（通常是屏幕尺寸）。
///
/// 出错时返回空结果而不是 panic：布局算不出来是个显示问题，
/// 不该把整个游戏带崩。两种出错：
///
/// - 嵌套超过 [`MAX_DEPTH`]（见那里的说明）；
/// - taffy 自己报错。
pub fn solve(root: &LayoutNode, available: Vec2) -> Solved {
    if root.depth() > MAX_DEPTH {
        return Solved::default();
    }
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(taffy_root) = build(&mut taffy, root) else {
        return Solved::default();
    };

    let space = Size {
        width: AvailableSpace::Definite(available.x),
        height: AvailableSpace::Definite(available.y),
    };
    if taffy.compute_layout(taffy_root, space).is_err() {
        return Solved::default();
    }

    let mut solved = Solved::default();
    collect(&taffy, taffy_root, root, &mut solved);

    // 根节点的 margin 要自己补上。
    //
    // taffy 只在**父容器**排列子节点时用 margin，而根节点没有父容器，
    // 它的 location 恒为 (0,0)——于是「用 margin 把整个面板挪到屏幕右侧」
    // 这个再自然不过的用法会静默失效，面板画在右边、控件排在左上角。
    //
    // 补在这里而不是外面包一层容器：包容器会多一个节点，而那个节点
    // 又要有自己的 id 和样式，`Solved` 里凭空多一项。
    let offset = Vec2::new(root.style.margin.left, root.style.margin.top);
    if offset != Vec2::ZERO {
        for (_, rect) in &mut solved.entries {
            rect.min += offset;
            rect.max += offset;
        }
    }
    solved
}

/// 把声明树搬进 taffy。
///
/// **迭代而不是递归。** 递归版本在 debug 构建下**二十来层就爆栈**——
/// 而嵌套的面板套列表套行很容易到这个深度，症状是整个进程直接消失，
/// 没有 panic、没有日志。这里用显式栈：先把整棵树按前序压平，
/// 再倒着建（保证建父节点时子节点已经就位）。
fn build(taffy: &mut TaffyTree<()>, root: &LayoutNode) -> Result<NodeId, taffy::TaffyError> {
    // 前序压平：`flat[i]` 的子节点在 `flat` 里的下标记在 `child_slots` 里。
    let mut flat: Vec<&LayoutNode> = Vec::new();
    let mut child_slots: Vec<Vec<usize>> = Vec::new();
    let mut stack = vec![(root, usize::MAX)];

    while let Some((node, parent)) = stack.pop() {
        let index = flat.len();
        flat.push(node);
        child_slots.push(Vec::new());
        if parent != usize::MAX {
            child_slots[parent].push(index);
        }
        // 倒着压，弹出来才是原顺序——顺序错了整个界面的子元素会反向排列。
        for child in node.children.iter().rev() {
            stack.push((child, index));
        }
    }

    // 倒着建：下标大的一定是下标小的后代，所以建到父节点时子节点已经有了。
    let mut ids: Vec<Option<NodeId>> = vec![None; flat.len()];
    for index in (0..flat.len()).rev() {
        let node = flat[index];
        let style = taffy_style_of(node);
        let id = if child_slots[index].is_empty() {
            taffy.new_leaf(style)?
        } else {
            let children: Vec<NodeId> = child_slots[index]
                .iter()
                .map(|c| ids[*c].expect("子节点先于父节点建好"))
                .collect();
            taffy.new_with_children(style, &children)?
        };
        ids[index] = Some(id);
    }

    Ok(ids[0].expect("根节点一定建了"))
}

/// 节点的 taffy 样式，含固有尺寸的处理。
fn taffy_style_of(node: &LayoutNode) -> taffy::Style {
    let mut style = node.style.to_taffy();
    // 叶子的固有尺寸：宽高是 Auto 时用测量值顶上。
    //
    // 不给的话 flexbox 会认为这个节点零尺寸——一段文字所在的行会整个塌陷，
    // 而且不报错，只是界面上什么都看不见。
    if let Some(content) = node.content_size {
        if node.style.width == Length::Auto {
            style.size.width = length(content.x);
        }
        if node.style.height == Length::Auto {
            style.size.height = length(content.y);
        }
    }
    style
}

/// 把 taffy 的相对坐标累加成绝对坐标。同样是迭代的。
fn collect(taffy: &TaffyTree<()>, taffy_root: NodeId, root: &LayoutNode, out: &mut Solved) {
    // 前序遍历，用显式栈。压栈时倒序，弹出来才是原顺序。
    let mut stack = vec![(taffy_root, root, Vec2::ZERO)];
    while let Some((taffy_node, node, origin)) = stack.pop() {
        let Ok(layout) = taffy.layout(taffy_node) else {
            continue;
        };
        // taffy 给的 location 是**相对父节点**的。不累加的话所有节点都会
        // 挤在各自父容器的左上角——嵌套一深就全叠在一起了。
        let min = origin + Vec2::new(layout.location.x, layout.location.y);
        out.entries.push((
            node.id,
            Rect {
                min,
                max: min + Vec2::new(layout.size.width, layout.size.height),
            },
        ));

        let Ok(children) = taffy.children(taffy_node) else {
            continue;
        };
        for (taffy_child, child) in children.iter().zip(&node.children).rev() {
            stack.push((*taffy_child, child, min));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> Id {
        Id::new(name)
    }

    fn row(name: &str) -> LayoutNode {
        LayoutNode::new(
            id(name),
            Style {
                direction: Direction::Row,
                ..Default::default()
            },
        )
    }

    fn fixed(name: &str, w: f32, h: f32) -> LayoutNode {
        LayoutNode::new(
            id(name),
            Style {
                width: Length::Px(w),
                height: Length::Px(h),
                ..Default::default()
            },
        )
    }

    /// 绝对定位的节点不占流里的位置。
    ///
    /// 浮层要的就是这个：菜单弹出来时不该把它下面的控件往下顶。
    #[test]
    fn an_absolute_node_does_not_push_its_siblings() {
        let float = LayoutNode::new(
            id("float"),
            Style {
                width: Length::Px(50.0),
                height: Length::Px(50.0),
                absolute: true,
                ..Default::default()
            },
        );
        let root = LayoutNode::new(id("root"), Style::default()).with_children([
            fixed("a", 30.0, 20.0),
            float,
            fixed("b", 30.0, 20.0),
        ]);
        let solved = solve(&root, Vec2::new(200.0, 200.0));

        let a = solved.rect(id("a")).unwrap();
        let b = solved.rect(id("b")).unwrap();
        assert_eq!(b.min.y, a.max.y, "浮层把兄弟顶开了：a={a:?} b={b:?}");
        // 浮层自己仍然被排出了尺寸——不然没法拿它去算摆在哪。
        assert_eq!(
            solved.rect(id("float")).unwrap().size(),
            Vec2::new(50.0, 50.0)
        );
    }

    #[test]
    fn a_single_node_fills_what_it_is_told_to() {
        let root = LayoutNode::new(
            id("root"),
            Style {
                width: Length::Px(200.0),
                height: Length::Px(100.0),
                ..Default::default()
            },
        );
        let solved = solve(&root, Vec2::new(800.0, 600.0));

        assert_eq!(
            solved.rect(id("root")).unwrap().size(),
            Vec2::new(200.0, 100.0)
        );
    }

    #[test]
    fn a_column_stacks_children_downward() {
        let root = LayoutNode::new(id("root"), Style::default())
            .with_child(fixed("a", 50.0, 20.0))
            .with_child(fixed("b", 50.0, 30.0));
        let solved = solve(&root, Vec2::new(800.0, 600.0));

        let a = solved.rect(id("a")).unwrap();
        let b = solved.rect(id("b")).unwrap();
        assert_eq!(a.min.y, 0.0);
        assert_eq!(b.min.y, 20.0, "第二个该接在第一个下面");
        assert_eq!(a.min.x, b.min.x);
    }

    #[test]
    fn a_row_stacks_children_rightward() {
        let root = row("root")
            .with_child(fixed("a", 50.0, 20.0))
            .with_child(fixed("b", 30.0, 20.0));
        let solved = solve(&root, Vec2::new(800.0, 600.0));

        assert_eq!(solved.rect(id("a")).unwrap().min.x, 0.0);
        assert_eq!(solved.rect(id("b")).unwrap().min.x, 50.0);
    }

    #[test]
    fn gap_separates_siblings() {
        let root = LayoutNode::new(
            id("root"),
            Style {
                gap: 8.0,
                ..Default::default()
            },
        )
        .with_child(fixed("a", 50.0, 20.0))
        .with_child(fixed("b", 50.0, 20.0));
        let solved = solve(&root, Vec2::new(800.0, 600.0));

        assert_eq!(solved.rect(id("b")).unwrap().min.y, 28.0);
    }

    #[test]
    fn padding_insets_the_children() {
        let root = LayoutNode::new(
            id("root"),
            Style {
                padding: Edges::all(10.0),
                ..Default::default()
            },
        )
        .with_child(fixed("a", 50.0, 20.0));
        let solved = solve(&root, Vec2::new(800.0, 600.0));

        assert_eq!(solved.rect(id("a")).unwrap().min, Vec2::new(10.0, 10.0));
    }

    #[test]
    fn nested_positions_are_absolute() {
        // taffy 给的是相对父节点的坐标。不累加的话所有节点都挤在
        // 各自父容器的左上角，嵌套一深就全叠在一起了。
        let inner = LayoutNode::new(
            id("inner"),
            Style {
                padding: Edges::all(5.0),
                ..Default::default()
            },
        )
        .with_child(fixed("leaf", 10.0, 10.0));
        let root = LayoutNode::new(
            id("root"),
            Style {
                padding: Edges::all(20.0),
                ..Default::default()
            },
        )
        .with_child(inner);

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(
            solved.rect(id("leaf")).unwrap().min,
            Vec2::new(25.0, 25.0),
            "20 的外层内边距加 5 的内层内边距"
        );
    }

    #[test]
    fn grow_splits_the_leftover_space() {
        let root = row("root")
            .with_child(LayoutNode::new(
                id("a"),
                Style {
                    grow: 1.0,
                    ..Default::default()
                },
            ))
            .with_child(LayoutNode::new(
                id("b"),
                Style {
                    grow: 3.0,
                    ..Default::default()
                },
            ));
        // 根节点要有确定的宽度，剩余空间才有意义。
        let root = LayoutNode::new(
            id("outer"),
            Style {
                direction: Direction::Row,
                width: Length::Px(400.0),
                ..Default::default()
            },
        )
        .with_children(root.children);

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.rect(id("a")).unwrap().size().x, 100.0);
        assert_eq!(solved.rect(id("b")).unwrap().size().x, 300.0);
    }

    #[test]
    fn percent_is_relative_to_the_parent() {
        let root = LayoutNode::new(
            id("root"),
            Style {
                width: Length::Px(400.0),
                height: Length::Px(200.0),
                ..Default::default()
            },
        )
        .with_child(LayoutNode::new(
            id("half"),
            Style {
                width: Length::Percent(0.5),
                height: Length::Px(10.0),
                ..Default::default()
            },
        ));

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.rect(id("half")).unwrap().size().x, 200.0);
    }

    #[test]
    fn content_size_keeps_a_text_leaf_from_collapsing() {
        // 文字节点不给固有尺寸的话，flexbox 认为它零尺寸——
        // 整行塌陷，界面上什么都看不见，而且不报错。
        let root = LayoutNode::new(id("root"), Style::default()).with_child(LayoutNode::leaf(
            id("text"),
            Style::default(),
            Vec2::new(120.0, 18.0),
        ));
        let solved = solve(&root, Vec2::new(800.0, 600.0));

        assert_eq!(
            solved.rect(id("text")).unwrap().size(),
            Vec2::new(120.0, 18.0)
        );
    }

    #[test]
    fn an_explicit_size_overrides_the_content_size() {
        let root = LayoutNode::new(id("root"), Style::default()).with_child(LayoutNode::leaf(
            id("text"),
            Style {
                width: Length::Px(50.0),
                ..Default::default()
            },
            Vec2::new(120.0, 18.0),
        ));
        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.rect(id("text")).unwrap().size().x, 50.0);
    }

    #[test]
    fn center_justification_centers_on_the_main_axis() {
        let root = LayoutNode::new(
            id("root"),
            Style {
                direction: Direction::Row,
                justify: Justify::Center,
                width: Length::Px(200.0),
                ..Default::default()
            },
        )
        .with_child(fixed("a", 40.0, 10.0));

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.rect(id("a")).unwrap().min.x, 80.0);
    }

    #[test]
    fn hit_testing_picks_the_topmost() {
        // 前序遍历里后面的画在上面。从前往后找的话，
        // 点在按钮上会命中它底下的面板。
        let root = LayoutNode::new(
            id("panel"),
            Style {
                width: Length::Px(200.0),
                height: Length::Px(100.0),
                padding: Edges::all(10.0),
                ..Default::default()
            },
        )
        .with_child(fixed("button", 80.0, 30.0));

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.hit(Vec2::new(20.0, 20.0)), Some(id("button")));
        // 按钮之外、面板之内。
        assert_eq!(solved.hit(Vec2::new(150.0, 80.0)), Some(id("panel")));
        // 面板之外。
        assert_eq!(solved.hit(Vec2::new(500.0, 500.0)), None);
    }

    #[test]
    fn the_traversal_order_is_preorder() {
        // 绘制顺序要和这个一致：父容器先画，子控件叠在上面。
        let root = LayoutNode::new(id("root"), Style::default())
            .with_child(
                LayoutNode::new(id("a"), Style::default()).with_child(fixed("a1", 5.0, 5.0)),
            )
            .with_child(fixed("b", 5.0, 5.0));

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        let order: Vec<Id> = solved.iter().map(|(i, _)| i).collect();
        assert_eq!(order, vec![id("root"), id("a"), id("a1"), id("b")]);
    }

    #[test]
    fn duplicate_ids_are_detectable() {
        // 重复 id 会让焦点、拖动、滚动位置串到别的控件上，
        // 症状是「点 A 却是 B 亮了」，很难查。
        let root = LayoutNode::new(id("root"), Style::default())
            .with_child(fixed("same", 5.0, 5.0))
            .with_child(fixed("same", 5.0, 5.0));

        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.duplicate_ids(), vec![id("same")]);
    }

    #[test]
    fn child_ids_disambiguate_repeated_widgets() {
        let list = Id::new("list");
        assert_ne!(list.index(0), list.index(1));
        assert_ne!(Id::new("a"), Id::new("b"));
        // 同样的名字在同样的父下必须稳定，否则跨帧状态每帧都丢。
        assert_eq!(Id::new("list").index(3), list.index(3));
    }

    #[test]
    fn an_empty_tree_solves_to_one_node() {
        let solved = solve(
            &LayoutNode::new(id("only"), Style::default()),
            Vec2::new(100.0, 100.0),
        );
        assert_eq!(solved.len(), 1);
    }

    /// 造一棵 `depth` 层深的链。
    fn chain(depth: usize) -> LayoutNode {
        let mut node = fixed("leaf", 10.0, 10.0);
        for i in 0..depth.saturating_sub(1) {
            node = LayoutNode::new(Id::new(&format!("n{i}")), Style::default()).with_child(node);
        }
        node
    }

    #[test]
    fn realistic_nesting_works() {
        // 真实界面：面板套列表套行套按钮套文字，十来层顶天了。
        let root = chain(16);
        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert_eq!(solved.len(), 16);
        assert!(solved.rect(id("leaf")).is_some());
    }

    #[test]
    fn nesting_past_the_limit_fails_instead_of_crashing() {
        // taffy 的 compute_layout 是递归的，debug 构建下 20~25 层就爆栈，
        // 表现为**整个进程直接消失**——没有 panic、没有日志。
        // 上限拦住之后至少是「界面不显示」，还能查。
        let root = chain(MAX_DEPTH + 10);
        let solved = solve(&root, Vec2::new(800.0, 600.0));
        assert!(solved.is_empty(), "超深的树该被拒绝，而不是拿去算");
    }

    #[test]
    fn depth_is_measured_without_recursing() {
        // 深度检查本身要是递归的，就会先于 taffy 爆栈——
        // 那样连「深度超限」这个错误都报不出来。
        let root = chain(5_000);
        assert!(root.depth() > MAX_DEPTH);
    }

    #[test]
    fn count_is_iterative_too() {
        let root = chain(5_000);
        assert_eq!(root.count(), 5_000);
    }

    // ── 网格 ──

    /// 建一个 `columns` 列的网格，里面塞 `count` 个 40×20 的格子。
    fn grid_of(columns: usize, count: usize, available: Vec2) -> Solved {
        let cell = Style {
            width: Length::Px(40.0),
            height: Length::Px(20.0),
            ..Default::default()
        };
        // 网格自己必须有确定宽度。宽度是 `Auto` 的话它会缩到内容宽，
        // `Fr` 就没有剩余空间可分——列会挤成一团，而这不是网格的错。
        let mut root = LayoutNode::new(
            Id::new("grid"),
            Style {
                width: Length::Percent(1.0),
                ..Style::default().with_uniform_grid(columns)
            },
        );
        for index in 0..count {
            root.children
                .push(LayoutNode::new(Id::new("grid").index(index), cell));
        }
        solve(&root, available)
    }

    #[test]
    fn a_grid_wraps_after_the_column_count() {
        // 四列六格：第 4 格该换到第二行，而且和第 0 格左对齐。
        let solved = grid_of(4, 6, Vec2::new(400.0, 400.0));
        let at = |i: usize| solved.rect(Id::new("grid").index(i)).unwrap();

        assert_eq!(at(0).min.y, at(3).min.y, "前四个该在同一行");
        assert!(at(4).min.y > at(0).min.y, "第五个该换行");
        assert_eq!(at(4).min.x, at(0).min.x, "换行之后该回到第一列");
    }

    #[test]
    fn grid_columns_line_up_across_rows() {
        // 这是网格相对「一行行手排」的**唯一**理由：跨行对齐。
        // 手排的话每行各按各的内容宽，第二列会参差不齐。
        let solved = grid_of(3, 6, Vec2::new(300.0, 400.0));
        let at = |i: usize| solved.rect(Id::new("grid").index(i)).unwrap();

        assert_eq!(at(1).min.x, at(4).min.x, "第二列在两行里该对齐");
        assert_eq!(at(2).min.x, at(5).min.x, "第三列在两行里该对齐");
    }

    #[test]
    fn uniform_grid_columns_are_equally_wide() {
        // `Fr(1.0)` 各占一份。列宽不等的话物品栏会歪。
        let mut root = LayoutNode::new(
            Id::new("g"),
            Style {
                width: Length::Percent(1.0),
                ..Style::default().with_uniform_grid(4)
            },
        );
        for index in 0..4 {
            root.children.push(LayoutNode::new(
                Id::new("g").index(index),
                Style {
                    height: Length::Px(10.0),
                    ..Default::default()
                },
            ));
        }
        let solved = solve(&root, Vec2::new(400.0, 100.0));
        let width = |i: usize| solved.rect(Id::new("g").index(i)).unwrap().size().x;

        for index in 1..4 {
            assert!(
                (width(index) - width(0)).abs() < 0.01,
                "第 {index} 列宽 {} ≠ 第 0 列宽 {}",
                width(index),
                width(0)
            );
        }
    }

    #[test]
    fn a_spanning_child_takes_several_columns() {
        // 网格里的小标题要占满一整行，而不是被挤在第一列。
        let mut root = LayoutNode::new(
            Id::new("g"),
            Style {
                width: Length::Percent(1.0),
                ..Style::default().with_uniform_grid(3)
            },
        );
        root.children.push(LayoutNode::new(
            Id::new("title"),
            Style {
                height: Length::Px(20.0),
                ..Default::default()
            }
            .spanning(3),
        ));
        root.children.push(LayoutNode::new(
            Id::new("cell"),
            Style {
                height: Length::Px(20.0),
                ..Default::default()
            },
        ));

        let solved = solve(&root, Vec2::new(300.0, 200.0));
        let title = solved.rect(Id::new("title")).unwrap();
        let cell = solved.rect(Id::new("cell")).unwrap();

        assert!(title.size().x > cell.size().x * 2.5, "标题没有跨列");
        assert!(cell.min.y > title.min.y, "格子该排在标题下面一行");
    }

    #[test]
    fn a_grid_can_give_each_column_its_own_width() {
        // 键位表：名字一列自适应，按键一列固定宽。
        let mut root = LayoutNode::new(
            Id::new("g"),
            Style {
                width: Length::Percent(1.0),
                ..Style::default().with_grid(&[Track::Fr(1.0), Track::Px(80.0)])
            },
        );
        for name in ["a", "b"] {
            root.children.push(LayoutNode::new(
                Id::new(name),
                Style {
                    height: Length::Px(20.0),
                    ..Default::default()
                },
            ));
        }

        let solved = solve(&root, Vec2::new(300.0, 100.0));
        assert!((solved.rect(Id::new("b")).unwrap().size().x - 80.0).abs() < 0.01);
        assert!(solved.rect(Id::new("a")).unwrap().size().x > 150.0);
    }

    #[test]
    fn too_many_columns_are_truncated_instead_of_panicking() {
        // 静默截断比 panic 好，但列数不对在画面上表现为整个网格错位，
        // 所以那条路径会记一条日志。
        let style = Style::default().with_uniform_grid(MAX_GRID_COLUMNS + 5);
        assert_eq!(style.grid_column_count as usize, MAX_GRID_COLUMNS);
    }

    #[test]
    fn zero_columns_degrade_to_one() {
        // 计算出来的列数可能是 0（比如「可用宽度 / 格子宽度」向下取整）。
        // 那时排成一列比整个网格消失好。
        assert_eq!(Style::default().with_uniform_grid(0).grid_column_count, 1);
    }

    // ── display: none ──

    #[test]
    fn a_hidden_node_takes_no_space() {
        // 和「画的时候跳过」不是一回事：那样它仍然占着位置。
        let row = |hidden: bool| {
            let mut root = LayoutNode::new(
                Id::new("row"),
                Style {
                    direction: Direction::Row,
                    ..Default::default()
                },
            );
            root.children.push(LayoutNode::new(
                Id::new("a"),
                Style {
                    width: Length::Px(50.0),
                    height: Length::Px(10.0),
                    display: if hidden { Display::None } else { Display::Flex },
                    ..Default::default()
                },
            ));
            root.children.push(LayoutNode::new(
                Id::new("b"),
                Style {
                    width: Length::Px(50.0),
                    height: Length::Px(10.0),
                    ..Default::default()
                },
            ));
            solve(&root, Vec2::new(400.0, 100.0))
                .rect(Id::new("b"))
                .unwrap()
                .min
                .x
        };

        assert!(row(true) < row(false), "藏起来之后后面的该补上来");
        assert_eq!(row(true), 0.0);
    }

    // ── max_size ──

    #[test]
    fn max_width_caps_a_growing_child() {
        // 「铺满，但别超过这么宽」——正文栏、对话框、提示条都是这样。
        let solve_with = |max: f32| {
            let mut root = LayoutNode::new(
                Id::new("root"),
                Style {
                    width: Length::Percent(1.0),
                    ..Default::default()
                },
            );
            root.children.push(LayoutNode::new(
                Id::new("body"),
                Style {
                    width: Length::Percent(1.0),
                    height: Length::Px(10.0),
                    max_size: Vec2::new(max, 0.0),
                    ..Default::default()
                },
            ));
            solve(&root, Vec2::new(1000.0, 100.0))
                .rect(Id::new("body"))
                .unwrap()
                .size()
                .x
        };

        assert!((solve_with(0.0) - 1000.0).abs() < 0.01, "0 该表示不限");
        assert!((solve_with(600.0) - 600.0).abs() < 0.01);
    }

    #[test]
    fn max_height_caps_a_tall_child() {
        let mut root = LayoutNode::new(Id::new("root"), Style::default());
        root.children.push(LayoutNode::new(
            Id::new("tall"),
            Style {
                height: Length::Px(500.0),
                max_size: Vec2::new(0.0, 120.0),
                ..Default::default()
            },
        ));

        let height = solve(&root, Vec2::new(400.0, 1000.0))
            .rect(Id::new("tall"))
            .unwrap()
            .size()
            .y;
        assert!((height - 120.0).abs() < 0.01);
    }

    #[test]
    fn min_size_still_wins_over_max_size() {
        // CSS 的规矩：min 压过 max。两个都设且冲突时，结果是 min——
        // 这一条不写测试的话很容易在改动里被弄反。
        let mut root = LayoutNode::new(Id::new("root"), Style::default());
        root.children.push(LayoutNode::new(
            Id::new("x"),
            Style {
                width: Length::Px(10.0),
                height: Length::Px(10.0),
                min_size: Vec2::new(200.0, 0.0),
                max_size: Vec2::new(50.0, 0.0),
                ..Default::default()
            },
        ));

        let width = solve(&root, Vec2::new(400.0, 100.0))
            .rect(Id::new("x"))
            .unwrap()
            .size()
            .x;
        assert!((width - 200.0).abs() < 0.01, "min 该压过 max，实际 {width}");
    }
}
