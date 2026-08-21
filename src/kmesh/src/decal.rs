//! 网格贴花：把一块贴花投影到已有几何上，生成一片贴合的网格。
//!
//! 弹孔、血迹、裂纹这类东西。
//!
//! # 为什么是网格贴花而不是延迟贴花
//!
//! 延迟贴花的做法是画一个盒子，对盒子里的每个像素从**深度缓冲**反解
//! 世界坐标，再判断是否落在贴花体内。它更通用（对蒙皮动画也有效），
//! 但需要把深度缓冲当纹理采样——而深度缓冲此刻正挂在管线上当深度附件，
//! 要么复制一份，要么改成只读深度附件。这个引擎是**前向渲染**，
//! 没有现成的 G-buffer 可用。
//!
//! 网格贴花是在 CPU 上把接收面裁进贴花盒，生成一片贴合的三角形。
//! 代价是接收几何一变就要重新生成，所以**只适合静态几何**——
//! 弹孔打在墙上没问题，打在会动的角色身上就不行。
//!
//! # 深度冲突
//!
//! 贴花网格和它贴合的表面在同一个平面上，深度值几乎相同——
//! 不处理的话会出现斑驳的闪烁（z-fighting）。这里的做法是把贴花顶点
//! **沿法线抬起**一点点。抬多少由调用方定：太小挡不住冲突，
//! 太大会在掠射角下看出贴花浮在表面上方。

use crate::{Mesh, Vertex};
use kmath::{Mat4, Vec3};

/// 一块贴花的投影体。
///
/// 是一个**有朝向的盒子**：贴花沿盒子的 -Z 方向投影，
/// 贴花图铺满盒子的 XY 截面。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decal {
    /// 盒子的变换（位置 + 朝向 + 尺寸）。
    pub transform: Mat4,
    /// 表面法线与投影方向的夹角超过它就不贴。
    ///
    /// 弹孔打在墙上时，墙的侧面（几乎与投影方向平行）不该被贴上
    /// 一条拉伸得很长的条纹。取余弦值：0.3 大约是 72°。
    pub normal_threshold: f32,
    /// 沿法线抬起多少，用来避开深度冲突。
    pub offset: f32,
}

impl Default for Decal {
    fn default() -> Self {
        Self {
            transform: Mat4::IDENTITY,
            normal_threshold: 0.3,
            offset: 0.005,
        }
    }
}

impl Decal {
    /// 由位置、朝向、尺寸构造。
    ///
    /// `normal` 是贴花要贴上去的表面法线（贴花从这个方向照过去）。
    pub fn new(position: Vec3, normal: Vec3, size: Vec3, roll: f32) -> Self {
        let normal = normal.normalize_or(Vec3::Y);
        // 贴花沿盒子的 -Z 投影，所以盒子的 +Z 要对着表面法线。
        let up = if normal.y.abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let right = up.cross(normal).normalize_or(Vec3::X);
        let up = normal.cross(right);

        // `roll` 让同一个位置的贴花能有不同朝向——一排一模一样的
        // 弹孔看着很假。
        let (sin, cos) = roll.sin_cos();
        let rotated_right = right * cos + up * sin;
        let rotated_up = up * cos - right * sin;

        let transform = Mat4::from_cols(
            (rotated_right * size.x).extend(0.0),
            (rotated_up * size.y).extend(0.0),
            (normal * size.z).extend(0.0),
            position.extend(1.0),
        );
        Self {
            transform,
            ..Default::default()
        }
    }

    /// 贴花体的世界空间包围盒。用来先粗筛接收面。
    pub fn bounds(&self) -> kmath::Aabb {
        let mut bounds = kmath::Aabb::EMPTY;
        for x in [-0.5f32, 0.5] {
            for y in [-0.5f32, 0.5] {
                for z in [-0.5f32, 0.5] {
                    bounds.expand(self.transform.transform_point3(Vec3::new(x, y, z)));
                }
            }
        }
        bounds
    }
}

/// 把一块网格裁进贴花体，生成贴花网格。
///
/// `receiver_transform` 是接收网格的世界变换。返回的网格在**世界空间**。
///
/// 一个三角形都没落进去时返回 `None`——空网格会让渲染器白白建一份
/// GPU 缓冲，还要每帧参与剔除。
pub fn project(receiver: &Mesh, receiver_transform: Mat4, decal: &Decal) -> Option<Mesh> {
    let to_decal = decal.transform.inverse();
    let receiver_normal_matrix = Mat4::from_mat3(
        kmath::Mat3::from_mat4(receiver_transform)
            .inverse()
            .transpose(),
    );
    // 贴花的投影方向：盒子的 +Z 轴。
    let projection_axis = decal.transform.z_axis.truncate().normalize_or(Vec3::Z);

    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for triangle in receiver.indices().chunks_exact(3) {
        let corners: Vec<Vec3> = triangle
            .iter()
            .map(|i| {
                receiver_transform
                    .transform_point3(Vec3::from_array(receiver.vertices()[*i as usize].position))
            })
            .collect();

        // 面法线用**世界空间**的三角形算，而不是插值顶点法线：
        // 顶点法线在平滑着色的模型上是圆滑的，用它判角度会让
        // 一个平面上的三角形有的贴有的不贴。
        let face_normal = (corners[1] - corners[0])
            .cross(corners[2] - corners[0])
            .normalize_or(Vec3::ZERO);
        if face_normal == Vec3::ZERO {
            continue;
        }
        // 背向贴花的面不贴。不判的话墙的另一面也会出现弹孔。
        if face_normal.dot(projection_axis) < decal.normal_threshold {
            continue;
        }

        // 裁进单位立方体。
        let local: Vec<Vec3> = corners
            .iter()
            .map(|c| to_decal.transform_point3(*c))
            .collect();
        let clipped = clip_to_unit_cube(&local);
        if clipped.len() < 3 {
            continue;
        }

        // 扇形三角化：裁剪结果一定是凸多边形。
        let base = vertices.len() as u32;
        for point in &clipped {
            let world = decal.transform.transform_point3(*point);
            // 沿法线抬起，避开深度冲突。
            let position = world + face_normal * decal.offset;
            // UV 直接取贴花空间的 XY：贴花图铺满盒子的截面。
            // +0.5 是因为单位立方体的范围是 [-0.5, 0.5]。
            let uv = [point.x + 0.5, 0.5 - point.y];
            let normal = receiver_normal_matrix
                .transform_vector3(face_normal)
                .normalize_or(face_normal);
            vertices.push(Vertex::new(position, normal, uv));
        }
        for offset in 1..clipped.len() as u32 - 1 {
            indices.extend_from_slice(&[base, base + offset, base + offset + 1]);
        }
    }

    (!indices.is_empty()).then(|| Mesh::new(vertices, indices))
}

/// 把一个三角形裁进单位立方体 `[-0.5, 0.5]³`。
///
/// 用 Sutherland–Hodgman：对六个面依次裁剪。每次裁剪的结果仍是凸多边形，
/// 所以可以一路裁下去。
fn clip_to_unit_cube(triangle: &[Vec3]) -> Vec<Vec3> {
    let mut polygon = triangle.to_vec();

    // 六个面：(轴, 是否取正侧)。
    for axis in 0..3 {
        for positive in [true, false] {
            polygon = clip_to_plane(&polygon, axis, positive);
            // 全裁没了就不用继续了。
            if polygon.len() < 3 {
                return Vec::new();
            }
        }
    }
    polygon
}

/// 对一个轴对齐的平面做一次裁剪。
fn clip_to_plane(polygon: &[Vec3], axis: usize, positive: bool) -> Vec<Vec3> {
    // 到平面的有符号距离：正数在里面。
    let distance = |point: &Vec3| -> f32 {
        let value = point[axis];
        if positive { 0.5 - value } else { value + 0.5 }
    };

    let mut out = Vec::with_capacity(polygon.len() + 1);
    for index in 0..polygon.len() {
        let current = polygon[index];
        let next = polygon[(index + 1) % polygon.len()];
        let d0 = distance(&current);
        let d1 = distance(&next);

        if d0 >= 0.0 {
            out.push(current);
        }
        // 一进一出时在交点处插一个顶点。
        if (d0 >= 0.0) != (d1 >= 0.0) {
            // 分母是两个距离之差；同号时不会走到这里，所以不会为零，
            // 但浮点上仍可能极小——夹一下免得插值系数跑到区间外。
            let t = (d0 / (d0 - d1)).clamp(0.0, 1.0);
            out.push(current.lerp(next, t));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一块铺在 XZ 平面上、法线朝 +Y 的地板，边长 `size`。
    fn floor(size: f32) -> Mesh {
        let h = size * 0.5;
        let vertices = vec![
            Vertex::new(Vec3::new(-h, 0.0, -h), Vec3::Y, [0.0, 0.0]),
            Vertex::new(Vec3::new(h, 0.0, -h), Vec3::Y, [1.0, 0.0]),
            Vertex::new(Vec3::new(h, 0.0, h), Vec3::Y, [1.0, 1.0]),
            Vertex::new(Vec3::new(-h, 0.0, h), Vec3::Y, [0.0, 1.0]),
        ];
        // 从 +Y 看下去是逆时针。
        Mesh::new(vertices, vec![0, 2, 1, 0, 3, 2])
    }

    /// 一块贴在原点、朝上、边长 1 的贴花。
    fn decal_at(x: f32, z: f32, size: f32) -> Decal {
        Decal::new(
            Vec3::new(x, 0.0, z),
            Vec3::Y,
            Vec3::new(size, size, size),
            0.0,
        )
    }

    #[test]
    fn a_decal_on_a_floor_produces_geometry() {
        let mesh = project(&floor(10.0), Mat4::IDENTITY, &decal_at(0.0, 0.0, 1.0))
            .expect("贴花该落在地板上");
        assert!(!mesh.indices().is_empty());
        assert_eq!(mesh.indices().len() % 3, 0);
    }

    #[test]
    fn a_decal_outside_the_receiver_produces_nothing() {
        // 空网格会让渲染器白白建一份 GPU 缓冲，还要每帧参与剔除。
        assert!(project(&floor(10.0), Mat4::IDENTITY, &decal_at(100.0, 100.0, 1.0)).is_none());
    }

    #[test]
    fn the_geometry_stays_inside_the_decal_box() {
        // 裁剪没做对的话，贴花会沿着接收三角形一路铺出去——
        // 一个弹孔变成横跨整面墙的条纹。
        let decal = decal_at(0.0, 0.0, 2.0);
        let mesh = project(&floor(50.0), Mat4::IDENTITY, &decal).unwrap();

        for vertex in mesh.vertices() {
            let position = Vec3::from_array(vertex.position);
            assert!(
                position.x.abs() <= 1.0 + 1e-3 && position.z.abs() <= 1.0 + 1e-3,
                "顶点 {position:?} 跑出了贴花盒"
            );
        }
    }

    #[test]
    fn uvs_span_the_decal() {
        // UV 不铺满的话贴花图只会显示一角。
        let mesh = project(&floor(50.0), Mat4::IDENTITY, &decal_at(0.0, 0.0, 2.0)).unwrap();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for vertex in mesh.vertices() {
            lo = lo.min(vertex.uv[0]);
            hi = hi.max(vertex.uv[0]);
        }
        assert!(lo < 0.05, "U 的最小值该接近 0，实测 {lo}");
        assert!(hi > 0.95, "U 的最大值该接近 1，实测 {hi}");
    }

    #[test]
    fn uvs_stay_in_range() {
        let mesh = project(&floor(50.0), Mat4::IDENTITY, &decal_at(0.3, -0.7, 1.5)).unwrap();
        for vertex in mesh.vertices() {
            assert!(
                (-1e-3..=1.0 + 1e-3).contains(&vertex.uv[0])
                    && (-1e-3..=1.0 + 1e-3).contains(&vertex.uv[1]),
                "UV 越界：{:?}",
                vertex.uv
            );
        }
    }

    #[test]
    fn back_faces_are_skipped() {
        // 不判的话墙的另一面也会出现弹孔。
        let mut flipped = floor(10.0);
        // 把绕序反过来，法线就朝下了。
        let indices: Vec<u32> = flipped
            .indices()
            .chunks_exact(3)
            .flat_map(|t| [t[0], t[2], t[1]])
            .collect();
        flipped = Mesh::new(flipped.vertices().to_vec(), indices);

        assert!(project(&flipped, Mat4::IDENTITY, &decal_at(0.0, 0.0, 1.0)).is_none());
    }

    #[test]
    fn steep_surfaces_are_skipped() {
        // 墙的侧面（几乎与投影方向平行）被贴上的话，会是一条
        // 拉伸得很长的条纹。
        let decal = Decal::new(
            Vec3::ZERO,
            // 投影方向几乎与地板平行。
            Vec3::new(1.0, 0.05, 0.0).normalize(),
            Vec3::splat(2.0),
            0.0,
        );
        assert!(project(&floor(10.0), Mat4::IDENTITY, &decal).is_none());
    }

    #[test]
    fn the_threshold_can_be_relaxed() {
        let mut decal = Decal::new(
            Vec3::ZERO,
            // 与地板法线的点积约 0.196：低于默认阈值 0.3，高于放宽后的 0.05。
            Vec3::new(1.0, 0.2, 0.0).normalize(),
            Vec3::splat(3.0),
            0.0,
        );
        assert!(project(&floor(10.0), Mat4::IDENTITY, &decal).is_none());

        decal.normal_threshold = 0.05;
        assert!(project(&floor(10.0), Mat4::IDENTITY, &decal).is_some());
    }

    #[test]
    fn geometry_is_lifted_off_the_surface() {
        // 贴花和它贴合的表面在同一个平面上，不抬起来会 z-fighting，
        // 表现为斑驳的闪烁。
        let mut decal = decal_at(0.0, 0.0, 1.0);
        decal.offset = 0.02;
        let mesh = project(&floor(10.0), Mat4::IDENTITY, &decal).unwrap();

        for vertex in mesh.vertices() {
            assert!(
                (vertex.position[1] - 0.02).abs() < 1e-4,
                "顶点该被抬到 0.02，实测 {}",
                vertex.position[1]
            );
        }
    }

    #[test]
    fn the_receiver_transform_is_respected() {
        // 接收物被挪走之后，贴花要跟着挪，不能还贴在原点。
        let moved = Mat4::from_translation(Vec3::new(50.0, 0.0, 0.0));
        assert!(project(&floor(10.0), moved, &decal_at(0.0, 0.0, 1.0)).is_none());
        assert!(project(&floor(10.0), moved, &decal_at(50.0, 0.0, 1.0)).is_some());
    }

    #[test]
    fn normals_point_along_the_surface() {
        let mesh = project(&floor(10.0), Mat4::IDENTITY, &decal_at(0.0, 0.0, 1.0)).unwrap();
        for vertex in mesh.vertices() {
            let normal = Vec3::from_array(vertex.normal);
            assert!((normal - Vec3::Y).length() < 1e-3, "法线是 {normal:?}");
        }
    }

    #[test]
    fn roll_rotates_the_decal() {
        // 一排一模一样的弹孔看着很假。
        let straight = Decal::new(Vec3::ZERO, Vec3::Y, Vec3::splat(2.0), 0.0);
        let rolled = Decal::new(
            Vec3::ZERO,
            Vec3::Y,
            Vec3::splat(2.0),
            std::f32::consts::FRAC_PI_4,
        );
        assert_ne!(straight.transform, rolled.transform);

        // 转过之后仍然贴得上，而且仍在盒子里。
        let mesh = project(&floor(50.0), Mat4::IDENTITY, &rolled).unwrap();
        assert!(!mesh.indices().is_empty());
    }

    #[test]
    fn the_bounds_cover_the_box() {
        let decal = Decal::new(Vec3::new(3.0, 4.0, 5.0), Vec3::Y, Vec3::splat(2.0), 0.0);
        let bounds = decal.bounds();
        assert!(bounds.contains(Vec3::new(3.0, 4.0, 5.0)));
        // 盒子边长 2，所以从中心往外一格该还在里面。
        assert!(bounds.size().max_element() >= 2.0 - 1e-3);
    }

    #[test]
    fn a_decal_larger_than_the_receiver_keeps_the_whole_receiver() {
        let mesh = project(&floor(1.0), Mat4::IDENTITY, &decal_at(0.0, 0.0, 10.0)).unwrap();
        // 地板两个三角形都完整落在贴花体内，不该被裁。
        assert_eq!(mesh.indices().len(), 6);
    }

    #[test]
    fn degenerate_triangles_are_skipped() {
        // 零面积的三角形算不出法线，归一化会得到 NaN。
        let vertices = vec![
            Vertex::new(Vec3::ZERO, Vec3::Y, [0.0, 0.0]),
            Vertex::new(Vec3::ZERO, Vec3::Y, [0.0, 0.0]),
            Vertex::new(Vec3::ZERO, Vec3::Y, [0.0, 0.0]),
        ];
        let mesh = Mesh::new(vertices, vec![0, 1, 2]);
        assert!(project(&mesh, Mat4::IDENTITY, &decal_at(0.0, 0.0, 1.0)).is_none());
    }

    #[test]
    fn no_nan_in_the_output() {
        let mesh = project(&floor(50.0), Mat4::IDENTITY, &decal_at(0.13, -0.77, 1.7)).unwrap();
        for vertex in mesh.vertices() {
            assert!(vertex.position.iter().all(|v| v.is_finite()));
            assert!(vertex.normal.iter().all(|v| v.is_finite()));
            assert!(vertex.uv.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn indices_stay_in_range() {
        let mesh = project(&floor(50.0), Mat4::IDENTITY, &decal_at(0.0, 0.0, 3.0)).unwrap();
        let count = mesh.vertices().len() as u32;
        assert!(mesh.indices().iter().all(|i| *i < count));
    }

    #[test]
    fn clipping_a_fully_inside_triangle_changes_nothing() {
        let triangle = vec![
            Vec3::new(-0.1, -0.1, 0.0),
            Vec3::new(0.1, -0.1, 0.0),
            Vec3::new(0.0, 0.1, 0.0),
        ];
        let clipped = clip_to_unit_cube(&triangle);
        assert_eq!(clipped.len(), 3);
        for (a, b) in clipped.iter().zip(&triangle) {
            assert!((*a - *b).length() < 1e-5);
        }
    }

    #[test]
    fn clipping_a_fully_outside_triangle_yields_nothing() {
        let triangle = vec![
            Vec3::new(5.0, 5.0, 5.0),
            Vec3::new(6.0, 5.0, 5.0),
            Vec3::new(5.0, 6.0, 5.0),
        ];
        assert!(clip_to_unit_cube(&triangle).len() < 3);
    }

    #[test]
    fn clipping_a_straddling_triangle_adds_vertices() {
        // 一进一出时要在交点处插顶点。不插的话裁出来的多边形
        // 会缺一角，贴花边缘出现缺口。
        let triangle = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(0.0, 2.0, 0.0),
        ];
        let clipped = clip_to_unit_cube(&triangle);
        assert!(
            clipped.len() > 3,
            "该在交点处插顶点，实测 {} 个",
            clipped.len()
        );
        for point in &clipped {
            assert!(point.x <= 0.5 + 1e-4 && point.y <= 0.5 + 1e-4);
        }
    }
}
