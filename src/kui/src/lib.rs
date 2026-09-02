//! kui —— 界面**核心**：布局、样式、绘制图元、命中测试、标记语言。
//!
//! # 三层分工
//!
//! 对着 Bevy 的 `bevy_ui` / `bevy_ui_render` / `bevy_ui_widgets`：
//!
//! | 层 | 这里对应的 | 管什么 |
//! |---|---|---|
//! | 核心 | **本 crate** | 布局（taffy）、[`Style`]、[`DrawList`]、[`Interaction`]、[`markup`] |
//! | 渲染 | `krender::ui` | 把绘制列表传上显存、画到屏幕 |
//! | 控件 | `kui_widgets` | 按钮、复选框、滑条、文本框、滚动区 |
//!
//! 分开的实际好处：只想画个血条或准星的项目，链进这一层就够了，
//! 不必把一整套控件、文本编辑、输入法处理一起背上。反过来，控件层
//! 能大改甚至整个换掉，而不动核心。
//!
//! **这一层不认识「按钮」是什么**——它只知道矩形、文字、和一次命中测试。
//!
//! # 一张纹理画完一整个界面
//!
//! 字形图集左上角留了一块纯白，纯色图元采样那一点、文字采样自己的字形，
//! 两者共用一张纹理——一整个界面通常只要**一次绘制**。
//!
//! # 用法
//!
//! ```no_run
//! use kui::{Rect, Ui};
//! use kfont::{Font, TextStyle};
//! use kmath::{Vec2, Vec4};
//!
//! let mut ui = Ui::new();
//! ui.add_font(Font::from_file(kfont::system_font().unwrap())?);
//!
//! ui.begin_frame(Vec2::new(1280.0, 720.0), 1.0);
//! ui.rounded_rect(Rect::new(20.0, 20.0, 200.0, 48.0), 8.0, Vec4::new(0.2, 0.2, 0.25, 0.9));
//! ui.text_centered(
//!     Rect::new(20.0, 20.0, 200.0, 48.0),
//!     "开始游戏",
//!     &TextStyle { size: 20.0, ..Default::default() },
//!     Vec4::ONE,
//! );
//! ui.end_frame();
//! # Ok::<(), kfont::FontError>(())
//! ```

mod context;
pub mod draw;
pub mod interact;
/// 界面标记语言的解析。
pub mod markup;
/// 标记属性到样式的转换。
pub mod style_attr;

mod layout;

pub use context::Ui;
pub use draw::{DrawBatch, DrawList, MODE_RECT, MODE_SEGMENT, Rect, UiVertex};
pub use interact::{EditAction, Hit, Interaction, NavKey, PointerButton, Response, UiInput};
pub use layout::{
    AlignCross, Direction, Display, Edges, Id, Justify, LayoutNode, Length, MAX_DEPTH,
    MAX_GRID_COLUMNS, Solved, Style, Track, solve,
};
pub use style_attr::{Applied, Selector, StyleError, Visual};

/// UI 着色器源码，由 `krender` 编译。
pub const UI_WGSL: &str = include_str!("ui.wgsl");
