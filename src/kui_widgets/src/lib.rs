//! kui_widgets —— 现成的界面控件：按钮、复选框、单选按钮、滑条、
//! 文本框、列表、滚动区/滚动条、可折叠分组、遮罩、对话框、菜单。
//!
//! # 画的和算的分开
//!
//! 每个控件里最容易出错的都不是「画成什么样」，而是那点算术：浮层在
//! 屏幕边上要翻到哪一侧、方向键该跳过哪些项、拖出屏幕之后夹到哪。
//! 这些**完全不需要窗口、字体、GPU**，所以一律做成纯函数单独放：
//!
//! | 模块 | 算什么 |
//! |---|---|
//! | [`popover`] | 浮层摆在哪：翻面、夹进屏幕 |
//! | [`menu::navigate`] | 方向键跳到哪一项：跳过禁用项、绕回 |
//! | [`dialog`] | 拖动后夹到哪：拖出屏幕要留一截抓得住 |
//! | [`Slider`] | 值域、步进、精度之间的换算 |
//! | [`list::ListAction`] | 一次点击要把选中集合改成什么样 |
//! | [`text_edit`] | 光标、选区、字符边界 |
//!
//! 拎出来之后每条规则都能直接写成测试，而不必先造一个窗口出来。
//!
//! # 为什么和 `kui` 分开
//!
//! 三层分工，对着 Bevy 的 `bevy_ui` / `bevy_ui_render` / `bevy_ui_widgets`：
//!
//! | 层 | 这里对应的 | 管什么 |
//! |---|---|---|
//! | 核心 | [`kui`] | 布局（taffy）、样式、绘制图元、命中测试、标记语言 |
//! | 渲染 | `krender::ui` | 把绘制列表传上显存、画到屏幕 |
//! | 控件 | 本 crate | 按钮、滑条这些**具体**的东西 |
//!
//! 分开的实际好处：只想画个血条或准星的项目，链进核心那一层就够了，
//! 不必把一整套控件、文本编辑、输入法处理一起背上。反过来，
//! 控件层能大改甚至整个换掉，而不动核心。
//!
//! 界面渲染早就在 `krender::ui` 里了——这次拆的是**控件**这一层。
//!
//! # 状态在调用方，不在控件里
//!
//! 和 `bevy_ui_widgets` 一样：
//!
//! ```ignore
//! let clicked = widgets.checkbox("sound", "音效", self.sound_on);
//! //                                              ^^^^^^^^^^^^^ 你保管
//! ```
//!
//! 存在控件里的话，同一个 id 在两处用就会互相覆盖，而且调用方没法
//! 直接读写——一个「重置全部设置」的按钮就没法实现了。
//!
//! **例外**都是「这份状态除了喂回给控件之外没有别的用处」的那种：
//!
//! - **文本框**的状态又大又贵（光标、选区、输入法合成），每帧从外面
//!   搬进搬出不划算，所以它自己存。和 `bevy_ui_widgets` 一致。
//! - **折叠分组**纯粹是外观，和游戏逻辑无关。让调用方为每个分组存一个
//!   bool，只会让每个面板都多出一堆和业务无关的字段。
//! - **菜单开着哪一条、高亮在第几项**同理，全是瞬态外观。
//! - **列表的锚点**（按住 Shift 从哪一项开始选）和**滑条的拖动起点**
//!   都只在一次操作中间有意义，操作一结束就该忘掉。
//!
//! 注意这些例外存的都**不是那份数据本身**：滑条的值、列表选中了哪些，
//! 仍然在调用方手里。
//!
//! # 键盘
//!
//! 能用鼠标做的都能用键盘做，而且**走同一条通道**：方向键在单选组里
//! 换选、在菜单里挪高亮，报告出来都是 `response.clicked`。所以已经写着
//!
//! ```ignore
//! if w.response(r).clicked { ... }
//! ```
//!
//! 的地方自动支持键盘，不必为它再写一条分支——分成两条的话，漏掉的
//! 那些就成了只有鼠标能用的控件，而这种漏很难被发现。
//!
//! Tab 走大块，方向键走块内：一组二十个单选按钮在 Tab 序列里只占**一站**
//! （[roving tabindex](kui::Hit::tab_stop)），进去之后用方向键挑。

#![warn(missing_docs)]

pub mod button;
pub mod checkbox;
pub mod dialog;
pub mod folder;
pub mod image;
pub mod label;
pub mod list;
pub mod menu;
pub mod modal;
pub mod panel;
pub mod popover;
pub mod radio;
pub mod scrollarea;
pub mod scrollbar;
pub mod slider;
#[cfg(test)]
mod testing;
pub mod text_edit;
pub mod text_input;
pub mod widgets;

pub use list::ListAction;
pub use menu::{Layout as MenuLayout, MenuAction, MenuKey};
pub use popover::{Align, Placement, Side};
pub use slider::{Orientation, Slider, TrackClick};
pub use text_edit::TextEdit;
pub use widgets::{Theme, WidgetUi};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        Align, ListAction, MenuAction, MenuKey, MenuLayout, Orientation, Placement, Side, Slider,
        TextEdit, Theme, TrackClick, WidgetUi,
    };
    pub use kui::{
        AlignCross, Direction, Edges, Id, Justify, Length, NavKey, Response, Style, UiInput,
    };
}
