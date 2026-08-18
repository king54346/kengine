//! 音频解码与资源加载。
//!
//! 解码交给 `symphonia`（纯 Rust，支持 WAV / OGG-Vorbis），
//! 与 `ktexture` 用现成的 PNG / JPEG 解码器是同一个取舍：格式解析是标准化的
//! 苦力活，自己写既不会更快也不会更对。
//!
//! 解出来一律转成**交错的 `f32`**，因为混音器只认这一种（见 [`AudioBuffer`]）。

use crate::buffer::AudioBuffer;
use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::Uuid;
use std::{path::PathBuf, sync::Arc};
use symphonia::core::{
    audio::Audio,
    codecs::audio::AudioDecoderOptions,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

/// 解码一段音频字节。
///
/// `extension` 只是给格式探测当提示用；探测本身以内容为准，
/// 所以扩展名写错了也照样能解出来。
pub fn decode(bytes: Vec<u8>, extension: &str) -> Result<AudioBuffer, String> {
    let source = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());

    let mut hint = Hint::new();
    if !extension.is_empty() {
        hint.with_extension(extension);
    }

    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| format!("认不出这是什么音频格式：{error}"))?;

    let track = reader
        .first_track(TrackType::Audio)
        .ok_or_else(|| "文件里没有音频轨".to_string())?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| "音频轨没有编解码参数".to_string())?
        .clone();

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|error| format!("没有能解这个编码的解码器：{error}"))?;

    let mut samples = Vec::new();
    let mut chunk = Vec::new();
    // 采样率与声道数从**解码结果**里读，而不是从容器头里读：
    // 有些文件的头信息与实际数据对不上，以实际数据为准才不会放快或放慢。
    let mut sample_rate = params.sample_rate.unwrap_or(48_000);
    let mut channels = 1u16;

    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            // 正常读到结尾。
            Ok(None) => break,
            Err(error) => return Err(format!("读音频包出错：{error}")),
        };
        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = decoded.spec();
                sample_rate = spec.rate();
                channels = spec.channels().count() as u16;

                chunk.clear();
                decoded.copy_to_vec_interleaved(&mut chunk);
                samples.extend_from_slice(&chunk);
            }
            // 单个包解不出来时跳过而不是整体失败：一段损坏的音频里
            // 绝大部分内容通常仍然是好的，丢一小段比整个文件用不了强。
            Err(error) => klog::warn!("跳过一个解不开的音频包：{error}"),
        }
    }

    if samples.is_empty() {
        return Err("解出来一个样本都没有".to_string());
    }

    Ok(AudioBuffer::new(samples, channels.max(1), sample_rate))
}

/// [`AudioBuffer`] 的资源加载器。
#[derive(Debug, Default, Clone, Copy)]
pub struct AudioLoader;

impl ResourceLoader for AudioLoader {
    fn extensions(&self) -> &[&str] {
        &["wav", "ogg"]
    }

    fn data_type_uuid(&self) -> Uuid {
        crate::buffer::AUDIO_BUFFER_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();

            let buffer = decode(bytes, &extension).map_err(LoadError::message)?;
            klog::debug!(
                "音频已解码：{}（{:.2} 秒 / {} Hz / {} 声道）",
                path.display(),
                buffer.duration(),
                buffer.sample_rate(),
                buffer.channels()
            );
            Ok(Box::new(buffer) as Box<dyn ResourceData>)
        })
    }
}

/// 把一段音频编成 16 位 PCM 的 WAV 字节。
///
/// 引擎不需要导出音频，但**测试需要**：有了它就能在测试里现造一个真实的
/// WAV 文件走一遍完整的解码路径，而不必往仓库里塞二进制素材。
pub fn encode_wav(buffer: &AudioBuffer) -> Vec<u8> {
    let channels = buffer.channels();
    let sample_rate = buffer.sample_rate();
    let bits_per_sample = 16u16;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_len = (buffer.samples().len() * 2) as u32;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // 子块大小
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in buffer.samples() {
        // 夹一下再转：超出 ±1 的样本直接乘会绕回去，变成刺耳的爆音。
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod test {
    use super::*;
    use kasset::{MemoryResourceIo, ResourceManager};

    #[test]
    fn a_wav_survives_an_encode_decode_roundtrip() {
        let original = AudioBuffer::tone(440.0, 0.25, 48_000);
        let decoded = decode(encode_wav(&original), "wav").expect("自己编的 WAV 应当解得开");

        assert_eq!(decoded.sample_rate(), 48_000);
        assert_eq!(decoded.channels(), 1);
        assert_eq!(decoded.frame_count(), original.frame_count());

        // 16 位量化的误差上限是 1/32768，放宽一点留给取整。
        for (index, (a, b)) in original.samples().iter().zip(decoded.samples()).enumerate() {
            assert!((a - b).abs() < 1e-3, "第 {index} 个样本差得太多：{a} vs {b}");
        }
    }

    #[test]
    fn stereo_wav_keeps_its_channels_and_interleaving() {
        // 左声道恒 1、右声道恒 -1：交错读错的话两边会串。
        let samples: Vec<f32> = (0..200)
            .map(|index| if index % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        let original = AudioBuffer::new(samples, 2, 44_100);

        let decoded = decode(encode_wav(&original), "wav").unwrap();

        assert_eq!(decoded.channels(), 2);
        assert_eq!(decoded.sample_rate(), 44_100);
        assert!((decoded.sample_at(0, 0) - 0.5).abs() < 1e-3);
        assert!((decoded.sample_at(0, 1) + 0.5).abs() < 1e-3);
    }

    #[test]
    fn a_wrong_extension_hint_still_decodes() {
        // 探测以内容为准，扩展名只是提示。
        let original = AudioBuffer::tone(220.0, 0.05, 48_000);

        assert!(decode(encode_wav(&original), "ogg").is_ok());
        assert!(decode(encode_wav(&original), "").is_ok());
    }

    #[test]
    fn garbage_is_rejected_with_a_readable_reason() {
        let error = decode(b"this is definitely not audio".to_vec(), "wav").unwrap_err();

        assert!(!error.is_empty());
    }

    #[test]
    fn an_empty_file_is_rejected() {
        assert!(decode(Vec::new(), "wav").is_err());
    }

    #[test]
    fn clipping_is_clamped_rather_than_wrapped() {
        // 超出 ±1 的样本直接转成 i16 会绕回去，变成刺耳的爆音。
        let loud = AudioBuffer::new(vec![5.0, -5.0], 1, 48_000);
        let decoded = decode(encode_wav(&loud), "wav").unwrap();

        assert!(decoded.sample_at(0, 0) > 0.9, "正向削波绕回去了");
        assert!(decoded.sample_at(1, 0) < -0.9, "负向削波绕回去了");
    }

    #[test]
    fn the_loader_plugs_into_the_resource_manager() {
        let wav = encode_wav(&AudioBuffer::tone(440.0, 0.1, 48_000));
        let io = MemoryResourceIo::new().with("beep.wav", wav);

        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(AudioLoader);

        let resource = manager
            .request_blocking::<AudioBuffer>("beep.wav")
            .expect("音频资源该能加载");
        let buffer = resource.data_ref().unwrap();

        assert_eq!(buffer.sample_rate(), 48_000);
        assert!((buffer.duration() - 0.1).abs() < 1e-3);
    }

    #[test]
    fn the_loader_claims_the_expected_extensions() {
        assert!(AudioLoader.extensions().contains(&"wav"));
        assert!(AudioLoader.extensions().contains(&"ogg"));
    }

    #[test]
    fn a_broken_file_fails_the_resource_rather_than_the_process() {
        let io = MemoryResourceIo::new().with("broken.wav", b"nonsense".to_vec());
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(AudioLoader);

        assert!(manager.request_blocking::<AudioBuffer>("broken.wav").is_err());
    }
}
