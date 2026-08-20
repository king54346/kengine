//! kfont — 字体加载、字形光栅化、文字排版
//!
//! 分四层，每层都能单独测：
//!
//! | 模块 | 干什么 | 依赖 |
//! |---|---|---|
//! | [`linebreak`] | 哪里可以换行（含 CJK 禁则） | 无 |
//! | [`layout`] | 把字符摆成一行行带坐标的字形 | 只认 [`Metrics`] trait |
//! | [`atlas`] | 字形图集，满了按 LRU 驱逐 | ktexture |
//! | [`font`] | 加载与光栅化 | ab_glyph |
//!
//! **排版不认识字体**：它只通过 [`Metrics`] 拿宽度和行高。于是断行、对齐、
//! 溢出这些最容易错的逻辑能用一个假字体测，仓库里不必塞字体，CI 上也不必
//! 碰系统字体。
//!
//! # 引擎不自带字体
//!
//! 覆盖中文的字体动辄十几兆。[`Font::from_bytes`] 只认字节，字体从哪来
//! 由调用方决定；[`system_font`] 提供一条找系统字体的方便路径。
//!
//! # 用法
//!
//! ```no_run
//! use kfont::{Font, FontStack, GlyphAtlas, TextStyle, layout};
//!
//! let mut fonts = FontStack::new();
//! fonts.push(Font::from_file("some.ttf")?);
//!
//! let style = TextStyle { size: 24.0, ..Default::default() };
//! let text = layout("中英混排 mixed", &style, &fonts.metrics(style.size), Some(300.0));
//!
//! // 排完再把用到的字形塞进图集。
//! let mut atlas = GlyphAtlas::new(1024);
//! atlas.begin_frame();
//! for glyph in &text.glyphs {
//!     let _ = fonts.ensure_glyph(&mut atlas, glyph.c, style.size);
//! }
//! # Ok::<(), kfont::FontError>(())
//! ```

pub mod atlas;
pub mod font;
pub mod layout;
pub mod linebreak;

pub use atlas::{AtlasError, GlyphAtlas, GlyphEntry, GlyphKey};
pub use font::{Font, FontError, FontStack, StackMetrics, system_font};
pub use layout::{Align, LineInfo, Metrics, PositionedGlyph, TextLayout, TextStyle, Wrap, layout};
pub use linebreak::{BreakClass, BreakOpportunity, break_class, break_opportunities, is_ideographic};
