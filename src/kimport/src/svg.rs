//! SVG：二维矢量图 → 填充和描边的三角形。
//!
//! three.js 的 `SVGLoader`。解析出来的是一份 [`Document`]：每条路径已经
//! 展平成折线、套好了变换，带着它的填充和描边样式。要画的话
//! [`Document::to_model`] 直接给一个 [`Model`]，或者自己拿轮廓去用
//! （拉伸成立体图标、做碰撞形状……）。
//!
//! # 支持到哪
//!
//! - 元素：`path` `rect`（含圆角）`circle` `ellipse` `line` `polyline`
//!   `polygon` `g` `svg`（`viewBox`）`use`（`href` / `xlink:href`，带 x / y）；
//!   `defs` `symbol` `clipPath` `mask` 和渐变里的东西只被引用、不直接画；
//! - 路径语法全集：`M L H V C S Q T A Z` 大小写、隐式重复、`1.5.5` 这种
//!   粘连的数字、弧的标志位不加分隔；弧转三次贝塞尔；
//! - 样式：表现属性、`style="..."`、`<style>` 里的 CSS（`.类`、`#id`、
//!   标签名、`标签.类`、逗号分组，多个类），按 CSS 的先后覆盖；可继承
//!   的属性沿着树往下传；
//! - `fill` `fill-opacity` `fill-rule` `stroke` `stroke-width`
//!   `stroke-opacity` `stroke-linejoin` `stroke-linecap` `stroke-miterlimit`
//!   `opacity` `display` `visibility`；
//! - 颜色：`#rgb` `#rrggbb` `rgb()` `rgba()` `hsl()` `hsla()`、147 个 CSS
//!   颜色名、`currentColor`、`none`；渐变（`url(#id)`）取第一个色标；
//! - 变换：`matrix` `translate` `scale` `rotate`（含圆心）`skewX` `skewY`；
//! - 长度单位：`px` `pt` `pc` `mm` `cm` `in` `em` `ex` `%`（相对视口）。
//!
//! # 不支持的
//!
//! 文字（`<text>`）、图片（`<image>`）、真正的渐变填充、滤镜、裁剪和
//! 蒙版、虚线（`stroke-dasharray`）、`marker`。

use crate::path::{
    Contour, FillRule, LineCap, LineJoin, Path, Segment, StrokeStyle, Tessellation, fill,
    stroke_styled,
};
use crate::xml::{self, Element};
use kasset::LoadError;
use kgltf::{MeshPart, Model, ModelNode, NodeTransform};
use kmaterial::{BlendMode, Material};
use kmath::{Affine2, Vec2, Vec3};
use kmesh::{Mesh, Vertex};
use std::collections::HashMap;

/// 一种颜料：线性 RGB + 不透明度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    /// 线性 RGB。
    pub color: Vec3,
    /// 不透明度（已经乘上了 `opacity`）。
    pub opacity: f32,
}

/// 一条路径的描边。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    /// 颜料。
    pub paint: Paint,
    /// 线型（线宽已经按变换缩放过）。
    pub style: StrokeStyle,
}

/// 一条可画的路径。
#[derive(Debug, Clone, PartialEq)]
pub struct SvgPath {
    /// 元素的 `id`（没有时为空）。
    pub id: String,
    /// 展平、变换之后的子路径，SVG 坐标（y 朝下）。
    pub contours: Vec<Contour>,
    /// 填充。`None` 是 `fill="none"`。
    pub fill: Option<Paint>,
    /// 填充规则。
    pub fill_rule: FillRule,
    /// 描边。`None` 是没有描边。
    pub stroke: Option<Stroke>,
}

impl SvgPath {
    /// 填充的三角形（SVG 坐标）。
    pub fn fill_tessellation(&self) -> Tessellation {
        let closed: Vec<Contour> = self
            .contours
            .iter()
            .map(|c| Contour {
                points: c.points.clone(),
                closed: true,
            })
            .collect();
        fill(&closed, self.fill_rule)
    }

    /// 描边的三角形（SVG 坐标）。没有描边时为空。
    pub fn stroke_tessellation(&self) -> Tessellation {
        let Some(stroke) = &self.stroke else {
            return Tessellation::default();
        };
        let mut out = Tessellation::default();
        for contour in &self.contours {
            let part = stroke_styled(contour, &stroke.style);
            let base = out.points.len() as u32;
            out.points.extend(part.points);
            out.indices.extend(part.indices.iter().map(|i| i + base));
        }
        out
    }
}

/// 解析好的 SVG。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    /// 按绘制顺序排的路径（后画的在上面）。
    pub paths: Vec<SvgPath>,
    /// `viewBox`（x, y, 宽, 高）。
    pub view_box: Option<[f32; 4]>,
}

/// 转成模型时的选项。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelOptions {
    /// 画填充。
    pub fills: bool,
    /// 画描边。
    pub strokes: bool,
    /// 相邻两层之间在 Z 上错开多少（SVG 单位），靠它保证后画的在上面。
    pub layer_step: f32,
    /// 把 y 翻成朝上（SVG 是朝下的）。
    pub flip_y: bool,
}

impl Default for ModelOptions {
    fn default() -> Self {
        // 太小会和深度精度打架（相机离得远时尤其明显），太大转着看会有视差。
        Self {
            fills: true,
            strokes: true,
            layer_step: 0.05,
            flip_y: true,
        }
    }
}

impl Document {
    /// 转成一个平面模型：每个填充 / 描边一个网格，不受光（和 three.js 的
    /// `MeshBasicMaterial` 一样），双面。
    ///
    /// 平面上的层叠顺序靠 Z 方向的微小错开保证——引擎按距离给透明物体
    /// 排序、按深度给不透明物体遮挡，两种情况下后画的都在前面。
    pub fn to_model(&self, name: &str, options: ModelOptions) -> Model {
        let mut meshes = Vec::new();
        let mut materials: Vec<Material> = Vec::new();
        let mut material_index: HashMap<[u32; 4], usize> = HashMap::new();
        let mut parts = Vec::new();
        let mut layer = 0usize;
        let y = if options.flip_y { -1.0 } else { 1.0 };
        let mut push = |t: Tessellation, paint: Paint, layer: usize| {
            if t.is_empty() {
                return;
            }
            let z = layer as f32 * options.layer_step;
            let vertices = t
                .points
                .iter()
                .map(|p| Vertex::new(Vec3::new(p.x, p.y * y, z), Vec3::Z, [p.x, p.y]))
                .collect();
            meshes.push(Mesh::new(vertices, t.indices));
            let key =
                [paint.color.x, paint.color.y, paint.color.z, paint.opacity].map(f32::to_bits);
            let material = *material_index.entry(key).or_insert_with(|| {
                let mut material =
                    kpbr::unlit::UnlitMaterial::new(paint.color.extend(paint.opacity))
                        .with_double_sided();
                if paint.opacity < 1.0 {
                    material.set_blend_mode(BlendMode::Alpha);
                }
                materials.push(material);
                materials.len() - 1
            });
            parts.push(MeshPart {
                mesh: meshes.len() - 1,
                material: Some(material),
            });
        };
        for path in &self.paths {
            if options.fills
                && let Some(paint) = path.fill
            {
                push(path.fill_tessellation(), paint, layer);
                layer += 1;
            }
            if options.strokes
                && let Some(stroke) = path.stroke
            {
                push(path.stroke_tessellation(), stroke.paint, layer);
                layer += 1;
            }
        }
        Model::new(
            meshes,
            materials,
            vec![ModelNode {
                name: name.to_string(),
                transform: NodeTransform::default(),
                children: Vec::new(),
                parts,
                skin: None,
            }],
            vec![0],
        )
    }
}

/// 解析 SVG。`tolerance` 是曲线展平的容差（SVG 单位，0.25 左右合适）。
pub fn parse(bytes: &[u8], tolerance: f32) -> Result<Document, LoadError> {
    let root = xml::parse(bytes)?;
    if root.name != "svg" {
        return Err(crate::bad("不是 SVG 文档（根元素不是 <svg>）"));
    }
    let mut ids = HashMap::new();
    collect_ids(&root, &mut ids);
    let mut css = Vec::new();
    collect_css(&root, &mut css);
    let view_box = root
        .attr("viewBox")
        .map(numbers)
        .filter(|v| v.len() == 4)
        .map(|v| [v[0], v[1], v[2], v[3]]);
    let viewport = view_box.map_or(
        Vec2::new(
            root.attr("width").map_or(300.0, |w| length(w, 300.0, 16.0)),
            root.attr("height")
                .map_or(150.0, |h| length(h, 150.0, 16.0)),
        ),
        |v| Vec2::new(v[2], v[3]),
    );
    let mut walker = Walker {
        ids: &ids,
        css: &css,
        tolerance,
        viewport,
        out: Vec::new(),
        depth: 0,
    };
    walker.element(&root, &Style::default(), Affine2::IDENTITY);
    Ok(Document {
        paths: walker.out,
        view_box,
    })
}

fn collect_ids<'a>(element: &'a Element, out: &mut HashMap<String, &'a Element>) {
    if let Some(id) = element.attr("id") {
        out.insert(id.to_string(), element);
    }
    element.children.iter().for_each(|c| collect_ids(c, out));
}

// ── CSS ──

#[derive(Debug, Clone)]
struct Rule {
    selector: Selector,
    declarations: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Selector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Selector {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        // 只认简单选择器：后代、子代组合器这里不做。
        if text.is_empty()
            || text.contains(|c: char| c.is_whitespace() || c == '>' || c == '[' || c == ':')
        {
            return None;
        }
        let mut selector = Selector {
            tag: None,
            id: None,
            classes: Vec::new(),
        };
        let mut current = String::new();
        let mut kind = 't';
        let commit = |kind: char, current: &mut String, selector: &mut Selector| {
            if !current.is_empty() {
                match kind {
                    '.' => selector.classes.push(std::mem::take(current)),
                    '#' => selector.id = Some(std::mem::take(current)),
                    _ => {
                        if current != "*" {
                            selector.tag = Some(current.clone());
                        }
                        current.clear();
                    }
                }
            }
        };
        for c in text.chars() {
            if c == '.' || c == '#' {
                commit(kind, &mut current, &mut selector);
                kind = c;
            } else {
                current.push(c);
            }
        }
        commit(kind, &mut current, &mut selector);
        Some(selector)
    }

    fn matches(&self, element: &Element) -> bool {
        if self.tag.as_deref().is_some_and(|t| t != element.name) {
            return false;
        }
        if self
            .id
            .as_deref()
            .is_some_and(|id| Some(id) != element.attr("id"))
        {
            return false;
        }
        let classes: Vec<&str> = element
            .attr("class")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        self.classes.iter().all(|c| classes.contains(&c.as_str()))
    }
}

fn collect_css(element: &Element, out: &mut Vec<Rule>) {
    if element.name == "style" {
        parse_css(&element.text, out);
    }
    element.children.iter().for_each(|c| collect_css(c, out));
}

fn parse_css(text: &str, out: &mut Vec<Rule>) {
    // 去注释。
    let mut clean = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        clean.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .find("*/")
            .map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    clean.push_str(rest);
    for block in clean.split('}') {
        let Some((selectors, body)) = block.split_once('{') else {
            continue;
        };
        let declarations = declarations(body);
        for selector in selectors.split(',').filter_map(Selector::parse) {
            out.push(Rule {
                selector,
                declarations: declarations.clone(),
            });
        }
    }
}

fn declarations(text: &str) -> Vec<(String, String)> {
    text.split(';')
        .filter_map(|d| d.split_once(':'))
        .map(|(k, v)| {
            (
                k.trim().to_ascii_lowercase(),
                v.trim().trim_end_matches("!important").trim().to_string(),
            )
        })
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

// ── 样式 ──

#[derive(Debug, Clone)]
struct Style {
    fill: Option<String>,
    fill_opacity: f32,
    fill_rule: FillRule,
    stroke: Option<String>,
    stroke_width: f32,
    stroke_opacity: f32,
    join: LineJoin,
    cap: LineCap,
    miter_limit: f32,
    /// 累乘的 `opacity`（严格说它是组透明度，这里近似成逐元素相乘）。
    opacity: f32,
    color: String,
    visible: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: Some("black".into()),
            fill_opacity: 1.0,
            fill_rule: FillRule::NonZero,
            stroke: None,
            stroke_width: 1.0,
            stroke_opacity: 1.0,
            join: LineJoin::Miter,
            cap: LineCap::Butt,
            miter_limit: 4.0,
            opacity: 1.0,
            color: "black".into(),
            visible: true,
        }
    }
}

const STYLE_PROPERTIES: [&str; 14] = [
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linejoin",
    "stroke-linecap",
    "stroke-miterlimit",
    "opacity",
    "color",
    "display",
    "visibility",
    "clip-rule",
];

impl Style {
    fn apply(&mut self, key: &str, value: &str, viewport: Vec2) -> bool {
        let value = value.trim();
        if value == "inherit" {
            return true;
        }
        let number = |v: &str| {
            v.trim().trim_end_matches('%').parse::<f32>().ok().map(|n| {
                if v.trim().ends_with('%') {
                    n / 100.0
                } else {
                    n
                }
            })
        };
        match key {
            "fill" => self.fill = (value != "none").then(|| value.to_string()),
            "fill-opacity" => self.fill_opacity = number(value).unwrap_or(1.0).clamp(0.0, 1.0),
            "fill-rule" => {
                self.fill_rule = if value == "evenodd" {
                    FillRule::EvenOdd
                } else {
                    FillRule::NonZero
                }
            }
            "stroke" => self.stroke = (value != "none").then(|| value.to_string()),
            "stroke-width" => {
                self.stroke_width =
                    length(value, viewport.length() / std::f32::consts::SQRT_2, 16.0)
            }
            "stroke-opacity" => self.stroke_opacity = number(value).unwrap_or(1.0).clamp(0.0, 1.0),
            "stroke-linejoin" => {
                self.join = match value {
                    "round" => LineJoin::Round,
                    "bevel" => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                }
            }
            "stroke-linecap" => {
                self.cap = match value {
                    "round" => LineCap::Round,
                    "square" => LineCap::Square,
                    _ => LineCap::Butt,
                }
            }
            "stroke-miterlimit" => self.miter_limit = number(value).unwrap_or(4.0).max(1.0),
            "opacity" => self.opacity *= number(value).unwrap_or(1.0).clamp(0.0, 1.0),
            "color" => self.color = value.to_string(),
            "display" => {
                if value == "none" {
                    self.visible = false;
                }
            }
            "visibility" => self.visible = !matches!(value, "hidden" | "collapse"),
            _ => return false,
        }
        true
    }
}

// ── 遍历 ──

struct Walker<'a> {
    ids: &'a HashMap<String, &'a Element>,
    css: &'a [Rule],
    tolerance: f32,
    viewport: Vec2,
    out: Vec<SvgPath>,
    depth: usize,
}

impl Walker<'_> {
    fn style_of(&self, element: &Element, parent: &Style) -> Style {
        let mut style = parent.clone();
        // opacity 不继承：从 1 开始乘自己的，再乘父级的。
        style.opacity = parent.opacity;
        // display 不继承，但父级 display:none 时整棵子树都不画，visible 已经是 false。
        for key in STYLE_PROPERTIES {
            if let Some(value) = element.attr(key) {
                style.apply(key, value, self.viewport);
            }
        }
        for rule in self.css {
            if rule.selector.matches(element) {
                for (key, value) in &rule.declarations {
                    style.apply(key, value, self.viewport);
                }
            }
        }
        if let Some(inline) = element.attr("style") {
            for (key, value) in declarations(inline) {
                style.apply(&key, &value, self.viewport);
            }
        }
        style
    }

    fn element(&mut self, element: &Element, parent: &Style, transform: Affine2) {
        self.depth += 1;
        if self.depth > 64 {
            self.depth -= 1;
            return;
        }
        let style = self.style_of(element, parent);
        let mut transform = transform;
        if let Some(t) = element.attr("transform") {
            transform *= parse_transform(t);
        }
        match element.name.as_str() {
            "svg" | "g" | "a" | "switch" => {
                if style.visible || element.name == "g" {
                    // 嵌套的 svg：x / y 平移（viewBox 缩放这里不做）。
                    if element.name == "svg" && self.depth > 1 {
                        let x = element
                            .attr("x")
                            .map_or(0.0, |v| length(v, self.viewport.x, 16.0));
                        let y = element
                            .attr("y")
                            .map_or(0.0, |v| length(v, self.viewport.y, 16.0));
                        transform *= Affine2::from_translation(Vec2::new(x, y));
                    }
                    for child in &element.children {
                        self.element(child, &style, transform);
                    }
                }
            }
            "use" => {
                let href = element
                    .attr("href")
                    .or_else(|| element.attr("xlink:href"))
                    .unwrap_or("");
                if let Some(target) = href
                    .strip_prefix('#')
                    .and_then(|id| self.ids.get(id))
                    .copied()
                {
                    let x = element
                        .attr("x")
                        .map_or(0.0, |v| length(v, self.viewport.x, 16.0));
                    let y = element
                        .attr("y")
                        .map_or(0.0, |v| length(v, self.viewport.y, 16.0));
                    let placed = transform * Affine2::from_translation(Vec2::new(x, y));
                    if target.name == "symbol" {
                        for child in &target.children {
                            self.element(child, &style, placed);
                        }
                    } else {
                        self.element(target, &style, placed);
                    }
                }
            }
            "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon"
                if style.visible =>
            {
                self.shape(element, &style, transform);
            }
            // defs / symbol / clipPath / mask / 渐变 / style / title……不直接画。
            _ => {}
        }
        self.depth -= 1;
    }

    fn shape(&mut self, element: &Element, style: &Style, transform: Affine2) {
        let len = |name: &str, reference: f32| {
            element
                .attr(name)
                .map_or(0.0, |v| length(v, reference, 16.0))
        };
        let (vw, vh) = (self.viewport.x, self.viewport.y);
        let diagonal = self.viewport.length() / std::f32::consts::SQRT_2;
        let mut path = Path::new();
        match element.name.as_str() {
            "path" => path = parse_path_data(element.attr("d").unwrap_or("")),
            "rect" => {
                let (x, y, w, h) = (
                    len("x", vw),
                    len("y", vh),
                    len("width", vw),
                    len("height", vh),
                );
                if w <= 0.0 || h <= 0.0 {
                    return;
                }
                let mut rx = element.attr("rx").map(|v| length(v, vw, 16.0));
                let mut ry = element.attr("ry").map(|v| length(v, vh, 16.0));
                if rx.is_none() {
                    rx = ry;
                }
                if ry.is_none() {
                    ry = rx;
                }
                let rx = rx.unwrap_or(0.0).clamp(0.0, w / 2.0);
                let ry = ry.unwrap_or(0.0).clamp(0.0, h / 2.0);
                if rx > 0.0 && ry > 0.0 {
                    // 四个圆角用四分之一椭圆（三次贝塞尔的魔数 0.5523）。
                    let k = 0.552_284_8;
                    let (cx, cy) = (rx * k, ry * k);
                    path.push(Segment::MoveTo(Vec2::new(x + rx, y)));
                    path.push(Segment::LineTo(Vec2::new(x + w - rx, y)));
                    path.push(Segment::Cubic(
                        Vec2::new(x + w - rx + cx, y),
                        Vec2::new(x + w, y + ry - cy),
                        Vec2::new(x + w, y + ry),
                    ));
                    path.push(Segment::LineTo(Vec2::new(x + w, y + h - ry)));
                    path.push(Segment::Cubic(
                        Vec2::new(x + w, y + h - ry + cy),
                        Vec2::new(x + w - rx + cx, y + h),
                        Vec2::new(x + w - rx, y + h),
                    ));
                    path.push(Segment::LineTo(Vec2::new(x + rx, y + h)));
                    path.push(Segment::Cubic(
                        Vec2::new(x + rx - cx, y + h),
                        Vec2::new(x, y + h - ry + cy),
                        Vec2::new(x, y + h - ry),
                    ));
                    path.push(Segment::LineTo(Vec2::new(x, y + ry)));
                    path.push(Segment::Cubic(
                        Vec2::new(x, y + ry - cy),
                        Vec2::new(x + rx - cx, y),
                        Vec2::new(x + rx, y),
                    ));
                } else {
                    path.push(Segment::MoveTo(Vec2::new(x, y)));
                    path.push(Segment::LineTo(Vec2::new(x + w, y)));
                    path.push(Segment::LineTo(Vec2::new(x + w, y + h)));
                    path.push(Segment::LineTo(Vec2::new(x, y + h)));
                }
                path.push(Segment::Close);
            }
            "circle" | "ellipse" => {
                let (cx, cy) = (len("cx", vw), len("cy", vh));
                let (rx, ry) = if element.name == "circle" {
                    let r = len("r", diagonal);
                    (r, r)
                } else {
                    (len("rx", vw), len("ry", vh))
                };
                if rx <= 0.0 || ry <= 0.0 {
                    return;
                }
                let k = 0.552_284_8;
                path.push(Segment::MoveTo(Vec2::new(cx + rx, cy)));
                for q in 0..4 {
                    let a0 = q as f32 * std::f32::consts::FRAC_PI_2;
                    let a1 = a0 + std::f32::consts::FRAC_PI_2;
                    let p = |a: f32| Vec2::new(cx + rx * a.cos(), cy + ry * a.sin());
                    let t = |a: f32| Vec2::new(-rx * a.sin(), ry * a.cos());
                    path.push(Segment::Cubic(p(a0) + t(a0) * k, p(a1) - t(a1) * k, p(a1)));
                }
                path.push(Segment::Close);
            }
            "line" => {
                path.push(Segment::MoveTo(Vec2::new(len("x1", vw), len("y1", vh))));
                path.push(Segment::LineTo(Vec2::new(len("x2", vw), len("y2", vh))));
            }
            "polyline" | "polygon" => {
                let values = numbers(element.attr("points").unwrap_or(""));
                for (i, pair) in values.chunks_exact(2).enumerate() {
                    let p = Vec2::new(pair[0], pair[1]);
                    path.push(if i == 0 {
                        Segment::MoveTo(p)
                    } else {
                        Segment::LineTo(p)
                    });
                }
                if element.name == "polygon" {
                    path.push(Segment::Close);
                }
            }
            _ => return,
        }
        if path.is_empty() {
            return;
        }
        // 先变换控制点再展平：仿射变换下贝塞尔的控制点变换等于曲线变换。
        let transformed = Path {
            segments: path
                .segments
                .iter()
                .map(|s| match *s {
                    Segment::MoveTo(p) => Segment::MoveTo(transform.transform_point2(p)),
                    Segment::LineTo(p) => Segment::LineTo(transform.transform_point2(p)),
                    Segment::Quadratic(c, p) => Segment::Quadratic(
                        transform.transform_point2(c),
                        transform.transform_point2(p),
                    ),
                    Segment::Cubic(a, b, p) => Segment::Cubic(
                        transform.transform_point2(a),
                        transform.transform_point2(b),
                        transform.transform_point2(p),
                    ),
                    Segment::Close => Segment::Close,
                })
                .collect(),
        };
        let mut contours = transformed.flatten(self.tolerance);
        // 单点子路径（`M x y` 后面什么都没有）flatten 会丢掉；圆头 / 方头描边要画成点。
        contours.extend(single_points(&transformed).into_iter().map(|p| Contour {
            points: vec![p],
            closed: false,
        }));
        let scale = transform.matrix2.determinant().abs().sqrt();
        let fill = style
            .fill
            .as_deref()
            .and_then(|f| self.color(f, &style.color))
            .map(|color| Paint {
                color,
                opacity: style.fill_opacity * style.opacity,
            });
        let stroke = style
            .stroke
            .as_deref()
            .and_then(|s| self.color(s, &style.color))
            .filter(|_| style.stroke_width > 0.0)
            .map(|color| Stroke {
                paint: Paint {
                    color,
                    opacity: style.stroke_opacity * style.opacity,
                },
                style: StrokeStyle {
                    width: style.stroke_width * scale,
                    join: style.join,
                    cap: style.cap,
                    miter_limit: style.miter_limit,
                    round_segments: 8,
                },
            });
        // 直线和折线没有填充（面积为零），但 fill 默认是黑色：别产生空网格。
        let fill = fill.filter(|_| !matches!(element.name.as_str(), "line"));
        if fill.is_none() && stroke.is_none() {
            return;
        }
        self.out.push(SvgPath {
            id: element.attr("id").unwrap_or("").to_string(),
            contours,
            fill,
            fill_rule: style.fill_rule,
            stroke,
        });
    }

    /// 颜色值 → 线性 RGB。渐变取第一个色标。
    fn color(&self, value: &str, current: &str) -> Option<Vec3> {
        let value = value.trim();
        if value == "currentColor" {
            return parse_color(current);
        }
        if let Some(id) = value
            .strip_prefix("url(#")
            .and_then(|v| v.split(')').next())
        {
            return self.gradient_color(id, 0).or_else(|| {
                // `url(#x) red`：引用失效时的回退色。
                value
                    .split(')')
                    .nth(1)
                    .and_then(|fallback| parse_color(fallback.trim()))
            });
        }
        parse_color(value)
    }

    fn gradient_color(&self, id: &str, depth: usize) -> Option<Vec3> {
        let gradient = self.ids.get(id)?;
        if let Some(stop) = gradient.children.iter().find(|c| c.name == "stop") {
            let inline = stop.attr("style").map(declarations).unwrap_or_default();
            let color = inline
                .iter()
                .find(|(k, _)| k == "stop-color")
                .map(|(_, v)| v.as_str())
                .or_else(|| stop.attr("stop-color"))
                .unwrap_or("black");
            return parse_color(color);
        }
        // 没有色标的渐变通过 href 继承别人的。
        let href = gradient
            .attr("href")
            .or_else(|| gradient.attr("xlink:href"))?;
        if depth > 8 {
            return None;
        }
        self.gradient_color(href.trim_start_matches('#'), depth + 1)
    }
}

fn single_points(path: &Path) -> Vec<Vec2> {
    let mut out = Vec::new();
    let segments = &path.segments;
    for (i, s) in segments.iter().enumerate() {
        if let Segment::MoveTo(p) = s {
            let next = segments.get(i + 1);
            let lone = match next {
                None | Some(Segment::MoveTo(_)) => true,
                Some(Segment::Close) => {
                    matches!(segments.get(i + 2), None | Some(Segment::MoveTo(_)))
                }
                _ => false,
            };
            if lone {
                out.push(*p);
            }
        }
    }
    out
}

// ── 数值与单位 ──

/// 长度：带单位的数字 → 用户单位（px）。`%` 相对 `reference`。
fn length(text: &str, reference: f32, font_size: f32) -> f32 {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(text.len());
    // `1e5` 里的 e 属于数字，`1em` 里的 e 属于单位。
    let split = if text[..split].ends_with(['e', 'E']) && text[split..].starts_with(['m', 'x']) {
        split - 1
    } else {
        split
    };
    let value: f32 = text[..split].parse().unwrap_or(0.0);
    match text[split..].trim() {
        "px" | "" => value,
        "pt" => value * 4.0 / 3.0,
        "pc" => value * 16.0,
        "mm" => value * 96.0 / 25.4,
        "cm" => value * 96.0 / 2.54,
        "in" => value * 96.0,
        "em" => value * font_size,
        "ex" => value * font_size * 0.5,
        "%" => value / 100.0 * reference,
        _ => value,
    }
}

/// 一串以逗号或空白分隔的数（也处理 `1-2`、`1.5.5` 这种粘连）。
fn numbers(text: &str) -> Vec<f32> {
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        at: 0,
    };
    let mut out = Vec::new();
    while let Some(n) = scanner.number() {
        out.push(n);
    }
    out
}

struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Scanner<'_> {
    fn skip_separators(&mut self) {
        while self.at < self.bytes.len()
            && matches!(self.bytes[self.at], b' ' | b'\t' | b'\r' | b'\n' | b',')
        {
            self.at += 1;
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.at;
        let b = self.bytes;
        let mut i = self.at;
        if i < b.len() && matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        let mut digits = false;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits = true;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
                digits = true;
            }
        }
        if !digits {
            return None;
        }
        if i < b.len() && matches!(b[i], b'e' | b'E') {
            let mut j = i + 1;
            if j < b.len() && matches!(b[j], b'+' | b'-') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        self.at = i;
        std::str::from_utf8(&b[start..i]).ok()?.parse().ok()
    }

    /// 弧的标志位：一个 `0` 或 `1`，后面可以直接粘着下一个数。
    fn flag(&mut self) -> Option<bool> {
        self.skip_separators();
        let c = *self.bytes.get(self.at)?;
        if c == b'0' || c == b'1' {
            self.at += 1;
            Some(c == b'1')
        } else {
            None
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip_separators();
        let c = *self.bytes.get(self.at)?;
        if c.is_ascii_alphabetic() && !matches!(c, b'e' | b'E') {
            self.at += 1;
            Some(c)
        } else {
            None
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_separators();
        self.at >= self.bytes.len()
    }
}

/// 解析 `d` 属性。
pub fn parse_path_data(d: &str) -> Path {
    let mut scanner = Scanner {
        bytes: d.as_bytes(),
        at: 0,
    };
    let mut path = Path::new();
    let mut cursor = Vec2::ZERO;
    let mut start = Vec2::ZERO;
    // 上一段的第二控制点（S / T 的反射用）。
    let mut last_cubic: Option<Vec2> = None;
    let mut last_quad: Option<Vec2> = None;
    let mut command = 0u8;
    while !scanner.at_end() {
        if let Some(c) = scanner.command() {
            command = c;
        } else if command == 0 {
            break;
        }
        let relative = command.is_ascii_lowercase();
        let base = if relative { cursor } else { Vec2::ZERO };
        let point = |s: &mut Scanner| -> Option<Vec2> { Some(Vec2::new(s.number()?, s.number()?)) };
        let mut reflected_cubic = None;
        let mut reflected_quad = None;
        match command.to_ascii_uppercase() {
            b'M' => {
                let Some(p) = point(&mut scanner) else { break };
                cursor = base + p;
                start = cursor;
                path.push(Segment::MoveTo(cursor));
                // M 之后的隐式重复是 L。
                command = if relative { b'l' } else { b'L' };
            }
            b'L' => {
                let Some(p) = point(&mut scanner) else { break };
                cursor = base + p;
                path.push(Segment::LineTo(cursor));
            }
            b'H' => {
                let Some(x) = scanner.number() else { break };
                cursor.x = if relative { cursor.x + x } else { x };
                path.push(Segment::LineTo(cursor));
            }
            b'V' => {
                let Some(y) = scanner.number() else { break };
                cursor.y = if relative { cursor.y + y } else { y };
                path.push(Segment::LineTo(cursor));
            }
            b'C' => {
                let (Some(a), Some(b), Some(p)) = (
                    point(&mut scanner),
                    point(&mut scanner),
                    point(&mut scanner),
                ) else {
                    break;
                };
                let (a, b, p) = (base + a, base + b, base + p);
                path.push(Segment::Cubic(a, b, p));
                reflected_cubic = Some(b);
                cursor = p;
            }
            b'S' => {
                let (Some(b), Some(p)) = (point(&mut scanner), point(&mut scanner)) else {
                    break;
                };
                let a = last_cubic.map_or(cursor, |c| cursor * 2.0 - c);
                let (b, p) = (base + b, base + p);
                path.push(Segment::Cubic(a, b, p));
                reflected_cubic = Some(b);
                cursor = p;
            }
            b'Q' => {
                let (Some(c), Some(p)) = (point(&mut scanner), point(&mut scanner)) else {
                    break;
                };
                let (c, p) = (base + c, base + p);
                path.push(Segment::Quadratic(c, p));
                reflected_quad = Some(c);
                cursor = p;
            }
            b'T' => {
                let Some(p) = point(&mut scanner) else { break };
                let c = last_quad.map_or(cursor, |q| cursor * 2.0 - q);
                let p = base + p;
                path.push(Segment::Quadratic(c, p));
                reflected_quad = Some(c);
                cursor = p;
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rotation)) =
                    (scanner.number(), scanner.number(), scanner.number())
                else {
                    break;
                };
                let (Some(large), Some(sweep)) = (scanner.flag(), scanner.flag()) else {
                    break;
                };
                let Some(p) = point(&mut scanner) else { break };
                let p = base + p;
                arc_to_cubics(&mut path, cursor, p, rx, ry, rotation, large, sweep);
                cursor = p;
            }
            b'Z' => {
                path.push(Segment::Close);
                cursor = start;
                // Z 不带参数：后面要是直接跟数字（不合规范），到此为止。
                command = 0;
            }
            _ => {
                // 不认识的命令：跳过它的参数。
                while scanner.number().is_some() {}
            }
        }
        last_cubic = reflected_cubic;
        last_quad = reflected_quad;
    }
    path
}

/// SVG 端点参数化的椭圆弧 → 若干段三次贝塞尔（每段不超过 90°）。
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    path: &mut Path,
    from: Vec2,
    to: Vec2,
    rx: f32,
    ry: f32,
    rotation: f32,
    large: bool,
    sweep: bool,
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if from.distance_squared(to) < 1e-12 {
        return;
    }
    if rx < 1e-6 || ry < 1e-6 {
        path.push(Segment::LineTo(to));
        return;
    }
    let phi = rotation.to_radians();
    let (sin, cos) = phi.sin_cos();
    // 规范 F.6.5：换到以弧为中心、轴对齐的坐标里求圆心。
    let d = (from - to) * 0.5;
    let p = Vec2::new(cos * d.x + sin * d.y, -sin * d.x + cos * d.y);
    let lambda = (p.x * p.x) / (rx * rx) + (p.y * p.y) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let numerator = rx * rx * ry * ry - rx * rx * p.y * p.y - ry * ry * p.x * p.x;
    let denominator = rx * rx * p.y * p.y + ry * ry * p.x * p.x;
    let mut coefficient = (numerator / denominator).max(0.0).sqrt();
    if large == sweep {
        coefficient = -coefficient;
    }
    let center_prime = Vec2::new(coefficient * rx * p.y / ry, -coefficient * ry * p.x / rx);
    let mid = (from + to) * 0.5;
    let center = Vec2::new(
        cos * center_prime.x - sin * center_prime.y,
        sin * center_prime.x + cos * center_prime.y,
    ) + mid;
    let angle = |u: Vec2, v: Vec2| {
        let a = u.y.atan2(u.x);
        let b = v.y.atan2(v.x);
        b - a
    };
    let u = Vec2::new((p.x - center_prime.x) / rx, (p.y - center_prime.y) / ry);
    let v = Vec2::new((-p.x - center_prime.x) / rx, (-p.y - center_prime.y) / ry);
    let theta = Vec2::X.angle_to(u);
    let mut delta = angle(u, v);
    if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    } else if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    }
    let segments = (delta.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = delta / segments as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let point_at = |t: f32| {
        let e = Vec2::new(rx * t.cos(), ry * t.sin());
        center + Vec2::new(cos * e.x - sin * e.y, sin * e.x + cos * e.y)
    };
    let derivative = |t: f32| {
        let e = Vec2::new(-rx * t.sin(), ry * t.cos());
        Vec2::new(cos * e.x - sin * e.y, sin * e.x + cos * e.y)
    };
    for s in 0..segments {
        let t0 = theta + step * s as f32;
        let t1 = t0 + step;
        let p0 = point_at(t0);
        let p1 = if s + 1 == segments { to } else { point_at(t1) };
        path.push(Segment::Cubic(
            p0 + derivative(t0) * k,
            p1 - derivative(t1) * k,
            p1,
        ));
    }
}

/// `transform` 属性 → 仿射矩阵（按书写顺序从左往右乘）。
pub fn parse_transform(text: &str) -> Affine2 {
    let mut result = Affine2::IDENTITY;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let Some(close) = rest[open..].find(')') else {
            break;
        };
        let args = numbers(&rest[open + 1..open + close]);
        let arg = |i: usize, default: f32| args.get(i).copied().unwrap_or(default);
        let t = match name {
            "matrix" if args.len() == 6 => {
                Affine2::from_cols_array(&[args[0], args[1], args[2], args[3], args[4], args[5]])
            }
            "translate" => Affine2::from_translation(Vec2::new(arg(0, 0.0), arg(1, 0.0))),
            "scale" => {
                let sx = arg(0, 1.0);
                Affine2::from_scale(Vec2::new(sx, arg(1, sx)))
            }
            "rotate" => {
                let r = Affine2::from_angle(arg(0, 0.0).to_radians());
                if args.len() >= 3 {
                    let c = Vec2::new(args[1], args[2]);
                    Affine2::from_translation(c) * r * Affine2::from_translation(-c)
                } else {
                    r
                }
            }
            "skewX" => {
                Affine2::from_cols_array(&[1.0, 0.0, arg(0, 0.0).to_radians().tan(), 1.0, 0.0, 0.0])
            }
            "skewY" => {
                Affine2::from_cols_array(&[1.0, arg(0, 0.0).to_radians().tan(), 0.0, 1.0, 0.0, 0.0])
            }
            _ => Affine2::IDENTITY,
        };
        result *= t;
        rest = &rest[open + close + 1..];
    }
    result
}

// ── 颜色 ──

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// CSS 颜色 → 线性 RGB。认不出返回 `None`。
pub fn parse_color(text: &str) -> Option<Vec3> {
    let text = text.trim().to_ascii_lowercase();
    let srgb = if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().filter_map(|c| c.to_digit(16)).collect();
        match digits.len() {
            3 | 4 => Vec3::new(digits[0] as f32, digits[1] as f32, digits[2] as f32) * 17.0 / 255.0,
            6 | 8 => {
                Vec3::new(
                    (digits[0] * 16 + digits[1]) as f32,
                    (digits[2] * 16 + digits[3]) as f32,
                    (digits[4] * 16 + digits[5]) as f32,
                ) / 255.0
            }
            _ => return None,
        }
    } else if let Some(body) = text
        .strip_prefix("rgb(")
        .or_else(|| text.strip_prefix("rgba("))
    {
        let parts: Vec<&str> = body
            .trim_end_matches(')')
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .collect();
        let channel = |s: &str| {
            if let Some(p) = s.strip_suffix('%') {
                p.parse::<f32>().unwrap_or(0.0) / 100.0
            } else {
                s.parse::<f32>().unwrap_or(0.0) / 255.0
            }
        };
        if parts.len() < 3 {
            return None;
        }
        Vec3::new(channel(parts[0]), channel(parts[1]), channel(parts[2]))
    } else if let Some(body) = text
        .strip_prefix("hsl(")
        .or_else(|| text.strip_prefix("hsla("))
    {
        let parts: Vec<f32> = body
            .trim_end_matches(')')
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.trim_end_matches(['%', 'd', 'e', 'g'])
                    .parse()
                    .unwrap_or(0.0)
            })
            .collect();
        if parts.len() < 3 {
            return None;
        }
        let (h, s, l) = (
            parts[0].rem_euclid(360.0) / 360.0,
            parts[1] / 100.0,
            parts[2] / 100.0,
        );
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let hue = |mut t: f32| {
            t = t.rem_euclid(1.0);
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        };
        Vec3::new(hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
    } else {
        let value = named_color(&text)?;
        Vec3::new(
            ((value >> 16) & 0xff) as f32,
            ((value >> 8) & 0xff) as f32,
            (value & 0xff) as f32,
        ) / 255.0
    };
    Some(Vec3::new(
        srgb_to_linear(srgb.x),
        srgb_to_linear(srgb.y),
        srgb_to_linear(srgb.z),
    ))
}

fn named_color(name: &str) -> Option<u32> {
    const NAMES: [(&str, u32); 148] = [
        ("aliceblue", 0xf0f8ff),
        ("antiquewhite", 0xfaebd7),
        ("aqua", 0x00ffff),
        ("aquamarine", 0x7fffd4),
        ("azure", 0xf0ffff),
        ("beige", 0xf5f5dc),
        ("bisque", 0xffe4c4),
        ("black", 0x000000),
        ("blanchedalmond", 0xffebcd),
        ("blue", 0x0000ff),
        ("blueviolet", 0x8a2be2),
        ("brown", 0xa52a2a),
        ("burlywood", 0xdeb887),
        ("cadetblue", 0x5f9ea0),
        ("chartreuse", 0x7fff00),
        ("chocolate", 0xd2691e),
        ("coral", 0xff7f50),
        ("cornflowerblue", 0x6495ed),
        ("cornsilk", 0xfff8dc),
        ("crimson", 0xdc143c),
        ("cyan", 0x00ffff),
        ("darkblue", 0x00008b),
        ("darkcyan", 0x008b8b),
        ("darkgoldenrod", 0xb8860b),
        ("darkgray", 0xa9a9a9),
        ("darkgreen", 0x006400),
        ("darkgrey", 0xa9a9a9),
        ("darkkhaki", 0xbdb76b),
        ("darkmagenta", 0x8b008b),
        ("darkolivegreen", 0x556b2f),
        ("darkorange", 0xff8c00),
        ("darkorchid", 0x9932cc),
        ("darkred", 0x8b0000),
        ("darksalmon", 0xe9967a),
        ("darkseagreen", 0x8fbc8f),
        ("darkslateblue", 0x483d8b),
        ("darkslategray", 0x2f4f4f),
        ("darkslategrey", 0x2f4f4f),
        ("darkturquoise", 0x00ced1),
        ("darkviolet", 0x9400d3),
        ("deeppink", 0xff1493),
        ("deepskyblue", 0x00bfff),
        ("dimgray", 0x696969),
        ("dimgrey", 0x696969),
        ("dodgerblue", 0x1e90ff),
        ("firebrick", 0xb22222),
        ("floralwhite", 0xfffaf0),
        ("forestgreen", 0x228b22),
        ("fuchsia", 0xff00ff),
        ("gainsboro", 0xdcdcdc),
        ("ghostwhite", 0xf8f8ff),
        ("gold", 0xffd700),
        ("goldenrod", 0xdaa520),
        ("gray", 0x808080),
        ("green", 0x008000),
        ("greenyellow", 0xadff2f),
        ("grey", 0x808080),
        ("honeydew", 0xf0fff0),
        ("hotpink", 0xff69b4),
        ("indianred", 0xcd5c5c),
        ("indigo", 0x4b0082),
        ("ivory", 0xfffff0),
        ("khaki", 0xf0e68c),
        ("lavender", 0xe6e6fa),
        ("lavenderblush", 0xfff0f5),
        ("lawngreen", 0x7cfc00),
        ("lemonchiffon", 0xfffacd),
        ("lightblue", 0xadd8e6),
        ("lightcoral", 0xf08080),
        ("lightcyan", 0xe0ffff),
        ("lightgoldenrodyellow", 0xfafad2),
        ("lightgray", 0xd3d3d3),
        ("lightgreen", 0x90ee90),
        ("lightgrey", 0xd3d3d3),
        ("lightpink", 0xffb6c1),
        ("lightsalmon", 0xffa07a),
        ("lightseagreen", 0x20b2aa),
        ("lightskyblue", 0x87cefa),
        ("lightslategray", 0x778899),
        ("lightslategrey", 0x778899),
        ("lightsteelblue", 0xb0c4de),
        ("lightyellow", 0xffffe0),
        ("lime", 0x00ff00),
        ("limegreen", 0x32cd32),
        ("linen", 0xfaf0e6),
        ("magenta", 0xff00ff),
        ("maroon", 0x800000),
        ("mediumaquamarine", 0x66cdaa),
        ("mediumblue", 0x0000cd),
        ("mediumorchid", 0xba55d3),
        ("mediumpurple", 0x9370db),
        ("mediumseagreen", 0x3cb371),
        ("mediumslateblue", 0x7b68ee),
        ("mediumspringgreen", 0x00fa9a),
        ("mediumturquoise", 0x48d1cc),
        ("mediumvioletred", 0xc71585),
        ("midnightblue", 0x191970),
        ("mintcream", 0xf5fffa),
        ("mistyrose", 0xffe4e1),
        ("moccasin", 0xffe4b5),
        ("navajowhite", 0xffdead),
        ("navy", 0x000080),
        ("oldlace", 0xfdf5e6),
        ("olive", 0x808000),
        ("olivedrab", 0x6b8e23),
        ("orange", 0xffa500),
        ("orangered", 0xff4500),
        ("orchid", 0xda70d6),
        ("palegoldenrod", 0xeee8aa),
        ("palegreen", 0x98fb98),
        ("paleturquoise", 0xafeeee),
        ("palevioletred", 0xdb7093),
        ("papayawhip", 0xffefd5),
        ("peachpuff", 0xffdab9),
        ("peru", 0xcd853f),
        ("pink", 0xffc0cb),
        ("plum", 0xdda0dd),
        ("powderblue", 0xb0e0e6),
        ("purple", 0x800080),
        ("rebeccapurple", 0x663399),
        ("red", 0xff0000),
        ("rosybrown", 0xbc8f8f),
        ("royalblue", 0x4169e1),
        ("saddlebrown", 0x8b4513),
        ("salmon", 0xfa8072),
        ("sandybrown", 0xf4a460),
        ("seagreen", 0x2e8b57),
        ("seashell", 0xfff5ee),
        ("sienna", 0xa0522d),
        ("silver", 0xc0c0c0),
        ("skyblue", 0x87ceeb),
        ("slateblue", 0x6a5acd),
        ("slategray", 0x708090),
        ("slategrey", 0x708090),
        ("snow", 0xfffafa),
        ("springgreen", 0x00ff7f),
        ("steelblue", 0x4682b4),
        ("tan", 0xd2b48c),
        ("teal", 0x008080),
        ("thistle", 0xd8bfd8),
        ("tomato", 0xff6347),
        ("turquoise", 0x40e0d0),
        ("violet", 0xee82ee),
        ("wheat", 0xf5deb3),
        ("white", 0xffffff),
        ("whitesmoke", 0xf5f5f5),
        ("yellow", 0xffff00),
        ("yellowgreen", 0x9acd32),
    ];
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(body: &str) -> Document {
        parse(
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{body}</svg>"#
            )
            .as_bytes(),
            0.25,
        )
        .unwrap()
    }

    #[test]
    fn path_data_handles_glued_numbers_and_implicit_commands() {
        let path = parse_path_data("M10-20l5.5.5 1 1zm1,1");
        assert_eq!(path.segments[0], Segment::MoveTo(Vec2::new(10.0, -20.0)));
        assert_eq!(path.segments[1], Segment::LineTo(Vec2::new(15.5, -19.5)));
        assert_eq!(path.segments[2], Segment::LineTo(Vec2::new(16.5, -18.5)));
        assert_eq!(path.segments[3], Segment::Close);
        // z 之后的 m 相对的是子路径起点。
        assert_eq!(path.segments[4], Segment::MoveTo(Vec2::new(11.0, -19.0)));
    }

    #[test]
    fn arc_flags_without_separators() {
        let path = parse_path_data("M0 0a10 10 0 0110 10");
        let Segment::Cubic(_, _, end) = path.segments[1] else {
            panic!("{:?}", path.segments)
        };
        assert!((end - Vec2::new(10.0, 10.0)).length() < 1e-4);
    }

    #[test]
    fn a_semicircle_arc_passes_through_the_top() {
        let path = parse_path_data("M-10 0 A10 10 0 0 1 10 0");
        let contour = &path.flatten(0.01)[0];
        let top = contour.points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        // sweep=1（顺时针，SVG 的 y 朝下）从左到右经过 y = -10。
        assert!((top + 10.0).abs() < 0.05, "{top}");
    }

    #[test]
    fn css_classes_ids_and_inline_styles_cascade() {
        let d = doc(
            r#"<style>.a { fill: red } #x { stroke: blue; stroke-width: 3 } rect.b { fill: #00ff00 }</style>
            <rect class="a" width="10" height="10"/>
            <rect id="x" class="a b" width="10" height="10" style="fill-opacity:0.5"/>"#,
        );
        assert_eq!(d.paths.len(), 2);
        assert_eq!(d.paths[0].fill.unwrap().color, Vec3::new(1.0, 0.0, 0.0));
        let second = &d.paths[1];
        assert_eq!(
            second.fill.unwrap().color,
            Vec3::new(0.0, 1.0, 0.0),
            "rect.b 比 .a 后写，覆盖它"
        );
        assert_eq!(second.fill.unwrap().opacity, 0.5);
        let stroke = second.stroke.unwrap();
        assert_eq!(stroke.paint.color, Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(stroke.style.width, 3.0);
    }

    #[test]
    fn groups_pass_styles_and_transforms_down() {
        let d = doc(
            r#"<g fill="none" stroke="black" transform="translate(10 20) scale(2)">
            <line x1="0" y1="0" x2="5" y2="0"/></g>"#,
        );
        let path = &d.paths[0];
        assert!(path.fill.is_none());
        assert_eq!(path.contours[0].points[0], Vec2::new(10.0, 20.0));
        assert_eq!(path.contours[0].points[1], Vec2::new(20.0, 20.0));
        assert_eq!(path.stroke.unwrap().style.width, 2.0, "线宽跟着缩放");
    }

    #[test]
    fn use_references_and_defs_are_not_drawn_twice() {
        let d = doc(
            r##"<defs><circle id="c" r="5" fill="red"/></defs><use href="#c" x="10" y="10"/>"##,
        );
        assert_eq!(d.paths.len(), 1);
        let xs: Vec<f32> = d.paths[0].contours[0].points.iter().map(|p| p.x).collect();
        let min = xs.iter().copied().fold(f32::MAX, f32::min);
        assert!((min - 5.0).abs() < 0.1, "{min}");
    }

    #[test]
    fn colors_and_units() {
        assert_eq!(parse_color("#fff"), Some(Vec3::ONE));
        assert_eq!(parse_color("rgb(255, 0, 0)"), Some(Vec3::X));
        assert_eq!(parse_color("Lime"), Some(Vec3::Y));
        assert!(parse_color("hsl(240, 100%, 50%)").is_some_and(|c| (c - Vec3::Z).length() < 1e-4));
        assert!((length("1in", 0.0, 16.0) - 96.0).abs() < 1e-4);
        assert!((length("50%", 200.0, 16.0) - 100.0).abs() < 1e-4);
        assert!((length("2em", 0.0, 16.0) - 32.0).abs() < 1e-4);
        assert!((length("1e1", 0.0, 16.0) - 10.0).abs() < 1e-4);
    }

    #[test]
    fn gradients_fall_back_to_their_first_stop() {
        let d = doc(
            r##"<defs><linearGradient id="g"><stop offset="0" stop-color="blue"/></linearGradient>
            <linearGradient id="h" href="#g"/></defs><rect width="1" height="1" fill="url(#h)"/>"##,
        );
        assert_eq!(d.paths[0].fill.unwrap().color, Vec3::Z);
    }

    #[test]
    fn rotate_about_a_center() {
        let t = parse_transform("rotate(90, 10, 10)");
        let p = t.transform_point2(Vec2::new(20.0, 10.0));
        assert!((p - Vec2::new(10.0, 20.0)).length() < 1e-4, "{p}");
    }

    #[test]
    fn model_layers_later_paths_in_front() {
        let d = doc(
            r#"<rect width="10" height="10" fill="red"/><rect width="5" height="5" fill="blue"/>"#,
        );
        let model = d.to_model("svg", ModelOptions::default());
        let z = |i: usize| model.meshes()[i].vertices()[0].position[2];
        assert!(z(1) > z(0));
    }
}
