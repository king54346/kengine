//! G-code（3D 打印机 / CNC 的刀路）。产物是一串按层分好的线段。
//!
//! 读的是打印机真正关心的那几条指令：
//!
//! | 指令 | 含义 |
//! |---|---|
//! | `G0` / `G1` | 直线移动；`E` 增加表示这一段在挤出 |
//! | `G2` / `G3` | 顺 / 逆时针圆弧（`I` `J` 圆心偏移或 `R` 半径） |
//! | `G90` / `G91` | 绝对 / 相对坐标 |
//! | `M82` / `M83` | 只把挤出轴切到绝对 / 相对 |
//! | `G92` | 重设当前位置（不移动） |
//! | `G20` / `G21` | 英寸 / 毫米 |
//!
//! # 和 three.js `GCodeLoader` 不同的两处
//!
//! - **挤出判定是逐段的**。three.js 的 `state.extruding` 一旦变真就不再
//!   变回假（它只在 `E` 增加时赋值），于是第一次挤出之后的所有空走都被
//!   画成了挤出线。这里每段各自判断。
//! - **圆弧会被细分成折线**，而 three.js 直接丢弃 `G2` / `G3`。
//!
//! 分层规则沿用 three.js：**在一个新的 Z 上开始挤出**才算新的一层，
//! 单纯的抬刀（Z 变了但没挤出）不算。

use crate::{bad, limits, loader};
use kasset::{LoadError, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kgizmo::{Color, LineSet, LineSetBuilder};
use kmath::Vec3;
use std::{path::PathBuf, sync::Arc};

/// [`GCode`] 的资源类型标识。
pub const GCODE_TYPE_UUID: Uuid = uuid!("5b1c2e7a-93d4-4f60-8a2b-e1c7d09f3a61");

/// 一层：挤出的线段与空走的线段，各自两两成对。
#[derive(Debug, Clone, Default)]
pub struct Layer {
    /// 这一层开始挤出时的 Z（打印机坐标，毫米）。
    pub z: f32,
    /// 挤出段，`[起点, 终点]`。
    pub extrusion: Vec<[Vec3; 2]>,
    /// 空走段。
    pub travel: Vec<[Vec3; 2]>,
}

/// 解析好的一份 G-code。坐标是打印机坐标系（Z 朝上，单位毫米）。
#[derive(Debug, Clone, Default)]
pub struct GCode {
    /// 按出现顺序的各层。
    pub layers: Vec<Layer>,
}

impl ResourceData for GCode {
    fn type_uuid(&self) -> Uuid {
        GCODE_TYPE_UUID
    }
}

impl GCode {
    /// 挤出段总数。
    pub fn extrusion_count(&self) -> usize {
        self.layers.iter().map(|l| l.extrusion.len()).sum()
    }

    /// 空走段总数。
    pub fn travel_count(&self) -> usize {
        self.layers.iter().map(|l| l.travel.len()).sum()
    }

    /// 全部线段的包围盒。
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for layer in &self.layers {
            for [a, b] in layer.extrusion.iter().chain(&layer.travel) {
                min = min.min(a.min(*b));
                max = max.max(a.max(*b));
            }
        }
        if min.x > max.x { (Vec3::ZERO, Vec3::ZERO) } else { (min, max) }
    }

    /// 前 `layers` 层建成一个常驻线段集（挤出绿、空走红，和 three.js 一样）。
    ///
    /// 坐标换成 Y 朝上：打印机的 Z 是高度。
    pub fn to_line_set(&self, layers: usize, travel: bool) -> LineSet {
        let convert = |p: Vec3| Vec3::new(p.x, p.z, -p.y);
        let mut builder = LineSetBuilder::default();
        for layer in self.layers.iter().take(layers) {
            for [a, b] in &layer.extrusion {
                builder.line(convert(*a), convert(*b), Color::rgb(0.0, 1.0, 0.0));
            }
            if travel {
                for [a, b] in &layer.travel {
                    builder.line(convert(*a), convert(*b), Color::rgb(1.0, 0.0, 0.0).with_alpha(0.35));
                }
            }
        }
        builder.build()
    }
}

loader! {
    /// 读 `.gcode` / `.gco` / `.nc`。
    GCodeLoader -> GCode : ["gcode", "gco", "nc"] = GCODE_TYPE_UUID, parse
}

/// 解析 G-code。
pub async fn parse(bytes: Vec<u8>, _path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<GCode, LoadError> {
    parse_text(&String::from_utf8_lossy(&bytes))
}

#[derive(Clone, Copy)]
struct State {
    position: Vec3,
    e: f32,
    relative: bool,
    /// `Some(相对?)`：M82 / M83 单独指定了挤出轴。
    extrusion_relative: Option<bool>,
    scale: f32,
}

/// 从文本解析。
pub fn parse_text(text: &str) -> Result<GCode, LoadError> {
    let mut state = State {
        position: Vec3::ZERO,
        e: 0.0,
        relative: false,
        extrusion_relative: None,
        scale: 1.0,
    };
    let mut code = GCode::default();
    let mut segments = 0usize;

    for (number, raw) in text.lines().enumerate() {
        if number > limits::LINES {
            return Err(bad("G-code 行数超过上限"));
        }
        // `;` 之后与 `( ... )` 里都是注释。
        let line = raw.split(';').next().unwrap_or("");
        let line = strip_parenthesized(line);
        let mut tokens = line.split_ascii_whitespace();
        let Some(command) = tokens.next() else { continue };
        let command = command.to_ascii_uppercase();
        // N 行号前缀：`N10 G1 X...`。
        let command = if command.starts_with('N') {
            match tokens.next() {
                Some(next) => next.to_ascii_uppercase(),
                None => continue,
            }
        } else {
            command
        };
        let mut args = [None::<f32>; 26];
        for token in tokens {
            let mut chars = token.chars();
            let Some(letter) = chars.next().filter(char::is_ascii_alphabetic) else { continue };
            if let Ok(value) = chars.as_str().parse::<f32>() {
                args[(letter.to_ascii_uppercase() as u8 - b'A') as usize] = Some(value);
            }
        }
        let arg = |c: char| args[(c as u8 - b'A') as usize];

        match command.as_str() {
            "G0" | "G00" | "G1" | "G01" | "G2" | "G02" | "G3" | "G03" => {
                let axis = |current: f32, value: Option<f32>| match value {
                    Some(v) if state.relative => current + v * state.scale,
                    Some(v) => v * state.scale,
                    None => current,
                };
                let target = Vec3::new(
                    axis(state.position.x, arg('X')),
                    axis(state.position.y, arg('Y')),
                    axis(state.position.z, arg('Z')),
                );
                let e = match arg('E') {
                    Some(v) if state.extrusion_relative.unwrap_or(state.relative) => state.e + v,
                    Some(v) => v,
                    None => state.e,
                };
                let extruding = e - state.e > 1e-7;

                if extruding {
                    match code.layers.last_mut() {
                        // 还没挤出过的「层」只是开头的空走，直接认领成这一层。
                        Some(layer) if layer.extrusion.is_empty() => layer.z = target.z,
                        Some(layer) if (layer.z - target.z).abs() <= 1e-6 => {}
                        _ => code.layers.push(Layer {
                            z: target.z,
                            ..Default::default()
                        }),
                    }
                }
                if code.layers.is_empty() {
                    code.layers.push(Layer {
                        z: state.position.z,
                        ..Default::default()
                    });
                }
                let layer = code.layers.last_mut().expect("刚保证过非空");
                let list = if extruding { &mut layer.extrusion } else { &mut layer.travel };

                let before = list.len();
                let arc = matches!(command.as_str(), "G2" | "G02" | "G3" | "G03");
                if arc {
                    let clockwise = matches!(command.as_str(), "G2" | "G02");
                    let points = arc_points(state.position, target, arg('I'), arg('J'), arg('R'), clockwise, state.scale);
                    let mut previous = state.position;
                    for point in points {
                        list.push([previous, point]);
                        previous = point;
                    }
                } else if target != state.position {
                    list.push([state.position, target]);
                }
                segments += list.len() - before;
                if segments > limits::VERTICES / 2 {
                    return Err(bad("G-code 线段数超过上限"));
                }
                state.position = target;
                state.e = e;
            }
            "G90" => {
                state.relative = false;
                state.extrusion_relative = None;
            }
            "G91" => {
                state.relative = true;
                state.extrusion_relative = None;
            }
            "M82" => state.extrusion_relative = Some(false),
            "M83" => state.extrusion_relative = Some(true),
            "G20" => state.scale = 25.4,
            "G21" => state.scale = 1.0,
            "G92" => {
                if let Some(x) = arg('X') { state.position.x = x * state.scale; }
                if let Some(y) = arg('Y') { state.position.y = y * state.scale; }
                if let Some(z) = arg('Z') { state.position.z = z * state.scale; }
                if let Some(e) = arg('E') { state.e = e; }
                // 什么参数都不带的 G92 把所有轴清零。
                if args.iter().all(Option::is_none) {
                    state.position = Vec3::ZERO;
                    state.e = 0.0;
                }
            }
            _ => {}
        }
    }
    code.layers.retain(|l| !l.extrusion.is_empty() || !l.travel.is_empty());
    Ok(code)
}

fn strip_parenthesized(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 把一段 XY 平面上的圆弧细分成点（不含起点，含终点）。Z 线性插值（螺旋）。
fn arc_points(from: Vec3, to: Vec3, i: Option<f32>, j: Option<f32>, r: Option<f32>, clockwise: bool, scale: f32) -> Vec<Vec3> {
    let start = from.truncate();
    let end = to.truncate();
    let center = match (i, j, r) {
        (i, j, _) if i.is_some() || j.is_some() => start + kmath::Vec2::new(i.unwrap_or(0.0), j.unwrap_or(0.0)) * scale,
        (_, _, Some(radius)) => {
            // R 形式：两个候选圆心，R 为负取大弧。
            let radius = radius * scale;
            let chord = end - start;
            let d = chord.length();
            if d < 1e-9 || d > 2.0 * radius.abs() + 1e-4 {
                return vec![to];
            }
            let h = (radius * radius - d * d / 4.0).max(0.0).sqrt();
            let mid = (start + end) / 2.0;
            let perp = kmath::Vec2::new(-chord.y, chord.x) / d;
            let left = (radius > 0.0) != clockwise;
            if left { mid + perp * h } else { mid - perp * h }
        }
        _ => return vec![to],
    };
    let a0 = (start - center).to_angle();
    let mut a1 = (end - center).to_angle();
    let radius = (start - center).length();
    if clockwise {
        if a1 >= a0 - 1e-6 { a1 -= std::f32::consts::TAU; }
    } else if a1 <= a0 + 1e-6 {
        a1 += std::f32::consts::TAU;
    }
    let sweep = a1 - a0;
    let steps = ((sweep.abs() * radius / 0.5).ceil() as usize).clamp(2, 256);
    (1..=steps)
        .map(|k| {
            let t = k as f32 / steps as f32;
            let angle = a0 + sweep * t;
            let p = center + kmath::Vec2::from_angle(angle) * radius;
            Vec3::new(p.x, p.y, from.z + (to.z - from.z) * t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travel_after_extrusion_is_still_travel() {
        let code = parse_text("G90\nM83\nG1 Z0.2\nG1 X10 E1\nG0 X20\nG1 X30 E1\n").unwrap();
        assert_eq!(code.layers.len(), 1);
        assert_eq!(code.extrusion_count(), 2);
        // G1 Z0.2（空走）+ G0 X20（空走）。
        assert_eq!(code.travel_count(), 2);
    }

    #[test]
    fn a_new_layer_starts_when_extruding_at_a_new_height() {
        let code = parse_text("G1 Z0.2\nG1 X1 E1\nG1 Z0.4\nG1 X2 E2\n").unwrap();
        assert_eq!(code.layers.len(), 2);
        assert!((code.layers[1].z - 0.4).abs() < 1e-6);
    }

    #[test]
    fn arcs_are_subdivided_and_end_on_target() {
        let code = parse_text("G1 X10 Y0 E0\nG3 X-10 Y0 I-10 J0 E1\n").unwrap();
        let arc = &code.layers.last().unwrap().extrusion;
        assert!(arc.len() > 4);
        let end = arc.last().unwrap()[1];
        assert!((end - Vec3::new(-10.0, 0.0, 0.0)).length() < 1e-3);
        // 逆时针从 (10,0) 到 (-10,0)：经过 y > 0 的上半圆。
        assert!(arc.iter().all(|s| s[1].y >= -1e-3));
    }

    #[test]
    fn relative_mode_and_comments() {
        let code = parse_text("G91 ; relative\nG1 X1 E1 (move)\nG1 X1 E1\n").unwrap();
        let last = code.layers[0].extrusion.last().unwrap();
        assert!((last[1].x - 2.0).abs() < 1e-6);
    }
}
