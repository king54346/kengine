//! 由线段拼出来的常用形状。
//!
//! 每个形状都在 CPU 上退化成线段推进 [`Gizmos`] 的缓冲。分段数是写死的常量：
//! 调试绘制不值得为「多远的球画几段」再引入一套 LOD 逻辑，一圈 24 段
//! 在实际视距下已经看不出棱角。

use crate::{Color, Gizmos};
use kmath::{Aabb, Mat4, Vec3};

/// 一整圈分多少段。
const RING_SEGMENTS: usize = 24;

/// 箭头尖端那四根倒刺占箭身长度的比例。
const ARROW_HEAD_RATIO: f32 = 0.15;

impl Gizmos {
    // ───────────────────────── 盒子 ─────────────────────────

    /// 轴对齐包围盒的十二条棱。空盒子什么都不画。
    pub fn aabb(&mut self, aabb: Aabb, color: Color) {
        if !self.enabled() || aabb.is_empty() {
            return;
        }
        self.box_corners(&aabb.corners(), color);
    }

    /// 有朝向的盒子：先在局部空间取半长，再整体过一遍矩阵。
    ///
    /// 碰撞体、相机、选中框都是这个形状——它们都有朝向，用 AABB 画会偏大。
    pub fn cuboid(&mut self, transform: Mat4, half_extents: Vec3, color: Color) {
        if !self.enabled() {
            return;
        }
        let local = Aabb::from_center_half_extents(Vec3::ZERO, half_extents).corners();
        let corners = local.map(|c| transform.transform_point3(c));
        self.box_corners(&corners, color);
    }

    /// 由 [`Aabb::corners`] 那个顺序的八个角画出十二条棱。
    fn box_corners(&mut self, c: &[Vec3; 8], color: Color) {
        // corners 的排列：低位是 x，其次 y，最高位是 z。
        // 于是「只有一位不同」的两个下标就是一条棱。
        const EDGES: [(usize, usize); 12] = [
            (0, 1),
            (2, 3),
            (4, 5),
            (6, 7), // 沿 x
            (0, 2),
            (1, 3),
            (4, 6),
            (5, 7), // 沿 y
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7), // 沿 z
        ];
        for (a, b) in EDGES {
            self.line(c[a], c[b], color);
        }
    }

    // ───────────────────────── 圆与球 ─────────────────────────

    /// 一个圆环，`normal` 决定它所在的平面。
    pub fn circle(&mut self, center: Vec3, normal: Vec3, radius: f32, color: Color) {
        if !self.enabled() {
            return;
        }
        let normal = normal.normalize_or_zero();
        if normal == Vec3::ZERO {
            return;
        }
        let (u, v) = normal.any_orthonormal_pair();
        self.ring(center, u * radius, v * radius, color);
    }

    /// 球体：三个正交的大圆。
    ///
    /// 只画三个圈而不是画满经纬线——多了就成一团白，反而看不出位置。
    pub fn sphere(&mut self, center: Vec3, radius: f32, color: Color) {
        if !self.enabled() {
            return;
        }
        self.ring(center, Vec3::X * radius, Vec3::Y * radius, color);
        self.ring(center, Vec3::Y * radius, Vec3::Z * radius, color);
        self.ring(center, Vec3::Z * radius, Vec3::X * radius, color);
    }

    /// 以 `u`、`v` 两个半轴张成的椭圆环。所有圆形都走这里。
    fn ring(&mut self, center: Vec3, u: Vec3, v: Vec3, color: Color) {
        let step = std::f32::consts::TAU / RING_SEGMENTS as f32;
        let mut previous = center + u;
        for i in 1..=RING_SEGMENTS {
            let angle = step * i as f32;
            let point = center + u * angle.cos() + v * angle.sin();
            self.line(previous, point, color);
            previous = point;
        }
    }

    /// 半圆弧，从 `u` 方向绕向 `v` 方向。胶囊的两个帽子用它。
    fn half_ring(&mut self, center: Vec3, u: Vec3, v: Vec3, color: Color) {
        let segments = RING_SEGMENTS / 2;
        let step = std::f32::consts::PI / segments as f32;
        let mut previous = center + u;
        for i in 1..=segments {
            let angle = step * i as f32;
            let point = center + u * angle.cos() + v * angle.sin();
            self.line(previous, point, color);
            previous = point;
        }
    }

    // ───────────────────────── 柱状体 ─────────────────────────

    /// 圆柱：两端的圆环加四根母线。
    pub fn cylinder(&mut self, from: Vec3, to: Vec3, radius: f32, color: Color) {
        if !self.enabled() {
            return;
        }
        let Some((u, v)) = self.axis_basis(from, to, radius) else {
            return;
        };

        self.ring(from, u, v, color);
        self.ring(to, u, v, color);
        for side in [u, -u, v, -v] {
            self.line(from + side, to + side, color);
        }
    }

    /// 圆锥：底面圆环加四根到锥尖的棱。
    pub fn cone(&mut self, base: Vec3, tip: Vec3, radius: f32, color: Color) {
        if !self.enabled() {
            return;
        }
        let Some((u, v)) = self.axis_basis(base, tip, radius) else {
            return;
        };

        self.ring(base, u, v, color);
        for side in [u, -u, v, -v] {
            self.line(base + side, tip, color);
        }
    }

    /// 胶囊：两个半球帽加中间的圆柱面。
    ///
    /// 这是角色控制器最常见的碰撞形状，物理调试绘制里出现频率最高。
    pub fn capsule(&mut self, from: Vec3, to: Vec3, radius: f32, color: Color) {
        if !self.enabled() {
            return;
        }
        let Some((u, v)) = self.axis_basis(from, to, radius) else {
            // 退化成球：两个端点重合时胶囊就是一个球。
            self.sphere(from, radius, color);
            return;
        };
        let axis = (to - from).normalize_or_zero() * radius;

        // 中段：两端的圆环 + 四根母线。
        self.ring(from, u, v, color);
        self.ring(to, u, v, color);
        for side in [u, -u, v, -v] {
            self.line(from + side, to + side, color);
        }

        // 两个帽子各画两条正交的半圆弧，勾出球面的轮廓。
        self.half_ring(from, u, -axis, color);
        self.half_ring(from, v, -axis, color);
        self.half_ring(to, u, axis, color);
        self.half_ring(to, v, axis, color);
    }

    /// 给一条从 `from` 到 `to` 的轴配一组垂直于它、长度为 `radius` 的基。
    ///
    /// 两点重合时返回 `None`——此时轴向无从谈起，调用方各自决定怎么退化。
    fn axis_basis(&self, from: Vec3, to: Vec3, radius: f32) -> Option<(Vec3, Vec3)> {
        let axis = to - from;
        if axis.length_squared() < 1e-12 {
            return None;
        }
        let (u, v) = axis.normalize().any_orthonormal_pair();
        Some((u * radius, v * radius))
    }

    // ───────────────────────── 指示物 ─────────────────────────

    /// 带箭头的线段。看方向类的量（速度、法线、受力）用它。
    pub fn arrow(&mut self, from: Vec3, to: Vec3, color: Color) {
        if !self.enabled() {
            return;
        }
        self.line(from, to, color);

        let Some((u, v)) = self.axis_basis(from, to, 1.0) else {
            return;
        };
        let axis = to - from;
        let head = axis.length() * ARROW_HEAD_RATIO;
        let back = to - axis.normalize() * head;
        for side in [u, -u, v, -v] {
            self.line(to, back + side * head * 0.5, color);
        }
    }

    /// 坐标系的三根轴：X 红、Y 绿、Z 蓝。
    ///
    /// 颜色是行业惯例（Blender、Unity、Godot 都一样），不做成参数——
    /// 一眼认出哪根是哪根，比自定义颜色值钱。
    pub fn transform(&mut self, matrix: Mat4, length: f32) {
        if !self.enabled() {
            return;
        }
        let origin = matrix.w_axis.truncate();
        // 用矩阵的列而不是 transform_vector3：列本身就是被变换后的轴，
        // 而且保留了缩放，能一眼看出物体被拉扁了。
        self.line(
            origin,
            origin + matrix.x_axis.truncate() * length,
            Color::RED,
        );
        self.line(
            origin,
            origin + matrix.y_axis.truncate() * length,
            Color::GREEN,
        );
        self.line(
            origin,
            origin + matrix.z_axis.truncate() * length,
            Color::rgb(0.0, 0.3, 1.0),
        );
    }

    /// 一片网格地板，用来提供空间参照。
    ///
    /// `half_count` 是从中心往外的格子数，总共 `2 * half_count` 格见方。
    pub fn grid(&mut self, center: Vec3, cell_size: f32, half_count: u32, color: Color) {
        if !self.enabled() || cell_size <= 0.0 {
            return;
        }
        let extent = cell_size * half_count as f32;
        let count = half_count as i32;

        for i in -count..=count {
            let offset = i as f32 * cell_size;
            // 中轴画亮一点，方便找原点在哪。
            let line_color = if i == 0 { color.scaled(2.0) } else { color };

            self.line(
                center + Vec3::new(offset, 0.0, -extent),
                center + Vec3::new(offset, 0.0, extent),
                line_color,
            );
            self.line(
                center + Vec3::new(-extent, 0.0, offset),
                center + Vec3::new(extent, 0.0, offset),
                line_color,
            );
        }
    }

    /// 相机视锥，由它的 view-projection 矩阵反解出来。
    ///
    /// 把裁剪空间的立方体八个角逆变换回世界空间即可。这是检查剔除对不对
    /// 最直接的手段：让相机 A 停住，用相机 B 从外面看 A 的视锥。
    pub fn frustum(&mut self, view_projection: Mat4, color: Color) {
        if !self.enabled() {
            return;
        }
        let inverse = view_projection.inverse();
        // wgpu 的深度范围是 [0, 1]，近平面 z = 0、远平面 z = 1；
        // 顺序对齐 `Aabb::corners`，好复用同一张棱表。
        let mut corners = [Vec3::ZERO; 8];
        for (i, corner) in corners.iter_mut().enumerate() {
            let x = if i & 1 == 0 { -1.0 } else { 1.0 };
            let y = if i & 2 == 0 { -1.0 } else { 1.0 };
            let z = if i & 4 == 0 { 0.0 } else { 1.0 };
            let clip = inverse * kmath::Vec4::new(x, y, z, 1.0);
            let point = clip.truncate() / clip.w;
            // 矩阵不可逆（`inverse()` 会给出 NaN）或者退化到 w≈0 时，
            // 角点会跑到无穷远，画出来是几条贯穿整个世界的线。
            // 判 `is_finite` 而不是判 `w`：NaN 的比较永远为假，用 w 拦不住。
            if !point.is_finite() {
                return;
            }
            *corner = point;
        }
        self.box_corners(&corners, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;
    use kmath::{Quat, Vec3};

    fn on() -> Gizmos {
        let mut g = Gizmos::new();
        g.set_enabled(true);
        g
    }

    fn points(gizmos: &Gizmos) -> Vec<Vec3> {
        gizmos
            .vertices(Layer::Depth)
            .iter()
            .map(|v| Vec3::from_array(v.position))
            .collect()
    }

    fn all_finite(gizmos: &Gizmos) -> bool {
        gizmos
            .vertices(Layer::Depth)
            .iter()
            .all(|v| v.position.iter().all(|c| c.is_finite()))
    }

    #[test]
    fn a_box_has_twelve_edges() {
        let mut gizmos = on();
        gizmos.aabb(Aabb::new(Vec3::ZERO, Vec3::ONE), Color::WHITE);
        assert_eq!(gizmos.vertices(Layer::Depth).len(), 24);
    }

    #[test]
    fn box_edges_all_have_unit_length_on_a_unit_cube() {
        // 十二条棱如果配错了角点，就会连出对角线——长度立刻露馅。
        let mut gizmos = on();
        gizmos.aabb(Aabb::new(Vec3::ZERO, Vec3::ONE), Color::WHITE);

        for edge in points(&gizmos).chunks(2) {
            let length = (edge[1] - edge[0]).length();
            assert!((length - 1.0).abs() < 1e-5, "棱长 {length}，应当是 1");
        }
    }

    #[test]
    fn an_empty_aabb_draws_nothing() {
        // 空盒子的 min 是 +∞，画出来就是满屏 NaN。
        let mut gizmos = on();
        gizmos.aabb(Aabb::EMPTY, Color::WHITE);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn a_rotated_cuboid_stays_the_same_size() {
        // 有朝向的盒子不该像 AABB 那样越转越大。
        let mut gizmos = on();
        let rotation = Mat4::from_quat(Quat::from_rotation_y(0.7));
        gizmos.cuboid(rotation, Vec3::splat(0.5), Color::WHITE);

        for edge in points(&gizmos).chunks(2) {
            assert!((edge[1] - edge[0]).length() - 1.0 < 1e-5);
        }
    }

    #[test]
    fn a_sphere_is_three_closed_rings() {
        let mut gizmos = on();
        gizmos.sphere(Vec3::ZERO, 2.0, Color::CYAN);

        let vertices = points(&gizmos);
        assert_eq!(vertices.len(), 3 * RING_SEGMENTS * 2);
        // 每个点都该落在球面上，不然就是基向量配错了。
        assert!(vertices.iter().all(|p| (p.length() - 2.0).abs() < 1e-4));
    }

    #[test]
    fn rings_close_up() {
        // 环必须首尾相接；差一段就会留个缺口。
        let mut gizmos = on();
        gizmos.circle(Vec3::ZERO, Vec3::Y, 1.0, Color::WHITE);

        let vertices = points(&gizmos);
        let first = vertices[0];
        let last = vertices[vertices.len() - 1];
        assert!((first - last).length() < 1e-5, "环没有闭合");
    }

    #[test]
    fn a_circle_with_no_normal_draws_nothing() {
        // 零法向量归一化会得到 NaN。
        let mut gizmos = on();
        gizmos.circle(Vec3::ZERO, Vec3::ZERO, 1.0, Color::WHITE);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn a_degenerate_capsule_falls_back_to_a_sphere() {
        // 两端重合时轴向无从谈起，但胶囊本身仍然是个合法的球。
        let mut gizmos = on();
        gizmos.capsule(Vec3::Y, Vec3::Y, 1.0, Color::WHITE);

        assert!(!gizmos.is_empty());
        assert!(all_finite(&gizmos));
        assert!(
            points(&gizmos)
                .iter()
                .all(|p| ((*p - Vec3::Y).length() - 1.0).abs() < 1e-4)
        );
    }

    #[test]
    fn degenerate_cylinders_and_cones_draw_nothing() {
        let mut gizmos = on();
        gizmos.cylinder(Vec3::ZERO, Vec3::ZERO, 1.0, Color::WHITE);
        gizmos.cone(Vec3::ZERO, Vec3::ZERO, 1.0, Color::WHITE);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn a_cone_ends_at_its_tip() {
        let mut gizmos = on();
        let tip = Vec3::new(0.0, 3.0, 0.0);
        gizmos.cone(Vec3::ZERO, tip, 1.0, Color::WHITE);

        let touching_tip = points(&gizmos)
            .iter()
            .filter(|p| (**p - tip).length() < 1e-5)
            .count();
        assert_eq!(touching_tip, 4, "四根棱都该收在锥尖");
    }

    #[test]
    fn an_arrow_has_a_head() {
        let mut gizmos = on();
        gizmos.arrow(Vec3::ZERO, Vec3::X * 4.0, Color::YELLOW);
        // 箭身一段 + 四根倒刺。
        assert_eq!(gizmos.vertices(Layer::Depth).len(), 5 * 2);
    }

    #[test]
    fn a_zero_length_arrow_is_just_a_point() {
        let mut gizmos = on();
        gizmos.arrow(Vec3::ONE, Vec3::ONE, Color::YELLOW);
        assert_eq!(gizmos.vertices(Layer::Depth).len(), 2);
        assert!(all_finite(&gizmos));
    }

    #[test]
    fn transform_axes_follow_the_matrix_columns() {
        let mut gizmos = on();
        let matrix = Mat4::from_rotation_translation(
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            Vec3::new(5.0, 0.0, 0.0),
        );
        gizmos.transform(matrix, 1.0);

        let vertices = points(&gizmos);
        // 绕 Z 转 90°：局部 X 轴指向世界 +Y。
        assert!((vertices[1] - Vec3::new(5.0, 1.0, 0.0)).length() < 1e-5);
        // 起点都是矩阵的平移列。
        assert!((vertices[0] - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn a_grid_has_the_right_line_count() {
        let mut gizmos = on();
        gizmos.grid(Vec3::ZERO, 1.0, 2, Color::GRAY);
        // 每个方向 2*2+1 = 5 条，两个方向共 10 条线。
        assert_eq!(gizmos.vertices(Layer::Depth).len(), 10 * 2);
    }

    #[test]
    fn a_grid_with_no_cell_size_draws_nothing() {
        let mut gizmos = on();
        gizmos.grid(Vec3::ZERO, 0.0, 4, Color::GRAY);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn a_frustum_encloses_what_the_camera_sees() {
        // 用一个已知的投影反解视锥，近平面四个角应当正好在 z = -near 上。
        let near = 0.1;
        let far = 100.0;
        let projection = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1.0, near, far);

        let mut gizmos = on();
        gizmos.frustum(projection, Color::MAGENTA);

        let vertices = points(&gizmos);
        assert!(all_finite(&gizmos));

        let nearest = vertices.iter().map(|p| -p.z).fold(f32::INFINITY, f32::min);
        let farthest = vertices
            .iter()
            .map(|p| -p.z)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((nearest - near).abs() < 1e-3, "近平面在 {nearest}");
        assert!((farthest - far).abs() < 1e-1, "远平面在 {farthest}");
    }

    #[test]
    fn a_degenerate_projection_draws_nothing() {
        // 不可逆的矩阵会解出无穷大的角点，画出来是几条贯穿世界的线。
        let mut gizmos = on();
        gizmos.frustum(Mat4::ZERO, Color::MAGENTA);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn every_shape_respects_the_current_layer() {
        let mut gizmos = on();
        gizmos.on_top(|g| {
            g.sphere(Vec3::ZERO, 1.0, Color::RED);
            g.aabb(Aabb::new(Vec3::ZERO, Vec3::ONE), Color::RED);
            g.arrow(Vec3::ZERO, Vec3::X, Color::RED);
            g.capsule(Vec3::ZERO, Vec3::Y, 0.5, Color::RED);
            g.grid(Vec3::ZERO, 1.0, 1, Color::RED);
        });
        assert!(gizmos.vertices(Layer::Depth).is_empty());
        assert!(!gizmos.vertices(Layer::Overlay).is_empty());
    }
}
