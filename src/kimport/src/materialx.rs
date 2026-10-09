//! MaterialX（`.mtlx`）材质导入：`standard_surface` → 引擎材质。
//!
//! MaterialX 是一种**节点图**材质格式：表面着色器（这里只认 Autodesk
//! Standard Surface）的每个输入要么是常量，要么连着一张节点图——
//! 贴图、UV 变换、数学运算、条件选择……
//!
//! # 怎么落到引擎上
//!
//! | MaterialX | 引擎 |
//! |---|---|
//! | 常量输入 | 材质系数（基础色、金属度、粗糙度、自发光）和 [`Physical`] 的扩展参数 |
//! | 连着节点图的输入 | 编译成 WGSL 的 `material_surface` 钩子，算完交给 `physical_surface` |
//! | `image` 节点引用的图 | `custom_texture0` / `custom_texture1`（最多两张不同的图） |
//!
//! 节点图是**一次性编译**成直线代码的：每个节点一个 `let`，按依赖顺序排。
//! 没有分支、没有循环，着色器编译器能把用不到的全部折掉。
//!
//! # 支持的节点
//!
//! 图像与坐标：`image` `tiledimage` `texcoord` `place2d` `rotate2d` `rotate3d`
//! `position` `normal` `tangent` `time` `constant`；
//! 类型：`convert` `combine2/3/4` `separate2/3/4` `extract` `swizzle`（部分）；
//! 数学：`add` `subtract` `multiply` `divide` `modulo` `power` `min` `max`
//! `absval` `floor` `ceil` `sin` `cos` `sqrt` `sign` `ln` `exp` `clamp` `mix`
//! `invert` `smoothstep` `dotproduct` `crossproduct` `magnitude` `normalize`
//! `luminance` `dot`；
//! 条件：`ifgreater` `ifgreatereq` `ifequal`；
//! 法线：`normalmap` `heighttonormal`（和 MaterialX 1.39 的 GLSL 实现一致，
//! 用屏幕空间导数）。
//!
//! 不认识的节点出一条警告、输出 0，材质照常能用。
//!
//! # 没做的
//!
//! - `specular` / `specular_color`：引擎的 F0 固定按 IOR 1.5 算；
//! - 噪声节点（`noise2d`、`fractal3d`、`cellnoise`……）；
//! - 除 Standard Surface 以外的表面着色器（`gltf_pbr`、`UsdPreviewSurface`）。

use crate::xml::{self, Element};
use kasset::{LoadError, Resource};
use kmaterial::{BlendMode, Material};
use kmath::Vec3;
use kpbr::physical::{PHYSICAL_WGSL, Physical};
use kshader::Shader;
use ktexture::{Texture, TextureFormat};
use std::collections::HashMap;
use std::path::Path;

/// 导入出来的一个材质。
pub struct MaterialXMaterial {
    /// `surfacematerial` 的名字。
    pub name: String,
    /// 可以直接挂到节点上的材质。
    pub material: Material,
    /// 节点图编译出来的 WGSL（没有连节点图时为空）。调试用。
    pub shader_source: Option<String>,
    /// 不支持的节点、缺的图之类。不影响材质可用。
    pub warnings: Vec<String>,
}

/// 读一个 `.mtlx` 文件；`image` 节点引用的图按文件所在目录解析、同步读入。
pub fn load(path: impl AsRef<Path>) -> Result<Vec<MaterialXMaterial>, LoadError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source: std::sync::Arc::new(source),
    })?;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    parse(&bytes, &mut |file| {
        let bytes = std::fs::read(dir.join(file)).ok()?;
        Texture::from_encoded(&bytes).ok()
    })
}

/// 解析一份 MaterialX 文档。`image` 节点的图由 `images` 按文件名提供。
pub fn parse(
    bytes: &[u8],
    images: &mut dyn FnMut(&str) -> Option<Texture>,
) -> Result<Vec<MaterialXMaterial>, LoadError> {
    let root = xml::parse(bytes)?;
    let document = (root.name == "materialx")
        .then_some(&root)
        .or_else(|| root.child("materialx"))
        .ok_or_else(|| crate::bad("不是 MaterialX 文档（缺 <materialx> 根元素）"))?;
    let mut out = Vec::new();
    for material in document.children_named("surfacematerial") {
        let name = material.attr("name").unwrap_or("material").to_string();
        let shader = material
            .children_named("input")
            .find(|input| input.attr("name") == Some("surfaceshader"))
            .and_then(|input| input.attr("nodename"))
            .and_then(|node| find_node(document, node));
        let Some(shader) = shader else {
            klog::warn!("MaterialX 材质 {name} 没有表面着色器，跳过");
            continue;
        };
        if shader.name != "standard_surface" {
            klog::warn!(
                "MaterialX 材质 {name} 用的是 {}，只支持 standard_surface",
                shader.name
            );
            continue;
        }
        out.push(convert(document, &name, shader, images));
    }
    Ok(out)
}

// ── 值与类型 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Float,
    Vec2,
    Vec3,
    Vec4,
}

impl Ty {
    fn parse(name: &str) -> Self {
        match name {
            "vector2" => Ty::Vec2,
            "vector3" | "color3" => Ty::Vec3,
            "vector4" | "color4" => Ty::Vec4,
            _ => Ty::Float,
        }
    }

    fn width(self) -> usize {
        match self {
            Ty::Float => 1,
            Ty::Vec2 => 2,
            Ty::Vec3 => 3,
            Ty::Vec4 => 4,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Ty::Float => "f32",
            Ty::Vec2 => "vec2<f32>",
            Ty::Vec3 => "vec3<f32>",
            Ty::Vec4 => "vec4<f32>",
        }
    }
}

/// 一段 WGSL 表达式和它的类型。
#[derive(Debug, Clone)]
struct Value {
    expr: String,
    ty: Ty,
    /// 常量的数值（连着节点的输入为 `None`）。
    constant: Option<Vec<f32>>,
}

fn float_literal(v: f32) -> String {
    if v.is_finite() {
        format!("{v:?}")
    } else {
        "0.0".into()
    }
}

impl Value {
    fn constant(values: &[f32], ty: Ty) -> Self {
        let mut v = values.to_vec();
        v.resize(ty.width(), *values.last().unwrap_or(&0.0));
        let expr = if ty == Ty::Float {
            float_literal(v[0])
        } else {
            let parts: Vec<String> = v.iter().map(|x| float_literal(*x)).collect();
            format!("{}({})", ty.wgsl(), parts.join(", "))
        };
        Self {
            expr,
            ty,
            constant: Some(v),
        }
    }

    fn expr(expr: impl Into<String>, ty: Ty) -> Self {
        Self {
            expr: expr.into(),
            ty,
            constant: None,
        }
    }

    fn zero(ty: Ty) -> Self {
        Self::constant(&[0.0], ty)
    }

    /// 转成另一种类型：标量铺满、向量截断或补分量（补的 w 是 1）。
    fn cast(&self, ty: Ty) -> Value {
        if self.ty == ty {
            return self.clone();
        }
        if let Some(values) = &self.constant {
            let mut v = values.clone();
            if v.len() == 1 {
                v.resize(ty.width(), v[0]);
            } else {
                let fill = if ty == Ty::Vec4 && v.len() == 3 {
                    1.0
                } else {
                    0.0
                };
                v.resize(ty.width(), fill);
            }
            return Value::constant(&v, ty);
        }
        let e = &self.expr;
        let expr = match (self.ty, ty) {
            (Ty::Float, _) => format!("{}({e})", ty.wgsl()),
            (_, Ty::Float) => format!("({e}).x"),
            (Ty::Vec2, Ty::Vec3) => format!("vec3<f32>({e}, 0.0)"),
            (Ty::Vec2, Ty::Vec4) => format!("vec4<f32>({e}, 0.0, 1.0)"),
            (Ty::Vec3, Ty::Vec4) => format!("vec4<f32>({e}, 1.0)"),
            (Ty::Vec3 | Ty::Vec4, Ty::Vec2) => format!("({e}).xy"),
            (Ty::Vec4, Ty::Vec3) => format!("({e}).xyz"),
            _ => e.clone(),
        };
        Value::expr(expr, ty)
    }

    fn component(&self, index: usize) -> Value {
        if self.ty == Ty::Float {
            return self.clone();
        }
        let index = index.min(self.ty.width() - 1);
        Value::expr(
            format!("({}).{}", self.expr, ["x", "y", "z", "w"][index]),
            Ty::Float,
        )
    }
}

fn parse_numbers(text: &str) -> Vec<f32> {
    text.split(',')
        .filter_map(|part| part.trim().parse::<f32>().ok())
        .collect()
}

fn find_node<'a>(scope: &'a Element, name: &str) -> Option<&'a Element> {
    scope.children.iter().find(|child| {
        child.attr("name") == Some(name)
            && !matches!(child.name.as_str(), "input" | "output" | "nodegraph")
    })
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

// ── 节点图编译 ──

struct Codegen<'a> {
    document: &'a Element,
    lines: Vec<String>,
    /// (作用域名, 节点名) → 已经生成的变量。
    done: HashMap<(String, String), Value>,
    /// 用到的图：文件名 → 槽位。
    textures: Vec<(String, Texture)>,
    warnings: Vec<String>,
    images: &'a mut dyn FnMut(&str) -> Option<Texture>,
}

/// 一个作用域：文档顶层或某张节点图。`colorspace` 往下继承。
#[derive(Clone, Copy)]
struct Scope<'a> {
    element: &'a Element,
    name: &'a str,
}

impl<'a> Codegen<'a> {
    fn warn(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    /// 一个节点的某个输入。没写就用 `default`。
    fn input(&mut self, scope: Scope<'a>, node: &'a Element, name: &str, default: Value) -> Value {
        let Some(input) = node
            .children_named("input")
            .find(|input| input.attr("name") == Some(name))
        else {
            return default;
        };
        self.resolve(scope, input, default.ty)
    }

    fn has_input(node: &Element, name: &str) -> bool {
        node.children_named("input")
            .any(|input| input.attr("name") == Some(name))
    }

    /// 解析一个 `<input>`（或 `<output>`）元素指向的值。
    fn resolve(&mut self, scope: Scope<'a>, input: &'a Element, want: Ty) -> Value {
        let ty = input.attr("type").map(Ty::parse).unwrap_or(want);
        let selector = input.attr("output");
        let value = if let Some(graph) = input.attr("nodegraph") {
            self.graph_output(graph, selector.unwrap_or("out"), ty)
        } else if let Some(node) = input.attr("nodename") {
            self.node_output(scope, node, selector, ty)
        } else if let Some(interface) = input.attr("interfacename") {
            // 节点图的对外输入：值写在节点图元素上，连接关系在文档顶层。
            let outer = Scope {
                element: self.document,
                name: "",
            };
            match scope
                .element
                .children_named("input")
                .find(|i| i.attr("name") == Some(interface))
            {
                Some(declared) => self.resolve(outer, declared, ty),
                None => {
                    self.warn(format!("节点图 {} 没有输入 {interface}", scope.name));
                    Value::zero(ty)
                }
            }
        } else if let Some(text) = input.attr("value") {
            let numbers = parse_numbers(text);
            if numbers.is_empty() {
                match text {
                    "true" => Value::constant(&[1.0], ty),
                    _ => Value::zero(ty),
                }
            } else {
                Value::constant(&numbers, ty)
            }
        } else {
            Value::zero(ty)
        };
        value.cast(want)
    }

    fn graph_output(&mut self, graph: &str, output: &str, ty: Ty) -> Value {
        let document = self.document;
        let Some(element) = document
            .children_named("nodegraph")
            .find(|g| g.attr("name") == Some(graph))
        else {
            self.warn(format!("找不到节点图 {graph}"));
            return Value::zero(ty);
        };
        let Some(out) = element
            .children_named("output")
            .find(|o| o.attr("name") == Some(output))
        else {
            self.warn(format!("节点图 {graph} 没有输出 {output}"));
            return Value::zero(ty);
        };
        let scope = Scope {
            element,
            name: element.attr("name").unwrap_or(""),
        };
        self.resolve(scope, out, ty)
    }

    fn node_output(
        &mut self,
        scope: Scope<'a>,
        name: &str,
        selector: Option<&str>,
        ty: Ty,
    ) -> Value {
        let Some(node) = find_node(scope.element, name) else {
            self.warn(format!("找不到节点 {name}"));
            return Value::zero(ty);
        };
        let value = self.node(scope, node);
        // 多输出节点（separateN）：`outx` / `x` 这类选择器挑一个分量。
        match selector.and_then(|s| s.chars().last()) {
            Some(c @ ('x' | 'y' | 'z' | 'w' | 'r' | 'g' | 'b' | 'a'))
                if selector != Some("out") =>
            {
                let index = "xyzw".find(c).or_else(|| "rgba".find(c)).unwrap_or(0);
                value.component(index)
            }
            _ => value,
        }
    }

    /// 生成一个节点，返回保存它结果的变量。同一个节点只生成一次。
    fn node(&mut self, scope: Scope<'a>, node: &'a Element) -> Value {
        let key = (
            scope.name.to_string(),
            node.attr("name").unwrap_or("").to_string(),
        );
        if let Some(value) = self.done.get(&key) {
            return value.clone();
        }
        let ty = Ty::parse(node.attr("type").unwrap_or("float"));
        let expr = self.node_expr(scope, node, ty);
        // separateN 的「类型」是多输出，变量按输入的向量存。
        let ty = expr.ty;
        let var = format!("n_{}_{}", sanitize(&key.0), sanitize(&key.1));
        self.lines
            .push(format!("    let {var}: {} = {};", ty.wgsl(), expr.expr));
        let value = Value::expr(var, ty);
        self.done.insert(key, value.clone());
        value
    }

    fn colorspace(&self, scope: Scope<'a>, node: &Element) -> Option<String> {
        let file = node
            .children_named("input")
            .find(|input| input.attr("name") == Some("file"));
        file.and_then(|f| f.attr("colorspace"))
            .or_else(|| node.attr("colorspace"))
            .or_else(|| scope.element.attr("colorspace"))
            .map(str::to_string)
    }

    fn texture_slot(&mut self, file: &str) -> Option<usize> {
        if let Some(index) = self.textures.iter().position(|(name, _)| name == file) {
            return Some(index);
        }
        if self.textures.len() >= 2 {
            self.warn(format!("最多支持两张不同的图，{file} 被忽略"));
            return None;
        }
        match (self.images)(file) {
            Some(texture) => {
                self.textures
                    .push((file.to_string(), texture.with_format(TextureFormat::Linear)));
                Some(self.textures.len() - 1)
            }
            None => {
                self.warn(format!("读不到图 {file}"));
                None
            }
        }
    }

    fn node_expr(&mut self, scope: Scope<'a>, node: &'a Element, ty: Ty) -> Value {
        let zero = Value::zero(ty);
        let f = |v: f32| Value::constant(&[v], Ty::Float);
        let category = node.name.as_str();
        let uv = Value::expr("s.uv", Ty::Vec2);
        match category {
            "constant" | "dot" => {
                let key = if category == "constant" {
                    "value"
                } else {
                    "in"
                };
                self.input(scope, node, key, zero)
            }
            "texcoord" => uv.cast(ty),
            "time" => Value::expr("s.time", Ty::Float),
            "position" => Value::expr("s.world_position", Ty::Vec3).cast(ty),
            "normal" => Value::expr("s.geometric_normal", Ty::Vec3).cast(ty),
            "tangent" => Value::expr("s.tangent", Ty::Vec3).cast(ty),
            "bitangent" => Value::expr("s.bitangent", Ty::Vec3).cast(ty),
            "image" | "tiledimage" => {
                let default = self.input(scope, node, "default", zero.clone());
                let mut texcoord = self.input(scope, node, "texcoord", uv);
                if category == "tiledimage" {
                    let tiling =
                        self.input(scope, node, "uvtiling", Value::constant(&[1.0], Ty::Vec2));
                    let offset = self.input(scope, node, "uvoffset", Value::zero(Ty::Vec2));
                    texcoord = Value::expr(
                        format!(
                            "({}) * ({}) + ({})",
                            texcoord.expr, tiling.expr, offset.expr
                        ),
                        Ty::Vec2,
                    );
                }
                let file = node
                    .children_named("input")
                    .find(|input| input.attr("name") == Some("file"))
                    .and_then(|input| input.attr("value"));
                let Some(slot) = file.and_then(|file| self.texture_slot(file)) else {
                    return default;
                };
                // MaterialX 的 v 朝上，贴图的第一行在顶上。
                let sample = format!(
                    "textureSample(custom_texture{slot}, base_color_sampler, vec2<f32>(({t}).x, 1.0 - ({t}).y))",
                    t = texcoord.expr
                );
                let srgb = self
                    .colorspace(scope, node)
                    .is_some_and(|cs| cs.contains("srgb"));
                let rgba = if srgb {
                    format!("mtlx_srgb_to_linear({sample})")
                } else {
                    sample
                };
                Value::expr(rgba, Ty::Vec4).cast(ty)
            }
            "convert" => self
                .raw_input(scope, node, "in")
                .map_or(zero, |v| v.cast(ty)),
            "combine2" | "combine3" | "combine4" => {
                let count = ty.width();
                let parts: Vec<String> = (1..=count)
                    .map(|i| self.input(scope, node, &format!("in{i}"), f(0.0)).expr)
                    .collect();
                Value::expr(format!("{}({})", ty.wgsl(), parts.join(", ")), ty)
            }
            "separate2" | "separate3" | "separate4" => {
                let width = category[8..].parse::<usize>().unwrap_or(3);
                let vty = [Ty::Float, Ty::Float, Ty::Vec2, Ty::Vec3, Ty::Vec4][width];
                self.input(scope, node, "in", Value::zero(vty))
            }
            "extract" => {
                let raw = self
                    .raw_input(scope, node, "in")
                    .unwrap_or(Value::zero(Ty::Vec3));
                let index = node
                    .children_named("input")
                    .find(|input| input.attr("name") == Some("index"))
                    .and_then(|input| input.attr("value"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                raw.component(index)
            }
            "add" | "subtract" | "multiply" | "divide" | "modulo" | "power" | "min" | "max" => {
                let identity = if matches!(category, "multiply" | "divide" | "power") {
                    1.0
                } else {
                    0.0
                };
                let a = self.input(scope, node, "in1", Value::constant(&[identity], ty));
                let b = self.input(scope, node, "in2", Value::constant(&[identity], ty));
                let (a, b) = (a.expr, b.expr);
                let expr = match category {
                    "add" => format!("({a}) + ({b})"),
                    "subtract" => format!("({a}) - ({b})"),
                    "multiply" => format!("({a}) * ({b})"),
                    "divide" => format!("({a}) / ({b})"),
                    "modulo" => format!("({a}) - ({b}) * floor(({a}) / ({b}))"),
                    "power" => format!("pow({a}, {b})"),
                    "min" => format!("min({a}, {b})"),
                    _ => format!("max({a}, {b})"),
                };
                Value::expr(expr, ty)
            }
            "absval" | "floor" | "ceil" | "sin" | "cos" | "tan" | "sqrt" | "sign" | "ln"
            | "exp" | "normalize" => {
                let a = self.input(scope, node, "in", zero).expr;
                let function = match category {
                    "absval" => "abs",
                    "ln" => "log",
                    other => other,
                };
                Value::expr(format!("{function}({a})"), ty)
            }
            "clamp" => {
                let a = self.input(scope, node, "in", zero.clone()).expr;
                let low = self
                    .input(scope, node, "low", Value::constant(&[0.0], ty))
                    .expr;
                let high = self
                    .input(scope, node, "high", Value::constant(&[1.0], ty))
                    .expr;
                Value::expr(format!("clamp({a}, {low}, {high})"), ty)
            }
            "smoothstep" => {
                let a = self.input(scope, node, "in", zero.clone()).expr;
                let low = self
                    .input(scope, node, "low", Value::constant(&[0.0], ty))
                    .expr;
                let high = self
                    .input(scope, node, "high", Value::constant(&[1.0], ty))
                    .expr;
                Value::expr(format!("smoothstep({low}, {high}, {a})"), ty)
            }
            "mix" => {
                let fg = self.input(scope, node, "fg", zero.clone()).expr;
                let bg = self.input(scope, node, "bg", zero.clone()).expr;
                let amount = self.input(scope, node, "mix", f(0.0)).cast(ty).expr;
                Value::expr(format!("mix({bg}, {fg}, {amount})"), ty)
            }
            "invert" => {
                let a = self.input(scope, node, "in", zero).expr;
                let amount = self
                    .input(scope, node, "amount", Value::constant(&[1.0], ty))
                    .expr;
                Value::expr(format!("({amount}) - ({a})"), ty)
            }
            "dotproduct" => {
                let a = self.input(scope, node, "in1", Value::zero(Ty::Vec3)).expr;
                let b = self.input(scope, node, "in2", Value::zero(Ty::Vec3)).expr;
                Value::expr(format!("dot({a}, {b})"), Ty::Float)
            }
            "crossproduct" => {
                let a = self.input(scope, node, "in1", Value::zero(Ty::Vec3)).expr;
                let b = self.input(scope, node, "in2", Value::zero(Ty::Vec3)).expr;
                Value::expr(format!("cross({a}, {b})"), Ty::Vec3)
            }
            "magnitude" => {
                let a = self
                    .raw_input(scope, node, "in")
                    .unwrap_or(Value::zero(Ty::Vec3));
                Value::expr(format!("length({})", a.expr), Ty::Float)
            }
            "luminance" => {
                let a = self
                    .input(scope, node, "in", Value::zero(Ty::Vec3))
                    .cast(Ty::Vec3)
                    .expr;
                let l = format!("dot({a}, vec3<f32>(0.2722287, 0.6740818, 0.0536895))");
                Value::expr(l, Ty::Float).cast(ty)
            }
            "ifgreater" | "ifgreatereq" | "ifequal" => {
                let a = self.input(scope, node, "value1", f(1.0)).expr;
                let b = self.input(scope, node, "value2", f(0.0)).expr;
                let yes = self.input(scope, node, "in1", zero.clone()).expr;
                let no = self.input(scope, node, "in2", zero).expr;
                let op = match category {
                    "ifgreater" => ">",
                    "ifgreatereq" => ">=",
                    _ => "==",
                };
                Value::expr(format!("select({no}, {yes}, ({a}) {op} ({b}))"), ty)
            }
            "rotate2d" => {
                let a = self.input(scope, node, "in", Value::zero(Ty::Vec2)).expr;
                let amount = self.input(scope, node, "amount", f(0.0)).expr;
                Value::expr(format!("mtlx_rotate2d({a}, {amount})"), Ty::Vec2)
            }
            "rotate3d" => {
                let a = self.input(scope, node, "in", Value::zero(Ty::Vec3)).expr;
                let amount = self.input(scope, node, "amount", f(0.0)).expr;
                let axis = self
                    .input(
                        scope,
                        node,
                        "axis",
                        Value::constant(&[0.0, 1.0, 0.0], Ty::Vec3),
                    )
                    .expr;
                Value::expr(format!("mtlx_rotate3d({a}, {amount}, {axis})"), Ty::Vec3)
            }
            "place2d" => {
                let t = self.input(scope, node, "texcoord", uv).expr;
                let pivot = self.input(scope, node, "pivot", Value::zero(Ty::Vec2)).expr;
                let scale = self
                    .input(scope, node, "scale", Value::constant(&[1.0], Ty::Vec2))
                    .expr;
                let rotate = self.input(scope, node, "rotate", f(0.0)).expr;
                let offset = self
                    .input(scope, node, "offset", Value::zero(Ty::Vec2))
                    .expr;
                let order = self.input(scope, node, "operationorder", f(0.0)).expr;
                Value::expr(
                    format!("mtlx_place2d({t}, {pivot}, {scale}, {rotate}, {offset}, {order})"),
                    Ty::Vec2,
                )
            }
            "normalmap" => {
                let a = self
                    .input(
                        scope,
                        node,
                        "in",
                        Value::constant(&[0.5, 0.5, 1.0], Ty::Vec3),
                    )
                    .expr;
                let scale = self
                    .input(scope, node, "scale", f(1.0))
                    .cast(Ty::Float)
                    .expr;
                Value::expr(format!("mtlx_normalmap({a}, {scale}, s)"), Ty::Vec3)
            }
            "heighttonormal" => {
                let h = self.input(scope, node, "in", f(0.0)).expr;
                let scale = self.input(scope, node, "scale", f(1.0)).expr;
                let t = self.input(scope, node, "texcoord", uv).expr;
                Value::expr(format!("mtlx_heighttonormal({h}, {scale}, {t})"), Ty::Vec3)
            }
            other => {
                self.warn(format!(
                    "不支持的节点 {other}（{}），按 0 处理",
                    node.attr("name").unwrap_or("")
                ));
                zero
            }
        }
    }

    /// 输入按它**自己声明的**类型取（`convert`、`extract` 要知道源类型）。
    fn raw_input(&mut self, scope: Scope<'a>, node: &'a Element, name: &str) -> Option<Value> {
        let input = node
            .children_named("input")
            .find(|input| input.attr("name") == Some(name))?;
        let ty = Ty::parse(input.attr("type").unwrap_or("float"));
        Some(self.resolve(scope, input, ty))
    }
}

/// 节点图用到的辅助函数。和 MaterialX 1.39 的 GLSL 实现逐个对应。
const HELPERS_WGSL: &str = r#"
fn mtlx_srgb_to_linear(c: vec4<f32>) -> vec4<f32> {
    let lo = c.rgb / 12.92;
    let hi = pow((c.rgb + 0.055) / 1.055, vec3<f32>(2.4));
    return vec4<f32>(select(hi, lo, c.rgb <= vec3<f32>(0.04045)), c.a);
}

fn mtlx_rotate2d(v: vec2<f32>, degrees: f32) -> vec2<f32> {
    let r = radians(degrees);
    let c = cos(r);
    let s = sin(r);
    return vec2<f32>(c * v.x + s * v.y, -s * v.x + c * v.y);
}

fn mtlx_rotate3d(v: vec3<f32>, degrees: f32, axis_in: vec3<f32>) -> vec3<f32> {
    let axis = normalize(axis_in);
    let r = radians(degrees);
    let c = cos(r);
    let s = sin(r);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

fn mtlx_place2d(t: vec2<f32>, pivot: vec2<f32>, scale: vec2<f32>, degrees: f32, offset: vec2<f32>, order: f32) -> vec2<f32> {
    let centered = t - pivot;
    if (order < 0.5) {
        // SRT：先缩放、再旋转、最后平移。
        return mtlx_rotate2d(centered / scale, degrees) - offset + pivot;
    }
    // TRS
    return mtlx_rotate2d(centered - offset, degrees) / scale + pivot;
}

fn mtlx_normalmap(encoded: vec3<f32>, scale: f32, s: Surface) -> vec3<f32> {
    if (all(encoded == vec3<f32>(0.0))) {
        return s.geometric_normal;
    }
    var v = encoded * 2.0 - 1.0;
    v = vec3<f32>(v.xy * scale, v.z);
    return normalize(s.tangent * v.x + s.bitangent * v.y + s.geometric_normal * v.z);
}

fn mtlx_heighttonormal(height: f32, scale: f32, t: vec2<f32>) -> vec3<f32> {
    // 和 Sobel 滤波的尺度对齐。
    let dh = vec2<f32>(dpdx(height), dpdy(height)) * scale * (1.0 / 16.0);
    let du = vec2<f32>(dpdx(t.x), dpdy(t.x));
    let dv = vec2<f32>(dpdx(t.y), dpdy(t.y));
    var n = cross(vec3<f32>(du.x, dv.x, dh.x), vec3<f32>(du.y, dv.y, dh.y));
    if (dot(n, n) < 1e-12) {
        n = vec3<f32>(0.0, 0.0, 1.0);
    } else if (n.z < 0.0) {
        n = -n;
    }
    return normalize(n) * 0.5 + 0.5;
}
"#;

/// Standard Surface 的一个输入：常量值或连着的节点。
fn surface_input<'a>(
    codegen: &mut Codegen<'a>,
    shader: &'a Element,
    name: &str,
    default: &[f32],
    ty: Ty,
) -> Value {
    let top = Scope {
        element: codegen.document,
        name: "",
    };
    codegen.input(top, shader, name, Value::constant(default, ty))
}

fn scalar(value: &Value) -> Option<f32> {
    value.constant.as_ref().map(|v| v[0])
}

fn vec3(value: &Value) -> Option<Vec3> {
    value
        .constant
        .as_ref()
        .map(|v| Vec3::new(v[0], v[1.min(v.len() - 1)], v[2.min(v.len() - 1)]))
}

fn convert<'a>(
    document: &'a Element,
    name: &str,
    shader: &'a Element,
    images: &'a mut dyn FnMut(&str) -> Option<Texture>,
) -> MaterialXMaterial {
    let mut codegen = Codegen {
        document,
        lines: Vec::new(),
        done: HashMap::new(),
        textures: Vec::new(),
        warnings: Vec::new(),
        images,
    };
    let c = &mut codegen;
    let base = surface_input(c, shader, "base", &[1.0], Ty::Float);
    let base_color = surface_input(c, shader, "base_color", &[0.8], Ty::Vec3);
    let metalness = surface_input(c, shader, "metalness", &[0.0], Ty::Float);
    let roughness = surface_input(c, shader, "specular_roughness", &[0.2], Ty::Float);
    // opacity 在规范里是 color3，实际文件里常写成 float——按三通道平均。
    let opacity = surface_input(c, shader, "opacity", &[1.0], Ty::Vec3);
    let emission = surface_input(c, shader, "emission", &[0.0], Ty::Float);
    let emission_color = surface_input(c, shader, "emission_color", &[1.0], Ty::Vec3);
    let normal = Codegen::has_input(shader, "normal")
        .then(|| surface_input(c, shader, "normal", &[0.0, 0.0, 1.0], Ty::Vec3));

    // 扩展参数只认常量。
    let constant = |c: &mut Codegen<'a>, input: &str, default: f32| -> f32 {
        let value = surface_input(c, shader, input, &[default], Ty::Float);
        scalar(&value).unwrap_or_else(|| {
            c.warn(format!(
                "{input} 连着节点图，扩展参数只支持常量，按 {default} 处理"
            ));
            default
        })
    };
    let transmission = constant(c, "transmission", 0.0);
    let ior = constant(c, "specular_IOR", 1.5);
    let sheen = constant(c, "sheen", 0.0);
    let sheen_roughness = constant(c, "sheen_roughness", 0.3);
    let coat = constant(c, "coat", 0.0);
    let coat_roughness = constant(c, "coat_roughness", 0.1);
    let film = constant(c, "thin_film_thickness", 0.0);
    let anisotropy = constant(c, "specular_anisotropy", 0.0);
    let rotation = constant(c, "specular_rotation", 0.0);
    let sheen_color =
        vec3(&surface_input(c, shader, "sheen_color", &[1.0], Ty::Vec3)).unwrap_or(Vec3::ONE);
    // three.js 的 MaterialXLoader 也读 `ior`（不在 Standard Surface 规范里）。
    let ior = if Codegen::has_input(shader, "ior") {
        constant(c, "ior", ior)
    } else {
        ior
    };
    for unsupported in ["specular", "specular_color", "subsurface", "thin_walled"] {
        if Codegen::has_input(shader, unsupported) {
            c.warn(format!("{unsupported} 不支持，忽略"));
        }
    }

    // ── 常量部分落到材质系数上 ──
    let opacity_constant = vec3(&opacity).map(|o| (o.x + o.y + o.z) / 3.0);
    let color_constant = match (scalar(&base), vec3(&base_color)) {
        (Some(b), Some(color)) => color * b,
        _ => Vec3::ONE,
    };
    let emissive_constant = match (scalar(&emission), vec3(&emission_color)) {
        (Some(e), Some(color)) => color * e,
        _ => Vec3::ZERO,
    };
    let mut material = Material::standard()
        .with_name(name)
        .with_base_color(color_constant.extend(opacity_constant.unwrap_or(1.0)))
        .with_metallic(scalar(&metalness).unwrap_or(0.0))
        .with_roughness(scalar(&roughness).unwrap_or(0.2));
    if emissive_constant != Vec3::ZERO {
        material.set(kpbr::standard::EMISSIVE, emissive_constant);
    }
    let physical = Physical {
        transmission,
        ior,
        sheen: sheen_color * sheen,
        sheen_roughness,
        clearcoat: coat,
        clearcoat_roughness: coat_roughness,
        iridescence: if film > 0.0 { 1.0 } else { 0.0 },
        film_thickness: film,
        anisotropy,
        anisotropy_rotation: rotation * std::f32::consts::TAU,
        ..Physical::default()
    };

    // ── 连着节点图的部分生成钩子 ──
    let mut assignments = Vec::new();
    if base.constant.is_none() || base_color.constant.is_none() {
        assignments.push(format!(
            "    out.base_color = vec4<f32>(({}) * ({}), out.base_color.a);",
            base_color.expr, base.expr
        ));
    }
    if metalness.constant.is_none() {
        assignments.push(format!("    out.metallic = {};", metalness.expr));
    }
    if roughness.constant.is_none() {
        assignments.push(format!("    out.roughness = {};", roughness.expr));
    }
    if opacity.constant.is_none() {
        assignments.push(format!(
            "    out.base_color.a = dot({}, vec3<f32>(1.0 / 3.0));",
            opacity.expr
        ));
    }
    if emission.constant.is_none() || emission_color.constant.is_none() {
        assignments.push(format!(
            "    out.emissive = ({}) * ({});",
            emission_color.expr, emission.expr
        ));
    }
    if let Some(normal) = normal.filter(|n| n.constant.is_none()) {
        assignments.push(format!("    out.normal = normalize({});", normal.expr));
    }

    let has_graph = !assignments.is_empty();
    if physical.is_needed() || has_graph {
        physical.apply(&mut material);
    }
    let blended = opacity_constant.is_none_or(|o| o < 1.0);
    if blended {
        material.set_blend_mode(BlendMode::Alpha);
    }

    let shader_source = has_graph.then(|| {
        format!(
            "{PHYSICAL_WGSL}\n{HELPERS_WGSL}\nfn material_surface(s: Surface) -> Surface {{\n    var out = s;\n{}\n{}\n    return physical_surface(out);\n}}\n",
            codegen.lines.join("\n"),
            assignments.join("\n"),
        )
    });
    if let Some(source) = &shader_source {
        material.set_shader(Resource::new_ok(
            format!("materialx/{}.wgsl", sanitize(name)),
            Shader::snippet(source.clone()),
        ));
        for (slot, (file, texture)) in codegen.textures.iter().enumerate() {
            material.set_custom_texture(slot, Resource::new_ok(file.clone(), texture.clone()));
        }
    }
    for warning in &codegen.warnings {
        klog::warn!("MaterialX {name}：{warning}");
    }
    MaterialXMaterial {
        name: name.to_string(),
        material,
        shader_source,
        warnings: codegen.warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(_: &str) -> Option<Texture> {
        Some(Texture::new(1, 1, vec![255; 4]))
    }

    fn parse_str(text: &str) -> Vec<MaterialXMaterial> {
        parse(text.as_bytes(), &mut white).expect("解析失败")
    }

    const CONSTANTS: &str = r#"<?xml version="1.0"?>
<materialx version="1.39">
  <surfacematerial name="mat" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="ss" />
  </surfacematerial>
  <standard_surface name="ss" type="surfaceshader">
    <input name="base_color" type="color3" value="0.6, 0.8, 0.4" />
    <input name="base" type="float" value="0.5" />
    <input name="metalness" type="float" value="1.0" />
    <input name="specular_roughness" type="float" value="0.25" />
    <input name="opacity" type="float" value="0.7" />
  </standard_surface>
</materialx>"#;

    #[test]
    fn constants_become_material_factors() {
        let materials = parse_str(CONSTANTS);
        assert_eq!(materials.len(), 1);
        let m = &materials[0];
        assert_eq!(m.name, "mat");
        assert!(m.shader_source.is_none(), "全是常量时不需要生成钩子");
        let color = m.material.base_color();
        assert!((color.x - 0.3).abs() < 1e-6 && (color.y - 0.4).abs() < 1e-6);
        assert!((color.w - 0.7).abs() < 1e-6);
        assert_eq!(m.material.blend_mode(), BlendMode::Alpha);
        assert!((m.material.metallic() - 1.0).abs() < 1e-6);
        assert!((m.material.roughness() - 0.25).abs() < 1e-6);
    }

    const GRAPH: &str = r#"<?xml version="1.0"?>
<materialx version="1.39">
  <surfacematerial name="mat" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="ss" />
  </surfacematerial>
  <standard_surface name="ss" type="surfaceshader">
    <input name="base_color" type="color3" output="out" nodegraph="g" />
  </standard_surface>
  <nodegraph name="g">
    <texcoord name="tc" type="vector2" />
    <separate2 name="sep" type="vector2">
      <input name="in" type="vector2" nodename="tc" />
    </separate2>
    <combine3 name="c3" type="color3">
      <input name="in1" type="float" nodename="sep" output="outy" />
      <input name="in2" type="float" nodename="sep" output="x" />
      <input name="in3" type="float" value="0.25" />
    </combine3>
    <image name="img" type="color3">
      <input name="file" type="filename" value="a.png" colorspace="srgb_texture" />
    </image>
    <mix name="m" type="color3">
      <input name="fg" type="color3" nodename="c3" />
      <input name="bg" type="color3" nodename="img" />
      <input name="mix" type="float" value="0.5" />
    </mix>
    <mystery name="unknown" type="float" />
    <output name="out" type="color3" nodename="m" />
  </nodegraph>
</materialx>"#;

    #[test]
    fn a_graph_compiles_to_a_surface_hook() {
        let materials = parse_str(GRAPH);
        let m = &materials[0];
        let source = m.shader_source.as_deref().expect("连着节点图要生成钩子");
        assert!(source.contains("fn material_surface"));
        assert!(source.contains("return physical_surface(out);"));
        // 多输出选择器：`outy` 和 `x` 都认。
        assert!(source.contains("(n_g_sep).y"), "{source}");
        assert!(source.contains("(n_g_sep).x"), "{source}");
        assert!(source.contains("custom_texture0"));
        assert!(source.contains("mtlx_srgb_to_linear"), "sRGB 图要先解码");
        // 每个节点只生成一次，按依赖顺序。
        let tc = source.find("let n_g_tc").unwrap();
        let sep = source.find("let n_g_sep").unwrap();
        assert!(tc < sep);
        assert_eq!(source.matches("let n_g_tc").count(), 1);
        assert!(m.material.custom_texture(0).is_some());
    }

    #[test]
    fn unused_unknown_nodes_do_not_warn_but_used_ones_do() {
        let materials = parse_str(GRAPH);
        assert!(
            materials[0].warnings.is_empty(),
            "{:?}",
            materials[0].warnings
        );
        let used = GRAPH.replace(
            r#"<input name="mix" type="float" value="0.5" />"#,
            r#"<input name="mix" type="float" nodename="unknown" />"#,
        );
        let materials = parse_str(&used);
        assert!(materials[0].warnings.iter().any(|w| w.contains("mystery")));
    }

    #[test]
    fn nodes_with_the_same_name_in_different_graphs_are_distinct() {
        let text = r#"<materialx>
  <surfacematerial name="mat" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="ss" />
  </surfacematerial>
  <standard_surface name="ss" type="surfaceshader">
    <input name="base_color" type="color3" output="out" nodegraph="a" />
    <input name="specular_roughness" type="float" output="out" nodegraph="b" />
  </standard_surface>
  <nodegraph name="a">
    <constant name="k" type="color3"><input name="value" type="color3" value="1, 0, 0" /></constant>
    <output name="out" type="color3" nodename="k" />
  </nodegraph>
  <nodegraph name="b">
    <constant name="k" type="float"><input name="value" type="float" value="0.5" /></constant>
    <output name="out" type="float" nodename="k" />
  </nodegraph>
</materialx>"#;
        let source = parse_str(text)[0].shader_source.clone().unwrap();
        assert!(source.contains("let n_a_k: vec3<f32>"));
        assert!(source.contains("let n_b_k: f32"));
        assert!(source.contains("out.roughness = n_b_k;"));
    }

    #[test]
    fn scalar_extensions_map_to_physical_parameters() {
        let text = CONSTANTS.replace(
            r#"<input name="opacity" type="float" value="0.7" />"#,
            r#"<input name="sheen" type="float" value="1.0" />
    <input name="sheen_color" type="color3" value="1, 0, 0" />"#,
        );
        let m = &parse_str(&text)[0];
        let sheen = m
            .material
            .param(2)
            .and_then(kmaterial::MaterialValue::as_vec4)
            .expect("绒感写在第三个参数槽");
        assert_eq!(sheen.truncate(), Vec3::X);
        assert_ne!(m.material.blend_mode(), BlendMode::Alpha);
    }

    #[test]
    fn a_document_without_materialx_root_is_rejected() {
        assert!(parse(b"<foo/>", &mut white).is_err());
    }
}
