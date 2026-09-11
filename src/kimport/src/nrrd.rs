//! NRRD 体数据（医学影像常用的「裸数组 + 文本头」格式）。
//!
//! # 支持
//!
//! `encoding` 为 `raw` / `gzip`（`gz`）/ `ascii`（`text` / `txt`）；
//! `type` 为 `int8`…`int64` 各种整型与 `float` / `double`；三维
//! `sizes`；`endian`；`space directions` 里的体素间距。
//!
//! # 不支持
//!
//! `encoding: bzip2` / `hex`；`data file:` 分离式（头和数据在两个文件里）；
//! 四维及以上（多分量、时间序列）；`space` 的旋转——只取三个方向向量的
//! 长度当作各轴的体素尺寸，斜置的采样网格会被当成正交的。
//!
//! # 为什么值要归一化
//!
//! CT 的值域是 −1024…3071 的 Hounsfield 数，MRI 又是另一套量纲。
//! [`Volume::voxels`] 一律归一化到 `0..1`，原始范围记在
//! [`Volume::range`] 里。渲染只关心相对强度，而归一化之后
//! 窗宽窗位（window / level）就是一对普通的 0..1 参数，不必为每种
//! 模态各写一套。

use crate::{bad, limits};
use kasset::LoadError;
use kmath::Vec3;
use ktexture::{Sampler, Texture, TextureFormat};
use std::io::Read;

/// 一份三维体数据。
#[derive(Debug, Clone)]
pub struct Volume {
    /// 三个轴上的体素数。
    pub size: [usize; 3],
    /// 每个体素在世界里的尺寸。
    pub spacing: Vec3,
    /// 归一化到 `0..1` 的体素值，按 `x + y * sx + z * sx * sy` 排列。
    pub voxels: Vec<f32>,
    /// 归一化之前的原始值域 `(最小, 最大)`。
    pub range: (f32, f32),
}

impl Volume {
    /// 体数据在世界里的物理尺寸。
    pub fn extent(&self) -> Vec3 {
        Vec3::new(
            self.size[0] as f32 * self.spacing.x,
            self.size[1] as f32 * self.spacing.y,
            self.size[2] as f32 * self.spacing.z,
        )
    }

    /// 取一个体素，越界返回 0。
    pub fn at(&self, x: usize, y: usize, z: usize) -> f32 {
        if x >= self.size[0] || y >= self.size[1] || z >= self.size[2] {
            return 0.0;
        }
        self.voxels[x + y * self.size[0] + z * self.size[0] * self.size[1]]
    }

    /// 打成一张**纹理数组**：每个 Z 切片一层。
    ///
    /// 引擎没有三维纹理，但有 `custom_texture_array`（二维数组）。
    /// 体渲染的着色器沿视线步进时，把 Z 坐标拆成「层号 + 层内插值」
    /// 自己做三线性插值的第三个维度——硬件只在层**内**做双线性，
    /// 层**间**要手动 lerp 两层的采样结果。
    ///
    /// 四个通道装同一个灰度值：着色器里取哪个通道都行，也省掉为单通道
    /// 纹理单开一条格式分支。层数超过 `max_layers` 时按步长抽取，
    /// 免得一份 512 层的数据直接吃掉几百 MB 显存。
    pub fn to_texture_array(&self, max_layers: usize) -> Texture {
        let [sx, sy, sz] = self.size;
        let step = sz.div_ceil(max_layers.max(1)).max(1);
        let layers: Vec<usize> = (0..sz).step_by(step).collect();
        let mut data = Vec::with_capacity(sx * sy * layers.len() * 4);
        for &z in &layers {
            for y in 0..sy {
                for x in 0..sx {
                    let value = (self.at(x, y, z) * 255.0).clamp(0.0, 255.0) as u8;
                    data.extend_from_slice(&[value, value, value, 255]);
                }
            }
        }
        Texture::array(sx as u32, sy as u32, layers.len() as u32, data)
            // 体数据是**数据**不是颜色，走 sRGB 会把灰度整体压暗。
            .with_format(TextureFormat::Linear)
            .with_sampler(Sampler::default())
    }
}

/// 解析 NRRD。
pub fn parse(bytes: &[u8]) -> Result<Volume, LoadError> {
    if !bytes.starts_with(b"NRRD") {
        return Err(bad("不是 NRRD 文件"));
    }
    // 头以一个空行结束。行尾可能是 LF 也可能是 CRLF，两种都要认。
    let body = find_body(bytes).ok_or_else(|| bad("NRRD 头部没有以空行结束"))?;
    let header = String::from_utf8_lossy(&bytes[..body]);

    let mut sizes: Vec<usize> = Vec::new();
    let mut kind = String::new();
    let mut encoding = String::from("raw");
    let mut big_endian = false;
    let mut spacing = Vec3::ONE;
    let mut dimension = 0usize;

    for line in header.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "type" => kind = value.to_ascii_lowercase(),
            "dimension" => dimension = value.parse().unwrap_or(0),
            "sizes" => {
                sizes = value
                    .split_whitespace()
                    .filter_map(|token| token.parse().ok())
                    .collect()
            }
            "encoding" => encoding = value.to_ascii_lowercase(),
            "endian" => big_endian = value.eq_ignore_ascii_case("big"),
            "space directions" => spacing = parse_spacing(value),
            "spacings" => {
                let values: Vec<f32> = value
                    .split_whitespace()
                    .filter_map(|token| token.parse().ok())
                    .collect();
                if values.len() >= 3 {
                    spacing = Vec3::new(values[0], values[1], values[2]);
                }
            }
            _ => {}
        }
    }

    if dimension != 0 && dimension != 3 {
        return Err(bad(format!("只支持三维 NRRD，这份是 {dimension} 维")));
    }
    if sizes.len() != 3 || sizes.iter().any(|&n| n == 0) {
        return Err(bad("NRRD 的 sizes 不是三个正整数"));
    }
    let count = sizes
        .iter()
        .try_fold(1usize, |acc, &n| acc.checked_mul(n))
        .ok_or_else(|| bad("NRRD 的体素数溢出"))?;
    if count > limits::VERTICES {
        return Err(bad("NRRD 的体素数超过上限"));
    }

    let scalar = Scalar::parse(&kind).ok_or_else(|| bad(format!("未知的 NRRD 类型 {kind}")))?;
    let raw = match encoding.as_str() {
        "raw" => bytes[body..].to_vec(),
        "gzip" | "gz" => {
            let mut decoded = Vec::with_capacity(count * scalar.size());
            flate2::read::GzDecoder::new(&bytes[body..])
                // 上限按声明的体素数算：解压炸弹解到这里就停。
                .take((count * scalar.size()) as u64)
                .read_to_end(&mut decoded)
                .map_err(LoadError::custom)?;
            decoded
        }
        "ascii" | "text" | "txt" => {
            let text = String::from_utf8_lossy(&bytes[body..]);
            let values: Vec<f32> = text
                .split_whitespace()
                .take(count)
                .filter_map(|token| token.parse().ok())
                .collect();
            return finish(sizes, spacing, values);
        }
        other => return Err(bad(format!("不支持的 NRRD 编码 {other}"))),
    };
    if raw.len() < count * scalar.size() {
        return Err(bad("NRRD 的数据不足以填满声明的尺寸"));
    }
    let values = (0..count)
        .map(|index| scalar.read(&raw[index * scalar.size()..], big_endian))
        .collect();
    finish(sizes, spacing, values)
}

fn finish(sizes: Vec<usize>, spacing: Vec3, values: Vec<f32>) -> Result<Volume, LoadError> {
    let count = sizes[0] * sizes[1] * sizes[2];
    if values.len() < count {
        return Err(bad("NRRD 的数据不足以填满声明的尺寸"));
    }
    let mut voxels = values;
    voxels.truncate(count);
    let (min, max) = voxels
        .iter()
        .filter(|v| v.is_finite())
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let span = if max > min { max - min } else { 1.0 };
    for value in &mut voxels {
        *value = if value.is_finite() {
            (*value - min) / span
        } else {
            0.0
        };
    }
    Ok(Volume {
        size: [sizes[0], sizes[1], sizes[2]],
        spacing: if spacing.is_finite() && spacing.min_element() > 0.0 {
            spacing
        } else {
            Vec3::ONE
        },
        voxels,
        range: (min, max),
    })
}

/// 找头部之后的正文起点：一个空行（`\n\n` 或 `\r\n\r\n`）。
fn find_body(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
        .or_else(|| bytes.windows(2).position(|w| w == b"\n\n").map(|at| at + 2))
}

/// `space directions: (1,0,0) (0,1,0) (0,0,1)` → 每轴的体素尺寸。
///
/// 只取向量长度，丢掉方向：斜置采样网格会被当成正交的，见模块文档。
fn parse_spacing(value: &str) -> Vec3 {
    let mut axes = [1.0f32; 3];
    for (index, group) in value
        .split(')')
        .filter_map(|group| group.split_once('(').map(|(_, rest)| rest))
        .take(3)
        .enumerate()
    {
        let components: Vec<f32> = group
            .split(',')
            .filter_map(|token| token.trim().parse().ok())
            .collect();
        if components.len() >= 3 {
            axes[index] = Vec3::new(components[0], components[1], components[2]).length();
        }
    }
    Vec3::from_array(axes)
}

#[derive(Clone, Copy)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Scalar {
    fn parse(name: &str) -> Option<Self> {
        // NRRD 允许一堆同义词：`unsigned char` / `uchar` / `uint8` 都是一个东西。
        Some(match name.replace(' ', "").as_str() {
            "signedchar" | "int8" | "int8_t" | "char" => Self::I8,
            "unsignedchar" | "uchar" | "uint8" | "uint8_t" => Self::U8,
            "short" | "shortint" | "signedshort" | "int16" | "int16_t" => Self::I16,
            "ushort" | "unsignedshort" | "uint16" | "uint16_t" => Self::U16,
            "int" | "signedint" | "int32" | "int32_t" => Self::I32,
            "uint" | "unsignedint" | "uint32" | "uint32_t" => Self::U32,
            "longlong" | "int64" | "int64_t" => Self::I64,
            "ulonglong" | "uint64" | "uint64_t" => Self::U64,
            "float" => Self::F32,
            "double" => Self::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
        }
    }

    fn read(self, bytes: &[u8], big_endian: bool) -> f32 {
        macro_rules! number {
            ($ty:ty, $n:literal) => {{
                let raw: [u8; $n] = bytes[..$n].try_into().unwrap();
                if big_endian {
                    <$ty>::from_be_bytes(raw) as f32
                } else {
                    <$ty>::from_le_bytes(raw) as f32
                }
            }};
        }
        match self {
            Self::I8 => bytes[0] as i8 as f32,
            Self::U8 => bytes[0] as f32,
            Self::I16 => number!(i16, 2),
            Self::U16 => number!(u16, 2),
            Self::I32 => number!(i32, 4),
            Self::U32 => number!(u32, 4),
            Self::I64 => number!(i64, 8),
            Self::U64 => number!(u64, 8),
            Self::F32 => number!(f32, 4),
            Self::F64 => number!(f64, 8),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_volume() -> Vec<u8> {
        let mut bytes =
            b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 2 2 2\nencoding: raw\nendian: little\nspace directions: (2,0,0) (0,3,0) (0,0,4)\n\n"
                .to_vec();
        bytes.extend_from_slice(&[0, 32, 64, 96, 128, 160, 192, 255]);
        bytes
    }

    #[test]
    fn reads_raw_and_normalises() {
        let volume = parse(&raw_volume()).unwrap();
        assert_eq!(volume.size, [2, 2, 2]);
        assert_eq!(volume.range, (0.0, 255.0));
        assert_eq!(volume.at(0, 0, 0), 0.0);
        assert_eq!(volume.at(1, 1, 1), 1.0);
    }

    #[test]
    fn space_directions_become_spacing() {
        let volume = parse(&raw_volume()).unwrap();
        assert_eq!(volume.spacing, Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(volume.extent(), Vec3::new(4.0, 6.0, 8.0));
    }

    #[test]
    fn reads_gzip_bodies() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&[0, 32, 64, 96, 128, 160, 192, 255]).unwrap();
        let payload = encoder.finish().unwrap();
        let mut bytes =
            b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 2 2 2\nencoding: gzip\n\n".to_vec();
        bytes.extend_from_slice(&payload);
        assert_eq!(parse(&bytes).unwrap().at(1, 1, 1), 1.0);
    }

    #[test]
    fn reads_ascii_bodies() {
        let bytes =
            b"NRRD0004\ntype: short\ndimension: 3\nsizes: 2 1 1\nencoding: ascii\n\n-10 10\n";
        let volume = parse(bytes).unwrap();
        assert_eq!(volume.range, (-10.0, 10.0));
        assert_eq!(volume.at(0, 0, 0), 0.0);
        assert_eq!(volume.at(1, 0, 0), 1.0);
    }

    #[test]
    fn the_layer_array_subsamples_deep_volumes() {
        let volume = parse(&raw_volume()).unwrap();
        assert_eq!(volume.to_texture_array(64).layers(), 2);
        assert_eq!(volume.to_texture_array(1).layers(), 1);
    }

    #[test]
    fn a_truncated_body_is_rejected() {
        let mut bytes = raw_volume();
        bytes.truncate(bytes.len() - 4);
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn four_dimensional_volumes_are_rejected_rather_than_read_wrong() {
        let bytes = b"NRRD0004\ntype: uchar\ndimension: 4\nsizes: 2 2 2 2\nencoding: raw\n\nxxxxxxxxxxxxxxxx";
        assert!(parse(bytes).is_err());
    }
}
