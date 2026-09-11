//! UltraHDR：把 HDR 藏在一张普通 JPEG 里。
//!
//! # 它是怎么做到的
//!
//! 一个 UltraHDR 文件是**两张 JPEG 首尾相接**：
//!
//! 1. 一张普通的 SDR 图（老软件只会看到这一张，完全兼容）；
//! 2. 紧跟其后的一张灰度「增益图」（gain map），通常只有主图 1/4 大；
//! 3. 增益图的 XMP 里写着一组重建参数。
//!
//! 重建公式（Adobe / Google 的 hdrgm 1.0）：
//!
//! ```text
//! gain = 2^lerp(GainMapMin, GainMapMax, recovery^(1/Gamma))
//! HDR  = (SDR + OffsetSDR) * gain - OffsetHDR
//! ```
//!
//! 所以同一个文件在不显示 HDR 的地方就是张普通照片，在支持的地方能还原
//! 出十几档动态范围。文件大小只比 SDR 那张大几个百分点——`spruit_sunrise_2k`
//! 是 460 KB，同一张 `.hdr` 是 12 MB。
//!
//! # 两张图的分界怎么找
//!
//! 规范的做法是读 APP2 里的 MPF（多图格式）索引。这里用的是更简单的判据：
//! **扫第一个 `FF D9`**。JPEG 的熵编码数据里 `FF` 后面只可能跟 `00` 或
//! 复位标记，所以 `FF D9` 只会是真正的图像结束标记，不会在数据中间出现。
//! 找到它，后面那个 `FF D8` 就是增益图的开头。
//!
//! # 不支持
//!
//! ISO 21496-1 那套新的元数据（放在 APP2 而不是 XMP 里）、
//! 单通道以外的增益图（三通道各自独立的增益，规范允许但样本里没有）、
//! 以及 `BaseRenditionIsHDR="True"`（主图本身就是 HDR，反过来用增益图
//! 往下压）。遇到不认识的写法会退回「就当它是普通 JPEG」，
//! 得到一张 SDR 图而不是报错。

use crate::hdr::{HdrError, HdrImage};
use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::Uuid;
use std::{path::PathBuf, sync::Arc};

/// 增益图的重建参数，来自 XMP 的 `hdrgm:*`。
#[derive(Debug, Clone, Copy)]
pub struct GainMap {
    /// 增益的下界（以 2 为底的对数）。
    pub min: f32,
    /// 增益的上界（以 2 为底的对数）。
    pub max: f32,
    /// 增益图存储时用的伽马。
    pub gamma: f32,
    /// SDR 侧的偏移，避免纯黑处除零。
    pub offset_sdr: f32,
    /// HDR 侧的偏移。
    pub offset_hdr: f32,
}

impl Default for GainMap {
    /// 规范给的默认值。XMP 缺字段时用它。
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 1.0,
            gamma: 1.0,
            offset_sdr: 1.0 / 64.0,
            offset_hdr: 1.0 / 64.0,
        }
    }
}

/// 解一张 UltraHDR JPEG。
///
/// 没有增益图时退回「普通 JPEG 转成线性」——那是一张动态范围只有
/// `0..1` 的 [`HdrImage`]，不是错误。
pub fn decode(bytes: &[u8]) -> Result<HdrImage, HdrError> {
    let base = decode_jpeg(bytes)?;
    let Some(split) = gain_map_offset(bytes) else {
        return Ok(to_hdr(&base));
    };
    let tail = &bytes[split..];
    let parameters = parse_parameters(tail).unwrap_or_default();
    let Ok(gain) = decode_jpeg(tail) else {
        return Ok(to_hdr(&base));
    };

    let (width, height) = (base.0, base.1);
    let mut pixels = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            // 增益图通常比主图小，按双线性取值。最近邻会让高光边缘出现
            // 块状的台阶——增益是**指数**上的量，一档之差就是两倍亮度。
            let recovery = sample_bilinear(&gain, x as f32 / width as f32, y as f32 / height as f32);
            let level = recovery.powf(1.0 / parameters.gamma.max(1e-3));
            let log_gain = parameters.min + (parameters.max - parameters.min) * level;
            let gain = log_gain.exp2();
            let index = (y * width + x) * 3;
            for channel in 0..3 {
                let sdr = base.2[index + channel];
                let value = (sdr + parameters.offset_sdr) * gain - parameters.offset_hdr;
                pixels.push(value.max(0.0));
            }
        }
    }
    Ok(HdrImage::from_pixels(width, height, pixels))
}

/// 解码一张 JPEG 成 `(宽, 高, 线性 RGB)`。
///
/// sRGB → 线性的转换在这里做：JPEG 存的是显示空间的值，直接当作辐射度
/// 用会让暗部整体偏亮，增益图乘上去之后误差还会被放大。
fn decode_jpeg(bytes: &[u8]) -> Result<(usize, usize, Vec<f32>), HdrError> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|e| HdrError(e.to_string()))?
        .to_rgb8();
    let (width, height) = (image.width() as usize, image.height() as usize);
    let pixels = image
        .into_raw()
        .into_iter()
        .map(|value| srgb_to_linear(value as f32 / 255.0))
        .collect();
    Ok((width, height, pixels))
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn to_hdr(image: &(usize, usize, Vec<f32>)) -> HdrImage {
    HdrImage::from_pixels(image.0, image.1, image.2.clone())
}

/// 增益图（灰度）的双线性采样。`u`、`v` 在 `0..1`。
fn sample_bilinear(image: &(usize, usize, Vec<f32>), u: f32, v: f32) -> f32 {
    let (width, height, pixels) = image;
    if *width == 0 || *height == 0 {
        return 0.0;
    }
    let x = (u * *width as f32 - 0.5).max(0.0);
    let y = (v * *height as f32 - 0.5).max(0.0);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    // 增益图是灰度的，但 JPEG 解出来是三通道；取 R 即可。
    let at = |x: usize, y: usize| pixels[(y * width + x) * 3];
    let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
    let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
    // 采样用的是**线性化之后**的值，而增益图存的是「线性的 0..1 比例」
    // 而不是颜色。所以要把上面 `decode_jpeg` 做的 sRGB 转换还原回去。
    linear_to_srgb(top * (1.0 - fy) + bottom * fy)
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

/// 找增益图的起点：第一个 `FF D9` 之后的那个 `FF D8`，见模块文档。
fn gain_map_offset(bytes: &[u8]) -> Option<usize> {
    let end = bytes.windows(2).position(|w| w == [0xFF, 0xD9])? + 2;
    let rest = bytes.get(end..)?;
    let start = rest.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF])?;
    Some(end + start)
}

/// 从增益图的 XMP 里读 `hdrgm:*`。
fn parse_parameters(bytes: &[u8]) -> Option<GainMap> {
    // XMP 在 APP1 里，直接在前若干 KB 的原文里找属性即可——
    // 为几个浮点数接一个完整的 XML 解析器不划算。
    let head = &bytes[..bytes.len().min(16 * 1024)];
    let text = String::from_utf8_lossy(head);
    if !text.contains("hdrgm:") {
        return None;
    }
    let value = |name: &str| -> Option<f32> {
        let needle = format!("hdrgm:{name}=\"");
        let start = text.find(&needle)? + needle.len();
        let end = start + text[start..].find('"')?;
        text[start..end].trim().parse().ok()
    };
    let default = GainMap::default();
    Some(GainMap {
        min: value("GainMapMin").unwrap_or(default.min),
        max: value("GainMapMax").unwrap_or(default.max),
        gamma: value("Gamma").unwrap_or(default.gamma),
        offset_sdr: value("OffsetSDR").unwrap_or(default.offset_sdr),
        offset_hdr: value("OffsetHDR").unwrap_or(default.offset_hdr),
    })
}

/// 把 `.jpg` 当 UltraHDR 读的加载器。
///
/// # 注意它会盖住 [`TextureLoader`](ktexture::TextureLoader)
///
/// 资源管理器按**扩展名**找加载器、后注册的优先，不看请求的类型。
/// 注册了这个之后，同一个管理器里所有 `.jpg` 都会走 HDR 这条路。
/// 需要同时读普通 JPEG 贴图时，别注册它，直接调 [`decode`]。
#[derive(Debug, Default, Clone, Copy)]
pub struct UltraHdrLoader;

impl ResourceLoader for UltraHdrLoader {
    fn extensions(&self) -> &[&str] {
        &["jpg", "jpeg"]
    }

    fn data_type_uuid(&self) -> Uuid {
        crate::loader::HDR_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let image = decode(&bytes).map_err(LoadError::custom)?;
            klog::debug!(
                "UltraHDR 已解码：{}（{}×{}）",
                path.display(),
                image.width(),
                image.height()
            );
            Ok(Box::new(image) as Box<dyn ResourceData>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_is_the_first_end_of_image_marker() {
        // 主图 + EOI + 增益图的开头。
        let bytes = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xFF, 0xD9, 0xFF, 0xD8, 0xFF, 0xE1,
        ];
        assert_eq!(gain_map_offset(&bytes), Some(8));
    }

    #[test]
    fn a_plain_jpeg_has_no_gain_map() {
        assert_eq!(gain_map_offset(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xD9]), None);
    }

    #[test]
    fn xmp_parameters_are_read_and_missing_ones_fall_back() {
        let xmp = br#"<x:xmpmeta hdrgm:GainMapMin="0" hdrgm:GainMapMax="15.9991" hdrgm:Gamma="1" hdrgm:OffsetSDR="0.015625"/>"#;
        let parameters = parse_parameters(xmp).unwrap();
        assert_eq!(parameters.max, 15.9991);
        assert_eq!(parameters.gamma, 1.0);
        assert_eq!(parameters.offset_sdr, 0.015625);
        // 没写 OffsetHDR，取默认值。
        assert_eq!(parameters.offset_hdr, GainMap::default().offset_hdr);
    }

    #[test]
    fn a_document_without_hdrgm_is_not_a_gain_map() {
        assert!(parse_parameters(b"<x:xmpmeta>nothing here</x:xmpmeta>").is_none());
    }

    #[test]
    fn the_srgb_transfer_function_round_trips() {
        for value in [0.0f32, 0.002, 0.1, 0.5, 1.0] {
            let round = linear_to_srgb(srgb_to_linear(value));
            assert!((round - value).abs() < 1e-4, "{value} → {round}");
        }
    }
}
