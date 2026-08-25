//! 阴影贴图所需的光空间变换。
//!
//! 从光源视角把场景渲染成一张深度图，主 pass 再用它判断像素是否被遮挡。
//! 这里只负责算出「光空间矩阵」——纯数学，因而可以在没有 GPU 的情况下测试。

use kmath::{Aabb, Mat4, Vec3};

/// 阴影贴图的配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowSettings {
    /// 阴影贴图边长（像素）。越大越清晰，显存与带宽开销也越大。
    pub resolution: u32,
    /// 深度偏移，**世界单位（米）**。用于消除自阴影产生的条纹（阴影痤疮）。
    ///
    /// # 为什么是米而不是归一化深度
    ///
    /// 归一化深度里的同一个数字，换算成世界距离会随级联的深度范围漂移，
    /// 而那个范围又随**场景大小**走。实测过一次：把地面从 10×10 换成
    /// 100×100，偏移从 2 厘米涨到 22 厘米，角色的脚和小腿的阴影直接没了
    /// （躯干离地一米多，还留着）。
    ///
    /// 用米之后，同一个数字在任何场景里都是同一段距离。
    pub depth_bias: f32,
    /// 沿法线方向的偏移量，比纯深度偏移更不容易产生「漏光」。
    pub normal_bias: f32,
    /// 一个物体在某级级联里投影小于多少个纹素时就不画。
    ///
    /// 级联的主要代价是每级一次场景遍历。远处那几级覆盖几百米，
    /// 一个小物件投出的影子连一个纹素都占不到，画它纯属浪费。
    ///
    /// 设为 0 关掉。调大会让小物件在远处**先丢影子再丢自己**——
    /// 2 个纹素基本看不出来，超过 4 就开始能注意到了。
    pub min_shadow_texels: f32,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            resolution: 2048,
            // 2 厘米：够压住痤疮，又不至于让贴地的东西丢掉接触阴影。
            depth_bias: 0.02,
            normal_bias: 0.02,
            min_shadow_texels: 2.0,
        }
    }
}

/// 计算方向光的光空间矩阵（投影 × 视图）。
///
/// `bounds` 是需要产生阴影的场景范围。用**包围球**而非包围盒的八个角来定尺寸，
/// 这样光源旋转时阴影贴图覆盖的范围保持恒定，不会出现分辨率忽大忽小的抖动。
pub fn directional_light_matrix(light_direction: Vec3, bounds: Aabb) -> Mat4 {
    if bounds.is_empty() {
        return Mat4::IDENTITY;
    }

    let direction = light_direction.normalize_or(Vec3::NEG_Y);
    let center = bounds.center();
    // 包围球半径：对角线的一半，保证任意朝向都能覆盖。
    let radius = (bounds.size().length() * 0.5).max(1e-3);

    // 光源沿 -direction 退到球外，近平面留在球前。
    let eye = center - direction * radius * 2.0;
    let up = pick_up_vector(direction);

    let view = Mat4::look_at_rh(eye, center, up);
    let projection = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.0, radius * 4.0);

    projection * view
}

/// 选一个不与光照方向共线的上方向，避免 `look_at` 退化。
fn pick_up_vector(direction: Vec3) -> Vec3 {
    if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// 把世界坐标变换到光空间的归一化设备坐标。
    fn to_ndc(matrix: Mat4, point: Vec3) -> Vec3 {
        let clip = matrix * point.extend(1.0);
        clip.truncate() / clip.w
    }

    fn unit_scene() -> Aabb {
        Aabb::new(Vec3::splat(-5.0), Vec3::splat(5.0))
    }

    #[test]
    fn every_scene_corner_lands_inside_the_shadow_map() {
        // 场景任意一角都必须落在光空间的可视范围内，
        // 否则那部分物体不会写入深度图，阴影就会缺一块。
        let bounds = unit_scene();

        for direction in [
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(-1.0, -1.0, -1.0).normalize(),
            Vec3::new(0.3, -0.8, 0.5).normalize(),
            Vec3::new(1.0, -0.1, 0.0).normalize(),
        ] {
            let matrix = directional_light_matrix(direction, bounds);

            for corner in bounds.corners() {
                let ndc = to_ndc(matrix, corner);

                assert!(
                    (-1.0..=1.0).contains(&ndc.x) && (-1.0..=1.0).contains(&ndc.y),
                    "角点 {corner:?} 在光照方向 {direction:?} 下越出了 XY 范围：{ndc:?}"
                );
                // wgpu 的深度范围是 [0, 1]。
                assert!(
                    (0.0..=1.0).contains(&ndc.z),
                    "角点 {corner:?} 的深度越界：{}",
                    ndc.z
                );
            }
        }
    }

    #[test]
    fn straight_down_light_does_not_degenerate() {
        // 光照方向与默认上方向共线时 look_at 会退化成 NaN。
        let matrix = directional_light_matrix(Vec3::NEG_Y, unit_scene());

        assert!(matrix.is_finite(), "垂直向下的光产生了非有限矩阵");
        assert!(to_ndc(matrix, Vec3::ZERO).is_finite());
    }

    #[test]
    fn straight_up_light_does_not_degenerate() {
        let matrix = directional_light_matrix(Vec3::Y, unit_scene());

        assert!(matrix.is_finite());
    }

    #[test]
    fn zero_direction_falls_back_instead_of_producing_nan() {
        // 零向量归一化会得到 NaN，必须有兜底。
        let matrix = directional_light_matrix(Vec3::ZERO, unit_scene());

        assert!(matrix.is_finite());
    }

    #[test]
    fn empty_bounds_yields_identity() {
        let matrix = directional_light_matrix(Vec3::NEG_Y, Aabb::EMPTY);

        assert_eq!(matrix, Mat4::IDENTITY);
    }

    #[test]
    fn degenerate_bounds_do_not_divide_by_zero() {
        // 场景只有一个点时半径为 0，正交投影会退化。
        let point = Aabb::new(Vec3::ZERO, Vec3::ZERO);
        let matrix = directional_light_matrix(Vec3::NEG_Y, point);

        assert!(matrix.is_finite());
    }

    #[test]
    fn bounding_sphere_exactly_fills_the_projection() {
        // 用包围球定尺寸的意义：无论光源怎么转，球面恰好贴住投影边界，
        // 覆盖范围因而恒定；换成包围盒八角点会让范围随角度伸缩，
        // 画面上表现为阴影分辨率忽高忽低、边缘抖动。
        let bounds = unit_scene();
        let center = bounds.center();
        let radius = bounds.size().length() * 0.5;

        for i in 0..8 {
            let angle = i as f32 / 8.0 * std::f32::consts::TAU;
            let direction = Vec3::new(angle.cos(), -1.0, angle.sin()).normalize();
            let matrix = directional_light_matrix(direction, bounds);

            // 光空间里的「右」方向，垂直于光照方向。
            let right = direction.cross(pick_up_vector(direction)).normalize();
            let edge = to_ndc(matrix, center + right * radius);

            assert!(
                (edge.x.abs() - 1.0).abs() < 1e-3,
                "包围球边缘未贴合投影边界：{} （方向 {direction:?}）",
                edge.x
            );
        }
    }

    #[test]
    fn settings_have_sensible_defaults() {
        let settings = ShadowSettings::default();

        assert!(settings.resolution.is_power_of_two());
        // 偏移必须为正，否则不但消不掉痤疮反而会加重。
        assert!(settings.depth_bias > 0.0);
        assert!(settings.normal_bias > 0.0);
    }
}
