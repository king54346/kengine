//! IFC（Industry Foundation Classes）：建筑信息模型，STEP（ISO 10303-21）文本编码。
//!
//! three.js 的例子用的是 web-ifc（C++ 编译成 WASM 的完整几何内核）。这里是
//! 纯 Rust 的**子集**实现：够把常见的 Revit / ArchiCAD 导出画出来，不是
//! 一个 IFC 几何内核。
//!
//! # 支持的几何
//!
//! | 实体 | |
//! |---|---|
//! | `IfcExtrudedAreaSolid` | 矩形 / 圆 / 任意闭合轮廓（含洞）沿任意方向拉伸 |
//! | 轮廓曲线 | `IfcPolyline`、`IfcCompositeCurve`、`IfcTrimmedCurve`（圆弧，参数或点裁剪）、`IfcCircle` |
//! | `IfcFacetedBrep` / `IfcFaceBasedSurfaceModel` / `IfcShellBasedSurfaceModel` | 多边形面（含内环） |
//! | `IfcTriangulatedFaceSet` / `IfcPolygonalFaceSet` | IFC4 的网格 |
//! | `IfcMappedItem` | 表示映射 + 笛卡尔变换算子（实例化） |
//! | `IfcBooleanClippingResult` / `IfcBooleanResult` | **只画第一个操作数**，不做布尔运算 |
//!
//! # 不做的事（都是已知的差异）
//!
//! - **开洞不挖**：`IfcOpeningElement` 通过 `IfcRelVoidsElement` 在墙上挖门窗洞，
//!   那需要实体布尔运算。这里直接不画开洞体，墙是完整的，门窗嵌在墙里。
//! - `IfcSpace`（房间体积）不画——web-ifc 默认也不画。
//! - 半空间裁剪（屋顶斜切墙顶）不做，被裁的墙会高出一截。
//! - 扫掠曲面、NURBS 曲面、`IfcSweptDiskSolid` 不支持，打一条警告后跳过。
//!
//! # 颜色
//!
//! 优先 `IfcStyledItem`（直接挂在几何项上的表面样式），其次元素关联材质的
//! 样式（`IfcRelAssociatesMaterial` → `IfcMaterialDefinitionRepresentation`），
//! 都没有时按元素类型给一个默认色（窗和幕墙板是半透明的蓝灰玻璃）。
//!
//! # 为什么按颜色合并网格
//!
//! 一栋楼几千个构件，一个构件一个网格就是几千次绘制调用。构件身份在这个
//! 查看场景里用不上，所以同色的全部并成一块网格，整栋楼几十次绘制。

use crate::{bad, flat_model, limits, loader, path};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmaterial::Material;
use kmath::{Mat4, Quat, Vec2, Vec3, Vec4};
use kmesh::{Mesh, Vertex};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

loader! {
    /// 读 `.ifc`（STEP 文本编码）。
    IfcLoader -> Model : ["ifc"] = MODEL_TYPE_UUID, parse
}

// ───────────────────────────── STEP ─────────────────────────────

/// STEP 的一个参数值。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `$`（未设置）或 `*`（派生）。
    Null,
    /// `#123`。
    Ref(u32),
    /// 整数。
    Int(i64),
    /// 实数。
    Real(f64),
    /// `'文本'`。
    Str(String),
    /// `.ENUM.`。
    Enum(String),
    /// `( ... )`。
    List(Vec<Value>),
    /// `IFCPARAMETERVALUE(0.5)` 这种带类型的值。
    Typed(String, Box<Value>),
}

impl Value {
    fn as_ref(&self) -> Option<u32> {
        match self {
            Value::Ref(r) => Some(*r),
            _ => None,
        }
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            Value::Typed(_, v) => v.as_f64(),
            _ => None,
        }
    }
    fn list(&self) -> &[Value] {
        match self {
            Value::List(v) => v,
            _ => &[],
        }
    }
    fn as_enum(&self) -> Option<&str> {
        match self {
            Value::Enum(e) => Some(e),
            _ => None,
        }
    }
}

/// 一个实体实例。
#[derive(Debug, Clone)]
pub struct Entity {
    /// 类型名（大写，例如 `IFCWALL`）。
    pub kind: String,
    /// 参数。
    pub args: Vec<Value>,
}

impl Entity {
    fn arg(&self, index: usize) -> &Value {
        self.args.get(index).unwrap_or(&Value::Null)
    }
}

struct StepReader<'a> {
    text: &'a [u8],
    at: usize,
}

impl StepReader<'_> {
    fn skip_space(&mut self) {
        loop {
            while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.text[self.at..].starts_with(b"/*") {
                match self.text[self.at + 2..].windows(2).position(|w| w == b"*/") {
                    Some(end) => self.at += end + 4,
                    None => self.at = self.text.len(),
                }
                continue;
            }
            break;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, LoadError> {
        if depth > 64 {
            return Err(bad("IFC 参数嵌套过深"));
        }
        self.skip_space();
        let Some(&c) = self.text.get(self.at) else { return Err(bad("IFC 被截断")) };
        match c {
            b'$' | b'*' => {
                self.at += 1;
                Ok(Value::Null)
            }
            b'#' => {
                self.at += 1;
                let start = self.at;
                while self.at < self.text.len() && self.text[self.at].is_ascii_digit() {
                    self.at += 1;
                }
                let n = std::str::from_utf8(&self.text[start..self.at]).ok().and_then(|s| s.parse().ok()).ok_or_else(|| bad("IFC 引用写坏了"))?;
                Ok(Value::Ref(n))
            }
            b'\'' => {
                self.at += 1;
                let mut out = Vec::new();
                while self.at < self.text.len() {
                    let b = self.text[self.at];
                    self.at += 1;
                    if b == b'\'' {
                        if self.text.get(self.at) == Some(&b'\'') {
                            out.push(b'\'');
                            self.at += 1;
                            continue;
                        }
                        break;
                    }
                    out.push(b);
                }
                Ok(Value::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            b'.' => {
                self.at += 1;
                let start = self.at;
                while self.at < self.text.len() && self.text[self.at] != b'.' {
                    self.at += 1;
                }
                let e = String::from_utf8_lossy(&self.text[start..self.at]).into_owned();
                self.at += 1;
                Ok(Value::Enum(e))
            }
            b'(' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.text.get(self.at) {
                        Some(b')') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        Some(_) => items.push(self.value(depth + 1)?),
                        None => return Err(bad("IFC 列表没有闭合")),
                    }
                }
                Ok(Value::List(items))
            }
            b'"' => {
                // 二进制串，几何里用不到。
                self.at += 1;
                while self.at < self.text.len() && self.text[self.at] != b'"' {
                    self.at += 1;
                }
                self.at += 1;
                Ok(Value::Null)
            }
            c if c.is_ascii_alphabetic() => {
                let start = self.at;
                while self.at < self.text.len() && (self.text[self.at].is_ascii_alphanumeric() || self.text[self.at] == b'_') {
                    self.at += 1;
                }
                let name = String::from_utf8_lossy(&self.text[start..self.at]).to_ascii_uppercase();
                self.skip_space();
                let inner = self.value(depth + 1)?;
                // `IFCLENGTHMEASURE(1.)` 解析出来是只含一个值的列表，拆掉这一层。
                let inner = match inner {
                    Value::List(mut v) if v.len() == 1 => v.remove(0),
                    other => other,
                };
                Ok(Value::Typed(name, Box::new(inner)))
            }
            _ => {
                let start = self.at;
                while self.at < self.text.len() && matches!(self.text[self.at], b'0'..=b'9' | b'-' | b'+' | b'.' | b'E' | b'e') {
                    self.at += 1;
                }
                let token = std::str::from_utf8(&self.text[start..self.at]).unwrap_or("");
                if token.is_empty() {
                    return Err(bad(format!("IFC 里出现了意外的字符 {:?}", c as char)));
                }
                if let Ok(i) = token.parse::<i64>() {
                    Ok(Value::Int(i))
                } else {
                    Ok(Value::Real(token.parse::<f64>().map_err(|_| bad(format!("IFC 数写坏了：{token}")))?))
                }
            }
        }
    }
}

/// 解析 `DATA;` 段，返回 `实例号 → 实体`。
pub fn parse_step(text: &[u8]) -> Result<HashMap<u32, Entity>, LoadError> {
    let data = text.windows(5).position(|w| w == b"DATA;").ok_or_else(|| bad("不是 STEP 文件（找不到 DATA 段）"))?;
    let mut reader = StepReader { text, at: data + 5 };
    let mut entities = HashMap::new();
    loop {
        reader.skip_space();
        if reader.at >= text.len() || text[reader.at..].starts_with(b"ENDSEC") {
            break;
        }
        if text[reader.at] != b'#' {
            // 跳过无法识别的语句。
            while reader.at < text.len() && text[reader.at] != b';' {
                reader.at += 1;
            }
            reader.at += 1;
            continue;
        }
        let Value::Ref(id) = reader.value(0)? else { return Err(bad("IFC 实例号写坏了")) };
        reader.skip_space();
        if text.get(reader.at) != Some(&b'=') {
            return Err(bad(format!("IFC #{id} 后面缺少 =")));
        }
        reader.at += 1;
        // 实体名 + 参数表。不能走 `value()` 的带类型值路径：那条路会把只有
        // 一个元素的列表拆开（`IFCPARAMETERVALUE(0.5)` 要这样），而
        // `IFCCARTESIANPOINT((0.,0.,0.))` 的唯一参数本身就是列表，拆了就错了。
        reader.skip_space();
        let start = reader.at;
        while reader.at < text.len() && (text[reader.at].is_ascii_alphanumeric() || text[reader.at] == b'_') {
            reader.at += 1;
        }
        let kind = String::from_utf8_lossy(&text[start..reader.at]).to_ascii_uppercase();
        let Value::List(args) = reader.value(0)? else { return Err(bad(format!("IFC #{id} 没有参数表"))) };
        entities.insert(id, Entity { kind, args });
        if entities.len() > limits::VERTICES {
            return Err(bad("IFC 实体数超过上限"));
        }
        reader.skip_space();
        if text.get(reader.at) == Some(&b';') {
            reader.at += 1;
        }
    }
    Ok(entities)
}

// ───────────────────────────── 几何 ─────────────────────────────

/// 按颜色攒三角形。
#[derive(Default)]
struct Batches {
    by_color: HashMap<[u32; 4], (Vec<Vertex>, Vec<u32>)>,
    vertex_total: usize,
}

impl Batches {
    fn push_triangle(&mut self, color: Vec4, corners: [Vec3; 3], normal: Vec3) {
        let key = color.to_array().map(f32::to_bits);
        let (vertices, indices) = self.by_color.entry(key).or_default();
        let base = vertices.len() as u32;
        for p in corners {
            vertices.push(Vertex {
                position: p.to_array(),
                normal: normal.to_array(),
                ..Default::default()
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2]);
        self.vertex_total += 3;
    }
}

struct Ifc<'a> {
    entities: &'a HashMap<u32, Entity>,
    /// 几何项 → `IfcStyledItem` 给的颜色。
    styled: HashMap<u32, Vec4>,
    placements: std::cell::RefCell<HashMap<u32, Mat4>>,
    warnings: std::cell::RefCell<std::collections::BTreeSet<String>>,
    batches: Batches,
}

impl<'a> Ifc<'a> {
    fn get(&self, id: u32) -> Option<&'a Entity> {
        self.entities.get(&id)
    }

    fn warn(&self, message: String) {
        self.warnings.borrow_mut().insert(message);
    }

    fn point3(&self, value: &Value) -> Vec3 {
        let Some(e) = value.as_ref().and_then(|r| self.get(r)) else { return Vec3::ZERO };
        let c = e.arg(0).list();
        let get = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        Vec3::new(get(0), get(1), get(2))
    }

    fn point2(&self, value: &Value) -> Vec2 {
        self.point3(value).truncate()
    }

    fn direction(&self, value: &Value, default: Vec3) -> Vec3 {
        let Some(e) = value.as_ref().and_then(|r| self.get(r)) else { return default };
        let c = e.arg(0).list();
        let get = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        Vec3::new(get(0), get(1), get(2)).try_normalize().unwrap_or(default)
    }

    /// `IfcAxis2Placement3D` / `2D` → 矩阵。
    fn axis_placement(&self, value: &Value) -> Mat4 {
        let Some(e) = value.as_ref().and_then(|r| self.get(r)) else { return Mat4::IDENTITY };
        let origin = self.point3(e.arg(0));
        match e.kind.as_str() {
            "IFCAXIS2PLACEMENT2D" => {
                let x = self.direction(e.arg(1), Vec3::X);
                let x = Vec3::new(x.x, x.y, 0.0).try_normalize().unwrap_or(Vec3::X);
                let y = Vec3::Z.cross(x);
                Mat4::from_cols(x.extend(0.0), y.extend(0.0), Vec3::Z.extend(0.0), origin.extend(1.0))
            }
            _ => {
                let z = self.direction(e.arg(1), Vec3::Z);
                let reference = self.direction(e.arg(2), Vec3::X);
                let x = (reference - z * reference.dot(z)).try_normalize().unwrap_or_else(|| z.any_orthonormal_vector());
                let y = z.cross(x);
                Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), origin.extend(1.0))
            }
        }
    }

    /// `IfcLocalPlacement` 链（带缓存）。
    fn placement(&self, id: u32, depth: usize) -> Mat4 {
        if let Some(m) = self.placements.borrow().get(&id) {
            return *m;
        }
        let Some(e) = self.get(id) else { return Mat4::IDENTITY };
        if e.kind != "IFCLOCALPLACEMENT" || depth > 64 {
            return Mat4::IDENTITY;
        }
        let parent = e.arg(0).as_ref().map_or(Mat4::IDENTITY, |p| self.placement(p, depth + 1));
        let m = parent * self.axis_placement(e.arg(1));
        self.placements.borrow_mut().insert(id, m);
        m
    }

    /// `IfcCartesianTransformationOperator3D`（含非均匀的 `...3DnonUniform`）。
    fn transform_operator(&self, value: &Value) -> Mat4 {
        let Some(e) = value.as_ref().and_then(|r| self.get(r)) else { return Mat4::IDENTITY };
        let x = self.direction(e.arg(0), Vec3::X);
        let y0 = self.direction(e.arg(1), Vec3::Y);
        let origin = self.point3(e.arg(2));
        let scale = e.arg(3).as_f64().unwrap_or(1.0) as f32;
        let z0 = self.direction(e.arg(4), x.cross(y0).try_normalize().unwrap_or(Vec3::Z));
        let z = z0;
        let y = z.cross(x).try_normalize().unwrap_or(Vec3::Y);
        let (sy, sz) = if e.kind.contains("NONUNIFORM") {
            (e.arg(5).as_f64().unwrap_or(scale as f64) as f32, e.arg(6).as_f64().unwrap_or(scale as f64) as f32)
        } else {
            (scale, scale)
        };
        Mat4::from_cols((x * scale).extend(0.0), (y * sy).extend(0.0), (z * sz).extend(0.0), origin.extend(1.0))
    }

    // ── 二维曲线与轮廓 ──

    fn curve(&self, value: &Value, depth: usize) -> Vec<Vec2> {
        let Some(e) = value.as_ref().and_then(|r| self.get(r)) else { return Vec::new() };
        if depth > 32 {
            return Vec::new();
        }
        match e.kind.as_str() {
            "IFCPOLYLINE" => e.arg(0).list().iter().map(|p| self.point2(p)).collect(),
            "IFCCOMPOSITECURVE" => {
                let mut out: Vec<Vec2> = Vec::new();
                for segment in e.arg(0).list() {
                    let Some(s) = segment.as_ref().and_then(|r| self.get(r)) else { continue };
                    let same_sense = s.arg(1).as_enum() != Some("F");
                    let mut points = self.curve(s.arg(2), depth + 1);
                    if !same_sense {
                        points.reverse();
                    }
                    // 段与段首尾相接，去掉重复点。
                    if let (Some(last), Some(first)) = (out.last(), points.first())
                        && last.distance(*first) < 1e-6
                    {
                        points.remove(0);
                    }
                    out.extend(points);
                }
                out
            }
            "IFCTRIMMEDCURVE" => self.trimmed(e, depth),
            "IFCCIRCLE" => {
                let m = self.axis_placement(e.arg(0));
                let r = e.arg(1).as_f64().unwrap_or(1.0) as f32;
                (0..=48).map(|k| {
                    let a = k as f32 / 48.0 * std::f32::consts::TAU;
                    m.transform_point3(Vec3::new(r * a.cos(), r * a.sin(), 0.0)).truncate()
                }).collect()
            }
            "IFCINDEXEDPOLYCURVE" => {
                let Some(points) = e.arg(0).as_ref().and_then(|r| self.get(r)) else { return Vec::new() };
                points
                    .arg(0)
                    .list()
                    .iter()
                    .map(|p| {
                        let c = p.list();
                        Vec2::new(c.first().and_then(Value::as_f64).unwrap_or(0.0) as f32, c.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32)
                    })
                    .collect()
            }
            other => {
                self.warn(format!("不支持的曲线 {other}"));
                Vec::new()
            }
        }
    }

    fn trimmed(&self, e: &Entity, depth: usize) -> Vec<Vec2> {
        let Some(basis) = e.arg(0).as_ref().and_then(|r| self.get(r)) else { return Vec::new() };
        if basis.kind != "IFCCIRCLE" && basis.kind != "IFCELLIPSE" {
            // 直线等其它基曲线：用裁剪点连线。
            let pick = |v: &Value| v.list().iter().find(|t| matches!(t, Value::Ref(_))).map(|t| self.point2(t));
            return match (pick(e.arg(1)), pick(e.arg(2))) {
                (Some(a), Some(b)) => vec![a, b],
                _ => self.curve(e.arg(0), depth + 1),
            };
        }
        let m = self.axis_placement(basis.arg(0));
        let (rx, ry) = if basis.kind == "IFCELLIPSE" {
            (basis.arg(1).as_f64().unwrap_or(1.0) as f32, basis.arg(2).as_f64().unwrap_or(1.0) as f32)
        } else {
            let r = basis.arg(1).as_f64().unwrap_or(1.0) as f32;
            (r, r)
        };
        let inverse = m.inverse();
        // 裁剪值：优先参数（角度），其次点。
        let angle = |v: &Value| -> Option<f32> {
            for t in v.list() {
                if let Value::Typed(name, inner) = t
                    && name == "IFCPARAMETERVALUE"
                {
                    return inner.as_f64().map(|a| a as f32);
                }
            }
            None
        };
        let point_angle = |v: &Value| -> Option<f32> {
            v.list().iter().find(|t| matches!(t, Value::Ref(_))).map(|t| {
                let local = inverse.transform_point3(self.point3(t).truncate().extend(0.0));
                (local.y / ry).atan2(local.x / rx)
            })
        };
        let (mut a0, mut a1) = match (angle(e.arg(1)), angle(e.arg(2))) {
            (Some(a), Some(b)) => {
                // 参数按度写的（Revit 的 IFC2x3 不管单位声明一律写度）：
                // 有一端超过 2π 就当度。
                if a.abs() > 6.3 || b.abs() > 6.3 { (a.to_radians(), b.to_radians()) } else { (a, b) }
            }
            _ => match (point_angle(e.arg(1)), point_angle(e.arg(2))) {
                (Some(a), Some(b)) => (a, b),
                _ => (0.0, std::f32::consts::TAU),
            },
        };
        let sense = e.arg(3).as_enum() != Some("F");
        if !sense {
            std::mem::swap(&mut a0, &mut a1);
        }
        while a1 <= a0 {
            a1 += std::f32::consts::TAU;
        }
        let steps = (((a1 - a0) / std::f32::consts::TAU * 48.0).ceil() as usize).clamp(2, 96);
        let mut points: Vec<Vec2> = (0..=steps)
            .map(|k| {
                let a = a0 + (a1 - a0) * k as f32 / steps as f32;
                m.transform_point3(Vec3::new(rx * a.cos(), ry * a.sin(), 0.0)).truncate()
            })
            .collect();
        if !sense {
            points.reverse();
        }
        points
    }

    /// 轮廓 → (外轮廓, 洞)，都已经过轮廓自己的 `Position`。
    fn profile(&self, value: &Value) -> Option<(Vec<Vec2>, Vec<Vec<Vec2>>)> {
        let e = value.as_ref().and_then(|r| self.get(r))?;
        let place = |m: Mat4, points: Vec<Vec2>| -> Vec<Vec2> { points.into_iter().map(|p| m.transform_point3(p.extend(0.0)).truncate()).collect() };
        match e.kind.as_str() {
            "IFCRECTANGLEPROFILEDEF" | "IFCRECTANGLEHOLLOWPROFILEDEF" | "IFCROUNDEDRECTANGLEPROFILEDEF" => {
                let m = self.axis_placement(e.arg(2));
                let (hx, hy) = (e.arg(3).as_f64()? as f32 / 2.0, e.arg(4).as_f64()? as f32 / 2.0);
                let outer = place(m, vec![Vec2::new(-hx, -hy), Vec2::new(hx, -hy), Vec2::new(hx, hy), Vec2::new(-hx, hy)]);
                let mut holes = Vec::new();
                if e.kind == "IFCRECTANGLEHOLLOWPROFILEDEF"
                    && let Some(t) = e.arg(5).as_f64()
                {
                    let t = t as f32;
                    holes.push(place(m, vec![Vec2::new(-hx + t, -hy + t), Vec2::new(-hx + t, hy - t), Vec2::new(hx - t, hy - t), Vec2::new(hx - t, -hy + t)]));
                }
                Some((outer, holes))
            }
            "IFCCIRCLEPROFILEDEF" | "IFCCIRCLEHOLLOWPROFILEDEF" => {
                let m = self.axis_placement(e.arg(2));
                let r = e.arg(3).as_f64()? as f32;
                let ring = |r: f32| (0..48).map(|k| {
                    let a = k as f32 / 48.0 * std::f32::consts::TAU;
                    Vec2::new(r * a.cos(), r * a.sin())
                }).collect::<Vec<_>>();
                let mut holes = Vec::new();
                if e.kind == "IFCCIRCLEHOLLOWPROFILEDEF"
                    && let Some(t) = e.arg(4).as_f64()
                {
                    holes.push(place(m, ring(r - t as f32)));
                }
                Some((place(m, ring(r)), holes))
            }
            "IFCARBITRARYCLOSEDPROFILEDEF" => Some((self.curve(e.arg(2), 0), Vec::new())),
            "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                let holes = e.arg(3).list().iter().map(|c| self.curve(c, 0)).filter(|c| c.len() >= 3).collect();
                Some((self.curve(e.arg(2), 0), holes))
            }
            "IFCDERIVEDPROFILEDEF" => {
                let (outer, holes) = self.profile(e.arg(2))?;
                let m = self.transform_operator_2d(e.arg(3));
                Some((place(m, outer), holes.into_iter().map(|h| place(m, h)).collect()))
            }
            other => {
                self.warn(format!("不支持的截面 {other}"));
                None
            }
        }
    }

    fn transform_operator_2d(&self, value: &Value) -> Mat4 {
        let Some(e) = value.as_ref().and_then(|r| self.get(r)) else { return Mat4::IDENTITY };
        let x = self.direction(e.arg(0), Vec3::X);
        let origin = self.point3(e.arg(2));
        let scale = e.arg(3).as_f64().unwrap_or(1.0) as f32;
        let y = Vec3::Z.cross(x);
        Mat4::from_cols((x * scale).extend(0.0), (y * scale).extend(0.0), Vec3::Z.extend(0.0), origin.extend(1.0))
    }

    // ── 三维几何项 ──

    fn item(&mut self, id: u32, transform: Mat4, color: Vec4, depth: usize) {
        if depth > 32 {
            return;
        }
        let Some(e) = self.get(id) else { return };
        let color = self.styled.get(&id).copied().unwrap_or(color);
        match e.kind.as_str() {
            "IFCEXTRUDEDAREASOLID" => self.extrusion(e, transform, color),
            "IFCFACETEDBREP" | "IFCFACETEDBREPWITHVOIDS" => {
                if let Some(shell) = e.arg(0).as_ref() {
                    self.shell(shell, transform, color);
                }
            }
            "IFCFACEBASEDSURFACEMODEL" | "IFCSHELLBASEDSURFACEMODEL" => {
                for shell in e.arg(0).list().iter().filter_map(Value::as_ref) {
                    self.shell(shell, transform, color);
                }
            }
            "IFCMAPPEDITEM" => {
                let Some(map) = e.arg(0).as_ref().and_then(|r| self.get(r)) else { return };
                let origin = self.axis_placement(map.arg(0));
                let target = self.transform_operator(e.arg(1));
                let Some(representation) = map.arg(1).as_ref().and_then(|r| self.get(r)) else { return };
                let items: Vec<u32> = representation.arg(3).list().iter().filter_map(Value::as_ref).collect();
                for item in items {
                    self.item(item, transform * target * origin, color, depth + 1);
                }
            }
            "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
                if let Some(first) = e.arg(1).as_ref() {
                    self.item(first, transform, color, depth + 1);
                }
            }
            "IFCTRIANGULATEDFACESET" | "IFCPOLYGONALFACESET" => self.face_set(e, transform, color),
            "IFCHALFSPACESOLID" | "IFCPOLYGONALBOUNDEDHALFSPACE" | "IFCBOXEDHALFSPACE" => {}
            other => self.warn(format!("不支持的几何项 {other}")),
        }
    }

    fn emit(&mut self, transform: Mat4, color: Vec4, corners: [Vec3; 3]) {
        let world = corners.map(|p| transform.transform_point3(p));
        let normal = (world[1] - world[0]).cross(world[2] - world[0]);
        let Some(normal) = normal.try_normalize() else { return };
        self.batches.push_triangle(color, world, normal);
    }

    fn extrusion(&mut self, e: &Entity, transform: Mat4, color: Vec4) {
        let Some((outer, holes)) = self.profile(e.arg(0)) else { return };
        let position = self.axis_placement(e.arg(1));
        let direction = self.direction(e.arg(2), Vec3::Z);
        let depth = e.arg(3).as_f64().unwrap_or(0.0) as f32;
        if outer.len() < 3 || depth.abs() < 1e-9 {
            return;
        }
        // 外轮廓逆时针、洞顺时针。
        let area = |c: &[Vec2]| path::Contour { points: c.to_vec(), closed: true }.signed_area();
        let mut outer = outer;
        if area(&outer) < 0.0 {
            outer.reverse();
        }
        let holes: Vec<Vec<Vec2>> = holes
            .into_iter()
            .map(|mut h| {
                if area(&h) > 0.0 {
                    h.reverse();
                }
                h
            })
            .collect();
        let mut contours = vec![path::Contour { points: outer.clone(), closed: true }];
        contours.extend(holes.iter().map(|h| path::Contour { points: h.clone(), closed: true }));
        let fill = path::fill(&contours, path::FillRule::EvenOdd);
        let offset = direction * depth;
        let m = transform * position;
        // 拉伸方向朝 -Z 时两个盖和侧壁的绕向都要反过来。
        let flip = offset.z < 0.0;
        for t in fill.indices.chunks_exact(3) {
            let p = |i: u32| fill.points[i as usize].extend(0.0);
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            if flip {
                self.emit(m, color, [a + offset, c + offset, b + offset]);
                self.emit(m, color, [a, b, c]);
            } else {
                self.emit(m, color, [a + offset, b + offset, c + offset]);
                self.emit(m, color, [a, c, b]);
            }
        }
        for contour in std::iter::once(&outer).chain(&holes) {
            for k in 0..contour.len() {
                let a = contour[k].extend(0.0);
                let b = contour[(k + 1) % contour.len()].extend(0.0);
                if a.distance(b) < 1e-9 {
                    continue;
                }
                if flip {
                    self.emit(m, color, [a, b + offset, b]);
                    self.emit(m, color, [a, a + offset, b + offset]);
                } else {
                    self.emit(m, color, [a, b, b + offset]);
                    self.emit(m, color, [a, b + offset, a + offset]);
                }
            }
        }
    }

    fn shell(&mut self, id: u32, transform: Mat4, color: Vec4) {
        let Some(shell) = self.get(id) else { return };
        let faces: Vec<u32> = shell.arg(0).list().iter().filter_map(Value::as_ref).collect();
        for face in faces {
            let Some(face) = self.get(face) else { continue };
            let color = self.styled.get(&face.args.first().and_then(Value::as_ref).unwrap_or(0)).copied().unwrap_or(color);
            let mut outer: Option<Vec<Vec3>> = None;
            let mut holes: Vec<Vec<Vec3>> = Vec::new();
            for bound in face.arg(0).list().iter().filter_map(|b| b.as_ref().and_then(|r| self.get(r))) {
                let Some(lp) = bound.arg(0).as_ref().and_then(|r| self.get(r)) else { continue };
                if lp.kind != "IFCPOLYLOOP" {
                    continue;
                }
                let mut points: Vec<Vec3> = lp.arg(0).list().iter().map(|p| self.point3(p)).collect();
                if bound.arg(1).as_enum() == Some("F") {
                    points.reverse();
                }
                if bound.kind == "IFCFACEOUTERBOUND" && outer.is_none() {
                    outer = Some(points);
                } else {
                    holes.push(points);
                }
            }
            let outer = match outer {
                Some(o) => o,
                None if !holes.is_empty() => holes.remove(0),
                None => continue,
            };
            self.polygon(&outer, &holes, transform, color);
        }
    }

    /// 一个平面多边形（可带洞）三角化。
    fn polygon(&mut self, outer: &[Vec3], holes: &[Vec<Vec3>], transform: Mat4, color: Vec4) {
        if outer.len() < 3 {
            return;
        }
        if holes.is_empty() && outer.len() <= 4 {
            for k in 1..outer.len() - 1 {
                self.emit(transform, color, [outer[0], outer[k], outer[k + 1]]);
            }
            return;
        }
        // Newell 法线定出平面，投影到二维再三角化。
        let mut normal = Vec3::ZERO;
        for k in 0..outer.len() {
            let (a, b) = (outer[k], outer[(k + 1) % outer.len()]);
            normal += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
        }
        let Some(normal) = normal.try_normalize() else { return };
        let u = normal.any_orthonormal_vector();
        let v = normal.cross(u);
        let origin = outer[0];
        let flat = |p: &Vec3| Vec2::new((*p - origin).dot(u), (*p - origin).dot(v));
        let mut contours = vec![path::Contour { points: outer.iter().map(flat).collect(), closed: true }];
        contours.extend(holes.iter().map(|h| path::Contour { points: h.iter().map(flat).collect(), closed: true }));
        let fill = path::fill(&contours, path::FillRule::EvenOdd);
        let lift = |p: Vec2| origin + u * p.x + v * p.y;
        for t in fill.indices.chunks_exact(3) {
            let corners = [lift(fill.points[t[0] as usize]), lift(fill.points[t[1] as usize]), lift(fill.points[t[2] as usize])];
            // 耳切输出逆时针（按 u × v = normal），和原多边形同向。
            self.emit(transform, color, corners);
        }
    }

    fn face_set(&mut self, e: &Entity, transform: Mat4, color: Vec4) {
        let Some(points) = e.arg(0).as_ref().and_then(|r| self.get(r)) else { return };
        let coordinates: Vec<Vec3> = points
            .arg(0)
            .list()
            .iter()
            .map(|p| {
                let c = p.list();
                let g = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                Vec3::new(g(0), g(1), g(2))
            })
            .collect();
        let index = |v: &Value| v.as_f64().map(|i| i as usize).and_then(|i| coordinates.get(i.wrapping_sub(1)).copied());
        if e.kind == "IFCTRIANGULATEDFACESET" {
            for triangle in e.arg(3).list() {
                let c: Vec<Vec3> = triangle.list().iter().filter_map(index).collect();
                if c.len() == 3 {
                    self.emit(transform, color, [c[0], c[1], c[2]]);
                }
            }
        } else {
            let faces: Vec<&Entity> = e.arg(2).list().iter().filter_map(|f| f.as_ref().and_then(|r| self.get(r))).collect();
            for face in faces {
                let outer: Vec<Vec3> = face.arg(0).list().iter().filter_map(index).collect();
                let holes: Vec<Vec<Vec3>> = face.arg(1).list().iter().map(|h| h.list().iter().filter_map(index).collect()).collect();
                self.polygon(&outer, &holes, transform, color);
            }
        }
    }
}

/// `IfcSurfaceStyle` → 颜色。
fn surface_style_color(entities: &HashMap<u32, Entity>, value: &Value, depth: usize) -> Option<Vec4> {
    let e = entities.get(&value.as_ref()?)?;
    if depth > 8 {
        return None;
    }
    match e.kind.as_str() {
        "IFCPRESENTATIONSTYLEASSIGNMENT" => e.arg(0).list().iter().find_map(|s| surface_style_color(entities, s, depth + 1)),
        "IFCSURFACESTYLE" => e.arg(2).list().iter().find_map(|s| surface_style_color(entities, s, depth + 1)),
        "IFCSURFACESTYLERENDERING" | "IFCSURFACESTYLESHADING" => {
            let c = entities.get(&e.arg(0).as_ref()?)?;
            let g = |i: usize| c.arg(i).as_f64().unwrap_or(0.8) as f32;
            let transparency = if e.kind == "IFCSURFACESTYLERENDERING" { e.arg(1).as_f64().unwrap_or(0.0) as f32 } else { 0.0 };
            // IFC 的颜色是 sRGB 分量。
            let s = crate::amf::srgb_to_linear;
            Some(Vec4::new(s(g(1)), s(g(2)), s(g(3)), 1.0 - transparency.clamp(0.0, 0.95)))
        }
        _ => None,
    }
}

fn default_color(kind: &str) -> Vec4 {
    let s = crate::amf::srgb_to_linear;
    let rgb = |r: f32, g: f32, b: f32, a: f32| Vec4::new(s(r), s(g), s(b), a);
    match kind {
        "IFCWINDOW" | "IFCPLATE" | "IFCCURTAINWALL" => rgb(0.55, 0.7, 0.8, 0.35),
        "IFCDOOR" => rgb(0.55, 0.4, 0.28, 1.0),
        "IFCSLAB" | "IFCROOF" => rgb(0.7, 0.7, 0.7, 1.0),
        "IFCWALL" | "IFCWALLSTANDARDCASE" => rgb(0.9, 0.88, 0.84, 1.0),
        "IFCCOLUMN" | "IFCBEAM" | "IFCMEMBER" => rgb(0.6, 0.6, 0.62, 1.0),
        "IFCRAILING" | "IFCSTAIR" | "IFCSTAIRFLIGHT" => rgb(0.45, 0.45, 0.48, 1.0),
        "IFCFURNISHINGELEMENT" => rgb(0.75, 0.6, 0.45, 1.0),
        _ => rgb(0.8, 0.8, 0.8, 1.0),
    }
}

/// 解析 IFC。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "IFC".into());
    let entities = parse_step(&bytes)?;
    build(&entities, &name)
}

/// 从实体表建模型。
pub fn build(entities: &HashMap<u32, Entity>, name: &str) -> Result<Model, LoadError> {
    // 长度单位：IfcUnitAssignment 里的 LENGTHUNIT。
    let mut unit = 1.0f32;
    if let Some(assignment) = entities.values().find(|e| e.kind == "IFCUNITASSIGNMENT") {
        for u in assignment.arg(0).list().iter().filter_map(|v| v.as_ref().and_then(|r| entities.get(&r))) {
            if u.kind == "IFCSIUNIT" && u.arg(1).as_enum() == Some("LENGTHUNIT") {
                unit = match u.arg(2).as_enum() {
                    Some("MILLI") => 0.001,
                    Some("CENTI") => 0.01,
                    Some("DECI") => 0.1,
                    Some("KILO") => 1000.0,
                    _ => 1.0,
                };
            } else if u.kind == "IFCCONVERSIONBASEDUNIT" && u.arg(1).as_enum() == Some("LENGTHUNIT")
                && let Some(measure) = u.arg(3).as_ref().and_then(|r| entities.get(&r))
            {
                unit = measure.arg(0).as_f64().unwrap_or(1.0) as f32;
            }
        }
    }

    // 样式：几何项 → 颜色；材质 → 颜色。
    let mut styled = HashMap::new();
    let mut material_colors = HashMap::new();
    let mut styled_representation_colors: HashMap<u32, Vec4> = HashMap::new();
    for (&id, e) in entities {
        if e.kind == "IFCSTYLEDITEM"
            && let Some(color) = e.arg(1).list().iter().find_map(|s| surface_style_color(entities, s, 0))
        {
            match e.arg(0).as_ref() {
                Some(item) => {
                    styled.insert(item, color);
                }
                None => {
                    styled_representation_colors.insert(id, color);
                }
            }
        }
    }
    for e in entities.values().filter(|e| e.kind == "IFCMATERIALDEFINITIONREPRESENTATION") {
        let Some(material) = e.arg(3).as_ref() else { continue };
        for representation in e.arg(2).list().iter().filter_map(|r| r.as_ref().and_then(|r| entities.get(&r))) {
            if let Some(color) = representation.arg(3).list().iter().filter_map(Value::as_ref).find_map(|i| styled_representation_colors.get(&i)) {
                material_colors.insert(material, *color);
            }
        }
    }
    // 元素 → 材质颜色。
    let material_color = |material: u32| -> Option<Vec4> {
        let mut stack = vec![material];
        let mut guard = 0;
        while let Some(m) = stack.pop() {
            guard += 1;
            if guard > 64 {
                break;
            }
            if let Some(c) = material_colors.get(&m) {
                return Some(*c);
            }
            let Some(e) = entities.get(&m) else { continue };
            match e.kind.as_str() {
                "IFCMATERIALLIST" => stack.extend(e.arg(0).list().iter().filter_map(Value::as_ref)),
                "IFCMATERIALLAYERSETUSAGE" => stack.extend(e.arg(0).as_ref()),
                "IFCMATERIALLAYERSET" => stack.extend(e.arg(0).list().iter().filter_map(Value::as_ref)),
                "IFCMATERIALLAYER" => stack.extend(e.arg(0).as_ref()),
                _ => {}
            }
        }
        None
    };
    let mut element_colors: HashMap<u32, Vec4> = HashMap::new();
    for e in entities.values().filter(|e| e.kind == "IFCRELASSOCIATESMATERIAL") {
        let Some(color) = e.arg(5).as_ref().and_then(material_color) else { continue };
        for object in e.arg(4).list().iter().filter_map(Value::as_ref) {
            element_colors.insert(object, color);
        }
    }

    let mut ifc = Ifc {
        entities,
        styled,
        placements: Default::default(),
        warnings: Default::default(),
        batches: Batches::default(),
    };

    let mut ids: Vec<u32> = entities.keys().copied().collect();
    ids.sort_unstable();
    let mut products = 0usize;
    for id in ids {
        let e = &entities[&id];
        if matches!(e.kind.as_str(), "IFCOPENINGELEMENT" | "IFCSPACE" | "IFCANNOTATION" | "IFCGRID") {
            continue;
        }
        let (Some(placement), Some(shape)) = (e.arg(5).as_ref(), e.arg(6).as_ref()) else { continue };
        let Some(shape) = entities.get(&shape).filter(|s| s.kind == "IFCPRODUCTDEFINITIONSHAPE") else { continue };
        if entities.get(&placement).is_none_or(|p| p.kind != "IFCLOCALPLACEMENT") {
            continue;
        }
        let transform = ifc.placement(placement, 0);
        let color = element_colors.get(&id).copied().unwrap_or_else(|| default_color(&e.kind));
        // 只画 `Body`（没有标识符时也画），跳过 Axis / Box / FootPrint 这些辅助表示。
        let representations: Vec<u32> = shape.arg(2).list().iter().filter_map(Value::as_ref).collect();
        let mut drew = false;
        for representation in &representations {
            let Some(r) = entities.get(representation) else { continue };
            let identifier = match r.arg(1) {
                Value::Str(s) => s.as_str(),
                _ => "",
            };
            if !(identifier.eq_ignore_ascii_case("Body") || (identifier.is_empty() && !drew)) {
                continue;
            }
            for item in r.arg(3).list().iter().filter_map(Value::as_ref) {
                ifc.item(item, transform, color, 0);
            }
            drew = true;
        }
        products += 1;
        if ifc.batches.vertex_total > limits::VERTICES {
            return Err(bad("IFC 顶点数超过上限"));
        }
    }
    for warning in ifc.warnings.borrow().iter() {
        klog::warn!("IFC：{warning}（已跳过）");
    }
    if ifc.batches.by_color.is_empty() {
        return Err(bad("IFC 里没有能画的几何"));
    }

    let mut materials = Vec::new();
    let mut parts = Vec::new();
    let mut batches: Vec<_> = ifc.batches.by_color.into_iter().collect();
    batches.sort_by_key(|(k, _)| *k);
    for (key, (vertices, indices)) in batches {
        let color = Vec4::from_array(key.map(f32::from_bits));
        let mut material = Material::standard().with_base_color(color).with_roughness(0.8).with_metallic(0.0);
        if color.w < 0.999 {
            material.set_blend_mode(kmaterial::BlendMode::Alpha);
        }
        // 导出器写的绕向并不可靠（尤其是 B-rep 面），双面最稳妥。
        material.set_double_sided(true);
        let mesh = Mesh::new(vertices, indices);
        if !mesh.is_valid() {
            continue;
        }
        parts.push((format!("Color{}", materials.len()), mesh, Some(materials.len())));
        materials.push(material);
    }
    let mut model = flat_model(name, parts, materials);
    klog::debug!("IFC：{products} 个构件，{} 个三角形", model.triangle_count());
    // IFC 是 Z 朝上；单位换成米。
    if let Some(root) = model_root_transform(&mut model) {
        root.rotation = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        root.scale = Vec3::splat(unit);
    }
    Ok(model)
}

fn model_root_transform(model: &mut Model) -> Option<&mut kgltf::NodeTransform> {
    let root = *model.roots().first()?;
    model.node_mut(root).map(|node| &mut node.transform)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "ISO-10303-21;
HEADER;
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1= IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#2= IFCUNITASSIGNMENT((#1));
#3= IFCCARTESIANPOINT((0.,0.,0.));
#4= IFCAXIS2PLACEMENT3D(#3,$,$);
#5= IFCLOCALPLACEMENT($,#4);
#6= IFCCARTESIANPOINT((0.,0.));
#7= IFCAXIS2PLACEMENT2D(#6,$);
#8= IFCRECTANGLEPROFILEDEF(.AREA.,$,#7,2000.,1000.);
#9= IFCDIRECTION((0.,0.,1.));
#10= IFCEXTRUDEDAREASOLID(#8,#4,#9,3000.);
#11= IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#10));
#12= IFCPRODUCTDEFINITIONSHAPE($,$,(#11));
#13= IFCWALL('guid',$,'Wall',$,$,#5,#12,$);
#14= IFCTRIMMEDCURVE(#15,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#15= IFCCIRCLE(#7,10.);
ENDSEC;
END-ISO-10303-21;
";

    #[test]
    fn step_values() {
        let entities = parse_step(SAMPLE.as_bytes()).unwrap();
        assert_eq!(entities[&8].kind, "IFCRECTANGLEPROFILEDEF");
        assert_eq!(entities[&8].args[3], Value::Real(2000.0));
        assert_eq!(entities[&14].args[1], Value::List(vec![Value::Typed("IFCPARAMETERVALUE".into(), Box::new(Value::Real(0.0)))]));
    }

    #[test]
    fn an_extruded_box_wall() {
        let entities = parse_step(SAMPLE.as_bytes()).unwrap();
        let model = build(&entities, "t").unwrap();
        // 一个长方体：两个盖各 2 个三角形 + 四面侧壁各 2 个。
        assert_eq!(model.triangle_count(), 12);
        let root = &model.nodes()[model.roots()[0]];
        assert!((root.transform.scale.x - 0.001).abs() < 1e-9, "毫米换成米");
    }

    #[test]
    fn trimmed_circle_in_degrees_is_a_half_circle() {
        let entities = parse_step(SAMPLE.as_bytes()).unwrap();
        let ifc = Ifc {
            entities: &entities,
            styled: HashMap::new(),
            placements: Default::default(),
            warnings: Default::default(),
            batches: Batches::default(),
        };
        let points = ifc.curve(&Value::Ref(14), 0);
        assert!((points[0] - Vec2::new(10.0, 0.0)).length() < 1e-3);
        assert!((points.last().unwrap() - Vec2::new(-10.0, 0.0)).length() < 1e-3);
        assert!(points.iter().all(|p| p.y >= -1e-3));
    }
}
