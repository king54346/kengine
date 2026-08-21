//! 反射探针：局部的环境反射。
//!
//! # 为什么需要它
//!
//! 全局环境贴图假设环境在**无穷远**——采样时只用反射方向，不管着色点
//! 在哪。室外这没问题（天空确实很远），室内就完全错了：站在房间里的
//! 金属球会反射出天空，而不是墙。
//!
//! 反射探针把环境**绑到一个位置和一个盒子上**：
//!
//! - 位置：环境是从哪儿采集的
//! - 盒子：环境大致有多大（房间的形状）
//!
//! 有了盒子就能做**视差校正**：把反射射线和盒子求交，用交点而不是
//! 方向去采样。墙上的反射这才落在墙该在的地方。
//!
//! # 局限
//!
//! 视差盒是个**长方体**。房间越不像长方体，校正得越差——L 形的房间
//! 用一个盒子罩住的话，拐角处的反射会拉伸。办法是放多个探针，
//! 每个罩住一段。

use kmath::{Aabb, Vec3};

/// 一个反射探针。
///
/// 只有参数，不含像素。像素（预滤波的 mip 链）由上层管理，
/// 因为它们要拼成一张 GPU 纹理数组。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflectionProbe {
    /// 采集点：环境是站在哪儿采的。
    ///
    /// 视差校正要用它——校正后的方向是「从采集点看向交点」。
    pub position: Vec3,
    /// 视差盒，同时也是影响范围。
    ///
    /// 用同一个盒子而不是分开两个：影响范围比视差盒大的话，
    /// 盒子外面的物体会拿到一个对它来说毫无意义的校正。
    /// 想让影响范围更大就把盒子做大，代价是校正变糙。
    pub bounds: Aabb,
    /// 是否做视差校正。
    ///
    /// 关掉时探针退化成一张普通的无穷远环境图——户外的天空探针
    /// 就该这样，给天空套个盒子只会让远处的云跟着相机动。
    pub parallax: bool,
    /// 反射强度的缩放。
    pub intensity: f32,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            bounds: Aabb::new(Vec3::splat(-5.0), Vec3::splat(5.0)),
            parallax: true,
            intensity: 1.0,
        }
    }
}

impl ReflectionProbe {
    /// 以 `position` 为中心、边长 `size` 的探针。
    pub fn new(position: Vec3, size: Vec3) -> Self {
        Self {
            position,
            bounds: Aabb::from_center_half_extents(position, size * 0.5),
            ..Default::default()
        }
    }

    /// 一个不做视差校正的探针，用于户外。
    pub fn distant(position: Vec3) -> Self {
        Self {
            position,
            parallax: false,
            ..Default::default()
        }
    }

    /// 这个探针管不管 `point`。
    pub fn affects(&self, point: Vec3) -> bool {
        self.bounds.contains(point)
    }

    /// 盒子的体积。选探针时用来比谁更「贴身」。
    pub fn volume(&self) -> f32 {
        let size = self.bounds.size();
        size.x * size.y * size.z
    }

    /// 视差校正：把反射方向换成「从采集点看向反射射线与盒子的交点」。
    ///
    /// `position` 是着色点，`reflection` 是反射方向（要求已归一化）。
    ///
    /// 关掉视差时原样返回——户外的天空不该被套上盒子。
    pub fn correct(&self, position: Vec3, reflection: Vec3) -> Vec3 {
        if !self.parallax {
            return reflection;
        }

        // 射线与盒子求交，取**出射**的那个 t：着色点在盒子里，
        // 所以射线一定是从内部往外打，只有一个正交点。
        let Some(distance) = exit_distance(position, reflection, self.bounds) else {
            // 射线打不到盒子（着色点在盒外，或方向退化），
            // 退回未校正的方向。总比返回一个乱数好。
            return reflection;
        };

        let hit = position + reflection * distance;
        // 采集点和交点重合时方向没有定义（着色点正好在采集点上，
        // 而且反射方向指向零距离）。这时用原方向。
        (hit - self.position).normalize_or(reflection)
    }
}

/// 射线从盒子内部射出时，到边界的距离。
///
/// 标准 slab 法：每个轴算出与两个面的 t，取较大的那个（出射面），
/// 三个轴的出射 t 取**最小值**。取最大的话射线会「穿过」最近的那面墙，
/// 反射落在盒子外面。
///
/// 返回 [`None`] 表示没有正的出射距离。
///
/// # 和着色器保持一致
///
/// `ibl.wgsl` 里的 `parallax_correct` 用的是同一套公式。那边靠
/// 「除以零得 inf、inf 参与 min 会被忽略」隐式跳过零方向的轴，
/// 这边显式跳过——结果相同，但 Rust 里显式写出来更清楚。
///
/// 注意这个公式对**盒外**的原点算出的是出射点，不是入射点。
/// 探针只会在物体中心落在盒内时被选中，所以正常路径上原点总在盒内；
/// 大物体的边缘片元可能落在盒外，那时校正会退化，但不会产生乱数。
fn exit_distance(origin: Vec3, direction: Vec3, bounds: Aabb) -> Option<f32> {
    let mut nearest = f32::INFINITY;

    for axis in 0..3 {
        let d = direction[axis];
        // 方向在这个轴上几乎为零时，射线平行于这对面，永远不会
        // 在这个轴上出去。直接跳过——除以它会得到 inf 或 NaN。
        if d.abs() < 1e-6 {
            continue;
        }
        let inverse = 1.0 / d;
        let to_min = (bounds.min[axis] - origin[axis]) * inverse;
        let to_max = (bounds.max[axis] - origin[axis]) * inverse;
        nearest = nearest.min(to_min.max(to_max));
    }

    (nearest.is_finite() && nearest > 0.0).then_some(nearest)
}

/// 从一堆探针里挑出管 `point` 的那个，返回它在切片中的下标。
///
/// 有多个探针罩住同一个点时，**盒子最小的赢**：小盒子通常意味着
/// 更贴身的采集（一个房间的探针 vs 罩住整栋楼的探针），校正更准。
///
/// 没有探针罩住时返回 [`None`]，调用方退回全局环境。
pub fn select(probes: &[ReflectionProbe], point: Vec3) -> Option<usize> {
    probes
        .iter()
        .enumerate()
        .filter(|(_, probe)| probe.affects(point))
        // `total_cmp` 而不是 `partial_cmp().unwrap()`：退化的盒子
        // 会算出 NaN 体积，unwrap 会崩。
        .min_by(|(_, a), (_, b)| a.volume().total_cmp(&b.volume()))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个边长 10、中心在原点的探针。
    fn unit_probe() -> ReflectionProbe {
        ReflectionProbe::new(Vec3::ZERO, Vec3::splat(10.0))
    }

    #[test]
    fn a_probe_covers_its_box() {
        let probe = unit_probe();
        assert!(probe.affects(Vec3::ZERO));
        assert!(probe.affects(Vec3::new(4.9, 4.9, 4.9)));
        assert!(!probe.affects(Vec3::new(5.1, 0.0, 0.0)));
    }

    #[test]
    fn parallax_bends_the_reflection_toward_the_wall() {
        // 这是整个特性存在的理由：着色点偏离采集点时，
        // 校正后的方向必须和原方向不同，否则反射还是「无穷远」的。
        let probe = unit_probe();
        let position = Vec3::new(4.0, 0.0, 0.0);
        let reflection = Vec3::Y;

        let corrected = probe.correct(position, reflection);
        assert!(
            (corrected - reflection).length() > 0.1,
            "方向没被校正：{corrected:?}"
        );
        // 交点是 (4, 5, 0)，从采集点（原点）看过去该同时有 x 和 y 分量。
        assert!(corrected.x > 0.0 && corrected.y > 0.0);
    }

    #[test]
    fn a_reflection_from_the_capture_point_is_unchanged() {
        // 着色点正好在采集点上时，交点方向就是原方向——
        // 这时探针退化成普通环境图，正好是它该有的行为。
        let probe = unit_probe();
        for direction in [Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(1.0, 2.0, 3.0).normalize()] {
            let corrected = probe.correct(Vec3::ZERO, direction);
            assert!(
                (corrected - direction).length() < 1e-5,
                "{direction:?} 被改成了 {corrected:?}"
            );
        }
    }

    #[test]
    fn parallax_can_be_turned_off() {
        // 给天空套盒子的话，远处的云会跟着相机动。
        let mut probe = unit_probe();
        probe.parallax = false;
        let position = Vec3::new(4.0, 0.0, 0.0);
        assert_eq!(probe.correct(position, Vec3::Y), Vec3::Y);
    }

    #[test]
    fn the_corrected_direction_is_normalized() {
        // 着色器直接拿它去采样，没归一化的话粗糙度对应的 mip 会算错。
        let probe = unit_probe();
        for position in [
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(-3.0, 2.0, 1.0),
            Vec3::new(0.0, 4.9, 0.0),
        ] {
            let corrected = probe.correct(position, Vec3::new(1.0, 1.0, 1.0).normalize());
            assert!(
                (corrected.length() - 1.0).abs() < 1e-4,
                "长度是 {}",
                corrected.length()
            );
        }
    }

    #[test]
    fn the_corrected_direction_is_always_finite() {
        // 退化输入不能产生 NaN——NaN 采样出来是黑洞，而且会顺着
        // Bloom 扩散到整个画面。
        let probe = unit_probe();
        let cases = [
            (Vec3::ZERO, Vec3::ZERO),
            (Vec3::new(100.0, 0.0, 0.0), Vec3::Y),
            (Vec3::new(5.0, 5.0, 5.0), Vec3::new(1.0, 0.0, 0.0)),
            (Vec3::ZERO, Vec3::new(1e-9, 0.0, 0.0)),
        ];
        for (position, reflection) in cases {
            let corrected = probe.correct(position, reflection);
            assert!(
                corrected.is_finite(),
                "position={position:?} reflection={reflection:?} 得到 {corrected:?}"
            );
        }
    }

    #[test]
    fn a_ray_parallel_to_a_face_still_exits() {
        // 方向在某个轴上为零时，那个轴的 t 是 inf。按 inf 参与
        // min 会污染结果，所以要跳过那个轴而不是让它参与。
        let probe = unit_probe();
        let corrected = probe.correct(Vec3::new(0.0, 4.0, 0.0), Vec3::X);
        assert!(corrected.is_finite());
        // 交点是 (5, 4, 0)，方向该主要朝 +x 且带正 y。
        assert!(corrected.x > 0.5 && corrected.y > 0.0);
    }

    #[test]
    fn exit_distance_finds_the_nearest_face() {
        // 取最小的出射 t：取最大的话射线会「穿过」最近的那面墙，
        // 反射落在盒子外面。
        let bounds = Aabb::new(Vec3::splat(-5.0), Vec3::splat(5.0));
        // 从原点朝 +x，最近的面是 x=5，距离 5。
        assert!((exit_distance(Vec3::ZERO, Vec3::X, bounds).unwrap() - 5.0).abs() < 1e-5);
        // 从 (4,0,0) 朝 +x，距离 1——不是 y 或 z 轴的 5。
        let d = exit_distance(Vec3::new(4.0, 0.0, 0.0), Vec3::X, bounds).unwrap();
        assert!((d - 1.0).abs() < 1e-5, "实测 {d}");
    }

    #[test]
    fn exit_distance_from_outside_is_rejected_or_positive() {
        // 盒外的原点算出的是出射点而不是入射点。探针只在物体中心
        // 落在盒内时才被选中，所以这是边缘情况——要求它不产生
        // 负数或 NaN 就够了，不要求「正确」。
        let bounds = Aabb::new(Vec3::splat(-5.0), Vec3::splat(5.0));
        // 从盒子外面朝远离盒子的方向：没有正的出射距离。
        assert!(exit_distance(Vec3::new(50.0, 0.0, 0.0), Vec3::X, bounds).is_none());
        // 朝着盒子：有正的出射距离。
        let d = exit_distance(Vec3::new(50.0, 0.0, 0.0), -Vec3::X, bounds).unwrap();
        assert!(d > 0.0 && d.is_finite());
    }

    #[test]
    fn exit_distance_rejects_a_degenerate_direction() {
        let bounds = Aabb::new(Vec3::splat(-5.0), Vec3::splat(5.0));
        assert!(exit_distance(Vec3::ZERO, Vec3::ZERO, bounds).is_none());
    }

    #[test]
    fn the_smallest_probe_wins() {
        // 一个房间的探针罩在一栋楼的探针里面时，该用房间那个——
        // 小盒子意味着更贴身的采集。
        let probes = [
            ReflectionProbe::new(Vec3::ZERO, Vec3::splat(100.0)),
            ReflectionProbe::new(Vec3::ZERO, Vec3::splat(10.0)),
            ReflectionProbe::new(Vec3::ZERO, Vec3::splat(50.0)),
        ];
        assert_eq!(select(&probes, Vec3::ZERO), Some(1));
    }

    #[test]
    fn selection_ignores_probes_that_do_not_cover_the_point() {
        let probes = [
            // 更小，但罩不到查询点。
            ReflectionProbe::new(Vec3::new(100.0, 0.0, 0.0), Vec3::splat(2.0)),
            ReflectionProbe::new(Vec3::ZERO, Vec3::splat(20.0)),
        ];
        assert_eq!(select(&probes, Vec3::ZERO), Some(1));
    }

    #[test]
    fn selection_returns_none_when_nothing_covers_the_point() {
        // 调用方据此退回全局环境。
        let probes = [ReflectionProbe::new(Vec3::ZERO, Vec3::splat(2.0))];
        assert_eq!(select(&probes, Vec3::new(50.0, 0.0, 0.0)), None);
        assert_eq!(select(&[], Vec3::ZERO), None);
    }

    #[test]
    fn selection_survives_a_degenerate_probe() {
        // 零尺寸的盒子体积是 0，NaN 体积会让 `partial_cmp().unwrap()` 崩。
        let mut broken = ReflectionProbe::new(Vec3::ZERO, Vec3::splat(10.0));
        broken.bounds = Aabb::new(Vec3::splat(f32::NAN), Vec3::splat(f32::NAN));
        let probes = [broken, ReflectionProbe::new(Vec3::ZERO, Vec3::splat(10.0))];
        // 不崩就算过；选谁都行，NaN 盒子本来就 `contains` 不了任何点。
        let _ = select(&probes, Vec3::ZERO);
    }

    #[test]
    fn a_distant_probe_does_not_do_parallax() {
        let probe = ReflectionProbe::distant(Vec3::ZERO);
        assert!(!probe.parallax);
        assert_eq!(probe.correct(Vec3::new(3.0, 0.0, 0.0), Vec3::Y), Vec3::Y);
    }

    #[test]
    fn opposite_walls_reflect_in_opposite_directions() {
        // 站在房间左半边看到的墙，和站在右半边看到的，方向该相反。
        // 这一条不成立的话说明校正只是在缩放而没有真的换方向。
        let probe = unit_probe();
        let left = probe.correct(Vec3::new(-4.0, 0.0, 0.0), Vec3::Y);
        let right = probe.correct(Vec3::new(4.0, 0.0, 0.0), Vec3::Y);
        assert!(left.x < 0.0 && right.x > 0.0, "left={left:?} right={right:?}");
    }
}
