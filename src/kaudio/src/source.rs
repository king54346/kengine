//! 音频数据源抽象。
//!
//! [`AudioSource`] 是混音器对所有声音数据的统一接口——无论是一段
//! 已经全部解码进内存的缓冲（短音效），还是实时从磁盘按块解码的流（BGM），
//! 混音器都只调 `fill`，不知道背后是哪种。
//!
//! # 两种实现
//!
//! | 类型 | 适用场景 | 内存用量 |
//! |------|----------|----------|
//! | [`BufferedSource`] | 短音效（< 10 s）| 整段在内存 |
//! | [`StreamingSource`] | 长音乐（BGM）| 2 s 的环形缓冲 |
//!
//! 调用方不必自己判断，[`AudioLoader`] 已经按文件长度自动选择。

use crate::buffer::AudioBuffer;
use std::{
    collections::VecDeque,
    io::Cursor,
    sync::{Arc, Mutex},
};
use symphonia::core::{
    codecs::audio::AudioDecoderOptions,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

// ── trait ──────────────────────────────────────────────────────────────

/// 统一的音频数据来源。混音器通过这个接口拉样本，不关心底层是缓冲还是流。
pub trait AudioSource: Send + 'static {
    /// 输出采样率（Hz）。
    fn sample_rate(&self) -> u32;

    /// 声道数（1 = 单声道，2 = 立体声）。
    fn channels(&self) -> u16;

    /// 总帧数。流式来源返回 `None`——解完才知道有多长。
    fn frame_count(&self) -> Option<u64>;

    /// 用尽力填充 `out`，返回**实际写入的帧数**。
    ///
    /// `out` 以交错排列的帧为单位，长度应为声道数的整倍数。
    /// 到达结尾时只写剩余的部分，其余补零。
    /// 如果是循环的流，实现应当自行绕回并继续填，直到 `out` 满。
    fn fill(&mut self, out: &mut [f32]) -> usize;

    /// 跳到第 `frame` 帧。超出范围时实现自行夹住或忽略。
    fn seek(&mut self, frame: u64);

    /// 当前播放头所在帧。
    fn position(&self) -> u64;

    /// 是否到达结尾（非循环时有意义）。
    fn is_finished(&self) -> bool;
}

// ── BufferedSource ─────────────────────────────────────────────────────

/// 把一段完整解码的 [`AudioBuffer`] 包成 [`AudioSource`]。
///
/// 短音效（< 10 秒）走这条路：整段在内存，读取没有任何阻塞。
/// 克隆只增加 [`Arc`] 的引用计数，几乎是零开销。
#[derive(Clone)]
pub struct BufferedSource {
    buffer: AudioBuffer,
    /// 播放头，单位是**帧**。
    position: u64,
    /// 是否循环。
    pub looping: bool,
}

impl BufferedSource {
    /// 从缓冲创建，默认不循环。
    pub fn new(buffer: AudioBuffer) -> Self {
        Self {
            buffer,
            position: 0,
            looping: false,
        }
    }

    /// 设为循环。
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }

    /// 内部缓冲的引用。
    pub fn buffer(&self) -> &AudioBuffer {
        &self.buffer
    }
}

impl AudioSource for BufferedSource {
    fn sample_rate(&self) -> u32 {
        self.buffer.sample_rate()
    }

    fn channels(&self) -> u16 {
        self.buffer.channels()
    }

    fn frame_count(&self) -> Option<u64> {
        Some(self.buffer.frame_count() as u64)
    }

    fn fill(&mut self, out: &mut [f32]) -> usize {
        let ch = self.buffer.channels() as usize;
        if ch == 0 || out.is_empty() {
            return 0;
        }

        let frames_requested = out.len() / ch;
        let total_frames = self.buffer.frame_count() as u64;
        let mut written = 0usize;

        while written < frames_requested {
            if self.position >= total_frames {
                if self.looping && total_frames > 0 {
                    self.position = 0;
                } else {
                    break;
                }
            }

            let remaining_in_buf = (total_frames - self.position) as usize;
            let to_copy = (frames_requested - written).min(remaining_in_buf);
            let src_start = self.position as usize * ch;
            let dst_start = written * ch;

            out[dst_start..dst_start + to_copy * ch]
                .copy_from_slice(&self.buffer.samples()[src_start..src_start + to_copy * ch]);

            self.position += to_copy as u64;
            written += to_copy;
        }

        // 剩余的部分（到达结尾后）补零。
        out[written * ch..].fill(0.0);
        written
    }

    fn seek(&mut self, frame: u64) {
        let total = self.buffer.frame_count() as u64;
        self.position = frame.min(total);
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn is_finished(&self) -> bool {
        !self.looping && self.position >= self.buffer.frame_count() as u64
    }
}

// ── StreamingSource ────────────────────────────────────────────────────

/// 环形缓冲大小（帧数，约 2 秒 44.1 kHz 立体声）。
///
/// 后台线程预读的量。太小会因调度抖动产生断音，太大会让 seek 要等更久。
const RING_FRAMES: usize = 88_200;

/// 流式音频来源：后台线程按块解码，游戏线程从环形缓冲取样本。
///
/// 适合 BGM——整首歌几十 MB，不应该全部解进内存。
///
/// # 内部结构
///
/// ```text
/// 解码线程 ─→ Arc<Shared> ─→ 游戏线程（fill）
///              ├── ring: VecDeque<f32>（已解码但未消费）
///              ├── finished: bool（流读完了）
///              └── seek_to: Option<u64>（待 seek 的帧号）
/// ```
///
/// 解码线程发现 ring 满（> RING_FRAMES 帧）时休眠 5 ms，然后重试。
/// `fill` 从 ring 里弹出所需样本；不够时用静音填充（宁可卡一帧也不 panic）。
pub struct StreamingSource {
    shared: Arc<Mutex<Shared>>,
    sample_rate: u32,
    channels: u16,
    position: u64,
    looping: bool,
}

struct Shared {
    /// 已解码、待消费的交错样本。
    ring: VecDeque<f32>,
    /// 解码线程已经读完整个文件。
    finished: bool,
    /// 非 None 时，解码线程在下一轮应当 seek 到这个帧再继续解。
    seek_to: Option<u64>,
    /// 解码线程遇到了错误。
    error: Option<String>,
    /// 解码线程通知：停止（drop 时设置）。
    stop: bool,
}

impl StreamingSource {
    /// 从已经读好的字节和扩展名创建流式源。
    ///
    /// 立刻在后台线程开始解码，前 2 秒的数据会在 `fill` 被调用之前就绪。
    /// 失败时返回 `Err`（格式不识别 / 解码器缺失）。
    pub fn new(bytes: Vec<u8>, extension: &str, looping: bool) -> Result<Self, String> {
        // 先 probe 一次，拿到采样率和声道数，同时验证格式合法。
        let (sample_rate, channels) = probe_format(&bytes, extension)?;

        let shared = Arc::new(Mutex::new(Shared {
            ring: VecDeque::with_capacity(RING_FRAMES * channels as usize * 2),
            finished: false,
            seek_to: None,
            error: None,
            stop: false,
        }));

        // 后台解码线程。
        let shared_clone = Arc::clone(&shared);
        let extension = extension.to_string();
        std::thread::Builder::new()
            .name("kaudio-stream".into())
            .spawn(move || {
                decode_thread(bytes, &extension, channels, shared_clone);
            })
            .map_err(|e| format!("无法启动音频解码线程：{e}"))?;

        Ok(Self {
            shared,
            sample_rate,
            channels,
            position: 0,
            looping,
        })
    }
}

impl Drop for StreamingSource {
    fn drop(&mut self) {
        // 通知后台线程退出，避免线程泄漏。
        if let Ok(mut g) = self.shared.lock() {
            g.stop = true;
        }
    }
}

impl AudioSource for StreamingSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn frame_count(&self) -> Option<u64> {
        // 流式来源长度未知，直到解完才能确定。
        None
    }

    fn fill(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels as usize;
        if ch == 0 || out.is_empty() {
            return 0;
        }

        let frames_requested = out.len() / ch;

        // 如果需要循环，且 ring 是空的，且后台线程已经结束，
        // 则重新触发 seek 到 0，让后台线程重头开始。
        {
            let mut g = self.shared.lock().unwrap();
            if self.looping && g.finished && g.ring.is_empty() {
                g.finished = false;
                g.seek_to = Some(0);
            }
        }

        let written = {
            let mut g = self.shared.lock().unwrap();
            let available_samples = g.ring.len();
            let available_frames = available_samples / ch;
            let can_write = frames_written_available(available_frames, frames_requested);

            for dst in &mut out[..can_write * ch] {
                *dst = g.ring.pop_front().unwrap_or(0.0);
            }
            can_write
        };


        // ring 里不够用静音填充。
        out[written * ch..].fill(0.0);
        self.position += written as u64;
        written
    }

    fn seek(&mut self, frame: u64) {
        self.position = frame;
        if let Ok(mut g) = self.shared.lock() {
            g.seek_to = Some(frame);
            g.ring.clear();
            g.finished = false;
        }
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn is_finished(&self) -> bool {
        if self.looping {
            return false;
        }
        let g = self.shared.lock().unwrap();
        g.finished && g.ring.is_empty()
    }
}

// ── 内部工具函数 ──────────────────────────────────────────────────────

fn frames_written_available(available: usize, requested: usize) -> usize {
    available.min(requested)
}

/// 快速 probe：只读元数据，不启动解码器。
fn probe_format(bytes: &[u8], extension: &str) -> Result<(u32, u16), String> {
    let source =
        MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    if !extension.is_empty() {
        hint.with_extension(extension);
    }

    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("无法识别音频格式：{e}"))?;

    let track = reader
        .first_track(TrackType::Audio)
        .ok_or_else(|| "文件里没有音频轨".to_string())?;

    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "音频轨没有编解码参数".to_string())?;

    let sample_rate = params.sample_rate.unwrap_or(48_000);
    let channels = params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(2)
        .max(1);

    Ok((sample_rate, channels))
}

/// 后台解码线程的入口。
///
/// 循环：
/// 1. 检查 `stop` / `seek_to` 信号。
/// 2. ring 满（> RING_FRAMES 帧）时休眠 5 ms 让消费者消耗。
/// 3. 解下一个 packet，把样本推进 ring。
fn decode_thread(bytes: Vec<u8>, extension: &str, channels: u16, shared: Arc<Mutex<Shared>>) {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if !extension.is_empty() {
        hint.with_extension(extension);
    }

    let mut reader = match symphonia::default::get_probe().probe(
        &hint,
        source,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) {
        Ok(r) => r,
        Err(e) => {
            if let Ok(mut g) = shared.lock() {
                g.error = Some(format!("流式解码：probe 失败：{e}"));
                g.finished = true;
            }
            return;
        }
    };

    let track = match reader.first_track(TrackType::Audio) {
        Some(t) => t,
        None => {
            if let Ok(mut g) = shared.lock() {
                g.finished = true;
            }
            return;
        }
    };
    let track_id = track.id;
    let params = match track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .cloned()
    {
        Some(p) => p,
        None => {
            if let Ok(mut g) = shared.lock() {
                g.finished = true;
            }
            return;
        }
    };

    let mut decoder = match symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
    {
        Ok(d) => d,
        Err(e) => {
            if let Ok(mut g) = shared.lock() {
                g.error = Some(format!("流式解码：无法创建解码器：{e}"));
                g.finished = true;
            }
            return;
        }
    };

    let mut chunk: Vec<f32> = Vec::new();
    let ch = channels as usize;

    loop {
        // ① 检查停止信号。
        {
            let g = shared.lock().unwrap();
            if g.stop {
                return;
            }
        }

        // ② ring 满时主动让出（不能无限制往里塞，否则内存无界增长）。
        {
            let g = shared.lock().unwrap();
            if g.ring.len() >= RING_FRAMES * ch {
                drop(g);
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
        }

        // ③ 处理 seek 信号（目前实现：symphonia 不支持帧级 seek，
        //    所以 seek_to != 0 时只能重置解码器从头重新解。
        //    未来可以用 symphonia 的 seek_track 近似到关键帧。）
        let seek_to = {
            let mut g = shared.lock().unwrap();
            g.seek_to.take()
        };
        if let Some(_frame) = seek_to {
            // 重新打开解码器，从头解直到目标帧（暂时简单实现：只支持 seek 到 0）。
            // 后续可以用 reader.seek() 精确跳帧。
            if let Ok(mut g) = shared.lock() {
                g.ring.clear();
                g.finished = false;
            }
            // 重置解码器状态（symphonia 暂无 reset，重建一个）。
            decoder = match symphonia::default::get_codecs()
                .make_audio_decoder(&params, &AudioDecoderOptions::default())
            {
                Ok(d) => d,
                Err(_) => return,
            };
            continue;
        }

        // ④ 解下一个 packet。
        let packet = match reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => {
                // 正常到达文件末尾。
                if let Ok(mut g) = shared.lock() {
                    g.finished = true;
                }
                return;
            }
            Err(e) => {
                if let Ok(mut g) = shared.lock() {
                    g.error = Some(format!("流式解码：读包出错：{e}"));
                    g.finished = true;
                }
                return;
            }
        };

        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                chunk.clear();
                decoded.copy_to_vec_interleaved(&mut chunk);
                let mut g = shared.lock().unwrap();
                g.ring.extend(chunk.iter().copied());
            }
            Err(e) => {
                klog::warn!("流式解码：跳过一个解不开的包：{e}");
            }
        }
    }
}

// ── 测试 ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::AudioBuffer, decode::encode_wav};

    fn wav_bytes(duration_secs: f32, sample_rate: u32) -> Vec<u8> {
        encode_wav(&AudioBuffer::tone(440.0, duration_secs, sample_rate))
    }

    #[test]
    fn buffered_source_fills_the_correct_number_of_frames() {
        let buf = AudioBuffer::tone(440.0, 1.0, 48_000);
        let mut src = BufferedSource::new(buf);

        let mut out = vec![0.0f32; 480]; // 480 帧
        let written = src.fill(&mut out);

        assert_eq!(written, 480);
    }

    #[test]
    fn buffered_source_stops_at_end_when_not_looping() {
        let buf = AudioBuffer::new(vec![1.0; 100], 1, 48_000); // 100 帧
        let mut src = BufferedSource::new(buf);

        let mut out = vec![0.0f32; 200];
        let written = src.fill(&mut out);

        assert_eq!(written, 100, "只有 100 帧，不该写超");
        assert!(out[100..].iter().all(|s| *s == 0.0), "末尾应补零");
    }

    #[test]
    fn buffered_source_loops_indefinitely_when_looping() {
        let buf = AudioBuffer::new(vec![1.0; 100], 1, 48_000);
        let mut src = BufferedSource::new(buf).looping();

        let mut out = vec![0.0f32; 300];
        let written = src.fill(&mut out);

        assert_eq!(written, 300, "循环模式应填满整个 out");
        assert!(out.iter().all(|s| (*s - 1.0).abs() < 1e-6), "循环值应全是 1.0");
    }

    #[test]
    fn buffered_source_seek_repositions_playhead() {
        let buf = AudioBuffer::new(vec![1.0; 1000], 1, 48_000);
        let mut src = BufferedSource::new(buf);

        src.seek(500);
        assert_eq!(src.position(), 500);

        let mut out = vec![0.0f32; 100];
        let written = src.fill(&mut out);
        assert_eq!(written, 100);
        assert_eq!(src.position(), 600);
    }

    #[test]
    fn buffered_source_is_finished_after_playing_through() {
        let buf = AudioBuffer::new(vec![1.0; 50], 1, 48_000);
        let mut src = BufferedSource::new(buf);

        let mut out = vec![0.0f32; 100];
        src.fill(&mut out);

        assert!(src.is_finished());
    }

    #[test]
    fn streaming_source_creates_successfully_from_wav() {
        let bytes = wav_bytes(0.1, 48_000);
        let src = StreamingSource::new(bytes, "wav", false);
        assert!(src.is_ok(), "合法的 WAV 应当能创建流式源：{:?}", src.err());
    }

    #[test]
    fn streaming_source_rejects_garbage() {
        let result = StreamingSource::new(b"this is not audio".to_vec(), "wav", false);
        assert!(result.is_err(), "垃圾数据应当失败");
    }

    #[test]
    fn streaming_source_fills_output_after_a_brief_wait() {
        let bytes = wav_bytes(2.0, 48_000);
        let mut src = StreamingSource::new(bytes, "wav", false).unwrap();

        // 等后台线程填满一些数据。
        std::thread::sleep(std::time::Duration::from_millis(200));

        let mut out = vec![0.0f32; 4800]; // 0.1 秒 @ 48 kHz 单声道
        let written = src.fill(&mut out);

        // 至少应当解到了一点数据（可能少于 4800，取决于解码速度）。
        assert!(written > 0, "流式源应当写出了至少一帧");
    }
}
