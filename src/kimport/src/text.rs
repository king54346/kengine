//! 立体文字：TTF / OTF 字形轮廓 → 拉伸（可带倒角）的网格。
//!
//! three.js 的 `TTFLoader` + `TextGeometry`。字形轮廓从 [`kfont`] 取
//! （它是引擎里唯一碰字体解析库的地方），展平、按嵌套分出洞，再交给
//! [`path::extrude_shapes`](crate::path::extrude_shapes) 拉伸。
//!
//! ```no_run
//! use kimport::text::{TextGeometry, text_mesh};
//!
//! let font = kfont::Font::from_file("fonts/ttf/kenpixel.ttf").unwrap();
//! let mesh = text_mesh(&font, "three.js", &TextGeometry { size: 70.0, depth: 20.0, ..Default::default() });
//! ```
//!
//! # 字号
//!
//! 和 three.js 对齐：它的 `TTFLoader` 把字形按「1 em = 100/72 个 size」
//! 换算（字号按磅、屏幕按 72 dpi），同样的 `size` 两边出来的字一样大。

use crate::path::{Bevel, Contour, Shape, extrude_shapes, shapes_from_contours};
use kfont::{Font, OutlineCurve};
use kmath::Vec2;
use kmesh::Mesh;

/// 文字几何的参数（three.js `TextGeometry` 的那一组）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextGeometry {
    /// 字号。
    pub size: f32,
    /// 拉伸深度。
    pub depth: f32,
    /// 每段曲线细分几段。
    pub curve_segments: u32,
    /// 倒角；`None` 是直边。
    pub bevel: Option<Bevel>,
}

impl Default for TextGeometry {
    fn default() -> Self {
        Self {
            size: 100.0,
            depth: 50.0,
            curve_segments: 12,
            bevel: None,
        }
    }
}

/// 字形轮廓 → 闭合折线（已经缩放、平移到 `origin`）。
fn glyph_contours(
    curves: &[OutlineCurve],
    scale: f32,
    origin: Vec2,
    segments: u32,
) -> Vec<Contour> {
    let segments = segments.max(1);
    let point = |p: [f32; 2]| origin + Vec2::new(p[0], p[1]) * scale;
    let mut contours: Vec<Contour> = Vec::new();
    let mut current: Vec<Vec2> = Vec::new();
    let mut last_end: Option<[f32; 2]> = None;
    let finish = |current: &mut Vec<Vec2>, contours: &mut Vec<Contour>| {
        // 相邻重复点（零长度的收尾线段）会让倒角方向算不出来。
        current.dedup_by(|a, b| a.distance_squared(*b) < 1e-10);
        while current.len() > 1 && current[0].distance_squared(current[current.len() - 1]) < 1e-10 {
            current.pop();
        }
        if current.len() >= 3 {
            contours.push(Contour {
                points: std::mem::take(current),
                closed: true,
            });
        }
        current.clear();
    };
    for curve in curves {
        if last_end != Some(curve.start()) {
            finish(&mut current, &mut contours);
            current.push(point(curve.start()));
        }
        match *curve {
            OutlineCurve::Line(_, b) => current.push(point(b)),
            OutlineCurve::Quad(a, c, b) => {
                let (a, c, b) = (point(a), point(c), point(b));
                for s in 1..=segments {
                    let t = s as f32 / segments as f32;
                    let u = 1.0 - t;
                    current.push(a * (u * u) + c * (2.0 * u * t) + b * (t * t));
                }
            }
            OutlineCurve::Cubic(a, c1, c2, b) => {
                let (a, c1, c2, b) = (point(a), point(c1), point(c2), point(b));
                for s in 1..=segments {
                    let t = s as f32 / segments as f32;
                    let u = 1.0 - t;
                    current.push(
                        a * (u * u * u)
                            + c1 * (3.0 * u * u * t)
                            + c2 * (3.0 * u * t * t)
                            + b * (t * t * t),
                    );
                }
            }
        }
        last_end = Some(curve.end());
    }
    finish(&mut current, &mut contours);
    contours
}

/// 排版一段文字（支持 `\n` 换行），返回所有字形的形状。基线在 y = 0，
/// 第一行从 x = 0 开始，往下每行一个行高。
pub fn text_shapes(font: &Font, text: &str, size: f32, curve_segments: u32) -> Vec<Shape> {
    let scale = size * (100.0 / 72.0) / font.units_per_em();
    let (ascent, descent, gap) = font.line_metrics_unscaled();
    let line_height = (ascent - descent + gap) * scale;
    let mut shapes = Vec::new();
    let mut cursor = Vec2::ZERO;
    let mut previous: Option<char> = None;
    for c in text.chars() {
        if c == '\n' {
            cursor = Vec2::new(0.0, cursor.y - line_height);
            previous = None;
            continue;
        }
        if let Some(p) = previous {
            cursor.x += font.kern_unscaled(p, c) * scale;
        }
        let contours = glyph_contours(&font.glyph_outline(c), scale, cursor, curve_segments);
        shapes.extend(shapes_from_contours(contours));
        cursor.x += font.advance_unscaled(c) * scale;
        previous = Some(c);
    }
    shapes
}

/// 一段文字的立体网格（平面着色）。
pub fn text_mesh(font: &Font, text: &str, options: &TextGeometry) -> Mesh {
    let shapes = text_shapes(font, text, options.size, options.curve_segments);
    extrude_shapes(&shapes, options.depth, options.bevel)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/threejs/fonts/ttf/kenpixel.ttf"
    );

    fn font() -> Option<Font> {
        Font::from_file(FONT).ok()
    }

    #[test]
    fn glyph_outlines_are_y_up_and_scaled_like_three_js() {
        let Some(font) = font() else { return };
        let shapes = text_shapes(&font, "T", 72.0, 4);
        assert!(!shapes.is_empty());
        let max_y = shapes
            .iter()
            .flat_map(|s| s.outer.points.iter())
            .map(|p| p.y)
            .fold(f32::MIN, f32::max);
        let min_y = shapes
            .iter()
            .flat_map(|s| s.outer.points.iter())
            .map(|p| p.y)
            .fold(f32::MAX, f32::min);
        // 大写字母站在基线上、往上长，高度是 em 的一大截（72 磅 → 100 单位的 em）。
        assert!(min_y > -1.0, "T 不该伸到基线以下：{min_y}");
        assert!(max_y > 40.0 && max_y < 110.0, "T 的高度 {max_y}");
    }

    #[test]
    fn contours_do_not_repeat_their_first_point() {
        let Some(font) = font() else { return };
        // 像素字体的「o」是四根条拼的，没有洞；每根条是干净的四个角。
        let shapes = text_shapes(&font, "o", 70.0, 4);
        assert_eq!(shapes.len(), 4);
        for shape in &shapes {
            assert_eq!(shape.outer.points.len(), 4, "{:?}", shape.outer.points);
        }
    }

    #[test]
    fn the_cursor_advances_and_newlines_move_down() {
        let Some(font) = font() else { return };
        let one = text_shapes(&font, "a", 70.0, 4);
        let two = text_shapes(&font, "aa", 70.0, 4);
        let right = |shapes: &[Shape]| {
            shapes
                .iter()
                .flat_map(|s| s.outer.points.iter())
                .map(|p| p.x)
                .fold(f32::MIN, f32::max)
        };
        assert!(right(&two) > right(&one) + 10.0);
        let lines = text_shapes(&font, "a\na", 70.0, 4);
        let bottom = lines
            .iter()
            .flat_map(|s| s.outer.points.iter())
            .map(|p| p.y)
            .fold(f32::MAX, f32::min);
        assert!(bottom < -40.0, "第二行应该在下面：{bottom}");
    }

    #[test]
    fn the_mesh_spans_the_bevelled_depth() {
        let Some(font) = font() else { return };
        let options = TextGeometry {
            size: 70.0,
            depth: 20.0,
            curve_segments: 4,
            bevel: Some(Bevel {
                thickness: 2.0,
                size: 1.5,
                offset: 0.0,
                segments: 3,
            }),
        };
        let mesh = text_mesh(&font, "three.js", &options);
        let aabb = mesh.aabb();
        assert!((aabb.min.z + 2.0).abs() < 1e-3 && (aabb.max.z - 22.0).abs() < 1e-3);
    }
}
