//! 视频纹理：把 MP4（H.264）一帧帧解码成 RGBA，贴到材质上（three.js 的 `VideoTexture` / `VideoFrameTexture`）。
//!
//! ```no_run
//! use kvideo::VideoPlayer;
//!
//! let mut player = VideoPlayer::open("examples/threejs/textures/sintel.mp4").unwrap();
//! let mut texture = player.texture();
//! // 每帧：
//! # let dt = 1.0 / 60.0;
//! if let Some(frame) = player.advance(dt) {
//!     texture = texture.with_pixels(frame.to_vec()); // 同一个 id，渲染器原地重写显存
//! }
//! ```
//!
//! # 支持什么
//!
//! - 容器：MP4（`mp4` crate）。WebM / Ogg 不支持。
//! - 编码：H.264，解码器是 OpenH264（源码随 crate 编译，不依赖系统里的 ffmpeg）。Baseline 和 Main 档
//!   实测可用（`sintel.mp4` 是 Baseline，`pano.mp4` 是 Main）；OpenH264 对 High 档的支持不完整，遇到解不了的
//!   帧会重建解码器、跳到下一个关键帧（画面会顿一下），日志里有警告。真解不了就转码成 Baseline：
//!   `ffmpeg -i in.mp4 -c:v libx264 -profile:v baseline out.mp4`。
//! - 没有声音。
//!
//! # 特性
//!
//! 解码器 OpenH264 是 C++，随 crate 用 `cc` 编译，要本机有 C/C++ 编译器——这和引擎「`cargo build` 只要 Rust」
//! 的取舍冲突，所以放在 **`h264` 特性**后面、默认关。没开时 [`VideoPlayer::open`] 返回错误，
//! 其余（MP4 拆包、Annex B 转换）照常编译。根 crate 的 `video` 特性会打开它。
//!
//! 解码在调用 [`advance`](VideoPlayer::advance) 的线程上同步做：一帧 720p 大约几毫秒。

use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::path::Path;

use ktexture::{Sampler, Texture, TextureFormat};

/// 打开 / 解码视频时的错误。
#[derive(Debug)]
pub struct VideoError(String);

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "视频：{}", self.0)
    }
}

impl Error for VideoError {}

fn err(message: impl Into<String>) -> VideoError {
    VideoError(message.into())
}

/// 一路 H.264 视频的播放器：按时间推进、解码、循环。
pub struct VideoPlayer {
    reader: mp4::Mp4Reader<Cursor<Vec<u8>>>,
    track_id: u32,
    sample_count: u32,
    /// 下一个要解码的样本号（从 1 开始）。
    next_sample: u32,
    decoder: backend::Decoder,
    /// NAL 长度前缀的字节数（avcC 里的 lengthSizeMinusOne + 1）。
    length_size: usize,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    width: u32,
    height: u32,
    frame_rate: f64,
    /// 播放到第几秒了。
    clock: f64,
    /// 已经显示到第几帧（从 0 起）。
    shown: u64,
    rgba: Vec<u8>,
    /// 播完是否从头再来。
    pub looping: bool,
    /// 播放速度倍率。
    pub speed: f64,
    /// 暂停时 [`advance`](Self::advance) 不推进。
    pub paused: bool,
    annexb: Vec<u8>,
    /// 解码出错之后：换一个新解码器，跳到下一个关键帧再继续（坏帧之后的参考帧都不可信了）。
    wait_for_sync: bool,
}

impl VideoPlayer {
    /// 打开一个 MP4 文件（整个读进内存）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VideoError> {
        let bytes = std::fs::read(path.as_ref())
            .map_err(|e| err(format!("读不到 {}：{e}", path.as_ref().display())))?;
        Self::from_bytes(bytes)
    }

    /// 从内存里的 MP4 数据建播放器。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, VideoError> {
        let size = bytes.len() as u64;
        let reader = mp4::Mp4Reader::read_header(Cursor::new(bytes), size)
            .map_err(|e| err(format!("不是能读的 MP4：{e}")))?;
        let (track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| matches!(track.media_type(), Ok(mp4::MediaType::H264)))
            .ok_or_else(|| err("没有 H.264 视频轨"))?;
        let avcc = &track
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .avc1
            .as_ref()
            .ok_or_else(|| err("视频轨没有 avc1 配置"))?
            .avcc;
        let length_size = usize::from(avcc.length_size_minus_one & 3) + 1;
        let sps = avcc
            .sequence_parameter_sets
            .iter()
            .map(|s| s.bytes.clone())
            .collect();
        let pps = avcc
            .picture_parameter_sets
            .iter()
            .map(|s| s.bytes.clone())
            .collect();
        let width = u32::from(track.width());
        let height = u32::from(track.height());
        let frame_rate = track.frame_rate();
        let sample_count = track.sample_count();
        let track_id = *track_id;
        let decoder = backend::Decoder::new()?;
        Ok(Self {
            reader,
            track_id,
            sample_count,
            next_sample: 1,
            decoder,
            length_size,
            sps,
            pps,
            width: width.max(2),
            height: height.max(2),
            frame_rate: if frame_rate.is_finite() && frame_rate > 0.0 {
                frame_rate
            } else {
                30.0
            },
            clock: 0.0,
            shown: 0,
            rgba: vec![0; (width.max(2) * height.max(2) * 4) as usize],
            looping: true,
            speed: 1.0,
            paused: false,
            annexb: Vec::new(),
            wait_for_sync: false,
        })
    }

    /// 画面尺寸（像素）。以视频轨头里写的为准，解出来的帧尺寸不同时以帧为准。
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 帧率。
    pub fn frame_rate(&self) -> f64 {
        self.frame_rate
    }

    /// 时长（秒）。
    pub fn duration(&self) -> f64 {
        f64::from(self.sample_count) / self.frame_rate
    }

    /// 当前播放到的时间（秒）。
    pub fn time(&self) -> f64 {
        self.clock
    }

    /// 一张和视频同尺寸的贴图（当前画面；还没解过帧时是黑的）。sRGB、线性过滤、边缘夹住、不建 mip。
    pub fn texture(&self) -> Texture {
        Texture::new(self.width, self.height, self.rgba.clone())
            .with_format(TextureFormat::Srgb)
            .with_sampler(Sampler {
                mipmaps: false,
                ..Sampler::data()
            })
    }

    /// 当前画面的 RGBA8 像素（逐行，从上到下）。
    pub fn pixels(&self) -> &[u8] {
        &self.rgba
    }

    /// 时间往前走 `dt` 秒；到了下一帧就解码，返回新画面的像素（没到就返回 `None`）。
    /// 一次跨过好几帧时只解码、不逐帧返回，交出的是最后一帧。
    pub fn advance(&mut self, dt: f64) -> Option<&[u8]> {
        if self.paused {
            return None;
        }
        self.clock += dt.max(0.0) * self.speed;
        let wanted = (self.clock * self.frame_rate).floor() as u64;
        // 第一次调用也要出第 0 帧。
        let mut decoded = false;
        while self.shown < wanted + 1 {
            match self.decode_next() {
                Ok(true) => {
                    self.shown += 1;
                    decoded = true;
                }
                Ok(false) => {
                    if !self.looping || self.sample_count == 0 {
                        return decoded.then_some(self.rgba.as_slice());
                    }
                    // 播完：从头来（解码器状态也得清，下一个样本是关键帧）。
                    self.rewind();
                    return decoded.then_some(self.rgba.as_slice());
                }
                Err(error) => {
                    klog::warn!("{error}");
                    self.shown += 1;
                }
            }
        }
        decoded.then_some(self.rgba.as_slice())
    }

    /// 回到开头。
    pub fn rewind(&mut self) {
        self.next_sample = 1;
        self.clock = 0.0;
        self.shown = 0;
        if let Ok(decoder) = backend::Decoder::new() {
            self.decoder = decoder;
        }
        self.wait_for_sync = false;
    }

    /// 解下一个样本。`Ok(false)` = 没有样本了。
    fn decode_next(&mut self) -> Result<bool, VideoError> {
        loop {
            if self.next_sample > self.sample_count {
                return Ok(false);
            }
            let id = self.next_sample;
            self.next_sample += 1;
            let Some(sample) = self
                .reader
                .read_sample(self.track_id, id)
                .map_err(|e| err(format!("第 {id} 帧读不出来：{e}")))?
            else {
                continue;
            };
            if self.wait_for_sync {
                if !sample.is_sync {
                    continue;
                }
                self.wait_for_sync = false;
            }
            to_annex_b(
                &sample.bytes,
                self.length_size,
                &self.sps,
                &self.pps,
                &mut self.annexb,
            );
            match self.decoder.decode(&self.annexb, &mut self.rgba) {
                Ok(Some((w, h))) => {
                    self.width = w;
                    self.height = h;
                    return Ok(true);
                }
                // 解码器还在攒参考帧，接着喂下一个。
                Ok(None) => continue,
                Err(e) => {
                    // 解码器坏了状态（缺参考帧、参数集丢了）之后每一帧都会接着失败：重建，等下一个关键帧。
                    self.decoder = backend::Decoder::new()?;
                    self.wait_for_sync = true;
                    return Err(err(format!("第 {id} 帧解码失败，跳到下一个关键帧：{e}")));
                }
            }
        }
    }
}

/// H.264 解码后端。开了 `h264` 特性是 OpenH264；没开时是个占位，`new` 直接报错——
/// 这样 `kvideo` 默认是纯 Rust 的（MP4 拆包照常能用），只有要放视频的程序才需要 C++ 编译器。
#[cfg(feature = "h264")]
mod backend {
    use super::{VideoError, err};
    use openh264::OpenH264API;
    use openh264::decoder::{DecoderConfig, Flush};
    use openh264::formats::YUVSource;

    pub(super) struct Decoder(openh264::decoder::Decoder);

    impl Decoder {
        /// 不在每次解码后强制刷新：带 B 帧的码流要攒几帧再按显示顺序吐出来，强制刷新会把参考帧状态搞乱
        /// （Main 档的 `pano.mp4` 在第 9 帧报 0x4000，之后每帧「缺参数集」）。
        pub(super) fn new() -> Result<Self, VideoError> {
            openh264::decoder::Decoder::with_api_config(
                OpenH264API::from_source(),
                DecoderConfig::new().flush_after_decode(Flush::NoFlush),
            )
            .map(Self)
            .map_err(|e| err(format!("OpenH264 起不来：{e}")))
        }

        /// 喂一个 Annex B 包；出了一帧就写进 `rgba`（尺寸跟着变）并返回宽高，还在攒参考帧时返回 `None`。
        pub(super) fn decode(
            &mut self,
            annexb: &[u8],
            rgba: &mut Vec<u8>,
        ) -> Result<Option<(u32, u32)>, String> {
            match self.0.decode(annexb) {
                Ok(Some(image)) => {
                    let (w, h) = image.dimensions();
                    rgba.resize(w * h * 4, 0);
                    image.write_rgba8(rgba);
                    Ok(Some((w as u32, h as u32)))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

#[cfg(not(feature = "h264"))]
mod backend {
    use super::{VideoError, err};

    pub(super) struct Decoder;

    impl Decoder {
        pub(super) fn new() -> Result<Self, VideoError> {
            Err(err(
                "kvideo 编译时没开 `h264` 特性（OpenH264 要 C++ 编译器，默认关）。根 crate 用 `--features video` 打开",
            ))
        }

        pub(super) fn decode(
            &mut self,
            _annexb: &[u8],
            _rgba: &mut Vec<u8>,
        ) -> Result<Option<(u32, u32)>, String> {
            Err("没有 H.264 解码器".into())
        }
    }
}

/// MP4 里的样本是「长度前缀 + NAL」；OpenH264 要的是「起始码 + NAL」（Annex B）。
/// 关键帧前面补上 avcC 里的 SPS / PPS——MP4 把它们放在容器头里，码流里没有。
fn to_annex_b(
    sample: &[u8],
    length_size: usize,
    sps: &[Vec<u8>],
    pps: &[Vec<u8>],
    out: &mut Vec<u8>,
) {
    out.clear();
    let mut nals = Vec::new();
    let mut rest = sample;
    let mut has_idr = false;
    let mut has_parameter_sets = false;
    while rest.len() > length_size {
        let length = rest[..length_size]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        rest = &rest[length_size..];
        if length == 0 || length > rest.len() {
            break;
        }
        let nal = &rest[..length];
        match nal[0] & 0x1f {
            5 => has_idr = true,
            7 | 8 => has_parameter_sets = true,
            _ => {}
        }
        nals.push(nal);
        rest = &rest[length..];
    }
    if has_idr && !has_parameter_sets {
        for set in sps.iter().chain(pps) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(set);
        }
    }
    for nal in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefixed_nals_become_start_codes_with_parameter_sets_before_keyframes() {
        // 一个关键帧切片（类型 5）+ 一个 SEI（类型 6），2 字节长度前缀。
        let sample = [0, 2, 0x65, 0xAA, 0, 1, 0x06];
        let mut out = Vec::new();
        to_annex_b(&sample, 2, &[vec![0x67, 1]], &[vec![0x68, 2]], &mut out);
        assert_eq!(
            out,
            [
                0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x06
            ]
        );
        // 非关键帧不补。
        to_annex_b(&[0, 1, 0x41], 2, &[vec![0x67]], &[vec![0x68]], &mut out);
        assert_eq!(out, [0, 0, 0, 1, 0x41]);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn the_sample_video_decodes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/threejs/textures/sintel.mp4");
        if !path.exists() {
            return;
        }
        let mut player = VideoPlayer::open(&path).unwrap();
        let (w, h) = player.size();
        assert!(w > 0 && h > 0);
        let frame = player.advance(0.0).expect("第 0 帧").to_vec();
        assert_eq!(
            frame.len(),
            (player.size().0 * player.size().1 * 4) as usize
        );
        // 一秒以后画面变了。
        let later = player.advance(1.0).expect("一秒后的帧").to_vec();
        assert_ne!(frame, later);
        // 往后走几秒，总有不是一片黑的画面（片头是黑底白字）。
        let mut brightest = 0u32;
        for _ in 0..20 {
            if let Some(frame) = player.advance(0.5) {
                let mean = frame
                    .chunks_exact(4)
                    .map(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]))
                    .sum::<u32>()
                    / (frame.len() as u32 / 4);
                brightest = brightest.max(mean);
            }
        }
        assert!(brightest > 60, "解出来全是黑的");
    }
}
