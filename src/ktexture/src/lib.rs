//! ktexture —— 纹理资源。
//!
//! 这里只持有 CPU 端的像素数据与采样设置，**不依赖 wgpu**——
//! 上传显存由渲染器按 [`Texture::id`] 缓存完成。这样纹理资源可以在
//! 没有图形设备的环境（测试、资源打包工具）里正常加载。
//!
//! ```
//! use ktexture::prelude::*;
//!
//! // 纯色纹理，常用作缺省贴图。
//! let white = Texture::solid(2, 2, [255, 255, 255, 255]);
//! assert_eq!(white.width(), 2);
//! assert_eq!(white.data().len(), 2 * 2 * 4);
//! ```

#![warn(missing_docs)]

mod avif;
pub mod container;
mod gif;
mod loader;
pub mod lut;

pub use container::{Container, ContainerLoader};
pub use loader::TextureLoader;

use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use std::{error::Error, fmt, sync::Arc};

/// 图片解码失败。
#[derive(Debug)]
pub struct TextureError(String);

impl fmt::Display for TextureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "图片解码失败：{}", self.0)
    }
}

impl Error for TextureError {}

/// 把 RGBA8 像素存成 PNG。截图、烘焙结果落盘用。
pub fn write_png(
    path: impl AsRef<std::path::Path>,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), TextureError> {
    image::save_buffer(
        path.as_ref(),
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| TextureError(error.to_string()))
}

/// [`Texture`] 的资源类型标识。
pub const TEXTURE_TYPE_UUID: Uuid = uuid!("c4a91e07-6b3d-42f8-9e15-8a7d0c2b6f43");

/// 离屏相机视图在材质里的替身 id（见 [`Texture::camera_view`]），下标即视图编号。
/// 两个，和 `kcamera::MAX_VIEWS` 一致。
const CAMERA_VIEW_IDS: [Uuid; 2] = [
    uuid!("6e1f0c3a-9b27-4d58-8a41-0c7e2f9b5d10"),
    uuid!("6e1f0c3a-9b27-4d58-8a41-0c7e2f9b5d11"),
];

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{FilterMode, Sampler, Texture, TextureFormat, TextureLoader, WrapMode};
}

/// 像素格式。数据一律按 RGBA8 存放，区别只在于采样时是否做 sRGB → 线性转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureFormat {
    /// 线性空间。适合法线贴图、粗糙度等数据贴图。
    Linear,
    /// sRGB 空间。适合颜色贴图，采样时由硬件转成线性。
    #[default]
    Srgb,
}

/// 纹理过滤方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// 最近邻，适合像素风。
    Nearest,
    /// 线性插值。
    #[default]
    Linear,
}

/// 纹理坐标超出 `[0, 1]` 时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// 重复平铺。
    #[default]
    Repeat,
    /// 边缘拉伸。
    ClampToEdge,
    /// 镜像重复。
    MirrorRepeat,
}

/// 采样设置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampler {
    /// 放大时的过滤方式。
    pub mag_filter: FilterMode,
    /// 缩小时的过滤方式。
    pub min_filter: FilterMode,
    /// U 方向环绕方式。
    pub wrap_u: WrapMode,
    /// V 方向环绕方式。
    pub wrap_v: WrapMode,
    /// 生成 mip 链（默认开，three.js 的 `generateMipmaps`）。
    ///
    /// 没有 mip 的贴图缩小时每个像素只采原图的一个点：铺满地面的棋盘格远处闪成一片摩尔纹、
    /// 地形贴图一动就跳。渲染器上传时在 CPU 上逐级 2×2 平均（sRGB 的先换到线性再平均，不然越远越暗）。
    /// 关掉它的场合：当数据用、只按第 0 级采的查找表（反正采不到别的级），省 1/3 显存。
    pub mipmaps: bool,
    /// 各向异性过滤的倍数（1 = 关，常用 4–16，three.js 的 `texture.anisotropy`）。
    ///
    /// 掠射角下看的地面、路面：普通的三线性过滤按长轴挑 mip，整片糊掉；各向异性沿长轴多采几次，
    /// 远处的纹理还是清楚的。要 `mipmaps` 开着、三种过滤都是线性才生效（wgpu 的要求）。
    pub anisotropy: u8,
}

impl Default for Sampler {
    fn default() -> Self {
        Self {
            mag_filter: FilterMode::default(),
            min_filter: FilterMode::default(),
            wrap_u: WrapMode::default(),
            wrap_v: WrapMode::default(),
            mipmaps: true,
            anisotropy: 1,
        }
    }
}

impl Sampler {
    /// 像素风常用配置：最近邻 + 边缘拉伸。
    pub fn pixelated() -> Self {
        Self {
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Nearest,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            ..Self::default()
        }
    }

    /// 线性过滤 + 边缘拉伸，不要 mip：当数据用的贴图（查找表、调色板、按第 0 级采的场）。
    pub fn data() -> Self {
        Self {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            mipmaps: false,
            anisotropy: 1,
        }
    }

    /// 改各向异性倍数。
    pub fn with_anisotropy(mut self, anisotropy: u8) -> Self {
        self.anisotropy = anisotropy.max(1);
        self
    }
}

/// 一张纹理。
///
/// 克隆会共享同一个 `id`，渲染器据此避免重复上传显存。
///
/// # 也可以是一叠
///
/// [`layers`](Self::layers) 大于 1 时这就是一个**纹理数组**：同样尺寸、
/// 同样格式的若干张图叠在一起，着色器用一个整数下标去选。
///
/// 这和「把几张图拼进一张大图」（图集）是两回事，区别在**边界**：
/// 图集里相邻两块会在放大或取 mip 时互相渗色，得手工留边距；
/// 数组的每一层是独立的图，`Repeat` 平铺也只在自己这一层里绕。
#[derive(Clone)]
pub struct Texture {
    id: Uuid,
    width: u32,
    height: u32,
    /// 层数。普通贴图恒为 1。
    layers: u32,
    format: TextureFormat,
    sampler: Sampler,
    /// RGBA8 像素，长度恒为 `width * height * 4 * layers`，逐层排列。
    data: Arc<[u8]>,
    /// 内容版本。[`with_pixels`](Self::with_pixels) 换像素时加一，
    /// `id` 不变——渲染器据此**原地**重写显存，而不是再传一张新的。
    revision: u64,
    /// [`with_region`](Self::with_region) 改过的矩形 `(基准版本, x, y, 宽, 高)`：从基准版本到现在
    /// 只有这一块变了。渲染器手里正好是基准版本时只传这一块。
    dirty: Option<(u64, u32, u32, u32, u32)>,
    /// [`external`](Self::external) 替身。
    external: bool,
    /// 三维纹理（[`volume`](Self::volume)）：`layers` 是深度，渲染器建 `D3` 纹理。
    volume: bool,
}

impl Texture {
    /// 用 RGBA8 数据创建纹理。
    ///
    /// # Panics
    ///
    /// `data` 长度不等于 `width * height * 4` 时 panic。
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        let expected = width as usize * height as usize * 4;
        assert_eq!(
            data.len(),
            expected,
            "像素数据长度与尺寸不符：期望 {expected} 字节，实际 {}",
            data.len()
        );

        Self {
            id: Uuid::new_v4(),
            width,
            height,
            layers: 1,
            format: TextureFormat::default(),
            sampler: Sampler::default(),
            data: data.into(),
            revision: 0,
            dirty: None,
            external: false,
            volume: false,
        }
    }

    /// 用逐层排列的 RGBA8 数据创建一个纹理数组。
    ///
    /// # Panics
    ///
    /// `layers` 为 0，或 `data` 长度不等于 `width * height * 4 * layers` 时 panic。
    /// 长度对不上不是能兜底的事——多出来的字节会被静默丢掉，
    /// 少了则会让某一层是别人的像素，而两种情况都不报错。
    pub fn array(width: u32, height: u32, layers: u32, data: Vec<u8>) -> Self {
        assert!(layers > 0, "纹理数组至少要有一层");
        let expected = width as usize * height as usize * 4 * layers as usize;
        assert_eq!(
            data.len(),
            expected,
            "{layers} 层 {width}×{height} 需要 {expected} 字节，实际 {}",
            data.len()
        );

        Self {
            id: Uuid::new_v4(),
            width,
            height,
            layers,
            format: TextureFormat::default(),
            sampler: Sampler::default(),
            data: data.into(),
            revision: 0,
            dirty: None,
            external: false,
            volume: false,
        }
    }

    /// 把几张同尺寸的贴图叠成一个纹理数组。
    ///
    /// 层的顺序就是传进来的顺序——着色器里的下标即这里的下标。
    ///
    /// 格式与采样设置取**第一张**的：一个数组只有一份格式和一个采样器，
    /// 混着放本来就不成立。
    ///
    /// # Panics
    ///
    /// 传空切片，或各层尺寸不一致时 panic。尺寸不一致在 GPU 上没有
    /// 任何合理解释，早崩比画出错位的图好查。
    pub fn from_layers(layers: &[Texture]) -> Self {
        let Some(first) = layers.first() else {
            panic!("纹理数组至少要有一层");
        };

        let (width, height) = (first.width, first.height);
        let mut data = Vec::with_capacity(width as usize * height as usize * 4 * layers.len());
        for (index, layer) in layers.iter().enumerate() {
            assert!(
                layer.width == width && layer.height == height,
                "第 {index} 层是 {}×{}，和第 0 层的 {width}×{height} 不一致",
                layer.width,
                layer.height
            );
            assert_eq!(layer.layers, 1, "第 {index} 层自己就是个数组，不能再叠");
            data.extend_from_slice(&layer.data);
        }

        Self {
            id: Uuid::new_v4(),
            width,
            height,
            layers: layers.len() as u32,
            format: first.format,
            sampler: first.sampler,
            data: data.into(),
            revision: 0,
            dirty: None,
            external: false,
            volume: false,
        }
    }

    /// 从编码后的图片字节解码（PNG / JPEG / WebP / AVIF / 压缩纹理容器），统一转成 RGBA8。
    ///
    /// glTF 的内嵌贴图走这条路径，无需经过文件系统。
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, TextureError> {
        // DDS / KTX / KTX2 / PVR 走容器那条路，其余交给 `image`。
        if container::sniff(bytes).is_some() {
            return container::decode(bytes).map(|c| c.base());
        }
        // AVIF：`image` 的 AVIF 解码要 C 写的 dav1d，这里走纯 Rust 的那条。
        if avif::sniff(bytes) {
            let (width, height, rgba) = avif::decode(bytes)?;
            return Ok(Self::new(width, height, rgba));
        }
        if gif::sniff(bytes) {
            let (width, height, rgba) = gif::decode(bytes)?;
            return Ok(Self::new(width, height, rgba));
        }
        let image = image::load_from_memory(bytes).map_err(|e| TextureError(e.to_string()))?;
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self::new(width, height, rgba.into_raw()))
    }

    /// 把一张灰度**凹凸图**（bump map，亮 = 高）转成切线空间法线贴图。
    ///
    /// three.js 的 `bumpMap` 是在着色器里对高度求屏幕空间导数；这里在加载时
    /// 一次性转掉，渲染管线只认法线贴图一种扰动方式。`strength` 是高度的
    /// 放大倍数——三像素宽的一道凹槽，`strength = 1` 时斜率是 1/3。
    ///
    /// 边界按平铺处理（凹凸图几乎都是要平铺的）。
    pub fn bump_to_normal(&self, strength: f32) -> Texture {
        let (w, h) = (self.width.max(1), self.height.max(1));
        let height_at = |x: i64, y: i64| -> f32 {
            let x = x.rem_euclid(w as i64) as usize;
            let y = y.rem_euclid(h as i64) as usize;
            let i = (y * w as usize + x) * 4;
            // 取三通道平均：凹凸图偶尔是彩色存的。
            (self.data[i] as f32 + self.data[i + 1] as f32 + self.data[i + 2] as f32)
                / (3.0 * 255.0)
        };
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let dx = (height_at(x + 1, y) - height_at(x - 1, y)) * 0.5 * strength;
                // 纹理的 v 朝下而切线空间的副切线朝上，y 方向取反。
                let dy = (height_at(x, y - 1) - height_at(x, y + 1)) * 0.5 * strength;
                let n = [-dx, -dy, 1.0];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                for c in n {
                    out.push(((c / len * 0.5 + 0.5) * 255.0 + 0.5) as u8);
                }
                out.push(255);
            }
        }
        Texture::new(w, h, out)
            .with_format(TextureFormat::Linear)
            .with_sampler(self.sampler)
    }

    /// 创建纯色纹理，常用作缺省贴图。
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let data = rgba.repeat(width as usize * height as usize);
        Self::new(width, height, data)
    }

    /// 1×1 白色纹理。材质没有贴图时用它，可让着色器保持单一代码路径。
    pub fn white() -> Self {
        Self::solid(1, 1, [255, 255, 255, 255])
    }

    /// 生成凹凸网格状的法线贴图，用于在没有美术资源时验证切线空间是否正确。
    ///
    /// 输出的是切线空间法线（`[0,1]` 编码），因此格式必须是线性而非 sRGB。
    pub fn bumpy_normal(size: u32, cells: u32) -> Self {
        let size = size.max(2);
        let cells = cells.max(1) as f32;
        let mut data = Vec::with_capacity(size as usize * size as usize * 4);

        for y in 0..size {
            for x in 0..size {
                // 用两个正弦叠出规则的凹凸，其梯度即为法线的切线分量。
                let u = x as f32 / size as f32 * cells * std::f32::consts::TAU;
                let v = y as f32 / size as f32 * cells * std::f32::consts::TAU;
                let normal = kmath::Vec3::new(-u.sin() * 0.5, -v.sin() * 0.5, 1.0).normalize();

                let encode = |value: f32| ((value * 0.5 + 0.5) * 255.0).clamp(0.0, 255.0) as u8;
                data.extend_from_slice(&[
                    encode(normal.x),
                    encode(normal.y),
                    encode(normal.z),
                    255,
                ]);
            }
        }

        Self::new(size, size, data).with_format(TextureFormat::Linear)
    }

    /// 生成棋盘格纹理，便于在没有美术资源时检查 UV 是否正确。
    pub fn checkerboard(size: u32, cell: u32, a: [u8; 4], b: [u8; 4]) -> Self {
        let cell = cell.max(1);
        let mut data = Vec::with_capacity(size as usize * size as usize * 4);
        for y in 0..size {
            for x in 0..size {
                let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                data.extend_from_slice(if on { &a } else { &b });
            }
        }
        Self::new(size, size, data)
    }

    /// 生成一个边缘柔和的白色圆点，粒子没指定贴图时用它。
    ///
    /// `falloff` 控制边缘的软硬：1 是线性衰减，越大边缘越锐、中心越亮。
    /// 用平方衰减而非硬边圆，是因为硬边在放大后会露出明显的锯齿，
    /// 而粒子恰恰经常被放得很大。
    pub fn soft_circle(size: u32, falloff: f32) -> Self {
        let size = size.max(2);
        let center = (size - 1) as f32 * 0.5;
        let falloff = falloff.max(0.01);
        let mut data = Vec::with_capacity(size as usize * size as usize * 4);

        for y in 0..size {
            for x in 0..size {
                let dx = (x as f32 - center) / center;
                let dy = (y as f32 - center) / center;
                // 圆外一律为 0，保证方片的四角完全透明、不会露出边框。
                let distance = (dx * dx + dy * dy).sqrt().min(1.0);
                let alpha = (1.0 - distance).powf(falloff);
                let value = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
                data.extend_from_slice(&[255, 255, 255, value]);
            }
        }

        Self::new(size, size, data)
    }

    /// 换一份同尺寸的像素，**保留身份**（`id`、格式、采样设置不变）、
    /// 版本加一。
    ///
    /// 每帧都在变的贴图（Lottie 动画、程序化画布、视频帧）用它：
    /// 每帧 `Texture::new` 的话 id 每帧都是新的，渲染器会当成新贴图
    /// 各传一份、各建一个绑定组，显存只增不减。同一个 id 换版本，
    /// 渲染器直接往原来那块显存里写。
    ///
    /// # Panics
    ///
    /// `data` 长度和原来的不一样时 panic——尺寸变了就不是同一张图了，
    /// 该用 [`Texture::new`]。
    pub fn with_pixels(&self, data: Vec<u8>) -> Texture {
        assert_eq!(
            data.len(),
            self.data.len(),
            "with_pixels 只能换同样大小的像素：期望 {} 字节，实际 {}",
            self.data.len(),
            data.len()
        );
        Texture {
            data: data.into(),
            revision: self.revision + 1,
            dirty: None,
            external: false,
            volume: false,
            ..self.clone()
        }
    }

    /// 只改一块矩形（第 0 层）：`pixels` 是 `width × height` 的 RGBA8，写到 `(x, y)`。
    ///
    /// 和 [`with_pixels`](Self::with_pixels) 一样换版本、不换 id；不同的是渲染器只往显存里传这一块，
    /// 一张 2048² 的贴图上画一个 32² 的笔触，每帧传 4 KB 而不是 16 MB。同一帧里连续改好几块，
    /// 记的是它们的外接矩形。超出图边的部分裁掉。
    pub fn with_region(&self, x: u32, y: u32, width: u32, height: u32, pixels: &[u8]) -> Texture {
        assert_eq!(
            pixels.len(),
            width as usize * height as usize * 4,
            "with_region：像素数据长度和宽高对不上"
        );
        let mut data = self.data.to_vec();
        let (x1, y1) = ((x + width).min(self.width), (y + height).min(self.height));
        for row in y..y1 {
            let source = ((row - y) * width) as usize * 4;
            let target = (row * self.width + x) as usize * 4;
            let count = (x1.saturating_sub(x)) as usize * 4;
            data[target..target + count].copy_from_slice(&pixels[source..source + count]);
        }
        let rect = (
            x.min(self.width),
            y.min(self.height),
            x1.saturating_sub(x),
            y1.saturating_sub(y),
        );
        let dirty = match self.dirty {
            // 上一次的改动还没被渲染器取走（同一个基准）：合成外接矩形。
            Some((base, dx, dy, dw, dh)) => {
                let (left, top) = (dx.min(rect.0), dy.min(rect.1));
                let (right, bottom) = (
                    (dx + dw).max(rect.0 + rect.2),
                    (dy + dh).max(rect.1 + rect.3),
                );
                (base, left, top, right - left, bottom - top)
            }
            None => (self.revision, rect.0, rect.1, rect.2, rect.3),
        };
        Texture {
            data: data.into(),
            revision: self.revision + 1,
            dirty: Some(dirty),
            ..self.clone()
        }
    }

    /// 从 `base` 版本到现在改过的矩形 `(x, y, 宽, 高)`；不是 `base` 起算的（或整张换过）返回 `None`。
    pub fn dirty_region_since(&self, base: u64) -> Option<(u32, u32, u32, u32)> {
        match self.dirty {
            Some((from, x, y, w, h)) if from == base => Some((x, y, w, h)),
            _ => None,
        }
    }

    /// 内容版本，见 [`with_pixels`](Self::with_pixels)。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 指定像素格式。
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        if self.format != format {
            self.id = Uuid::new_v4();
        }
        self.format = format;
        self
    }

    /// 指定采样设置。
    pub fn with_sampler(mut self, sampler: Sampler) -> Self {
        if self.sampler != sampler {
            self.id = Uuid::new_v4();
        }
        self.sampler = sampler;
        self
    }

    /// 显存缓存键。克隆的纹理共享同一个 id。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 一张「指向离屏相机视图」的贴图：设进材质的任何贴图槽，渲染器绑的是
    /// 那台 `CameraTarget::View(slot)` 相机这一帧画出来的画面（线性 HDR），不是这张图的像素。
    ///
    /// 平面反射、监控屏幕、传送门都是这个用法：一台相机画进视图，材质按屏幕坐标或 UV 去采。
    /// 视图还没画出来（第一帧、窗口刚改尺寸）时采到的是白色。采样器是线性 + 夹边。
    ///
    /// 渲染器靠 [`id`](Self::id) 认出它，所以别对它调 [`with_sampler`](Self::with_sampler)
    /// 之类会换 id 的方法。
    pub fn camera_view(slot: u8) -> Self {
        let mut texture = Self::new(1, 1, vec![255; 4]).with_format(TextureFormat::Linear);
        texture.id = CAMERA_VIEW_IDS[usize::from(slot).min(CAMERA_VIEW_IDS.len() - 1)];
        texture.sampler = Sampler {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            ..Default::default()
        };
        texture
    }

    /// 一张「指向渲染器外部显存」的替身贴图，`id` 是那块显存登记时用的标识。
    ///
    /// 一般不直接调：计算着色器的存储纹理用 `krender::StorageTexture::texture` 拿替身。
    /// 渲染器认得这个 id 就绑那块显存；认不得（显存已经释放）就当一张 1×1 白图。
    pub fn external(id: Uuid) -> Self {
        let mut texture = Self::new(1, 1, vec![255; 4]).with_format(TextureFormat::Linear);
        texture.id = id;
        texture.external = true;
        texture
    }

    /// 一张**三维纹理**：`depth` 层 `width × height` 的 RGBA8，逐层排列（和 [`array`](Self::array) 一样的数据布局）。
    ///
    /// 和纹理数组的区别在采样：三维纹理按 `vec3` 坐标采、**层与层之间也插值**（三线性），
    /// 体积云、体素数据、3D 噪声、三维调色查找表要的是这个；纹理数组的层号是整数，层间不混。
    /// 设进材质的 `custom_texture_3d` 槽（钩子里 `textureSample(custom_texture_3d, base_color_sampler, uvw)`）。
    /// 不建 mip。
    ///
    /// # Panics
    ///
    /// `depth` 为 0，或数据长度不等于 `width * height * 4 * depth` 时 panic。
    pub fn volume(width: u32, height: u32, depth: u32, data: Vec<u8>) -> Self {
        let mut texture = Self::array(width, height, depth, data);
        texture.volume = true;
        texture.sampler.mipmaps = false;
        texture
    }

    /// 是不是三维纹理（[`volume`](Self::volume)）。
    pub fn is_volume(&self) -> bool {
        self.volume
    }

    /// 是不是 [`external`](Self::external) 替身。
    pub fn is_external(&self) -> bool {
        self.external
    }

    /// 这张贴图是不是 [`camera_view`](Self::camera_view) 的替身，是的话指向哪个视图。
    pub fn camera_view_slot(&self) -> Option<u8> {
        Self::camera_view_slot_of(self.id)
    }

    /// 按 id 判断（渲染器只有 id 时用）。
    pub fn camera_view_slot_of(id: Uuid) -> Option<u8> {
        CAMERA_VIEW_IDS
            .iter()
            .position(|view| *view == id)
            .map(|slot| slot as u8)
    }

    /// 视图 `slot` 的替身 id。
    pub fn camera_view_id(slot: u8) -> Uuid {
        CAMERA_VIEW_IDS[usize::from(slot).min(CAMERA_VIEW_IDS.len() - 1)]
    }

    /// 宽度（像素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 高度（像素）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 层数。普通贴图是 1，纹理数组大于 1。
    pub fn layers(&self) -> u32 {
        self.layers
    }

    /// 是不是纹理数组（层数大于 1）。
    pub fn is_array(&self) -> bool {
        self.layers > 1
    }

    /// 取某一层的像素。下标越界时返回 [`None`]。
    pub fn layer(&self, index: u32) -> Option<&[u8]> {
        if index >= self.layers {
            return None;
        }
        let stride = self.width as usize * self.height as usize * 4;
        let start = index as usize * stride;
        Some(&self.data[start..start + stride])
    }

    /// 像素格式。
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// 采样设置。
    pub fn sampler(&self) -> Sampler {
        self.sampler
    }

    /// RGBA8 像素数据。
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for Texture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不打印像素数据，否则一张贴图会刷屏几 MB。
        f.debug_struct("Texture")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("layers", &self.layers)
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// mip 链要几级（含第 0 级）：边长逐级减半到 1。
pub fn mip_level_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

/// 生成第 1 级起的 mip 链（每级逐层排列，和 [`Texture::data`] 一样的布局）。
///
/// 2×2 盒式平均；奇数边长时最后一行 / 列复用（夹边）。`srgb` 为真时先换到线性空间再平均——
/// 直接平均 sRGB 编码值会让远处整体发暗（gamma 的凹性）。alpha 一律线性平均。
pub fn generate_mips(
    data: &[u8],
    width: u32,
    height: u32,
    layers: u32,
    srgb: bool,
) -> Vec<Vec<u8>> {
    static TO_LINEAR: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    let to_linear = TO_LINEAR.get_or_init(|| {
        let mut table = [0.0; 256];
        for (i, value) in table.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *value = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
        }
        table
    });
    let to_srgb = |c: f32| {
        let s = if c <= 0.0031308 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    let mut levels = Vec::new();
    let (mut w, mut h) = (width.max(1), height.max(1));
    let mut previous = data.to_vec();
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4 * layers) as usize];
        for layer in 0..layers {
            let source = (w * h * 4 * layer) as usize;
            let target = (nw * nh * 4 * layer) as usize;
            for y in 0..nh {
                for x in 0..nw {
                    let mut sum = [0.0f32; 4];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        let i = source + ((sy * w + sx) * 4) as usize;
                        for c in 0..3 {
                            sum[c] += if srgb {
                                to_linear[previous[i + c] as usize]
                            } else {
                                previous[i + c] as f32 / 255.0
                            };
                        }
                        sum[3] += previous[i + 3] as f32 / 255.0;
                    }
                    let o = target + ((y * nw + x) * 4) as usize;
                    for c in 0..3 {
                        let v = sum[c] * 0.25;
                        next[o + c] = if srgb {
                            to_srgb(v)
                        } else {
                            (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
                        };
                    }
                    next[o + 3] = (sum[3] * 0.25 * 255.0 + 0.5) as u8;
                }
            }
        }
        levels.push(next.clone());
        previous = next;
        w = nw;
        h = nh;
    }
    levels
}

impl ResourceData for Texture {
    fn type_uuid(&self) -> Uuid {
        TEXTURE_TYPE_UUID
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn mip_chains_halve_down_to_one_texel_and_average_in_linear_space() {
        assert_eq!(super::mip_level_count(256, 64), 9);
        assert_eq!(super::mip_level_count(1, 1), 1);
        assert_eq!(super::mip_level_count(5, 3), 3);
        // 黑白相间的 2×2：sRGB 下平均出来不是 128（那是线性 0.21，偏暗），而是线性 0.5 ≈ sRGB 188。
        let data = [
            0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
        ];
        let srgb = super::generate_mips(&data, 2, 2, 1, true);
        assert_eq!(srgb.len(), 1);
        assert!(
            (186..=190).contains(&srgb[0][0]),
            "sRGB 平均应该在线性空间做：{}",
            srgb[0][0]
        );
        let linear = super::generate_mips(&data, 2, 2, 1, false);
        assert!((127..=128).contains(&linear[0][0]));
        assert_eq!(srgb[0][3], 255, "alpha 线性平均");
        // 多层：每层各自缩，按层排列。
        let two_layers: Vec<u8> = data.iter().copied().chain([10u8; 16]).collect();
        let mips = super::generate_mips(&two_layers, 2, 2, 2, false);
        assert_eq!(mips[0].len(), 8);
        assert_eq!(mips[0][4], 10);
    }

    #[test]
    fn region_writes_touch_only_their_rectangle_and_merge() {
        let texture = Texture::new(8, 8, vec![0; 8 * 8 * 4]);
        let base = texture.revision();
        let a = texture.with_region(1, 2, 2, 2, &[255; 2 * 2 * 4]);
        assert_eq!(a.dirty_region_since(base), Some((1, 2, 2, 2)));
        assert_eq!(a.data()[(2 * 8 + 1) * 4], 255, "矩形里写进去了");
        assert_eq!(a.data()[(2 * 8 + 3) * 4], 0, "矩形外没动");
        // 渲染器还没取走就又改了一块：合成外接矩形，基准不变。
        let b = a.with_region(5, 6, 2, 2, &[9; 2 * 2 * 4]);
        assert_eq!(b.dirty_region_since(base), Some((1, 2, 6, 6)));
        assert_eq!(b.dirty_region_since(a.revision()), None, "基准是最初那一版");
        // 超出图边的部分裁掉。
        let c = texture.with_region(7, 7, 2, 2, &[1; 2 * 2 * 4]);
        assert_eq!(c.dirty_region_since(base), Some((7, 7, 1, 1)));
        // 整张换过就不再是「只改了一块」。
        assert_eq!(
            b.with_pixels(vec![0; 8 * 8 * 4]).dirty_region_since(base),
            None
        );
    }

    use super::*;

    #[test]
    fn with_pixels_keeps_identity_and_bumps_the_revision() {
        let original = Texture::new(1, 1, vec![0, 0, 0, 255]).with_format(TextureFormat::Linear);
        let next = original.with_pixels(vec![255, 0, 0, 255]);
        assert_eq!(next.id(), original.id());
        assert_eq!(next.revision(), original.revision() + 1);
        assert_eq!(next.format(), TextureFormat::Linear);
        assert_eq!(texel(&next, 0, 0), [255, 0, 0, 255]);
        assert_eq!(texel(&original, 0, 0), [0, 0, 0, 255], "原来那份不受影响");
    }

    #[test]
    #[should_panic(expected = "with_pixels")]
    fn with_pixels_refuses_a_different_size() {
        Texture::new(1, 1, vec![0; 4]).with_pixels(vec![0; 8]);
    }

    /// 取某个像素的 RGBA。
    fn texel(texture: &Texture, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * texture.width() + x) * 4) as usize;
        texture.data()[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn soft_circle_is_opaque_at_the_center() {
        // 尺寸为奇数时正中间恰好落在一个像素上，那里必须是全不透明。
        assert_eq!(texel(&Texture::soft_circle(33, 1.0), 16, 16)[3], 255);
        // 偶数尺寸下没有像素正对圆心，最近的那个也应当几乎不透明。
        assert!(texel(&Texture::soft_circle(32, 1.0), 16, 16)[3] > 240);
    }

    #[test]
    fn soft_circle_corners_are_fully_transparent() {
        let texture = Texture::soft_circle(32, 1.0);

        // 四角必须透到底，否则粒子会显出方形边框。
        for (x, y) in [(0, 0), (31, 0), (0, 31), (31, 31)] {
            assert_eq!(texel(&texture, x, y)[3], 0, "({x}, {y}) 没有透明");
        }
    }

    #[test]
    fn soft_circle_alpha_decreases_outward() {
        let texture = Texture::soft_circle(64, 1.0);
        let center = 32;

        let mut previous = 255u8;
        for offset in 0..32 {
            let alpha = texel(&texture, center + offset, center)[3];
            assert!(alpha <= previous, "第 {offset} 个像素的透明度不该回升");
            previous = alpha;
        }
    }

    #[test]
    fn soft_circle_falloff_controls_edge_hardness() {
        let soft = Texture::soft_circle(64, 1.0);
        let sharp = Texture::soft_circle(64, 4.0);

        // 衰减指数越大，同一位置越暗（边缘更锐、亮区更集中）。
        assert!(texel(&sharp, 48, 32)[3] < texel(&soft, 48, 32)[3]);
    }

    #[test]
    fn solid_fills_every_pixel() {
        let texture = Texture::solid(3, 2, [10, 20, 30, 40]);

        assert_eq!(texture.width(), 3);
        assert_eq!(texture.height(), 2);
        assert_eq!(texture.data().len(), 3 * 2 * 4);
        assert!(texture.data().chunks(4).all(|p| p == [10, 20, 30, 40]));
    }

    #[test]
    #[should_panic(expected = "像素数据长度与尺寸不符")]
    fn mismatched_data_length_panics() {
        Texture::new(2, 2, vec![0; 3]);
    }

    #[test]
    fn clone_shares_gpu_id() {
        let texture = Texture::white();
        assert_eq!(texture.id(), texture.clone().id());
        assert_ne!(texture.id(), Texture::white().id());
    }

    #[test]
    fn bumpy_normal_is_linear_and_unit_length() {
        let texture = Texture::bumpy_normal(16, 2);

        // 法线贴图存的是方向数据，走 sRGB 会把数值扭曲。
        assert_eq!(texture.format(), TextureFormat::Linear);

        for pixel in texture.data().chunks(4) {
            let decode = |value: u8| value as f32 / 255.0 * 2.0 - 1.0;
            let normal = kmath::Vec3::new(decode(pixel[0]), decode(pixel[1]), decode(pixel[2]));

            // 量化到 8 位会有误差，放宽到 0.02。
            assert!(
                (normal.length() - 1.0).abs() < 0.02,
                "法线未归一化：{normal:?}"
            );
            // 切线空间法线必须朝外（+Z）。
            assert!(normal.z > 0.0);
        }
    }

    #[test]
    fn checkerboard_alternates_cells() {
        let a = [255, 255, 255, 255];
        let b = [0, 0, 0, 255];
        let texture = Texture::checkerboard(4, 2, a, b);

        let pixel = |x: u32, y: u32| {
            let i = ((y * 4 + x) * 4) as usize;
            &texture.data()[i..i + 4]
        };

        // (0,0) 与 (2,0) 分属相邻格子，颜色应当相反。
        assert_eq!(pixel(0, 0), a);
        assert_eq!(pixel(2, 0), b);
        assert_eq!(pixel(0, 2), b);
    }

    #[test]
    fn zero_cell_size_does_not_divide_by_zero() {
        let texture = Texture::checkerboard(2, 0, [1, 1, 1, 1], [2, 2, 2, 2]);
        assert_eq!(texture.data().len(), 2 * 2 * 4);
    }

    // ── 纹理数组 ──

    #[test]
    fn a_plain_texture_has_exactly_one_layer() {
        let texture = Texture::white();
        assert_eq!(texture.layers(), 1);
        assert!(!texture.is_array());
    }

    #[test]
    fn stacking_layers_concatenates_their_pixels() {
        let red = Texture::solid(2, 2, [255, 0, 0, 255]);
        let blue = Texture::solid(2, 2, [0, 0, 255, 255]);

        let array = Texture::from_layers(&[red, blue]);

        assert_eq!(array.layers(), 2);
        assert!(array.is_array());
        assert_eq!(array.width(), 2);
        assert_eq!(array.data().len(), 2 * 2 * 4 * 2);
        assert_eq!(&array.layer(0).unwrap()[..4], &[255, 0, 0, 255]);
        assert_eq!(&array.layer(1).unwrap()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn the_first_layer_decides_format_and_sampler() {
        // 一个数组只有一份格式和一个采样器，混着放本来就不成立。
        let first = Texture::solid(1, 1, [0; 4])
            .with_format(TextureFormat::Linear)
            .with_sampler(Sampler::pixelated());
        let second = Texture::solid(1, 1, [0; 4]);

        let array = Texture::from_layers(&[first, second]);

        assert_eq!(array.format(), TextureFormat::Linear);
        assert_eq!(array.sampler().mag_filter, FilterMode::Nearest);
    }

    #[test]
    fn layer_indexes_past_the_end_return_none() {
        let array = Texture::from_layers(&[Texture::white(), Texture::white()]);
        assert!(array.layer(1).is_some());
        assert!(array.layer(2).is_none());
    }

    #[test]
    #[should_panic(expected = "不一致")]
    fn mismatched_layer_sizes_panic_instead_of_rendering_garbage() {
        Texture::from_layers(&[Texture::solid(2, 2, [0; 4]), Texture::solid(4, 4, [0; 4])]);
    }

    #[test]
    #[should_panic(expected = "至少要有一层")]
    fn an_empty_stack_panics() {
        Texture::from_layers(&[]);
    }

    #[test]
    #[should_panic(expected = "字节")]
    fn array_data_of_the_wrong_length_panics() {
        // 少了会让某一层是别人的像素，多了会被静默丢掉，两种都不报错。
        Texture::array(2, 2, 3, vec![0; 2 * 2 * 4 * 2]);
    }

    #[test]
    fn a_one_layer_array_is_not_an_array() {
        assert!(!Texture::array(1, 1, 1, vec![0; 4]).is_array());
    }
}
