//! 内置图元。

use crate::{Mesh, Vertex};
use kmath::Vec3;

impl Mesh {
    /// 边长为 1 的立方体，六个面各有独立法线与完整 UV。
    pub fn cube() -> Self {
        // 每个面 4 个顶点，法线沿面朝外；UV 按左上→左下→右下→右上铺满。
        const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
            // +X
            (
                [1.0, 0.0, 0.0],
                [
                    [0.5, 0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, 0.5, 0.5],
                ],
            ),
            // -X
            (
                [-1.0, 0.0, 0.0],
                [
                    [-0.5, 0.5, 0.5],
                    [-0.5, -0.5, 0.5],
                    [-0.5, -0.5, -0.5],
                    [-0.5, 0.5, -0.5],
                ],
            ),
            // +Y
            (
                [0.0, 1.0, 0.0],
                [
                    [-0.5, 0.5, 0.5],
                    [-0.5, 0.5, -0.5],
                    [0.5, 0.5, -0.5],
                    [0.5, 0.5, 0.5],
                ],
            ),
            // -Y
            (
                [0.0, -1.0, 0.0],
                [
                    [-0.5, -0.5, -0.5],
                    [-0.5, -0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, -0.5, -0.5],
                ],
            ),
            // +Z
            (
                [0.0, 0.0, 1.0],
                [
                    [-0.5, 0.5, 0.5],
                    [-0.5, -0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, 0.5, 0.5],
                ],
            ),
            // -Z
            (
                [0.0, 0.0, -1.0],
                [
                    [0.5, 0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [-0.5, -0.5, -0.5],
                    [-0.5, 0.5, -0.5],
                ],
            ),
        ];
        const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

        let mut vertices = Vec::with_capacity(24);
        let mut indices = Vec::with_capacity(36);

        for (normal, corners) in FACES {
            let base = vertices.len() as u32;
            for (corner, uv) in corners.into_iter().zip(UVS) {
                vertices.push(Vertex {
                    position: corner,
                    normal,
                    uv,
                    ..Default::default()
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        let mut mesh = Self::new(vertices, indices);
        mesh.recompute_tangents();
        mesh
    }

    /// 每面一种颜色的立方体，便于分辨朝向。
    pub fn cube_colored() -> Self {
        const FACE_COLORS: [[f32; 3]; 6] = [
            [1.0, 0.3, 0.3], // +X 红
            [0.3, 1.0, 1.0], // -X 青
            [0.3, 1.0, 0.3], // +Y 绿
            [1.0, 0.3, 1.0], // -Y 品红
            [0.4, 0.5, 1.0], // +Z 蓝
            [1.0, 1.0, 0.3], // -Z 黄
        ];

        let mut mesh = Self::cube();
        for (index, vertex) in mesh.vertices_mut().iter_mut().enumerate() {
            // 立方体每面连续 4 个顶点。
            vertex.color = FACE_COLORS[index / 4];
        }
        mesh
    }

    /// XZ 平面上的方板，边长为 1，法线朝 +Y。
    ///
    /// `uv_scale` 控制纹理平铺次数，配合 `WrapMode::Repeat` 使用。
    pub fn plane(uv_scale: f32) -> Self {
        let vertices = vec![
            Vertex::new(Vec3::new(-0.5, 0.0, -0.5), Vec3::Y, [0.0, 0.0]),
            Vertex::new(Vec3::new(-0.5, 0.0, 0.5), Vec3::Y, [0.0, uv_scale]),
            Vertex::new(Vec3::new(0.5, 0.0, 0.5), Vec3::Y, [uv_scale, uv_scale]),
            Vertex::new(Vec3::new(0.5, 0.0, -0.5), Vec3::Y, [uv_scale, 0.0]),
        ];
        let mut mesh = Self::new(vertices, vec![0, 1, 2, 0, 2, 3]);
        mesh.recompute_tangents();
        mesh
    }

    /// UV 球，半径 0.5。
    ///
    /// `rings` 是纬度分段数，`segments` 是经度分段数，各自至少为 3 和 2。
    pub fn sphere(rings: u32, segments: u32) -> Self {
        let rings = rings.max(2);
        let segments = segments.max(3);

        let mut vertices = Vec::with_capacity(((rings + 1) * (segments + 1)) as usize);
        let mut indices = Vec::with_capacity((rings * segments * 6) as usize);

        for ring in 0..=rings {
            // theta 从北极 0 走到南极 π。
            let v = ring as f32 / rings as f32;
            let theta = v * std::f32::consts::PI;
            let (sin_theta, cos_theta) = theta.sin_cos();

            for segment in 0..=segments {
                let u = segment as f32 / segments as f32;
                let phi = u * std::f32::consts::TAU;
                let (sin_phi, cos_phi) = phi.sin_cos();

                // 单位球面上的点即为法线，半径 0.5 得到直径 1 的球。
                let normal = Vec3::new(sin_theta * cos_phi, cos_theta, sin_theta * sin_phi);
                vertices.push(Vertex::new(normal * 0.5, normal, [u, v]));
            }
        }

        let stride = segments + 1;
        for ring in 0..rings {
            for segment in 0..segments {
                let a = ring * stride + segment;
                let b = a + stride;

                // 两极处会退化成三角形，多出的那个三角形面积为零，无需特判。
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }

        let mut mesh = Self::new(vertices, indices);
        mesh.recompute_tangents();
        mesh
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kmath::Vec3;

    #[test]
    fn cube_normals_point_outward() {
        let mesh = Mesh::cube();

        for vertex in mesh.vertices() {
            // 立方体顶点都在原点外侧，法线朝外时点乘为正。
            assert!(
                vertex.normal().dot(vertex.position()) > 0.0,
                "法线与顶点朝向相反"
            );
            assert!((vertex.normal().length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn cube_has_six_faces_worth_of_geometry() {
        let mesh = Mesh::cube();

        assert_eq!(mesh.vertices().len(), 24);
        assert_eq!(mesh.index_count(), 36);
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn cube_colored_assigns_one_color_per_face() {
        let mesh = Mesh::cube_colored();

        // 同一面的 4 个顶点颜色应当一致，相邻面则不同。
        assert_eq!(mesh.vertices()[0].color, mesh.vertices()[3].color);
        assert_ne!(mesh.vertices()[0].color, mesh.vertices()[4].color);
    }

    #[test]
    fn plane_normal_points_up() {
        let mesh = Mesh::plane(1.0);

        assert!(mesh.vertices().iter().all(|v| v.normal == [0.0, 1.0, 0.0]));
    }

    #[test]
    fn plane_uv_scale_controls_tiling() {
        let mesh = Mesh::plane(4.0);

        let max_u = mesh.vertices().iter().map(|v| v.uv[0]).fold(0.0, f32::max);
        assert_eq!(max_u, 4.0);
    }

    #[test]
    fn sphere_vertices_lie_on_surface() {
        let mesh = Mesh::sphere(8, 12);

        for vertex in mesh.vertices() {
            // 所有顶点到球心距离应等于半径 0.5。
            assert!((vertex.position().length() - 0.5).abs() < 1e-5);
            // 球面上法线与位置同向。
            assert!((vertex.normal().length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn sphere_clamps_degenerate_parameters() {
        // 分段数过小会导致除零或空网格，应当被钳制。
        let mesh = Mesh::sphere(0, 0);

        assert!(mesh.is_valid());
        assert!(mesh.triangle_count() > 0);
    }

    #[test]
    fn sphere_aabb_is_unit_cube() {
        let aabb = Mesh::sphere(16, 24).aabb();

        assert!((aabb.size() - Vec3::ONE).length() < 1e-3);
    }
}
