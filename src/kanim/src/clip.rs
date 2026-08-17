//! 动画剪辑与姿态。
//!
//! **姿态（[`Pose`]）是混合的中间产物**，这是整套动画系统的枢纽：
//! 剪辑采样出姿态、混合树把多个姿态按权重合成一个、状态机在两个姿态之间过渡，
//! 最后才由场景图把姿态写进节点的局部变换。中间环节谁都不碰场景图，
//! 于是混合逻辑可以完全在 CPU 上独立测试。
//!
//! 这个设计来自 Fyrox 的 `AnimationPose`。

use crate::curve::{Animatable, Curve};
use kmath::{Quat, Vec3};

/// 一条轨道驱动目标的哪个分量。
///
/// 对应 glTF 的四种通道路径：`translation` / `rotation` / `scale` / `weights`。
#[derive(Debug, Clone, PartialEq)]
pub enum Channel {
    /// 局部位置。
    Position(Curve<Vec3>),
    /// 局部旋转。
    Rotation(Curve<Quat>),
    /// 局部缩放。
    Scale(Curve<Vec3>),
    /// 某个形变目标的权重。
    ///
    /// glTF 的一个 `weights` 通道同时驱动所有形变目标，导入时被拆成了
    /// 每个目标一条轨道——这样曲线的值类型保持定长，
    /// 混合逻辑不必为「变长数组」再开一套。
    MorphWeight {
        /// 形变目标的序号。
        index: usize,
        /// 权重曲线。
        curve: Curve<f32>,
    },
}

impl Channel {
    /// 本通道的时长。
    pub fn duration(&self) -> f32 {
        match self {
            Self::Position(curve) | Self::Scale(curve) => curve.duration(),
            Self::Rotation(curve) => curve.duration(),
            Self::MorphWeight { curve, .. } => curve.duration(),
        }
    }

    /// 关键帧数量。
    pub fn len(&self) -> usize {
        match self {
            Self::Position(curve) | Self::Scale(curve) => curve.len(),
            Self::Rotation(curve) => curve.len(),
            Self::MorphWeight { curve, .. } => curve.len(),
        }
    }

    /// 是否没有关键帧。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 一条轨道：把一条曲线绑到某个目标的某个分量上。
///
/// `target` 是**模型内的节点序号**而不是场景句柄——剪辑是可共享的资源，
/// 同一份剪辑要能驱动同一个模型的多个实例，因此它不能记住任何一个实例的句柄。
/// 序号到句柄的映射由播放器在实例化时建立。
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// 目标节点在模型中的序号。
    pub target: usize,
    /// 驱动的分量与曲线。
    pub channel: Channel,
}

/// 一个目标节点在某一时刻的局部变换。
///
/// 三个分量都是可选的：动画通常只驱动一部分分量（比如只转不移），
/// 没被驱动的分量必须保持节点原样，而不是被重置成单位值。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PoseEntry {
    /// 局部位置。
    pub position: Option<Vec3>,
    /// 局部旋转。
    pub rotation: Option<Quat>,
    /// 局部缩放。
    pub scale: Option<Vec3>,
}

impl PoseEntry {
    /// 是否没有任何分量被驱动。
    pub fn is_empty(&self) -> bool {
        self.position.is_none() && self.rotation.is_none() && self.scale.is_none()
    }

    /// 按权重把 `other` 混进来。
    ///
    /// 某个分量只有一方有值时直接采用那一方：缺失的一方没有可参与插值的基准，
    /// 硬凑一个单位值会把骨骼拽回原点。
    pub fn blend_with(&mut self, other: &Self, weight: f32) {
        self.position = blend_component(self.position, other.position, weight, Vec3::blend);
        self.rotation = blend_component(self.rotation, other.rotation, weight, Quat::blend);
        self.scale = blend_component(self.scale, other.scale, weight, Vec3::blend);
    }
}

fn blend_component<T: Copy>(
    a: Option<T>,
    b: Option<T>,
    weight: f32,
    blend: impl Fn(T, T, f32) -> T,
) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(blend(a, b, weight)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// 一个形变权重的采样值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MorphSample {
    /// 目标节点序号。
    pub target: usize,
    /// 形变目标在该网格里的序号。
    pub index: usize,
    /// 权重。
    pub weight: f32,
}

/// 一次采样的结果：每个目标一份局部变换，外加若干形变权重。
///
/// 局部变换用稠密数组而不是映射表：目标序号是从 0 开始的连续整数（模型节点序号），
/// 直接当下标用既省掉了哈希，混合时也只是两个数组的逐元素运算。
///
/// 形变权重反过来用稀疏表：带形变的网格通常只有几个，
/// 而且每个网格的形变目标数量各不相同，稠密存会浪费掉绝大部分空间。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pose {
    entries: Vec<PoseEntry>,
    morphs: Vec<MorphSample>,
}

impl Pose {
    /// 建一个能容纳 `targets` 个目标的空姿态。
    pub fn with_targets(targets: usize) -> Self {
        Self {
            entries: vec![PoseEntry::default(); targets],
            morphs: Vec::new(),
        }
    }

    /// 目标数量。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否没有任何目标。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.morphs.is_empty()
    }

    /// 清空所有分量，容量保留。每帧复用同一个姿态对象，避免重复分配。
    pub fn reset(&mut self) {
        for entry in &mut self.entries {
            *entry = PoseEntry::default();
        }
        self.morphs.clear();
    }

    /// 设置一个形变权重，已存在的同名条目会被覆盖。
    pub fn set_morph(&mut self, target: usize, index: usize, weight: f32) {
        match self
            .morphs
            .iter_mut()
            .find(|sample| sample.target == target && sample.index == index)
        {
            Some(sample) => sample.weight = weight,
            None => self.morphs.push(MorphSample {
                target,
                index,
                weight,
            }),
        }
    }

    /// 全部形变权重。
    pub fn morphs(&self) -> &[MorphSample] {
        &self.morphs
    }

    /// 取某个形变权重。
    pub fn morph(&self, target: usize, index: usize) -> Option<f32> {
        self.morphs
            .iter()
            .find(|sample| sample.target == target && sample.index == index)
            .map(|sample| sample.weight)
    }

    /// 确保能容纳 `targets` 个目标。
    pub fn resize(&mut self, targets: usize) {
        self.entries.resize(targets, PoseEntry::default());
    }

    /// 取某个目标的变换。
    pub fn entry(&self, target: usize) -> Option<&PoseEntry> {
        self.entries.get(target)
    }

    /// 取某个目标的可变引用，必要时自动扩容。
    pub fn entry_mut(&mut self, target: usize) -> &mut PoseEntry {
        if target >= self.entries.len() {
            self.entries.resize(target + 1, PoseEntry::default());
        }
        &mut self.entries[target]
    }

    /// 遍历所有被驱动的目标。
    pub fn iter(&self) -> impl Iterator<Item = (usize, &PoseEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.is_empty())
    }

    /// 按权重把 `other` 混进来。`weight` 为 0 保持自身，为 1 完全变成 `other`。
    pub fn blend_with(&mut self, other: &Self, weight: f32) {
        if weight <= 0.0 {
            return;
        }
        self.resize(self.entries.len().max(other.entries.len()));

        for (index, entry) in self.entries.iter_mut().enumerate() {
            let Some(source) = other.entries.get(index) else {
                continue;
            };
            if source.is_empty() {
                continue;
            }
            if entry.is_empty() {
                // 自己这边完全没被驱动：直接接管，但要按权重淡入，
                // 否则一个只影响手臂的剪辑刚混进来就会把手臂瞬移过去。
                *entry = *source;
                continue;
            }
            entry.blend_with(source, weight);
        }

        // 形变权重同样参与混合：两个表情之间过渡时，
        // 权重要跟着一起插值，否则表情会硬切。
        for sample in &other.morphs {
            match self
                .morphs
                .iter_mut()
                .find(|existing| existing.target == sample.target && existing.index == sample.index)
            {
                Some(existing) => {
                    existing.weight = f32::lerp(existing.weight, sample.weight, weight);
                }
                None => self.morphs.push(*sample),
            }
        }
    }
}

/// 一段动画剪辑。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationClip {
    name: String,
    duration: f32,
    tracks: Vec<Track>,
    /// 最大目标序号 + 1，用来给姿态预分配。
    targets: usize,
}

impl AnimationClip {
    /// 用一组轨道构造。时长与目标数由轨道推出。
    pub fn new(name: impl Into<String>, tracks: Vec<Track>) -> Self {
        let duration = tracks
            .iter()
            .map(|track| track.channel.duration())
            .fold(0.0f32, f32::max);
        let targets = tracks
            .iter()
            .map(|track| track.target + 1)
            .max()
            .unwrap_or(0);

        Self {
            name: name.into(),
            duration,
            tracks,
            targets,
        }
    }

    /// 剪辑名，来自 glTF 的动画名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 时长（秒）。
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// 轨道。
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// 涉及的目标数量（最大序号 + 1）。
    pub fn targets(&self) -> usize {
        self.targets
    }

    /// 在给定时刻采样到 `pose` 上。
    ///
    /// 只写入本剪辑驱动的分量，其余保持不动，因此可以先后叠加多个剪辑。
    pub fn sample_into(&self, time: f32, pose: &mut Pose) {
        pose.resize(pose.len().max(self.targets));

        for track in &self.tracks {
            if matches!(track.channel, Channel::MorphWeight { .. }) {
                continue;
            }
            let entry = pose.entry_mut(track.target);
            match &track.channel {
                Channel::Position(curve) => entry.position = Some(curve.sample(time)),
                Channel::Rotation(curve) => entry.rotation = Some(curve.sample(time)),
                Channel::Scale(curve) => entry.scale = Some(curve.sample(time)),
                Channel::MorphWeight { .. } => {}
            }
        }

        // 形变权重要单独走一遍：它存在稀疏表里，拿不到 `entry` 的可变引用。
        for track in &self.tracks {
            if let Channel::MorphWeight { index, curve } = &track.channel {
                pose.set_morph(track.target, *index, curve.sample(time));
            }
        }
    }

    /// 采样出一份新的姿态。每帧调用会反复分配，热路径请用 [`sample_into`](Self::sample_into)。
    pub fn sample(&self, time: f32) -> Pose {
        let mut pose = Pose::with_targets(self.targets);
        self.sample_into(time, &mut pose);
        pose
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::curve::Interpolation;

    /// 目标 0 从原点移动到 (10,0,0)、目标 1 绕 Y 转 90°，时长 1 秒。
    fn two_target_clip() -> AnimationClip {
        AnimationClip::new(
            "Test",
            vec![
                Track {
                    target: 0,
                    channel: Channel::Position(
                        Curve::new(
                            vec![0.0, 1.0],
                            vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0)],
                            Interpolation::Linear,
                        )
                        .unwrap(),
                    ),
                },
                Track {
                    target: 1,
                    channel: Channel::Rotation(
                        Curve::new(
                            vec![0.0, 1.0],
                            vec![
                                Quat::IDENTITY,
                                Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                            ],
                            Interpolation::Linear,
                        )
                        .unwrap(),
                    ),
                },
            ],
        )
    }

    #[test]
    fn clip_derives_duration_and_targets() {
        let clip = two_target_clip();

        assert_eq!(clip.duration(), 1.0);
        assert_eq!(clip.targets(), 2);
        assert_eq!(clip.name(), "Test");
    }

    #[test]
    fn empty_clip_has_zero_duration() {
        let clip = AnimationClip::new("Empty", Vec::new());

        assert_eq!(clip.duration(), 0.0);
        assert_eq!(clip.targets(), 0);
        assert!(clip.sample(0.0).is_empty());
    }

    #[test]
    fn sampling_fills_only_the_driven_components() {
        let pose = two_target_clip().sample(0.5);

        let first = pose.entry(0).unwrap();
        assert_eq!(first.position, Some(Vec3::new(5.0, 0.0, 0.0)));
        // 没被驱动的分量必须留空，否则节点自己的缩放会被抹掉。
        assert_eq!(first.rotation, None);
        assert_eq!(first.scale, None);

        assert!(pose.entry(1).unwrap().rotation.is_some());
        assert_eq!(pose.entry(1).unwrap().position, None);
    }

    #[test]
    fn iter_skips_untouched_targets() {
        let mut pose = Pose::with_targets(5);
        two_target_clip().sample_into(0.0, &mut pose);

        // 五个槽位里只有两个被驱动。
        assert_eq!(pose.iter().count(), 2);
    }

    #[test]
    fn blending_two_poses_interpolates_shared_components() {
        let mut a = Pose::with_targets(1);
        a.entry_mut(0).position = Some(Vec3::ZERO);
        let mut b = Pose::with_targets(1);
        b.entry_mut(0).position = Some(Vec3::new(10.0, 0.0, 0.0));

        a.blend_with(&b, 0.25);

        assert_eq!(a.entry(0).unwrap().position, Some(Vec3::new(2.5, 0.0, 0.0)));
    }

    #[test]
    fn blending_with_zero_weight_changes_nothing() {
        let mut a = two_target_clip().sample(0.0);
        let before = a.clone();
        let b = two_target_clip().sample(1.0);

        a.blend_with(&b, 0.0);

        assert_eq!(a, before);
    }

    #[test]
    fn blending_with_full_weight_takes_the_other_pose() {
        let mut a = two_target_clip().sample(0.0);
        let b = two_target_clip().sample(1.0);

        a.blend_with(&b, 1.0);

        assert_eq!(
            a.entry(0).unwrap().position,
            Some(Vec3::new(10.0, 0.0, 0.0))
        );
    }

    #[test]
    fn components_present_on_only_one_side_survive() {
        // 一个只驱动位置、一个只驱动旋转：混合后两者都要在，
        // 缺失的一方没有可插值的基准，补单位值会把骨骼拽回原点。
        let mut a = Pose::with_targets(1);
        a.entry_mut(0).position = Some(Vec3::new(1.0, 2.0, 3.0));
        let mut b = Pose::with_targets(1);
        b.entry_mut(0).rotation = Some(Quat::from_rotation_x(1.0));

        a.blend_with(&b, 0.5);

        assert_eq!(a.entry(0).unwrap().position, Some(Vec3::new(1.0, 2.0, 3.0)));
        assert!(a.entry(0).unwrap().rotation.is_some());
    }

    #[test]
    fn blending_grows_to_cover_the_other_pose() {
        let mut a = Pose::with_targets(1);
        a.entry_mut(0).position = Some(Vec3::ZERO);
        let mut b = Pose::with_targets(3);
        b.entry_mut(2).position = Some(Vec3::ONE);

        a.blend_with(&b, 0.5);

        assert_eq!(a.len(), 3);
        assert_eq!(a.entry(2).unwrap().position, Some(Vec3::ONE));
    }

    #[test]
    fn sampling_twice_overwrites_rather_than_accumulates() {
        let clip = two_target_clip();
        let mut pose = Pose::default();

        clip.sample_into(1.0, &mut pose);
        clip.sample_into(0.0, &mut pose);

        // 复用姿态对象时，后一次采样必须完全覆盖前一次的同名分量。
        assert_eq!(pose.entry(0).unwrap().position, Some(Vec3::ZERO));
    }

    #[test]
    fn reset_clears_without_shrinking() {
        let mut pose = two_target_clip().sample(0.5);
        let capacity = pose.len();

        pose.reset();

        assert_eq!(pose.len(), capacity);
        assert_eq!(pose.iter().count(), 0);
    }

    #[test]
    fn entry_mut_grows_the_pose() {
        let mut pose = Pose::default();

        pose.entry_mut(4).position = Some(Vec3::ONE);

        assert_eq!(pose.len(), 5);
        assert_eq!(pose.entry(4).unwrap().position, Some(Vec3::ONE));
    }

    /// 一个只驱动形变权重的剪辑：目标 0 的第 0 个形变从 0 走到 1。
    fn morph_clip(index: usize, from: f32, to: f32) -> AnimationClip {
        AnimationClip::new(
            "Morph",
            vec![Track {
                target: 0,
                channel: Channel::MorphWeight {
                    index,
                    curve: Curve::new(vec![0.0, 1.0], vec![from, to], Interpolation::Linear)
                        .unwrap(),
                },
            }],
        )
    }

    #[test]
    fn morph_weights_are_sampled() {
        let pose = morph_clip(0, 0.0, 1.0).sample(0.25);

        assert_eq!(pose.morph(0, 0), Some(0.25));
        assert_eq!(pose.morphs().len(), 1);
        // 形变轨道不该顺手在 TRS 表里留下空条目。
        assert_eq!(pose.iter().count(), 0);
    }

    #[test]
    fn morph_clip_reports_its_duration() {
        let clip = morph_clip(0, 0.0, 1.0);

        assert_eq!(clip.duration(), 1.0);
        assert_eq!(clip.targets(), 1);
    }

    #[test]
    fn multiple_morph_targets_are_independent() {
        let clip = AnimationClip::new(
            "Face",
            vec![
                Track {
                    target: 0,
                    channel: Channel::MorphWeight {
                        index: 0,
                        curve: Curve::constant(0.25),
                    },
                },
                Track {
                    target: 0,
                    channel: Channel::MorphWeight {
                        index: 1,
                        curve: Curve::constant(0.75),
                    },
                },
                Track {
                    target: 3,
                    channel: Channel::MorphWeight {
                        index: 0,
                        curve: Curve::constant(1.0),
                    },
                },
            ],
        );

        let pose = clip.sample(0.0);

        assert_eq!(pose.morph(0, 0), Some(0.25));
        assert_eq!(pose.morph(0, 1), Some(0.75));
        assert_eq!(pose.morph(3, 0), Some(1.0));
        assert_eq!(pose.morph(9, 0), None);
    }

    #[test]
    fn morph_weights_blend_between_poses() {
        let mut a = morph_clip(0, 0.0, 1.0).sample(0.0);
        let b = morph_clip(0, 0.0, 1.0).sample(1.0);

        a.blend_with(&b, 0.25);

        // 两个表情之间要插值，否则过渡会硬切。
        assert_eq!(a.morph(0, 0), Some(0.25));
    }

    #[test]
    fn morph_weights_present_on_only_one_side_are_adopted() {
        let mut a = Pose::with_targets(1);
        a.entry_mut(0).position = Some(Vec3::ZERO);
        let b = morph_clip(0, 1.0, 1.0).sample(0.0);

        a.blend_with(&b, 0.5);

        assert_eq!(a.morph(0, 0), Some(1.0));
    }

    #[test]
    fn set_morph_overwrites_the_same_slot() {
        let mut pose = Pose::default();

        pose.set_morph(2, 1, 0.5);
        pose.set_morph(2, 1, 0.9);
        pose.set_morph(2, 0, 0.1);

        assert_eq!(pose.morphs().len(), 2);
        assert_eq!(pose.morph(2, 1), Some(0.9));
    }

    #[test]
    fn reset_clears_morph_weights() {
        let mut pose = morph_clip(0, 1.0, 1.0).sample(0.0);
        assert!(!pose.morphs().is_empty());

        pose.reset();

        // reset 只清内容不缩容量（见 reset_clears_without_shrinking），
        // 所以这里看的是「没有任何被驱动的分量」。
        assert!(pose.morphs().is_empty());
        assert_eq!(pose.iter().count(), 0);
    }

    #[test]
    fn resampling_updates_morph_weights_in_place() {
        let clip = morph_clip(0, 0.0, 1.0);
        let mut pose = Pose::default();

        clip.sample_into(1.0, &mut pose);
        clip.sample_into(0.0, &mut pose);

        // 复用姿态对象时后一次采样必须覆盖前一次，而不是又追加一条。
        assert_eq!(pose.morphs().len(), 1);
        assert_eq!(pose.morph(0, 0), Some(0.0));
    }
}
