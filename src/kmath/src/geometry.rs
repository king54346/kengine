//! 几何基元：包围盒与平面。
//!
//! 放在 kmath 而非 kmesh，是因为相机剔除也要用它们——
//! 相机不应该为了一个包围盒去依赖网格。

use crate::{Mat4, Vec3, Vec4};

/// 轴对齐包围盒。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// 各轴最小值。
    pub min: Vec3,
    /// 各轴最大值。
    pub max: Vec3,
}

impl Default for Aabb {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Aabb {
    /// 空包围盒，与任何点求并都会得到那个点。
    pub const EMPTY: Self = Self {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    /// 用两个角点构造，自动取最小/最大。
    pub fn new(a: Vec3, b: Vec3) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    /// 由中心与各轴半长构造。
    pub fn from_center_half_extents(center: Vec3, half_extents: Vec3) -> Self {
        Self {
            min: center - half_extents,
            max: center + half_extents,
        }
    }

    /// 把一个点纳入包围盒。
    pub fn expand(&mut self, point: Vec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    /// 合并另一个包围盒。
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// 中心点。包围盒为空时返回原点。
    pub fn center(&self) -> Vec3 {
        if self.is_empty() {
            Vec3::ZERO
        } else {
            (self.min + self.max) * 0.5
        }
    }

    /// 各轴尺寸。包围盒为空时返回零。
    pub fn size(&self) -> Vec3 {
        if self.is_empty() {
            Vec3::ZERO
        } else {
            self.max - self.min
        }
    }

    /// 各轴半长。
    pub fn half_extents(&self) -> Vec3 {
        self.size() * 0.5
    }

    /// 是否没有纳入任何点。
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    /// 是否包含某个点。
    pub fn contains(&self, point: Vec3) -> bool {
        !self.is_empty()
            && point.cmpge(self.min).all()
            && point.cmple(self.max).all()
    }

    /// 八个角点。
    pub fn corners(&self) -> [Vec3; 8] {
        let (lo, hi) = (self.min, self.max);
        [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
        ]
    }

    /// 用矩阵变换包围盒，返回变换后仍然轴对齐的新包围盒。
    ///
    /// 变换八个角点再取包围——旋转后的结果会比原盒子大，这是 AABB 的固有代价。
    pub fn transform(&self, matrix: Mat4) -> Self {
        if self.is_empty() {
            return *self;
        }

        let mut result = Self::EMPTY;
        for corner in self.corners() {
            result.expand(matrix.transform_point3(corner));
        }
        result
    }
}

/// 一个平面，满足 `dot(normal, p) + d = 0`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    /// 平面法线，指向"正"半空间。
    pub normal: Vec3,
    /// 到原点的有符号距离。
    pub d: f32,
}

impl Plane {
    /// 从 `Vec4`（xyz 为法线，w 为距离）构造并归一化。
    ///
    /// 从投影矩阵提取出来的平面系数不是归一化的，必须先除以法线长度，
    /// 否则算出的距离不是真实距离。
    pub fn from_vec4(coefficients: Vec4) -> Self {
        let normal = coefficients.truncate();
        let length = normal.length();
        if length > f32::EPSILON {
            Self {
                normal: normal / length,
                d: coefficients.w / length,
            }
        } else {
            // 退化平面：给一个合法但不会误判的默认值。
            Self {
                normal: Vec3::Y,
                d: f32::NEG_INFINITY,
            }
        }
    }

    /// 点到平面的有符号距离，正值表示在法线指向的一侧。
    pub fn distance_to(&self, point: Vec3) -> f32 {
        self.normal.dot(point) + self.d
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn expand_grows_to_cover_points() {
        let mut aabb = Aabb::EMPTY;
        aabb.expand(Vec3::new(1.0, 2.0, 3.0));
        aabb.expand(Vec3::new(-1.0, 0.0, 5.0));

        assert_eq!(aabb.min, Vec3::new(-1.0, 0.0, 3.0));
        assert_eq!(aabb.max, Vec3::new(1.0, 2.0, 5.0));
    }

    #[test]
    fn empty_aabb_reports_zero_metrics() {
        let aabb = Aabb::EMPTY;

        assert!(aabb.is_empty());
        // 空盒子不能返回 inf/NaN，否则会污染后续计算。
        assert_eq!(aabb.center(), Vec3::ZERO);
        assert_eq!(aabb.size(), Vec3::ZERO);
        assert!(!aabb.contains(Vec3::ZERO));
    }

    #[test]
    fn union_with_empty_is_identity() {
        let a = Aabb::new(Vec3::ZERO, Vec3::ONE);

        assert_eq!(a.union(&Aabb::EMPTY), a);
        assert_eq!(Aabb::EMPTY.union(&a), a);
    }

    #[test]
    fn contains_checks_bounds_inclusively() {
        let aabb = Aabb::new(Vec3::ZERO, Vec3::ONE);

        assert!(aabb.contains(Vec3::splat(0.5)));
        assert!(aabb.contains(Vec3::ZERO));
        assert!(aabb.contains(Vec3::ONE));
        assert!(!aabb.contains(Vec3::splat(1.5)));
    }

    #[test]
    fn translation_moves_aabb() {
        let aabb = Aabb::new(Vec3::splat(-0.5), Vec3::splat(0.5));

        let moved = aabb.transform(Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)));

        assert_eq!(moved.center(), Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(moved.size(), Vec3::ONE);
    }

    #[test]
    fn rotation_grows_aabb() {
        let aabb = Aabb::new(Vec3::splat(-0.5), Vec3::splat(0.5));

        // 绕 Y 轴转 45°，轴对齐盒子必然变大——这是 AABB 的固有代价。
        let rotated = aabb.transform(Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4));

        assert!(rotated.size().x > 1.0);
        assert!((rotated.size().y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn transforming_empty_aabb_stays_empty() {
        let result = Aabb::EMPTY.transform(Mat4::from_translation(Vec3::ONE));

        assert!(result.is_empty());
    }

    #[test]
    fn plane_is_normalized_on_construction() {
        // 系数整体放大 5 倍，归一化后距离应当不变。
        let plane = Plane::from_vec4(Vec4::new(0.0, 5.0, 0.0, -10.0));

        assert!((plane.normal.length() - 1.0).abs() < 1e-6);
        assert_eq!(plane.d, -2.0);
        assert_eq!(plane.distance_to(Vec3::new(0.0, 3.0, 0.0)), 1.0);
    }

    #[test]
    fn degenerate_plane_does_not_produce_nan() {
        let plane = Plane::from_vec4(Vec4::ZERO);

        assert!(plane.normal.is_finite());
    }

    #[test]
    fn distance_sign_indicates_side() {
        // XZ 平面，法线朝上。
        let plane = Plane::from_vec4(Vec4::new(0.0, 1.0, 0.0, 0.0));

        assert!(plane.distance_to(Vec3::Y) > 0.0);
        assert!(plane.distance_to(-Vec3::Y) < 0.0);
        assert_eq!(plane.distance_to(Vec3::ZERO), 0.0);
    }
}
