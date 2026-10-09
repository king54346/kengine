//! 网格的三角形 BVH：成千上万次射线求交时用（烘光照贴图、可见性、拾取复杂模型）。
//!
//! [`Mesh::raycast`](crate::Mesh::raycast) 逐个三角形测，一次几微秒，拾取够用；
//! 但烘一张 256² 的光照贴图、每个纹素朝光打几条射线，就是几十万次 × 几千个三角形。
//! 这里用 [`kmath::Bvh`] 先按包围盒筛，只测射线穿过的那几个盒子里的三角形。
//!
//! 坐标是建的时候给的那个空间（通常先把网格变换到世界空间再建，见 [`MeshBvh::from_mesh`]）。

use crate::Mesh;
use kmath::{Aabb, Bvh, Intersection, Mat4, Ray3d, Vec3};

/// 一组三角形 + 它们的 BVH。
#[derive(Debug, Clone)]
pub struct MeshBvh {
    triangles: Vec<[Vec3; 3]>,
    bvh: Bvh,
}

impl MeshBvh {
    /// 从若干三角形建。
    pub fn new(triangles: Vec<[Vec3; 3]>) -> Self {
        let bounds: Vec<Aabb> = triangles
            .iter()
            .map(|[a, b, c]| {
                // 轴对齐的三角形包围盒在那个轴上厚度为 0，射线-盒子测试的板块法遇到 0 厚度
                // 会出 NaN（0 × ∞），撑开一点点。
                let pad = Vec3::splat(1e-4);
                Aabb::new(a.min(*b).min(*c) - pad, a.max(*b).max(*c) + pad)
            })
            .collect();
        Self {
            bvh: Bvh::build(&bounds),
            triangles,
        }
    }

    /// 从网格建，顶点先乘 `transform`（通常是节点的世界矩阵）。
    pub fn from_mesh(mesh: &Mesh, transform: Mat4) -> Self {
        let mut triangles = Vec::with_capacity(mesh.indices().len() / 3);
        Self::append(&mut triangles, mesh, transform);
        Self::new(triangles)
    }

    /// 好几个网格合在一棵树里（一整个模型）。
    pub fn from_meshes<'a>(meshes: impl IntoIterator<Item = (&'a Mesh, Mat4)>) -> Self {
        let mut triangles = Vec::new();
        for (mesh, transform) in meshes {
            Self::append(&mut triangles, mesh, transform);
        }
        Self::new(triangles)
    }

    fn append(triangles: &mut Vec<[Vec3; 3]>, mesh: &Mesh, transform: Mat4) {
        let vertices = mesh.vertices();
        for tri in mesh.indices().chunks_exact(3) {
            let corner = |k: usize| {
                vertices
                    .get(tri[k] as usize)
                    .map(|v| transform.transform_point3(v.position()))
            };
            if let (Some(a), Some(b), Some(c)) = (corner(0), corner(1), corner(2)) {
                triangles.push([a, b, c]);
            }
        }
    }

    /// 三角形数。
    pub fn len(&self) -> usize {
        self.triangles.len()
    }

    /// 是否一个三角形都没有。
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    fn candidates(&self, origin: Vec3, dir: Vec3, max: f32, out: &mut Vec<u32>) {
        let ray = Ray3d::new(origin, dir);
        self.bvh.query(
            |aabb| {
                if ray.hit_aabb(aabb, max).is_some() {
                    Intersection::Intersects
                } else {
                    Intersection::Outside
                }
            },
            out,
        );
    }

    /// 最近的命中：`(t, 面法线（未归一化）)`，`origin + dir * t` 是命中点，只算 `0 < t ≤ max`。双面求交。
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max: f32) -> Option<(f32, Vec3)> {
        let mut candidates = Vec::new();
        self.candidates(origin, dir, max, &mut candidates);
        let mut best: Option<(f32, Vec3)> = None;
        for index in candidates {
            let [a, b, c] = self.triangles[index as usize];
            if let Some((t, normal)) = crate::ray_triangle(origin, dir, a, b, c)
                && t > 0.0
                && t <= max
                && best.is_none_or(|(b, _)| t < b)
            {
                best = Some((t, normal));
            }
        }
        best
    }

    /// 有没有挡着（`0 < t ≤ max` 之间有任何命中）。阴影 / 可见性只要这个，碰到第一个就返回。
    pub fn occluded(&self, origin: Vec3, dir: Vec3, max: f32) -> bool {
        let mut candidates = Vec::new();
        self.candidates(origin, dir, max, &mut candidates);
        candidates.into_iter().any(|index| {
            let [a, b, c] = self.triangles[index as usize];
            crate::ray_triangle(origin, dir, a, b, c).is_some_and(|(t, _)| t > 0.0 && t <= max)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agrees_with_brute_force_raycast() {
        let mesh = Mesh::torus_knot(1.0, 0.3, 64, 12, 2, 3);
        let bvh = MeshBvh::from_mesh(&mesh, Mat4::IDENTITY);
        assert_eq!(bvh.len(), mesh.indices().len() / 3);
        let mut hits = 0;
        for i in 0..200 {
            let a = i as f32 * 0.37;
            let origin = Vec3::new(a.cos() * 4.0, (a * 0.7).sin() * 2.0, a.sin() * 4.0);
            let dir =
                (Vec3::new((a * 1.3).sin() * 0.5, 0.0, (a * 0.9).cos() * 0.5) - origin).normalize();
            let expected = mesh.raycast(origin, dir).map(|(t, _)| t);
            let actual = bvh.raycast(origin, dir, f32::MAX).map(|(t, _)| t);
            match (expected, actual) {
                (Some(e), Some(a)) => {
                    assert!((e - a).abs() < 1e-4, "第 {i} 条：暴力 {e}，BVH {a}");
                    hits += 1;
                }
                (None, None) => {}
                other => panic!("第 {i} 条射线结果不一致：{other:?}"),
            }
            assert_eq!(
                bvh.occluded(origin, dir, 100.0),
                expected.is_some_and(|t| t <= 100.0)
            );
        }
        assert!(hits > 20, "测试射线大多打空了（{hits}），没测到东西");
        // max 之外的命中不算。
        let origin = Vec3::new(0.0, 0.0, 5.0);
        if let Some((t, _)) = bvh.raycast(origin, Vec3::NEG_Z, f32::MAX) {
            assert!(bvh.raycast(origin, Vec3::NEG_Z, t * 0.5).is_none());
            assert!(!bvh.occluded(origin, Vec3::NEG_Z, t * 0.5));
        }
    }
}
