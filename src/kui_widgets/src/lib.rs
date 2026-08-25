//! kui_widgets —— 现成的界面控件：按钮、复选框、滑条、文本框、
//! 滚动区、可折叠分组。
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
//! **两个例外**，和 `bevy_ui_widgets` 的取舍一致：
//!
//! - **文本框**的状态又大又贵（光标、选区、输入法合成），每帧从外面
//!   搬进搬出不划算，所以它自己存。
//! - **折叠分组**纯粹是外观，和游戏逻辑无关。让调用方为每个分组存一个
//!   bool，只会让每个面板都多出一堆和业务无关的字段。

#![warn(missing_docs)]

/// 对话框的位置：拖动、夹进屏幕、居中。
pub mod dialog;
/// 菜单的键盘导航：方向键跳到哪一项。
pub mod menu;
/// 浮层定位：下拉框、菜单、提示气泡的摆放。
pub mod popover;
pub mod text_edit;
pub mod widgets;

pub use text_edit::TextEdit;
pub use widgets::{Theme, WidgetUi};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{TextEdit, Theme, WidgetUi};
    pub use kui::{AlignCross, Direction, Edges, Id, Justify, Length, Response, Style, UiInput};
}
