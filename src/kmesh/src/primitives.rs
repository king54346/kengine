//! 内置图元。

use crate::{Mesh, Vertex};
use kmath::Vec3;

impl Mesh {
    /// 边长为 1 的立方体，六个面各有独立法线与完整 UV。
    pub fn cube() -> Self {
        // 每个面 4 个顶点，法线沿面朝外；UV 按左上→左下→右下→右上铺满。
        //
        // # 四个角的顺序不能随便写
        //
        // 渲染器开着背面剔除（`front_face: Ccw` + `cull_mode: Back`），
        // 绕序反了的面**从外面就是看不见的**，而且不报任何错——
        // 立方体会变成只剩几个面的空壳。曾经 ±X 和 ±Y 四个面就是反的。
        //
        // 定角的规矩：给这个面挑一组「向右」`r` 和「向上」`u`，
        // 使 `r × u == 法线`（右手系）。然后按
        // 左上 `-r+u`、左下 `-r-u`、右下 `+r-u`、右上 `+r+u` 排。
        // 这样出来的绕序必然是对的——展开算一下就是
        // `(v1-v0) × (v2-v0) = 4(r × u) = 4n`。
        //
        // 同一组 `r`/`u` 也决定了贴图怎么贴：`r` 是 u 增大的方向，
        // `u` 是 v **减小**的方向（v 向下）。所以下面每个面都标出了
        // 自己那组 r/u。
        const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
            // +X：r = -Z，u = +Y
            (
                [1.0, 0.0, 0.0],
                [
                    [0.5, 0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, 0.5, -0.5],
                ],
            ),
            // -X：r = +Z，u = +Y
            (
                [-1.0, 0.0, 0.0],
                [
                    [-0.5, 0.5, -0.5],
                    [-0.5, -0.5, -0.5],
                    [-0.5, -0.5, 0.5],
                    [-0.5, 0.5, 0.5],
                ],
            ),
            // +Y：r = +X，u = -Z（顶面的「上」朝 -Z，即贴图的上方朝北）
            (
                [0.0, 1.0, 0.0],
                [
                    [-0.5, 0.5, -0.5],
                    [-0.5, 0.5, 0.5],
                    [0.5, 0.5, 0.5],
                    [0.5, 0.5, -0.5],
                ],
            ),
            // -Y：r = +X，u = +Z
            (
                [0.0, -1.0, 0.0],
                [
                    [-0.5, -0.5, 0.5],
                    [-0.5, -0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, -0.5, 0.5],
                ],
            ),
            // +Z：r = +X，u = +Y
            (
                [0.0, 0.0, 1.0],
                [
                    [-0.5, 0.5, 0.5],
                    [-0.5, -0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, 0.5, 0.5],
                ],
            ),
            // -Z：r = -X，u = +Y
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

                // 绕序：从球**外面**看必须是逆时针，否则整个球会被背面剔除
                // 剔掉，看到的变成远侧半球的内壁——轮廓一模一样，
                // 但法线全背对相机，光照和深度都是错的。
                //
                // `a` 同环右邻是 `a + 1`，下一环正下方是 `b`。
                // 沿 `a → a+1 → b` 走出来的法线朝外（`recompute_tangents`
                // 也靠这个方向）。
                //
                // 两极处会退化成三角形，多出的那个三角形面积为零，无需特判。
                indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
            }
        }

        let mut mesh = Self::new(vertices, indices);
        mesh.recompute_tangents();
        mesh
    }

    /// 圆柱，直径 1、高 1，中轴沿 Y。
    ///
    /// `segments` 是周向分段数，至少 3。
    ///
    /// 侧面与两个端盖**不共享顶点**：共享的话端盖边缘的法线会被侧面
    /// 的法线拉平，柱子的上下边缘看上去像是圆角的。
    pub fn cylinder(segments: u32) -> Self {
        let segments = segments.max(3);
        let (half_height, radius) = (0.5_f32, 0.5_f32);

        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        // ── 侧面 ──
        for segment in 0..=segments {
            let u = segment as f32 / segments as f32;
            let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
            let normal = Vec3::new(cos, 0.0, sin);

            vertices.push(Vertex::new(
                normal * radius + Vec3::Y * half_height,
                normal,
                [u, 0.0],
            ));
            vertices.push(Vertex::new(
                normal * radius - Vec3::Y * half_height,
                normal,
                [u, 1.0],
            ));
        }
        for segment in 0..segments {
            let a = segment * 2;
            indices.extend_from_slice(&[a, a + 2, a + 1, a + 2, a + 3, a + 1]);
        }

        // ── 两个端盖 ──
        for (sign, normal) in [(1.0_f32, Vec3::Y), (-1.0, Vec3::NEG_Y)] {
            let center = vertices.len() as u32;
            vertices.push(Vertex::new(
                Vec3::Y * half_height * sign,
                normal,
                [0.5, 0.5],
            ));

            for segment in 0..=segments {
                let u = segment as f32 / segments as f32;
                let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
                vertices.push(Vertex::new(
                    Vec3::new(cos * radius, half_height * sign, sin * radius),
                    normal,
                    [cos * 0.5 + 0.5, sin * 0.5 + 0.5],
                ));
            }

            for segment in 0..segments {
                let a = center + 1 + segment;
                // 上下两个盖的绕序相反，否则有一个会朝里。
                if sign > 0.0 {
                    indices.extend_from_slice(&[center, a + 1, a]);
                } else {
                    indices.extend_from_slice(&[center, a, a + 1]);
                }
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

    #[test]
    fn cylinder_side_normals_point_outward() {
        let mesh = Mesh::cylinder(16);
        for vertex in mesh.vertices() {
            let normal = Vec3::from_array(vertex.normal);
            let position = Vec3::from_array(vertex.position);
            // 侧面的顶点法线该是水平的、指着外面。
            if normal.y.abs() < 0.5 {
                let radial = Vec3::new(position.x, 0.0, position.z);
                assert!(
                    normal.dot(radial) > 0.0,
                    "侧面法线朝里了：{normal:?} vs {radial:?}"
                );
            }
        }
    }

    #[test]
    fn cylinder_caps_are_flat() {
        // 端盖和侧面不共享顶点。共享的话端盖边缘的法线会被侧面拉平，
        // 柱子的上下边缘看上去像圆角的。
        let mesh = Mesh::cylinder(16);
        let top = mesh
            .vertices()
            .iter()
            .filter(|v| v.position[1] > 0.49 && v.normal[1] > 0.9)
            .count();
        assert!(top > 3, "顶盖上没有朝上的法线，端盖多半和侧面共享了顶点");
    }

    #[test]
    fn cylinder_fits_the_unit_box() {
        let mesh = Mesh::cylinder(24);
        for vertex in mesh.vertices() {
            let p = Vec3::from_array(vertex.position);
            assert!(p.y.abs() <= 0.5 + 1e-5, "高度超了：{}", p.y);
            let radius = (p.x * p.x + p.z * p.z).sqrt();
            assert!(radius <= 0.5 + 1e-5, "半径超了：{radius}");
        }
    }

    #[test]
    fn cylinder_indices_stay_in_range() {
        let mesh = Mesh::cylinder(3);
        let count = mesh.vertices().len() as u32;
        assert!(mesh.indices().iter().all(|i| *i < count));
        assert_eq!(mesh.indices().len() % 3, 0);
    }

    #[test]
    fn cylinder_clamps_tiny_segment_counts() {
        // 少于 3 段构不成一个封闭的柱面。
        for segments in [0, 1, 2, 3] {
            let mesh = Mesh::cylinder(segments);
            assert!(!mesh.indices().is_empty(), "{segments} 段时是空的");
        }
    }

    #[test]
    fn cylinder_winding_is_outward() {
        // 面朝里的话背面剔除会把柱子剔没，画面上什么都不剩。
        let mesh = Mesh::cylinder(16);
        for triangle in mesh.indices().chunks_exact(3) {
            let p: Vec<Vec3> = triangle
                .iter()
                .map(|i| Vec3::from_array(mesh.vertices()[*i as usize].position))
                .collect();
            let face = (p[1] - p[0]).cross(p[2] - p[0]);
            if face.length_squared() < 1e-12 {
                continue;
            }
            let center = (p[0] + p[1] + p[2]) / 3.0;
            assert!(
                face.dot(center) > -1e-4,
                "三角形朝里了：面法线 {face:?}，中心 {center:?}"
            );
        }
    }

    // ── 绕序 ──
    //
    // 真正决定「看不看得见」的是 GPU 的剔除规则，那条在
    // `krender/tests/culling_convention.rs` 里真的渲一遍验。
    // 这里验的是**便宜的那一半**：三角形绕序和它自己的顶点法线一致。
    // 两者等价（渲染器的约定就是「几何法线朝相机 = 正面」），
    // 但这一条不需要显卡，CI 上也跑得动。

    /// 一个三角形的几何法线（右手系叉积）。
    fn geometric_normal(mesh: &Mesh, tri: &[u32]) -> Vec3 {
        let p = |i: u32| Vec3::from(mesh.vertices()[i as usize].position);
        (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]))
    }

    /// 每个三角形的绕序都要和顶点法线一致。
    ///
    /// 面积为零的三角形（球的两极）跳过——它没有朝向可言。
    fn assert_winding_matches_normals(name: &str, mesh: &Mesh) {
        let mut checked = 0;
        for (index, tri) in mesh.indices().chunks_exact(3).enumerate() {
            let geometric = geometric_normal(mesh, tri);
            if geometric.length() < 1e-6 {
                continue;
            }
            let vertex = tri
                .iter()
                .map(|&i| Vec3::from(mesh.vertices()[i as usize].normal))
                .fold(Vec3::ZERO, |a, b| a + b);

            assert!(
                geometric.dot(vertex) > 0.0,
                "{name} 的三角形 #{index} 绕序反了：                 几何法线 {geometric:?}，顶点法线 {vertex:?}。                 这种面从外面看不见，而且不报任何错。"
            );
            checked += 1;
        }
        assert!(checked > 0, "{name} 一个有效三角形都没有");
    }

    #[test]
    fn every_cube_face_is_wound_outwards() {
        // 这一条曾经挂过：±X 和 ±Y 四个面反着绕，立方体在画面上
        // 只剩两个面看得见。
        assert_winding_matches_normals("立方体", &Mesh::cube());
    }

    #[test]
    fn the_sphere_is_wound_outwards() {
        // 这一条也曾经挂过，而且更隐蔽：球翻面之后轮廓一模一样，
        // 只是看到的变成了远侧半球的内壁。
        assert_winding_matches_normals("球", &Mesh::sphere(12, 18));
    }

    #[test]
    fn the_plane_is_wound_upwards() {
        assert_winding_matches_normals("平面", &Mesh::plane(1.0));
    }

    #[test]
    fn the_cylinder_is_wound_outwards() {
        assert_winding_matches_normals("圆柱", &Mesh::cylinder(16));
    }

    #[test]
    fn the_cube_has_one_quad_per_axis_direction() {
        // 六个面的法线必须各占一个轴向。修绕序时把某个面的顶点抄错位置
        // 的话，这一条会先响。
        let mut normals: Vec<[i32; 3]> = Mesh::cube()
            .vertices()
            .chunks_exact(4)
            .map(|face| {
                let n = face[0].normal;
                [n[0] as i32, n[1] as i32, n[2] as i32]
            })
            .collect();
        normals.sort();

        assert_eq!(
            normals,
            vec![
                [-1, 0, 0],
                [0, -1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [0, 1, 0],
                [1, 0, 0],
            ]
        );
    }

    #[test]
    fn every_cube_face_covers_the_whole_uv_square() {
        // 定角的规矩改动之后 UV 很容易跟着错位。每个面的四个角
        // 应当正好是 (0,0)、(0,1)、(1,1)、(1,0) 各一个。
        for (index, face) in Mesh::cube().vertices().chunks_exact(4).enumerate() {
            let mut uvs: Vec<[i32; 2]> = face
                .iter()
                .map(|v| [v.uv[0] as i32, v.uv[1] as i32])
                .collect();
            uvs.sort();
            assert_eq!(
                uvs,
                vec![[0, 0], [0, 1], [1, 0], [1, 1]],
                "第 {index} 个面的 UV 没铺满"
            );
        }
    }
}
