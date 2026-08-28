//! 几何图元：二维与三维的基本形状。
//!
//! 这些类型**只描述形状，不含位置与朝向**——一个 [`Sphere`] 就是「半径 r」，
//! 摆在哪由调用方给。这样同一个形状能被复用（一百个同样的球共享一个描述），
//! 也让「形状」与「位姿」这两件事各归各位。
//!
//! # 它们用来做什么
//!
//! - **算面积、体积、包围盒**：物理属性、剔除、UI 布局；
//! - **采样**（[`ShapeSample`]）：在形状里或形状表面上取随机点——粒子发射、
//!   刷怪点、程序化散布植被；
//! - **画出来**：`kgizmo` 认得这些类型，调试时一行就能画一个胶囊体。
//!
//! # 为什么不直接用网格
//!
//! 网格是**离散**的，一个球被切成几百个三角形之后，「球心到表面的距离恒等于
//! 半径」这件事就不再精确成立了。碰撞、采样、包围盒都想要那个精确的定义，
//! 所以形状与它的网格是两回事——后者由 `kmesh` 按前者生成。

use crate::{Aabb, Aabb2d, BoundingCircle, Rng, Vec2, Vec3};
use std::f32::consts::{PI, TAU};

// ══════════════════════════════════════════════════════════════════════════════
// 二维
// ══════════════════════════════════════════════════════════════════════════════

/// 圆。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle {
    /// 半径。
    pub radius: f32,
}

impl Circle {
    /// 由半径构造。
    pub fn new(radius: f32) -> Self {
        Self { radius }
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        PI * self.radius * self.radius
    }

    /// 周长。
    pub fn perimeter(&self) -> f32 {
        TAU * self.radius
    }
}

/// 轴对齐的矩形，由半尺寸描述。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rectangle {
    /// 半宽与半高。
    pub half_size: Vec2,
}

impl Rectangle {
    /// 由整个宽高构造。
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            half_size: Vec2::new(width, height) * 0.5,
        }
    }

    /// 正方形。
    pub fn square(side: f32) -> Self {
        Self::new(side, side)
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        4.0 * self.half_size.x * self.half_size.y
    }

    /// 周长。
    pub fn perimeter(&self) -> f32 {
        4.0 * (self.half_size.x + self.half_size.y)
    }
}

/// 椭圆。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse {
    /// 两个方向的半轴长。
    pub half_size: Vec2,
}

impl Ellipse {
    /// 由两个半轴构造。
    pub fn new(half_width: f32, half_height: f32) -> Self {
        Self {
            half_size: Vec2::new(half_width, half_height),
        }
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        PI * self.half_size.x * self.half_size.y
    }
}

/// 圆环（甜甜圈的二维版）：两个同心圆之间的部分。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Annulus {
    /// 内圈半径。
    pub inner_radius: f32,
    /// 外圈半径。
    pub outer_radius: f32,
}

impl Annulus {
    /// 由内外半径构造，顺序写反也认。
    pub fn new(inner_radius: f32, outer_radius: f32) -> Self {
        Self {
            inner_radius: inner_radius.min(outer_radius),
            outer_radius: inner_radius.max(outer_radius),
        }
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        PI * (self.outer_radius * self.outer_radius - self.inner_radius * self.inner_radius)
    }
}

/// 三角形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triangle2d {
    /// 三个顶点。
    pub vertices: [Vec2; 3],
}

impl Triangle2d {
    /// 由三个顶点构造。
    pub fn new(a: Vec2, b: Vec2, c: Vec2) -> Self {
        Self {
            vertices: [a, b, c],
        }
    }

    /// 面积。退化成一条线时是 0。
    pub fn area(&self) -> f32 {
        let [a, b, c] = self.vertices;
        ((b - a).perp_dot(c - a) * 0.5).abs()
    }
}

/// 二维胶囊：一个矩形两头各接半个圆。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capsule2d {
    /// 半径。
    pub radius: f32,
    /// 中间那段直的部分的**一半**长度。
    pub half_length: f32,
}

impl Capsule2d {
    /// 由半径与整段直边长度构造。
    pub fn new(radius: f32, length: f32) -> Self {
        Self {
            radius,
            half_length: length * 0.5,
        }
    }

    /// 面积：中间的矩形加上两端拼成的整圆。
    pub fn area(&self) -> f32 {
        2.0 * self.radius * self.half_length * 2.0 + PI * self.radius * self.radius
    }
}

/// 正多边形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegularPolygon {
    /// 外接圆半径（中心到顶点）。
    pub circumradius: f32,
    /// 边数。
    pub sides: u32,
}

impl RegularPolygon {
    /// 由外接圆半径与边数构造。边数至少为 3。
    pub fn new(circumradius: f32, sides: u32) -> Self {
        Self {
            circumradius,
            sides: sides.max(3),
        }
    }

    /// 全部顶点，从正上方开始逆时针排。
    pub fn vertices(&self) -> Vec<Vec2> {
        (0..self.sides)
            .map(|i| {
                // 从 +Y 起步：正多边形画出来「尖朝上」符合直觉。
                let angle = i as f32 / self.sides as f32 * TAU + PI * 0.5;
                Vec2::new(angle.cos(), angle.sin()) * self.circumradius
            })
            .collect()
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        let n = self.sides as f32;
        0.5 * n * self.circumradius * self.circumradius * (TAU / n).sin()
    }
}

/// 一段圆弧。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc2d {
    /// 半径。
    pub radius: f32,
    /// 张角的**一半**（弧度）。弧以 +Y 方向为中线，向两边各张这么多。
    pub half_angle: f32,
}

impl Arc2d {
    /// 由半径与整个张角构造。
    pub fn new(radius: f32, angle: f32) -> Self {
        Self {
            radius,
            half_angle: angle * 0.5,
        }
    }

    /// 弧长。
    pub fn length(&self) -> f32 {
        self.radius * self.half_angle * 2.0
    }

    /// 弧上的点，`t ∈ [0, 1]` 从一端走到另一端。
    pub fn point(&self, t: f32) -> Vec2 {
        let angle = PI * 0.5 + (t * 2.0 - 1.0) * self.half_angle;
        Vec2::new(angle.cos(), angle.sin()) * self.radius
    }
}

/// 扇形：圆弧加上到圆心的两条半径（一块披萨）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircularSector {
    /// 边界弧。
    pub arc: Arc2d,
}

impl CircularSector {
    /// 由半径与整个张角构造。
    pub fn new(radius: f32, angle: f32) -> Self {
        Self {
            arc: Arc2d::new(radius, angle),
        }
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        self.arc.radius * self.arc.radius * self.arc.half_angle
    }
}

/// 弓形：圆弧加上连接两端的**弦**（切掉一块的圆）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircularSegment {
    /// 边界弧。
    pub arc: Arc2d,
}

impl CircularSegment {
    /// 由半径与整个张角构造。
    pub fn new(radius: f32, angle: f32) -> Self {
        Self {
            arc: Arc2d::new(radius, angle),
        }
    }

    /// 面积：扇形减去中间那个三角形。
    pub fn area(&self) -> f32 {
        let angle = self.arc.half_angle * 2.0;
        0.5 * self.arc.radius * self.arc.radius * (angle - angle.sin())
    }
}

/// 二维线段。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment2d {
    /// 两个端点。
    pub endpoints: [Vec2; 2],
}

impl Segment2d {
    /// 由两个端点构造。
    pub fn new(from: Vec2, to: Vec2) -> Self {
        Self {
            endpoints: [from, to],
        }
    }

    /// 长度。
    pub fn length(&self) -> f32 {
        self.endpoints[0].distance(self.endpoints[1])
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 三维
// ══════════════════════════════════════════════════════════════════════════════

/// 球。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sphere {
    /// 半径。
    pub radius: f32,
}

impl Sphere {
    /// 由半径构造。
    pub fn new(radius: f32) -> Self {
        Self { radius }
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        4.0 / 3.0 * PI * self.radius.powi(3)
    }

    /// 表面积。
    pub fn surface_area(&self) -> f32 {
        4.0 * PI * self.radius * self.radius
    }
}

/// 长方体，由半尺寸描述。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cuboid {
    /// 三个轴向的半长。
    pub half_size: Vec3,
}

impl Cuboid {
    /// 由整个长宽高构造。
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self {
            half_size: Vec3::new(x, y, z) * 0.5,
        }
    }

    /// 正方体。
    pub fn cube(side: f32) -> Self {
        Self::new(side, side, side)
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        8.0 * self.half_size.x * self.half_size.y * self.half_size.z
    }

    /// 表面积。
    pub fn surface_area(&self) -> f32 {
        let h = self.half_size;
        8.0 * (h.x * h.y + h.y * h.z + h.z * h.x)
    }
}

/// 圆柱，轴沿 Y。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cylinder {
    /// 半径。
    pub radius: f32,
    /// 高的一半。
    pub half_height: f32,
}

impl Cylinder {
    /// 由半径与整个高度构造。
    pub fn new(radius: f32, height: f32) -> Self {
        Self {
            radius,
            half_height: height * 0.5,
        }
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        PI * self.radius * self.radius * self.half_height * 2.0
    }

    /// 表面积（含两个底面）。
    pub fn surface_area(&self) -> f32 {
        TAU * self.radius * (self.radius + self.half_height * 2.0)
    }
}

/// 三维胶囊：圆柱两头各接半个球。轴沿 Y。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capsule3d {
    /// 半径。
    pub radius: f32,
    /// 中间圆柱段高度的一半。
    pub half_length: f32,
}

impl Capsule3d {
    /// 由半径与整个圆柱段长度构造。
    pub fn new(radius: f32, length: f32) -> Self {
        Self {
            radius,
            half_length: length * 0.5,
        }
    }

    /// 体积：圆柱加上两端拼成的整球。
    pub fn volume(&self) -> f32 {
        let cylinder = PI * self.radius * self.radius * self.half_length * 2.0;
        cylinder + 4.0 / 3.0 * PI * self.radius.powi(3)
    }
}

/// 圆锥，轴沿 Y，尖朝上。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cone {
    /// 底面半径。
    pub radius: f32,
    /// 高的一半。
    pub half_height: f32,
}

impl Cone {
    /// 由底面半径与整个高度构造。
    pub fn new(radius: f32, height: f32) -> Self {
        Self {
            radius,
            half_height: height * 0.5,
        }
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        PI * self.radius * self.radius * self.half_height * 2.0 / 3.0
    }

    /// 母线长（底面边缘到顶点）。
    pub fn slant_height(&self) -> f32 {
        (self.radius * self.radius + (self.half_height * 2.0).powi(2)).sqrt()
    }
}

/// 圆台：削掉尖的圆锥。轴沿 Y。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConicalFrustum {
    /// 下底半径。
    pub radius_bottom: f32,
    /// 上底半径。
    pub radius_top: f32,
    /// 高的一半。
    pub half_height: f32,
}

impl ConicalFrustum {
    /// 由上下底半径与整个高度构造。
    pub fn new(radius_bottom: f32, radius_top: f32, height: f32) -> Self {
        Self {
            radius_bottom,
            radius_top,
            half_height: height * 0.5,
        }
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        let (r, t) = (self.radius_bottom, self.radius_top);
        PI * self.half_height * 2.0 / 3.0 * (r * r + r * t + t * t)
    }
}

/// 圆环体（甜甜圈）。主轴沿 Y，环躺在 XZ 平面上。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Torus {
    /// 主半径：中心到管子中心线。
    pub major_radius: f32,
    /// 管半径：管子自己的粗细。
    pub minor_radius: f32,
}

impl Torus {
    /// 由主半径与管半径构造。
    pub fn new(major_radius: f32, minor_radius: f32) -> Self {
        Self {
            major_radius,
            minor_radius,
        }
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        2.0 * PI * PI * self.major_radius * self.minor_radius * self.minor_radius
    }

    /// 表面积。
    pub fn surface_area(&self) -> f32 {
        4.0 * PI * PI * self.major_radius * self.minor_radius
    }
}

/// 三维三角形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triangle3d {
    /// 三个顶点。
    pub vertices: [Vec3; 3],
}

impl Triangle3d {
    /// 由三个顶点构造。
    pub fn new(a: Vec3, b: Vec3, c: Vec3) -> Self {
        Self {
            vertices: [a, b, c],
        }
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        let [a, b, c] = self.vertices;
        (b - a).cross(c - a).length() * 0.5
    }

    /// 法线。退化成一条线时返回 [`None`]。
    pub fn normal(&self) -> Option<Vec3> {
        let [a, b, c] = self.vertices;
        (b - a).cross(c - a).try_normalize()
    }
}

/// 四面体。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tetrahedron {
    /// 四个顶点。
    pub vertices: [Vec3; 4],
}

impl Tetrahedron {
    /// 由四个顶点构造。
    pub fn new(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> Self {
        Self {
            vertices: [a, b, c, d],
        }
    }

    /// 体积。
    pub fn volume(&self) -> f32 {
        let [a, b, c, d] = self.vertices;
        (b - a).cross(c - a).dot(d - a).abs() / 6.0
    }

    /// 四个面，顶点顺序都朝外。
    pub fn faces(&self) -> [Triangle3d; 4] {
        let [a, b, c, d] = self.vertices;
        [
            Triangle3d::new(a, b, c),
            Triangle3d::new(a, c, d),
            Triangle3d::new(a, d, b),
            Triangle3d::new(b, d, c),
        ]
    }
}

/// 三维线段。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment3d {
    /// 两个端点。
    pub endpoints: [Vec3; 2],
}

impl Segment3d {
    /// 由两个端点构造。
    pub fn new(from: Vec3, to: Vec3) -> Self {
        Self {
            endpoints: [from, to],
        }
    }

    /// 长度。
    pub fn length(&self) -> f32 {
        self.endpoints[0].distance(self.endpoints[1])
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 包围体
// ══════════════════════════════════════════════════════════════════════════════

/// 能算出自己二维包围体的形状。
pub trait Bounded2d {
    /// 轴对齐包围盒（形状按 `angle` 弧度转过之后）。
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d;
    /// 包围圆。
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle;
}

/// 能算出自己三维包围盒的形状。
pub trait Bounded3d {
    /// 轴对齐包围盒。
    fn aabb_3d(&self, translation: Vec3) -> Aabb;
}

/// 把一组点转过一个角度再求包围盒——多边形类形状的通用实现。
fn rotated_aabb(points: &[Vec2], translation: Vec2, angle: f32) -> Aabb2d {
    let (sin, cos) = angle.sin_cos();
    let rotated: Vec<Vec2> = points
        .iter()
        .map(|p| translation + Vec2::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos))
        .collect();
    Aabb2d::from_points(&rotated)
}

impl Bounded2d for Circle {
    fn aabb_2d(&self, translation: Vec2, _angle: f32) -> Aabb2d {
        // 圆转多少度都一样，所以 angle 用不上。
        Aabb2d::new(translation, Vec2::splat(self.radius))
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        BoundingCircle::new(translation, self.radius)
    }
}

impl Bounded2d for Rectangle {
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d {
        // 转过之后的矩形不再轴对齐，包围盒会胀大——这正是包围盒要随姿态
        // 重算的原因。
        let h = self.half_size;
        let corners = [
            Vec2::new(-h.x, -h.y),
            Vec2::new(h.x, -h.y),
            Vec2::new(h.x, h.y),
            Vec2::new(-h.x, h.y),
        ];
        rotated_aabb(&corners, translation, angle)
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        BoundingCircle::new(translation, self.half_size.length())
    }
}

impl Bounded2d for Triangle2d {
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d {
        rotated_aabb(&self.vertices, translation, angle)
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        let mut circle = BoundingCircle::from_points(&self.vertices);
        circle.center += translation;
        circle
    }
}

impl Bounded2d for Capsule2d {
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d {
        // 两端球心转过去，再各外扩一个半径。
        let ends = [
            Vec2::new(0.0, -self.half_length),
            Vec2::new(0.0, self.half_length),
        ];
        rotated_aabb(&ends, translation, angle).grown(Vec2::splat(self.radius))
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        BoundingCircle::new(translation, self.half_length + self.radius)
    }
}

impl Bounded2d for RegularPolygon {
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d {
        rotated_aabb(&self.vertices(), translation, angle)
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        BoundingCircle::new(translation, self.circumradius)
    }
}

impl Bounded2d for Ellipse {
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d {
        // 转过的椭圆有闭式解：半轴投影到坐标轴上。
        // 拿外接矩形的四角去算会明显偏大——那是矩形的包围盒，不是椭圆的。
        let (sin, cos) = angle.sin_cos();
        let (a, b) = (self.half_size.x, self.half_size.y);
        let half = Vec2::new(
            ((a * cos).powi(2) + (b * sin).powi(2)).sqrt(),
            ((a * sin).powi(2) + (b * cos).powi(2)).sqrt(),
        );
        Aabb2d::new(translation, half)
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        BoundingCircle::new(translation, self.half_size.max_element())
    }
}

impl Bounded2d for Annulus {
    fn aabb_2d(&self, translation: Vec2, _angle: f32) -> Aabb2d {
        Aabb2d::new(translation, Vec2::splat(self.outer_radius))
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        BoundingCircle::new(translation, self.outer_radius)
    }
}

impl Bounded2d for Segment2d {
    fn aabb_2d(&self, translation: Vec2, angle: f32) -> Aabb2d {
        rotated_aabb(&self.endpoints, translation, angle)
    }
    fn bounding_circle(&self, translation: Vec2) -> BoundingCircle {
        let [a, b] = self.endpoints;
        BoundingCircle::new(translation + (a + b) * 0.5, a.distance(b) * 0.5)
    }
}

impl Bounded3d for Sphere {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        Aabb::from_center_half_extents(translation, Vec3::splat(self.radius))
    }
}

impl Bounded3d for Cuboid {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        Aabb::from_center_half_extents(translation, self.half_size)
    }
}

impl Bounded3d for Cylinder {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        Aabb::from_center_half_extents(
            translation,
            Vec3::new(self.radius, self.half_height, self.radius),
        )
    }
}

impl Bounded3d for Capsule3d {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        Aabb::from_center_half_extents(
            translation,
            Vec3::new(
                self.radius,
                self.half_length + self.radius,
                self.radius,
            ),
        )
    }
}

impl Bounded3d for Cone {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        Aabb::from_center_half_extents(
            translation,
            Vec3::new(self.radius, self.half_height, self.radius),
        )
    }
}

impl Bounded3d for Torus {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        let reach = self.major_radius + self.minor_radius;
        Aabb::from_center_half_extents(
            translation,
            Vec3::new(reach, self.minor_radius, reach),
        )
    }
}

impl Bounded3d for Triangle3d {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        let mut aabb = Aabb::new(self.vertices[0], self.vertices[0]);
        for vertex in &self.vertices[1..] {
            aabb.expand(*vertex);
        }
        Aabb::new(aabb.min + translation, aabb.max + translation)
    }
}

impl Bounded3d for Tetrahedron {
    fn aabb_3d(&self, translation: Vec3) -> Aabb {
        let mut aabb = Aabb::new(self.vertices[0], self.vertices[0]);
        for vertex in &self.vertices[1..] {
            aabb.expand(*vertex);
        }
        Aabb::new(aabb.min + translation, aabb.max + translation)
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 采样
// ══════════════════════════════════════════════════════════════════════════════

/// 能在自己内部或边界上取随机点的形状。
///
/// 粒子发射、刷怪点、程序化散布植被都用它。
///
/// # 「均匀」是什么意思
///
/// **按面积（或体积）均匀**，不是按参数均匀。这两者差别很大：圆里按
/// `(随机半径, 随机角度)` 取点会在圆心附近堆成一坨，因为外圈的面积大得多。
/// 下面每个实现都为此做了修正——多数时候就是那句 `sqrt()`。
pub trait ShapeSample {
    /// 采样点的类型：二维形状给 [`Vec2`]，三维给 [`Vec3`]。
    type Output;

    /// 在形状**内部**（含边界）取一个随机点。
    fn sample_interior(&self, rng: &mut Rng) -> Self::Output;

    /// 在形状**边界**上取一个随机点。
    fn sample_boundary(&self, rng: &mut Rng) -> Self::Output;
}

impl ShapeSample for Circle {
    type Output = Vec2;

    fn sample_interior(&self, rng: &mut Rng) -> Vec2 {
        // `sqrt` 是必须的：不开方的话点会在圆心堆积——半径 r 处那一圈的
        // 面积正比于 r，所以累积分布是 r²，取反函数就是开方。
        let radius = self.radius * rng.next_f32().sqrt();
        let angle = rng.next_f32() * TAU;
        Vec2::new(angle.cos(), angle.sin()) * radius
    }

    fn sample_boundary(&self, rng: &mut Rng) -> Vec2 {
        let angle = rng.next_f32() * TAU;
        Vec2::new(angle.cos(), angle.sin()) * self.radius
    }
}

impl ShapeSample for Rectangle {
    type Output = Vec2;

    fn sample_interior(&self, rng: &mut Rng) -> Vec2 {
        Vec2::new(
            rng.next_signed() * self.half_size.x,
            rng.next_signed() * self.half_size.y,
        )
    }

    fn sample_boundary(&self, rng: &mut Rng) -> Vec2 {
        // 先按边长加权挑一条边，再在边上均匀取点。不加权的话四条边
        // 被选中的概率一样，而长边上的点就会比短边稀疏。
        let (w, h) = (self.half_size.x * 2.0, self.half_size.y * 2.0);
        let pick = rng.next_f32() * (w + h) * 2.0;
        let t = rng.next_signed();
        if pick < w {
            Vec2::new(t * self.half_size.x, -self.half_size.y)
        } else if pick < w * 2.0 {
            Vec2::new(t * self.half_size.x, self.half_size.y)
        } else if pick < w * 2.0 + h {
            Vec2::new(-self.half_size.x, t * self.half_size.y)
        } else {
            Vec2::new(self.half_size.x, t * self.half_size.y)
        }
    }
}

impl ShapeSample for Triangle2d {
    type Output = Vec2;

    fn sample_interior(&self, rng: &mut Rng) -> Vec2 {
        let [a, b, c] = self.vertices;
        // 重心坐标：两个随机数落在单位正方形里，落到上三角的那一半
        // 沿对角线折回来。这样得到的分布在三角形内是均匀的。
        let (mut u, mut v) = (rng.next_f32(), rng.next_f32());
        if u + v > 1.0 {
            u = 1.0 - u;
            v = 1.0 - v;
        }
        a + (b - a) * u + (c - a) * v
    }

    fn sample_boundary(&self, rng: &mut Rng) -> Vec2 {
        let [a, b, c] = self.vertices;
        let edges = [(a, b), (b, c), (c, a)];
        let lengths: [f32; 3] = [a.distance(b), b.distance(c), c.distance(a)];
        let total: f32 = lengths.iter().sum();

        let mut pick = rng.next_f32() * total;
        for (i, (from, to)) in edges.into_iter().enumerate() {
            if pick < lengths[i] || i == 2 {
                return from + (to - from) * rng.next_f32();
            }
            pick -= lengths[i];
        }
        a
    }
}

impl ShapeSample for Sphere {
    type Output = Vec3;

    fn sample_interior(&self, rng: &mut Rng) -> Vec3 {
        // 同样的道理，只是三维要开立方根：体积正比于 r³。
        rng.unit_vector() * (self.radius * rng.next_f32().cbrt())
    }

    fn sample_boundary(&self, rng: &mut Rng) -> Vec3 {
        rng.unit_vector() * self.radius
    }
}

impl ShapeSample for Cuboid {
    type Output = Vec3;

    fn sample_interior(&self, rng: &mut Rng) -> Vec3 {
        Vec3::new(
            rng.next_signed() * self.half_size.x,
            rng.next_signed() * self.half_size.y,
            rng.next_signed() * self.half_size.z,
        )
    }

    fn sample_boundary(&self, rng: &mut Rng) -> Vec3 {
        // 按面积加权挑一个面：六个面等概率的话，小面上的点会密得多。
        let h = self.half_size;
        let areas = [h.y * h.z, h.z * h.x, h.x * h.y];
        let total = (areas[0] + areas[1] + areas[2]) * 2.0;
        let mut pick = rng.next_f32() * total;

        let (u, v) = (rng.next_signed(), rng.next_signed());
        for (axis, area) in areas.into_iter().enumerate() {
            for side in [-1.0_f32, 1.0] {
                if pick < area {
                    return match axis {
                        0 => Vec3::new(side * h.x, u * h.y, v * h.z),
                        1 => Vec3::new(u * h.x, side * h.y, v * h.z),
                        _ => Vec3::new(u * h.x, v * h.y, side * h.z),
                    };
                }
                pick -= area;
            }
        }
        Vec3::new(h.x, u * h.y, v * h.z)
    }
}

impl ShapeSample for Cylinder {
    type Output = Vec3;

    fn sample_interior(&self, rng: &mut Rng) -> Vec3 {
        let disk = Circle::new(self.radius).sample_interior(rng);
        Vec3::new(disk.x, rng.next_signed() * self.half_height, disk.y)
    }

    fn sample_boundary(&self, rng: &mut Rng) -> Vec3 {
        let side = TAU * self.radius * self.half_height * 2.0;
        let caps = TAU * self.radius * self.radius; // 两个底面合计
        if rng.next_f32() * (side + caps) < side {
            let angle = rng.next_f32() * TAU;
            Vec3::new(
                angle.cos() * self.radius,
                rng.next_signed() * self.half_height,
                angle.sin() * self.radius,
            )
        } else {
            let disk = Circle::new(self.radius).sample_interior(rng);
            let y = if rng.next_f32() < 0.5 {
                -self.half_height
            } else {
                self.half_height
            };
            Vec3::new(disk.x, y, disk.y)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng() -> Rng {
        Rng::new(0x5EED_1234)
    }

    // ── 面积与体积 ──

    #[test]
    fn areas_and_volumes_match_the_textbook() {
        assert!((Circle::new(2.0).area() - 4.0 * PI).abs() < 1e-4);
        assert!((Rectangle::new(3.0, 4.0).area() - 12.0).abs() < 1e-4);
        assert!((Sphere::new(3.0).volume() - 36.0 * PI).abs() < 1e-3);
        assert!((Cuboid::new(2.0, 3.0, 4.0).volume() - 24.0).abs() < 1e-4);
        assert!((Cylinder::new(2.0, 5.0).volume() - 20.0 * PI).abs() < 1e-3);
    }

    #[test]
    fn a_triangles_area_survives_degeneracy() {
        let flat = Triangle2d::new(Vec2::ZERO, Vec2::X, Vec2::new(2.0, 0.0));
        assert_eq!(flat.area(), 0.0, "三点共线的三角形面积是 0");

        let unit = Triangle2d::new(Vec2::ZERO, Vec2::X, Vec2::Y);
        assert!((unit.area() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_regular_polygon_approaches_its_circle() {
        // 边数越多越接近外接圆的面积，这是一条能抓住系数写错的断言。
        let circle = Circle::new(1.0).area();
        let many = RegularPolygon::new(1.0, 720).area();
        assert!((many - circle).abs() < 1e-3, "720 边形面积 {many}");

        let square = RegularPolygon::new(1.0, 4).area();
        assert!((square - 2.0).abs() < 1e-5, "外接圆半径为 1 的正方形面积是 2");
    }

    #[test]
    fn a_capsule_is_a_rectangle_plus_a_full_circle() {
        let capsule = Capsule2d::new(1.0, 4.0);
        assert!((capsule.area() - (8.0 + PI)).abs() < 1e-4);
    }

    #[test]
    fn a_tetrahedrons_volume_is_a_sixth_of_the_box() {
        let unit = Tetrahedron::new(Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z);
        assert!((unit.volume() - 1.0 / 6.0).abs() < 1e-6);
    }

    // ── 包围体 ──

    #[test]
    fn rotating_a_rectangle_grows_its_aabb() {
        // 包围盒是**轴对齐**的，所以转 45° 之后必然胀大——这正是它要随姿态
        // 重算的原因。不重算的话剔除会把已经转出去的部分判成还在里面。
        let rectangle = Rectangle::new(2.0, 2.0);

        let straight = rectangle.aabb_2d(Vec2::ZERO, 0.0);
        let tilted = rectangle.aabb_2d(Vec2::ZERO, PI / 4.0);

        assert!((straight.half_size().x - 1.0).abs() < 1e-5);
        assert!(
            tilted.half_size().x > 1.4,
            "转 45° 之后该胀到 √2 ≈ 1.414，实际 {}",
            tilted.half_size().x
        );
    }

    #[test]
    fn rotating_a_circle_changes_nothing() {
        let circle = Circle::new(2.0);
        let a = circle.aabb_2d(Vec2::ZERO, 0.0);
        let b = circle.aabb_2d(Vec2::ZERO, 1.234);
        assert_eq!(a, b);
    }

    #[test]
    fn an_ellipse_uses_the_closed_form_not_its_corner_box() {
        // 转 90° 的椭圆，长短轴互换，包围盒也该跟着换过来。
        // 拿外接矩形的四角去算会得到一个明显偏大的方盒子。
        let ellipse = Ellipse::new(3.0, 1.0);
        let turned = ellipse.aabb_2d(Vec2::ZERO, PI / 2.0);

        assert!((turned.half_size().x - 1.0).abs() < 1e-4, "{}", turned.half_size());
        assert!((turned.half_size().y - 3.0).abs() < 1e-4);
    }

    #[test]
    fn a_capsule_bounding_circle_reaches_the_far_cap() {
        let capsule = Capsule2d::new(1.0, 4.0);
        let circle = capsule.bounding_circle(Vec2::ZERO);
        assert!((circle.radius - 3.0).abs() < 1e-5, "2 + 1 = 3");
    }

    #[test]
    fn a_torus_is_flat_in_its_own_plane() {
        let torus = Torus::new(3.0, 0.5);
        let aabb = torus.aabb_3d(Vec3::ZERO);

        assert!((aabb.half_extents().x - 3.5).abs() < 1e-5);
        assert!((aabb.half_extents().y - 0.5).abs() < 1e-5, "厚度只有管半径");
    }

    // ── 采样 ──

    #[test]
    fn sampled_points_land_inside_their_shape() {
        let mut rng = rng();
        let circle = Circle::new(2.0);
        let sphere = Sphere::new(2.0);
        let cuboid = Cuboid::new(2.0, 4.0, 6.0);

        for _ in 0..500 {
            assert!(circle.sample_interior(&mut rng).length() <= 2.0 + 1e-5);
            assert!(sphere.sample_interior(&mut rng).length() <= 2.0 + 1e-5);

            let point = cuboid.sample_interior(&mut rng);
            assert!(point.abs().cmple(cuboid.half_size + 1e-5).all(), "{point}");
        }
    }

    #[test]
    fn boundary_samples_land_on_the_boundary() {
        let mut rng = rng();
        let circle = Circle::new(2.0);
        let sphere = Sphere::new(2.0);

        for _ in 0..500 {
            assert!((circle.sample_boundary(&mut rng).length() - 2.0).abs() < 1e-4);
            assert!((sphere.sample_boundary(&mut rng).length() - 2.0).abs() < 1e-4);
        }
    }

    #[test]
    fn a_cuboids_boundary_samples_sit_on_a_face() {
        let mut rng = rng();
        let cuboid = Cuboid::new(2.0, 4.0, 6.0);

        for _ in 0..500 {
            let point = cuboid.sample_boundary(&mut rng);
            let h = cuboid.half_size;
            // 至少有一个分量顶到了对应的半尺寸上。
            let on_face = (point.x.abs() - h.x).abs() < 1e-4
                || (point.y.abs() - h.y).abs() < 1e-4
                || (point.z.abs() - h.z).abs() < 1e-4;
            assert!(on_face, "{point} 不在任何一个面上");
            assert!(point.abs().cmple(h + 1e-4).all(), "{point} 跑到盒外了");
        }
    }

    #[test]
    fn interior_samples_do_not_pile_up_at_the_centre() {
        // 少了那句 `sqrt` 的话点会往圆心堆——这是采样代码最常见的错。
        // 判据：内半圈（面积占一半）里的点应当接近一半。
        let mut rng = rng();
        let circle = Circle::new(1.0);
        let half_area_radius = 0.5_f32.sqrt();

        let inside = (0..4000)
            .filter(|_| circle.sample_interior(&mut rng).length() < half_area_radius)
            .count();

        let ratio = inside as f32 / 4000.0;
        assert!(
            (ratio - 0.5).abs() < 0.05,
            "内半圈占了 {ratio:.3}，该在 0.5 附近（堆在圆心会明显偏大）"
        );
    }

    #[test]
    fn sphere_interior_samples_are_volume_uniform() {
        // 三维版同理，只是该开立方根。
        let mut rng = rng();
        let sphere = Sphere::new(1.0);
        let half_volume_radius = 0.5_f32.cbrt();

        let inside = (0..4000)
            .filter(|_| sphere.sample_interior(&mut rng).length() < half_volume_radius)
            .count();

        let ratio = inside as f32 / 4000.0;
        assert!((ratio - 0.5).abs() < 0.05, "内半球占了 {ratio:.3}");
    }

    #[test]
    fn triangle_samples_stay_within_their_barycentric_bounds() {
        let mut rng = rng();
        let triangle = Triangle2d::new(Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::new(0.0, 3.0));

        for _ in 0..500 {
            let p = triangle.sample_interior(&mut rng);
            // 这个三角形是 x/4 + y/3 ≤ 1 的那块。
            assert!(p.x >= -1e-5 && p.y >= -1e-5, "{p}");
            assert!(p.x / 4.0 + p.y / 3.0 <= 1.0 + 1e-5, "{p} 跑到斜边外了");
        }
    }

    #[test]
    fn sampling_is_reproducible_from_a_seed() {
        // 确定性是可测试性的前提：同一个种子必须给出同一串点。
        let circle = Circle::new(1.0);
        let first: Vec<Vec2> = {
            let mut r = Rng::new(42);
            (0..10).map(|_| circle.sample_interior(&mut r)).collect()
        };
        let second: Vec<Vec2> = {
            let mut r = Rng::new(42);
            (0..10).map(|_| circle.sample_interior(&mut r)).collect()
        };
        assert_eq!(first, second);
    }
}
