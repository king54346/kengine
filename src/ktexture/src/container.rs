//! 压缩纹理容器：DDS / KTX / KTX2 / PVR。
//!
//! # 为什么要在引擎里解压
//!
//! 理想情况下压缩块应当**原样**传给 GPU——BC / ASTC / ETC 都是显卡能
//! 直接采样的格式，省显存也省带宽。但 [`Texture`] 的约定是「一律 RGBA8」，
//! 渲染器按这个约定上传。改成保留压缩块要动顶层的纹理抽象、每种格式的
//! 硬件支持探测、以及不支持时的回退路径——是一件独立的事。
//!
//! 所以这里做的是**CPU 解压到 RGBA8**：能读所有这些容器，显存上没有好处。
//! 块解码本身复用 `texture2ddecoder`，这个模块只负责容器那一层
//! （头部、mip 链、立方体的六个面、超压缩）。
//!
//! # 支持
//!
//! | 容器 | 支持到哪 |
//! |---|---|
//! | DDS | BC1–BC7（含 BC6H 的有/无符号）、未压缩 RGB/RGBA/L/LA、mip 链、立方体 |
//! | KTX 1 | BC1–BC5、ETC1、ETC2、EAC、ASTC 4×4/6×6/8×8、PVRTC、未压缩 RGBA |
//! | KTX 2 | 全部非超压缩的 `vkFormat`（未压缩与块格式）、zstd 与 zlib 超压缩 |
//! | PVR | v2 与 v3 的 PVRTC 1，mip 链、立方体 |
//!
//! # 不支持：Basis Universal（ETC1S / UASTC）
//!
//! `vkFormat = 0` 的 KTX2 装的是 Basis 的中间格式，要靠一个**转码器**
//! 才能变成 GPU 格式。那是一整套带 Huffman 码表与全局码本的算法
//! （官方实现一千多行 C++），纯 Rust 没有现成的。这里遇到它会明确报错
//! 而不是画出一团噪声。受影响的样本：`2d_uastc.ktx2`、`2d_etc1s.ktx2`、
//! `spiritedaway.ktx2`，以及 `coffeemat.glb` 里那几张 `KHR_texture_basisu` 贴图。

use crate::{Sampler, Texture, TextureError, TextureFormat};
use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::{Uuid, uuid};
use std::{path::PathBuf, sync::Arc};

/// [`Container`] 的资源类型标识。
pub const CONTAINER_TYPE_UUID: Uuid = uuid!("2f6b91ca-84d7-4e05-9a3b-c17e58d06b92");

/// 一个容器解出来的全部内容。
#[derive(Debug, Clone)]
pub struct Container {
    /// 人类可读的像素格式名，给例子显示用。
    pub format: String,
    /// 第 0 级的宽高。
    pub width: u32,
    /// 第 0 级的宽高。
    pub height: u32,
    /// 立方体贴图为 6，普通纹理为 1。
    pub faces: u32,
    /// 每一级 mip 一张图（已解成 RGBA8）；立方体贴图时每级是一张 6 层的数组。
    pub levels: Vec<Texture>,
}

impl ResourceData for Container {
    fn type_uuid(&self) -> Uuid {
        CONTAINER_TYPE_UUID
    }
}

/// 把压缩纹理容器整个读进来（含 mip 链、立方体的六个面、格式名）。
///
/// 和 [`TextureLoader`](crate::TextureLoader) 的区别：那个只要第 0 级，
/// 直接产出一张 [`Texture`]；这个保留整个容器的结构，给「要显示 mip 链
/// 或者格式信息」的场合用。
///
/// 两者扩展名重叠，而资源管理器按扩展名找加载器、后注册的优先——
/// 同一个管理器里只该注册其中一个。
#[derive(Debug, Default, Clone, Copy)]
pub struct ContainerLoader;

impl ResourceLoader for ContainerLoader {
    fn extensions(&self) -> &[&str] {
        &["dds", "ktx", "ktx2", "pvr"]
    }

    fn data_type_uuid(&self) -> Uuid {
        CONTAINER_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let container = decode(&bytes).map_err(LoadError::custom)?;
            klog::debug!(
                "压缩纹理已解码：{}（{} {}×{}，{} 级 mip）",
                path.display(),
                container.format,
                container.width,
                container.height,
                container.levels.len()
            );
            Ok(Box::new(container) as Box<dyn ResourceData>)
        })
    }
}

impl Container {
    /// 第 0 级。容器至少有一级，所以这里不会失败。
    pub fn base(&self) -> Texture {
        self.levels[0].clone()
    }

    /// mip 级数。
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }
}

fn err(message: impl Into<String>) -> TextureError {
    TextureError(message.into())
}

/// 按魔数认出容器类型。不是已知容器时返回 `None`，调用方据此退回 `image`。
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"DDS ") {
        Some("dds")
    } else if bytes.starts_with(b"\xabKTX 11\xbb\r\n\x1a\n") {
        Some("ktx")
    } else if bytes.starts_with(b"\xabKTX 20\xbb\r\n\x1a\n") {
        Some("ktx2")
    } else if bytes.starts_with(b"PVR\x03") || bytes.get(44..48) == Some(b"PVR!") {
        Some("pvr")
    } else {
        None
    }
}

/// 解一个容器。
pub fn decode(bytes: &[u8]) -> Result<Container, TextureError> {
    match sniff(bytes) {
        Some("dds") => dds(bytes),
        Some("ktx") => ktx1(bytes),
        Some("ktx2") => ktx2(bytes),
        Some("pvr") => pvr(bytes),
        _ => Err(err("不是已知的压缩纹理容器")),
    }
}

/// 块解码函数的统一签名：输入压缩块、宽高，输出小端 BGRA 字。
type Decoder = fn(&[u8], usize, usize, &mut [u32]) -> Result<(), &'static str>;

/// 一种像素格式怎么解：块尺寸、每块字节数、解码函数。
#[derive(Clone, Copy)]
struct Layout {
    name: &'static str,
    block: (usize, usize),
    bytes: usize,
    decoder: Decoder,
    /// 数据贴图（法线、粗糙度）不该走 sRGB。
    linear: bool,
}

impl Layout {
    /// 这一级 mip 的压缩数据有多少字节。
    fn size(&self, width: usize, height: usize) -> usize {
        width.div_ceil(self.block.0) * height.div_ceil(self.block.1) * self.bytes
    }
}

/// `texture2ddecoder` 输出的是小端 BGRA 字，转成引擎要的 RGBA8。
fn to_rgba(pixels: Vec<u32>) -> Vec<u8> {
    pixels
        .into_iter()
        .flat_map(|p| {
            let [b, g, r, a] = p.to_le_bytes();
            [r, g, b, a]
        })
        .collect()
}

/// 解一级 mip 的一个面。
///
/// # 为什么要 `catch_unwind`
///
/// 块解码交给的是第三方 crate，而喂给它的是**外部文件**。一个写坏的
/// （或者恶意构造的）纹理不该让整个进程挂掉——游戏读到一张坏贴图，
/// 合理的表现是这张贴图加载失败，不是崩溃。
///
/// 这不是假设性的风险：`texture2ddecoder` 0.1.2 的 BC6H 解码在
/// `endpoint_bits == 16` 的模式（mode 15）上会算出 `1u16 << 16`，
/// debug 下直接 panic、release 下掩码变成 0 于是颜色是错的。
/// 三份 `disturb_dx10_bc6h_*.dds` 样本就会踩到。
fn decode_level(
    layout: &Layout,
    data: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, TextureError> {
    let needed = layout.size(width, height);
    let data = data
        .get(..needed)
        .ok_or_else(|| err(format!("{} 这一级的数据被截断", layout.name)))?;
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut pixels = vec![0u32; width * height];
        (layout.decoder)(data, width, height, &mut pixels).map(|()| pixels)
    }))
    .map_err(|_| err(format!("{} 的块解码器崩了（第三方解码器的 bug）", layout.name)))?
    .map_err(err)?;
    Ok(to_rgba(decoded))
}

/// 各级 mip 的尺寸：每级减半，最小为 1。
///
/// `level` 用 `min(31)` 夹一下：头部里的 mip 数是外部数据，写着 255 的
/// 文件会让移位溢出（debug 下 panic，release 下是未定义的结果）。
fn level_size(width: u32, height: u32, level: usize) -> (usize, usize) {
    let shift = level.min(31) as u32;
    (
        (width as usize >> shift).max(1),
        (height as usize >> shift).max(1),
    )
}

/// mip 链最多到 1×1，16384 的图也就 15 级。头部写得更多时按这个夹住。
fn clamp_levels(levels: u32, width: u32, height: u32) -> usize {
    let full = 32 - width.max(height).max(1).leading_zeros();
    levels.max(1).min(full) as usize
}

/// 把每级、每面的数据装成 [`Container`]。
///
/// `fetch(level, face)` 交出那一份原始数据；解码与组装在这里统一做，
/// 四种容器的差别只剩「数据在哪儿」。
fn assemble(
    layout: Layout,
    width: u32,
    height: u32,
    levels: usize,
    faces: u32,
    mut fetch: impl FnMut(usize, u32) -> Result<Vec<u8>, TextureError>,
) -> Result<Container, TextureError> {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(err("纹理尺寸不在 1..16384 之间"));
    }
    let mut result = Vec::with_capacity(levels);
    for level in 0..levels.max(1) {
        let (w, h) = level_size(width, height, level);
        let mut face_textures = Vec::with_capacity(faces as usize);
        for face in 0..faces {
            let data = fetch(level, face)?;
            let rgba = decode_level(&layout, &data, w, h)?;
            face_textures.push(Texture::new(w as u32, h as u32, rgba));
        }
        let mut texture = if faces > 1 {
            Texture::from_layers(&face_textures)
        } else {
            face_textures.remove(0)
        };
        texture = texture
            .with_format(if layout.linear {
                TextureFormat::Linear
            } else {
                TextureFormat::Srgb
            })
            .with_sampler(Sampler::default());
        result.push(texture);
    }
    Ok(Container {
        format: layout.name.into(),
        width,
        height,
        faces,
        levels: result,
    })
}

// ── 未压缩格式的「解码器」──
//
// 它们不是块格式，但套进同一个 `Layout` 之后，四种容器的组装代码就
// 只有一份。块尺寸 1×1、每块若干字节，解码函数负责换通道顺序。

macro_rules! raw_decoder {
    ($name:ident, $bytes:expr, $body:expr) => {
        fn $name(data: &[u8], width: usize, height: usize, out: &mut [u32]) -> Result<(), &'static str> {
            let convert: fn(&[u8]) -> [u8; 4] = $body;
            if data.len() < width * height * $bytes {
                return Err("未压缩数据被截断");
            }
            for (index, pixel) in out.iter_mut().enumerate().take(width * height) {
                let [r, g, b, a] = convert(&data[index * $bytes..]);
                // 下游按小端 BGRA 字解释。
                *pixel = u32::from_le_bytes([b, g, r, a]);
            }
            Ok(())
        }
    };
}

raw_decoder!(raw_rgba8, 4, |c| [c[0], c[1], c[2], c[3]]);
raw_decoder!(raw_bgra8, 4, |c| [c[2], c[1], c[0], c[3]]);
raw_decoder!(raw_bgrx8, 4, |c| [c[2], c[1], c[0], 255]);
raw_decoder!(raw_rgb8, 3, |c| [c[0], c[1], c[2], 255]);
raw_decoder!(raw_bgr8, 3, |c| [c[2], c[1], c[0], 255]);
raw_decoder!(raw_l8, 1, |c| [c[0], c[0], c[0], 255]);
raw_decoder!(raw_la8, 2, |c| [c[0], c[0], c[0], c[1]]);
raw_decoder!(raw_rgba16_unorm, 8, |c| {
    let channel = |i: usize| (u16::from_le_bytes([c[i * 2], c[i * 2 + 1]]) >> 8) as u8;
    [channel(0), channel(1), channel(2), channel(3)]
});
raw_decoder!(raw_rgba16_float, 8, |c| {
    let channel = |i: usize| tone(half_to_f32(u16::from_le_bytes([c[i * 2], c[i * 2 + 1]])));
    [channel(0), channel(1), channel(2), channel(3)]
});
raw_decoder!(raw_rgba32_float, 16, |c| {
    let channel =
        |i: usize| tone(f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()));
    [channel(0), channel(1), channel(2), channel(3)]
});
raw_decoder!(raw_rgb9e5, 4, |c| {
    // 共享指数：三个 9 位尾数 + 一个 5 位指数，偏移 15、尾数隐含 1/512。
    let word = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    let exponent = (word >> 27) as i32 - 15 - 9;
    let scale = (exponent as f32).exp2();
    let mantissa = |shift: u32| ((word >> shift) & 0x1ff) as f32 * scale;
    [tone(mantissa(0)), tone(mantissa(9)), tone(mantissa(18)), 255]
});
raw_decoder!(raw_r11g11b10, 4, |c| {
    let word = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    // R 和 G 是 6 位尾数 + 5 位指数，B 是 5 位尾数 + 5 位指数。
    let unpack = |bits: u32, mantissa_bits: u32| {
        let mantissa = (bits & ((1 << mantissa_bits) - 1)) as f32;
        let exponent = (bits >> mantissa_bits) as i32 - 15;
        if bits >> mantissa_bits == 0 {
            mantissa / (1 << mantissa_bits) as f32 * (-14.0f32).exp2()
        } else {
            (1.0 + mantissa / (1 << mantissa_bits) as f32) * (exponent as f32).exp2()
        }
    };
    [
        tone(unpack(word & 0x7ff, 6)),
        tone(unpack((word >> 11) & 0x7ff, 6)),
        tone(unpack((word >> 22) & 0x3ff, 5)),
        255,
    ]
});

/// 半精度浮点转单精度。
fn half_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exponent = ((bits >> 10) & 0x1f) as i32;
    let mantissa = (bits & 0x3ff) as u32;
    let value = match exponent {
        0 if mantissa == 0 => 0.0,
        // 非规格化数。
        0 => mantissa as f32 / 1024.0 * (-14.0f32).exp2(),
        31 => f32::INFINITY,
        _ => (1.0 + mantissa as f32 / 1024.0) * ((exponent - 15) as f32).exp2(),
    };
    if sign == 1 { -value } else { value }
}

/// HDR 值落到 8 位上：Reinhard + sRGB 传递函数。
///
/// 这是**有损**的——HDR 容器（BC6H、RGBA16F、RGB9E5）的高光会被压回
/// `0..1`。[`Texture`] 只存 RGBA8，要保住 HDR 范围得走
/// [`kpbr::hdr::HdrImage`] 那条路（[`super::ultrahdr`] 就是那么做的）。
fn tone(value: f32) -> u8 {
    let mapped = if value.is_finite() && value > 0.0 {
        value / (1.0 + value)
    } else {
        0.0
    };
    // sRGB 传递函数：不做的话解出来的 HDR 图整体偏暗。
    let encoded = if mapped <= 0.003_130_8 {
        mapped * 12.92
    } else {
        1.055 * mapped.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).clamp(0.0, 255.0) as u8
}

// ── DDS ──

fn dds(bytes: &[u8]) -> Result<Container, TextureError> {
    let word = |at: usize| -> Result<u32, TextureError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(|| err("DDS 头部被截断"))?
                .try_into()
                .unwrap(),
        ))
    };
    if word(4)? != 124 {
        return Err(err("DDS 头部长度不是 124"));
    }
    let height = word(12)?;
    let width = word(16)?;
    let mips = clamp_levels(word(28)?, width, height);
    let pixel_flags = word(80)?;
    let four_cc = bytes.get(84..88).ok_or_else(|| err("DDS 头部被截断"))?;
    let bit_count = word(88)?;
    let (red_mask, green_mask, blue_mask, alpha_mask) =
        (word(92)?, word(96)?, word(100)?, word(104)?);
    let caps2 = word(112)?;
    // DDSCAPS2_CUBEMAP = 0x200。六个面全在才当立方体处理。
    let faces = if caps2 & 0x200 != 0 { 6 } else { 1 };

    let mut offset = 128usize;
    let layout = if pixel_flags & 0x4 != 0 {
        // DDPF_FOURCC
        match four_cc {
            b"DXT1" => bc(1),
            b"DXT2" | b"DXT3" => bc(2),
            b"DXT4" | b"DXT5" => bc(3),
            b"ATI1" | b"BC4U" => bc(4),
            b"ATI2" | b"BC5U" => bc(5),
            b"DX10" => {
                let dxgi = word(128)?;
                offset = 148;
                dxgi_layout(dxgi)?
            }
            other => {
                return Err(err(format!(
                    "不支持的 DDS FourCC {}",
                    String::from_utf8_lossy(other)
                )));
            }
        }
    } else {
        // 未压缩：靠通道掩码认。DDS 的掩码是按小端 32 位字写的，
        // 所以 0x00ff0000 是「字节序里的第三个字节」= B8G8R8A8 的 R。
        match (bit_count, red_mask, green_mask, blue_mask, alpha_mask) {
            (32, 0x00ff_0000, _, _, 0xff00_0000) => raw("B8G8R8A8", raw_bgra8, 4, false),
            (32, 0x00ff_0000, _, _, 0) => raw("B8G8R8X8", raw_bgrx8, 4, false),
            (32, 0x0000_00ff, _, _, _) => raw("R8G8B8A8", raw_rgba8, 4, false),
            (24, 0x00ff_0000, _, _, _) => raw("B8G8R8", raw_bgr8, 3, false),
            (24, 0x0000_00ff, _, _, _) => raw("R8G8B8", raw_rgb8, 3, false),
            (16, _, _, _, _) => raw("L8A8", raw_la8, 2, false),
            (8, _, _, _, _) => raw("L8", raw_l8, 1, false),
            _ => return Err(err("不支持的 DDS 未压缩像素格式")),
        }
    };

    // DDS 的排列是「先面、后 mip」：每个面完整的一条 mip 链接着下一个面。
    let mut face_offsets = Vec::with_capacity(faces as usize);
    let mut cursor = offset;
    for _ in 0..faces {
        face_offsets.push(cursor);
        for level in 0..mips {
            let (w, h) = level_size(width, height, level);
            cursor += layout.size(w, h);
        }
    }

    assemble(layout, width, height, mips, faces, |level, face| {
        let mut at = face_offsets[face as usize];
        for skipped in 0..level {
            let (w, h) = level_size(width, height, skipped);
            at += layout.size(w, h);
        }
        bytes
            .get(at..)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| err("DDS 数据被截断"))
    })
}

fn bc(index: u32) -> Layout {
    match index {
        1 => Layout {
            name: "BC1 (DXT1)",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_bc1,
            linear: false,
        },
        2 => Layout {
            name: "BC2 (DXT3)",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_bc2,
            linear: false,
        },
        3 => Layout {
            name: "BC3 (DXT5)",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_bc3,
            linear: false,
        },
        4 => Layout {
            name: "BC4",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_bc4,
            linear: true,
        },
        5 => Layout {
            name: "BC5",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_bc5,
            linear: true,
        },
        6 => Layout {
            name: "BC6H (HDR)",
            block: (4, 4),
            bytes: 16,
            decoder: |data, w, h, out| texture2ddecoder::decode_bc6(data, w, h, out, false),
            linear: true,
        },
        7 => Layout {
            name: "BC6H 有符号 (HDR)",
            block: (4, 4),
            bytes: 16,
            decoder: |data, w, h, out| texture2ddecoder::decode_bc6(data, w, h, out, true),
            linear: true,
        },
        _ => Layout {
            name: "BC7",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_bc7,
            linear: false,
        },
    }
}

fn raw(name: &'static str, decoder: Decoder, bytes: usize, linear: bool) -> Layout {
    Layout {
        name,
        block: (1, 1),
        bytes,
        decoder,
        linear,
    }
}

fn dxgi_layout(format: u32) -> Result<Layout, TextureError> {
    Ok(match format {
        70..=72 => bc(1),
        73..=75 => bc(2),
        76..=78 => bc(3),
        79..=80 => bc(4),
        82..=83 => bc(5),
        95 => bc(6),
        96 => bc(7),
        97..=99 => bc(8),
        28 | 29 => raw("R8G8B8A8", raw_rgba8, 4, false),
        87 | 88 => raw("B8G8R8A8", raw_bgra8, 4, false),
        10 => raw("R16G16B16A16 float", raw_rgba16_float, 8, true),
        11 => raw("R16G16B16A16 unorm", raw_rgba16_unorm, 8, true),
        2 => raw("R32G32B32A32 float", raw_rgba32_float, 16, true),
        67 => raw("RGB9E5", raw_rgb9e5, 4, true),
        26 => raw("R11G11B10 float", raw_r11g11b10, 4, true),
        other => return Err(err(format!("不支持的 DXGI 格式 {other}"))),
    })
}

// ── KTX 1 ──

fn ktx1(bytes: &[u8]) -> Result<Container, TextureError> {
    let word = |at: usize| -> Result<u32, TextureError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(|| err("KTX 头部被截断"))?
                .try_into()
                .unwrap(),
        ))
    };
    if word(12)? != 0x0403_0201 {
        return Err(err("不支持大端字节序的 KTX"));
    }
    let internal_format = word(28)?;
    let width = word(36)?;
    let height = word(40)?;
    let faces = word(48)?.max(1);
    let levels = clamp_levels(word(52)?, width, height);
    let key_value_bytes = word(60)? as usize;
    let layout = gl_layout(internal_format)?;

    // KTX1 的排列是「先 mip、后面」，每级前面有一个 4 字节长度。
    let mut level_offsets = Vec::with_capacity(levels);
    let mut cursor = 64 + key_value_bytes;
    for level in 0..levels {
        let size = word(cursor)? as usize;
        level_offsets.push(cursor + 4);
        let (w, h) = level_size(width, height, level);
        let face_size = layout.size(w, h);
        // 规范规定每面数据按 4 字节对齐。
        let padded = face_size.div_ceil(4) * 4;
        cursor += 4 + if faces == 6 { padded * 6 } else { size.div_ceil(4) * 4 };
    }

    assemble(layout, width, height, levels, faces, |level, face| {
        let (w, h) = level_size(width, height, level);
        let padded = layout.size(w, h).div_ceil(4) * 4;
        let at = level_offsets[level] + padded * face as usize;
        bytes
            .get(at..)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| err("KTX 数据被截断"))
    })
}

fn gl_layout(format: u32) -> Result<Layout, TextureError> {
    Ok(match format {
        0x83F0 | 0x83F1 => bc(1),
        0x8C4C | 0x8C4D => bc(1),
        0x83F2 | 0x8C4E => bc(2),
        0x83F3 | 0x8C4F => bc(3),
        // RED_RGTC1 / RG_RGTC2
        0x8DBB | 0x8DBC => bc(4),
        0x8DBD | 0x8DBE => bc(5),
        // BPTC
        0x8E8C | 0x8E8D => bc(8),
        0x8E8E => bc(7),
        0x8E8F => bc(6),
        0x8D64 => Layout {
            name: "ETC1",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_etc1,
            linear: false,
        },
        0x9274 | 0x9275 => Layout {
            name: "ETC2 RGB",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_etc2_rgb,
            linear: false,
        },
        0x9278 | 0x9279 => Layout {
            name: "ETC2 RGBA",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_etc2_rgba8,
            linear: false,
        },
        0x9270 | 0x9271 => Layout {
            name: "EAC R11",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_eacr,
            linear: true,
        },
        0x9272 | 0x9273 => Layout {
            name: "EAC RG11",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_eacrg,
            linear: true,
        },
        0x8C00 | 0x8C02 => Layout {
            name: "PVRTC 4bpp",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_pvrtc_4bpp,
            linear: false,
        },
        0x8C01 | 0x8C03 => Layout {
            name: "PVRTC 2bpp",
            block: (8, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_pvrtc_2bpp,
            linear: false,
        },
        // ASTC 的 14 种块尺寸在 GL 枚举里是连续的两段（线性与 sRGB）。
        0x93B0..=0x93BD => astc((format - 0x93B0) as usize)?,
        0x93D0..=0x93DD => astc((format - 0x93D0) as usize)?,
        0x8058 | 0x1908 => raw("RGBA8", raw_rgba8, 4, false),
        0x8051 | 0x1907 => raw("RGB8", raw_rgb8, 3, false),
        other => return Err(err(format!("不支持的 KTX 内部格式 0x{other:04X}"))),
    })
}

/// ASTC 的 14 种块尺寸，顺序和 GL / Vulkan 的枚举顺序一致。
const ASTC_BLOCKS: [(usize, usize, &str); 14] = [
    (4, 4, "ASTC 4×4"),
    (5, 4, "ASTC 5×4"),
    (5, 5, "ASTC 5×5"),
    (6, 5, "ASTC 6×5"),
    (6, 6, "ASTC 6×6"),
    (8, 5, "ASTC 8×5"),
    (8, 6, "ASTC 8×6"),
    (8, 8, "ASTC 8×8"),
    (10, 5, "ASTC 10×5"),
    (10, 6, "ASTC 10×6"),
    (10, 8, "ASTC 10×8"),
    (10, 10, "ASTC 10×10"),
    (12, 10, "ASTC 12×10"),
    (12, 12, "ASTC 12×12"),
];

// `texture2ddecoder::decode_astc` 要把块尺寸当参数传，而 `Layout` 存的是
// 一个普通函数指针（不能捕获）。所以每种块尺寸生成一个薄包装。
macro_rules! astc_decoders {
    ($($name:ident => ($w:expr, $h:expr)),+ $(,)?) => {
        $(fn $name(data: &[u8], width: usize, height: usize, out: &mut [u32]) -> Result<(), &'static str> {
            texture2ddecoder::decode_astc(data, width, height, $w, $h, out)
        })+
        /// 按块尺寸取解码函数。
        fn astc_decoder(block: (usize, usize)) -> Option<Decoder> {
            Some(match block {
                $(($w, $h) => $name as Decoder,)+
                _ => return None,
            })
        }
    };
}

astc_decoders! {
    astc_4_4 => (4, 4), astc_5_4 => (5, 4), astc_5_5 => (5, 5), astc_6_5 => (6, 5),
    astc_6_6 => (6, 6), astc_8_5 => (8, 5), astc_8_6 => (8, 6), astc_8_8 => (8, 8),
    astc_10_5 => (10, 5), astc_10_6 => (10, 6), astc_10_8 => (10, 8),
    astc_10_10 => (10, 10), astc_12_10 => (12, 10), astc_12_12 => (12, 12),
}

/// 按 GL / Vulkan 枚举里的次序取第 `index` 种 ASTC 块尺寸。
fn astc(index: usize) -> Result<Layout, TextureError> {
    let (width, height, name) = *ASTC_BLOCKS
        .get(index)
        .ok_or_else(|| err("ASTC 块尺寸下标越界"))?;
    Ok(Layout {
        name,
        block: (width, height),
        // ASTC 无论块多大，一块永远是 16 字节——压缩率的差别全在这里。
        bytes: 16,
        decoder: astc_decoder((width, height)).ok_or_else(|| err("没有这个 ASTC 块尺寸的解码器"))?,
        linear: false,
    })
}

// ── KTX 2 ──

fn ktx2(bytes: &[u8]) -> Result<Container, TextureError> {
    let word = |at: usize| -> Result<u32, TextureError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(|| err("KTX2 头部被截断"))?
                .try_into()
                .unwrap(),
        ))
    };
    let long = |at: usize| -> Result<u64, TextureError> {
        Ok(u64::from_le_bytes(
            bytes
                .get(at..at + 8)
                .ok_or_else(|| err("KTX2 头部被截断"))?
                .try_into()
                .unwrap(),
        ))
    };
    let vk_format = word(12)?;
    let width = word(20)?;
    let height = word(24)?;
    let faces = word(36)?.max(1);
    let levels = clamp_levels(word(40)?, width, height);
    let supercompression = word(44)?;

    if vk_format == 0 {
        return Err(err(
            "这份 KTX2 装的是 Basis Universal（ETC1S / UASTC），需要转码器，引擎没有实现",
        ));
    }
    let layout = vk_layout(vk_format)?;

    // 级索引：每级三个 u64（字节偏移、字节数、解压后的字节数）。
    let mut planes = Vec::with_capacity(levels);
    for level in 0..levels {
        let at = 80 + level * 24;
        let offset = long(at)? as usize;
        let length = long(at + 8)? as usize;
        let uncompressed = long(at + 16)? as usize;
        let raw = bytes
            .get(offset..offset + length)
            .ok_or_else(|| err("KTX2 的级数据被截断"))?;
        planes.push(match supercompression {
            0 => raw.to_vec(),
            1 => return Err(err("BasisLZ 超压缩需要 Basis 转码器，引擎没有实现")),
            2 => zstd_decode(raw, uncompressed)?,
            3 => zlib_decode(raw, uncompressed)?,
            other => return Err(err(format!("不支持的 KTX2 超压缩方式 {other}"))),
        });
    }

    assemble(layout, width, height, levels, faces, |level, face| {
        let (w, h) = level_size(width, height, level);
        let face_size = layout.size(w, h);
        let at = face_size * face as usize;
        planes[level]
            .get(at..)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| err("KTX2 的面数据被截断"))
    })
}

fn zstd_decode(data: &[u8], expected: usize) -> Result<Vec<u8>, TextureError> {
    use std::io::Read;
    let decoder =
        ruzstd::StreamingDecoder::new(data).map_err(|e| err(e.to_string()))?;
    let mut out = Vec::with_capacity(expected);
    decoder
        .take(expected.max(1) as u64)
        .read_to_end(&mut out)
        .map_err(|e| err(e.to_string()))?;
    Ok(out)
}

fn zlib_decode(data: &[u8], expected: usize) -> Result<Vec<u8>, TextureError> {
    use std::io::Read;
    let mut out = Vec::with_capacity(expected);
    flate2::read::ZlibDecoder::new(data)
        .take(expected.max(1) as u64)
        .read_to_end(&mut out)
        .map_err(|e| err(e.to_string()))?;
    Ok(out)
}

fn vk_layout(format: u32) -> Result<Layout, TextureError> {
    Ok(match format {
        // 未压缩
        23 => raw("R8G8B8", raw_rgb8, 3, true),
        29 => raw("R8G8B8 sRGB", raw_rgb8, 3, false),
        37 => raw("R8G8B8A8", raw_rgba8, 4, true),
        43 => raw("R8G8B8A8 sRGB", raw_rgba8, 4, false),
        91 => raw("R16G16B16A16 unorm", raw_rgba16_unorm, 8, true),
        97 => raw("R16G16B16A16 float", raw_rgba16_float, 8, true),
        109 => raw("R32G32B32A32 float", raw_rgba32_float, 16, true),
        122 => raw("B10G11R11 float", raw_r11g11b10, 4, true),
        123 => raw("E5B9G9R9", raw_rgb9e5, 4, true),
        // 块压缩
        131..=132 => bc(1),
        133..=134 => bc(1),
        135..=136 => bc(2),
        137..=138 => bc(3),
        139..=140 => bc(4),
        141..=142 => bc(5),
        143 => bc(6),
        144 => bc(7),
        145..=146 => bc(8),
        147..=148 => Layout {
            name: "ETC2 RGB",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_etc2_rgb,
            linear: false,
        },
        149..=150 => Layout {
            name: "ETC2 RGBA1",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_etc2_rgba1,
            linear: false,
        },
        151..=152 => Layout {
            name: "ETC2 RGBA8",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_etc2_rgba8,
            linear: false,
        },
        153..=154 => Layout {
            name: "EAC R11",
            block: (4, 4),
            bytes: 8,
            decoder: texture2ddecoder::decode_eacr,
            linear: true,
        },
        155..=156 => Layout {
            name: "EAC RG11",
            block: (4, 4),
            bytes: 16,
            decoder: texture2ddecoder::decode_eacrg,
            linear: true,
        },
        // vkFormat 157..184 是 14 种块尺寸 × (UNORM, SRGB) 交替排列。
        157..=184 => astc(((format - 157) / 2) as usize)?,
        // HDR ASTC（`*_SFLOAT_BLOCK_EXT`，扩展号段）。块布局和 LDR 完全
        // 一样，差别在端点的解释方式；`texture2ddecoder` 的解码器认得
        // HDR 端点模式，但输出仍然是 8 位——高光会被压回 `0..1`。
        1000066000..=1000066013 => astc((format - 1000066000) as usize)?,
        other => return Err(err(format!("不支持的 KTX2 vkFormat {other}"))),
    })
}

// ── PVR ──

fn pvr(bytes: &[u8]) -> Result<Container, TextureError> {
    let word = |at: usize| -> Result<u32, TextureError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(|| err("PVR 头部被截断"))?
                .try_into()
                .unwrap(),
        ))
    };
    let (width, height, offset, layout, mips, faces) = if bytes.starts_with(b"PVR\x03") {
        let format = word(8)?;
        if word(12)? != 0 || format > 3 {
            return Err(err("只支持 PVRTC1 的 PVR v3"));
        }
        let two_bpp = format < 2;
        (
            word(28)?,
            word(24)?,
            52 + word(48)? as usize,
            pvrtc(two_bpp, format % 2 == 1),
            clamp_levels(word(44)?, word(28)?, word(24)?),
            word(40)?.max(1),
        )
    } else {
        let format = word(16)? & 255;
        if ![24, 25].contains(&format) {
            return Err(err("不支持的 PVR v2 像素格式"));
        }
        (
            word(8)?,
            word(4)?,
            word(0)? as usize,
            pvrtc(format == 24, word(40)? != 0),
            // v2 的 mip 数在第 12 字节，而且**不含**第 0 级。第 44 字节
            // 是魔数 "PVR!"（`sniff` 就是靠它认的），拿它当 mip 数会得到
            // 一个荒唐的大数。
            clamp_levels(word(12)? + 1, word(8)?, word(4)?),
            1,
        )
    };
    if !width.is_power_of_two() || !height.is_power_of_two() {
        return Err(err("PVRTC 的宽高必须是 2 的幂"));
    }

    // PVR v3 的排列是「先 mip、后面」。
    let mut level_offsets = Vec::with_capacity(mips);
    let mut cursor = offset;
    for level in 0..mips {
        level_offsets.push(cursor);
        let (w, h) = level_size(width, height, level);
        cursor += pvrtc_size(&layout, w, h) * faces as usize;
    }

    assemble(layout, width, height, mips, faces, |level, face| {
        let (w, h) = level_size(width, height, level);
        let at = level_offsets[level] + pvrtc_size(&layout, w, h) * face as usize;
        bytes
            .get(at..)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| err("PVR 数据被截断"))
    })
}

fn pvrtc(two_bpp: bool, alpha: bool) -> Layout {
    Layout {
        name: match (two_bpp, alpha) {
            (true, true) => "PVRTC 2bpp RGBA",
            (true, false) => "PVRTC 2bpp RGB",
            (false, true) => "PVRTC 4bpp RGBA",
            (false, false) => "PVRTC 4bpp RGB",
        },
        block: if two_bpp { (8, 4) } else { (4, 4) },
        bytes: 8,
        decoder: if two_bpp {
            texture2ddecoder::decode_pvrtc_2bpp
        } else {
            texture2ddecoder::decode_pvrtc_4bpp
        },
        linear: false,
    }
}

/// PVRTC 每级至少占一个「最小块组」，不能按宽高直接算。
fn pvrtc_size(layout: &Layout, width: usize, height: usize) -> usize {
    let two_bpp = layout.block.0 == 8;
    let minimum = width.max(if two_bpp { 16 } else { 8 }) * height.max(8) / if two_bpp { 4 } else { 2 };
    layout.size(width, height).max(minimum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing_knows_the_four_containers() {
        assert_eq!(sniff(b"DDS |||||"), Some("dds"));
        assert_eq!(sniff(b"\xabKTX 11\xbb\r\n\x1a\n...."), Some("ktx"));
        assert_eq!(sniff(b"\xabKTX 20\xbb\r\n\x1a\n...."), Some("ktx2"));
        assert_eq!(sniff(b"PVR\x03...."), Some("pvr"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), None);
    }

    #[test]
    fn half_floats_round_trip_the_easy_values() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0x4000), 2.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
    }

    #[test]
    fn tone_mapping_keeps_zero_at_zero_and_clamps_the_top() {
        assert_eq!(tone(0.0), 0);
        assert_eq!(tone(f32::NAN), 0);
        assert!(tone(1000.0) > 250);
        assert!(tone(0.5) > tone(0.25));
    }

    #[test]
    fn block_sizes_round_up_to_whole_blocks() {
        let layout = bc(1);
        // 5×5 的图占 2×2 个 4×4 的块。
        assert_eq!(layout.size(5, 5), 4 * 8);
        assert_eq!(layout.size(4, 4), 8);
    }

    #[test]
    fn a_basis_ktx2_reports_a_useful_error_rather_than_noise() {
        let mut bytes = b"\xabKTX 20\xbb\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 200]); // vkFormat = 0
        let error = decode(&bytes).unwrap_err().to_string();
        assert!(error.contains("Basis"), "错误信息是「{error}」");
    }

    #[test]
    fn an_unknown_container_is_not_claimed() {
        assert!(decode(b"not a texture container at all").is_err());
    }
}
