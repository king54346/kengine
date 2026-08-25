//! 菜单的键盘导航。
//!
//! 菜单里真正容易出错的不是画，是**按方向键该跳到哪一项**：中间夹着
//! 禁用项、要不要绕回开头、菜单全禁用时怎么办。所以这里只做这件事，
//! 做成不碰绘制的纯函数——不需要窗口、字体、GPU 就能全测一遍。
//!
//! 摆在哪里由 [`popover`](crate::popover) 算，画成什么样由
//! [`WidgetUi::list_item`](crate::WidgetUi::list_item) 出几何。
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
