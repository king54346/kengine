//! kui — 用户界面
//!
//! 目前是**即时模式的绘制层**：矩形、圆角、边框、裁剪、文字、贴图。
//! 布局（taffy）、事件路由、控件还没做，见 `next.md`。
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
pub mod layout;
pub mod widgets;

pub use context::Ui;
pub use draw::{DrawBatch, DrawList, Rect, UiVertex};
pub use interact::{Interaction, PointerButton, Response, UiInput};
pub use layout::{
    AlignCross, Direction, Edges, Id, Justify, LayoutNode, Length, MAX_DEPTH, Solved, Style, solve,
};
pub use widgets::{Theme, WidgetUi};

/// UI 着色器源码，由 `krender` 编译。
pub const UI_WGSL: &str = include_str!("ui.wgsl");
