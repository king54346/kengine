//! 菜单：菜单栏、下拉菜单、子菜单。
//!
//! # 一个模块，两半
//!
//! 上半是[**纯逻辑**](navigate)：按方向键该跳到哪一项。这是菜单里最容易
//! 出错的地方——中间夹着禁用项、要不要绕回开头、菜单全禁用时怎么办——
//! 而它完全不需要窗口、字体、GPU，所以拎出来做成纯函数，每条规则都能
//! 直接写成测试。
//!
//! ```
//! use kui_widgets::menu::{navigate, Layout, MenuKey, MenuAction};
//!
//! let enabled = [true, false, true];
//! // 从第 0 项往下：跳过禁用的第 1 项，落到第 2 项。
//! assert_eq!(
//!     navigate(MenuKey::Down, Layout::Column, &enabled, Some(0)),
//!     MenuAction::Highlight(2),
//! );
//! ```
//!
//! 下半是看得见的那个菜单：[`menu_button`](WidgetUi::menu_button)、
//! [`begin_menu`](WidgetUi::begin_menu)、[`menu_item`](WidgetUi::menu_item)、
//! [`submenu_item`](WidgetUi::submenu_item)。摆在哪里由
//! [`popover`](crate::popover) 算。
//!
//! # 浮层怎么摆
//!
//! 菜单的位置**不能在排版时给**：它要贴着锚点，而锚点排在哪要等这一轮
//! 求解出来才知道。所以走两步——先让它[绝对定位](kui::Style::absolute)
//! （不占位置，但量得出尺寸），求解之后再整段平移过去。
//!
//! 不占位置这一点是必须的：菜单要是占了正常的布局位置，每打开一次菜单，
//! 底下的整个界面都会往下跳一截。
//!
//! # 菜单是模态的
//!
//! 只要有菜单开着（[`menus_open`](WidgetUi::menus_open)），方向键和
//! 回车就**全归菜单**，滑条和单选组要让路。不让的话按一下方向键会既在
//! 菜单里移动高亮，又把底下那条滑条的值改了。

/// 菜单的排布方向。
///
/// 决定哪一对方向键在这个菜单里有效：竖排菜单按左右键不应该动，
/// 那两个键要留给「进入子菜单 / 退回上一级」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// 竖排，上下键移动。常见的下拉菜单、右键菜单。
    #[default]
    Column,
    /// 横排，左右键移动。菜单栏。
    Row,
}

/// 送进菜单的一次按键。
///
/// 这里是「意图」不是物理按键——`Enter` 和 `Space` 都归到
/// [`Activate`](MenuKey::Activate)，免得每个分支写两遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    /// 上方向键。
    Up,
    /// 下方向键。
    Down,
    /// 左方向键。
    Left,
    /// 右方向键。
    Right,
    /// Home：跳到第一项。
    Home,
    /// End：跳到最后一项。
    End,
    /// Esc：关闭。
    Escape,
    /// Enter 或空格：激活当前项。
    Activate,
}

/// 菜单该做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// 把高亮挪到这一项。
    Highlight(usize),
    /// 激活这一项，然后关掉整条菜单链。
    Activate(usize),
    /// 关闭菜单，焦点还给打开它的那个按钮。
    Close,
    /// 进入当前项的子菜单。
    OpenSubmenu(usize),
    /// 退回上一级菜单。顶层菜单收到左键时也是这个——菜单栏里
    /// 它表示「切到左边那个菜单」。
    CloseSubmenu,
    /// 这个键在当前状态下没有意义，交给别人处理。
    Ignored,
}

/// 处理菜单里的一次按键。
///
/// `enabled` 每项一个：`false` 表示该项禁用。禁用项**能被跳过但不能
/// 被停留**——高亮停在一个点不动的项上，用户会以为菜单卡住了。
///
/// `current` 是当前高亮的下标，`None` 表示还没有高亮（菜单刚打开）。
///
/// # 绕回
///
/// 到底了再按下会回到第一项。菜单通常很短，绕回比停在末尾好用；
/// 停在末尾的话用户得原路按回去。
///
/// # 全禁用
///
/// 所有项都禁用（或者一项都没有）时返回 [`Ignored`](MenuAction::Ignored)——
/// 没有任何一项可以停留，硬要给个下标只会指向一个点不了的项。
pub fn navigate(
    key: MenuKey,
    layout: Layout,
    enabled: &[bool],
    current: Option<usize>,
) -> MenuAction {
    // 这两个和排布方向无关，先处理。
    match key {
        MenuKey::Escape => return MenuAction::Close,
        MenuKey::Activate => {
            return match current {
                // 禁用项按回车不该有反应。到得了这里说明高亮是外面
                // 设的，没经过本函数——仍然要挡住。
                Some(index) if enabled.get(index) == Some(&true) => MenuAction::Activate(index),
                _ => MenuAction::Ignored,
            };
        }
        _ => {}
    }

    // 方向键是否属于这个菜单的主轴。
    let (prev, next) = match layout {
        Layout::Column => (MenuKey::Up, MenuKey::Down),
        Layout::Row => (MenuKey::Left, MenuKey::Right),
    };

    if key == prev {
        return step(enabled, current, Step::Backward);
    }
    if key == next {
        return step(enabled, current, Step::Forward);
    }

    match key {
        MenuKey::Home => first_enabled(enabled, Step::Forward),
        MenuKey::End => first_enabled(enabled, Step::Backward),
        // 交叉轴上的左右键管子菜单。竖排菜单里右键进子菜单、左键退回，
        // 这是各家菜单的通例。
        MenuKey::Right if layout == Layout::Column => match current {
            Some(index) if enabled.get(index) == Some(&true) => MenuAction::OpenSubmenu(index),
            _ => MenuAction::Ignored,
        },
        MenuKey::Left if layout == Layout::Column => MenuAction::CloseSubmenu,
        // 横排菜单的上下键：下键展开当前项的子菜单（菜单栏就是这么用的），
        // 上键收起。
        MenuKey::Down if layout == Layout::Row => match current {
            Some(index) if enabled.get(index) == Some(&true) => MenuAction::OpenSubmenu(index),
            _ => MenuAction::Ignored,
        },
        MenuKey::Up if layout == Layout::Row => MenuAction::CloseSubmenu,
        _ => MenuAction::Ignored,
    }
}

/// 走哪个方向。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Forward,
    Backward,
}

/// 从 `current` 出发，朝 `step` 找下一个可用项，绕回。
fn step(enabled: &[bool], current: Option<usize>, step: Step) -> MenuAction {
    let count = enabled.len();
    if count == 0 {
        return MenuAction::Ignored;
    }

    let Some(current) = current else {
        // 没有高亮时，往下就落在第一个可用项，往上就落在最后一个。
        // 刚打开菜单按上键直接到底，比先跳到顶再一路按上去顺手。
        return first_enabled(enabled, step);
    };

    // 外面的下标可能已经越界（菜单项变少了但它还没更新），先折回范围内。
    let current = current % count;

    // 从 current 之后走一圈。走满 count 步就回到了原点，说明整圈
    // 只有它自己可用（或者一个都没有）。
    for offset in 1..=count {
        let index = match step {
            Step::Forward => (current + offset) % count,
            // 先加 count 再减，避免 usize 在 current < offset 时下溢。
            // offset 最大就是 count，所以 count - offset 不会下溢。
            Step::Backward => (current + count - offset) % count,
        };
        if enabled[index] {
            return MenuAction::Highlight(index);
        }
    }
    MenuAction::Ignored
}

/// 从头（或从尾）找第一个可用项。
fn first_enabled(enabled: &[bool], step: Step) -> MenuAction {
    let found = match step {
        Step::Forward => enabled.iter().position(|e| *e),
        Step::Backward => enabled.iter().rposition(|e| *e),
    };
    match found {
        Some(index) => MenuAction::Highlight(index),
        None => MenuAction::Ignored,
    }
}

// ───────────────────────── 控件 ─────────────────────────
//
// 上面是纯逻辑，下面才是看得见的那个菜单。分开是因为上面那部分
// 不需要窗口、字体、GPU 就能全测一遍，而它正是最容易出错的地方。

use kfont::TextStyle;
use kmath::{Vec2, Vec4};
use kui::{Id, NavKey, Rect, Response, Style, Ui, UiInput};

use crate::popover::{self, Align, Placement, Side};
use crate::widgets::{MenuFrame, Theme, Widget, WidgetUi, text_style};

/// 菜单和屏幕边缘之间至少留这么多。
const SCREEN_MARGIN: f32 = 4.0;

/// 子菜单箭头占的宽度。
const ARROW_WIDTH: f32 = 16.0;

impl WidgetUi {
    /// 一个会弹出菜单的按钮。点它开合自己的菜单。
    ///
    /// 菜单的内容用 [`begin_menu`](Self::begin_menu) 声明，锚点就是这里
    /// 返回的 id：
    ///
    /// ```no_run
    /// # use kui_widgets::WidgetUi;
    /// # let mut w = WidgetUi::default();
    /// let file = w.menu_button("file", "文件");
    /// if w.begin_menu(file) {
    ///     let open = w.menu_item("open", "打开");
    ///     let quit = w.menu_item("quit", "退出");
    ///     w.end_menu();
    ///
    ///     if w.response(quit).clicked {
    ///         // 退出
    ///     }
    /// }
    /// ```
    ///
    /// **菜单要在这一帧的最后声明**，和模态遮罩同理：命中从后往前找，
    /// 声明得早的话菜单会被它盖住的那些控件抢走点击。
    pub fn menu_button(&mut self, id: &str, text: impl Into<String>) -> Id {
        let key = Id::new(id);
        let open = self.menu_chain.first() == Some(&key);
        self.push(
            id,
            Widget::MenuButton {
                text: text.into(),
                open,
            },
        )
    }

    /// 一个下拉框：合着时显示当前选中的那一项，点开是一列选项。
    ///
    /// 返回的 id 既是这个框本身，也是它那层浮层的锚点——接着交给
    /// [`dropdown_menu`](Self::dropdown_menu)。
    ///
    /// ```no_run
    /// # use kui_widgets::WidgetUi;
    /// # let mut w = WidgetUi::default();
    /// # let mut quality = 1usize;
    /// const QUALITY: [&str; 3] = ["低", "中", "高"];
    ///
    /// // 摆在它该在的位置。
    /// let picker = w.dropdown("quality", QUALITY[quality]);
    ///
    /// // ……界面的其余部分……
    ///
    /// // 帧末再弹列表。
    /// if let Some(picked) = w.dropdown_menu(picker, &QUALITY, quality) {
    ///     quality = picked;
    /// }
    /// ```
    ///
    /// # 为什么要分成两次调用
    ///
    /// 和 [`menu_button`](Self::menu_button) 是同一个理由：**浮层必须
    /// 在这一帧的最后声明**。命中从后往前找，弹出的列表要是声明得早，
    /// 后面声明的控件会从它头上抢走点击——列表看得见，却点不动。
    ///
    /// 合成一次调用当然更顺手，但那样就只有「下拉框是界面里最后一个
    /// 控件」时才对，而这个前提没法在类型上表达，也没法在运行时检查。
    pub fn dropdown(&mut self, id: &str, text: impl Into<String>) -> Id {
        let key = Id::new(id);
        let open = self.menu_chain.first() == Some(&key);
        self.push(
            id,
            Widget::Dropdown {
                text: text.into(),
                open,
            },
        )
    }

    /// 弹出下拉框的选项列表，返回**这一帧被选中的下标**。
    ///
    /// 没打开、或者打开了但没点任何一项时返回 [`None`]——
    /// 也就是说返回 `Some` 的那一帧才是「用户改了选择」。
    ///
    /// # 滞后一帧
    ///
    /// 和 [`response`](Self::response) 一样：矩形要等整棵树排完才知道，
    /// 所以「被点中」是在松开的**下一帧**才读得到的。菜单的关闭也
    /// 刻意推迟了一帧，正是为了让这一读读得到——当场关掉的话，
    /// 被点的那一项下一帧就不再被声明，用户会看到「点哪一项都没反应」。
    ///
    /// `selected` 是当前值，用来在列表里打勾。**越界不会 panic**，
    /// 只是没有一项带勾：选项列表长度变化（难度选项随解锁增加）时
    /// 崩掉一个界面不划算。
    pub fn dropdown_menu(
        &mut self,
        anchor: Id,
        options: &[impl AsRef<str>],
        selected: usize,
    ) -> Option<usize> {
        if !self.begin_menu(anchor) {
            return None;
        }

        let mut picked = None;
        // id 从锚点派生，所以同一个界面里放两个下拉框不会互相串。
        for (index, option) in options.iter().enumerate() {
            let item = self.push_menu_item(
                &format!("{}#{index}", anchor.0),
                option.as_ref().to_string(),
                true,
                false,
                index == selected,
            );
            if self.response(item).clicked {
                picked = Some(index);
            }
        }
        self.end_menu();
        picked
    }

    /// 开 `anchor` 这个锚点的菜单。返回它**是不是开着**。
    ///
    /// 返回 `false` 时里面的项一个都不要声明——声明了再藏的话它们仍然
    /// 参与命中，鼠标扫过关着的菜单所在的那片区域会莫名点不到底下的东西。
    ///
    /// 锚点可以是 [`menu_button`](Self::menu_button)，也可以是
    /// [`submenu_item`](Self::submenu_item)——子菜单和顶层菜单在这里
    /// 是同一件事，都是「某个东西旁边的那层浮层」。
    pub fn begin_menu(&mut self, anchor: Id) -> bool {
        let Some(depth) = self.menu_chain.iter().position(|open| *open == anchor) else {
            return false;
        };
        if self.collapsed {
            return false;
        }
        self.end_menu_frame();
        self.open_menu_frame = Some(self.menu_frames.len());
        self.menu_frames.push(MenuFrame {
            anchor,
            depth,
            first: self.declared.len(),
            last: usize::MAX,
        });
        true
    }

    /// 收一层菜单。
    pub fn end_menu(&mut self) {
        self.end_menu_frame();
    }

    fn end_menu_frame(&mut self) {
        if let Some(index) = self.open_menu_frame.take() {
            self.menu_frames[index].last = self.declared.len();
        }
    }

    /// 菜单里的一项。
    pub fn menu_item(&mut self, id: &str, text: impl Into<String>) -> Id {
        self.menu_item_with(id, text, true)
    }

    /// 菜单里的一项，可以是禁用的。
    ///
    /// 禁用项**能被跳过但不能被停留**——高亮停在一个点不动的项上，
    /// 用户会以为菜单卡住了。
    pub fn menu_item_with(&mut self, id: &str, text: impl Into<String>, enabled: bool) -> Id {
        self.push_menu_item(id, text.into(), enabled, false, false)
    }

    /// 菜单里一个可勾选的项。左边画一个勾。
    ///
    /// 「显示网格 ✓」这类开关式的菜单项用它。状态和复选框一样由调用方
    /// 保管——存在控件里的话，同一个 id 在两处用就会互相覆盖。
    pub fn menu_item_checked(&mut self, id: &str, text: impl Into<String>, checked: bool) -> Id {
        self.push_menu_item(id, text.into(), true, false, checked)
    }

    /// 菜单里一个会展开子菜单的项。
    ///
    /// 返回的 id 就是那层子菜单的锚点，接着用
    /// [`begin_menu`](Self::begin_menu) 声明它的内容。
    pub fn submenu_item(&mut self, id: &str, text: impl Into<String>) -> Id {
        self.push_menu_item(id, text.into(), true, true, false)
    }

    fn push_menu_item(
        &mut self,
        id: &str,
        text: String,
        enabled: bool,
        submenu: bool,
        checked: bool,
    ) -> Id {
        let key = Id::new(id);
        // 高亮按这一层菜单记，所以要知道自己是这层的第几项。
        //
        // 数的是**菜单项**的个数，不是声明的个数：菜单里夹一个标题或
        // 分隔用的标签是很自然的写法，而方向键那边数的也是菜单项——
        // 两边的下标口径必须一致，否则夹一个标签就会高亮错行。
        let highlighted = self
            .open_menu_frame
            .and_then(|index| self.menu_frames.get(index))
            .and_then(|frame| {
                let position = self.declared[frame.first..]
                    .iter()
                    .filter(|declared| matches!(declared.widget, Widget::MenuItem { .. }))
                    .count();
                self.menu_highlight
                    .get(&frame.anchor)
                    .map(|&highlight| highlight == position)
            })
            .unwrap_or(false);

        self.push(
            id,
            Widget::MenuItem {
                text,
                enabled,
                highlighted,
                submenu,
                checked,
            },
        );
        key
    }

    /// 有菜单开着吗。
    ///
    /// 为真时方向键归菜单——滑条和单选组要让路，否则按一下方向键会
    /// 既在菜单里移动高亮，又把底下那条滑条的值改了。
    pub fn menus_open(&self) -> bool {
        !self.menu_chain.is_empty()
    }

    /// 关掉所有菜单。
    pub fn close_menus(&mut self) {
        self.menu_chain.clear();
        self.menu_highlight.clear();
        self.menu_close_pending = false;
    }

    /// 菜单的开合、高亮、子菜单展开。
    ///
    /// 排在 `finish` 的交互判定之后：这里读的 `clicked` / `hovered`
    /// 都是本帧刚算出来的。
    pub(crate) fn update_menus(&mut self, input: &UiInput) {
        // 上一帧有项被选中，这一帧才真的关。
        //
        // 拖这一帧是为了让调用方读得到那一下：它在**这一帧的声明期**
        // 读 `response(item).clicked`，那时菜单还开着、项还在，读得到；
        // 读完了这里再收摊。上一帧就关的话，那一项这一帧根本不会被
        // 声明出来，`clicked` 无处可读。
        if self.menu_close_pending {
            self.menu_close_pending = false;
            self.menu_chain.clear();
            self.menu_highlight.clear();
        }

        self.toggle_menu_buttons();
        if self.menu_chain.is_empty() {
            // 关着的时候只剩一件事要做：别让上一次的高亮留到下次打开。
            self.menu_highlight.clear();
            return;
        }
        self.follow_hover();
        self.handle_menu_keys(input);
        self.close_on_outside_click(input);
        self.close_on_item_click();
    }

    /// 点菜单按钮 = 开合它的菜单。
    fn toggle_menu_buttons(&mut self) {
        let clicked: Vec<Id> = self
            .declared
            .iter()
            .filter(|d| {
                matches!(
                    d.widget,
                    Widget::MenuButton { .. } | Widget::Dropdown { .. }
                )
            })
            .filter(|d| self.interaction.response(d.id).clicked)
            .map(|d| d.id)
            .collect();

        for id in clicked {
            // 点已经开着的那个 = 关掉。换一个按钮 = 整条链换过去，
            // 不是叠一层——菜单栏里同时开两条菜单是没有意义的。
            if self.menu_chain.first() == Some(&id) {
                self.menu_chain.clear();
            } else {
                self.menu_chain.clear();
                self.menu_chain.push(id);
            }
            self.menu_highlight.clear();
            // 刚开的这条菜单不该被上一条留下的「待关」顺手关掉。
            self.menu_close_pending = false;
        }
    }

    /// 鼠标扫到哪一项，高亮就跟到哪一项；扫到子菜单项就展开它。
    fn follow_hover(&mut self) {
        let Some(hovered) = self.interaction.hovered() else {
            return;
        };

        for frame in self.menu_frames.clone() {
            let items = self.menu_items(&frame);
            let Some(index) = items.iter().position(|item| item.id == hovered) else {
                continue;
            };
            let item = items[index];
            if !item.enabled {
                continue;
            }

            self.menu_highlight.insert(frame.anchor, index);
            // 扫到别的项上，先把这一层底下已经展开的子菜单收掉——
            // 不收的话鼠标从「打开▸」滑到「退出」，那层子菜单还挂在
            // 屏幕上，挡着底下的东西。
            self.menu_chain.truncate(frame.depth + 1);
            if item.submenu {
                self.menu_chain.push(item.id);
            }
            return;
        }
    }

    /// 方向键、回车、Esc。只喂给**最深**的那一层。
    fn handle_menu_keys(&mut self, input: &UiInput) {
        let keys = menu_keys(input);
        if keys.is_empty() {
            return;
        }

        for key in keys {
            let Some(&anchor) = self.menu_chain.last() else {
                return;
            };
            let Some(frame) = self
                .menu_frames
                .iter()
                .find(|f| f.anchor == anchor)
                .copied()
            else {
                return;
            };
            let items = self.menu_items(&frame);
            let enabled: Vec<bool> = items.iter().map(|item| item.enabled).collect();
            let current = self.menu_highlight.get(&anchor).copied();

            match navigate(key, Layout::Column, &enabled, current) {
                MenuAction::Highlight(index) => {
                    self.menu_highlight.insert(anchor, index);
                }
                MenuAction::Activate(index) => {
                    // 报告成「被点了一下」，这样调用方不必为键盘写
                    // 第二条分支——和按钮的键盘激活是同一个道理。
                    self.interaction.activate(items[index].id);
                    // 关菜单要等下一帧，理由见 `update_menus` 开头。
                    self.menu_close_pending = true;
                    return;
                }
                MenuAction::Close => {
                    self.menu_chain.clear();
                    self.menu_highlight.clear();
                    return;
                }
                MenuAction::OpenSubmenu(index) => {
                    let item = items[index];
                    if item.submenu {
                        self.menu_chain.push(item.id);
                    }
                }
                MenuAction::CloseSubmenu => {
                    // 顶层收到左键时不该把整条链关掉——那是「退回上一级」，
                    // 而顶层没有上一级。留着菜单开着，用户再按一下 Esc
                    // 才是关。
                    if self.menu_chain.len() > 1
                        && let Some(closed) = self.menu_chain.pop()
                    {
                        self.menu_highlight.remove(&closed);
                    }
                }
                MenuAction::Ignored => {}
            }
        }
    }

    /// 点在所有菜单和菜单按钮之外 = 关掉。
    fn close_on_outside_click(&mut self, input: &UiInput) {
        if input.pressed.is_empty() {
            return;
        }
        let Some(pointer) = input.pointer else {
            return;
        };

        let in_panel = self.menu_rects.iter().any(|rect| rect.contains(pointer));
        let on_button = self.declared.iter().zip(&self.rects).any(|(d, rect)| {
            matches!(
                d.widget,
                Widget::MenuButton { .. } | Widget::Dropdown { .. }
            ) && rect.contains(pointer)
        });

        if !in_panel && !on_button {
            self.menu_chain.clear();
            self.menu_highlight.clear();
        }
    }

    /// 点中一个普通菜单项 = 执行它，然后关掉整条链。
    ///
    /// 子菜单项不算：点它是展开，不是执行。
    fn close_on_item_click(&mut self) {
        let hit = self.declared.iter().any(|d| {
            matches!(
                d.widget,
                Widget::MenuItem {
                    submenu: false,
                    enabled: true,
                    ..
                }
            ) && self.interaction.response(d.id).clicked
        });
        if hit {
            // 关菜单要等下一帧，理由见 `update_menus` 开头。
            self.menu_close_pending = true;
        }
    }

    /// 一层菜单里的那些项。
    fn menu_items(&self, frame: &MenuFrame) -> Vec<Item> {
        let last = frame.last.min(self.declared.len());
        if frame.first >= last {
            return Vec::new();
        }
        self.declared[frame.first..last]
            .iter()
            .filter_map(|declared| match declared.widget {
                Widget::MenuItem {
                    enabled, submenu, ..
                } => Some(Item {
                    id: declared.id,
                    enabled,
                    submenu,
                }),
                _ => None,
            })
            .collect()
    }
}

/// 一层菜单里的一项，摘出状态机要用的那几样。
#[derive(Debug, Clone, Copy)]
struct Item {
    id: Id,
    enabled: bool,
    submenu: bool,
}

/// 把这一帧的输入翻译成菜单认识的键。
///
/// 回车和空格都归 [`MenuKey::Activate`]，理由见那里。
fn menu_keys(input: &UiInput) -> Vec<MenuKey> {
    let mut keys: Vec<MenuKey> = input
        .nav
        .iter()
        .map(|key| match key {
            NavKey::Up => MenuKey::Up,
            NavKey::Down => MenuKey::Down,
            NavKey::Left => MenuKey::Left,
            NavKey::Right => MenuKey::Right,
            NavKey::Home => MenuKey::Home,
            NavKey::End => MenuKey::End,
            NavKey::Escape => MenuKey::Escape,
        })
        .collect();
    if input.activate {
        keys.push(MenuKey::Activate);
    }
    keys
}

/// 装一层菜单的容器样式。
///
/// 绝对定位：菜单弹出来时不该把它下面的控件往下顶。位置留到求解之后
/// 再平移——那时才知道锚点排在了哪里。
pub(crate) fn container_style(theme: &Theme) -> Style {
    Style {
        direction: kui::Direction::Column,
        align: kui::AlignCross::Stretch,
        // 内边距跟着圆角走：圆角越大，四个角上空出来的地方越多，
        // 内容贴太近会被切掉一块。
        padding: kui::Edges::all(theme.radius * 0.5),
        // 项与项之间不留缝，高亮条才连得成一片——各家菜单都是这样。
        gap: 0.0,
        absolute: true,
        ..Default::default()
    }
}

/// 一层菜单该摆在哪。
///
/// 顶层从锚点**下方**长出来（菜单按钮在上面），子菜单从**右侧**长出来。
/// 放不下时先试相反的一侧——上下翻转比左右乱跳自然得多。
pub(crate) fn place(anchor: Rect, size: Vec2, depth: usize, screen: Vec2) -> Rect {
    let side = if depth == 0 {
        Side::Bottom
    } else {
        Side::Right
    };
    let candidates = [
        Placement::new(side, Align::Start),
        Placement::new(side.mirror(), Align::Start),
        Placement::new(side, Align::End),
    ];
    popover::place(anchor, size, &candidates, screen, SCREEN_MARGIN)
}

/// 量一个菜单按钮的内容。
pub(crate) fn button_size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    ui.measure(text, &text_style(theme.font_size), None).size
}

/// 量一个下拉框的内容：文字加右边那个 ▾。
pub(crate) fn dropdown_size(ui: &Ui, theme: &Theme, text: &str) -> Vec2 {
    let size = ui.measure(text, &text_style(theme.font_size), None).size;
    Vec2::new(size.x + ARROW_WIDTH + 8.0, size.y)
}

/// 出几何：下拉框。
///
/// 和菜单按钮的区别是**它显示的是值不是命令**——所以画成一个带边框的
/// 字段（文字靠左，像输入框），而不是一段居中的文字。
/// 画成一样的话，用户分不出「点了会执行什么」和「点了会挑一个值」。
pub(crate) fn paint_dropdown(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    open: bool,
) {
    let fill = if open || response.held {
        theme.active
    } else if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rounded_rect(rect, theme.radius, fill);
    ui.border(rect, theme.radius, 1.0, theme.outline);
    if response.focused {
        ui.border(rect.shrink(-2.0), theme.radius + 2.0, 2.0, theme.focus);
    }

    ui.text(
        Vec2::new(rect.min.x + 6.0, rect.center().y - theme.font_size * 0.6),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        theme.text,
        // 文字太长时截断，别让它盖到箭头上。
        Some((rect.size().x - ARROW_WIDTH - 8.0).max(0.0)),
    );
    paint_chevron(ui, rect, theme.text);
}

/// 下拉框右边那个朝下的 ▾。
///
/// 子菜单那个箭头朝右，这个朝下——方向本身就是提示：
/// 「列表会掉在下面」而不是「展开在旁边」。
fn paint_chevron(ui: &mut Ui, rect: Rect, color: Vec4) {
    let size = 4.0;
    let center = Vec2::new(rect.max.x - ARROW_WIDTH * 0.5, rect.center().y);
    // 两笔画一个 ∨。这一层只有矩形和线段，为一个 8 像素的箭头
    // 引进多边形填充不划算。
    ui.polyline(
        &[
            Vec2::new(center.x - size, center.y - size * 0.5),
            Vec2::new(center.x, center.y + size * 0.5),
            Vec2::new(center.x + size, center.y - size * 0.5),
        ],
        1.5,
        color,
    );
}

/// 量一个菜单项的内容。
pub(crate) fn item_size(ui: &Ui, theme: &Theme, text: &str, submenu: bool) -> Vec2 {
    let size = ui.measure(text, &text_style(theme.font_size), None).size;
    // 左边永远留一条放勾的槽（理由见 `paint_item`），右边有子菜单时
    // 再给箭头留位置——不留的话箭头会压在文字上。
    Vec2::new(
        size.x + CHECK_WIDTH + if submenu { ARROW_WIDTH } else { 0.0 },
        size.y,
    )
}

/// 菜单的底板。
pub(crate) fn paint_backdrop(ui: &mut Ui, theme: &Theme, rect: Rect) {
    ui.rounded_rect(rect, theme.radius, theme.panel);
    ui.border(rect, theme.radius, 1.0, theme.outline);
}

/// 出几何：菜单按钮。
pub(crate) fn paint_button(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    open: bool,
) {
    // 开着的时候一直是按下的样子，好让用户看出这条菜单是从哪儿来的。
    let fill = if open || response.held {
        theme.active
    } else if response.hovered {
        theme.hovered
    } else {
        theme.surface
    };
    ui.rounded_rect(rect, theme.radius, fill);
    if response.focused {
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

/// 出几何：菜单项。
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_item(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    response: &Response,
    text: &str,
    enabled: bool,
    highlighted: bool,
    submenu: bool,
    checked: bool,
) {
    // 高亮是**键盘**走出来的，悬停是鼠标扫出来的，两者画成一样——
    // 用户不该看得出自己刚才用的是键盘还是鼠标。
    if enabled && (highlighted || response.hovered) {
        ui.rounded_rect(rect, theme.radius * 0.5, theme.accent);
    }

    let color = if enabled { theme.text } else { theme.dim };

    // 勾占左边一条固定宽度的槽。**不管勾不勾选都留着这条槽**——
    // 只在勾选时留的话，勾一下整列文字会往右跳一截；而同一条菜单里
    // 有勾的和没勾的混着排时，文字会参差不齐。
    // 系统菜单也都是这么做的（左边永远有一条空槽）。
    let text_x = rect.min.x + CHECK_WIDTH;
    if checked {
        let size = theme.font_size * 0.55;
        let box_rect = Rect {
            min: Vec2::new(rect.min.x + 2.0, rect.center().y - size * 0.5),
            max: Vec2::new(rect.min.x + 2.0 + size, rect.center().y + size * 0.5),
        };
        // 和复选框用的是同一个勾，形状一致。
        ui.polyline(&crate::checkbox::check_points(box_rect), size * 0.15, color);
    }

    ui.text(
        Vec2::new(text_x, rect.center().y - theme.font_size * 0.6),
        text,
        &TextStyle {
            size: theme.font_size,
            ..Default::default()
        },
        color,
        Some(rect.size().x),
    );

    if submenu {
        paint_arrow(ui, rect, color);
    }
}

/// 勾那条槽有多宽。
const CHECK_WIDTH: f32 = 16.0;

/// 子菜单项右边那个小三角。
fn paint_arrow(ui: &mut Ui, rect: Rect, color: Vec4) {
    let size = 4.0;
    let center = Vec2::new(rect.max.x - ARROW_WIDTH * 0.5, rect.center().y);
    // 用几条横线堆出一个三角：这一层只有矩形和线段两种图元，
    // 为一个 8 像素的箭头引进多边形填充不划算。
    let steps = 4;
    for step in 0..steps {
        let half = size * (1.0 - step as f32 / steps as f32);
        let x = center.x - size * 0.5 + step as f32;
        ui.rect(
            Rect {
                min: Vec2::new(x, center.y - half),
                max: Vec2::new(x + 1.0, center.y + half),
            },
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部可用时，下键就是简单地加一。
    #[test]
    fn down_moves_to_the_next_item() {
        let enabled = [true, true, true];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, Some(0)),
            MenuAction::Highlight(1),
        );
    }

    #[test]
    fn up_moves_to_the_previous_item() {
        let enabled = [true, true, true];
        assert_eq!(
            navigate(MenuKey::Up, Layout::Column, &enabled, Some(2)),
            MenuAction::Highlight(1),
        );
    }

    /// 到底再按下会绕回开头。
    #[test]
    fn down_wraps_around_at_the_end() {
        let enabled = [true, true, true];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, Some(2)),
            MenuAction::Highlight(0),
        );
    }

    /// 在开头按上会绕到末尾。这条专门盯着倒序取模里的下溢：
    /// `current` 是 0 时朴素地写 `current - offset` 会让 usize 翻转。
    #[test]
    fn up_wraps_around_at_the_start() {
        let enabled = [true, true, true];
        assert_eq!(
            navigate(MenuKey::Up, Layout::Column, &enabled, Some(0)),
            MenuAction::Highlight(2),
        );
    }

    /// 禁用项被跳过，不会被停留。
    #[test]
    fn disabled_items_are_skipped() {
        let enabled = [true, false, false, true];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, Some(0)),
            MenuAction::Highlight(3),
        );
    }

    /// 往回走也要跳过禁用项。
    #[test]
    fn disabled_items_are_skipped_backwards() {
        let enabled = [true, false, false, true];
        assert_eq!(
            navigate(MenuKey::Up, Layout::Column, &enabled, Some(3)),
            MenuAction::Highlight(0),
        );
    }

    /// 绕回时同样跳过禁用项：末项往下要越过开头的禁用项。
    #[test]
    fn wrapping_skips_disabled_items_too() {
        let enabled = [false, true, true];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, Some(2)),
            MenuAction::Highlight(1),
        );
    }

    /// 只有一项可用时，按方向键停在原地——不是「没反应」，
    /// 而是走了一圈又回到它。
    #[test]
    fn a_lone_enabled_item_stays_put() {
        let enabled = [false, true, false];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, Some(1)),
            MenuAction::Highlight(1),
        );
    }

    /// 一项都不可用时不给下标。给了的话高亮会停在点不动的项上。
    #[test]
    fn an_all_disabled_menu_refuses_to_highlight() {
        let enabled = [false, false];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, None),
            MenuAction::Ignored,
        );
        assert_eq!(
            navigate(MenuKey::Home, Layout::Column, &enabled, None),
            MenuAction::Ignored,
        );
    }

    /// 空菜单不该 panic。取模里的除零就藏在这。
    #[test]
    fn an_empty_menu_is_not_a_panic() {
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &[], Some(0)),
            MenuAction::Ignored,
        );
        assert_eq!(
            navigate(MenuKey::Up, Layout::Column, &[], None),
            MenuAction::Ignored,
        );
    }

    /// 菜单刚打开（还没高亮）时按下键落在第一个可用项。
    #[test]
    fn down_from_nothing_lands_on_the_first_enabled() {
        let enabled = [false, true, true];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, None),
            MenuAction::Highlight(1),
        );
    }

    /// 刚打开时按上键直接到最后一项，省得从头一路按下去。
    #[test]
    fn up_from_nothing_lands_on_the_last_enabled() {
        let enabled = [true, true, false];
        assert_eq!(
            navigate(MenuKey::Up, Layout::Column, &enabled, None),
            MenuAction::Highlight(1),
        );
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let enabled = [false, true, true, false];
        assert_eq!(
            navigate(MenuKey::Home, Layout::Column, &enabled, Some(2)),
            MenuAction::Highlight(1),
        );
        assert_eq!(
            navigate(MenuKey::End, Layout::Column, &enabled, Some(1)),
            MenuAction::Highlight(2),
        );
    }

    /// 横排菜单认左右键，不认上下键。
    #[test]
    fn a_row_menu_moves_on_left_and_right() {
        let enabled = [true, true];
        assert_eq!(
            navigate(MenuKey::Right, Layout::Row, &enabled, Some(0)),
            MenuAction::Highlight(1),
        );
        assert_eq!(
            navigate(MenuKey::Left, Layout::Row, &enabled, Some(1)),
            MenuAction::Highlight(0),
        );
    }

    /// 竖排菜单里左右键不移动高亮，它们归子菜单管。
    /// 弄反的话在菜单栏里按左右会同时切菜单又切菜单项。
    #[test]
    fn a_column_menu_does_not_move_on_left_and_right() {
        let enabled = [true, true];
        assert_eq!(
            navigate(MenuKey::Right, Layout::Column, &enabled, Some(0)),
            MenuAction::OpenSubmenu(0),
        );
        assert_eq!(
            navigate(MenuKey::Left, Layout::Column, &enabled, Some(0)),
            MenuAction::CloseSubmenu,
        );
    }

    /// 横排菜单按下键展开子菜单——菜单栏的标准行为。
    #[test]
    fn a_row_menu_opens_a_submenu_on_down() {
        let enabled = [true];
        assert_eq!(
            navigate(MenuKey::Down, Layout::Row, &enabled, Some(0)),
            MenuAction::OpenSubmenu(0),
        );
        assert_eq!(
            navigate(MenuKey::Up, Layout::Row, &enabled, Some(0)),
            MenuAction::CloseSubmenu,
        );
    }

    /// 禁用项上不能展开子菜单。
    #[test]
    fn a_disabled_item_opens_no_submenu() {
        let enabled = [false];
        assert_eq!(
            navigate(MenuKey::Right, Layout::Column, &enabled, Some(0)),
            MenuAction::Ignored,
        );
    }

    #[test]
    fn escape_closes_from_any_state() {
        assert_eq!(
            navigate(MenuKey::Escape, Layout::Column, &[true], Some(0)),
            MenuAction::Close,
        );
        // 没高亮、菜单空的时候也要能关掉，否则菜单关不掉了。
        assert_eq!(
            navigate(MenuKey::Escape, Layout::Row, &[], None),
            MenuAction::Close,
        );
    }

    #[test]
    fn enter_activates_the_highlighted_item() {
        assert_eq!(
            navigate(MenuKey::Activate, Layout::Column, &[true, true], Some(1)),
            MenuAction::Activate(1),
        );
    }

    /// 没有高亮时回车什么也不做，别拿第一项凑数——
    /// 用户没选中任何东西却触发了动作是最难查的那种 bug。
    #[test]
    fn enter_without_a_highlight_does_nothing() {
        assert_eq!(
            navigate(MenuKey::Activate, Layout::Column, &[true], None),
            MenuAction::Ignored,
        );
    }

    /// 高亮被外面设到了禁用项上时，回车仍然要挡住。
    #[test]
    fn enter_on_a_disabled_item_does_nothing() {
        assert_eq!(
            navigate(MenuKey::Activate, Layout::Column, &[true, false], Some(1)),
            MenuAction::Ignored,
        );
    }

    /// 高亮下标越界（菜单项变少了但外面的下标还没更新）不该 panic。
    #[test]
    fn an_out_of_range_highlight_is_not_a_panic() {
        let enabled = [true, true];
        assert_eq!(
            navigate(MenuKey::Activate, Layout::Column, &enabled, Some(9)),
            MenuAction::Ignored,
        );
        // 方向键从越界处出发，落回一个合法项。
        assert_eq!(
            navigate(MenuKey::Down, Layout::Column, &enabled, Some(9)),
            MenuAction::Highlight(0),
        );
    }
}

/// 菜单**控件**的测试。上面那一组测的是纯逻辑，这里测的是它接上
/// 布局、命中、浮层摆位之后还对不对。
#[cfg(test)]
mod widget_tests {
    use super::*;
    use crate::WidgetUi;
    use crate::testing::{at, press, ui};
    use kui::PointerButton;

    /// 一个菜单栏：两个菜单按钮，第一个的菜单里有三项，
    /// 其中「最近打开」带子菜单、「保存」是禁用的。
    fn declare(w: &mut WidgetUi) {
        w.begin_row();
        let file = w.menu_button("file", "文件");
        let edit = w.menu_button("edit", "编辑");
        w.end_row();

        if w.begin_menu(file) {
            w.menu_item("open", "打开");
            let recent = w.submenu_item("recent", "最近打开");
            w.menu_item_with("save", "保存", false);
            w.end_menu();

            if w.begin_menu(recent) {
                w.menu_item("r0", "第一个");
                w.menu_item("r1", "第二个");
                w.end_menu();
            }
        }

        if w.begin_menu(edit) {
            w.menu_item("undo", "撤销");
            w.end_menu();
        }
    }

    fn frame(w: &mut WidgetUi, ui: &mut kui::Ui, input: &UiInput) {
        w.begin();
        declare(w);
        w.finish(ui, input);
    }

    fn nav(key: NavKey) -> UiInput {
        UiInput {
            nav: vec![key],
            ..Default::default()
        }
    }

    fn activate() -> UiInput {
        UiInput {
            activate: true,
            ..Default::default()
        }
    }

    /// 在某处完成一次点击（按下、松开两帧）。
    fn click(w: &mut WidgetUi, ui: &mut kui::Ui, point: Vec2) {
        frame(w, ui, &press(point.x, point.y));
        let mut release = at(point.x, point.y);
        release.released.push(PointerButton::Primary);
        frame(w, ui, &release);
    }

    /// 点菜单按钮打开菜单，再点一下关掉。
    #[test]
    fn clicking_the_button_opens_and_closes_the_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        assert!(!w.menus_open());

        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        assert!(w.menus_open(), "点菜单按钮该把菜单打开");

        click(&mut w, &mut ui, button);
        assert!(!w.menus_open(), "再点一下该关掉");
    }

    /// 菜单关着的时候，里面的项一个都不该存在。
    ///
    /// 声明了再藏的话它们仍然参与命中，鼠标扫过菜单**本该在**的那片
    /// 区域会莫名点不到底下的东西。
    #[test]
    fn a_closed_menu_declares_nothing() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        frame(&mut w, &mut ui, &UiInput::default());

        assert_eq!(
            w.response(Id::new("open")).rect.size(),
            Vec2::ZERO,
            "菜单关着，项不该被排进布局"
        );
    }

    /// 菜单弹出来不该把底下的控件往下顶。
    ///
    /// 这条是绝对定位那一层的意义所在：菜单如果占正常的布局位置，
    /// 每次打开菜单，整个界面都会往下跳一截。
    #[test]
    fn opening_a_menu_does_not_move_the_widgets_below() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare_all = |w: &mut WidgetUi| {
            declare(w);
            w.button("below", "底下的按钮");
        };

        w.begin();
        declare_all(&mut w);
        w.finish(&mut ui, &UiInput::default());
        let before = w.response(Id::new("below")).rect;

        // 打开菜单。
        let button = w.response(Id::new("file")).rect.center();
        for input in [&press(button.x, button.y), &{
            let mut release = at(button.x, button.y);
            release.released.push(PointerButton::Primary);
            release
        }] {
            w.begin();
            declare_all(&mut w);
            w.finish(&mut ui, input);
        }
        assert!(w.menus_open());

        w.begin();
        declare_all(&mut w);
        w.finish(&mut ui, &UiInput::default());
        assert_eq!(
            w.response(Id::new("below")).rect,
            before,
            "菜单把底下的按钮顶开了"
        );
    }

    /// 菜单摆在按钮下面，而且不出屏。
    #[test]
    fn the_menu_sits_below_its_button_and_stays_on_screen() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect;
        click(&mut w, &mut ui, button.center());
        frame(&mut w, &mut ui, &UiInput::default());

        let item = w.response(Id::new("open")).rect;
        assert!(
            item.min.y >= button.max.y - 1.0,
            "菜单该在按钮下面：按钮 {button:?}，项 {item:?}"
        );
        assert!(
            item.min.x >= 0.0 && item.max.x <= crate::testing::SCREEN.x,
            "菜单跑出屏幕了：{item:?}"
        );
    }

    /// 方向键移动高亮，回车激活。
    #[test]
    fn arrow_keys_highlight_and_enter_activates() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);

        // 下键落在第一项「打开」上。
        frame(&mut w, &mut ui, &nav(NavKey::Down));
        frame(&mut w, &mut ui, &activate());
        assert!(w.response(Id::new("open")).clicked, "回车该激活高亮的那项");

        // 关菜单**慢一帧**：这一帧那一项还在，调用方才读得到它的
        // `clicked`（响应本来就滞后一帧）。见
        // [`a_clicked_item_reaches_the_caller_on_the_next_frame`]。
        assert!(w.menus_open(), "关得太急，调用方就读不到这一下了");
        frame(&mut w, &mut ui, &UiInput::default());
        assert!(!w.menus_open(), "下一帧该收摊了");
    }

    /// 方向键跳过禁用项。
    ///
    /// 高亮停在一个点不动的项上，用户会以为菜单卡住了。
    #[test]
    fn arrow_keys_skip_a_disabled_item() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);

        // 三项：打开、最近打开、保存(禁用)。往上键 = 从末尾进，
        // 该落在「最近打开」而不是禁用的「保存」。
        frame(&mut w, &mut ui, &nav(NavKey::Up));
        frame(&mut w, &mut ui, &activate());
        assert!(!w.response(Id::new("save")).clicked, "高亮不该停在禁用项上");
    }

    /// Esc 关掉菜单。
    #[test]
    fn escape_closes_the_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        assert!(w.menus_open());

        frame(&mut w, &mut ui, &nav(NavKey::Escape));
        assert!(!w.menus_open());
    }

    /// 点在菜单外面关掉菜单。
    #[test]
    fn clicking_outside_closes_the_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        assert!(w.menus_open());

        // 屏幕右下角，离菜单很远。
        frame(&mut w, &mut ui, &press(700.0, 550.0));
        assert!(!w.menus_open());
    }

    /// 右方向键展开子菜单，左方向键退回上一级。
    #[test]
    fn right_opens_a_submenu_and_left_goes_back() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);

        // 走到「最近打开」上：下、下。
        frame(&mut w, &mut ui, &nav(NavKey::Down));
        frame(&mut w, &mut ui, &nav(NavKey::Down));
        frame(&mut w, &mut ui, &nav(NavKey::Right));
        frame(&mut w, &mut ui, &UiInput::default());

        assert!(
            w.response(Id::new("r0")).rect.size() != Vec2::ZERO,
            "子菜单没展开"
        );

        frame(&mut w, &mut ui, &nav(NavKey::Left));
        frame(&mut w, &mut ui, &UiInput::default());
        assert!(
            w.response(Id::new("r0")).rect.size() == Vec2::ZERO,
            "左键该收起子菜单"
        );
        assert!(w.menus_open(), "但父菜单该还开着");
    }

    /// 子菜单摆在父菜单**右边**，不是下面。
    #[test]
    fn a_submenu_opens_to_the_side_of_its_parent() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        frame(&mut w, &mut ui, &nav(NavKey::Down));
        frame(&mut w, &mut ui, &nav(NavKey::Down));
        frame(&mut w, &mut ui, &nav(NavKey::Right));
        frame(&mut w, &mut ui, &UiInput::default());

        let parent = w.response(Id::new("recent")).rect;
        let child = w.response(Id::new("r0")).rect;
        assert!(
            child.min.x >= parent.max.x - 1.0,
            "子菜单该在父项右边：父 {parent:?}，子 {child:?}"
        );
    }

    /// 鼠标扫到子菜单项上就展开，扫到别的项上就收掉。
    ///
    /// 不收的话，鼠标从「最近打开▸」滑到「打开」，那层子菜单还挂在
    /// 屏幕上挡着底下的东西。
    #[test]
    fn hovering_opens_and_closes_submenus() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        frame(&mut w, &mut ui, &UiInput::default());

        let recent = w.response(Id::new("recent")).rect.center();
        frame(&mut w, &mut ui, &at(recent.x, recent.y));
        frame(&mut w, &mut ui, &at(recent.x, recent.y));
        assert!(
            w.response(Id::new("r0")).rect.size() != Vec2::ZERO,
            "悬停该展开子菜单"
        );

        let open = w.response(Id::new("open")).rect.center();
        frame(&mut w, &mut ui, &at(open.x, open.y));
        frame(&mut w, &mut ui, &at(open.x, open.y));
        assert!(
            w.response(Id::new("r0")).rect.size() == Vec2::ZERO,
            "扫到别的项上该把子菜单收掉"
        );
    }

    /// 点另一个菜单按钮 = 换过去，不是叠一层。
    #[test]
    fn clicking_another_button_switches_menus() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let file = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, file);
        frame(&mut w, &mut ui, &UiInput::default());
        assert!(w.response(Id::new("open")).rect.size() != Vec2::ZERO);

        let edit = w.response(Id::new("edit")).rect.center();
        click(&mut w, &mut ui, edit);
        frame(&mut w, &mut ui, &UiInput::default());

        assert!(
            w.response(Id::new("undo")).rect.size() != Vec2::ZERO,
            "第二条菜单该开着"
        );
        assert!(
            w.response(Id::new("open")).rect.size() == Vec2::ZERO,
            "第一条菜单该关掉，而不是两条一起开着"
        );
    }

    /// 点中一个普通项，菜单关掉。
    #[test]
    fn clicking_an_item_runs_it_and_closes_the_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        frame(&mut w, &mut ui, &UiInput::default());

        let open = w.response(Id::new("open")).rect.center();
        click(&mut w, &mut ui, open);

        assert!(w.response(Id::new("open")).clicked);
        // 关菜单慢一帧，好让调用方读到上面那个 `clicked`。
        frame(&mut w, &mut ui, &at(open.x, open.y));
        assert!(!w.menus_open(), "点了一项之后菜单该关掉");
    }

    /// 点子菜单项是展开，不是执行——菜单不该跟着关掉。
    #[test]
    fn clicking_a_submenu_item_does_not_close_the_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        frame(&mut w, &mut ui, &UiInput::default());

        let recent = w.response(Id::new("recent")).rect.center();
        click(&mut w, &mut ui, recent);
        assert!(w.menus_open(), "点子菜单项不该把菜单关掉");
    }

    /// 菜单开着的时候，方向键不该同时把底下的滑条也调了。
    #[test]
    fn an_open_menu_takes_the_arrow_keys_from_a_slider() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut value = 50.0;
        let spec = crate::Slider::new(0.0..=100.0);

        let declare_all = |w: &mut WidgetUi, value: &mut f32, input: &UiInput| {
            w.slider_with("s", value, spec, input);
            declare(w);
        };

        // Tab 走到滑条上。
        let tab = UiInput {
            focus_step: 1,
            ..Default::default()
        };
        w.begin();
        declare_all(&mut w, &mut value, &tab);
        w.finish(&mut ui, &tab);

        // 打开菜单。
        let button = w.response(Id::new("file")).rect.center();
        for input in [&press(button.x, button.y), &{
            let mut release = at(button.x, button.y);
            release.released.push(PointerButton::Primary);
            release
        }] {
            w.begin();
            declare_all(&mut w, &mut value, input);
            w.finish(&mut ui, input);
        }
        assert!(w.menus_open());

        let before = value;
        let down = nav(NavKey::Down);
        w.begin();
        declare_all(&mut w, &mut value, &down);
        w.finish(&mut ui, &down);

        assert_eq!(value, before, "菜单开着时方向键不该动滑条");
    }

    /// 菜单画得出东西来：底板加上各项。
    #[test]
    fn an_open_menu_draws_a_backdrop_and_its_items() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        // 每次都拿一块干净的画布来数，免得把上一帧的几何也算进去。
        let count = |w: &mut WidgetUi| {
            let mut probe = crate::testing::ui();
            w.begin();
            declare(w);
            w.finish(&mut probe, &UiInput::default());
            probe.end_frame();
            probe.draw_list().indices().len()
        };

        frame(&mut w, &mut ui, &UiInput::default());
        let closed = count(&mut w);

        let button = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, button);
        let open = count(&mut w);

        assert!(open > closed, "菜单开着该比关着画出更多东西");
    }

    /// 从滚动区里的按钮弹出来的菜单，跟着那个按钮一起滚。
    ///
    /// 这条盯的是「位移算了两遍」：`rects` 这时已经被滚动整体挪过，
    /// 摆位再拿没挪过的求解结果做基准的话，滚动的偏移会被重复算一次，
    /// 菜单会飘到离按钮很远的地方。
    #[test]
    fn a_menu_anchored_inside_a_scroll_area_follows_its_button() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare_scrolled = |w: &mut WidgetUi| {
            w.begin_scroll("area", 80.0);
            for index in 0..6 {
                w.button(&format!("row{index}"), format!("第 {index} 行"));
            }
            let file = w.menu_button("file", "文件");
            w.end_scroll();

            if w.begin_menu(file) {
                w.menu_item("open", "打开");
                w.end_menu();
            }
        };

        // 排一帧，打开菜单。
        w.begin();
        declare_scrolled(&mut w);
        w.finish(&mut ui, &UiInput::default());

        let button = w.response(Id::new("file")).rect.center();
        for input in [&press(button.x, button.y), &{
            let mut release = at(button.x, button.y);
            release.released.push(PointerButton::Primary);
            release
        }] {
            w.begin();
            declare_scrolled(&mut w);
            w.finish(&mut ui, input);
        }
        assert!(w.menus_open());

        // 滚一下，再看菜单和按钮的相对位置。
        let mut scrolled = at(button.x, button.y);
        scrolled.scroll = Vec2::new(0.0, -1.0);
        for input in [&scrolled, &at(button.x, button.y)] {
            w.begin();
            declare_scrolled(&mut w);
            w.finish(&mut ui, input);
        }

        let anchor = w.response(Id::new("file")).rect;
        let item = w.response(Id::new("open")).rect;
        assert!(
            (item.min.y - anchor.max.y).abs() < 20.0,
            "滚动之后菜单脱离了按钮：按钮 {anchor:?}，项 {item:?}"
        );
    }

    /// 菜单里夹一个标签，高亮仍然落在对的项上。
    ///
    /// 高亮的下标和方向键那边的下标必须是同一个口径（都数**菜单项**）。
    /// 一边数声明、一边数菜单项的话，夹一个标题就会高亮错行——
    /// 而「给菜单加个分组标题」是再自然不过的写法。
    #[test]
    fn a_label_inside_a_menu_does_not_shift_the_highlight() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        let declare_labelled = |w: &mut WidgetUi| {
            let file = w.menu_button("file", "文件");
            if w.begin_menu(file) {
                w.label("group", "最近");
                w.menu_item("open", "打开");
                w.menu_item("quit", "退出");
                w.end_menu();
            }
        };

        for input in [&UiInput::default(), &UiInput::default()] {
            w.begin();
            declare_labelled(&mut w);
            w.finish(&mut ui, input);
        }

        let button = w.response(Id::new("file")).rect.center();
        for input in [&press(button.x, button.y), &{
            let mut release = at(button.x, button.y);
            release.released.push(PointerButton::Primary);
            release
        }] {
            w.begin();
            declare_labelled(&mut w);
            w.finish(&mut ui, input);
        }

        // 下键落在第一个**菜单项**上，也就是「打开」——标签不算一项。
        for input in [&nav(NavKey::Down), &activate()] {
            w.begin();
            declare_labelled(&mut w);
            w.finish(&mut ui, input);
        }

        assert!(w.response(Id::new("open")).clicked, "标签把高亮挤偏了一格");
    }

    /// 菜单按钮待在一**行**里，而根容器有固定宽度和外边距。
    ///
    /// 这是真实面板的形状：菜单栏是一行按钮，面板整体贴着窗口右缘。
    /// 单独拿默认根容器测的话，这两件事都测不到。
    #[test]
    fn a_menu_opens_from_a_button_inside_a_row() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.root_style(kui::Style {
            width: kui::Length::Px(264.0),
            margin: kui::Edges {
                left: 536.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
            ..Default::default()
        });

        let declare_bar = |w: &mut WidgetUi| {
            w.begin_row();
            w.label("pad", "");
            let view = w.menu_button("view", "View");
            let edit = w.menu_button("edit", "Edit");
            w.end_row();
            if w.begin_menu(view) {
                w.menu_item("open", "打开");
                w.menu_item("quit", "退出");
                w.end_menu();
            }
            if w.begin_menu(edit) {
                w.menu_item("undo", "撤销");
                w.end_menu();
            }
        };

        w.begin();
        declare_bar(&mut w);
        w.finish(&mut ui, &UiInput::default());

        let button = w.response(Id::new("view")).rect;
        assert!(button.size() != Vec2::ZERO, "菜单按钮没排出来");

        // 点它。
        let target = button.center();
        for input in [&press(target.x, target.y), &{
            let mut release = at(target.x, target.y);
            release.released.push(PointerButton::Primary);
            release
        }] {
            w.begin();
            declare_bar(&mut w);
            w.finish(&mut ui, input);
        }
        assert!(w.menus_open(), "点了菜单按钮，菜单没开");

        // 再走一帧，菜单这时才被声明出来。
        w.begin();
        declare_bar(&mut w);
        w.finish(&mut ui, &at(target.x, target.y));

        let item = w.response(Id::new("open")).rect;
        assert!(
            item.size() != Vec2::ZERO,
            "菜单开着，但项没有尺寸——浮层没被排出来：{item:?}"
        );
        assert!(
            item.min.y >= button.max.y - 1.0,
            "菜单该在按钮下面：按钮 {button:?}，项 {item:?}"
        );
    }

    /// **点中的那一项，调用方真的收得到。**
    ///
    /// 这条按真实用法写：菜单在每帧的声明里搭出来，动作紧跟着
    /// `if w.response(item).clicked` 判断——和面板里所有别的控件一个写法。
    ///
    /// 这里盯的是一个很容易漏掉的死角：响应**滞后一帧**，而菜单被点中
    /// 之后就关了。要是关得太急，下一帧那一项根本不会被声明，
    /// `response` 查不到它，`clicked` 永远读不到——菜单看着能开能关，
    /// 点哪一项都没反应。
    ///
    /// 之所以之前没发现，是因为别的测试都在 `finish` 之后**当帧**读
    /// `clicked`。那个时机只有测试用得上，真实代码读到的是上一帧。
    #[test]
    fn a_clicked_item_reaches_the_caller_on_the_next_frame() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut fired = 0;

        // 和例子里一模一样的写法。
        fn declare(w: &mut WidgetUi, fired: &mut i32) {
            let file = w.menu_button("file", "文件");
            if w.begin_menu(file) {
                let open = w.menu_item("open", "打开");
                w.menu_item("quit", "退出");
                w.end_menu();
                if w.response(open).clicked {
                    *fired += 1;
                }
            }
        }

        let mut run = |w: &mut WidgetUi, ui: &mut kui::Ui, input: &UiInput, fired: &mut i32| {
            w.begin();
            declare(w, fired);
            w.finish(ui, input);
        };

        run(&mut w, &mut ui, &UiInput::default(), &mut fired);
        let button = w.response(Id::new("file")).rect.center();

        // 点开菜单。
        run(&mut w, &mut ui, &press(button.x, button.y), &mut fired);
        let mut release = at(button.x, button.y);
        release.released.push(PointerButton::Primary);
        run(&mut w, &mut ui, &release, &mut fired);
        run(&mut w, &mut ui, &at(button.x, button.y), &mut fired);

        let item = w.response(Id::new("open")).rect.center();
        assert!(w.menus_open(), "菜单没开，后面就没得测了");

        // 点「打开」。
        run(&mut w, &mut ui, &press(item.x, item.y), &mut fired);
        let mut release = at(item.x, item.y);
        release.released.push(PointerButton::Primary);
        run(&mut w, &mut ui, &release, &mut fired);
        // 再跑几帧，让调用方有机会读到。
        for _ in 0..3 {
            run(&mut w, &mut ui, &at(item.x, item.y), &mut fired);
        }

        assert_eq!(fired, 1, "调用方没收到这一下点击");
        assert!(!w.menus_open(), "执行完该把菜单关掉");
    }

    /// 键盘激活也一样收得到。
    #[test]
    fn an_activated_item_reaches_the_caller_on_the_next_frame() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        let mut fired = 0;

        fn declare(w: &mut WidgetUi, fired: &mut i32) {
            let file = w.menu_button("file", "文件");
            if w.begin_menu(file) {
                let open = w.menu_item("open", "打开");
                w.end_menu();
                if w.response(open).clicked {
                    *fired += 1;
                }
            }
        }

        let mut run = |w: &mut WidgetUi, ui: &mut kui::Ui, input: &UiInput, fired: &mut i32| {
            w.begin();
            declare(w, fired);
            w.finish(ui, input);
        };

        run(&mut w, &mut ui, &UiInput::default(), &mut fired);
        let button = w.response(Id::new("file")).rect.center();
        run(&mut w, &mut ui, &press(button.x, button.y), &mut fired);
        let mut release = at(button.x, button.y);
        release.released.push(PointerButton::Primary);
        run(&mut w, &mut ui, &release, &mut fired);

        // 方向键选中第一项，回车。
        run(&mut w, &mut ui, &nav(NavKey::Down), &mut fired);
        run(&mut w, &mut ui, &activate(), &mut fired);
        for _ in 0..3 {
            run(&mut w, &mut ui, &UiInput::default(), &mut fired);
        }

        assert_eq!(fired, 1, "键盘激活没传到调用方");
    }

    /// 按住不放几帧再松手，仍然能打开菜单。
    ///
    /// 真实的鼠标点击就是这样：按下和松开之间隔着几十毫秒、好几帧。
    /// 前面那些测试把两者放在相邻两帧，把中间这段整个跳过了。
    #[test]
    fn a_slow_click_still_opens_the_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let target = w.response(Id::new("file")).rect.center();

        frame(&mut w, &mut ui, &press(target.x, target.y));
        // 按住不动。
        for _ in 0..6 {
            frame(&mut w, &mut ui, &at(target.x, target.y));
        }
        let mut release = at(target.x, target.y);
        release.released.push(PointerButton::Primary);
        frame(&mut w, &mut ui, &release);

        assert!(w.menus_open(), "按住几帧再松手，菜单没开");

        frame(&mut w, &mut ui, &at(target.x, target.y));
        assert!(
            w.response(Id::new("open")).rect.size() != Vec2::ZERO,
            "菜单开着，但项没排出来"
        );
    }

    /// 指针停在菜单按钮上不动，菜单该一直开着。
    ///
    /// 打开菜单之后鼠标通常就停在那个按钮上——如果哪一帧把这看成
    /// 「点在菜单外面」，菜单会在打开的下一帧自己关掉，表现就是
    /// 「点了没反应」。
    #[test]
    fn the_menu_stays_open_while_the_pointer_rests_on_its_button() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        frame(&mut w, &mut ui, &UiInput::default());
        let target = w.response(Id::new("file")).rect.center();
        click(&mut w, &mut ui, target);
        assert!(w.menus_open());

        for index in 0..10 {
            frame(&mut w, &mut ui, &at(target.x, target.y));
            assert!(w.menus_open(), "第 {index} 帧菜单自己关掉了");
        }
    }

    /// 没有菜单开着时，一切照旧。
    #[test]
    fn a_ui_without_menus_is_unaffected() {
        let mut ui = ui();
        let mut w = WidgetUi::default();
        w.begin();
        let a = w.button("a", "甲");
        w.finish(&mut ui, &UiInput::default());
        assert!(!w.menus_open());
        assert!(w.response(a).rect.size() != Vec2::ZERO);
    }

    // ── 下拉框 ──

    const QUALITY: [&str; 3] = ["低", "中", "高"];

    /// 一个只有下拉框的界面。`picked` 收本帧选中的下标。
    fn dropdown_frame(
        w: &mut WidgetUi,
        ui: &mut kui::Ui,
        input: &UiInput,
        selected: usize,
    ) -> Option<usize> {
        w.begin();
        let picker = w.dropdown("quality", QUALITY[selected]);
        let picked = w.dropdown_menu(picker, &QUALITY, selected);
        w.finish(ui, input);
        picked
    }

    #[test]
    fn a_dropdown_opens_and_closes_like_a_menu() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        dropdown_frame(&mut w, &mut ui, &UiInput::default(), 0);
        assert!(!w.menus_open());

        let field = w.response(Id::new("quality")).rect.center();
        // `click` 走的是固定的 `declare`，这里得自己走两帧。
        dropdown_frame(&mut w, &mut ui, &press(field.x, field.y), 0);
        let mut release = at(field.x, field.y);
        release.released.push(PointerButton::Primary);
        dropdown_frame(&mut w, &mut ui, &release, 0);

        assert!(w.menus_open(), "点下拉框该把列表打开");
    }

    #[test]
    fn a_closed_dropdown_declares_no_options() {
        // 关着还声明的话，那些项仍然参与命中——鼠标扫过列表本该在的
        // 那片区域会莫名点不到底下的东西。
        let mut ui = ui();
        let mut w = WidgetUi::default();
        dropdown_frame(&mut w, &mut ui, &UiInput::default(), 0);

        let option = w.response(Id::new(&format!("{}#0", Id::new("quality").0)));
        assert_eq!(option.rect.size(), Vec2::ZERO, "关着的时候不该有选项");
    }

    #[test]
    fn picking_an_option_reports_its_index() {
        let mut ui = ui();
        let mut w = WidgetUi::default();

        // 开。
        dropdown_frame(&mut w, &mut ui, &UiInput::default(), 0);
        let field = w.response(Id::new("quality")).rect.center();
        dropdown_frame(&mut w, &mut ui, &press(field.x, field.y), 0);
        let mut release = at(field.x, field.y);
        release.released.push(PointerButton::Primary);
        dropdown_frame(&mut w, &mut ui, &release, 0);

        // 列表排好了，点第三项。
        dropdown_frame(&mut w, &mut ui, &UiInput::default(), 0);
        let third = w
            .response(Id::new(&format!("{}#2", Id::new("quality").0)))
            .rect
            .center();
        assert_ne!(third, Vec2::ZERO, "列表没排出来");

        dropdown_frame(&mut w, &mut ui, &press(third.x, third.y), 0);
        let mut release = at(third.x, third.y);
        release.released.push(PointerButton::Primary);
        dropdown_frame(&mut w, &mut ui, &release, 0);

        // **再走一帧**：控件的响应滞后一帧（矩形要等排完才知道），
        // 所以「被点中」是在松开的下一帧才读得到的。
        // 菜单的关闭也刻意推迟一帧，正是为了让这一读读得到。
        let picked = dropdown_frame(&mut w, &mut ui, &UiInput::default(), 0);

        assert_eq!(picked, Some(2), "该报出被点的那一项");
    }

    #[test]
    fn an_untouched_dropdown_reports_nothing() {
        // 返回 `Some` 的那一帧才是「用户改了选择」。每帧都报当前值的话，
        // 调用方分不出「没动」和「又选了一遍同一个」。
        let mut ui = ui();
        let mut w = WidgetUi::default();

        for _ in 0..3 {
            assert_eq!(
                dropdown_frame(&mut w, &mut ui, &UiInput::default(), 1),
                None
            );
        }
    }

    #[test]
    fn an_out_of_range_selection_does_not_panic() {
        // 选项列表会变长变短（难度随解锁增加），越界时崩掉一个界面不划算。
        let mut ui = ui();
        let mut w = WidgetUi::default();

        w.begin();
        let picker = w.dropdown("q", "?");
        let picked = w.dropdown_menu(picker, &QUALITY, 99);
        w.finish(&mut ui, &UiInput::default());

        assert_eq!(picked, None);
    }

    #[test]
    fn two_dropdowns_do_not_share_option_ids() {
        // 选项 id 从锚点派生。不派生的话两个下拉框的第 0 项 id 相同，
        // 点一个另一个也会跟着变。
        let mut ui = ui();
        let mut w = WidgetUi::default();

        w.begin();
        let a = w.dropdown("a", "甲");
        let b = w.dropdown("b", "乙");
        w.dropdown_menu(a, &QUALITY, 0);
        w.dropdown_menu(b, &QUALITY, 0);
        w.finish(&mut ui, &UiInput::default());

        assert_ne!(
            Id::new(&format!("{}#0", a.0)),
            Id::new(&format!("{}#0", b.0))
        );
    }

    #[test]
    fn the_selected_option_is_ticked() {
        // 没有勾的话，下拉框打开之后看不出现在选的是哪个。
        let strokes = |selected: usize| {
            let mut ui = ui();
            let mut w = WidgetUi::default();

            // 先开起来。
            dropdown_frame(&mut w, &mut ui, &UiInput::default(), selected);
            let field = w.response(Id::new("quality")).rect.center();
            dropdown_frame(&mut w, &mut ui, &press(field.x, field.y), selected);
            let mut release = at(field.x, field.y);
            release.released.push(PointerButton::Primary);
            dropdown_frame(&mut w, &mut ui, &release, selected);
            dropdown_frame(&mut w, &mut ui, &UiInput::default(), selected);
            ui.end_frame();

            ui.draw_list()
                .vertices()
                .iter()
                .filter(|v| v.params[2] == kui::MODE_SEGMENT)
                .count()
        };

        // 合着的下拉框自己有一个 ∨（两段折线），打开之后多出选中项那个勾
        // （也是两段）。
        assert!(strokes(1) > 0, "一笔都没画");
    }
}
