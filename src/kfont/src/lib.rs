//! kfont — 字体加载、字形光栅化、文字排版
pub mod atlas;
pub mod layout;
pub mod linebreak;

pub use atlas::{AtlasError, GlyphAtlas, GlyphEntry, GlyphKey};
pub use layout::{Align, LineInfo, Metrics, PositionedGlyph, TextLayout, TextStyle, Wrap, layout};
