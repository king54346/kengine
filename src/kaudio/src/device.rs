//! 声卡输出。
//!
//! **全 crate 唯一碰硬件的地方**，作用与 `krender` 之于 wgpu、`kphysics` 之于
//! rapier 相同。混音器不知道声卡的存在，这里也不做任何 DSP，只负责把
//! [`Mixer`] 渲染出来的样本递给 cpal。
//!
//! # 没有声卡时不报错
//!
//! 拿不到设备（跑在 CI 上、用户拔了耳机、系统没装声卡）时进入**静默模式**：
//! 所有接口照常工作，声源状态照常推进，只是没有人来取样本。
//! 游戏逻辑因此不必写两套，也不会因为一台没声卡的机器就起不来。
//!
//! # 锁
//!
//! 音频回调在独立线程上跑，与游戏线程共享同一个 [`Mixer`]，中间隔一把互斥锁。
//! 在音频回调里加锁通常是禁忌，但这里成立：游戏线程的临界区只是改几个
//! `f32`（位置、增益），持锁时间以纳秒计，而音频缓冲有 10 毫秒的余量。
//! 用 `try_lock` 失败就输出静音反而更糟——那是**必然听得见**的断音，
//! 而等一下几乎必然听不见。`fyrox-sound` 也是这么做的。

use crate::mixer::Mixer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;

/// 声卡输出。
///
/// 拿着它就等于流在播；丢掉它流就停。
pub struct AudioDevice {
    /// 持有 cpal 的流。它一被丢弃播放就停止，所以必须留着。
    stream: Option<cpal::Stream>,
    mixer: Arc<Mutex<Mixer>>,
    sample_rate: u32,
    channels: u16,
    /// 设备名，拿不到设备时是 `None`。
    name: Option<String>,
}

impl std::fmt::Debug for AudioDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioDevice")
            .field("name", &self.name)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("silent", &self.stream.is_none())
            .finish()
    }
}

impl AudioDevice {
    /// 静默模式下假装的采样率。
    ///
    /// 没有真实设备时也得给混音器一个采样率，否则播放头无从推进。
    pub const FALLBACK_SAMPLE_RATE: u32 = 48_000;

    /// 打开默认输出设备。
    ///
    /// 失败时返回一个静默模式的设备，而不是错误——见模块文档。
    pub fn open() -> Self {
        match Self::try_open() {
            Ok(device) => device,
            Err(reason) => {
                klog::warn!("没有可用的音频输出（{reason}），进入静默模式");
                Self::silent()
            }
        }
    }

    /// 不接声卡，只跑混音。测试与无声卡环境用。
    pub fn silent() -> Self {
        Self {
            stream: None,
            mixer: Arc::new(Mutex::new(Mixer::new(Self::FALLBACK_SAMPLE_RATE))),
            sample_rate: Self::FALLBACK_SAMPLE_RATE,
            channels: 2,
            name: None,
        }
    }

    fn try_open() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "系统没有默认输出设备".to_string())?;
        let name = device.to_string();

        let supported = device
            .default_output_config()
            .map_err(|error| format!("读不到默认输出格式：{error}"))?;

        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let config = cpal::StreamConfig {
            channels,
            sample_rate: supported.sample_rate(),
            // 交给系统定：强行指定缓冲大小在部分后端上会直接建流失败，
            // 而默认值通常已经是延迟与稳定性之间调好的那个。
            buffer_size: cpal::BufferSize::Default,
        };

        let mixer = Arc::new(Mutex::new(Mixer::new(sample_rate)));
        let callback_mixer = mixer.clone();

        // 只处理 f32 输出。现代系统上默认格式几乎总是 f32；
        // 遇到别的格式宁可退回静默，也不要在这里铺开一堆样本格式转换。
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!(
                "输出格式是 {:?}，只支持 F32",
                supported.sample_format()
            ));
        }

        let stream = device
            .build_output_stream::<f32, _, _>(
                config,
                move |out: &mut [f32], _| {
                    callback_mixer.lock().render(out, channels);
                },
                |error| klog::error!("音频流出错：{error}"),
                None,
            )
            .map_err(|error| format!("建不出输出流：{error}"))?;

        stream
            .play()
            .map_err(|error| format!("流启动失败：{error}"))?;

        klog::info!("音频输出已就绪：{name}（{sample_rate} Hz / {channels} 声道）");

        Ok(Self {
            stream: Some(stream),
            mixer,
            sample_rate,
            channels,
            name: Some(name),
        })
    }

    /// 共享的混音器。改声源、改听者都通过它。
    ///
    /// 拿到的锁**要尽快放掉**：音频回调在另一条线程上等着它，
    /// 持锁做耗时的事会直接变成断音。
    pub fn mixer(&self) -> &Arc<Mutex<Mixer>> {
        &self.mixer
    }

    /// 输出采样率。
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 输出声道数。
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 设备名；静默模式下为 [`None`]。
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// 是否处于静默模式（没有真实输出）。
    pub fn is_silent(&self) -> bool {
        self.stream.is_none()
    }

    /// 暂停输出流。静默模式下什么都不做。
    pub fn pause(&self) {
        if let Some(stream) = &self.stream
            && let Err(error) = stream.pause()
        {
            klog::warn!("音频流暂停失败：{error}");
        }
    }

    /// 恢复输出流。
    pub fn resume(&self) {
        if let Some(stream) = &self.stream
            && let Err(error) = stream.play()
        {
            klog::warn!("音频流恢复失败：{error}");
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{AudioBuffer, Sound};

    #[test]
    fn a_silent_device_still_provides_a_working_mixer() {
        // 没声卡的机器上游戏也得能跑，逻辑不该写两套。
        let device = AudioDevice::silent();

        assert!(device.is_silent());
        assert!(device.name().is_none());
        assert_eq!(device.sample_rate(), AudioDevice::FALLBACK_SAMPLE_RATE);

        let mut mixer = device.mixer().lock();
        let handle = mixer.add(Sound::new(AudioBuffer::tone(440.0, 1.0, 48_000)));
        assert!(mixer.sound(handle).unwrap().is_playing());
    }

    #[test]
    fn pausing_a_silent_device_is_harmless() {
        let device = AudioDevice::silent();

        device.pause();
        device.resume();
    }

    #[test]
    fn the_mixer_handle_is_shared_not_copied() {
        // 音频回调改的必须是同一个混音器，否则游戏这边设的音量永远不生效。
        let device = AudioDevice::silent();
        let shared = device.mixer().clone();

        device
            .mixer()
            .lock()
            .add(Sound::new(AudioBuffer::tone(440.0, 0.1, 48_000)));

        assert_eq!(shared.lock().len(), 1);
    }

    #[test]
    fn opening_the_real_device_never_panics() {
        // 这台机器有没有声卡都行：有就用，没有就静默，但绝不能崩。
        let device = AudioDevice::open();

        assert!(device.sample_rate() > 0);
        assert!(device.channels() >= 1);
    }
}
