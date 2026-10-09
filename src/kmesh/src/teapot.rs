//! Utah 茶壶：three.js `TeapotGeometry` 的移植。
//!
//! 32 个双三次 Bezier 片（壶身 20 片、盖子 8 片、底 4 片），每片按
//! `segments × segments` 细分。和 three.js 一样把盖子在 XY 上放大 7.7%
//! 堵住和壶身之间的缝，Z 向上的原始数据转成 Y 向上。

use crate::teapot_data::{PATCHES, VERTICES};
use crate::{Mesh, Vertex};
use kmath::Vec3;

/// Bernstein 基函数与它们对参数的导数。
fn bernstein(t: f32) -> ([f32; 4], [f32; 4]) {
    let u = 1.0 - t;
    (
        [u * u * u, 3.0 * t * u * u, 3.0 * t * t * u, t * t * t],
        [
            -3.0 * u * u,
            3.0 * u * u - 6.0 * t * u,
            6.0 * t * u - 3.0 * t * t,
            3.0 * t * t,
        ],
    )
}

impl Mesh {
    /// Utah 茶壶，高 `size`（从壶底到盖顶），中心在原点。
    ///
    /// `segments` 是每个 Bezier 片每个方向的细分数（three.js 默认 10）。
    pub fn teapot(size: f32, segments: u32) -> Self {
        let segments = segments.max(2) as usize;
        // three.js 的 `size` 是半高；这边给整高更直观。
        let max_height = 3.15f32;
        let half_height = max_height / 2.0;
        let scale = size / max_height;

        let per_row = segments + 1;
        let mut vertices: Vec<Vertex> = Vec::with_capacity(32 * per_row * per_row);
        let mut indices: Vec<u32> = Vec::new();

        for patch in 0..32 {
            let lid = (20..28).contains(&patch);
            // 这一片的 16 个控制点：`control[b][a]`，b 配 s、a 配 t。
            let mut control = [[Vec3::ZERO; 4]; 4];
            for (b, row) in control.iter_mut().enumerate() {
                for (a, point) in row.iter_mut().enumerate() {
                    let index = PATCHES[patch * 16 + b * 4 + a] as usize;
                    let mut p = Vec3::new(
                        VERTICES[index * 3],
                        VERTICES[index * 3 + 1],
                        VERTICES[index * 3 + 2],
                    );
                    if lid {
                        p.x *= 1.077;
                        p.y *= 1.077;
                    }
                    *point = p;
                }
            }

            let base = vertices.len() as u32;
            for s_step in 0..=segments {
                let s = s_step as f32 / segments as f32;
                let (bs, dbs) = bernstein(s);
                for t_step in 0..=segments {
                    let t = t_step as f32 / segments as f32;
                    let (bt, dbt) = bernstein(t);
                    let mut position = Vec3::ZERO;
                    let mut ds = Vec3::ZERO;
                    let mut dt = Vec3::ZERO;
                    for b in 0..4 {
                        for a in 0..4 {
                            let p = control[b][a];
                            position += p * (bs[b] * bt[a]);
                            ds += p * (dbs[b] * bt[a]);
                            dt += p * (bs[b] * dbt[a]);
                        }
                    }
                    // 原始数据 Z 向上：(x, y, z) → (x, z, −y)。
                    let normal = if position.x == 0.0 && position.y == 0.0 {
                        // 顶尖和底尖：法线退化，按在上半还是下半朝上或朝下。
                        Vec3::new(0.0, if position.z > half_height { 1.0 } else { -1.0 }, 0.0)
                    } else {
                        let n = dt.cross(ds).normalize_or(Vec3::Z);
                        Vec3::new(n.x, n.z, -n.y)
                    };
                    vertices.push(Vertex::new(
                        Vec3::new(position.x, position.z - half_height, -position.y) * scale,
                        normal,
                        [1.0 - t, 1.0 - s],
                    ));
                }
            }
            let same =
                |a: u32, b: u32| vertices[a as usize].position == vertices[b as usize].position;
            for s_step in 0..segments {
                for t_step in 0..segments {
                    let v1 = base + (s_step * per_row + t_step) as u32;
                    let v2 = v1 + 1;
                    let v3 = v2 + per_row as u32;
                    let v4 = v1 + per_row as u32;
                    // 尖顶处的三角形有两个顶点重合，面积为零，丢掉。
                    if !(same(v1, v2) || same(v1, v3) || same(v2, v3)) {
                        indices.extend_from_slice(&[v1, v2, v3]);
                    }
                    if !(same(v1, v3) || same(v1, v4) || same(v3, v4)) {
                        indices.extend_from_slice(&[v1, v3, v4]);
                    }
                }
            }
        }

        let mut mesh = Mesh::new(vertices, indices);
        mesh.recompute_tangents();
        mesh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_teapot_has_the_requested_height() {
        let mesh = Mesh::teapot(2.0, 6);
        let aabb = mesh.aabb();
        assert!((aabb.size().y - 2.0).abs() < 0.05, "高度 {}", aabb.size().y);
        assert!(mesh.is_valid());
    }

    #[test]
    fn the_winding_agrees_with_the_normals() {
        // 绕序（背面剔除用的）和存的法线（光照用的）得是同一个朝向，
        // 不然开着剔除时茶壶是个只剩内壁的壳。
        let mesh = Mesh::teapot(2.0, 6);
        let vertices = mesh.vertices();
        let (mut agree, mut total) = (0, 0);
        for triangle in mesh.indices().chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| vertices[triangle[k] as usize]);
            let face = (b.position() - a.position()).cross(c.position() - a.position());
            if face.length() < 1e-6 {
                continue;
            }
            total += 1;
            if face.dot(a.normal() + b.normal() + c.normal()) > 0.0 {
                agree += 1;
            }
        }
        assert!(agree * 100 >= total * 98, "{agree}/{total}");
    }
}
