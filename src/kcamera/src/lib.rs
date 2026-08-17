//! kcamera —— 相机与可见性剔除。
//!
//! [`Camera`] 只描述投影参数，位姿由所在的场景节点提供。
//! [`Frustum`] 从「视图投影矩阵」提取六个裁剪面，用于剔除看不见的物体。
//!
//! ```
//! use kcamera::prelude::*;
//! use kmath::{Aabb, Mat4, Vec3};
//!
//! let camera = Camera::default();
//! // 相机位于 +Z，朝原点看。
//! let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
//! let frustum = Frustum::from_view_projection(camera.projection_matrix(16.0 / 9.0) * view);
//!
//! // 原点处的小盒子在视野内，远处的不在。
//! assert!(frustum.intersects(&Aabb::new(-Vec3::ONE, Vec3::ONE)));
//! assert!(!frustum.intersects(&Aabb::new(
//!     Vec3::new(1000.0, 0.0, 0.0),
//!     Vec3::new(1001.0, 1.0, 1.0),
//! )));
//! ```

#![warn(missing_docs)]

use kmath::{Aabb, Intersection, Mat4, Plane, Vec3, Vec4};

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{Camera, Frustum, Projection};
    pub use kmath::Intersection;
}

/// 投影方式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// 透视投影，有近大远小效果。
    Perspective {
        /// 垂直视场角（角度制）。
        fov_y_degrees: f32,
    },
    /// 正交投影，常用于 2D 与等距视角。
    Orthographic {
        /// 垂直方向的可视高度，宽度按宽高比推导。
        height: f32,
    },
}

impl Default for Projection {
    fn default() -> Self {
        Self::Perspective {
            fov_y_degrees: 45.0,
        }
    }
}

/// 一台相机。位姿来自所在节点的世界变换，这里只描述投影。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// 投影方式。
    pub projection: Projection,
    /// 近裁剪面。
    pub z_near: f32,
    /// 远裁剪面。
    pub z_far: f32,
    /// 是否启用。渲染器使用场景中第一个启用的相机。
    pub enabled: bool,
    /// 是否对本相机启用视锥剔除。
    pub frustum_culling: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            projection: Projection::default(),
            z_near: 0.1,
            z_far: 1000.0,
            enabled: true,
            frustum_culling: true,
        }
    }
}

impl Camera {
    /// 创建一台透视相机。
    pub fn perspective(fov_y_degrees: f32) -> Self {
        Self {
            projection: Projection::Perspective { fov_y_degrees },
            ..Default::default()
        }
    }

    /// 创建一台正交相机。
    pub fn orthographic(height: f32) -> Self {
        Self {
            projection: Projection::Orthographic { height },
            ..Default::default()
        }
    }

    /// 按给定宽高比计算投影矩阵。
    ///
    /// 采用右手坐标系 + `[0, 1]` 深度范围，与 wgpu 的 NDC 约定一致。
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        // 窗口最小化时宽高比可能是 0 或 NaN，兜底避免矩阵退化成全 NaN。
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };

        match self.projection {
            Projection::Perspective { fov_y_degrees } => Mat4::perspective_rh(
                fov_y_degrees
                    .to_radians()
                    .clamp(1e-3, std::f32::consts::PI - 1e-3),
                aspect,
                self.z_near,
                self.z_far,
            ),
            Projection::Orthographic { height } => {
                let half_height = (height * 0.5).max(f32::EPSILON);
                let half_width = half_height * aspect;
                Mat4::orthographic_rh(
                    -half_width,
                    half_width,
                    -half_height,
                    half_height,
                    self.z_near,
                    self.z_far,
                )
            }
        }
    }

    /// 视场角（仅透视相机有意义）。
    pub fn fov_y_degrees(&self) -> Option<f32> {
        match self.projection {
            Projection::Perspective { fov_y_degrees } => Some(fov_y_degrees),
            Projection::Orthographic { .. } => None,
        }
    }
}

/// 视锥的六个面。法线一律指向视锥内部，因此「在所有面的正侧」即为可见。
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    planes: [Plane; 6],
}

impl Frustum {
    /// 从视图投影矩阵提取六个裁剪面（Gribb–Hartmann 方法）。
    ///
    /// 原理：裁剪空间里可见区域满足 `-w <= x,y <= w` 与 `0 <= z <= w`
    /// （wgpu 的深度范围是 `[0, 1]`，与 OpenGL 的 `[-1, 1]` 不同，
    /// 所以近平面取的是 `row_z` 而非 `row_w + row_z`），
    /// 把这些不等式用矩阵的行表示出来，就是世界空间中的平面方程。
    pub fn from_view_projection(view_projection: Mat4) -> Self {
        // glam 的矩阵按列存储，转置后各轴即为原矩阵的行。
        let m = view_projection.transpose();
        let row_x = Vec4::from(m.x_axis);
        let row_y = Vec4::from(m.y_axis);
        let row_z = Vec4::from(m.z_axis);
        let row_w = Vec4::from(m.w_axis);

        Self {
            planes: [
                Plane::from_vec4(row_w + row_x), // 左：x >= -w
                Plane::from_vec4(row_w - row_x), // 右：x <= w
                Plane::from_vec4(row_w + row_y), // 下：y >= -w
                Plane::from_vec4(row_w - row_y), // 上：y <= w
                Plane::from_vec4(row_z),         // 近：z >= 0
                Plane::from_vec4(row_w - row_z), // 远：z <= w
            ],
        }
    }

    /// 六个裁剪面。
    pub fn planes(&self) -> &[Plane; 6] {
        &self.planes
    }

    /// 包围盒是否与视锥相交（即是否可能可见）。
    ///
    /// 判据：只要包围盒完全落在任意一个面的外侧，就一定不可见。
    /// 反过来不成立——斜角处的盒子可能被误判为可见，
    /// 但这对剔除是安全的：宁可多画，不可漏画。
    pub fn intersects(&self, aabb: &Aabb) -> bool {
        self.classify(aabb) != Intersection::Outside
    }

    /// 包围盒相对视锥的三态位置关系。
    ///
    /// 比 [`intersects`](Frustum::intersects) 多告诉一件事：盒子是否**完整**落在视锥内。
    /// 层次剔除靠它剪枝——BVH 的某个节点整个在视锥内时，
    /// 其下所有物体都可见，不必再逐个判定。
    pub fn classify(&self, aabb: &Aabb) -> Intersection {
        if aabb.is_empty() {
            return Intersection::Outside;
        }

        let center = aabb.center();
        let half = aabb.half_extents();
        let mut result = Intersection::Inside;

        for plane in &self.planes {
            // 盒子在该平面法线方向上的投影半径。
            let radius = half.x * plane.normal.x.abs()
                + half.y * plane.normal.y.abs()
                + half.z * plane.normal.z.abs();
            let distance = plane.distance_to(center);

            if distance + radius < 0.0 {
                // 整个盒子在这个面的外侧，后面的面不用看了。
                return Intersection::Outside;
            }
            if distance - radius < 0.0 {
                // 跨在这个面上：还不能判外，但也不再是「完全在内」。
                result = Intersection::Intersects;
            }
        }

        result
    }

    /// 点是否在视锥内。
    pub fn contains_point(&self, point: Vec3) -> bool {
        self.planes
            .iter()
            .all(|plane| plane.distance_to(point) >= 0.0)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// 相机位于 +Z 轴、朝原点看的视锥。
    fn default_frustum() -> Frustum {
        let camera = Camera::default();
        let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        Frustum::from_view_projection(camera.projection_matrix(1.0) * view)
    }

    #[test]
    fn object_at_origin_is_visible() {
        let frustum = default_frustum();

        assert!(frustum.contains_point(Vec3::ZERO));
        assert!(frustum.intersects(&Aabb::new(-Vec3::splat(0.5), Vec3::splat(0.5))));
    }

    #[test]
    fn object_behind_camera_is_culled() {
        let frustum = default_frustum();

        // 相机在 z=5 朝 -z 看，z=10 处的物体在背后。
        assert!(!frustum.contains_point(Vec3::new(0.0, 0.0, 10.0)));
        assert!(!frustum.intersects(&Aabb::new(
            Vec3::new(-1.0, -1.0, 9.0),
            Vec3::new(1.0, 1.0, 11.0),
        )));
    }

    #[test]
    fn object_far_to_the_side_is_culled() {
        let frustum = default_frustum();

        assert!(!frustum.intersects(&Aabb::new(
            Vec3::new(1000.0, 0.0, 0.0),
            Vec3::new(1001.0, 1.0, 1.0),
        )));
    }

    #[test]
    fn object_beyond_far_plane_is_culled() {
        let frustum = default_frustum();

        // 默认远裁剪面是 1000。
        assert!(!frustum.contains_point(Vec3::new(0.0, 0.0, -2000.0)));
    }

    #[test]
    fn huge_box_containing_camera_is_visible() {
        let frustum = default_frustum();

        // 包住整个视锥的大盒子必须判为可见，否则场景会整个消失。
        assert!(frustum.intersects(&Aabb::new(Vec3::splat(-500.0), Vec3::splat(500.0))));
    }

    #[test]
    fn empty_aabb_is_never_visible() {
        assert!(!default_frustum().intersects(&Aabb::EMPTY));
        assert_eq!(default_frustum().classify(&Aabb::EMPTY), Intersection::Outside);
    }

    #[test]
    fn classify_separates_inside_from_straddling() {
        let frustum = default_frustum();

        // 原点处的小盒完整落在视锥里。
        assert_eq!(
            frustum.classify(&Aabb::new(-Vec3::splat(0.1), Vec3::splat(0.1))),
            Intersection::Inside
        );
        // 包住整个视锥的大盒必然跨在每个面上。
        assert_eq!(
            frustum.classify(&Aabb::new(Vec3::splat(-500.0), Vec3::splat(500.0))),
            Intersection::Intersects
        );
        // 远处的盒子完全在外。
        assert_eq!(
            frustum.classify(&Aabb::new(
                Vec3::new(1000.0, 0.0, 0.0),
                Vec3::new(1001.0, 1.0, 1.0)
            )),
            Intersection::Outside
        );
    }

    #[test]
    fn classify_agrees_with_intersects() {
        let frustum = default_frustum();

        // 两个判定必须同源，否则层次剔除与逐个剔除会给出不同的可见集。
        for x in -6..=6 {
            for z in -6..=6 {
                let aabb = Aabb::from_center_half_extents(
                    Vec3::new(x as f32, 0.0, z as f32),
                    Vec3::splat(0.5),
                );
                assert_eq!(
                    frustum.intersects(&aabb),
                    frustum.classify(&aabb) != Intersection::Outside
                );
            }
        }
    }

    #[test]
    fn plane_normals_point_inward() {
        let frustum = default_frustum();

        // 法线朝内时，视锥内部的点到所有面的距离都为正。
        for plane in frustum.planes() {
            assert!(plane.distance_to(Vec3::ZERO) > 0.0);
        }
    }

    #[test]
    fn orthographic_projection_is_size_preserving() {
        let camera = Camera::orthographic(10.0);
        let projection = camera.projection_matrix(1.0);

        // 正交投影下，远近两点的投影 x 坐标相同。
        let near = projection.project_point3(Vec3::new(1.0, 0.0, -1.0));
        let far = projection.project_point3(Vec3::new(1.0, 0.0, -100.0));

        assert!((near.x - far.x).abs() < 1e-5);
    }

    #[test]
    fn perspective_projection_shrinks_with_distance() {
        let camera = Camera::perspective(60.0);
        let projection = camera.projection_matrix(1.0);

        let near = projection.project_point3(Vec3::new(1.0, 0.0, -1.0));
        let far = projection.project_point3(Vec3::new(1.0, 0.0, -100.0));

        // 越远越靠近画面中心。
        assert!(far.x.abs() < near.x.abs());
    }

    #[test]
    fn degenerate_aspect_ratio_does_not_produce_nan() {
        let camera = Camera::default();

        // 窗口最小化时宽高比可能是 0 或 NaN。
        for aspect in [0.0, f32::NAN, -1.0] {
            let matrix = camera.projection_matrix(aspect);
            assert!(matrix.is_finite(), "宽高比 {aspect} produced 非有限矩阵");
        }
    }

    #[test]
    fn orthographic_frustum_culls_sideways_objects() {
        let camera = Camera::orthographic(4.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let frustum = Frustum::from_view_projection(camera.projection_matrix(1.0) * view);

        // 可视高度 4 表示 y ∈ [-2, 2]，y=10 处应被剔除。
        assert!(frustum.contains_point(Vec3::ZERO));
        assert!(!frustum.contains_point(Vec3::new(0.0, 10.0, 0.0)));
    }

    #[test]
    fn fov_only_applies_to_perspective() {
        assert_eq!(Camera::perspective(75.0).fov_y_degrees(), Some(75.0));
        assert_eq!(Camera::orthographic(5.0).fov_y_degrees(), None);
    }
}
