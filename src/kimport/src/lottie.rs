//! Lottie（Bodymovin JSON）矢量动画 → 每帧一张 RGBA 画布。
//!
//! three.js 的例子是借浏览器里的 `lottie-web` 往 Canvas 上画，再把 Canvas
//! 当贴图用。这边没有浏览器，自己解析、自己求值、用 [`raster`](crate::raster)
//! 画：
//!
//! ```no_run
//! use kimport::lottie::Animation;
//!
//! let animation = Animation::parse(&std::fs::read("logo.json").unwrap()).unwrap();
//! let canvas = animation.render(42.0, 512, 512);
//! let texture = canvas.to_texture(true);
//! ```
//!
//! # 支持到哪
//!
//! - 图层：形状（4）、空（3，只当父级）、纯色（1）、预合成（0，带 `st`
//!   和 `sr` 时间偏移）；`ip` / `op` 显隐、`parent` 父子级联、轨道遮罩
//!   （`td` / `tt` 的四种模式）、`hd` 隐藏；
//! - 形状：组（`gr`）、贝塞尔路径（`sh`，可动画）、矩形（`rc`，含圆角）、
//!   椭圆（`el`）；填充（`fl`，非零 / 奇偶）、描边（`st`，端头、连接、
//!   斜接上限）、组变换（`tr`）、修剪路径（`tm`，「同时」和「逐个」两种）；
//! - 动画：关键帧的贝塞尔缓动、定格（`h`）、旧版的 `e` 终值写法、
//!   拆分的位置（`p.s`）；
//! - 变换：锚点、位置、缩放、旋转、不透明度。
//!
//! 绘制顺序和 After Effects 一致：图层列表里**靠前的在上面**，组里也是
//! 靠前的在上面；填充 / 描边作用于它之前的所有路径（含子组里的）。
//!
//! # 不支持的
//!
//! 渐变填充 / 描边（按第一个色标的颜色画）、蒙版（`masksProperties`）、
//! 合并路径、圆角 / 位移 / 中继器修饰器、星形、文字层、图片层、表达式、
//! 位置的空间贝塞尔（`ti` / `to`，按直线插值）、斜切。

use crate::path::{
    Contour, FillRule, LineCap, LineJoin, StrokeStyle, Tessellation, fill, stroke_styled,
};
use crate::raster::{Canvas, MatteMode};
use kasset::LoadError;
use kmath::{Affine2, Vec2};
use serde_json::Value;
use std::collections::HashMap;

// ── 可动画的属性 ──

#[derive(Debug, Clone)]
struct Key {
    time: f32,
    start: Vec<f32>,
    /// 旧版本把终值写在这一帧里；新版本用下一帧的 `s`。
    end: Option<Vec<f32>>,
    /// 缓出、缓入的贝塞尔控制点（x, y）。
    out_tangent: (f32, f32),
    in_tangent: (f32, f32),
    hold: bool,
}

#[derive(Debug, Clone)]
enum Prop {
    Static(Vec<f32>),
    Animated(Vec<Key>),
}

/// 一个值：数字、数字数组，或者形状 `{i, o, v, c}`。形状编码成
/// `[c, n, v.x, v.y, ..., i.x, i.y, ..., o.x, o.y, ...]`。
fn floats(value: &Value) -> Vec<f32> {
    match value {
        Value::Number(n) => vec![n.as_f64().unwrap_or(0.0) as f32],
        Value::Array(items) => {
            if let Some(first @ Value::Object(_)) = items.first() {
                return floats(first);
            }
            items
                .iter()
                .filter_map(Value::as_f64)
                .map(|v| v as f32)
                .collect()
        }
        Value::Object(map) if map.contains_key("v") => {
            let points = |key: &str| -> Vec<[f32; 2]> {
                map.get(key)
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .map(|p| {
                                let p = floats(p);
                                [
                                    p.first().copied().unwrap_or(0.0),
                                    p.get(1).copied().unwrap_or(0.0),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let (v, i, o) = (points("v"), points("i"), points("o"));
            let n = v.len();
            let closed = map.get("c").and_then(Value::as_bool).unwrap_or(false);
            let mut out = vec![if closed { 1.0 } else { 0.0 }, n as f32];
            for list in [&v, &i, &o] {
                for k in 0..n {
                    let p = list.get(k).copied().unwrap_or([0.0, 0.0]);
                    out.extend_from_slice(&p);
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn first_number(value: Option<&Value>, default: f32) -> f32 {
    value
        .map(floats)
        .and_then(|v| v.first().copied())
        .unwrap_or(default)
}

fn tangent(value: Option<&Value>) -> (f32, f32) {
    let get = |key: &str| first_number(value.and_then(|v| v.get(key)), 0.0);
    (get("x"), get("y"))
}

impl Prop {
    fn parse(value: Option<&Value>, default: &[f32]) -> Prop {
        let Some(value) = value else {
            return Prop::Static(default.to_vec());
        };
        let k = value.get("k").unwrap_or(value);
        let animated = value.get("a").and_then(Value::as_i64) == Some(1)
            || matches!(k, Value::Array(list) if list.first().is_some_and(|f| f.get("t").is_some()));
        if !animated {
            let v = floats(k);
            return Prop::Static(if v.is_empty() { default.to_vec() } else { v });
        }
        let keys: Vec<Key> = k
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|key| Key {
                        time: first_number(key.get("t"), 0.0),
                        start: key.get("s").map(floats).unwrap_or_default(),
                        end: key.get("e").map(floats),
                        out_tangent: tangent(key.get("o")),
                        in_tangent: tangent(key.get("i")),
                        hold: key.get("h").and_then(Value::as_i64) == Some(1),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if keys.is_empty() {
            Prop::Static(default.to_vec())
        } else {
            Prop::Animated(keys)
        }
    }

    fn at(&self, frame: f32) -> Vec<f32> {
        let keys = match self {
            Prop::Static(v) => return v.clone(),
            Prop::Animated(keys) => keys,
        };
        if frame <= keys[0].time {
            return keys[0].start.clone();
        }
        for pair in keys.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if frame < b.time {
                if a.hold {
                    return a.start.clone();
                }
                let end = a.end.as_ref().unwrap_or(&b.start);
                let span = (b.time - a.time).max(1e-6);
                let t = ease((frame - a.time) / span, a.out_tangent, a.in_tangent);
                return lerp(&a.start, end, t);
            }
        }
        let last = &keys[keys.len() - 1];
        if last.start.is_empty() {
            // 旧格式的最后一帧只有时间：值是倒数第二帧的终值。
            keys.iter()
                .rev()
                .find_map(|k| {
                    k.end
                        .clone()
                        .or_else(|| (!k.start.is_empty()).then(|| k.start.clone()))
                })
                .unwrap_or_default()
        } else {
            last.start.clone()
        }
    }

    fn scalar(&self, frame: f32) -> f32 {
        self.at(frame).first().copied().unwrap_or(0.0)
    }

    fn vec2(&self, frame: f32) -> Vec2 {
        let v = self.at(frame);
        Vec2::new(
            v.first().copied().unwrap_or(0.0),
            v.get(1).copied().unwrap_or(0.0),
        )
    }
}

fn lerp(a: &[f32], b: &[f32], t: f32) -> Vec<f32> {
    if a.len() != b.len() {
        return if t < 1.0 { a.to_vec() } else { b.to_vec() };
    }
    a.iter().zip(b).map(|(x, y)| x + (y - x) * t).collect()
}

/// 缓动：控制点为 (0,0)、`out`、`in`、(1,1) 的三次贝塞尔，按 x 求 y。
fn ease(x: f32, out: (f32, f32), inn: (f32, f32)) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let curve = |t: f32, p1: f32, p2: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    // 二分求 t：曲线在 x 上单调（AE 保证控制点 x 在 [0, 1]）。
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        if curve(mid, out.0.clamp(0.0, 1.0), inn.0.clamp(0.0, 1.0)) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    curve((lo + hi) * 0.5, out.1, inn.1)
}

// ── 变换 ──

#[derive(Debug, Clone)]
enum Position {
    Joined(Prop),
    Split(Prop, Prop),
}

#[derive(Debug, Clone)]
struct Transform {
    anchor: Prop,
    position: Position,
    scale: Prop,
    rotation: Prop,
    opacity: Prop,
}

impl Transform {
    fn parse(value: Option<&Value>) -> Self {
        let get = |key: &str| value.and_then(|v| v.get(key));
        let position = match get("p") {
            Some(p) if p.get("s").and_then(Value::as_bool) == Some(true) => Position::Split(
                Prop::parse(p.get("x"), &[0.0]),
                Prop::parse(p.get("y"), &[0.0]),
            ),
            other => Position::Joined(Prop::parse(other, &[0.0, 0.0])),
        };
        Self {
            anchor: Prop::parse(get("a"), &[0.0, 0.0]),
            position,
            scale: Prop::parse(get("s"), &[100.0, 100.0]),
            rotation: Prop::parse(get("r").or_else(|| get("rz")), &[0.0]),
            opacity: Prop::parse(get("o"), &[100.0]),
        }
    }

    fn matrix(&self, frame: f32) -> Affine2 {
        let position = match &self.position {
            Position::Joined(p) => p.vec2(frame),
            Position::Split(x, y) => Vec2::new(x.scalar(frame), y.scalar(frame)),
        };
        let scale = self.scale.vec2(frame) / 100.0;
        Affine2::from_translation(position)
            * Affine2::from_angle(self.rotation.scalar(frame).to_radians())
            * Affine2::from_scale(scale)
            * Affine2::from_translation(-self.anchor.vec2(frame))
    }

    fn opacity(&self, frame: f32) -> f32 {
        (self.opacity.scalar(frame) / 100.0).clamp(0.0, 1.0)
    }
}

// ── 形状 ──

#[derive(Debug, Clone)]
enum Item {
    Group(Vec<Item>),
    Path(Prop),
    Rect {
        position: Prop,
        size: Prop,
        roundness: Prop,
    },
    Ellipse {
        position: Prop,
        size: Prop,
    },
    Fill {
        color: Prop,
        opacity: Prop,
        rule: FillRule,
    },
    Stroke {
        color: Prop,
        opacity: Prop,
        width: Prop,
        style: StrokeStyle,
    },
    Transform(Transform),
    Trim {
        start: Prop,
        end: Prop,
        offset: Prop,
        individually: bool,
    },
}

fn parse_items(list: Option<&Value>, unsupported: &mut Vec<String>) -> Vec<Item> {
    let Some(list) = list.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in list {
        if item.get("hd").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let ty = item.get("ty").and_then(Value::as_str).unwrap_or("");
        let get = |key: &str| item.get(key);
        let parsed = match ty {
            "gr" => Item::Group(parse_items(get("it"), unsupported)),
            "sh" => Item::Path(Prop::parse(get("ks"), &[])),
            "rc" => Item::Rect {
                position: Prop::parse(get("p"), &[0.0, 0.0]),
                size: Prop::parse(get("s"), &[0.0, 0.0]),
                roundness: Prop::parse(get("r"), &[0.0]),
            },
            "el" => Item::Ellipse {
                position: Prop::parse(get("p"), &[0.0, 0.0]),
                size: Prop::parse(get("s"), &[0.0, 0.0]),
            },
            "fl" | "gf" => {
                if ty == "gf" {
                    unsupported.push("渐变填充（按第一个色标画）".into());
                }
                Item::Fill {
                    color: gradient_or_color(item),
                    opacity: Prop::parse(get("o"), &[100.0]),
                    rule: if first_number(get("r"), 1.0) == 2.0 {
                        FillRule::EvenOdd
                    } else {
                        FillRule::NonZero
                    },
                }
            }
            "st" | "gs" => {
                if ty == "gs" {
                    unsupported.push("渐变描边（按第一个色标画）".into());
                }
                let join = match first_number(get("lj"), 2.0) as i64 {
                    1 => LineJoin::Miter,
                    3 => LineJoin::Bevel,
                    _ => LineJoin::Round,
                };
                let cap = match first_number(get("lc"), 2.0) as i64 {
                    1 => LineCap::Butt,
                    3 => LineCap::Square,
                    _ => LineCap::Round,
                };
                Item::Stroke {
                    color: gradient_or_color(item),
                    opacity: Prop::parse(get("o"), &[100.0]),
                    width: Prop::parse(get("w"), &[1.0]),
                    style: StrokeStyle {
                        width: 1.0,
                        join,
                        cap,
                        miter_limit: first_number(get("ml"), 4.0),
                        round_segments: 8,
                    },
                }
            }
            "tr" => Item::Transform(Transform::parse(Some(item))),
            "tm" => Item::Trim {
                start: Prop::parse(get("s"), &[0.0]),
                end: Prop::parse(get("e"), &[100.0]),
                offset: Prop::parse(get("o"), &[0.0]),
                individually: first_number(get("m"), 1.0) == 2.0,
            },
            other => {
                unsupported.push(format!("形状 {other}"));
                continue;
            }
        };
        out.push(parsed);
    }
    out
}

/// 纯色的 `c`，或者渐变 `g.k` 的第一个色标（`[位置, r, g, b, ...]`）。
fn gradient_or_color(item: &Value) -> Prop {
    if let Some(gradient) = item.get("g") {
        let stops = Prop::parse(gradient.get("k"), &[0.0, 0.0, 0.0, 0.0]);
        let first = stops.at(0.0);
        return Prop::Static(vec![
            first.get(1).copied().unwrap_or(0.0),
            first.get(2).copied().unwrap_or(0.0),
            first.get(3).copied().unwrap_or(0.0),
            1.0,
        ]);
    }
    Prop::parse(item.get("c"), &[0.0, 0.0, 0.0, 1.0])
}

// ── 图层 ──

#[derive(Debug, Clone)]
struct Layer {
    index: Option<i64>,
    parent: Option<i64>,
    kind: i64,
    in_point: f32,
    out_point: f32,
    start_time: f32,
    stretch: f32,
    transform: Transform,
    items: Vec<Item>,
    /// 这个图层是下一层的遮罩（自己不画）。
    is_matte: bool,
    /// 用上一层当遮罩，怎么用。
    matte: Option<MatteMode>,
    hidden: bool,
    reference: Option<String>,
    solid: Option<([f32; 4], Vec2)>,
}

fn parse_layers(list: Option<&Value>, unsupported: &mut Vec<String>) -> Vec<Layer> {
    let Some(list) = list.and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .map(|layer| {
            let get = |key: &str| layer.get(key);
            let kind = get("ty").and_then(Value::as_i64).unwrap_or(-1);
            if !matches!(kind, 0 | 1 | 3 | 4) {
                unsupported.push(format!("图层类型 {kind}"));
            }
            if get("masksProperties")
                .and_then(Value::as_array)
                .is_some_and(|m| !m.is_empty())
            {
                unsupported.push("图层蒙版".into());
            }
            let solid = (kind == 1).then(|| {
                let color = get("sc")
                    .and_then(Value::as_str)
                    .and_then(|hex| u32::from_str_radix(hex.trim_start_matches('#'), 16).ok())
                    .unwrap_or(0);
                let rgb = [(color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff]
                    .map(|c| c as f32 / 255.0);
                (
                    [rgb[0], rgb[1], rgb[2], 1.0],
                    Vec2::new(first_number(get("sw"), 0.0), first_number(get("sh"), 0.0)),
                )
            });
            Layer {
                index: get("ind").and_then(Value::as_i64),
                parent: get("parent").and_then(Value::as_i64),
                kind,
                in_point: first_number(get("ip"), 0.0),
                out_point: first_number(get("op"), f32::MAX),
                start_time: first_number(get("st"), 0.0),
                stretch: first_number(get("sr"), 1.0).max(1e-6),
                transform: Transform::parse(get("ks")),
                items: parse_items(get("shapes"), unsupported),
                is_matte: first_number(get("td"), 0.0) != 0.0,
                matte: match first_number(get("tt"), 0.0) as i64 {
                    1 => Some(MatteMode::Alpha),
                    2 => Some(MatteMode::AlphaInverted),
                    3 => Some(MatteMode::Luma),
                    4 => Some(MatteMode::LumaInverted),
                    _ => None,
                },
                hidden: get("hd").and_then(Value::as_bool) == Some(true),
                reference: get("refId").and_then(Value::as_str).map(str::to_string),
                solid,
            }
        })
        .collect()
}

/// 一段解析好的 Lottie 动画。
#[derive(Debug, Clone)]
pub struct Animation {
    /// 合成宽（像素）。
    pub width: f32,
    /// 合成高。
    pub height: f32,
    /// 帧率。
    pub frame_rate: f32,
    /// 起始帧。
    pub in_point: f32,
    /// 结束帧（不含）。
    pub out_point: f32,
    layers: Vec<Layer>,
    assets: HashMap<String, Vec<Layer>>,
    /// 用到了但不支持的特性（去重），导入时打一条警告。
    pub unsupported: Vec<String>,
}

impl Animation {
    /// 解析 JSON。
    pub fn parse(bytes: &[u8]) -> Result<Animation, LoadError> {
        let root: Value = serde_json::from_slice(bytes)
            .map_err(|e| crate::bad(format!("Lottie JSON 解析失败：{e}")))?;
        if root.get("layers").is_none() {
            return Err(crate::bad("不是 Lottie 动画（缺 layers）"));
        }
        let mut unsupported = Vec::new();
        let layers = parse_layers(root.get("layers"), &mut unsupported);
        let mut assets = HashMap::new();
        for asset in root
            .get("assets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(id), Some(_)) =
                (asset.get("id").and_then(Value::as_str), asset.get("layers"))
            {
                assets.insert(
                    id.to_string(),
                    parse_layers(asset.get("layers"), &mut unsupported),
                );
            }
        }
        unsupported.sort();
        unsupported.dedup();
        if !unsupported.is_empty() {
            klog::warn!("Lottie 用到了不支持的特性：{}", unsupported.join("、"));
        }
        Ok(Animation {
            width: first_number(root.get("w"), 512.0),
            height: first_number(root.get("h"), 512.0),
            frame_rate: first_number(root.get("fr"), 30.0).max(1.0),
            in_point: first_number(root.get("ip"), 0.0),
            out_point: first_number(root.get("op"), 60.0),
            layers,
            assets,
            unsupported,
        })
    }

    /// 总帧数。
    pub fn frame_count(&self) -> f32 {
        (self.out_point - self.in_point).max(1.0)
    }

    /// 循环播放时，第 `seconds` 秒对应的帧。
    pub fn frame_at(&self, seconds: f32) -> f32 {
        self.in_point + (seconds * self.frame_rate).rem_euclid(self.frame_count())
    }

    /// 画第 `frame` 帧，整个合成缩放到 `width × height`。
    pub fn render(&self, frame: f32, width: usize, height: usize) -> Canvas {
        let mut canvas = Canvas::new(width, height);
        self.render_into(frame, &mut canvas);
        canvas
    }

    /// 画进一张现成的画布（先清空）。每帧都画的话复用画布省掉分配。
    pub fn render_into(&self, frame: f32, canvas: &mut Canvas) {
        canvas.clear();
        let fit = Affine2::from_scale(Vec2::new(
            canvas.width() as f32 / self.width.max(1.0),
            canvas.height() as f32 / self.height.max(1.0),
        ));
        self.render_layers(&self.layers, frame, fit, canvas, 0);
    }

    fn render_layers(
        &self,
        layers: &[Layer],
        frame: f32,
        base: Affine2,
        out: &mut Canvas,
        depth: usize,
    ) {
        if depth > 8 {
            return;
        }
        let mut layer_canvas = Canvas::new(out.width(), out.height());
        let mut matte_canvas: Option<Canvas> = None;
        // 列表靠前的在上面：倒着画。
        for i in (0..layers.len()).rev() {
            let layer = &layers[i];
            if layer.is_matte || layer.hidden || !visible(layer, frame) {
                continue;
            }
            layer_canvas.clear();
            self.draw_layer(layers, i, frame, base, &mut layer_canvas, depth);
            let opacity = layer.transform.opacity(local_frame(layer, frame));
            match (layer.matte, i.checked_sub(1).map(|j| &layers[j])) {
                (Some(mode), Some(source)) => {
                    let matte =
                        matte_canvas.get_or_insert_with(|| Canvas::new(out.width(), out.height()));
                    matte.clear();
                    if visible(source, frame) {
                        let source_index = i - 1;
                        self.draw_layer(layers, source_index, frame, base, matte, depth);
                        let source_opacity = source.transform.opacity(local_frame(source, frame));
                        if source_opacity < 1.0 {
                            let mut faded = Canvas::new(out.width(), out.height());
                            faded.composite(matte, source_opacity, None);
                            *matte = faded;
                        }
                    }
                    out.composite(&layer_canvas, opacity, Some((matte, mode)));
                }
                _ => out.composite(&layer_canvas, opacity, None),
            }
        }
    }

    /// 图层自己的矩阵，连同所有父级。
    fn layer_matrix(layers: &[Layer], index: usize, frame: f32) -> Affine2 {
        let mut matrix = Affine2::IDENTITY;
        let mut current = Some(index);
        let mut guard = 0;
        while let Some(i) = current {
            let layer = &layers[i];
            matrix = layer.transform.matrix(local_frame(layer, frame)) * matrix;
            current = layer
                .parent
                .and_then(|p| layers.iter().position(|l| l.index == Some(p)));
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        matrix
    }

    fn draw_layer(
        &self,
        layers: &[Layer],
        index: usize,
        frame: f32,
        base: Affine2,
        out: &mut Canvas,
        depth: usize,
    ) {
        let layer = &layers[index];
        let local = local_frame(layer, frame);
        let matrix = base * Self::layer_matrix(layers, index, frame);
        match layer.kind {
            4 => {
                for op in group_ops(&layer.items, local, matrix, 1.0) {
                    out.fill(&op.0, Affine2::IDENTITY, op.1);
                }
            }
            1 => {
                if let Some((color, size)) = layer.solid {
                    let corners = [
                        Vec2::ZERO,
                        Vec2::new(size.x, 0.0),
                        size,
                        Vec2::new(0.0, size.y),
                    ];
                    let rect = Tessellation {
                        points: corners
                            .iter()
                            .map(|p| matrix.transform_point2(*p))
                            .collect(),
                        indices: vec![0, 1, 2, 0, 2, 3],
                    };
                    out.fill(&rect, Affine2::IDENTITY, color);
                }
            }
            0 => {
                if let Some(children) = layer.reference.as_ref().and_then(|r| self.assets.get(r)) {
                    // 预合成的时间：父图层的局部帧就是子合成的帧。
                    self.render_layers(children, local, matrix, out, depth + 1);
                }
            }
            _ => {}
        }
    }
}

fn local_frame(layer: &Layer, frame: f32) -> f32 {
    (frame - layer.start_time) / layer.stretch
}

fn visible(layer: &Layer, frame: f32) -> bool {
    frame >= layer.in_point && frame < layer.out_point
}

// ── 组的求值 ──

/// 一个子路径：折线（画布坐标）+ 是否闭合。
type Polyline = Contour;

/// 贝塞尔路径（局部坐标）→ 变换后展平。
fn bezier_polylines(encoded: &[f32], matrix: Affine2) -> Vec<Polyline> {
    if encoded.len() < 2 {
        return Vec::new();
    }
    let closed = encoded[0] != 0.0;
    let n = encoded[1] as usize;
    if n == 0 || encoded.len() < 2 + n * 6 {
        return Vec::new();
    }
    let point = |list: usize, k: usize| {
        Vec2::new(
            encoded[2 + list * n * 2 + k * 2],
            encoded[3 + list * n * 2 + k * 2],
        )
    };
    let vertex = |k: usize| point(0, k);
    let in_tangent = |k: usize| point(1, k);
    let out_tangent = |k: usize| point(2, k);
    let mut points = vec![matrix.transform_point2(vertex(0))];
    let segments = if closed { n } else { n - 1 };
    for k in 0..segments {
        let j = (k + 1) % n;
        let p0 = matrix.transform_point2(vertex(k));
        let p1 = matrix.transform_point2(vertex(k) + out_tangent(k));
        let p2 = matrix.transform_point2(vertex(j) + in_tangent(j));
        let p3 = matrix.transform_point2(vertex(j));
        let straight =
            out_tangent(k).length_squared() < 1e-12 && in_tangent(j).length_squared() < 1e-12;
        let steps = if straight {
            1
        } else {
            let length = p0.distance(p1) + p1.distance(p2) + p2.distance(p3);
            ((length / 2.0).sqrt().ceil() as usize).clamp(2, 48)
        };
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let u = 1.0 - t;
            points.push(
                p0 * (u * u * u)
                    + p1 * (3.0 * u * u * t)
                    + p2 * (3.0 * u * t * t)
                    + p3 * (t * t * t),
            );
        }
    }
    if closed && points.len() > 1 && points[0].distance_squared(points[points.len() - 1]) < 1e-8 {
        points.pop();
    }
    vec![Polyline { points, closed }]
}

fn ellipse_encoded(center: Vec2, size: Vec2) -> Vec<f32> {
    let (rx, ry) = (size.x * 0.5, size.y * 0.5);
    let k = 0.552_284_8;
    // 从顶点开始顺时针，和 AE 一致。
    let v = [(0.0, -ry), (rx, 0.0), (0.0, ry), (-rx, 0.0)];
    let t = [(rx * k, 0.0), (0.0, ry * k), (-rx * k, 0.0), (0.0, -ry * k)];
    let mut out = vec![1.0, 4.0];
    for p in v {
        out.extend_from_slice(&[center.x + p.0, center.y + p.1]);
    }
    for p in t {
        out.extend_from_slice(&[-p.0, -p.1]);
    }
    for p in t {
        out.extend_from_slice(&[p.0, p.1]);
    }
    out
}

fn rect_encoded(center: Vec2, size: Vec2, roundness: f32) -> Vec<f32> {
    let half = size * 0.5;
    let r = roundness.clamp(0.0, half.x.min(half.y));
    if r <= 0.0 {
        let corners = [
            (half.x, -half.y),
            (half.x, half.y),
            (-half.x, half.y),
            (-half.x, -half.y),
        ];
        let mut out = vec![1.0, 4.0];
        for c in corners {
            out.extend_from_slice(&[center.x + c.0, center.y + c.1]);
        }
        out.extend(std::iter::repeat_n(0.0, 16));
        return out;
    }
    let k = r * 0.552_284_8;
    // 八个点，每个角两个，切线只在圆弧那一侧有。
    let v = [
        (half.x, -half.y + r),
        (half.x, half.y - r),
        (half.x - r, half.y),
        (-half.x + r, half.y),
        (-half.x, half.y - r),
        (-half.x, -half.y + r),
        (-half.x + r, -half.y),
        (half.x - r, -half.y),
    ];
    let inn = [
        (0.0, -k),
        (0.0, 0.0),
        (k, 0.0),
        (0.0, 0.0),
        (0.0, k),
        (0.0, 0.0),
        (-k, 0.0),
        (0.0, 0.0),
    ];
    let out_t = [
        (0.0, 0.0),
        (0.0, k),
        (0.0, 0.0),
        (-k, 0.0),
        (0.0, 0.0),
        (0.0, -k),
        (0.0, 0.0),
        (k, 0.0),
    ];
    let mut out = vec![1.0, 8.0];
    for p in v {
        out.extend_from_slice(&[center.x + p.0, center.y + p.1]);
    }
    for p in inn.iter().chain(out_t.iter()) {
        out.extend_from_slice(&[p.0, p.1]);
    }
    out
}

/// 修剪：保留每条折线（或全体首尾相接）的 [start, end] 那一段，`offset` 是
/// 整圈的比例。三个量都是 0..1。
fn trim(
    polylines: Vec<Polyline>,
    start: f32,
    end: f32,
    offset: f32,
    individually: bool,
) -> Vec<Polyline> {
    if individually {
        return trim_tagged(polylines, start, end, offset)
            .into_iter()
            .map(|(_, p)| p)
            .collect();
    }
    trim_core(polylines, start, end, offset, false)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

/// 「逐个」修剪，每段结果带着它来自第几条折线。
fn trim_tagged(
    polylines: Vec<Polyline>,
    start: f32,
    end: f32,
    offset: f32,
) -> Vec<(usize, Polyline)> {
    trim_core(polylines, start, end, offset, true)
}

fn trim_core(
    polylines: Vec<Polyline>,
    start: f32,
    end: f32,
    offset: f32,
    individually: bool,
) -> Vec<(usize, Polyline)> {
    let (mut a, mut b) = (start.min(end), start.max(end));
    if b - a >= 1.0 - 1e-6 {
        return polylines.into_iter().enumerate().collect();
    }
    if b - a <= 1e-6 {
        return Vec::new();
    }
    a += offset;
    b += offset;
    let shift = a.floor();
    a -= shift;
    b -= shift;
    // [a, b] 可能跨过 1：拆成两段。
    let ranges: Vec<(f32, f32)> = if b > 1.0 {
        vec![(a, 1.0), (0.0, b - 1.0)]
    } else {
        vec![(a, b)]
    };

    let lengths = |p: &Polyline| -> Vec<f32> {
        let mut out = vec![0.0];
        let count = p.points.len();
        let segments = if p.closed {
            count
        } else {
            count.saturating_sub(1)
        };
        for i in 0..segments {
            let last = *out.last().unwrap_or(&0.0);
            out.push(last + p.points[i].distance(p.points[(i + 1) % count]));
        }
        out
    };
    // 一条折线上 [from, to]（弧长）那一段。
    let cut = |p: &Polyline, cumulative: &[f32], from: f32, to: f32| -> Option<Polyline> {
        if to - from <= 1e-6 {
            return None;
        }
        let count = p.points.len();
        let at = |d: f32| -> (usize, Vec2) {
            let k = cumulative
                .partition_point(|&c| c < d)
                .saturating_sub(1)
                .min(cumulative.len().saturating_sub(2));
            let (c0, c1) = (cumulative[k], cumulative[k + 1]);
            let t = if c1 > c0 { (d - c0) / (c1 - c0) } else { 0.0 };
            (
                k,
                p.points[k].lerp(p.points[(k + 1) % count], t.clamp(0.0, 1.0)),
            )
        };
        let (k0, start_point) = at(from);
        let (k1, end_point) = at(to);
        let mut points = vec![start_point];
        for k in k0 + 1..=k1 {
            points.push(p.points[k % count]);
        }
        points.push(end_point);
        points.dedup_by(|x, y| x.distance_squared(*y) < 1e-10);
        (points.len() >= 2).then_some(Polyline {
            points,
            closed: false,
        })
    };

    let mut out = Vec::new();
    if individually {
        // 「逐个」：所有路径首尾相接当一条量。
        let all: Vec<(Polyline, Vec<f32>)> = polylines
            .into_iter()
            .map(|p| {
                let c = lengths(&p);
                (p, c)
            })
            .collect();
        let total: f32 = all.iter().map(|(_, c)| *c.last().unwrap_or(&0.0)).sum();
        let mut base = 0.0;
        for (source, (p, c)) in all.iter().enumerate() {
            let length = *c.last().unwrap_or(&0.0);
            for &(ra, rb) in &ranges {
                let (from, to) = (
                    (ra * total - base).max(0.0),
                    (rb * total - base).min(length),
                );
                if let Some(piece) = cut(p, c, from, to) {
                    out.push((source, piece));
                }
            }
            base += length;
        }
    } else {
        for (source, p) in polylines.into_iter().enumerate() {
            let c = lengths(&p);
            let length = *c.last().unwrap_or(&0.0);
            for &(ra, rb) in &ranges {
                if let Some(piece) = cut(&p, &c, ra * length, rb * length) {
                    out.push((source, piece));
                }
            }
        }
    }
    out
}

/// 一个绘制操作：三角形（画布坐标）+ 颜色。
type Op = (Tessellation, [f32; 4]);

/// 还没三角化的样式：它作用于哪些路径槽、用什么画。
///
/// 样式要等**整个图层**求值完才能画：修剪之类的修饰器作用于列表里排在
/// 它前面的所有形状——包括前面那些组里的、已经「挂」上了描边的形状。
/// AE 导出的文件常常就是这样：组里是「路径 + 描边」，修剪写在组外面。
/// 所以形状先放进槽里，修饰器改槽，最后样式按槽里的最终几何来画。
enum Pending {
    Fill {
        slots: Vec<usize>,
        color: [f32; 4],
        rule: FillRule,
    },
    Stroke {
        slots: Vec<usize>,
        color: [f32; 4],
        style: StrokeStyle,
    },
}

/// 求值一个组。返回这个组里（含子组）所有的路径槽，绘制操作按
/// **从下到上**的顺序追加进 `pending`。
fn group(
    items: &[Item],
    frame: f32,
    parent: Affine2,
    parent_opacity: f32,
    slots: &mut Vec<Vec<Polyline>>,
    pending: &mut Vec<Pending>,
) -> Vec<usize> {
    // 组变换：AE 里写在组的最后一项，但作用于整个组。
    let (matrix, opacity) = items
        .iter()
        .find_map(|item| match item {
            Item::Transform(t) => {
                Some((parent * t.matrix(frame), parent_opacity * t.opacity(frame)))
            }
            _ => None,
        })
        .unwrap_or((parent, parent_opacity));
    let scale = matrix.matrix2.determinant().abs().sqrt();
    let mut mine: Vec<usize> = Vec::new();
    // 列表顺序的绘制操作，前面的在上面；每一项自己内部是从下到上。
    let mut ops_top_first: Vec<Vec<Pending>> = Vec::new();
    let push_slot =
        |slots: &mut Vec<Vec<Polyline>>, mine: &mut Vec<usize>, paths: Vec<Polyline>| {
            slots.push(paths);
            mine.push(slots.len() - 1);
        };
    for item in items {
        match item {
            Item::Group(children) => {
                let mut child_pending = Vec::new();
                let child_slots =
                    group(children, frame, matrix, opacity, slots, &mut child_pending);
                mine.extend(child_slots);
                ops_top_first.push(child_pending);
            }
            Item::Path(prop) => {
                push_slot(slots, &mut mine, bezier_polylines(&prop.at(frame), matrix))
            }
            Item::Rect {
                position,
                size,
                roundness,
            } => push_slot(
                slots,
                &mut mine,
                bezier_polylines(
                    &rect_encoded(
                        position.vec2(frame),
                        size.vec2(frame),
                        roundness.scalar(frame),
                    ),
                    matrix,
                ),
            ),
            Item::Ellipse { position, size } => push_slot(
                slots,
                &mut mine,
                bezier_polylines(
                    &ellipse_encoded(position.vec2(frame), size.vec2(frame)),
                    matrix,
                ),
            ),
            Item::Trim {
                start,
                end,
                offset,
                individually,
            } => {
                let (a, b, o) = (
                    start.scalar(frame) / 100.0,
                    end.scalar(frame) / 100.0,
                    offset.scalar(frame) / 360.0,
                );
                if *individually {
                    // 「逐个」：所有路径首尾相接当一条量，结果按原来的槽放回去。
                    let lengths: Vec<usize> = mine.iter().map(|&i| slots[i].len()).collect();
                    let all: Vec<Polyline> = mine
                        .iter()
                        .flat_map(|&i| std::mem::take(&mut slots[i]))
                        .collect();
                    let tagged = trim_tagged(all, a, b, o);
                    let mut owner_of = Vec::new();
                    for (k, n) in lengths.iter().enumerate() {
                        owner_of.extend(std::iter::repeat_n(k, *n));
                    }
                    for (source, piece) in tagged {
                        if let Some(&k) = owner_of.get(source) {
                            slots[mine[k]].push(piece);
                        }
                    }
                } else {
                    for &i in &mine {
                        let taken = std::mem::take(&mut slots[i]);
                        slots[i] = trim(taken, a, b, o, false);
                    }
                }
            }
            Item::Fill {
                color,
                opacity: o,
                rule,
            } => {
                let c = color.at(frame);
                let alpha = opacity
                    * (o.scalar(frame) / 100.0).clamp(0.0, 1.0)
                    * c.get(3).copied().unwrap_or(1.0);
                ops_top_first.push(vec![Pending::Fill {
                    slots: mine.clone(),
                    color: rgba(&c, alpha),
                    rule: *rule,
                }]);
            }
            Item::Stroke {
                color,
                opacity: o,
                width,
                style,
            } => {
                let c = color.at(frame);
                let alpha = opacity
                    * (o.scalar(frame) / 100.0).clamp(0.0, 1.0)
                    * c.get(3).copied().unwrap_or(1.0);
                let style = StrokeStyle {
                    width: width.scalar(frame) * scale,
                    ..*style
                };
                ops_top_first.push(vec![Pending::Stroke {
                    slots: mine.clone(),
                    color: rgba(&c, alpha),
                    style,
                }]);
            }
            Item::Transform(_) => {}
        }
    }
    pending.extend(ops_top_first.into_iter().rev().flatten());
    mine
}

fn rgba(c: &[f32], alpha: f32) -> [f32; 4] {
    [
        c.first().copied().unwrap_or(0.0),
        c.get(1).copied().unwrap_or(0.0),
        c.get(2).copied().unwrap_or(0.0),
        alpha.clamp(0.0, 1.0),
    ]
}

/// 一个图层的全部绘制操作，从下到上。
fn group_ops(items: &[Item], frame: f32, matrix: Affine2, opacity: f32) -> Vec<Op> {
    let mut slots = Vec::new();
    let mut pending = Vec::new();
    group(items, frame, matrix, opacity, &mut slots, &mut pending);
    pending
        .into_iter()
        .map(|op| match op {
            Pending::Fill {
                slots: ids,
                color,
                rule,
            } => {
                let closed: Vec<Contour> = ids
                    .iter()
                    .flat_map(|&i| slots[i].iter())
                    .map(|p| Contour {
                        points: p.points.clone(),
                        closed: true,
                    })
                    .collect();
                (fill(&closed, rule), color)
            }
            Pending::Stroke {
                slots: ids,
                color,
                style,
            } => {
                let mut tessellation = Tessellation::default();
                for p in ids.iter().flat_map(|&i| slots[i].iter()) {
                    let part = stroke_styled(p, &style);
                    let base = tessellation.points.len() as u32;
                    tessellation.points.extend(part.points);
                    tessellation
                        .indices
                        .extend(part.indices.iter().map(|i| i + base));
                }
                (tessellation, color)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animation(layers: &str) -> Animation {
        Animation::parse(
            format!(r#"{{"v":"5.5.7","fr":30,"ip":0,"op":60,"w":100,"h":100,"layers":{layers}}}"#)
                .as_bytes(),
        )
        .unwrap()
    }

    fn rect_layer(color: &str, extra: &str) -> String {
        format!(
            r#"{{"ty":4,"ind":1,"ip":0,"op":60,"st":0,"ks":{{}}{extra},"shapes":[{{"ty":"gr","it":[
                {{"ty":"rc","p":{{"a":0,"k":[50,50]}},"s":{{"a":0,"k":[40,40]}},"r":{{"a":0,"k":0}}}},
                {{"ty":"fl","c":{{"a":0,"k":{color}}},"o":{{"a":0,"k":100}},"r":1}},
                {{"ty":"tr","p":{{"a":0,"k":[0,0]}},"a":{{"a":0,"k":[0,0]}},"s":{{"a":0,"k":[100,100]}},"r":{{"a":0,"k":0}},"o":{{"a":0,"k":100}}}}
            ]}}]}}"#
        )
    }

    #[test]
    fn keyframes_ease_and_hold() {
        let prop = Prop::parse(
            Some(&serde_json::json!({"a":1,"k":[
                {"t":0,"s":[0],"o":{"x":[0.0],"y":[0.0]},"i":{"x":[1.0],"y":[1.0]}},
                {"t":10,"s":[100],"h":1},
                {"t":20,"s":[50]}
            ]})),
            &[0.0],
        );
        assert!(
            (prop.scalar(5.0) - 50.0).abs() < 1.0,
            "线性缓动的中点：{}",
            prop.scalar(5.0)
        );
        assert_eq!(prop.scalar(15.0), 100.0, "定格");
        assert_eq!(prop.scalar(30.0), 50.0);
        assert_eq!(prop.scalar(-5.0), 0.0);
    }

    #[test]
    fn ease_in_out_is_slow_at_the_ends() {
        let y = ease(0.1, (0.333, 0.0), (0.667, 1.0));
        assert!(y < 0.1, "{y}");
        assert!((ease(0.5, (0.333, 0.0), (0.667, 1.0)) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn a_filled_rect_lands_where_it_should() {
        let anim = animation(&format!("[{}]", rect_layer("[1,0,0,1]", "")));
        let canvas = anim.render(0.0, 100, 100);
        assert_eq!(canvas.pixel(50, 50), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(canvas.pixel(10, 10)[3], 0.0);
    }

    #[test]
    fn earlier_layers_are_on_top() {
        let anim = animation(&format!(
            "[{},{}]",
            rect_layer("[0,1,0,1]", ""),
            rect_layer("[1,0,0,1]", "")
        ));
        let canvas = anim.render(0.0, 100, 100);
        assert_eq!(canvas.pixel(50, 50)[1], 1.0);
    }

    #[test]
    fn layers_outside_their_range_are_hidden() {
        let layer = rect_layer("[1,0,0,1]", "").replace(r#""ip":0,"op":60"#, r#""ip":30,"op":60"#);
        let anim = animation(&format!("[{layer}]"));
        assert_eq!(anim.render(10.0, 100, 100).pixel(50, 50)[3], 0.0);
        assert_eq!(anim.render(40.0, 100, 100).pixel(50, 50)[3], 1.0);
    }

    #[test]
    fn inverted_track_matte_cuts_a_hole() {
        // 遮罩层（td）在上，被遮的层（tt=2）紧跟其后。
        let matte = rect_layer("[1,1,1,1]", r#","td":1"#).replace("[40,40]", "[10,10]");
        let content = rect_layer("[0,0,1,1]", r#","tt":2"#);
        let anim = animation(&format!("[{matte},{content}]"));
        let canvas = anim.render(0.0, 100, 100);
        assert_eq!(canvas.pixel(50, 50)[3], 0.0, "遮罩盖住的地方挖空");
        assert_eq!(canvas.pixel(35, 35)[2], 1.0, "遮罩以外照常");
    }

    #[test]
    fn trim_keeps_the_requested_fraction() {
        let line = vec![Polyline {
            points: vec![Vec2::ZERO, Vec2::new(10.0, 0.0)],
            closed: false,
        }];
        let half = trim(line.clone(), 0.0, 0.5, 0.0, false);
        assert_eq!(half[0].points.last().copied(), Some(Vec2::new(5.0, 0.0)));
        // 偏移 0.75 把 [0, 0.5] 推到 [0.75, 1.25]：首尾各一段。
        let wrapped = trim(line, 0.0, 0.5, 0.75, false);
        assert_eq!(wrapped.len(), 2);
        let empty = trim(
            vec![Polyline {
                points: vec![Vec2::ZERO, Vec2::X],
                closed: false,
            }],
            0.3,
            0.3,
            0.0,
            false,
        );
        assert!(empty.is_empty());
    }

    #[test]
    fn the_sample_logo_renders_its_strokes() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/threejs/textures/lottie/24017-lottie-logo-animation.json"
        );
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let anim = Animation::parse(&bytes).unwrap();
        assert!(anim.unsupported.is_empty(), "{:?}", anim.unsupported);
        // 第 0 帧什么都还没画出来（修剪路径从 0 开始），最后一帧字都写完了。
        let covered = |canvas: &Canvas| {
            let mut n = 0;
            for y in 0..canvas.height() {
                for x in 0..canvas.width() {
                    if canvas.pixel(x, y)[3] > 0.5 {
                        n += 1;
                    }
                }
            }
            n
        };
        let start = covered(&anim.render(0.0, 128, 128));
        let end = covered(&anim.render(anim.out_point - 1.0, 128, 128));
        assert!(end > start + 200, "开头 {start} 像素，结尾 {end} 像素");
    }
}
