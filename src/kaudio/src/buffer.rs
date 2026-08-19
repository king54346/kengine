//! 解码后的音频数据。
//!
//! 统一存成**交错排列的 `f32`**：交错是声卡要的格式，`f32` 是混音要的格式，
//! 存成别的样子只是把转换推迟到每帧都要做的混音循环里。
//! 一分钟 48 kHz 立体声约 23 MB——短音效随便放，长音乐将来该走流式。

use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use std::sync::Arc;

/// [`AudioBuffer`] 的资源类型标识。
pub const AUDIO_BUFFER_TYPE_UUID: Uuid = uuid!("3c7f21a8-5e64-4b9d-8f13-6a2c95e07d4b");

/// 一段解码后的音频。
#[derive(Clone, PartialEq)]
pub struct AudioBuffer {
    /// 交错排列的样本：`[左0, 右0, 左1, 右1, …]`。
    ///
    /// 用 [`Arc`] 共享：混音器要把缓冲拿走自己持有，而一首歌有几十 MB，
    /// 每播放一次深拷贝一遍是不可接受的。与 `kmesh::Mesh` 同一个理由。
    samples: Arc<Vec<f32>>,
    channels: u16,
    sample_rate: u32,
}

impl std::fmt::Debug for AudioBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印样本数组，一首歌有几百万个。
        f.debug_struct("AudioBuffer")
            .field("frames", &self.frame_count())
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("seconds", &self.duration())
            .finish()
    }
}

impl Default for AudioBuffer {
    fn default() -> Self {
        Self::silence(0, 1, 48_000)
    }
}

impl AudioBuffer {
    /// 用交错样本构造。
    ///
    /// 样本数不是声道数的整倍数时，尾部凑不满一帧的部分会被丢掉——
    /// 留着的话最后一帧会读到别的声道的数据，听起来像一声爆音。
    pub fn new(mut samples: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        let channels = channels.max(1);
        let sample_rate = sample_rate.max(1);
        let usable = samples.len() / channels as usize * channels as usize;
        samples.truncate(usable);

        Self {
            samples: Arc::new(samples),
            channels,
            sample_rate,
        }
    }

    /// 一段静音。
    pub fn silence(frames: usize, channels: u16, sample_rate: u32) -> Self {
        let channels = channels.max(1);
        Self::new(vec![0.0; frames * channels as usize], channels, sample_rate)
    }

    /// 一段正弦波。主要用于测试与「没有音频资源时也能听到点声音」。
    pub fn tone(frequency: f32, seconds: f32, sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let frames = (seconds.max(0.0) * sample_rate as f32) as usize;
        let step = std::f32::consts::TAU * frequency / sample_rate as f32;

        let samples = (0..frames)
            .map(|index| (index as f32 * step).sin())
            .collect();
        Self::new(samples, 1, sample_rate)
    }

    /// 帧数（每帧含所有声道各一个样本）。
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// 声道数。
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 采样率。
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 时长，秒。
    pub fn duration(&self) -> f32 {
        self.frame_count() as f32 / self.sample_rate as f32
    }

    /// 是否没有任何样本。
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// 是否是单声道。
    ///
    /// 只有单声道才谈得上 3D 定位：立体声素材自带左右信息，
    /// 再按方向摆一次只会打架。
    pub fn is_mono(&self) -> bool {
        self.channels == 1
    }

    /// 全部交错样本。
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// 两份缓冲是否共享同一块样本内存。主要供测试断言克隆没有深拷贝。
    pub fn shares_data_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.samples, &other.samples)
    }

    /// 取第 `frame` 帧、第 `channel` 声道的样本。越界返回 0。
    pub fn sample_at(&self, frame: usize, channel: usize) -> f32 {
        if channel >= self.channels as usize {
            return 0.0;
        }
        self.samples
            .get(frame * self.channels as usize + channel)
            .copied()
            .unwrap_or(0.0)
    }

    /// 按**小数**帧位置取样，相邻两帧之间线性插值。
    ///
    /// 变调和采样率转换都会让播放头落在两帧之间。直接取整会产生刺耳的
    /// 阶梯噪声（俗称 zipper noise），线性插值几乎免费就能压掉大部分。
    ///
    /// 播放头用 `f64`：`f32` 的尾数只有 24 位，48 kHz 下播到第 350 秒
    /// 就开始丢整数精度，一首歌还没放完音高就飘了。
    pub fn sample_lerp(&self, frame: f64, channel: usize) -> f32 {
        let frames = self.frame_count();
        if frames == 0 || frame < 0.0 {
            return 0.0;
        }

        let index = frame.floor();
        let fraction = (frame - index) as f32;
        let index = index as usize;

        let current = self.sample_at(index, channel);
        // 最后一帧之后没有「下一帧」可插，直接用它自己，避免尾部滑向 0 的假淡出。
        let next = if index + 1 < frames {
            self.sample_at(index + 1, channel)
        } else {
            current
        };

        current + (next - current) * fraction
    }

    /// 把一帧混成单声道（各声道取平均）。
    ///
    /// 立体声素材要做 3D 定位时先塌成单声道，否则素材自带的左右信息
    /// 会和按方向算出来的声像叠在一起，声音会飘。
    pub fn mono_lerp(&self, frame: f64) -> f32 {
        let mut sum = 0.0;
        for channel in 0..self.channels as usize {
            sum += self.sample_lerp(frame, channel);
        }
        sum / self.channels as f32
    }

    /// 峰值幅度，用来检查是否削波。
    pub fn peak(&self) -> f32 {
        self.samples
            .iter()
            .fold(0.0f32, |peak, s| peak.max(s.abs()))
    }
}

impl ResourceData for AudioBuffer {
    fn type_uuid(&self) -> Uuid {
        AUDIO_BUFFER_TYPE_UUID
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_buffer_reports_its_shape() {
        let buffer = AudioBuffer::new(vec![0.0; 200], 2, 100);

        assert_eq!(buffer.frame_count(), 100);
        assert_eq!(buffer.channels(), 2);
        assert_eq!(buffer.duration(), 1.0);
        assert!(!buffer.is_mono());
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped() {
        // 留着的话最后一帧会读到别的声道的数据，听起来像一声爆音。
        let buffer = AudioBuffer::new(vec![1.0, 2.0, 3.0], 2, 48_000);

        assert_eq!(buffer.frame_count(), 1);
        assert_eq!(buffer.samples().len(), 2);
    }

    #[test]
    fn degenerate_parameters_are_clamped_instead_of_dividing_by_zero() {
        let buffer = AudioBuffer::new(vec![1.0, 2.0], 0, 0);

        assert_eq!(buffer.channels(), 1);
        assert_eq!(buffer.sample_rate(), 1);
        assert!(buffer.duration().is_finite());
    }

    #[test]
    fn samples_are_addressed_by_frame_and_channel() {
        let buffer = AudioBuffer::new(vec![1.0, -1.0, 2.0, -2.0], 2, 48_000);

        assert_eq!(buffer.sample_at(0, 0), 1.0);
        assert_eq!(buffer.sample_at(0, 1), -1.0);
        assert_eq!(buffer.sample_at(1, 0), 2.0);
        // 越界安静返回 0，而不是 panic：播放头跑过尾部是常态。
        assert_eq!(buffer.sample_at(9, 0), 0.0);
        assert_eq!(buffer.sample_at(0, 5), 0.0);
    }

    #[test]
    fn fractional_positions_are_interpolated() {
        let buffer = AudioBuffer::new(vec![0.0, 1.0], 1, 48_000);

        assert_eq!(buffer.sample_lerp(0.0, 0), 0.0);
        assert_eq!(buffer.sample_lerp(0.5, 0), 0.5);
        assert_eq!(buffer.sample_lerp(0.25, 0), 0.25);
        assert_eq!(buffer.sample_lerp(1.0, 0), 1.0);
    }

    #[test]
    fn the_last_frame_holds_instead_of_fading_to_zero() {
        // 尾部没有「下一帧」可插，滑向 0 会造出一段素材里本来没有的淡出。
        let buffer = AudioBuffer::new(vec![0.0, 1.0], 1, 48_000);

        assert_eq!(buffer.sample_lerp(1.5, 0), 1.0);
    }

    #[test]
    fn an_out_of_range_playhead_is_silent_not_a_panic() {
        let buffer = AudioBuffer::new(vec![1.0, 1.0], 1, 48_000);

        assert_eq!(buffer.sample_lerp(-1.0, 0), 0.0);
        assert_eq!(AudioBuffer::default().sample_lerp(0.0, 0), 0.0);
    }

    #[test]
    fn stereo_collapses_to_mono_by_averaging() {
        // 立体声素材要做 3D 定位时先塌成单声道，否则素材自带的左右信息
        // 会和按方向算出来的声像打架。
        let buffer = AudioBuffer::new(vec![1.0, 0.0], 2, 48_000);

        assert_eq!(buffer.mono_lerp(0.0), 0.5);
    }

    #[test]
    fn a_tone_has_the_expected_length_and_amplitude() {
        let tone = AudioBuffer::tone(440.0, 0.5, 48_000);

        assert_eq!(tone.frame_count(), 24_000);
        assert!(tone.is_mono());
        assert!((tone.duration() - 0.5).abs() < 1e-6);
        // 正弦波的峰值应当贴近 1，明显偏离说明生成有问题。
        assert!(
            tone.peak() > 0.99 && tone.peak() <= 1.0,
            "峰值 {}",
            tone.peak()
        );
    }

    #[test]
    fn a_tone_actually_oscillates_at_the_requested_frequency() {
        // 数过零点：440 Hz 跑 1 秒应当过零约 880 次。
        let tone = AudioBuffer::tone(440.0, 1.0, 48_000);
        let crossings = tone
            .samples()
            .windows(2)
            .filter(|w| w[0].signum() != w[1].signum())
            .count();

        assert!((crossings as i32 - 880).abs() <= 2, "过零 {crossings} 次");
    }

    #[test]
    fn cloning_shares_the_samples_instead_of_copying_them() {
        // 一首歌几十 MB，每播放一次深拷贝一遍是不可接受的。
        let buffer = AudioBuffer::tone(440.0, 1.0, 48_000);
        let copy = buffer.clone();

        assert!(buffer.shares_data_with(&copy));
    }

    #[test]
    fn silence_is_silent() {
        let buffer = AudioBuffer::silence(128, 2, 48_000);

        assert_eq!(buffer.frame_count(), 128);
        assert_eq!(buffer.peak(), 0.0);
    }
}
