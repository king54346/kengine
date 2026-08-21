//! 级联阴影（CSM）。
//!
//! # 单张阴影图的问题
//!
//! 一张阴影图要覆盖整个场景。场景一大，每个纹素对应的世界尺寸就变大——
//! 近处的阴影边缘会碎成一格一格的方块。把分辨率翻倍只能拖延，
//! 因为问题的根源是**近处和远处共用同一个采样密度**，而近处占的屏幕
//! 面积远大于远处。
//!
//! # 级联
//!
//! 把相机视锥按距离切成几段，每段单独一张阴影图。近处那段覆盖的世界
//! 范围小，同样分辨率下纹素密度高得多。
//!
//! # 切分点为什么不是等分
//!
//! 屏幕上一个像素对应的世界尺寸随距离**线性**增长，而等分切法让每段
//! 覆盖的距离相同——近处那段被浪费，远处那段仍然不够。
//!
//! 实用的做法是**对数分布**（近处密、远处疏），再和等分按 `lambda`
//! 混合：纯对数在近平面极小时会让第一级退化成一个点。
//! 这个混合公式来自 Parallel-Split Shadow Maps（Zhang et al. 2006），
//! 是业界标准做法。

use kmath::{Aabb, Mat4, Vec3};

/// 最多支持几级。
///
/// 四级足够覆盖到几公里外。再多的话每级的收益迅速下降，
/// 而每级都是一次完整的阴影 pass（多一次场景遍历 + 一张深度图）。
pub const MAX_CASCADES: usize = 4;

/// 级联阴影的参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CascadeSettings {
    /// 级数，会被夹到 `1..=MAX_CASCADES`。
    pub count: usize,
    /// 对数分布与等分的混合系数，0 是纯等分、1 是纯对数。
    ///
    /// 纯对数在近平面很小时会让第一级退化成一个点（近平面 0.01 米时，
    /// 第一级只覆盖几厘米），所以要和等分混。0.75~0.95 是常用范围。
    pub lambda: f32,
    /// 相邻级之间重叠多少（比例）。
    ///
    /// 不重叠的话，两级的交界处会有一条能看出来的硬边——
    /// 两侧的纹素密度不同，阴影边缘的锯齿粗细会突变。
    pub overlap: f32,
    /// 阴影覆盖到多远。超过这个距离不再投影阴影。
    ///
    /// 单独给一个值而不是用相机的远平面：远平面通常是几千米
    /// （为了画天空和远山），而阴影在一两百米外就看不出来了，
    /// 拿远平面去分级会把所有精度浪费在看不见的地方。
    pub max_distance: f32,
}

impl Default for CascadeSettings {
    fn default() -> Self {
        Self {
            count: 3,
            lambda: 0.85,
            overlap: 0.1,
            max_distance: 200.0,
        }
    }
}

/// 一级级联。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cascade {
    /// 这一级覆盖的近距离（沿相机前方，米）。
    pub near: f32,
    /// 覆盖的远距离。
    pub far: f32,
    /// 光空间矩阵（投影 × 视图）。
    pub matrix: Mat4,
}

/// 按 PSSM 公式算出各级的切分距离。
///
/// 返回 `count + 1` 个值：`[near, split1, split2, ..., far]`。
pub fn split_distances(near: f32, far: f32, count: usize, lambda: f32) -> Vec<f32> {
    let count = count.clamp(1, MAX_CASCADES);
    let lambda = lambda.clamp(0.0, 1.0);
    // 近平面必须为正：对数分布要取 `far / near` 的幂，near 为 0 会得到无穷。
    let near = near.max(1e-3);
    let far = far.max(near + 1e-3);

    let mut splits = Vec::with_capacity(count + 1);
    splits.push(near);
    for i in 1..count {
        let ratio = i as f32 / count as f32;
        // 对数分布：近处密、远处疏。
        let logarithmic = near * (far / near).powf(ratio);
        // 等分：每段一样长。
        let uniform = near + (far - near) * ratio;
        splits.push(logarithmic * lambda + uniform * (1.0 - lambda));
    }
    splits.push(far);
    splits
}

/// 算出所有级联。
///
/// - `view_projection` 是相机的 view-projection 矩阵，用来反解视锥；
/// - `light_direction` 是方向光的朝向；
/// - `scene_bounds` 用来把光源沿光轴退到场景之外，保证近处的遮挡物
///   （在视锥切片之外但仍能投影进来的物体）不被裁掉。
pub fn compute(
    view_projection: Mat4,
    light_direction: Vec3,
    scene_bounds: Aabb,
    settings: CascadeSettings,
) -> Vec<Cascade> {
    let count = settings.count.clamp(1, MAX_CASCADES);
    let inverse = view_projection.inverse();

    // 相机的近远平面要从投影矩阵反解出来：调用方给的 `max_distance`
    // 是阴影的覆盖范围，不是相机的实际近平面。
    let Some((camera_near, _)) = near_far_of(&inverse) else {
        return Vec::new();
    };
    let splits = split_distances(camera_near, settings.max_distance, count, settings.lambda);

    let mut cascades = Vec::with_capacity(count);
    for i in 0..count {
        let near = splits[i];
        // 往前借一点，和上一级重叠。不重叠的话交界处会有一条硬边。
        let near = if i == 0 {
            near
        } else {
            near - (near - splits[i - 1]) * settings.overlap.clamp(0.0, 0.5)
        };
        let far = splits[i + 1];

        let corners = slice_corners(&inverse, camera_near, near, far);
        // 用这一段视锥的包围盒当阴影范围，但**深度方向要扩到整个场景**——
        // 切片外面的高楼照样会把影子投进来。
        let mut slice = Aabb::EMPTY;
        for corner in corners {
            slice.expand(corner);
        }
        if slice.is_empty() {
            continue;
        }

        cascades.push(Cascade {
            near,
            far,
            matrix: cascade_matrix(light_direction, slice, scene_bounds),
        });
    }
    cascades
}

/// 从逆 view-projection 反解相机的近远平面距离。
fn near_far_of(inverse: &Mat4) -> Option<(f32, f32)> {
    let unproject = |z: f32| -> Option<Vec3> {
        let clip = *inverse * kmath::Vec4::new(0.0, 0.0, z, 1.0);
        (clip.w.abs() > 1e-9).then(|| clip.truncate() / clip.w)
    };
    // wgpu 的深度范围是 [0, 1]：0 是近平面、1 是远平面。
    let near_point = unproject(0.0)?;
    let far_point = unproject(1.0)?;
    let eye_to_near = near_point;
    let eye_to_far = far_point;
    let distance = (eye_to_far - eye_to_near).length();
    (distance.is_finite() && distance > 0.0).then_some((1e-2, distance))
}

/// 反解出视锥中 `[near, far]` 这一段的八个角（世界坐标）。
fn slice_corners(inverse: &Mat4, camera_near: f32, near: f32, far: f32) -> [Vec3; 8] {
    // 先把整个视锥的八个角解出来，再沿每条棱插值取切片。
    //
    // 直接按深度值插值是**错的**：透视投影的深度不是线性的，
    // 按 NDC 的 z 线性插值会让切片落在完全错误的距离上。
    // 沿棱在世界空间插值才对。
    let mut near_plane = [Vec3::ZERO; 4];
    let mut far_plane = [Vec3::ZERO; 4];
    for (index, (x, y)) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .into_iter()
        .enumerate()
    {
        near_plane[index] = unproject(inverse, x, y, 0.0);
        far_plane[index] = unproject(inverse, x, y, 1.0);
    }

    // 整个视锥的深度跨度，用来把距离换算成插值系数。
    let span = (far_plane[0] - near_plane[0]).length().max(1e-6);
    let t_near = ((near - camera_near) / span).clamp(0.0, 1.0);
    let t_far = ((far - camera_near) / span).clamp(0.0, 1.0);

    let mut corners = [Vec3::ZERO; 8];
    for i in 0..4 {
        let direction = far_plane[i] - near_plane[i];
        corners[i] = near_plane[i] + direction * t_near;
        corners[i + 4] = near_plane[i] + direction * t_far;
    }
    corners
}

fn unproject(inverse: &Mat4, x: f32, y: f32, z: f32) -> Vec3 {
    let clip = *inverse * kmath::Vec4::new(x, y, z, 1.0);
    if clip.w.abs() < 1e-9 {
        return Vec3::ZERO;
    }
    clip.truncate() / clip.w
}

/// 一级级联的光空间矩阵。
///
/// **XY 范围只覆盖这一段视锥，深度范围往光源方向拉长。** 两者必须分开：
///
/// - XY 紧贴切片正是级联的全部意义。把场景尺度并进 XY 的话，
///   每级的正交范围都变成整个场景那么大，纹素密度和单张阴影图一样——
///   级联白做了（我第一版就是这么错的，三级密度分别是
///   0.300 / 0.305 / 0.367 米每纹素，几乎没有区别）。
/// - 深度**必须**拉长：一栋在切片之外的高楼照样会把影子投进切片里，
///   深度范围不够的话它不会被写进深度图，影子就凭空消失了。
fn cascade_matrix(direction: Vec3, slice: Aabb, scene: Aabb) -> Mat4 {
    if slice.is_empty() {
        return Mat4::IDENTITY;
    }
    let direction = direction.normalize_or(Vec3::NEG_Y);
    let center = slice.center();
    // 包围球半径：光源转动时正交范围保持恒定，不会出现分辨率抖动。
    let radius = (slice.size().length() * 0.5).max(1e-3);

    // 深度往回拉多远：够覆盖场景里最高的遮挡物就行。
    // 场景为空时退化成 2 倍半径（和单张阴影图一样的做法）。
    let back = if scene.is_empty() {
        radius * 2.0
    } else {
        (scene.size().length() + radius).max(radius * 2.0)
    };

    let eye = center - direction * back;
    let up = if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let view = Mat4::look_at_rh(eye, center, up);
    // XY 只到 radius——这是密度的来源。
    // 远平面要够到切片背面：back + radius。
    let projection = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.0, back + radius);
    projection * view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Mat4 {
        let projection = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 1000.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 5.0, 20.0), Vec3::ZERO, Vec3::Y);
        projection * view
    }

    fn scene() -> Aabb {
        Aabb::new(Vec3::new(-200.0, -10.0, -200.0), Vec3::new(200.0, 60.0, 200.0))
    }

    fn light() -> Vec3 {
        Vec3::new(-0.4, -1.0, -0.3).normalize()
    }

    #[test]
    fn splits_start_and_end_where_told() {
        let splits = split_distances(0.1, 200.0, 3, 0.85);
        assert_eq!(splits.len(), 4);
        assert!((splits[0] - 0.1).abs() < 1e-4);
        assert!((splits[3] - 200.0).abs() < 1e-3);
    }

    #[test]
    fn splits_increase_monotonically() {
        // 乱序的切分点会让某一级的 near 大于 far，那一级整个作废。
        for lambda in [0.0, 0.5, 0.85, 1.0] {
            let splits = split_distances(0.1, 500.0, 4, lambda);
            for pair in splits.windows(2) {
                assert!(pair[1] > pair[0], "lambda={lambda} 时切分点乱序：{splits:?}");
            }
        }
    }

    #[test]
    fn a_higher_lambda_packs_more_detail_near_the_camera() {
        // 这正是对数分布存在的理由：近处占的屏幕面积远大于远处。
        let uniform = split_distances(1.0, 400.0, 3, 0.0);
        let logarithmic = split_distances(1.0, 400.0, 3, 1.0);
        assert!(
            logarithmic[1] < uniform[1],
            "对数分布的第一级该更近：{} vs {}",
            logarithmic[1],
            uniform[1]
        );
    }

    #[test]
    fn a_tiny_near_plane_does_not_collapse_the_first_cascade() {
        // 纯对数在近平面极小时会让第一级退化成一个点：
        // 近平面 0.001 米时，第一级只覆盖几毫米，等于白搭一级。
        let splits = split_distances(0.001, 300.0, 3, 0.85);
        assert!(
            splits[1] > 0.5,
            "第一级只到 {} 米，太近了",
            splits[1]
        );
    }

    #[test]
    fn a_zero_near_plane_is_handled() {
        // 对数分布要取 `far / near` 的幂，near 为 0 会得到无穷。
        let splits = split_distances(0.0, 100.0, 3, 0.9);
        assert!(splits.iter().all(|s| s.is_finite()), "{splits:?}");
    }

    #[test]
    fn the_count_is_clamped() {
        assert_eq!(split_distances(0.1, 100.0, 0, 0.8).len(), 2, "至少一级");
        assert_eq!(
            split_distances(0.1, 100.0, 99, 0.8).len(),
            MAX_CASCADES + 1,
            "最多 MAX_CASCADES 级"
        );
    }

    #[test]
    fn compute_returns_the_requested_number_of_cascades() {
        for count in 1..=MAX_CASCADES {
            let cascades = compute(
                camera(),
                light(),
                scene(),
                CascadeSettings {
                    count,
                    ..Default::default()
                },
            );
            assert_eq!(cascades.len(), count);
        }
    }

    #[test]
    fn cascades_cover_the_whole_range_without_gaps() {
        // 有缝的话那段距离上的物体完全没有阴影。
        let cascades = compute(camera(), light(), scene(), CascadeSettings::default());
        for pair in cascades.windows(2) {
            assert!(
                pair[1].near <= pair[0].far + 1e-3,
                "第 {} 级到 {} 米，下一级从 {} 米开始，中间有缝",
                0,
                pair[0].far,
                pair[1].near
            );
        }
    }

    #[test]
    fn cascades_overlap_at_their_boundaries() {
        // 不重叠的话交界处有一条硬边：两侧纹素密度不同，
        // 阴影边缘的锯齿粗细会突变。
        let cascades = compute(
            camera(),
            light(),
            scene(),
            CascadeSettings {
                overlap: 0.25,
                ..Default::default()
            },
        );
        for pair in cascades.windows(2) {
            assert!(
                pair[1].near < pair[0].far,
                "第二级从 {} 开始，第一级到 {} 结束，没有重叠",
                pair[1].near,
                pair[0].far
            );
        }
    }

    #[test]
    fn zero_overlap_makes_the_cascades_meet_exactly() {
        let cascades = compute(
            camera(),
            light(),
            scene(),
            CascadeSettings {
                overlap: 0.0,
                ..Default::default()
            },
        );
        for pair in cascades.windows(2) {
            assert!((pair[1].near - pair[0].far).abs() < 1e-3);
        }
    }

    #[test]
    fn nearer_cascades_cover_less_ground() {
        // 级联的全部意义：近处那级覆盖的世界范围小，
        // 同样分辨率下纹素密度高得多。
        let cascades = compute(camera(), light(), scene(), CascadeSettings::default());
        assert!(cascades.len() >= 2);

        // 用光空间里单位世界长度对应多少 NDC 来衡量密度。
        let density = |cascade: &Cascade| -> f32 {
            let a = cascade.matrix * Vec3::ZERO.extend(1.0);
            let b = cascade.matrix * Vec3::X.extend(1.0);
            (b.truncate() / b.w - a.truncate() / a.w).length()
        };
        // **要求量级上的差距**，不是「大一点点」。
        //
        // 第一版这里只写了 `>`，于是 0.300 vs 0.305 米每纹素也算通过——
        // 而那时级联其实完全没起作用（XY 范围被场景尺度撑大了，
        // 三级的密度几乎一样）。这条断言就是为了拦住那种情况。
        let ratio = density(&cascades[0]) / density(&cascades[1]);
        assert!(
            ratio > 2.0,
            "近处那级的密度只有远处的 {ratio:.2} 倍，级联等于没做"
        );
    }

    #[test]
    fn every_cascade_matrix_is_finite() {
        // NaN 矩阵会让整张阴影图变黑，而且不报错。
        for direction in [
            Vec3::NEG_Y,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(-0.3, -0.9, 0.2).normalize(),
            // 几乎与上方向共线，`look_at` 容易退化。
            Vec3::new(0.001, -1.0, 0.001).normalize(),
        ] {
            let cascades = compute(camera(), direction, scene(), CascadeSettings::default());
            for cascade in &cascades {
                assert!(
                    cascade.matrix.to_cols_array().iter().all(|v| v.is_finite()),
                    "光照方向 {direction:?} 下矩阵有 NaN"
                );
            }
        }
    }

    #[test]
    fn occluders_outside_the_slice_stay_inside_the_depth_range() {
        // 一栋在切片之外的高楼照样会把影子投进切片里。深度范围不扩的话
        // 它不会被写进深度图，影子就凭空消失了。
        let cascades = compute(camera(), light(), scene(), CascadeSettings::default());
        let cascade = &cascades[0];

        // 在第一级切片的正上方远处放一个遮挡物。
        let occluder = Vec3::new(0.0, 55.0, 15.0);
        let clip = cascade.matrix * occluder.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(
            (0.0..=1.0).contains(&ndc.z),
            "高处的遮挡物落在深度范围之外：{}",
            ndc.z
        );
    }

    #[test]
    fn a_degenerate_camera_produces_no_cascades() {
        // 不可逆的矩阵会解出 NaN 的视锥角点。
        let cascades = compute(Mat4::ZERO, light(), scene(), CascadeSettings::default());
        assert!(cascades.is_empty());
    }

    #[test]
    fn an_empty_scene_still_produces_usable_matrices() {
        let cascades = compute(camera(), light(), Aabb::EMPTY, CascadeSettings::default());
        for cascade in &cascades {
            assert!(cascade.matrix.to_cols_array().iter().all(|v| v.is_finite()));
        }
    }
}
