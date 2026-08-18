//! 混音器：把一堆声源合成一段输出样本。
//!
//! **完全不碰硬件**——输入是声源与听者，输出是一段 `f32`。声卡那一侧
//! （`device` 模块）只负责把这段样本递出去。于是混音的正确性可以在
//! 没有声卡的机器上逐样本验证，而它正是「听起来对不对」的主要来源。
//!
//! # 增益在块之间要过渡
//!
//! 距离衰减与声像**每块算一次**，不是每样本算一次——后者要为每个样本做
//! 一次开方和两次三角函数，代价高得离谱。但直接在块边界换增益会产生
//! 「咔」的一声（声源移动越快越明显），所以每块内部把增益从上一块的值
//! **线性过渡**到这一块的目标值。几行代码，换掉一整类爆音。

use crate::{
    buffer::AudioBuffer,
    spatial::{Listener, Spatial, equal_power_pan},
};
use kcore::pool::{Handle, Pool};
use kmath::Vec3;

/// 一个声源的播放状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Status {
    /// 正在播放。
    #[default]
    Playing,
    /// 暂停。播放头停在原处，恢复时接着放。
    Paused,
    /// 已停止。非循环声源播完会自动进入此状态，随后被回收。
    Stopped,
}

/// 一个正在播放的声源。
#[derive(Debug, Clone)]
pub struct Sound {
    buffer: AudioBuffer,
    /// 世界空间位置。只有开了 [`spatial`](Self::spatial) 才有意义。
    pub position: Vec3,
    /// 音量倍率。
    pub gain: f32,
    /// 播放速度，同时改变音高。1 是原速。
    pub pitch: f32,
    /// 是否循环。
    pub looping: bool,
    /// 空间参数。为 [`None`] 时是 2D 声音：音量恒定、居中，背景音乐用。
    pub spatial: Option<Spatial>,
    status: Status,
    /// 播放头，单位是**源缓冲的帧**。用 `f64`，理由见 [`AudioBuffer::sample_lerp`]。
    playhead: f64,
    /// 上一块结束时的左右增益，用来做块间过渡。
    ///
    /// `None` 表示这是第一块——第一块直接用目标值，不做过渡，
    /// 否则每个声音开头都会有一段不该有的淡入。
    previous_gains: Option<[f32; 2]>,
}

impl Sound {
    /// 用一段缓冲创建声源，默认是不循环的 2D 声音。
    pub fn new(buffer: AudioBuffer) -> Self {
        Self {
            buffer,
            position: Vec3::ZERO,
            gain: 1.0,
            pitch: 1.0,
            looping: false,
            spatial: None,
            status: Status::Playing,
            playhead: 0.0,
            previous_gains: None,
        }
    }

    /// 指定音量。
    pub fn with_gain(mut self, gain: f32) -> Self {
        self.gain = gain.max(0.0);
        self
    }

    /// 指定播放速度（同时改变音高）。
    pub fn with_pitch(mut self, pitch: f32) -> Self {
        self.pitch = pitch.max(0.0);
        self
    }

    /// 设为循环。
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }

    /// 设为 3D 声音，并指定空间参数。
    pub fn with_spatial(mut self, position: Vec3, spatial: Spatial) -> Self {
        self.position = position;
        self.spatial = Some(spatial);
        self
    }

    /// 播放状态。
    pub fn status(&self) -> Status {
        self.status
    }

    /// 是否正在播放。
    pub fn is_playing(&self) -> bool {
        self.status == Status::Playing
    }

    /// 播放头所在的秒数。
    pub fn playback_position(&self) -> f32 {
        self.playhead as f32 / self.buffer.sample_rate() as f32
    }

    /// 音源时长。
    pub fn duration(&self) -> f32 {
        self.buffer.duration()
    }

    /// 继续播放。
    pub fn play(&mut self) {
        if self.status != Status::Stopped {
            self.status = Status::Playing;
        }
    }

    /// 暂停。播放头停在原处。
    pub fn pause(&mut self) {
        if self.status == Status::Playing {
            self.status = Status::Paused;
        }
    }

    /// 停止并倒回开头。停止的声源会在下一次混音时被回收。
    pub fn stop(&mut self) {
        self.status = Status::Stopped;
        self.playhead = 0.0;
    }

    /// 跳到指定秒数。
    pub fn seek(&mut self, seconds: f32) {
        self.playhead = (seconds.max(0.0) * self.buffer.sample_rate() as f32) as f64;
    }

    /// 这一块的目标左右增益。
    fn target_gains(&self, listener: &Listener) -> [f32; 2] {
        let base = self.gain * listener.gain;
        let Some(spatial) = self.spatial.as_ref() else {
            // 2D 声音：居中、不衰减。
            return [base, base];
        };

        let distance = (self.position - listener.position).length();
        let base = base * spatial.gain(distance);
        // `panning` 把声像往中间收：完全硬摆到单侧耳机听起来很不舒服。
        let pan = listener.pan_of(self.position) * spatial.panning;
        let [left, right] = equal_power_pan(pan);
        [base * left, base * right]
    }
}

/// 混音器。
#[derive(Debug)]
pub struct Mixer {
    sounds: Pool<Sound>,
    /// 听者。
    pub listener: Listener,
    /// 总音量。
    pub master_gain: f32,
    /// 输出采样率。
    sample_rate: u32,
}

impl Mixer {
    /// 按给定输出采样率创建。
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sounds: Pool::new(),
            listener: Listener::default(),
            master_gain: 1.0,
            sample_rate: sample_rate.max(1),
        }
    }

    /// 输出采样率。
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 加一个声源，返回它的句柄。
    pub fn add(&mut self, sound: Sound) -> Handle<Sound> {
        self.sounds.spawn(sound)
    }

    /// 移除一个声源。
    pub fn remove(&mut self, handle: Handle<Sound>) {
        if self.sounds.is_valid_handle(handle) {
            self.sounds.free(handle);
        }
    }

    /// 声源的只读引用。
    pub fn sound(&self, handle: Handle<Sound>) -> Option<&Sound> {
        self.sounds.try_borrow(handle).ok()
    }

    /// 声源的可变引用。
    pub fn sound_mut(&mut self, handle: Handle<Sound>) -> Option<&mut Sound> {
        self.sounds.try_borrow_mut(handle).ok()
    }

    /// 当前声源数量（含暂停的）。
    pub fn len(&self) -> usize {
        self.sounds.alive_count() as usize
    }

    /// 是否一个声源都没有。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 正在播放的声源数量。
    pub fn playing_count(&self) -> usize {
        self.sounds.iter().filter(|s| s.is_playing()).count()
    }

    /// 停掉并清空所有声源。
    pub fn clear(&mut self) {
        self.sounds.clear();
    }

    /// 把所有声源混进 `out`，**覆盖**其原有内容。
    ///
    /// `out` 是交错排列的，长度必须是 `channels` 的整倍数；多出来的尾巴会被清零。
    /// 支持 1 或 2 声道；更多声道时只写前两个，其余置零（见 [`Mixer`] 的文档）。
    ///
    /// 播完的非循环声源会在这里被回收。
    pub fn render(&mut self, out: &mut [f32], channels: u16) {
        out.fill(0.0);

        let channels = channels.max(1) as usize;
        let frames = out.len() / channels;
        if frames == 0 {
            return;
        }

        let (listener, master) = (self.listener, self.master_gain.max(0.0));
        let output_rate = self.sample_rate as f64;
        let mut finished = Vec::new();

        for (handle, sound) in self.sounds.pair_iter_mut() {
            if sound.status != Status::Playing || sound.buffer.is_empty() {
                continue;
            }

            let source_frames = sound.buffer.frame_count() as f64;
            // 源采样率与输出采样率的比值就是重采样系数；音高乘在上面。
            let step = sound.buffer.sample_rate() as f64 / output_rate * sound.pitch.max(0.0) as f64;

            let target = sound.target_gains(&listener);
            let target = [target[0] * master, target[1] * master];
            // 第一块直接用目标值：做过渡的话每个声音开头都会有一段不该有的淡入。
            let start = sound.previous_gains.unwrap_or(target);
            sound.previous_gains = Some(target);

            let spatialized = sound.spatial.is_some();
            let mut playhead = sound.playhead;
            let mut stopped = false;

            for frame in 0..frames {
                if playhead >= source_frames {
                    if sound.looping {
                        // 取模而不是归零：跳过的那点零头保住，
                        // 循环点才不会随块长抖动。
                        playhead %= source_frames.max(1.0);
                    } else {
                        stopped = true;
                        break;
                    }
                }

                // 块内线性过渡，把块边界的「咔」声抹掉。
                let t = frame as f32 / frames as f32;
                let left_gain = start[0] + (target[0] - start[0]) * t;
                let right_gain = start[1] + (target[1] - start[1]) * t;

                let base = out.len().min((frame + 1) * channels) - channels;
                if spatialized || sound.buffer.is_mono() {
                    // 3D 声音先塌成单声道再按方向摆，否则素材自带的左右
                    // 信息会和算出来的声像打架。
                    let sample = sound.buffer.mono_lerp(playhead);
                    out[base] += sample * left_gain;
                    if channels > 1 {
                        out[base + 1] += sample * right_gain;
                    }
                } else {
                    out[base] += sound.buffer.sample_lerp(playhead, 0) * left_gain;
                    if channels > 1 {
                        out[base + 1] += sound.buffer.sample_lerp(playhead, 1) * right_gain;
                    }
                }

                playhead += step;
            }

            sound.playhead = playhead;
            if stopped {
                sound.status = Status::Stopped;
                sound.playhead = 0.0;
                finished.push(handle);
            }
        }

        // 播完的非循环声源就地回收，调用方不必自己收尸。
        for handle in finished {
            self.sounds.free(handle);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::spatial::Attenuation;

    /// 一段恒为 1 的单声道缓冲，方便直接读出增益。
    fn constant(frames: usize, sample_rate: u32) -> AudioBuffer {
        AudioBuffer::new(vec![1.0; frames], 1, sample_rate)
    }

    /// 渲染一块并返回样本。
    fn render(mixer: &mut Mixer, frames: usize, channels: u16) -> Vec<f32> {
        let mut out = vec![0.0; frames * channels as usize];
        mixer.render(&mut out, channels);
        out
    }

    #[test]
    fn an_empty_mixer_outputs_silence() {
        let mut mixer = Mixer::new(48_000);
        let out = render(&mut mixer, 64, 2);

        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn render_overwrites_rather_than_accumulating() {
        // 不清零的话上一块的内容会留在缓冲里，听起来像一段无限回声。
        let mut mixer = Mixer::new(48_000);
        let mut out = vec![7.0; 32];

        mixer.render(&mut out, 2);

        assert!(out.iter().all(|s| *s == 0.0), "输出缓冲没被覆盖");
    }

    #[test]
    fn a_two_d_sound_plays_at_its_gain_in_both_channels() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(1000, 48_000)).with_gain(0.5));

        let out = render(&mut mixer, 16, 2);

        // 第一块不做过渡，全程就是目标增益。
        for frame in out.chunks_exact(2) {
            assert!((frame[0] - 0.5).abs() < 1e-6);
            assert!((frame[1] - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn several_sounds_sum_together() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(1000, 48_000)).with_gain(0.25));
        mixer.add(Sound::new(constant(1000, 48_000)).with_gain(0.25));

        let out = render(&mut mixer, 8, 2);

        assert!((out[0] - 0.5).abs() < 1e-6, "两个声源没有叠加：{}", out[0]);
    }

    #[test]
    fn the_master_gain_scales_everything() {
        let mut mixer = Mixer::new(48_000);
        mixer.master_gain = 0.5;
        mixer.add(Sound::new(constant(1000, 48_000)));

        assert!((render(&mut mixer, 8, 2)[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_paused_sound_contributes_nothing_but_keeps_its_playhead() {
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 48_000)));

        render(&mut mixer, 480, 2);
        let position = mixer.sound(handle).unwrap().playback_position();
        assert!(position > 0.0);

        mixer.sound_mut(handle).unwrap().pause();
        let out = render(&mut mixer, 480, 2);

        assert!(out.iter().all(|s| *s == 0.0), "暂停的声源还在出声");
        assert_eq!(mixer.sound(handle).unwrap().playback_position(), position);
    }

    #[test]
    fn a_finished_sound_is_reclaimed_automatically() {
        // 播完的音效要是不自己消失，调用方就得每帧扫一遍收尸。
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(64, 48_000)));
        assert_eq!(mixer.len(), 1);

        render(&mut mixer, 256, 2);

        assert_eq!(mixer.len(), 0, "播完的声源没被回收");
    }

    #[test]
    fn a_looping_sound_never_finishes() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(64, 48_000)).looping());

        for _ in 0..10 {
            render(&mut mixer, 256, 2);
        }

        assert_eq!(mixer.len(), 1);
        assert_eq!(mixer.playing_count(), 1);
    }

    #[test]
    fn a_looping_sound_keeps_producing_sound_past_the_end() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(64, 48_000)).looping());

        // 一块 256 帧远长于 64 帧的素材，中间必然绕好几圈。
        let out = render(&mut mixer, 256, 2);

        assert!(out.iter().all(|s| (*s - 1.0).abs() < 1e-5), "循环时出现了静音空档");
    }

    #[test]
    fn resampling_stretches_a_lower_rate_source() {
        // 24 kHz 的素材在 48 kHz 的设备上要放慢一倍，播放头每输出帧只走半帧。
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 24_000)));

        render(&mut mixer, 480, 2);

        // 走了 480 个输出帧 = 240 个源帧 = 源时间轴上的 0.01 秒。
        let position = mixer.sound(handle).unwrap().playback_position();
        assert!((position - 0.01).abs() < 1e-4, "播放头在 {position} 秒");
    }

    #[test]
    fn pitch_changes_how_fast_the_playhead_moves() {
        let mut mixer = Mixer::new(48_000);
        let normal = mixer.add(Sound::new(constant(48_000, 48_000)));
        let fast = mixer.add(Sound::new(constant(48_000, 48_000)).with_pitch(2.0));

        render(&mut mixer, 480, 2);

        let slow_position = mixer.sound(normal).unwrap().playback_position();
        let fast_position = mixer.sound(fast).unwrap().playback_position();
        assert!((fast_position - slow_position * 2.0).abs() < 1e-4);
    }

    #[test]
    fn zero_pitch_freezes_the_playhead_instead_of_dividing_by_zero() {
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 48_000)).with_pitch(0.0));

        render(&mut mixer, 480, 2);

        assert_eq!(mixer.sound(handle).unwrap().playback_position(), 0.0);
        assert_eq!(mixer.len(), 1, "音高为 0 不该被当成播完");
    }

    #[test]
    fn seeking_moves_the_playhead() {
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 48_000)));

        mixer.sound_mut(handle).unwrap().seek(0.5);

        assert!((mixer.sound(handle).unwrap().playback_position() - 0.5).abs() < 1e-4);
    }

    // ── 3D ──

    #[test]
    fn a_distant_sound_is_quieter_than_a_near_one() {
        let mut mixer = Mixer::new(48_000);
        let spatial = Spatial::default()
            .with_range(1.0, 100.0)
            .with_model(Attenuation::Inverse, 1.0)
            // 只看距离，把方向的影响排除掉。
            .with_panning(0.0);

        let near = mixer.add(Sound::new(constant(48_000, 48_000)).with_spatial(Vec3::Z * -1.0, spatial));
        render(&mut mixer, 8, 2);
        let near_level = render(&mut mixer, 8, 2)[0];
        mixer.remove(near);

        mixer.add(Sound::new(constant(48_000, 48_000)).with_spatial(Vec3::Z * -50.0, spatial));
        render(&mut mixer, 8, 2);
        let far_level = render(&mut mixer, 8, 2)[0];

        assert!(far_level < near_level * 0.2, "远处 {far_level} 对近处 {near_level}");
    }

    #[test]
    fn a_sound_on_the_right_is_louder_in_the_right_channel() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(
            Sound::new(constant(48_000, 48_000))
                .with_spatial(Vec3::X * 3.0, Spatial::default().with_range(10.0, 100.0)),
        );

        let out = render(&mut mixer, 8, 2);

        assert!(out[1] > out[0] * 2.0, "右侧的声音没有偏到右声道：{out:?}");
    }

    #[test]
    fn turning_the_listener_swaps_the_channels() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(
            Sound::new(constant(48_000, 48_000))
                .with_spatial(Vec3::X * 3.0, Spatial::default().with_range(10.0, 100.0)),
        );

        let facing = render(&mut mixer, 8, 2);
        mixer.listener.forward = Vec3::Z;
        // 换过朝向之后要再渲染一块，块间过渡才走完。
        render(&mut mixer, 64, 2);
        let turned = render(&mut mixer, 8, 2);

        assert!(facing[1] > facing[0], "转身前该偏右");
        assert!(turned[0] > turned[1], "转身后该偏左");
    }

    #[test]
    fn a_two_d_sound_ignores_the_listener_entirely() {
        // 背景音乐不该因为玩家走动而忽大忽小。
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(48_000, 48_000)));

        let before = render(&mut mixer, 8, 2)[0];
        mixer.listener.position = Vec3::splat(1000.0);
        let after = render(&mut mixer, 8, 2)[0];

        assert!((before - after).abs() < 1e-6);
    }

    #[test]
    fn gains_ramp_across_a_block_instead_of_jumping() {
        // 直接在块边界换增益会「咔」一声，声源移动越快越明显。
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 48_000)).with_gain(1.0));

        render(&mut mixer, 64, 2);
        mixer.sound_mut(handle).unwrap().gain = 0.0;
        let out = render(&mut mixer, 64, 2);

        // 这一块应当从 1 平滑滑到 0，而不是一上来就静音。
        assert!((out[0] - 1.0).abs() < 1e-5, "块首没有接住上一块的增益");
        assert!(out[out.len() - 2].abs() < 0.05, "块尾没有到达目标增益");
        // 单调下降，中间不该有跳变。
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        for pair in left.windows(2) {
            assert!(pair[1] <= pair[0] + 1e-6, "过渡不单调：{pair:?}");
        }
    }

    #[test]
    fn the_first_block_does_not_fade_in() {
        // 从 0 过渡到目标值的话，每个音效开头都会有一段不该有的淡入。
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(48_000, 48_000)));

        let out = render(&mut mixer, 64, 2);

        assert!((out[0] - 1.0).abs() < 1e-6, "第一块被淡入了：{}", out[0]);
    }

    // ── 边界 ──

    #[test]
    fn mono_output_is_supported() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(1000, 48_000)));

        let out = render(&mut mixer, 16, 1);

        assert!(out.iter().all(|s| (*s - 1.0).abs() < 1e-6));
    }

    #[test]
    fn an_empty_buffer_is_skipped_without_panicking() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(AudioBuffer::default()));

        let out = render(&mut mixer, 16, 2);

        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_zero_length_block_is_harmless() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(1000, 48_000)));

        let mut out = [];
        mixer.render(&mut out, 2);
    }

    #[test]
    fn a_ragged_buffer_length_does_not_read_out_of_bounds() {
        // 长度不是声道数整倍数时，尾巴要么被写满要么留零，但绝不能越界。
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(1000, 48_000)));

        let mut out = vec![0.0; 9];
        mixer.render(&mut out, 2);
    }

    #[test]
    fn stopping_a_sound_rewinds_it() {
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 48_000)));
        render(&mut mixer, 480, 2);

        mixer.sound_mut(handle).unwrap().stop();

        assert_eq!(mixer.sound(handle).unwrap().status(), Status::Stopped);
        assert_eq!(mixer.sound(handle).unwrap().playback_position(), 0.0);
    }

    #[test]
    fn a_stopped_sound_cannot_be_resumed_by_play() {
        // `stop` 的语义是「这个声音结束了」，`play` 不该把它救回来——
        // 想重放应当新建一个声源。
        let mut mixer = Mixer::new(48_000);
        let handle = mixer.add(Sound::new(constant(48_000, 48_000)));
        mixer.sound_mut(handle).unwrap().stop();
        mixer.sound_mut(handle).unwrap().play();

        assert_eq!(mixer.sound(handle).unwrap().status(), Status::Stopped);
    }

    #[test]
    fn clear_removes_everything() {
        let mut mixer = Mixer::new(48_000);
        mixer.add(Sound::new(constant(1000, 48_000)));
        mixer.add(Sound::new(constant(1000, 48_000)));

        mixer.clear();

        assert!(mixer.is_empty());
    }

    #[test]
    fn mixing_is_deterministic() {
        fn run() -> Vec<f32> {
            let mut mixer = Mixer::new(48_000);
            mixer.add(Sound::new(AudioBuffer::tone(440.0, 1.0, 44_100)).with_gain(0.4));
            mixer.add(
                Sound::new(AudioBuffer::tone(660.0, 1.0, 48_000))
                    .with_spatial(Vec3::new(2.0, 0.0, -3.0), Spatial::default()),
            );
            let mut out = vec![0.0; 2048];
            for _ in 0..4 {
                mixer.render(&mut out, 2);
            }
            out
        }

        assert_eq!(run(), run());
    }

    #[test]
    fn output_stays_finite_under_extreme_settings() {
        // 削波是可以接受的，NaN 不行——它会一路传进声卡，
        // 表现为一声巨响然后整个输出静音。
        let mut mixer = Mixer::new(48_000);
        mixer.master_gain = 1e6;
        mixer.add(
            Sound::new(AudioBuffer::tone(440.0, 0.1, 48_000))
                .with_gain(1e6)
                .with_pitch(1e6)
                .with_spatial(Vec3::ZERO, Spatial::default().with_range(1e-9, 1e-9)),
        );

        let out = render(&mut mixer, 512, 2);

        assert!(out.iter().all(|s| s.is_finite()), "输出里出现了 NaN 或 inf");
    }
}
