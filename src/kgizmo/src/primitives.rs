//! 把 [`kmath`] 的几何图元画成线框。
//!
//! [`kmath::Sphere`] 那些类型只描述形状，不知道自己该怎么显示；这里补上
//! 那一层。分成两个 trait 而不是一个，是因为二维和三维的线框结构不同：
//!
//! - **二维**图元的线框就是若干条**平面轮廓**（多数只有一条），所以
//!   [`Gizmo2d`] 只交出点串，位姿变换与推顶点的活由这里统一做——十一种
//!   图元共用同一份绘制代码。
//! - **三维**图元的线框是一把散落在空间里的环与线（一个球是三个正交的圈），
//!   没有「一条轮廓」可言。而且 [`Gizmos`] 上本来就有 `sphere` / `cylinder`
//!   这些画好了的方法，让 [`Gizmo3d`] 直接调它们，比把那些代码按数据结构
//!   重写一遍划算。

use crate::{Color, Gizmos};
use kmath::{
    Annulus, Arc2d, Capsule2d, Capsule3d, Circle, CircularSector, CircularSegment, Cone,
    ConicalFrustum, Cuboid, Cylinder, Ellipse, Mat4, Rectangle, RegularPolygon, Segment2d,
    Segment3d, Sphere, Tetrahedron, Torus, Triangle2d, Triangle3d, Vec2, Vec3,
};

/// 圆弧类形状分多少段。和 `shapes.rs` 里的环用同一个数量级——
/// 调试绘制不值得为「多远画几段」再引入一套 LOD。
const ARC_SEGMENTS: usize = 32;

/// 一条轮廓：一串点，加上「首尾要不要连起来」。
pub type Outline = (Vec<Vec2>, bool);

/// 能画成平面线框的二维图元。
pub trait Gizmo2d {
    /// 局部空间（XY 平面、以原点为中心）的轮廓。
    ///
    /// 多数图元只有一条；[`Annulus`] 有两条（内外两个圈）。
    fn outlines(&self) -> Vec<Outline>;
}

/// 能画成三维线框的图元。
pub trait Gizmo3d {
    /// 按给定位姿把自己画进 `gizmos`。
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color);
}

impl Gizmos {
    /// 画一个二维图元，摆在 `translation` 处、绕原点转 `angle` 弧度。
    ///
    /// 画在 **XY 平面**（z = 0）上。二维例子配一台正交相机正对这个平面，
    /// 出来就是一张二维图——kengine 不为此单独做一套二维绘制。
    /// `?Sized` 是为了能直接传 `&dyn Gizmo2d`：把一堆不同图元放进一个
    /// 列表里逐个画，是这套接口最常见的用法。
    pub fn primitive_2d(
        &mut self,
        shape: &(impl Gizmo2d + ?Sized),
        translation: Vec2,
        angle: f32,
        color: Color,
    ) {
        if !self.enabled() {
            return;
        }
        let (sin, cos) = angle.sin_cos();
        for (points, closed) in shape.outlines() {
            // 少于两个点连不成线；一条轮廓退化成一个点时直接跳过，
            // 否则会往缓冲里推一条零长度的线段。
            if points.len() < 2 {
                continue;
            }
            let placed: Vec<Vec3> = points
                .into_iter()
                .map(|p| {
                    let rotated = Vec2::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos);
                    (translation + rotated).extend(0.0)
                })
                .collect();

            if closed {
                self.polyline_closed(&placed, color);
            } else {
                self.polyline(&placed, color);
            }
        }
    }

    /// 画一个三维图元。
    pub fn primitive_3d(&mut self, shape: &(impl Gizmo3d + ?Sized), transform: Mat4, color: Color) {
        if !self.enabled() {
            return;
        }
        shape.draw_gizmo(self, transform, color);
    }
}

/// 圆弧上的采样点。`from`、`to` 是起止角（弧度）。
fn arc_points(radius: f32, from: f32, to: f32, segments: usize) -> Vec<Vec2> {
    (0..=segments)
        .map(|i| {
            let angle = from + (to - from) * i as f32 / segments as f32;
            Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

// ── 二维 ──────────────────────────────────────────────────────────────────────

impl Gizmo2d for Circle {
    fn outlines(&self) -> Vec<Outline> {
        // 首尾都取到会得到重合的两点，闭合折线自己会连回去，所以少取一段。
        let mut points = arc_points(self.radius, 0.0, std::f32::consts::TAU, ARC_SEGMENTS);
        points.pop();
        vec![(points, true)]
    }
}

impl Gizmo2d for Rectangle {
    fn outlines(&self) -> Vec<Outline> {
        let h = self.half_size;
        vec![(
            vec![
                Vec2::new(-h.x, -h.y),
                Vec2::new(h.x, -h.y),
                Vec2::new(h.x, h.y),
                Vec2::new(-h.x, h.y),
            ],
            true,
        )]
    }
}

impl Gizmo2d for Ellipse {
    fn outlines(&self) -> Vec<Outline> {
        let points = (0..ARC_SEGMENTS)
            .map(|i| {
                let angle = i as f32 / ARC_SEGMENTS as f32 * std::f32::consts::TAU;
                Vec2::new(
                    angle.cos() * self.half_size.x,
                    angle.sin() * self.half_size.y,
                )
            })
            .collect();
        vec![(points, true)]
    }
}

impl Gizmo2d for Annulus {
    fn outlines(&self) -> Vec<Outline> {
        // 两条独立的圈——这正是 `outlines` 返回一个 `Vec` 而不是单条的理由。
        let mut inner = Circle::new(self.inner_radius).outlines();
        inner.extend(Circle::new(self.outer_radius).outlines());
        inner
    }
}

impl Gizmo2d for Triangle2d {
    fn outlines(&self) -> Vec<Outline> {
        vec![(self.vertices.to_vec(), true)]
    }
}

impl Gizmo2d for RegularPolygon {
    fn outlines(&self) -> Vec<Outline> {
        vec![(self.vertices(), true)]
    }
}

impl Gizmo2d for Segment2d {
    fn outlines(&self) -> Vec<Outline> {
        vec![(self.endpoints.to_vec(), false)]
    }
}

impl Gizmo2d for Capsule2d {
    fn outlines(&self) -> Vec<Outline> {
        // 一圈走下来：右直边 → 上半圆 → 左直边 → 下半圆。
        // 拆成「两个半圆 + 两条线」分别画也行，但那样是四条轮廓，
        // 接缝处的线宽会叠起来。
        let half = ARC_SEGMENTS / 2;
        let (r, l) = (self.radius, self.half_length);

        let mut points = Vec::with_capacity(ARC_SEGMENTS + 2);
        for point in arc_points(r, 0.0, std::f32::consts::PI, half) {
            points.push(point + Vec2::new(0.0, l));
        }
        for point in arc_points(r, std::f32::consts::PI, std::f32::consts::TAU, half) {
            points.push(point - Vec2::new(0.0, l));
        }
        vec![(points, true)]
    }
}

impl Gizmo2d for Arc2d {
    fn outlines(&self) -> Vec<Outline> {
        // 以 +Y 为中线向两边各张 half_angle。
        let mid = std::f32::consts::FRAC_PI_2;
        vec![(
            arc_points(
                self.radius,
                mid - self.half_angle,
                mid + self.half_angle,
                ARC_SEGMENTS,
            ),
            false,
        )]
    }
}

impl Gizmo2d for CircularSector {
    fn outlines(&self) -> Vec<Outline> {
        // 弧 + 回到圆心，闭合起来就是一块披萨。
        let (mut points, _) = self.arc.outlines().remove(0);
        points.push(Vec2::ZERO);
        vec![(points, true)]
    }
}

impl Gizmo2d for CircularSegment {
    fn outlines(&self) -> Vec<Outline> {
        // 弧 + 弦。闭合折线自己会把两端连起来，那条线就是弦。
        let (points, _) = self.arc.outlines().remove(0);
        vec![(points, true)]
    }
}

// ── 三维 ──────────────────────────────────────────────────────────────────────

/// 取变换的位置。
fn origin_of(transform: Mat4) -> Vec3 {
    transform.w_axis.truncate()
}

/// 把一个局部点搬到世界。
fn place(transform: Mat4, local: Vec3) -> Vec3 {
    transform.transform_point3(local)
}

impl Gizmo3d for Sphere {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        // 球转多少度都一样，位置之外的部分用不上。
        gizmos.sphere(origin_of(transform), self.radius, color);
    }
}

impl Gizmo3d for Cuboid {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        gizmos.cuboid(transform, self.half_size, color);
    }
}

impl Gizmo3d for Cylinder {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        gizmos.cylinder(
            place(transform, Vec3::NEG_Y * self.half_height),
            place(transform, Vec3::Y * self.half_height),
            self.radius,
            color,
        );
    }
}

impl Gizmo3d for Capsule3d {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        gizmos.capsule(
            place(transform, Vec3::NEG_Y * self.half_length),
            place(transform, Vec3::Y * self.half_length),
            self.radius,
            color,
        );
    }
}

impl Gizmo3d for Cone {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        gizmos.cone(
            place(transform, Vec3::NEG_Y * self.half_height),
            place(transform, Vec3::Y * self.half_height),
            self.radius,
            color,
        );
    }
}

impl Gizmo3d for ConicalFrustum {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        let bottom = place(transform, Vec3::NEG_Y * self.half_height);
        let top = place(transform, Vec3::Y * self.half_height);
        let axis = (top - bottom).normalize_or_zero();
        if axis == Vec3::ZERO {
            return;
        }
        let (u, v) = axis.any_orthonormal_pair();

        gizmos.circle(bottom, axis, self.radius_bottom, color);
        gizmos.circle(top, axis, self.radius_top, color);

        // 四条母线。多画几条并不会让形状更清楚，反而糊成一片。
        for i in 0..4 {
            let angle = i as f32 / 4.0 * std::f32::consts::TAU;
            let dir = u * angle.cos() + v * angle.sin();
            gizmos.line(
                bottom + dir * self.radius_bottom,
                top + dir * self.radius_top,
                color,
            );
        }
    }
}

impl Gizmo3d for Torus {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        let center = origin_of(transform);
        let axis = transform.y_axis.truncate().normalize_or_zero();
        if axis == Vec3::ZERO {
            return;
        }
        let (u, v) = axis.any_orthonormal_pair();

        // 内外两圈勾出整体轮廓。
        gizmos.circle(center, axis, self.major_radius + self.minor_radius, color);
        gizmos.circle(center, axis, self.major_radius - self.minor_radius, color);

        // 沿主圆均匀取几处画管子的截面圈，管有多粗一眼就看出来了。
        const SECTIONS: usize = 8;
        for i in 0..SECTIONS {
            let angle = i as f32 / SECTIONS as f32 * std::f32::consts::TAU;
            let outward = u * angle.cos() + v * angle.sin();
            gizmos.circle(
                center + outward * self.major_radius,
                // 截面圈的法线是「绕主圆走的方向」，也就是切向。
                outward.cross(axis),
                self.minor_radius,
                color,
            );
        }
    }
}

impl Gizmo3d for Triangle3d {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        let placed: Vec<Vec3> = self.vertices.iter().map(|v| place(transform, *v)).collect();
        gizmos.polyline_closed(&placed, color);
    }
}

impl Gizmo3d for Tetrahedron {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        let v: Vec<Vec3> = self.vertices.iter().map(|p| place(transform, *p)).collect();
        // 四个顶点两两相连，六条边。
        for (a, b) in [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
            gizmos.line(v[a], v[b], color);
        }
    }
}

impl Gizmo3d for Segment3d {
    fn draw_gizmo(&self, gizmos: &mut Gizmos, transform: Mat4, color: Color) {
        gizmos.line(
            place(transform, self.endpoints[0]),
            place(transform, self.endpoints[1]),
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gizmos() -> Gizmos {
        let mut gizmos = Gizmos::new();
        gizmos.set_enabled(true);
        gizmos
    }

    #[test]
    fn a_circle_outline_does_not_repeat_its_first_point() {
        // 闭合折线自己会连回起点。首尾都取的话最后一段是零长度，
        // 画出来看不见，但白占顶点。
        let outlines = Circle::new(1.0).outlines();
        assert_eq!(outlines.len(), 1);

        let (points, closed) = &outlines[0];
        assert!(closed);
        assert!(
            points[0].distance(points[points.len() - 1]) > 1e-3,
            "首尾重合了"
        );
    }

    #[test]
    fn an_annulus_has_two_rings() {
        let outlines = Annulus::new(1.0, 2.0).outlines();
        assert_eq!(outlines.len(), 2, "圆环是内外两条轮廓");
    }

    #[test]
    fn a_sector_closes_through_the_centre() {
        // 扇形要回到圆心才是一块披萨，否则画出来只是一段弧。
        let (points, closed) = CircularSector::new(2.0, 1.0).outlines().remove(0);
        assert!(closed);
        assert_eq!(points[points.len() - 1], Vec2::ZERO);
    }

    #[test]
    fn a_segment_does_not_pass_through_the_centre() {
        // 弓形和扇形只差这一点：它的封口是弦，不经过圆心。
        let (points, _) = CircularSegment::new(2.0, 1.0).outlines().remove(0);
        assert!(points.iter().all(|p| p.length() > 1.0), "不该有点落在圆心");
    }

    #[test]
    fn primitives_land_where_they_are_placed() {
        let mut gizmos = gizmos();
        gizmos.primitive_2d(
            &Rectangle::square(2.0),
            Vec2::new(10.0, 5.0),
            0.0,
            Color::WHITE,
        );

        let vertices = gizmos.vertices(gizmos.layer());
        assert!(!vertices.is_empty());
        // 所有点都该落在那个矩形的四角附近，而不是原点。
        for vertex in vertices {
            let p = Vec2::new(vertex.position[0], vertex.position[1]);
            assert!(
                (p - Vec2::new(10.0, 5.0)).length() < 2.0,
                "{p} 没跟着位移走"
            );
        }
    }

    #[test]
    fn rotating_a_primitive_actually_turns_it() {
        let mut straight = gizmos();
        straight.primitive_2d(&Rectangle::new(4.0, 0.5), Vec2::ZERO, 0.0, Color::WHITE);
        let wide = straight
            .vertices(straight.layer())
            .iter()
            .map(|v| v.position[0].abs())
            .fold(0.0_f32, f32::max);

        let mut turned = gizmos();
        turned.primitive_2d(
            &Rectangle::new(4.0, 0.5),
            Vec2::ZERO,
            std::f32::consts::FRAC_PI_2,
            Color::WHITE,
        );
        let tall = turned
            .vertices(turned.layer())
            .iter()
            .map(|v| v.position[1].abs())
            .fold(0.0_f32, f32::max);

        assert!((wide - 2.0).abs() < 1e-4, "横着的时候宽 4");
        assert!((tall - 2.0).abs() < 1e-4, "转 90° 之后该变成高 4");
    }

    #[test]
    fn a_disabled_gizmos_draws_nothing() {
        let mut gizmos = Gizmos::new();
        gizmos.set_enabled(false);
        gizmos.primitive_2d(&Circle::new(1.0), Vec2::ZERO, 0.0, Color::WHITE);
        gizmos.primitive_3d(&Sphere::new(1.0), Mat4::IDENTITY, Color::WHITE);

        assert!(gizmos.is_empty());
    }

    #[test]
    fn three_dimensional_primitives_produce_lines() {
        for name in ["sphere", "torus", "tetra", "frustum"] {
            let mut gizmos = gizmos();
            match name {
                "sphere" => gizmos.primitive_3d(&Sphere::new(1.0), Mat4::IDENTITY, Color::WHITE),
                "torus" => gizmos.primitive_3d(&Torus::new(2.0, 0.5), Mat4::IDENTITY, Color::WHITE),
                "tetra" => gizmos.primitive_3d(
                    &Tetrahedron::new(Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z),
                    Mat4::IDENTITY,
                    Color::WHITE,
                ),
                _ => gizmos.primitive_3d(
                    &ConicalFrustum::new(1.0, 0.5, 2.0),
                    Mat4::IDENTITY,
                    Color::WHITE,
                ),
            }
            assert!(!gizmos.is_empty(), "{name} 什么都没画出来");
        }
    }

    #[test]
    fn a_degenerate_frustum_axis_does_not_panic() {
        // 高度为 0 时两个底面重合，轴向退化成零向量。
        let mut gizmos = gizmos();
        gizmos.primitive_3d(
            &ConicalFrustum::new(1.0, 0.5, 0.0),
            Mat4::IDENTITY,
            Color::WHITE,
        );
        // 画不出来是对的，崩了才不对。
    }
}
