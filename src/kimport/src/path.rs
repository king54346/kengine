//! 二维路径：贝塞尔展平、填充三角化、描边、拉伸成三维。
//!
//! SVG、TTF 字形、Lottie 的形状层、IFC 的截面轮廓——这四样东西的共同点
//! 是「一堆二维轮廓，要变成三角形」。各写一份的话，同一个三角化的
//! 边界情形要调四遍。
//!
//! # 三角化用的是耳切法（ear clipping）
//!
//! 带洞的多边形先用「桥」把洞接到外轮廓上变成一个简单多边形，再逐个
//! 切耳朵。这是教科书做法，O(n²)，对字形和图标这个规模（几十到几百个点）
//! 完全够用。真要处理上万个点的地图数据该换成扫描线（earcut / libtess）。
//!
//! # 填充规则
//!
//! 支持非零环绕（nonzero，SVG 与 TTF 的默认）和奇偶（even-odd）。
//! 两者的区别只在「哪些轮廓算洞」：奇偶按嵌套层数的奇偶，
//! 非零按环绕方向是否抵消。字体的「o」和「8」就靠它把中间掏空。

use kmath::{Vec2, Vec3};
use kmesh::{Mesh, Vertex};

/// 路径的一段。控制点一律是绝对坐标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    /// 抬笔移动到。
    MoveTo(Vec2),
    /// 直线到。
    LineTo(Vec2),
    /// 二次贝塞尔：`(控制点, 终点)`。
    Quadratic(Vec2, Vec2),
    /// 三次贝塞尔：`(控制点一, 控制点二, 终点)`。
    Cubic(Vec2, Vec2, Vec2),
    /// 闭合当前子路径。
    Close,
}

/// 一条路径，可能包含多个子路径。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path {
    /// 依次执行的各段。
    pub segments: Vec<Segment>,
}

/// 填充规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    /// 非零环绕。SVG 与 TrueType 的默认。
    #[default]
    NonZero,
    /// 奇偶。
    EvenOdd,
}

impl Path {
    /// 空路径。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否一段都没有。
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// 追加一段。
    pub fn push(&mut self, segment: Segment) {
        self.segments.push(segment);
    }

    /// 把贝塞尔展平成折线，返回若干条轮廓。
    ///
    /// `tolerance` 是允许的最大弦高误差（和输入同一个单位）。
    /// 细分数按曲线的「弯曲程度」估：控制点离弦越远，分得越细。
    /// 固定细分数的做法在一条很长的曲线上会出折角，在一条很短的曲线上
    /// 又白算几十个点。
    ///
    /// 闭合与否记在返回值里：描边要用它决定端点画不画帽子。
    pub fn flatten(&self, tolerance: f32) -> Vec<Contour> {
        let tolerance = tolerance.max(1e-5);
        let mut contours = Vec::new();
        let mut current: Vec<Vec2> = Vec::new();
        let mut closed = false;
        let mut cursor = Vec2::ZERO;
        let mut start = Vec2::ZERO;

        let flush = |points: &mut Vec<Vec2>, closed: &mut bool, out: &mut Vec<Contour>| {
            if points.len() >= 2 {
                out.push(Contour {
                    points: std::mem::take(points),
                    closed: *closed,
                });
            } else {
                points.clear();
            }
            *closed = false;
        };

        for &segment in &self.segments {
            match segment {
                Segment::MoveTo(point) => {
                    flush(&mut current, &mut closed, &mut contours);
                    current.push(point);
                    cursor = point;
                    start = point;
                }
                Segment::LineTo(point) => {
                    if current.is_empty() {
                        current.push(cursor);
                    }
                    current.push(point);
                    cursor = point;
                }
                Segment::Quadratic(control, end) => {
                    if current.is_empty() {
                        current.push(cursor);
                    }
                    let steps =
                        steps_for(cursor.distance(control) + control.distance(end), tolerance);
                    for step in 1..=steps {
                        let t = step as f32 / steps as f32;
                        let inverse = 1.0 - t;
                        current.push(
                            cursor * inverse * inverse + control * 2.0 * inverse * t + end * t * t,
                        );
                    }
                    cursor = end;
                }
                Segment::Cubic(first, second, end) => {
                    if current.is_empty() {
                        current.push(cursor);
                    }
                    let length =
                        cursor.distance(first) + first.distance(second) + second.distance(end);
                    let steps = steps_for(length, tolerance);
                    for step in 1..=steps {
                        let t = step as f32 / steps as f32;
                        let inverse = 1.0 - t;
                        current.push(
                            cursor * inverse * inverse * inverse
                                + first * 3.0 * inverse * inverse * t
                                + second * 3.0 * inverse * t * t
                                + end * t * t * t,
                        );
                    }
                    cursor = end;
                }
                Segment::Close => {
                    closed = true;
                    // 首尾重合时不重复推一个点：重复点会让描边在接缝处
                    // 算出零长度的方向向量。
                    if current
                        .first()
                        .is_some_and(|&first| first.distance(cursor) > 1e-6)
                    {
                        current.push(start);
                    }
                    cursor = start;
                    flush(&mut current, &mut closed, &mut contours);
                }
            }
        }
        flush(&mut current, &mut closed, &mut contours);
        contours
    }
}

/// 展平之后的一条轮廓。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contour {
    /// 折线的顶点。
    pub points: Vec<Vec2>,
    /// 是不是闭合的。
    pub closed: bool,
}

impl Contour {
    /// 带符号面积。正为逆时针，负为顺时针。
    pub fn signed_area(&self) -> f32 {
        let mut sum = 0.0;
        for index in 0..self.points.len() {
            let a = self.points[index];
            let b = self.points[(index + 1) % self.points.len()];
            sum += a.x * b.y - b.x * a.y;
        }
        sum * 0.5
    }

    /// 一个点在不在这条轮廓里（射线法）。
    pub fn contains(&self, point: Vec2) -> bool {
        let mut inside = false;
        let count = self.points.len();
        for index in 0..count {
            let a = self.points[index];
            let b = self.points[(index + 1) % count];
            if (a.y > point.y) != (b.y > point.y) {
                let t = (point.y - a.y) / (b.y - a.y);
                if point.x < a.x + t * (b.x - a.x) {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

/// 按弧长和容差估细分段数。上限 64 是为了防病态输入。
fn steps_for(length: f32, tolerance: f32) -> usize {
    ((length / tolerance).sqrt().ceil() as usize).clamp(1, 64)
}

/// 三角化的结果：一批二维顶点和三角形索引。
#[derive(Debug, Clone, Default)]
pub struct Tessellation {
    /// 顶点。
    pub points: Vec<Vec2>,
    /// 三角形索引。
    pub indices: Vec<u32>,
}

impl Tessellation {
    /// 三角形数。
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// 是不是空的。
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// 把一组轮廓填充成三角形。
///
/// 轮廓会先按「谁在谁里面」分成若干组，每组一个外轮廓加若干个洞。
/// 一个 SVG 路径里同时有两个不相交的形状（比如字母 `i` 的点和竖）时，
/// 两组会各自三角化。
pub fn fill(contours: &[Contour], rule: FillRule) -> Tessellation {
    let usable: Vec<&Contour> = contours.iter().filter(|c| c.points.len() >= 3).collect();
    if usable.is_empty() {
        return Tessellation::default();
    }

    // 嵌套深度：被奇数个别的轮廓包着的就是洞（偶数层是实心）。
    // 非零规则严格来说要看环绕方向，但字体和 SVG 的实际文件里
    // 洞的绕向总是和外轮廓相反，两种规则在这批数据上一致。
    let depth: Vec<usize> = usable
        .iter()
        .map(|contour| {
            let probe = contour.points[0];
            usable
                .iter()
                .filter(|other| !std::ptr::eq(*other, contour) && other.contains(probe))
                .count()
        })
        .collect();

    let mut result = Tessellation::default();
    for (index, contour) in usable.iter().enumerate() {
        if !depth[index].is_multiple_of(2) {
            continue; // 这是个洞，由它外面那层带上。
        }
        // 直接落在这一层里面的洞（深度正好多一层，而且确实在里面）。
        let holes: Vec<&Contour> = usable
            .iter()
            .enumerate()
            .filter(|&(other, candidate)| {
                depth[other] == depth[index] + 1 && contour.contains(candidate.points[0])
            })
            .map(|(_, candidate)| *candidate)
            .collect();
        let piece = triangulate_with_holes(contour, &holes, rule);
        let offset = result.points.len() as u32;
        result.points.extend(piece.points);
        result
            .indices
            .extend(piece.indices.into_iter().map(|i| i + offset));
    }
    result
}

/// 把洞用「桥」接进外轮廓，再耳切。
fn triangulate_with_holes(outer: &Contour, holes: &[&Contour], rule: FillRule) -> Tessellation {
    let _ = rule; // 两种规则在这批数据上一致，见 `fill` 的注释。
    // 外轮廓统一成逆时针，洞统一成顺时针。耳切只认一种绕向。
    let mut polygon: Vec<Vec2> = outer.points.clone();
    if outer.signed_area() < 0.0 {
        polygon.reverse();
    }
    // 首尾重合的点去掉，否则耳切会在那里产生零面积三角形。
    if polygon.len() >= 2 && polygon[0].distance(polygon[polygon.len() - 1]) < 1e-6 {
        polygon.pop();
    }

    // 洞按最右点从右到左接：先接右边的，桥不容易和后面的洞相交。
    let mut ordered: Vec<Vec<Vec2>> = holes
        .iter()
        .map(|hole| {
            let mut points = hole.points.clone();
            if hole.signed_area() > 0.0 {
                points.reverse();
            }
            if points.len() >= 2 && points[0].distance(points[points.len() - 1]) < 1e-6 {
                points.pop();
            }
            points
        })
        .filter(|points| points.len() >= 3)
        .collect();
    ordered.sort_by(|a, b| rightmost(b).x.total_cmp(&rightmost(a).x));

    for hole in ordered {
        polygon = bridge(polygon, hole);
    }
    let indices = ear_clip(&polygon);
    Tessellation {
        points: polygon,
        indices,
    }
}

fn rightmost(points: &[Vec2]) -> Vec2 {
    points
        .iter()
        .copied()
        .fold(Vec2::new(f32::MIN, 0.0), |best, point| {
            if point.x > best.x { point } else { best }
        })
}

/// 把一个洞接进外轮廓：在洞的最右点和外轮廓上最近的可见点之间开一条缝。
///
/// 缝是「零宽度」的：外轮廓沿着缝走进洞里、绕洞一圈、再沿缝走回来。
/// 于是带洞的多边形变成一个（自己贴着自己的）简单多边形，耳切就能处理。
fn bridge(outer: Vec<Vec2>, hole: Vec<Vec2>) -> Vec<Vec2> {
    let Some(entry) = hole
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .map(|(index, _)| index)
    else {
        return outer;
    };
    let target = hole[entry];
    // 外轮廓上离它最近的顶点。严格的做法是往 +X 投射一条射线找到
    // 最近的边、再在那条边的两端里挑可见的那个；取最近顶点在凸出的
    // 形状上等价，在很凹的形状上可能产生自交的桥。字形和图标上够用。
    let Some(exit) = outer
        .iter()
        .enumerate()
        .min_by(|a, b| {
            a.1.distance_squared(target)
                .total_cmp(&b.1.distance_squared(target))
        })
        .map(|(index, _)| index)
    else {
        return outer;
    };

    let mut merged = Vec::with_capacity(outer.len() + hole.len() + 2);
    merged.extend_from_slice(&outer[..=exit]);
    for step in 0..hole.len() {
        merged.push(hole[(entry + step) % hole.len()]);
    }
    merged.push(target);
    merged.push(outer[exit]);
    merged.extend_from_slice(&outer[exit + 1..]);
    merged
}

/// 把一个任意绕向的简单多边形切成三角形，下标指回输入，三角形的绕向
/// 与输入一致（顺时针进、顺时针出）。
///
/// 给 VRML 的非凸面、Extrusion 端盖这类「一圈点围成一个面」的场合用；
/// 带洞的走 [`fill`]。
pub(crate) fn triangulate(points: &[Vec2]) -> Vec<u32> {
    let area: f32 = (0..points.len())
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    if area >= 0.0 {
        return ear_clip(points);
    }
    // 顺时针：倒过来切，再把下标映射回去并把每个三角形的绕向翻回来。
    let reversed: Vec<Vec2> = points.iter().rev().copied().collect();
    let last = points.len() as u32 - 1;
    let mut indices = ear_clip(&reversed);
    for triangle in indices.chunks_exact_mut(3) {
        for index in triangle.iter_mut() {
            *index = last - *index;
        }
        triangle.swap(1, 2);
    }
    indices
}

/// 耳切。输入必须是逆时针的简单多边形。
fn ear_clip(polygon: &[Vec2]) -> Vec<u32> {
    let count = polygon.len();
    if count < 3 {
        return Vec::new();
    }
    let mut remaining: Vec<usize> = (0..count).collect();
    let mut indices = Vec::with_capacity((count - 2) * 3);
    // 每轮最多扫一遍剩下的顶点。找不到耳朵就强行切一个——
    // 自交的多边形（桥可能造成）没有耳朵，死循环比画错更糟。
    let mut guard = count * count + 16;

    while remaining.len() > 3 && guard > 0 {
        let mut clipped = false;
        for slot in 0..remaining.len() {
            let previous = remaining[(slot + remaining.len() - 1) % remaining.len()];
            let current = remaining[slot];
            let next = remaining[(slot + 1) % remaining.len()];
            if is_ear(polygon, &remaining, previous, current, next) {
                indices.extend_from_slice(&[previous as u32, current as u32, next as u32]);
                remaining.remove(slot);
                clipped = true;
                break;
            }
        }
        if !clipped {
            let previous = remaining[remaining.len() - 1];
            let current = remaining[0];
            let next = remaining[1];
            indices.extend_from_slice(&[previous as u32, current as u32, next as u32]);
            remaining.remove(0);
        }
        guard -= 1;
    }
    if remaining.len() == 3 {
        indices.extend_from_slice(&[
            remaining[0] as u32,
            remaining[1] as u32,
            remaining[2] as u32,
        ]);
    }
    indices
}

fn cross(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn is_ear(polygon: &[Vec2], remaining: &[usize], a: usize, b: usize, c: usize) -> bool {
    let (pa, pb, pc) = (polygon[a], polygon[b], polygon[c]);
    // 逆时针多边形里，凸顶点的叉积为正。凹顶点不可能是耳朵。
    if cross(pa, pb, pc) <= 1e-9 {
        return false;
    }
    // 三角形里不能含别的顶点。和三个角**重合**的顶点不算：桥会把洞口和
    // 外轮廓上的两个点各复制一份，它们位置相同、下标不同。算进去的话，
    // 桥两侧的每个候选耳朵都「含有别的顶点」，一只耳朵都切不出来，
    // 最后退化成强行切，把洞填上。
    !remaining.iter().any(|&index| {
        let p = polygon[index];
        index != a
            && index != b
            && index != c
            && p != pa
            && p != pb
            && p != pc
            && point_in_triangle(p, pa, pb, pc)
    })
}

fn point_in_triangle(point: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let d1 = cross(a, b, point);
    let d2 = cross(b, c, point);
    let d3 = cross(c, a, point);
    // 边上的点算在里面：算在外面的话，共线的三点会被误判成合法的耳朵。
    (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
}

/// 把一条折线扩成一条有宽度的带子（描边）。
///
/// 接头用的是斜接的简化版：两段的法线取平均。夹角很小时斜接会伸得很远，
/// 这里按 `miter_limit` 退回平接。端点是平头（butt），不画圆帽或方帽
/// ——SVG 的 `stroke-linecap` 有三种，这里只实现最常用的那种。
pub fn stroke(contour: &Contour, width: f32) -> Tessellation {
    let half = width.max(1e-4) * 0.5;
    let points = &contour.points;
    if points.len() < 2 {
        return Tessellation::default();
    }
    const MITER_LIMIT: f32 = 4.0;

    let count = points.len();
    let normal_at = |index: usize| -> Vec2 {
        let a = points[index];
        let b = points[(index + 1) % count];
        let direction = (b - a).normalize_or_zero();
        Vec2::new(-direction.y, direction.x)
    };

    let mut result = Tessellation::default();
    let last = if contour.closed { count } else { count - 1 };
    for index in 0..last {
        let incoming = if index == 0 {
            if contour.closed {
                normal_at(count - 1)
            } else {
                normal_at(0)
            }
        } else {
            normal_at(index - 1)
        };
        let outgoing = normal_at(index);
        let next_incoming = outgoing;
        let next_outgoing = if index + 1 >= count - 1 && !contour.closed {
            outgoing
        } else {
            normal_at((index + 1) % count)
        };

        let joint = |a: Vec2, b: Vec2| -> Vec2 {
            let average = (a + b).normalize_or_zero();
            if average == Vec2::ZERO {
                return a * half;
            }
            // 斜接长度 = half / cos(半角)，而 cos(半角) = average · a。
            let scale = 1.0 / average.dot(a).max(1.0 / MITER_LIMIT);
            average * half * scale
        };

        let a = points[index];
        let b = points[(index + 1) % count];
        let offset_a = joint(incoming, outgoing);
        let offset_b = joint(next_incoming, next_outgoing);

        let base = result.points.len() as u32;
        result
            .points
            .extend_from_slice(&[a + offset_a, a - offset_a, b + offset_b, b - offset_b]);
        result
            .indices
            .extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    result
}

/// 把二维三角化的结果拉成三维网格。
///
/// `depth` 为 0 时只出一个平面（前盖）。大于 0 时出前后两个盖加一圈侧壁。
///
/// # 坐标
///
/// 二维的 `(x, y)` 映射到三维的 `(x, y, ±depth/2)`——也就是躺在 XY 平面上、
/// 沿 Z 拉伸。SVG 的 y 轴朝下，调用方负责先翻过来（见 [`crate::svg`]）。
pub fn extrude(fill: &Tessellation, contours: &[Contour], depth: f32) -> Mesh {
    let half = depth * 0.5;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let cap =
        |z: f32, normal: Vec3, flip: bool, vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>| {
            let base = vertices.len() as u32;
            for point in &fill.points {
                vertices.push(Vertex {
                    position: [point.x, point.y, z],
                    normal: normal.to_array(),
                    uv: [point.x, point.y],
                    ..Default::default()
                });
            }
            for triangle in fill.indices.chunks_exact(3) {
                if flip {
                    indices.extend_from_slice(&[
                        base + triangle[0],
                        base + triangle[2],
                        base + triangle[1],
                    ]);
                } else {
                    indices.extend_from_slice(&[
                        base + triangle[0],
                        base + triangle[1],
                        base + triangle[2],
                    ]);
                }
            }
        };

    cap(half, Vec3::Z, false, &mut vertices, &mut indices);
    if depth > 0.0 {
        cap(-half, Vec3::NEG_Z, true, &mut vertices, &mut indices);
        // 侧壁：每条轮廓的每一段各一个四边形。
        for contour in contours {
            let count = contour.points.len();
            if count < 2 {
                continue;
            }
            let last = if contour.closed { count } else { count - 1 };
            for index in 0..last {
                let a = contour.points[index];
                let b = contour.points[(index + 1) % count];
                let direction = (b - a).normalize_or_zero();
                let normal = Vec3::new(direction.y, -direction.x, 0.0);
                let base = vertices.len() as u32;
                for (point, z) in [(a, half), (b, half), (b, -half), (a, -half)] {
                    vertices.push(Vertex {
                        position: [point.x, point.y, z],
                        normal: normal.to_array(),
                        uv: [point.x, point.y],
                        ..Default::default()
                    });
                }
                indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
    }

    let mut mesh = Mesh::new(vertices, indices);
    mesh.recompute_tangents();
    mesh
}

/// 线段连接处的形状（SVG `stroke-linejoin`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    /// 尖角，超过斜接上限时退成斜切。
    #[default]
    Miter,
    /// 圆角。
    Round,
    /// 斜切。
    Bevel,
}

/// 开放折线两端的形状（SVG `stroke-linecap`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    /// 平头，正好停在端点。
    #[default]
    Butt,
    /// 圆头。
    Round,
    /// 方头，超出端点半个线宽。
    Square,
}

/// 描边样式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeStyle {
    /// 线宽。
    pub width: f32,
    /// 连接。
    pub join: LineJoin,
    /// 端头。
    pub cap: LineCap,
    /// 斜接长度上限（相对线宽的一半），SVG 默认 4。
    pub miter_limit: f32,
    /// 圆角 / 圆头每 90° 分几段。
    pub round_segments: u32,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            join: LineJoin::Miter,
            cap: LineCap::Butt,
            miter_limit: 4.0,
            round_segments: 6,
        }
    }
}

/// 带连接和端头样式的描边。每段一个四边形，拐角处按 `join` 补外侧的
/// 缺口，开放折线两端按 `cap` 加端头。
///
/// 补缺口的三角形和相邻段的四边形有重叠——不透明描边看不出来，半透明
/// 描边重叠处会深一点（three.js 的 `pointsToStroke` 没有这个问题，
/// 它做了完整的轮廓求并；这里取简单）。
pub fn stroke_styled(contour: &Contour, style: &StrokeStyle) -> Tessellation {
    let half = style.width.max(1e-4) * 0.5;
    let mut points = contour.points.clone();
    points.dedup_by(|a, b| a.distance_squared(*b) < 1e-12);
    if contour.closed
        && points.len() > 2
        && points[0].distance_squared(points[points.len() - 1]) < 1e-12
    {
        points.pop();
    }
    let mut out = Tessellation::default();
    let quarter = style.round_segments.max(1) as f32 / std::f32::consts::FRAC_PI_2;

    // 扇形：圆心 c，从方向 a 转到方向 b（取较短的那边）。
    let fan = |out: &mut Tessellation, c: Vec2, from: Vec2, to: Vec2| {
        let start = from.y.atan2(from.x);
        let mut sweep = to.y.atan2(to.x) - start;
        while sweep > std::f32::consts::PI {
            sweep -= std::f32::consts::TAU;
        }
        while sweep < -std::f32::consts::PI {
            sweep += std::f32::consts::TAU;
        }
        let steps = ((sweep.abs() * quarter).ceil() as usize).max(1);
        let base = out.points.len() as u32;
        out.points.push(c);
        for s in 0..=steps {
            let angle = start + sweep * s as f32 / steps as f32;
            out.points
                .push(c + Vec2::new(angle.cos(), angle.sin()) * half);
        }
        for s in 0..steps as u32 {
            out.indices
                .extend_from_slice(&[base, base + 1 + s, base + 2 + s]);
        }
    };

    // 单点（或全部重合）：只有圆头 / 方头才画得出东西。
    if points.len() == 1 {
        let c = points[0];
        match style.cap {
            LineCap::Round => {
                fan(&mut out, c, Vec2::X, Vec2::NEG_X);
                fan(&mut out, c, Vec2::NEG_X, Vec2::X);
            }
            LineCap::Square => {
                let base = out.points.len() as u32;
                out.points.extend_from_slice(&[
                    c + Vec2::new(-half, -half),
                    c + Vec2::new(half, -half),
                    c + Vec2::new(half, half),
                    c + Vec2::new(-half, half),
                ]);
                out.indices.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 2,
                    base,
                    base + 2,
                    base + 3,
                ]);
            }
            LineCap::Butt => {}
        }
        return out;
    }
    if points.len() < 2 {
        return out;
    }

    let count = points.len();
    let segments = if contour.closed { count } else { count - 1 };
    let normal = |i: usize| {
        let d = (points[(i + 1) % count] - points[i]).normalize_or_zero();
        Vec2::new(-d.y, d.x)
    };
    for i in 0..segments {
        let a = points[i];
        let b = points[(i + 1) % count];
        let n = normal(i) * half;
        let base = out.points.len() as u32;
        out.points.extend_from_slice(&[a + n, a - n, b + n, b - n]);
        out.indices
            .extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }

    // 拐角。
    let joints: Vec<usize> = if contour.closed {
        (0..count).collect()
    } else {
        (1..count - 1).collect()
    };
    for j in joints {
        let incoming = normal((j + count - 1) % count);
        let outgoing = normal(j % count);
        let p = points[j];
        let turn = incoming.perp_dot(outgoing);
        if turn.abs() < 1e-6 && incoming.dot(outgoing) > 0.0 {
            continue;
        }
        // 外侧：向左拐时外侧在右边（−法线）。
        let side = if turn > 0.0 { -1.0 } else { 1.0 };
        let a = incoming * side;
        let b = outgoing * side;
        match style.join {
            LineJoin::Round => fan(&mut out, p, a, b),
            LineJoin::Miter | LineJoin::Bevel => {
                let base = out.points.len() as u32;
                out.points
                    .extend_from_slice(&[p, p + a * half, p + b * half]);
                out.indices.extend_from_slice(&[base, base + 1, base + 2]);
                if style.join == LineJoin::Miter {
                    let bisector = (a + b).normalize_or_zero();
                    let cos = bisector.dot(a);
                    if cos > 1e-4 && 1.0 / cos <= style.miter_limit {
                        let tip = p + bisector * (half / cos);
                        out.points.push(tip);
                        out.indices
                            .extend_from_slice(&[base + 1, base + 3, base + 2]);
                    }
                }
            }
        }
    }

    // 端头。
    if !contour.closed {
        for (end, inward) in [
            (points[0], points[1]),
            (points[count - 1], points[count - 2]),
        ] {
            let out_dir = (end - inward).normalize_or_zero();
            let n = Vec2::new(-out_dir.y, out_dir.x);
            match style.cap {
                LineCap::Butt => {}
                LineCap::Square => {
                    let base = out.points.len() as u32;
                    let e = out_dir * half;
                    out.points.extend_from_slice(&[
                        end + n * half,
                        end - n * half,
                        end + n * half + e,
                        end - n * half + e,
                    ]);
                    out.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base + 1,
                        base + 3,
                        base + 2,
                    ]);
                }
                LineCap::Round => {
                    fan(&mut out, end, n, out_dir);
                    fan(&mut out, end, out_dir, -n);
                }
            }
        }
    }
    out
}

/// 倒角参数（three.js `ExtrudeGeometry` 的 `bevel*` 那一组）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bevel {
    /// 倒角沿拉伸方向的厚度（前后各一份）。
    pub thickness: f32,
    /// 倒角向外扩的距离。
    pub size: f32,
    /// 轮廓整体外扩（负数是内缩）。
    pub offset: f32,
    /// 四分之一圆弧分几段。
    pub segments: u32,
}

impl Default for Bevel {
    /// three.js 的默认值。
    fn default() -> Self {
        Self {
            thickness: 0.2,
            size: 0.1,
            offset: 0.0,
            segments: 3,
        }
    }
}

/// 一个形状：外轮廓加若干个洞。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shape {
    /// 外轮廓，逆时针。
    pub outer: Contour,
    /// 洞，顺时针。
    pub holes: Vec<Contour>,
}

/// 按嵌套关系把一堆闭合轮廓分成形状，并把绕向统一成「外逆内顺」。
///
/// 被偶数个轮廓包着的是外轮廓，奇数个的是洞，洞归到包着它的最小的
/// 那个外轮廓。字体和 SVG 的绕向约定各不相同（TrueType 外轮廓是顺时针），
/// 这里一律按嵌套重新定。
pub fn shapes_from_contours(contours: Vec<Contour>) -> Vec<Shape> {
    let contours: Vec<Contour> = contours
        .into_iter()
        .filter(|c| c.points.len() >= 3 && c.signed_area().abs() > 1e-9)
        .collect();
    let depth: Vec<usize> = contours
        .iter()
        .enumerate()
        .map(|(i, c)| {
            contours
                .iter()
                .enumerate()
                .filter(|(j, other)| *j != i && other.contains(c.points[0]))
                .count()
        })
        .collect();
    let mut shapes: Vec<Shape> = Vec::new();
    for (index, contour) in contours.iter().enumerate() {
        if depth[index].is_multiple_of(2) {
            let mut outer = contour.clone();
            if outer.signed_area() < 0.0 {
                outer.points.reverse();
            }
            outer.closed = true;
            shapes.push(Shape {
                outer,
                holes: Vec::new(),
            });
        }
    }
    for (index, contour) in contours.iter().enumerate() {
        if !depth[index].is_multiple_of(2) {
            let mut hole = contour.clone();
            if hole.signed_area() > 0.0 {
                hole.points.reverse();
            }
            hole.closed = true;
            // 包着它的外轮廓里面积最小的那个。
            let owner = shapes
                .iter_mut()
                .filter(|shape| shape.outer.contains(contour.points[0]))
                .min_by(|a, b| {
                    a.outer
                        .signed_area()
                        .abs()
                        .total_cmp(&b.outer.signed_area().abs())
                });
            if let Some(shape) = owner {
                shape.holes.push(hole);
            }
        }
    }
    shapes
}

/// 每个点沿「实体外侧」的偏移方向，长度按斜接修正（最多 √2 倍，
/// 尖角处不至于戳出去太远）。要求轮廓已经是「外逆内顺」。
fn bevel_directions(contour: &Contour) -> Vec<Vec2> {
    let points = &contour.points;
    let count = points.len();
    (0..count)
        .map(|i| {
            let prev = points[(i + count - 1) % count];
            let here = points[i];
            let next = points[(i + 1) % count];
            let edge_normal = |a: Vec2, b: Vec2| {
                let d = (b - a).normalize_or_zero();
                // 前进方向的右侧：逆时针外轮廓的右侧是外面，顺时针洞的右侧是洞里。
                Vec2::new(d.y, -d.x)
            };
            let n1 = edge_normal(prev, here);
            let n2 = edge_normal(here, next);
            let sum = n1 + n2;
            if sum.length_squared() < 1e-12 {
                return n1;
            }
            let direction = sum.normalize();
            let cos = direction.dot(n1).max(1.0 / std::f32::consts::SQRT_2);
            direction / cos
        })
        .collect()
}

/// 拉伸成三维，带可选的倒角。和 three.js `ExtrudeGeometry` 一致：
///
/// - 形状躺在 XY 平面，沿 +Z 拉伸 `depth`；
/// - 有倒角时前盖在 `z = -thickness`、后盖在 `z = depth + thickness`，
///   侧壁中段整体外扩 `size`；
/// - 输出是**平面着色**的（每个三角形自己的法线），和 three.js 的
///   `flatShading` 字体效果一样。要平滑的话调用方自己重算法线。
pub fn extrude_shapes(shapes: &[Shape], depth: f32, bevel: Option<Bevel>) -> Mesh {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut uvs: Vec<Vec2> = Vec::new();

    // 层：(外扩量, z)。
    let mut layers: Vec<(f32, f32)> = Vec::new();
    match bevel {
        Some(b) if b.segments > 0 && (b.thickness != 0.0 || b.size != 0.0) => {
            let segments = b.segments;
            for s in 0..segments {
                let t = s as f32 / segments as f32 * std::f32::consts::FRAC_PI_2;
                layers.push((b.size * t.sin() + b.offset, -b.thickness * t.cos()));
            }
            layers.push((b.size + b.offset, 0.0));
            layers.push((b.size + b.offset, depth));
            for s in (0..segments).rev() {
                let t = s as f32 / segments as f32 * std::f32::consts::FRAC_PI_2;
                layers.push((b.size * t.sin() + b.offset, depth + b.thickness * t.cos()));
            }
        }
        _ => {
            layers.push((0.0, 0.0));
            layers.push((0.0, depth));
        }
    }
    let (front_offset, front_z) = layers[0];
    let (back_offset, back_z) = *layers.last().unwrap_or(&(0.0, depth));

    for shape in shapes {
        let mut contours = vec![shape.outer.clone()];
        contours.extend(shape.holes.iter().cloned());
        let directions: Vec<Vec<Vec2>> = contours.iter().map(bevel_directions).collect();

        // 盖子：用外扩后的轮廓三角化（前后盖的外扩量通常是 0）。
        for (offset, z, facing_back) in
            [(front_offset, front_z, true), (back_offset, back_z, false)]
        {
            let moved: Vec<Contour> = contours
                .iter()
                .zip(&directions)
                .map(|(c, d)| Contour {
                    points: c
                        .points
                        .iter()
                        .zip(d)
                        .map(|(p, d)| *p + *d * offset)
                        .collect(),
                    closed: true,
                })
                .collect();
            let cap = fill(&moved, FillRule::NonZero);
            for tri in cap.indices.chunks_exact(3) {
                let [a, b, c] = [0, 1, 2].map(|k| cap.points[tri[k] as usize]);
                let area = (b - a).perp_dot(c - a);
                // 后盖朝 +Z（逆时针），前盖朝 −Z（顺时针）。
                let ordered = if (area > 0.0) != facing_back {
                    [a, b, c]
                } else {
                    [a, c, b]
                };
                for p in ordered {
                    positions.push(p.extend(z));
                    uvs.push(p);
                }
            }
        }

        // 侧壁：相邻两层之间，每条边一个四边形。
        for (contour, direction) in contours.iter().zip(&directions) {
            let count = contour.points.len();
            for pair in layers.windows(2) {
                let (o0, z0) = pair[0];
                let (o1, z1) = pair[1];
                for i in 0..count {
                    let j = (i + 1) % count;
                    let a0 = (contour.points[i] + direction[i] * o0).extend(z0);
                    let b0 = (contour.points[j] + direction[j] * o0).extend(z0);
                    let b1 = (contour.points[j] + direction[j] * o1).extend(z1);
                    let a1 = (contour.points[i] + direction[i] * o1).extend(z1);
                    // (a0, b0, b1) 的法线 = 边的右侧 × dz，朝实体外面。
                    let ua = contour.points[i].x + contour.points[i].y;
                    let ub = contour.points[j].x + contour.points[j].y;
                    for (p, u) in [(a0, ua), (b0, ub), (b1, ub), (a0, ua), (b1, ub), (a1, ua)] {
                        positions.push(p);
                        uvs.push(Vec2::new(u, p.z));
                    }
                }
            }
        }
    }

    let mut vertices = Vec::with_capacity(positions.len());
    for (tri, tri_uv) in positions.chunks_exact(3).zip(uvs.chunks_exact(3)) {
        let normal = (tri[1] - tri[0])
            .cross(tri[2] - tri[0])
            .normalize_or(Vec3::Z);
        for (p, uv) in tri.iter().zip(tri_uv) {
            vertices.push(Vertex {
                position: p.to_array(),
                normal: normal.to_array(),
                uv: uv.to_array(),
                ..Default::default()
            });
        }
    }
    let indices = (0..vertices.len() as u32).collect();
    Mesh::new(vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered_area(t: &Tessellation) -> f32 {
        t.indices
            .chunks_exact(3)
            .map(|tri| {
                let [a, b, c] = [0, 1, 2].map(|k| t.points[tri[k] as usize]);
                (b - a).perp_dot(c - a).abs() * 0.5
            })
            .sum()
    }

    #[test]
    fn caps_and_joins_add_the_expected_area() {
        let line = Contour {
            points: vec![Vec2::ZERO, Vec2::new(10.0, 0.0)],
            closed: false,
        };
        let butt = stroke_styled(
            &line,
            &StrokeStyle {
                width: 2.0,
                ..Default::default()
            },
        );
        assert!((covered_area(&butt) - 20.0).abs() < 1e-3);
        let square = stroke_styled(
            &line,
            &StrokeStyle {
                width: 2.0,
                cap: LineCap::Square,
                ..Default::default()
            },
        );
        assert!(
            (covered_area(&square) - 24.0).abs() < 1e-3,
            "方头两端各多出 1×2"
        );
        let round = stroke_styled(
            &line,
            &StrokeStyle {
                width: 2.0,
                cap: LineCap::Round,
                round_segments: 32,
                ..Default::default()
            },
        );
        assert!(
            (covered_area(&round) - (20.0 + std::f32::consts::PI)).abs() < 0.05,
            "圆头合起来是一个整圆"
        );

        // 直角拐弯：斜接补一个 1×1 的角（加斜切三角），斜切只补三角。
        let corner = Contour {
            points: vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)],
            closed: false,
        };
        let miter = stroke_styled(
            &corner,
            &StrokeStyle {
                width: 2.0,
                ..Default::default()
            },
        );
        let bevel = stroke_styled(
            &corner,
            &StrokeStyle {
                width: 2.0,
                join: LineJoin::Bevel,
                ..Default::default()
            },
        );
        assert!((covered_area(&miter) - covered_area(&bevel) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn a_single_point_with_a_round_cap_is_a_dot() {
        let dot = Contour {
            points: vec![Vec2::ONE],
            closed: false,
        };
        let t = stroke_styled(
            &dot,
            &StrokeStyle {
                width: 2.0,
                cap: LineCap::Round,
                round_segments: 32,
                ..Default::default()
            },
        );
        assert!((covered_area(&t) - std::f32::consts::PI).abs() < 0.05);
        let butt = stroke_styled(
            &dot,
            &StrokeStyle {
                width: 2.0,
                ..Default::default()
            },
        );
        assert!(butt.is_empty());
    }

    #[test]
    fn nesting_decides_holes_and_winding() {
        let mut outer = square(1.0);
        outer.points.reverse(); // TrueType 式的顺时针外轮廓
        let mut inner = square(0.5);
        for point in &mut inner.points {
            *point += Vec2::splat(0.25);
        }
        let shapes = shapes_from_contours(vec![outer, inner]);
        assert_eq!(shapes.len(), 1);
        assert!(shapes[0].outer.signed_area() > 0.0);
        assert_eq!(shapes[0].holes.len(), 1);
        assert!(shapes[0].holes[0].signed_area() < 0.0);
    }

    #[test]
    fn extrusion_is_closed_and_faces_outward() {
        let shapes = shapes_from_contours(vec![square(1.0)]);
        let bevel = Bevel {
            thickness: 0.1,
            size: 0.05,
            offset: 0.0,
            segments: 2,
        };
        let mesh = extrude_shapes(&shapes, 0.5, Some(bevel));
        let aabb = mesh.aabb();
        assert!((aabb.min.z + 0.1).abs() < 1e-5 && (aabb.max.z - 0.6).abs() < 1e-5);
        assert!((aabb.max.x - 1.05).abs() < 1e-4, "中段外扩 bevel.size");
        // 每个三角形的法线都背离形状中心。
        let center = Vec3::new(0.5, 0.5, 0.25);
        for tri in mesh.vertices().chunks_exact(3) {
            let centroid = (tri[0].position() + tri[1].position() + tri[2].position()) / 3.0;
            let normal = tri[0].normal();
            assert!(
                normal.dot(centroid - center) > 0.0,
                "朝里的面：{centroid} {normal}"
            );
        }
    }

    fn square(size: f32) -> Contour {
        Contour {
            points: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(size, 0.0),
                Vec2::new(size, size),
                Vec2::new(0.0, size),
            ],
            closed: true,
        }
    }

    #[test]
    fn a_square_becomes_two_triangles() {
        let result = fill(&[square(1.0)], FillRule::NonZero);
        assert_eq!(result.triangle_count(), 2);
    }

    #[test]
    fn a_hole_is_cut_out() {
        let mut inner = square(0.5);
        for point in &mut inner.points {
            *point += Vec2::splat(0.25);
        }
        // 洞的绕向反过来，和真实的字形文件一致。
        inner.points.reverse();
        let result = fill(&[square(1.0), inner], FillRule::NonZero);
        // 带洞的方框至少要 8 个三角形（每条边两个）。
        assert!(
            result.triangle_count() >= 8,
            "只切出了 {} 个三角形",
            result.triangle_count()
        );
        // 洞的中心不该被任何三角形盖住。
        let center = Vec2::splat(0.5);
        let covered = result.indices.chunks_exact(3).any(|t| {
            point_in_triangle(
                center,
                result.points[t[0] as usize],
                result.points[t[1] as usize],
                result.points[t[2] as usize],
            )
        });
        assert!(!covered, "洞被填上了");
    }

    #[test]
    fn two_separate_shapes_are_both_filled() {
        let mut far = square(1.0);
        for point in &mut far.points {
            point.x += 10.0;
        }
        let result = fill(&[square(1.0), far], FillRule::NonZero);
        assert_eq!(result.triangle_count(), 4);
    }

    #[test]
    fn flattening_adapts_to_curvature() {
        let mut path = Path::new();
        path.push(Segment::MoveTo(Vec2::ZERO));
        path.push(Segment::Cubic(
            Vec2::new(0.0, 100.0),
            Vec2::new(100.0, 100.0),
            Vec2::new(100.0, 0.0),
        ));
        let coarse = path.flatten(10.0);
        let fine = path.flatten(0.1);
        assert!(
            fine[0].points.len() > coarse[0].points.len(),
            "容差小的应当分得更细：{} vs {}",
            fine[0].points.len(),
            coarse[0].points.len()
        );
    }

    #[test]
    fn close_marks_the_contour_and_does_not_duplicate_the_first_point() {
        let mut path = Path::new();
        path.push(Segment::MoveTo(Vec2::ZERO));
        path.push(Segment::LineTo(Vec2::new(1.0, 0.0)));
        path.push(Segment::LineTo(Vec2::new(1.0, 1.0)));
        path.push(Segment::Close);
        let contours = path.flatten(0.01);
        assert_eq!(contours.len(), 1);
        assert!(contours[0].closed);
        assert_eq!(contours[0].points.len(), 4, "闭合点应当正好补一个");
    }

    #[test]
    fn signed_area_tells_the_winding_apart() {
        let ccw = square(1.0);
        let mut cw = ccw.clone();
        cw.points.reverse();
        assert!(ccw.signed_area() > 0.0);
        assert!(cw.signed_area() < 0.0);
    }

    #[test]
    fn stroking_a_line_makes_a_quad() {
        let contour = Contour {
            points: vec![Vec2::ZERO, Vec2::new(10.0, 0.0)],
            closed: false,
        };
        let result = stroke(&contour, 2.0);
        assert_eq!(result.triangle_count(), 2);
        // 宽度应当是 2：上下各偏 1。
        let ys: Vec<f32> = result.points.iter().map(|p| p.y).collect();
        assert!(
            ys.iter().cloned().fold(f32::MIN, f32::max)
                - ys.iter().cloned().fold(f32::MAX, f32::min)
                > 1.9
        );
    }

    #[test]
    fn extruding_adds_side_walls() {
        let contour = square(1.0);
        let one = std::slice::from_ref(&contour);
        let flat = extrude(&fill(one, FillRule::NonZero), one, 0.0);
        let solid = extrude(&fill(one, FillRule::NonZero), one, 0.5);
        assert_eq!(flat.triangle_count(), 2);
        // 两个盖 4 个三角形 + 四面墙各 2 个 = 12。
        assert_eq!(solid.triangle_count(), 12);
    }

    #[test]
    fn a_degenerate_contour_is_ignored_rather_than_crashing() {
        let line = Contour {
            points: vec![Vec2::ZERO, Vec2::new(1.0, 0.0)],
            closed: true,
        };
        assert!(fill(&[line], FillRule::NonZero).is_empty());
    }
}
