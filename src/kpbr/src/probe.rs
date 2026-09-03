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
    /// 盒子边缘往里多宽的一圈用来和外面的环境**过渡**（世界单位）。
    ///
    /// 逐对象选一个探针的话，物体跨过盒子边界的那一刻环境光会**跳一下**
    /// ——走过门口时整面墙的颜色突变，而且不报任何错。
    ///
    /// 在这一圈里，环境光从这个探针平滑过渡到「外面那个」（罩住它的更大
    /// 的探针，没有就是全局环境）。给 0 就退回原来的硬切换。
    ///
    /// 默认 0.5：比一个人的身宽略小，足以把突变抹开，又不至于让整个
    /// 小房间都处在过渡状态。
    pub blend_distance: f32,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            bounds: Aabb::new(Vec3::splat(-5.0), Vec3::splat(5.0)),
            parallax: true,
            intensity: 1.0,
            blend_distance: 0.5,
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

/// 挑主探针，外加一个用来过渡的次探针和权重。
///
/// 返回 `(主探针, 次探针, 权重)`：
///
/// - 主探针是 [`select`] 挑的那个（盒子最小的）。没有就返回 `None` 和 0。
/// - 次探针是**罩住同一个点、盒子次小的那个**；没有别的探针罩住时是
///   `None`，表示「外面就是全局环境」。
/// - 权重是次探针占的比例，0 表示纯用主探针。
///
/// # 权重怎么来的
///
/// 看着色点离主探针盒子的边有多近：深处是 0（纯主探针），
/// 贴着边是 1（完全交给外面那个）。过渡带的宽度是
/// [`ReflectionProbe::blend_distance`]。
///
/// 用 `smoothstep` 而不是线性：线性的话过渡带的**两端**各留一道
/// 导数不连续的折线，在大片平坦的墙上仍然看得出两道边。
pub fn select_blend(probes: &[ReflectionProbe], point: Vec3) -> (Option<usize>, Option<usize>, f32) {
    let mut inside: Vec<(usize, f32)> = probes
        .iter()
        .enumerate()
        .filter(|(_, probe)| probe.affects(point))
        .map(|(index, probe)| (index, probe.volume()))
        .collect();
    // `total_cmp`：退化的盒子会算出 NaN 体积，`partial_cmp().unwrap()` 会崩。
    inside.sort_by(|a, b| a.1.total_cmp(&b.1));

    let Some(&(primary, _)) = inside.first() else {
        return (None, None, 0.0);
    };
    let secondary = inside.get(1).map(|&(index, _)| index);

    let probe = &probes[primary];
    if probe.blend_distance <= 0.0 {
        return (Some(primary), secondary, 0.0);
    }

    // 到最近那个面的距离。取六个面里最小的——离任意一个面近就该开始过渡。
    let size = probe.bounds.size();
    let to_min = point - probe.bounds.min;
    let to_max = probe.bounds.max - point;
    let depth = to_min
        .x
        .min(to_min.y)
        .min(to_min.z)
        .min(to_max.x)
        .min(to_max.y)
        .min(to_max.z)
        // 盒子比过渡带还薄时，`depth` 永远到不了 `blend_distance`，
        // 整个探针都会处在半过渡状态。把过渡带压到半个盒子厚以内。
        .max(0.0);
    let band = probe.blend_distance.min(size.min_element() * 0.5).max(1e-6);

    let t = (depth / band).clamp(0.0, 1.0);
    // smoothstep(0,1,t)，再取反：深处权重 0，贴边权重 1。
    let weight = 1.0 - t * t * (3.0 - 2.0 * t);
    (Some(primary), secondary, weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个边长 10、中心在原点的探针。
    fn unit_probe() -> ReflectionProbe {
        ReflectionProbe::new(Vec3::ZERO, Vec3::splat(10.0))
    }

    /// 一个中心在 `center`、边长 `size`、过渡带 `blend` 的探针。
    fn probe_at(center: Vec3, size: f32, blend: f32) -> ReflectionProbe {
        ReflectionProbe {
            blend_distance: blend,
            ..ReflectionProbe::new(center, Vec3::splat(size))
        }
    }

    #[test]
    fn deep_inside_a_probe_there_is_no_blending() {
        // 房间正中央不该有任何过渡——那会让整个房间都蒙上一层
        // 外面的环境色。
        let probes = [probe_at(Vec3::ZERO, 10.0, 1.0)];
        let (primary, secondary, weight) = select_blend(&probes, Vec3::ZERO);
        assert_eq!(primary, Some(0));
        assert_eq!(secondary, None);
        assert_eq!(weight, 0.0);
    }

    #[test]
    fn the_weight_rises_to_one_at_the_boundary() {
        // 贴着盒子边时必须完全交给外面那个，否则边界上仍然是个突变——
        // 只是从「跳一大步」变成「跳一小步」。
        let probes = [probe_at(Vec3::ZERO, 10.0, 1.0)];
        // 盒子是 -5..5，过渡带 1，所以 x = 5 处权重为 1。
        let (_, _, at_edge) = select_blend(&probes, Vec3::new(5.0, 0.0, 0.0));
        assert!((at_edge - 1.0).abs() < 1e-5, "边界上的权重是 {at_edge}，该是 1");
        // 刚进过渡带（离边 1）时权重回到 0。
        let (_, _, at_band) = select_blend(&probes, Vec3::new(4.0, 0.0, 0.0));
        assert!(at_band.abs() < 1e-5, "过渡带外沿的权重是 {at_band}，该是 0");
    }

    #[test]
    fn the_weight_is_monotone_and_smooth_across_the_band() {
        // 单调是最起码的：中间冒出个峰的话，走过去会看到环境光
        // 来回晃一下。
        let probes = [probe_at(Vec3::ZERO, 10.0, 2.0)];
        let mut previous = -1.0f32;
        let mut samples = Vec::new();
        for step in 0..=40 {
            let x = 3.0 + step as f32 * 0.05; // 3.0 → 5.0，正好一整条过渡带
            let (_, _, weight) = select_blend(&probes, Vec3::new(x, 0.0, 0.0));
            assert!(weight >= previous - 1e-6, "权重在 x = {x} 处回落了");
            previous = weight;
            samples.push(weight);
        }
        // 两端的导数该趋近 0（smoothstep 而不是线性）。线性的话
        // 过渡带的两端各留一道折线，在大片平坦的墙上看得出两道边。
        let head = samples[1] - samples[0];
        let middle = samples[21] - samples[20];
        assert!(
            head < middle * 0.5,
            "过渡带起点的斜率({head:.4})和中段({middle:.4})差不多 —— 这是线性不是 smoothstep"
        );
    }

    #[test]
    fn the_secondary_is_the_next_smallest_box_that_also_covers_the_point() {
        // 小房间套在大厅里：走到房间门口时该过渡到**大厅**，
        // 而不是过渡到户外的天空。
        let probes = [
            probe_at(Vec3::ZERO, 40.0, 1.0), // 大厅
            probe_at(Vec3::ZERO, 10.0, 1.0), // 小房间
        ];
        let (primary, secondary, weight) = select_blend(&probes, Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(primary, Some(1), "该以小盒子为主");
        assert_eq!(secondary, Some(0), "该过渡到罩着它的大盒子");
        assert!(weight > 0.9);
    }

    #[test]
    fn no_probe_means_no_blending_either() {
        let probes = [probe_at(Vec3::ZERO, 10.0, 1.0)];
        let (primary, secondary, weight) = select_blend(&probes, Vec3::new(50.0, 0.0, 0.0));
        assert_eq!((primary, secondary, weight), (None, None, 0.0));
    }

    #[test]
    fn a_zero_blend_distance_keeps_the_old_hard_switch() {
        // 过渡是新加的。给 0 必须还原成原来的行为，否则所有已有的
        // 场景都会悄悄变个样。
        let probes = [probe_at(Vec3::ZERO, 10.0, 0.0)];
        for x in [0.0_f32, 3.0, 4.99] {
            let (_, _, weight) = select_blend(&probes, Vec3::new(x, 0.0, 0.0));
            assert_eq!(weight, 0.0, "x = {x} 处不该有过渡");
        }
    }

    #[test]
    fn a_box_thinner_than_the_blend_band_still_reaches_zero_at_its_centre() {
        // 过渡带比盒子还宽时，整个探针都会处在半过渡状态——
        // 那等于这个探针从来没被完整用过，而画面上只是「颜色不太对」。
        let probes = [probe_at(Vec3::ZERO, 1.0, 10.0)];
        let (_, _, centre) = select_blend(&probes, Vec3::ZERO);
        assert_eq!(centre, 0.0, "薄盒子的正中心也该是纯主探针");
    }

    #[test]
    fn select_and_select_blend_agree_on_the_primary() {
        // 两条路挑出来的主探针必须一样。不一样的话，改用带过渡的那条
        // 之后，有些物体会莫名其妙换了个探针。
        let probes = [
            probe_at(Vec3::new(0.0, 0.0, 0.0), 40.0, 1.0),
            probe_at(Vec3::new(3.0, 0.0, 0.0), 10.0, 1.0),
            probe_at(Vec3::new(-8.0, 0.0, 0.0), 6.0, 1.0),
        ];
        for step in 0..60 {
            let point = Vec3::new(-25.0 + step as f32, 0.0, 0.0);
            assert_eq!(
                select(&probes, point),
                select_blend(&probes, point).0,
                "在 {point:?} 处两条路挑了不同的主探针"
            );
        }
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
        for direction in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::new(1.0, 2.0, 3.0).normalize(),
        ] {
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
        assert!(
            left.x < 0.0 && right.x > 0.0,
            "left={left:?} right={right:?}"
        );
    }
}
