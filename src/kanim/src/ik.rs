//! 双骨 IK（反向动力学）。
//!
//! 给定一条「根—中—末」三关节链（胳膊、腿都是这个结构）和一个目标点，
//! 求出根与中两个关节各需要转多少，才能让末端落到目标上。
//!
//! 只做双骨而不做通用的 CCD/FABRIK：绝大多数角色 IK 需求就是手和脚，
//! 而双骨有**闭式解**——一次余弦定理算完，没有迭代、没有收敛问题、结果稳定。

use kmath::{Quat, Vec3};

/// 一条待求解的 IK 链，关节用姿态里的目标序号表示。
///
/// 这只是描述，求解在 [`solve_two_bone`] 里；把两者分开是为了让求解器
/// 保持纯函数，便于直接对着几何性质写测试。
#[derive(Debug, Clone, PartialEq)]
pub struct IkChain {
    /// 根关节（如肩、胯）。
    pub root: usize,
    /// 中间关节（如肘、膝）。
    pub mid: usize,
    /// 末端关节（如腕、踝）。
    pub end: usize,
    /// 末端要够到的世界坐标。
    pub target: Vec3,
    /// 极向量：中间关节朝哪边弯。为 [`None`] 时沿用当前的弯曲平面。
    pub pole: Option<Vec3>,
    /// 求解结果的权重，0 表示完全不生效，1 表示完全生效。
    pub weight: f32,
    /// 是否启用。
    pub enabled: bool,
}

impl IkChain {
    /// 新建一条链，默认全权重启用。
    pub fn new(root: usize, mid: usize, end: usize, target: Vec3) -> Self {
        Self {
            root,
            mid,
            end,
            target,
            pole: None,
            weight: 1.0,
            enabled: true,
        }
    }

    /// 指定极向量。
    pub fn with_pole(mut self, pole: Vec3) -> Self {
        self.pole = Some(pole);
        self
    }

    /// 指定权重。
    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight.clamp(0.0, 1.0);
        self
    }
}

/// 求解结果：施加在根与中关节上的**世界空间**旋转增量。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IkSolution {
    /// 根关节的旋转增量。
    pub root: Quat,
    /// 中间关节的旋转增量。
    pub mid: Quat,
}

impl IkSolution {
    /// 不做任何调整。
    pub const IDENTITY: Self = Self {
        root: Quat::IDENTITY,
        mid: Quat::IDENTITY,
    };

    /// 按权重淡入这个解，0 给出恒等、1 给出完整解。
    pub fn scaled(self, weight: f32) -> Self {
        let weight = weight.clamp(0.0, 1.0);
        Self {
            root: Quat::IDENTITY.slerp(self.root, weight),
            mid: Quat::IDENTITY.slerp(self.mid, weight),
        }
    }

    /// 把解应用到三个关节的世界坐标上，返回新的（中，末）位置。
    ///
    /// 调用方用它更新场景，测试用它验证末端是否真的够到了目标。
    /// 中关节的旋转要叠在根关节之后——它们是父子关系，不是并列关系。
    pub fn apply(&self, root: Vec3, mid: Vec3, end: Vec3) -> (Vec3, Vec3) {
        let new_mid = root + self.root * (mid - root);
        let new_end = new_mid + self.mid * (self.root * (end - mid));
        (new_mid, new_end)
    }
}

/// 求解一条双骨链。
///
/// 目标够不着时（超出两根骨头伸直的长度），链会朝目标方向伸直——
/// 这是唯一合理的退化行为：既不会抽搐，也不会突然缩回。
pub fn solve_two_bone(
    root: Vec3,
    mid: Vec3,
    end: Vec3,
    target: Vec3,
    pole: Option<Vec3>,
) -> IkSolution {
    let upper = (mid - root).length();
    let lower = (end - mid).length();
    // 骨头长度为零时没有可解的三角形。
    if upper <= f32::EPSILON || lower <= f32::EPSILON {
        return IkSolution::IDENTITY;
    }

    let to_target = target - root;
    let distance = to_target.length();
    if distance <= f32::EPSILON {
        return IkSolution::IDENTITY;
    }

    // 目标距离夹到三角形不等式允许的范围内。留一点余量，
    // 让完全伸直时的 acos 不至于因为浮点误差落到定义域外。
    let reach = (upper + lower) * 0.9999;
    let minimum = (upper - lower).abs() * 1.0001;
    let clamped = distance.clamp(minimum.min(reach), reach);

    let direction = to_target / distance;

    // 直接把两个关节该去的位置**构造**出来，再反求旋转。
    //
    // 另一条路是算出「当前夹角」与「目标夹角」的差、绕弯曲平面的法线转过去，
    // 但那样要处理一堆叉积的方向约定——根关节与中关节的自然转轴恰好反号，
    // 符号错一个，链就会朝反方向弯。构造位置没有这个问题：
    // 位置一旦确定，旋转就是唯一的。
    let bend = bend_direction(root, mid, direction, pole);

    // 余弦定理给出根关节处的夹角，于是中关节落在「目标方向转过该角」的位置上。
    let root_angle = law_of_cosines(upper, clamped, lower);
    let new_mid = root + (direction * root_angle.cos() + bend * root_angle.sin()) * upper;
    // 末端落在目标方向上、距离为夹紧后的长度——够不着时这就是伸直的姿态。
    let new_end = root + direction * clamped;

    let root_rotation = rotation_between(mid - root, new_mid - root);
    // 中关节的旋转叠在根关节之后，所以要拿被根关节转过的小臂去对齐。
    let mid_rotation = rotation_between(root_rotation * (end - mid), new_end - new_mid);

    IkSolution {
        root: root_rotation,
        mid: mid_rotation,
    }
}

/// 三角形中夹在 `adjacent_a` 与 `adjacent_b` 之间的那个角。
fn law_of_cosines(adjacent_a: f32, adjacent_b: f32, opposite: f32) -> f32 {
    let cosine = (adjacent_a * adjacent_a + adjacent_b * adjacent_b - opposite * opposite)
        / (2.0 * adjacent_a * adjacent_b);
    cosine.clamp(-1.0, 1.0).acos()
}

/// 中关节该往哪边弯：一个垂直于目标方向的单位向量。
///
/// 有极向量就朝极向量那边，否则沿用当前的弯曲方向（膝盖保持原来的朝向）。
fn bend_direction(root: Vec3, mid: Vec3, direction: Vec3, pole: Option<Vec3>) -> Vec3 {
    let reference = match pole {
        Some(pole) => pole - root,
        // 没给极向量时用当前上臂方向：它已经偏在某一侧，沿用它就不会突然翻面。
        None => mid - root,
    };

    // 去掉平行于目标方向的分量，剩下的就是「偏向哪一侧」。
    let perpendicular = reference - direction * reference.dot(direction);
    if perpendicular.length_squared() > 1e-12 {
        return perpendicular.normalize();
    }

    // 参考方向与目标方向共线：弯哪边都一样，挑一个确定的垂直方向，
    // 至少保证同样的输入给出同样的输出，而不是 NaN。
    let fallback = if direction.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let perpendicular = fallback - direction * fallback.dot(direction);
    perpendicular.normalize_or_zero()
}

/// 把向量 `from` 转到 `to` 的最短旋转。
fn rotation_between(from: Vec3, to: Vec3) -> Quat {
    let (from, to) = (from.normalize_or_zero(), to.normalize_or_zero());
    if from == Vec3::ZERO || to == Vec3::ZERO {
        return Quat::IDENTITY;
    }

    let dot = from.dot(to).clamp(-1.0, 1.0);
    if dot > 0.999_999 {
        return Quat::IDENTITY;
    }
    if dot < -0.999_999 {
        // 正好反向：转轴不唯一，随便取一个垂直方向转 180°。
        let reference = if from.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        let axis = from.cross(reference).normalize();
        return Quat::from_axis_angle(axis, std::f32::consts::PI);
    }

    let axis = from.cross(to).normalize();
    Quat::from_axis_angle(axis, dot.acos())
}

#[cfg(test)]
mod test {
    use super::*;

    /// 一条沿 +X 伸直、长度各为 1 的链，弯曲平面在 XY 上。
    fn straight_chain() -> (Vec3, Vec3, Vec3) {
        (
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
        )
    }

    /// 一条已经弯着的链，避免三点共线的退化情况。
    fn bent_chain() -> (Vec3, Vec3, Vec3) {
        (
            Vec3::ZERO,
            Vec3::new(1.0, 0.5, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
        )
    }

    #[test]
    fn reaches_a_target_within_range() {
        let (root, mid, end) = bent_chain();
        let target = Vec3::new(1.2, 0.9, 0.0);

        let solution = solve_two_bone(root, mid, end, target, None);
        let (_, new_end) = solution.apply(root, mid, end);

        assert!(
            (new_end - target).length() < 1e-3,
            "末端停在 {new_end:?}，没够到 {target:?}"
        );
    }

    #[test]
    fn reaches_targets_all_around() {
        let (root, mid, end) = bent_chain();

        // 在可达球面上取一圈目标，全都要够得到。
        for step in 0..16 {
            let angle = step as f32 / 16.0 * std::f32::consts::TAU;
            let target = Vec3::new(angle.cos(), angle.sin(), 0.0) * 1.5;

            let solution = solve_two_bone(root, mid, end, target, None);
            let (_, new_end) = solution.apply(root, mid, end);

            assert!(
                (new_end - target).length() < 1e-3,
                "目标 {target:?} 没够到，末端在 {new_end:?}"
            );
        }
    }

    #[test]
    fn bone_lengths_are_preserved() {
        let (root, mid, end) = bent_chain();
        let (upper, lower) = ((mid - root).length(), (end - mid).length());
        let target = Vec3::new(0.5, 1.2, 0.3);

        let solution = solve_two_bone(root, mid, end, target, None);
        let (new_mid, new_end) = solution.apply(root, mid, end);

        // 骨头不能被拉长或压扁，这是 IK 最基本的约束。
        assert!((( new_mid - root).length() - upper).abs() < 1e-4);
        assert!(((new_end - new_mid).length() - lower).abs() < 1e-4);
    }

    #[test]
    fn unreachable_target_straightens_the_chain() {
        let (root, mid, end) = bent_chain();
        let target = Vec3::new(100.0, 0.0, 0.0);

        let solution = solve_two_bone(root, mid, end, target, None);
        let (new_mid, new_end) = solution.apply(root, mid, end);

        // 够不着时应当朝目标伸直，而不是抽搐或缩回。
        let total = (new_mid - root).length() + (new_end - new_mid).length();
        assert!((( new_end - root).length() - total).abs() < 1e-3, "链没有伸直");
        // 而且要朝着目标的方向。
        let direction = (new_end - root).normalize();
        assert!(direction.dot(Vec3::X) > 0.999);
    }

    #[test]
    fn target_at_the_root_is_ignored() {
        let (root, mid, end) = bent_chain();

        // 目标与根重合时方向无从谈起，返回恒等即可，不能给出 NaN。
        let solution = solve_two_bone(root, mid, end, root, None);

        assert_eq!(solution, IkSolution::IDENTITY);
    }

    #[test]
    fn zero_length_bones_are_ignored() {
        let solution = solve_two_bone(Vec3::ZERO, Vec3::ZERO, Vec3::ONE, Vec3::ONE, None);

        assert_eq!(solution, IkSolution::IDENTITY);
    }

    #[test]
    fn collinear_chain_does_not_produce_nan() {
        // 完全伸直的链没有弯曲平面可言，容易算出 NaN。
        let (root, mid, end) = straight_chain();
        let target = Vec3::new(1.0, 1.0, 0.0);

        let solution = solve_two_bone(root, mid, end, target, None);
        let (new_mid, new_end) = solution.apply(root, mid, end);

        assert!(new_mid.is_finite() && new_end.is_finite());
        assert!((new_end - target).length() < 1e-3);
    }

    #[test]
    fn pole_vector_controls_the_bend_direction() {
        let (root, mid, end) = straight_chain();
        let target = Vec3::new(1.5, 0.0, 0.0);

        // 极向量分别指向 +Y 与 -Y，膝盖应当弯向相反的两侧。
        let up = solve_two_bone(root, mid, end, target, Some(Vec3::new(0.0, 5.0, 0.0)));
        let down = solve_two_bone(root, mid, end, target, Some(Vec3::new(0.0, -5.0, 0.0)));

        let (up_mid, _) = up.apply(root, mid, end);
        let (down_mid, _) = down.apply(root, mid, end);

        assert!(
            up_mid.y * down_mid.y < 0.0,
            "两个极向量给出了同侧的弯曲：{up_mid:?} / {down_mid:?}"
        );
    }

    #[test]
    fn weight_fades_the_solution_in() {
        let (root, mid, end) = bent_chain();
        let target = Vec3::new(0.5, 1.5, 0.0);
        let solution = solve_two_bone(root, mid, end, target, None);

        // 权重为 0 时完全不动。
        let (zero_mid, zero_end) = solution.scaled(0.0).apply(root, mid, end);
        assert!((zero_mid - mid).length() < 1e-5);
        assert!((zero_end - end).length() < 1e-5);

        // 权重为 1 时与完整解一致。
        let (full_mid, full_end) = solution.scaled(1.0).apply(root, mid, end);
        let (exact_mid, exact_end) = solution.apply(root, mid, end);
        assert!((full_mid - exact_mid).length() < 1e-5);
        assert!((full_end - exact_end).length() < 1e-5);

        // 中间权重给出中间结果。这里看末端而不是中关节：
        // 中关节可能恰好落回原位（旋转轴与它共线），末端却一定会朝目标移动。
        let (_, half_end) = solution.scaled(0.5).apply(root, mid, end);
        assert!((half_end - end).length() > 1e-4, "半权重下末端没动");
        assert!((half_end - exact_end).length() > 1e-4, "半权重下末端就已经到位了");
    }

    #[test]
    fn chain_builder_clamps_weight() {
        let chain = IkChain::new(0, 1, 2, Vec3::ONE).with_weight(5.0);

        assert_eq!(chain.weight, 1.0);
        assert!(chain.enabled);
    }
}
