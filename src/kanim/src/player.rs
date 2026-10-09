//! 播放与混合。
//!
//! [`Animator`] 持有若干个「正在播放的剪辑」（[`AnimationState`]），
//! 每帧推进它们的时间、采样成姿态、按权重混合成一个最终姿态。
//!
//! 状态机与混合树不直接产出姿态，它们只负责**设置各个状态的权重**——
//! 这样上层无论多复杂，混合这一步都只有一份实现。

use crate::{AnimationClip, Pose};
use kmath::Vec3;
use std::sync::Arc;

/// 根运动：把根骨骼的位移从动画里抽出来，交给游戏（角色控制器）去移动角色。
///
/// 走路动画里根骨骼是真往前走的。直接播的话模型跑出了碰撞体、播完一圈「瞬移」回原点；
/// 根运动把这段位移拿出来，姿态里根骨骼留在原地，位移按帧增量交给角色控制器——
/// 脚步和移动距离严丝合缝，撞墙也会停。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RootMotion {
    /// 根骨骼在模型里的节点序号（和 [`crate::Track::target`] 一个意思）。
    pub target: usize,
    /// 竖直方向也抽出来。默认 `false`：上下起伏留在动画里，只把水平位移交出去
    /// （跳跃这类真要改高度的动画才设 `true`）。
    pub vertical: bool,
}

/// 一次 [`Animator::tick`] 里发生的事：哪个状态绕回了开头、哪个播完了。
///
/// three.js 的 `loop` / `finished` 事件。以前只能每帧轮询时间、自己探测
/// 「这一帧的时间比上一帧小」——倒放、一帧跨好几圈、交叉淡化中途这些
/// 情况各要写一遍，而且很容易写错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationEvent {
    /// 循环播放的状态绕回了开头（倒放时是绕回结尾）。`count` 是这一帧绕了
    /// 几圈——帧间隔比剪辑还长时会大于 1。
    Looped {
        /// 状态序号。
        state: usize,
        /// 剪辑序号。
        clip: usize,
        /// 这一帧绕了几圈。
        count: u32,
    },
    /// 不循环的状态到达了终点（倒放时是起点）。只报一次。
    Finished {
        /// 状态序号。
        state: usize,
        /// 剪辑序号。
        clip: usize,
    },
}

/// 一个正在播放的剪辑。
#[derive(Debug, Clone)]
pub struct AnimationState {
    /// 剪辑在 [`Animator`] 剪辑表中的序号。
    clip: usize,
    /// 当前播放位置（秒）。
    time: f32,
    /// 播放速度，可为负（倒放）。
    speed: f32,
    /// 播完是否从头再来。
    looping: bool,
    /// 时间是否在推进。
    playing: bool,
    /// 混合权重。
    weight: f32,
}

impl AnimationState {
    /// 新建一个默认循环播放、权重为 1 的状态。
    pub fn new(clip: usize) -> Self {
        Self {
            clip,
            time: 0.0,
            speed: 1.0,
            looping: true,
            playing: true,
            weight: 1.0,
        }
    }

    /// 剪辑序号。
    pub fn clip(&self) -> usize {
        self.clip
    }

    /// 当前播放位置。
    pub fn time(&self) -> f32 {
        self.time
    }

    /// 跳到指定时刻。
    pub fn set_time(&mut self, time: f32) {
        self.time = time;
    }

    /// 播放速度。
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// 设置播放速度。负值表示倒放。
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    /// 是否循环。
    pub fn is_looping(&self) -> bool {
        self.looping
    }

    /// 设置是否循环。
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// 时间是否在推进。
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// 暂停或继续。
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    /// 混合权重。
    pub fn weight(&self) -> f32 {
        self.weight
    }

    /// 设置混合权重。状态机与混合树就是通过它起作用的。
    pub fn set_weight(&mut self, weight: f32) {
        self.weight = weight.max(0.0);
    }

    /// 非循环且已经播到末尾。
    pub fn is_finished(&self, duration: f32) -> bool {
        !self.looping && (self.time >= duration || (self.speed < 0.0 && self.time <= 0.0))
    }

    /// 推进时间。返回这一步里绕回开头的圈数，以及是否刚刚播完。
    fn advance(&mut self, dt: f32, duration: f32) -> (u32, bool) {
        if !self.playing {
            return (0, false);
        }
        let before = self.time;
        self.time += dt * self.speed;

        if duration <= 0.0 {
            self.time = 0.0;
            return (0, false);
        }

        if self.looping {
            // 绕了几圈 = 未取余的时间落在第几个周期（倒放时往负方向数）。
            let laps = (self.time / duration).floor().abs() as u32;
            // 用欧几里得取余而不是 `%`：倒放时 `%` 会给出负数，
            // 时间轴一旦变负，曲线采样就永远被夹在第一帧。
            self.time = self.time.rem_euclid(duration);
            (laps, false)
        } else {
            self.time = self.time.clamp(0.0, duration);
            let end = if self.speed >= 0.0 { duration } else { 0.0 };
            // 这一步才到终点（上一步还没到）——只报一次。
            let finished = self.time == end && before != end;
            (0, finished)
        }
    }
}

/// 正在进行的交叉淡化。
///
/// 淡化期间每个状态的权重由「开始时的权重」到「目标权重」逐帧线性插值，
/// 目标权重对 [`Crossfade::target`] 是 1，对其它状态是 0。
#[derive(Debug, Clone)]
struct Crossfade {
    /// 淡入的目标状态序号。
    target: usize,
    /// 淡出的起始状态序号。warp 调速与结束时恢复速度都用它。
    source: usize,
    /// 每个状态淡化开始时的权重，下标即状态序号。
    from: Vec<f32>,
    /// `source` 在淡化开始前的速度，结束时恢复。
    source_speed: f32,
    /// 已经过的时间（秒）。
    elapsed: f32,
    /// 总时长（秒）。
    duration: f32,
}

/// 动画播放器。
///
/// 剪辑表用 [`Arc`] 共享：同一个模型的多个实例各有自己的播放进度，
/// 但没必要各存一份关键帧数据——Soldier 那种模型光曲线就有几百 KB。
#[derive(Debug, Clone)]
pub struct Animator {
    clips: Arc<Vec<AnimationClip>>,
    states: Vec<AnimationState>,
    /// 混合结果，每帧复用以避免反复分配。
    pose: Pose,
    /// 单个剪辑的采样暂存区。
    scratch: Pose,
    /// 全局速度倍率，作用在所有状态上。
    speed: f32,
    /// 是否整体推进。
    playing: bool,
    /// 正在进行的交叉淡化；没有时为 [`None`]。
    crossfade: Option<Crossfade>,
    /// 上一次 `tick` / `step` 里发生的事件。每次推进前清空。
    events: Vec<AnimationEvent>,
    /// 根运动设置；`None` 时根骨骼照常被动画移动。
    root_motion: Option<RootMotion>,
    /// 上一次推进抽出来的根骨骼位移（根骨骼父节点的空间）。
    root_delta: Vec3,
}

impl Animator {
    /// 用一组剪辑创建播放器，初始没有任何状态在播。
    pub fn new(clips: Arc<Vec<AnimationClip>>) -> Self {
        let targets = clips.iter().map(AnimationClip::targets).max().unwrap_or(0);
        Self {
            clips,
            states: Vec::new(),
            pose: Pose::with_targets(targets),
            scratch: Pose::with_targets(targets),
            speed: 1.0,
            playing: true,
            crossfade: None,
            events: Vec::new(),
            root_motion: None,
            root_delta: Vec3::ZERO,
        }
    }

    /// 开 / 关根运动（见 [`RootMotion`]）。
    pub fn set_root_motion(&mut self, root_motion: Option<RootMotion>) {
        self.root_motion = root_motion;
        self.root_delta = Vec3::ZERO;
    }

    /// 当前的根运动设置。
    pub fn root_motion(&self) -> Option<RootMotion> {
        self.root_motion
    }

    /// 上一次 [`tick`](Self::tick) / [`step`](Self::step) 里根骨骼走了多远（根骨骼**父节点**的空间）。
    ///
    /// 各个在播的状态按权重混：走路和跑步混到一半，位移也是两者的一半。循环绕回开头那一帧
    /// 也是连续的（「结尾 − 上一刻」加上「这一刻 − 开头」），不会一下倒退一整圈。
    pub fn root_motion_delta(&self) -> Vec3 {
        self.root_delta
    }

    /// 剪辑表。
    pub fn clips(&self) -> &[AnimationClip] {
        &self.clips
    }

    /// 按名字找剪辑序号。
    pub fn clip_index(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|clip| clip.name() == name)
    }

    /// 全局速度倍率。
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// 设置全局速度倍率。
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    /// 是否整体在推进。
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// 整体暂停或继续。
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    /// 全部状态。
    pub fn states(&self) -> &[AnimationState] {
        &self.states
    }

    /// 按序号取状态。
    pub fn state_mut(&mut self, index: usize) -> Option<&mut AnimationState> {
        self.states.get_mut(index)
    }

    /// 添加一个播放状态，返回它的序号。剪辑序号无效时返回 [`None`]。
    pub fn add_state(&mut self, clip: usize) -> Option<usize> {
        if clip >= self.clips.len() {
            return None;
        }
        self.states.push(AnimationState::new(clip));
        Some(self.states.len() - 1)
    }

    /// 只播这一个剪辑，清掉其它状态。
    pub fn play(&mut self, clip: usize) -> Option<usize> {
        self.states.clear();
        self.crossfade = None;
        self.add_state(clip)
    }

    /// 按名字只播这一个剪辑。
    pub fn play_by_name(&mut self, name: &str) -> Option<usize> {
        let clip = self.clip_index(name)?;
        self.play(clip)
    }

    /// 清空所有播放状态。
    pub fn clear(&mut self) {
        self.states.clear();
        self.crossfade = None;
        self.pose.reset();
    }

    /// 按状态机或混合树算出的权重更新各状态。
    ///
    /// 权重表里出现的剪辑若还没有播放状态，会自动补一个；
    /// 没出现的状态权重清零但**保留**——它的播放进度还在，
    /// 过渡回去时能接着上次的位置播，而不是从头开始。
    pub fn apply_weights(&mut self, weights: &[(usize, f32)]) {
        // 手动喂权重等于接管控制权：进行中的交叉淡化作废。
        self.crossfade = None;

        for state in &mut self.states {
            state.weight = 0.0;
        }

        for &(clip, weight) in weights {
            if weight <= 0.0 {
                continue;
            }
            match self.states.iter_mut().find(|state| state.clip == clip) {
                Some(state) => state.set_weight(weight),
                None => {
                    if let Some(index) = self.add_state(clip) {
                        self.states[index].set_weight(weight);
                    }
                }
            }
        }
    }

    /// 推进一帧并重新混合出姿态。
    ///
    /// 暂停（[`set_playing`](Self::set_playing) 为 `false`）时既不推进时间
    /// 也不推进交叉淡化，只重算一次姿态——姿态保持不变。
    pub fn tick(&mut self, dt: f32) {
        if self.playing {
            self.advance(dt);
        }
        self.rebuild_pose();
    }

    /// 无视 [`playing`](Self::is_playing) 状态，强制推进 `dt` 秒并重新混合。
    ///
    /// 单步模式用：动画整体暂停，用户点一下就走一步，其余帧保持冻结。
    /// 引擎每帧仍会调用 [`tick`](Self::tick)，但在暂停态下它什么都不做，
    /// 所以只有这里显式调用时时间才前进。
    pub fn step(&mut self, dt: f32) {
        self.advance(dt);
        self.rebuild_pose();
    }

    /// 上一次 [`tick`](Self::tick) / [`step`](Self::step) 里发生的事件：
    /// 哪个状态绕回了开头、哪个播完了。下一次推进时清空。
    ///
    /// ```ignore
    /// animator.tick(dt);
    /// for event in animator.events() {
    ///     if let AnimationEvent::Finished { clip, .. } = event { /* 播下一段 */ }
    /// }
    /// ```
    pub fn events(&self) -> &[AnimationEvent] {
        &self.events
    }

    /// 推进所有状态的时间与交叉淡化，不重建姿态。
    fn advance(&mut self, dt: f32) {
        self.events.clear();
        let mut root_delta = Vec3::ZERO;
        let mut root_weight = 0.0;
        for index in 0..self.states.len() {
            let clip = self.states[index].clip;
            let duration = self.clips[clip].duration();
            let speed = self.speed;
            let before = self.states[index].time;
            let (laps, finished) = self.states[index].advance(dt * speed, duration);
            if let Some(root) = self.root_motion
                && let Some(curve) = self.clips[clip].position_curve(root.target)
                && self.states[index].weight > 0.0
            {
                let state = &self.states[index];
                // 这一步走过的位移：「这一刻 − 上一刻」，每绕一圈再补一整圈（倒放时减）。
                let mut delta = curve.sample(state.time) - curve.sample(before);
                if laps > 0 {
                    let lap = curve.sample(duration) - curve.sample(0.0);
                    let direction = if state.speed * speed >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    };
                    delta += lap * laps as f32 * direction;
                }
                root_delta += delta * state.weight;
                root_weight += state.weight;
            }
            if laps > 0 {
                self.events.push(AnimationEvent::Looped {
                    state: index,
                    clip,
                    count: laps,
                });
            }
            if finished {
                self.events
                    .push(AnimationEvent::Finished { state: index, clip });
            }
        }
        self.advance_crossfade(dt);
        self.root_delta = match self.root_motion {
            Some(root) if root_weight > 0.0 => {
                let delta = root_delta / root_weight;
                if root.vertical {
                    delta
                } else {
                    Vec3::new(delta.x, 0.0, delta.z)
                }
            }
            _ => Vec3::ZERO,
        };
    }

    /// 推进交叉淡化：按已过时间线性更新各状态权重，结束后恢复淡出状态的速度。
    fn advance_crossfade(&mut self, dt: f32) {
        let Some(fade) = &mut self.crossfade else {
            return;
        };
        fade.elapsed += dt;
        let t = if fade.duration > 0.0 {
            (fade.elapsed / fade.duration).clamp(0.0, 1.0)
        } else {
            1.0
        };

        for (index, state) in self.states.iter_mut().enumerate() {
            let from = fade.from.get(index).copied().unwrap_or(0.0);
            let target = if index == fade.target { 1.0 } else { 0.0 };
            state.weight = from + (target - from) * t;
        }

        if t >= 1.0 {
            // warp 改过淡出状态的速度，淡出后它权重为 0，这时恢复成原值
            // 看不见，却能保证它下次再入场时不是被上一次 warp 搅乱的速度。
            if let Some(source) = self.states.get_mut(fade.source) {
                source.speed = fade.source_speed;
            }
            self.crossfade = None;
        }
    }

    /// 在 `duration` 秒内把 `from` 状态的权重线性降到 0、`to` 状态升到 1，
    /// 其余状态保持不动。对应 three.js `AnimationAction::crossFadeTo`。
    ///
    /// `warp` 为真时（three.js 的默认）做两件「时间同步」的事：`to` 的时间
    /// 归零（入场的剪辑从头播），`from` 的速度调成 `from 时长 / to 时长`
    /// （退场的剪辑按比例加速或减速，淡化期间两段的相位对齐，走路的脚
    /// 不会突然错拍）。淡化结束会恢复 `from` 原来的速度。
    ///
    /// 与 three.js 的一个有意差异：它还会把入场的 `to` 按反比再调一遍速度
    /// （`endStartRatio`），这里不碰 `to`，让它保持调用方设置的自然节奏。
    ///
    /// 手动喂权重（[`apply_weights`](Self::apply_weights) 或 [`play`](Self::play)）
    /// 会取消进行中的淡化。返回 `false` 表示序号无效或 `from == to`。
    pub fn crossfade(&mut self, from: usize, to: usize, duration: f32, warp: bool) -> bool {
        if from >= self.states.len() || to >= self.states.len() || from == to {
            return false;
        }

        let from_weights: Vec<f32> = self.states.iter().map(|state| state.weight).collect();
        let from_duration = self.clips[self.states[from].clip].duration();
        let to_duration = self.clips[self.states[to].clip].duration();

        let source_speed = self.states[from].speed;
        if warp {
            if from_duration > 0.0 && to_duration > 0.0 {
                self.states[from].speed = from_duration / to_duration;
            }
            self.states[to].time = 0.0;
        }

        self.crossfade = Some(Crossfade {
            target: to,
            source: from,
            from: from_weights,
            source_speed,
            elapsed: 0.0,
            duration: duration.max(0.0),
        });
        true
    }

    /// 是否正在交叉淡化。
    pub fn is_crossfading(&self) -> bool {
        self.crossfade.is_some()
    }

    /// 交叉淡化的进度，`0` 是刚开始、`1` 是完成；不在淡化中返回 [`None`]。
    pub fn crossfade_progress(&self) -> Option<f32> {
        self.crossfade.as_ref().map(|fade| {
            if fade.duration > 0.0 {
                (fade.elapsed / fade.duration).clamp(0.0, 1.0)
            } else {
                1.0
            }
        })
    }

    /// 只重新混合，不推进时间。手动改了权重之后可以调它。
    pub fn rebuild_pose(&mut self) {
        self.pose.reset();

        // 增量归一化：第 i 个状态按 `w_i / (w_1 + … + w_i)` 混进来，
        // 最终结果恰好等于各状态的加权平均，而不需要事先求和再走第二遍。
        let mut accumulated = 0.0;
        for index in 0..self.states.len() {
            let state = &self.states[index];
            if state.weight <= 0.0 {
                continue;
            }
            let Some(clip) = self.clips.get(state.clip) else {
                continue;
            };

            let (time, weight) = (state.time, state.weight);
            self.scratch.reset();
            clip.sample_into(time, &mut self.scratch);

            accumulated += weight;
            let blend = if accumulated > 0.0 {
                weight / accumulated
            } else {
                0.0
            };
            self.pose.blend_with(&self.scratch, blend);
        }

        // 根运动：位移已经交出去了，姿态里根骨骼钉在各剪辑开头的位置（按同样的权重混），
        // 只留下抽出来之外的那部分（默认是上下起伏）。
        if let Some(root) = self.root_motion {
            let mut anchor = Vec3::ZERO;
            let mut total = 0.0;
            for state in &self.states {
                if state.weight <= 0.0 {
                    continue;
                }
                if let Some(curve) = self
                    .clips
                    .get(state.clip)
                    .and_then(|clip| clip.position_curve(root.target))
                {
                    anchor += curve.sample(0.0) * state.weight;
                    total += state.weight;
                }
            }
            if total > 0.0 {
                let anchor = anchor / total;
                let entry = self.pose.entry_mut(root.target);
                if let Some(position) = entry.position.as_mut() {
                    position.x = anchor.x;
                    position.z = anchor.z;
                    if root.vertical {
                        position.y = anchor.y;
                    }
                }
            }
        }
    }

    /// 上一次 [`tick`](Self::tick) 混出的姿态。
    pub fn pose(&self) -> &Pose {
        &self.pose
    }

    /// 某个状态所播剪辑的时长。
    pub fn state_duration(&self, index: usize) -> f32 {
        self.states
            .get(index)
            .and_then(|state| self.clips.get(state.clip))
            .map(AnimationClip::duration)
            .unwrap_or(0.0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Channel, Curve, Interpolation, Track};
    use kmath::Vec3;

    /// 一个 1 秒的位移剪辑：从原点走到 `end`。
    fn move_clip(name: &str, end: Vec3) -> AnimationClip {
        AnimationClip::new(
            name,
            vec![Track {
                target: 0,
                channel: Channel::Position(
                    Curve::new(vec![0.0, 1.0], vec![Vec3::ZERO, end], Interpolation::Linear)
                        .unwrap(),
                ),
            }],
        )
    }

    fn animator() -> Animator {
        Animator::new(Arc::new(vec![
            move_clip("A", Vec3::new(10.0, 0.0, 0.0)),
            move_clip("B", Vec3::new(0.0, 20.0, 0.0)),
        ]))
    }

    fn position(animator: &Animator) -> Vec3 {
        animator.pose().entry(0).unwrap().position.unwrap()
    }

    #[test]
    fn looping_reports_how_many_laps_a_frame_covered() {
        let mut animator = animator();
        animator.play_by_name("A").unwrap();
        animator.tick(0.5);
        assert!(animator.events().is_empty());
        animator.tick(0.7);
        assert_eq!(
            animator.events(),
            &[AnimationEvent::Looped {
                state: 0,
                clip: 0,
                count: 1
            }]
        );
        // 一帧跨两圈多（1 秒的剪辑、2.5 秒的帧间隔）。
        animator.tick(2.5);
        assert_eq!(
            animator.events(),
            &[AnimationEvent::Looped {
                state: 0,
                clip: 0,
                count: 2
            }]
        );
        // 倒放绕回结尾也算一圈。
        animator.state_mut(0).unwrap().set_speed(-1.0);
        animator.tick(1.0);
        assert_eq!(animator.events().len(), 1, "{:?}", animator.events());
    }

    #[test]
    fn a_one_shot_clip_reports_finished_exactly_once() {
        let mut animator = animator();
        let state = animator.play_by_name("A").unwrap();
        animator.state_mut(state).unwrap().set_looping(false);
        animator.tick(0.6);
        assert!(animator.events().is_empty());
        animator.tick(0.6);
        assert_eq!(
            animator.events(),
            &[AnimationEvent::Finished { state, clip: 0 }]
        );
        animator.tick(0.6);
        assert!(animator.events().is_empty(), "停在终点之后不再重复报告");
        // 倒放回到起点同样报一次。
        animator.state_mut(state).unwrap().set_speed(-1.0);
        animator.tick(2.0);
        assert_eq!(
            animator.events(),
            &[AnimationEvent::Finished { state, clip: 0 }]
        );
    }

    #[test]
    fn playing_a_clip_advances_time() {
        let mut animator = animator();
        animator.play_by_name("A").unwrap();

        animator.tick(0.25);

        assert_eq!(animator.states()[0].time(), 0.25);
        assert_eq!(position(&animator), Vec3::new(2.5, 0.0, 0.0));
    }

    #[test]
    fn looping_wraps_around() {
        let mut animator = animator();
        animator.play(0).unwrap();

        animator.tick(1.25);

        // 1 秒的剪辑走了 1.25 秒，应当回到 0.25 处。
        assert!((animator.states()[0].time() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn non_looping_clamps_at_the_end() {
        let mut animator = animator();
        let state = animator.play(0).unwrap();
        animator.state_mut(state).unwrap().set_looping(false);

        animator.tick(5.0);

        assert_eq!(animator.states()[0].time(), 1.0);
        assert!(animator.states()[0].is_finished(1.0));
    }

    #[test]
    fn reverse_playback_does_not_go_negative() {
        let mut animator = animator();
        let state = animator.play(0).unwrap();
        animator.state_mut(state).unwrap().set_speed(-1.0);

        animator.tick(0.25);

        // 倒放时用 `%` 会得到 -0.25，时间轴一旦变负就永远被夹在第一帧。
        assert!((animator.states()[0].time() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn global_speed_scales_every_state() {
        let mut animator = animator();
        animator.play(0).unwrap();
        animator.set_speed(2.0);

        animator.tick(0.25);

        assert!((animator.states()[0].time() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pausing_freezes_time_but_keeps_the_pose() {
        let mut animator = animator();
        animator.play(0).unwrap();
        animator.tick(0.5);
        let frozen = position(&animator);

        animator.set_playing(false);
        animator.tick(10.0);

        assert_eq!(animator.states()[0].time(), 0.5);
        assert_eq!(position(&animator), frozen);
    }

    #[test]
    fn equal_weights_average_the_two_clips() {
        let mut animator = animator();
        animator.add_state(0).unwrap();
        animator.add_state(1).unwrap();

        // 走到半程：两个剪辑分别在 (5,0,0) 与 (0,10,0)，等权混合取二者的平均。
        // 注意不能走满 1 秒——循环剪辑那时已经绕回起点了。
        animator.tick(0.5);

        assert_eq!(position(&animator), Vec3::new(2.5, 5.0, 0.0));
    }

    #[test]
    fn weights_are_normalised_incrementally() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.state_mut(a).unwrap().set_weight(3.0);
        animator.state_mut(b).unwrap().set_weight(1.0);

        animator.tick(0.5);

        // 3:1 的权重 → 第一个剪辑占四分之三。
        assert_eq!(position(&animator), Vec3::new(3.75, 2.5, 0.0));
    }

    #[test]
    fn zero_weight_states_are_skipped() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.state_mut(b).unwrap().set_weight(0.0);
        let _ = a;

        animator.tick(0.5);

        assert_eq!(position(&animator), Vec3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn play_replaces_previous_states() {
        let mut animator = animator();
        animator.add_state(0);
        animator.add_state(1);

        animator.play(1).unwrap();

        assert_eq!(animator.states().len(), 1);
        assert_eq!(animator.states()[0].clip(), 1);
    }

    #[test]
    fn invalid_clip_index_is_rejected() {
        let mut animator = animator();

        assert!(animator.add_state(99).is_none());
        assert!(animator.play_by_name("没有这个动画").is_none());
        assert!(animator.states().is_empty());
    }

    #[test]
    fn empty_animator_produces_an_empty_pose() {
        let mut animator = Animator::new(Arc::new(Vec::new()));

        animator.tick(1.0);

        assert_eq!(animator.pose().iter().count(), 0);
    }

    #[test]
    fn zero_length_clip_does_not_divide_by_zero() {
        let clip = AnimationClip::new(
            "Static",
            vec![Track {
                target: 0,
                channel: Channel::Position(Curve::constant(Vec3::ONE)),
            }],
        );
        let mut animator = Animator::new(Arc::new(vec![clip]));
        animator.play(0).unwrap();

        animator.tick(1.0);

        assert_eq!(animator.states()[0].time(), 0.0);
        assert_eq!(position(&animator), Vec3::ONE);
    }

    #[test]
    fn apply_weights_creates_states_on_demand() {
        let mut animator = animator();

        animator.apply_weights(&[(0, 0.25), (1, 0.75)]);

        assert_eq!(animator.states().len(), 2);
        assert_eq!(animator.states()[0].weight(), 0.25);
        assert_eq!(animator.states()[1].weight(), 0.75);
    }

    #[test]
    fn apply_weights_zeroes_states_not_mentioned() {
        let mut animator = animator();
        animator.apply_weights(&[(0, 1.0), (1, 1.0)]);
        animator.tick(0.5);

        animator.apply_weights(&[(1, 1.0)]);

        assert_eq!(animator.states()[0].weight(), 0.0);
        // 权重归零但状态保留：进度还在，过渡回去时能接着播。
        assert_eq!(animator.states().len(), 2);
        assert!((animator.states()[0].time() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn apply_weights_ignores_unknown_clips() {
        let mut animator = animator();

        animator.apply_weights(&[(99, 1.0)]);

        assert!(animator.states().is_empty());
    }

    #[test]
    fn rebuild_pose_reflects_weight_changes_without_ticking() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.tick(0.5);

        animator.state_mut(a).unwrap().set_weight(0.0);
        animator.state_mut(b).unwrap().set_weight(1.0);
        animator.rebuild_pose();

        assert_eq!(position(&animator), Vec3::new(0.0, 10.0, 0.0));
    }

    /// 两个时长不同的剪辑：A 2 秒，B 1 秒，各自驱动目标 0 的位移。
    fn uneven_animator() -> Animator {
        Animator::new(Arc::new(vec![
            AnimationClip::new(
                "A",
                vec![Track {
                    target: 0,
                    channel: Channel::Position(
                        Curve::new(
                            vec![0.0, 2.0],
                            vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0)],
                            Interpolation::Linear,
                        )
                        .unwrap(),
                    ),
                }],
            ),
            AnimationClip::new(
                "B",
                vec![Track {
                    target: 0,
                    channel: Channel::Position(
                        Curve::new(
                            vec![0.0, 1.0],
                            vec![Vec3::ZERO, Vec3::new(0.0, 10.0, 0.0)],
                            Interpolation::Linear,
                        )
                        .unwrap(),
                    ),
                }],
            ),
        ]))
    }

    #[test]
    fn crossfade_ramps_weights_over_duration() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.state_mut(a).unwrap().set_weight(1.0);
        animator.state_mut(b).unwrap().set_weight(0.0);

        assert!(animator.crossfade(a, b, 1.0, false));
        assert!(animator.is_crossfading());

        animator.tick(0.5);
        assert!((animator.states()[a].weight() - 0.5).abs() < 1e-5);
        assert!((animator.states()[b].weight() - 0.5).abs() < 1e-5);
        assert!((animator.crossfade_progress().unwrap() - 0.5).abs() < 1e-5);

        animator.tick(0.5);
        assert_eq!(animator.states()[a].weight(), 0.0);
        assert_eq!(animator.states()[b].weight(), 1.0);
        assert!(!animator.is_crossfading());
        assert!(animator.crossfade_progress().is_none());
    }

    #[test]
    fn crossfade_rejects_invalid_or_identical_states() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();

        assert!(!animator.crossfade(a, 99, 1.0, false));
        assert!(!animator.crossfade(99, a, 1.0, false));
        assert!(!animator.crossfade(a, a, 1.0, false));
        assert!(!animator.is_crossfading());
    }

    #[test]
    fn instant_crossfade_switches_immediately() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.state_mut(a).unwrap().set_weight(1.0);
        animator.state_mut(b).unwrap().set_weight(0.0);

        animator.crossfade(a, b, 0.0, false);
        animator.tick(0.1);

        assert!(!animator.is_crossfading());
        assert_eq!(animator.states()[a].weight(), 0.0);
        assert_eq!(animator.states()[b].weight(), 1.0);
    }

    #[test]
    fn warp_resets_target_time_and_scales_then_restores_source_speed() {
        let mut animator = uneven_animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.state_mut(a).unwrap().set_weight(1.0);
        animator.state_mut(b).unwrap().set_weight(0.0);

        // 让两个状态先各走一段（避开整周期，否则循环剪辑刚好绕回 0），
        // 确认入场的 b 时间确实被归零。
        animator.tick(0.3);
        assert!((animator.states()[b].time() - 0.3).abs() < 1e-6);

        assert!(animator.crossfade(a, b, 1.0, true));

        // warp：b 从头播，a 速度 = A 时长 / B 时长 = 2 / 1 = 2。
        assert_eq!(animator.states()[b].time(), 0.0);
        assert!((animator.states()[a].speed() - 2.0).abs() < 1e-6);

        // 淡化结束后 a 的速度恢复原值。
        animator.tick(1.0);
        assert!(!animator.is_crossfading());
        assert!((animator.states()[a].speed() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn crossfade_fades_everything_else_out() {
        let mut animator = Animator::new(Arc::new(vec![
            move_clip("A", Vec3::new(10.0, 0.0, 0.0)),
            move_clip("B", Vec3::new(0.0, 20.0, 0.0)),
            move_clip("C", Vec3::new(0.0, 0.0, 30.0)),
        ]));
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        let c = animator.add_state(2).unwrap();
        animator.state_mut(a).unwrap().set_weight(1.0);
        animator.state_mut(b).unwrap().set_weight(0.0);
        animator.state_mut(c).unwrap().set_weight(0.0);

        animator.crossfade(a, c, 1.0, false);
        animator.tick(0.5);

        // 只有 a → c 参与淡化，b 保持 0。
        assert!((animator.states()[a].weight() - 0.5).abs() < 1e-5);
        assert!((animator.states()[c].weight() - 0.5).abs() < 1e-5);
        assert_eq!(animator.states()[b].weight(), 0.0);
    }

    #[test]
    fn manual_weights_cancel_crossfade() {
        let mut animator = animator();
        let a = animator.add_state(0).unwrap();
        let b = animator.add_state(1).unwrap();
        animator.state_mut(a).unwrap().set_weight(1.0);
        animator.state_mut(b).unwrap().set_weight(0.0);
        animator.crossfade(a, b, 1.0, false);
        assert!(animator.is_crossfading());

        animator.apply_weights(&[(0, 1.0)]);

        assert!(!animator.is_crossfading());
        assert_eq!(animator.states()[0].weight(), 1.0);
    }

    #[test]
    fn step_advances_even_while_paused() {
        let mut animator = animator();
        let state = animator.play(0).unwrap();
        animator.tick(0.25);
        animator.set_playing(false);

        animator.step(0.25);

        assert!((animator.states()[state].time() - 0.5).abs() < 1e-6);

        // 暂停态下普通的 tick 不再推进。
        animator.tick(1.0);
        assert!((animator.states()[state].time() - 0.5).abs() < 1e-6);
    }

    // ── 根运动 ──

    /// 一段 1 秒的「走路」：根骨骼（序号 0）从 (0,0,0) 走到 (0,0,2)，中途上下起伏 0.1。
    fn walk_clip(name: &str, distance: f32) -> AnimationClip {
        AnimationClip::new(
            name,
            vec![Track {
                target: 0,
                channel: Channel::Position(
                    Curve::new(
                        vec![0.0, 0.5, 1.0],
                        vec![
                            Vec3::ZERO,
                            Vec3::new(0.0, 0.1, distance * 0.5),
                            Vec3::new(0.0, 0.0, distance),
                        ],
                        Interpolation::Linear,
                    )
                    .unwrap(),
                ),
            }],
        )
    }

    fn root_animator(clips: Vec<AnimationClip>) -> Animator {
        let mut animator = Animator::new(Arc::new(clips));
        animator.set_root_motion(Some(RootMotion {
            target: 0,
            vertical: false,
        }));
        animator
    }

    #[test]
    fn root_motion_hands_out_the_walk_and_keeps_the_pose_in_place() {
        let mut animator = root_animator(vec![walk_clip("walk", 2.0)]);
        animator.play(0);
        let mut travelled = Vec3::ZERO;
        for _ in 0..30 {
            animator.tick(1.0 / 60.0);
            travelled += animator.root_motion_delta();
            // 姿态里根骨骼的水平位置钉在开头。
            let position = animator.pose().entry(0).unwrap().position.unwrap();
            assert!(
                position.x.abs() < 1e-5 && position.z.abs() < 1e-5,
                "根骨骼没留在原地：{position}"
            );
        }
        assert!(
            (travelled.z - 1.0).abs() < 1e-3,
            "半秒应该走 1 米，走了 {travelled}"
        );
        assert_eq!(travelled.y, 0.0, "默认不抽竖直方向");
        // 起伏还在动画里。
        assert!(animator.pose().entry(0).unwrap().position.unwrap().y > 0.05);
    }

    #[test]
    fn root_motion_is_continuous_across_the_loop() {
        let mut animator = root_animator(vec![walk_clip("walk", 2.0)]);
        animator.play(0);
        let mut travelled = Vec3::ZERO;
        // 2.5 秒、帧长故意不整除剪辑：中间绕回开头两次。
        for _ in 0..(2.5 / 0.07) as usize {
            animator.tick(0.07);
            let step = animator.root_motion_delta();
            assert!(step.z > 0.0, "绕回开头那一帧不该倒退：{step}");
            travelled += step;
        }
        let time = (2.5 / 0.07f32).floor() * 0.07;
        assert!(
            (travelled.z - time * 2.0).abs() < 1e-3,
            "{time} 秒应该走 {} 米，走了 {}",
            time * 2.0,
            travelled.z
        );
    }

    #[test]
    fn root_motion_blends_with_the_weights() {
        let mut animator = root_animator(vec![walk_clip("walk", 2.0), walk_clip("run", 6.0)]);
        let walk = animator.add_state(0).unwrap();
        let run = animator.add_state(1).unwrap();
        animator.state_mut(walk).unwrap().set_weight(0.5);
        animator.state_mut(run).unwrap().set_weight(0.5);
        animator.tick(0.1);
        // 走 2 米/秒、跑 6 米/秒，各一半：4 米/秒 × 0.1 秒。
        assert!(
            (animator.root_motion_delta().z - 0.4).abs() < 1e-4,
            "{}",
            animator.root_motion_delta()
        );
    }

    #[test]
    fn without_root_motion_the_root_moves_as_animated() {
        let mut animator = Animator::new(Arc::new(vec![walk_clip("walk", 2.0)]));
        animator.play(0);
        animator.tick(0.5);
        assert_eq!(animator.root_motion_delta(), Vec3::ZERO);
        assert!((animator.pose().entry(0).unwrap().position.unwrap().z - 1.0).abs() < 1e-4);
    }
}
