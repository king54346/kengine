//! 九宫格：一张小图拉成任意大小的边框。
//!
//! # 为什么不能直接拉伸
//!
//! 一个 32×32 的对话框贴图拉成 400×200，圆角会被拉成椭圆、边框会变粗、
//! 花纹会糊。九宫格把图切成九块：**四个角不缩放**，
//! **上下两条只横向拉**，**左右两条只纵向拉**，**中间双向拉**。
//!
//! # 边距是像素，不是比例
//!
//! 边距（[`Slices`]）用源图的**像素**表示。用比例的话，同一套边距
//! 换一张不同尺寸的贴图就得重算，而美术给的边框宽度本来就是像素数。

use crate::SpriteRegion;
use kmath::{Vec2, Vec3, Vec4};
use kmesh::{Mesh, Vertex};

/// 九宫格的四条切割线，单位是源图的**像素**。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Slices {
    /// 左边距。
    pub left: f32,
    /// 右边距。
    pub right: f32,
    /// 上边距。
    pub top: f32,
    /// 下边距。
    pub bottom: f32,
}

impl Slices {
    /// 四边相同。
    pub fn all(value: f32) -> Self {
        Self {
            left: value,
            right: value,
            top: value,
            bottom: value,
        }
    }

    /// 四条边距加起来会不会超过给定的尺寸。
    ///
    /// 超过时角落会互相重叠，画出来是一团乱。
    fn overflows(&self, size: Vec2) -> bool {
        self.left + self.right > size.x || self.top + self.bottom > size.y
    }

    /// 按比例缩小，直到塞得进 `size`。
    ///
    /// 目标框比边框本身还小时会走到这里（比如一个 8 像素高的进度条用了
    /// 12 像素边距的九宫格）。不缩的话四个角会互相穿透。
    fn fit(self, size: Vec2) -> Self {
        let mut result = self;
        let horizontal = self.left + self.right;
        if horizontal > size.x && horizontal > 0.0 {
            let scale = size.x / horizontal;
            result.left *= scale;
            result.right *= scale;
        }
        let vertical = self.top + self.bottom;
        if vertical > size.y && vertical > 0.0 {
            let scale = size.y / vertical;
            result.top *= scale;
            result.bottom *= scale;
        }
        result
    }
}

/// 生成一个九宫格方片。
///
/// - `rect` 是目标矩形 `(左下, 右上)`，世界坐标；
/// - `region` 是这张图在图集里的区域；
/// - `texture_size` 是**整张图集**的像素尺寸（换算边距要用）；
/// - `slices` 是四条切割线，单位是源图像素。
///
/// 中间那块会被拉伸。想要平铺而不是拉伸的话得改成重复采样，
/// 那需要单独的绘制调用（UV 超出 0..1），这里不做。
pub fn build(
    rect: (Vec2, Vec2),
    region: SpriteRegion,
    texture_size: Vec2,
    slices: Slices,
    color: Vec4,
) -> Mesh {
    let (min, max) = rect;
    let size = (max - min).max(Vec2::ZERO);
    if size.x <= 0.0 || size.y <= 0.0 {
        return Mesh::new(Vec::new(), Vec::new());
    }

    // 边距超过目标尺寸时按比例缩小，否则四个角会互相穿透。
    let slices = if slices.overflows(size) {
        slices.fit(size)
    } else {
        slices
    };

    // 源图上四条线的 UV 位置。边距是源图像素，要除以整张图集的尺寸。
    let left_uv = slices.left / texture_size.x;
    let right_uv = slices.right / texture_size.x;
    let top_uv = slices.top / texture_size.y;
    let bottom_uv = slices.bottom / texture_size.y;

    // 四条线在目标矩形上的位置（世界坐标）与在源图上的位置（UV）。
    // 注意 V 轴：图集的 V 向下，世界的 Y 向上，所以上下是反的。
    let xs = [min.x, min.x + slices.left, max.x - slices.right, max.x];
    let ys = [min.y, min.y + slices.bottom, max.y - slices.top, max.y];
    let us = [
        region.min.x,
        region.min.x + left_uv,
        region.max.x - right_uv,
        region.max.x,
    ];
    let vs = [
        region.max.y,
        region.max.y - bottom_uv,
        region.min.y + top_uv,
        region.min.y,
    ];

    let mut vertices = Vec::with_capacity(16);
    for row in 0..4 {
        for col in 0..4 {
            vertices.push(
                Vertex::new(
                    Vec3::new(xs[col], ys[row], 0.0),
                    Vec3::Z,
                    [us[col], vs[row]],
                )
                .with_color(color.truncate()),
            );
        }
    }

    // 4×4 个顶点连成 3×3 = 9 块。
    let mut indices = Vec::with_capacity(9 * 6);
    for row in 0..3u32 {
        for col in 0..3u32 {
            let a = row * 4 + col;
            let (b, c, d) = (a + 1, a + 4, a + 5);
            // 逆时针为正面（从 +Z 看）。
            indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }

    Mesh::new(vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> SpriteRegion {
        // 整张 64×64 的图。
        SpriteRegion::new(Vec2::ZERO, Vec2::ONE)
    }

    fn size() -> Vec2 {
        Vec2::splat(64.0)
    }

    fn build_box(w: f32, h: f32, margin: f32) -> Mesh {
        build(
            (Vec2::ZERO, Vec2::new(w, h)),
            region(),
            size(),
            Slices::all(margin),
            Vec4::ONE,
        )
    }

    /// 按 x、y 取出所有顶点的坐标，去重后排序。
    fn axis(mesh: &Mesh, index: usize) -> Vec<f32> {
        let mut values: Vec<f32> = mesh.vertices().iter().map(|v| v.position[index]).collect();
        values.sort_by(f32::total_cmp);
        values.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        values
    }

    #[test]
    fn a_nine_slice_has_sixteen_vertices_and_nine_quads() {
        let mesh = build_box(200.0, 100.0, 8.0);
        assert_eq!(mesh.vertices().len(), 16);
        assert_eq!(mesh.indices().len(), 9 * 6);
    }

    #[test]
    fn the_corners_keep_their_source_size() {
        // 直接拉伸的话圆角会被拉成椭圆、边框会变粗。九宫格的意义
        // 就在于四个角**不缩放**。
        let mesh = build_box(400.0, 200.0, 12.0);
        let xs = axis(&mesh, 0);
        let ys = axis(&mesh, 1);

        assert_eq!(xs.len(), 4);
        assert!((xs[1] - xs[0] - 12.0).abs() < 1e-4, "左边距该是 12");
        assert!((xs[3] - xs[2] - 12.0).abs() < 1e-4, "右边距该是 12");
        assert!((ys[1] - ys[0] - 12.0).abs() < 1e-4, "下边距该是 12");
        assert!((ys[3] - ys[2] - 12.0).abs() < 1e-4, "上边距该是 12");
    }

    #[test]
    fn only_the_middle_stretches() {
        let small = build_box(100.0, 100.0, 10.0);
        let large = build_box(400.0, 100.0, 10.0);

        let small_middle = axis(&small, 0)[2] - axis(&small, 0)[1];
        let large_middle = axis(&large, 0)[2] - axis(&large, 0)[1];
        assert!(
            (large_middle - small_middle - 300.0).abs() < 1e-3,
            "变宽的部分该全落在中间那一列"
        );
    }

    #[test]
    fn the_mesh_covers_exactly_the_target_rect() {
        let mesh = build(
            (Vec2::new(30.0, 20.0), Vec2::new(230.0, 120.0)),
            region(),
            size(),
            Slices::all(9.0),
            Vec4::ONE,
        );
        let xs = axis(&mesh, 0);
        let ys = axis(&mesh, 1);
        assert_eq!((xs[0], xs[3]), (30.0, 230.0));
        assert_eq!((ys[0], ys[3]), (20.0, 120.0));
    }

    #[test]
    fn margins_shrink_instead_of_overlapping() {
        // 目标框比边框本身还小时（8 像素高的进度条用了 12 像素边距），
        // 不缩的话四个角会互相穿透，画出来一团乱。
        let mesh = build_box(200.0, 8.0, 12.0);
        let ys = axis(&mesh, 1);

        // 四条线该单调不减，而且都落在 0..=8 之间。
        for pair in ys.windows(2) {
            assert!(pair[1] >= pair[0] - 1e-4, "切割线交叉了：{ys:?}");
        }
        assert!(ys[0] >= -1e-4 && ys[ys.len() - 1] <= 8.0 + 1e-4);
    }

    #[test]
    fn a_degenerate_rect_builds_nothing() {
        assert!(build_box(0.0, 100.0, 4.0).indices().is_empty());
        assert!(build_box(100.0, -5.0, 4.0).indices().is_empty());
    }

    #[test]
    fn zero_margins_degenerate_to_a_plain_quad() {
        // 边距为 0 时九块里有八块是零面积，中间那块铺满整个矩形。
        let mesh = build_box(100.0, 50.0, 0.0);
        let xs = axis(&mesh, 0);
        assert_eq!(xs, vec![0.0, 100.0], "四条线该重合成两条");
    }

    #[test]
    fn the_v_axis_is_flipped() {
        // 图集的 V 向下、世界的 Y 向上。不翻的话整个框上下颠倒，
        // 圆角会跑到错误的角上。
        let mesh = build_box(200.0, 100.0, 10.0);
        let bottom = mesh
            .vertices()
            .iter()
            .min_by(|a, b| a.position[1].total_cmp(&b.position[1]))
            .unwrap();
        let top = mesh
            .vertices()
            .iter()
            .max_by(|a, b| a.position[1].total_cmp(&b.position[1]))
            .unwrap();
        assert!(bottom.uv[1] > top.uv[1], "V 没翻转");
    }

    #[test]
    fn uvs_stay_inside_the_region() {
        // 越界的话会采到图集里的邻居，边框上带一道别的图。
        let mesh = build(
            (Vec2::ZERO, Vec2::new(300.0, 150.0)),
            SpriteRegion::new(Vec2::new(0.25, 0.25), Vec2::new(0.5, 0.5)),
            size(),
            Slices::all(6.0),
            Vec4::ONE,
        );
        for v in mesh.vertices() {
            assert!(
                (0.25 - 1e-4..=0.5 + 1e-4).contains(&v.uv[0]),
                "U 越界：{}",
                v.uv[0]
            );
            assert!((0.25 - 1e-4..=0.5 + 1e-4).contains(&v.uv[1]));
        }
    }

    #[test]
    fn winding_faces_the_camera() {
        let mesh = build_box(200.0, 100.0, 10.0);
        let v = mesh.vertices();
        for tri in mesh.indices().chunks(3) {
            let a = Vec3::from_array(v[tri[0] as usize].position);
            let b = Vec3::from_array(v[tri[1] as usize].position);
            let c = Vec3::from_array(v[tri[2] as usize].position);
            let normal = (b - a).cross(c - a);
            // 零面积的块（边距为 0 时）法线是零向量，跳过。
            if normal.length_squared() > 1e-9 {
                assert!(normal.z > 0.0, "三角形朝后了");
            }
        }
    }

    #[test]
    fn asymmetric_margins_are_respected() {
        let mesh = build(
            (Vec2::ZERO, Vec2::new(200.0, 100.0)),
            region(),
            size(),
            Slices {
                left: 4.0,
                right: 20.0,
                top: 8.0,
                bottom: 2.0,
            },
            Vec4::ONE,
        );
        let xs = axis(&mesh, 0);
        let ys = axis(&mesh, 1);
        assert!((xs[1] - 4.0).abs() < 1e-4);
        assert!((xs[2] - 180.0).abs() < 1e-4);
        assert!((ys[1] - 2.0).abs() < 1e-4);
        assert!((ys[2] - 92.0).abs() < 1e-4);
    }
}
