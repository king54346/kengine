//! 在一组三角形上按面积均匀撒点（three-stylized 的 `surfaceSampler.ts`）。
//!
//! 先累计每个三角形的面积，抽一个 [0, 总面积) 的数二分找到三角形，再在三角形里均匀取重心坐标。
//! 同一个种子撒出同一批点。

use kmath::{Mat4, Rng, Vec3};
use kmesh::Mesh;

/// 撒点用的表面：世界空间（或者草地节点空间）的一组三角形。
#[derive(Debug, Clone, Default)]
pub struct SurfaceSampler {
    triangles: Vec<[Vec3; 3]>,
    normals: Vec<Vec3>,
    cumulative: Vec<f32>,
}

/// 撒到的一个点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfacePoint {
    pub position: Vec3,
    /// 所在三角形的面法线（朝上的那一面）。
    pub normal: Vec3,
}

impl SurfaceSampler {
    /// 用一组三角形建。退化的三角形丢掉；法线统一翻到朝上（y ≥ 0）那一面——草总是往上长。
    pub fn new(triangles: impl IntoIterator<Item = [Vec3; 3]>) -> Self {
        let mut sampler = Self::default();
        let mut total = 0.0;
        for triangle in triangles {
            let cross = (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
            let double_area = cross.length();
            if double_area < 1e-8 {
                continue;
            }
            total += double_area * 0.5;
            let normal = cross / double_area;
            sampler
                .normals
                .push(if normal.y < 0.0 { -normal } else { normal });
            sampler.triangles.push(triangle);
            sampler.cumulative.push(total);
        }
        sampler
    }

    /// 用一个网格的三角形建（先乘 `transform`）。
    pub fn from_mesh(mesh: &Mesh, transform: Mat4) -> Self {
        let vertices = mesh.vertices();
        let indices = mesh.indices();
        Self::new(indices.chunks_exact(3).map(|tri| {
            [tri[0], tri[1], tri[2]]
                .map(|i| transform.transform_point3(vertices[i as usize].position()))
        }))
    }

    /// 总面积（平方米）。
    pub fn area(&self) -> f32 {
        self.cumulative.last().copied().unwrap_or(0.0)
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// 均匀撒一个点。表面是空的时候返回 `None`。
    pub fn sample(&self, rng: &mut Rng) -> Option<SurfacePoint> {
        let total = self.area();
        if total <= 0.0 {
            return None;
        }
        let target = rng.next_f32() * total;
        let index = self
            .cumulative
            .partition_point(|&area| area < target)
            .min(self.triangles.len() - 1);
        let (mut u, mut v) = (rng.next_f32(), rng.next_f32());
        if u + v > 1.0 {
            u = 1.0 - u;
            v = 1.0 - v;
        }
        let [a, b, c] = self.triangles[index];
        Some(SurfacePoint {
            position: a * (1.0 - u - v) + b * u + c * v,
            normal: self.normals[index],
        })
    }

    /// 撒 `count` 个点；给了 `coverage` 时按它的值（0..1）做拒绝采样——值越低越稀，
    /// 最多尝试 `count × 64` 次，所以覆盖很低的表面可能撒不满。
    pub fn scatter(
        &self,
        count: usize,
        seed: u64,
        coverage: Option<&dyn Fn(Vec3) -> f32>,
    ) -> Vec<SurfacePoint> {
        let mut rng = Rng::new(seed);
        let mut points = Vec::with_capacity(count);
        if self.is_empty() {
            return points;
        }
        match coverage {
            None => points.extend((0..count).filter_map(|_| self.sample(&mut rng))),
            Some(coverage) => {
                let attempts = count.saturating_mul(64).max(256);
                for _ in 0..attempts {
                    if points.len() >= count {
                        break;
                    }
                    let Some(point) = self.sample(&mut rng) else {
                        break;
                    };
                    if rng.next_f32() <= coverage(point.position) {
                        points.push(point);
                    }
                }
            }
        }
        points
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f32) -> SurfaceSampler {
        let (a, b, c, d) = (
            Vec3::ZERO,
            Vec3::new(size, 0.0, 0.0),
            Vec3::new(size, 0.0, size),
            Vec3::new(0.0, 0.0, size),
        );
        SurfaceSampler::new([[a, c, b], [a, d, c]])
    }

    #[test]
    fn points_are_spread_by_area_and_stay_on_the_surface() {
        // 一大一小两块：大的面积是小的 3 倍，点数也该差不多 3 倍。
        let small = [
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let big = [
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(13.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 1.0),
        ];
        let sampler = SurfaceSampler::new([small, big]);
        assert!((sampler.area() - 2.0).abs() < 1e-5);
        let points = sampler.scatter(4000, 7, None);
        let on_big = points.iter().filter(|p| p.position.x >= 10.0).count() as f32;
        let ratio = on_big / (points.len() as f32 - on_big);
        assert!((ratio - 3.0).abs() < 0.4, "{ratio}");
        assert!(
            points
                .iter()
                .all(|p| p.normal == Vec3::Y && p.position.y == 0.0)
        );
    }

    #[test]
    fn the_same_seed_scatters_the_same_points() {
        let sampler = square(4.0);
        assert_eq!(sampler.scatter(50, 3, None), sampler.scatter(50, 3, None));
        assert_ne!(sampler.scatter(50, 3, None), sampler.scatter(50, 4, None));
    }

    #[test]
    fn coverage_thins_the_points_out() {
        let sampler = square(10.0);
        let left_only = |p: Vec3| if p.x < 5.0 { 1.0 } else { 0.0 };
        let points = sampler.scatter(500, 1, Some(&left_only));
        assert_eq!(points.len(), 500);
        assert!(points.iter().all(|p| p.position.x < 5.0));
    }

    #[test]
    fn downward_faces_are_flipped_up() {
        let sampler = SurfaceSampler::new([[
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ]]);
        assert_eq!(sampler.normals[0], Vec3::Y);
    }
}
