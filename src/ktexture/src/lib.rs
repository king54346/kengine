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

mod loader;

pub use loader::TextureLoader;

use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use std::{error::Error, fmt};

/// 图片解码失败。
#[derive(Debug)]
pub struct TextureError(String);

impl fmt::Display for TextureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "图片解码失败：{}", self.0)
    }
}

impl Error for TextureError {}

/// [`Texture`] 的资源类型标识。
pub const TEXTURE_TYPE_UUID: Uuid = uuid!("c4a91e07-6b3d-42f8-9e15-8a7d0c2b6f43");

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sampler {
    /// 放大时的过滤方式。
    pub mag_filter: FilterMode,
    /// 缩小时的过滤方式。
    pub min_filter: FilterMode,
    /// U 方向环绕方式。
    pub wrap_u: WrapMode,
    /// V 方向环绕方式。
    pub wrap_v: WrapMode,
}

impl Sampler {
    /// 像素风常用配置：最近邻 + 边缘拉伸。
    pub fn pixelated() -> Self {
        Self {
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Nearest,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
        }
    }
}

/// 一张纹理。
///
/// 克隆会共享同一个 `id`，渲染器据此避免重复上传显存。
#[derive(Clone)]
pub struct Texture {
    id: Uuid,
    width: u32,
    height: u32,
    format: TextureFormat,
    sampler: Sampler,
    /// RGBA8 像素，长度恒为 `width * height * 4`。
    data: Vec<u8>,
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
            format: TextureFormat::default(),
            sampler: Sampler::default(),
            data,
        }
    }

    /// 从编码后的图片字节解码（PNG / JPEG），统一转成 RGBA8。
    ///
    /// glTF 的内嵌贴图走这条路径，无需经过文件系统。
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, TextureError> {
        let image = image::load_from_memory(bytes).map_err(|e| TextureError(e.to_string()))?;
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self::new(width, height, rgba.into_raw()))
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

    /// 生成棋盘格纹理，便于在没有美术资源时检查 UV 是否正确。
    pub fn checkerboard(size: u32, cell: u32, a: [u8; 4], b: [u8; 4]) -> Self {
        let cell = cell.max(1);
        let mut data = Vec::with_capacity(size as usize * size as usize * 4);
        for y in 0..size {
            for x in 0..size {
                let on = ((x / cell) + (y / cell)) % 2 == 0;
                data.extend_from_slice(if on { &a } else { &b });
            }
        }
        Self::new(size, size, data)
    }

    /// 指定像素格式。
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        self.format = format;
        self
    }

    /// 指定采样设置。
    pub fn with_sampler(mut self, sampler: Sampler) -> Self {
        self.sampler = sampler;
        self
    }

    /// 显存缓存键。克隆的纹理共享同一个 id。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 宽度（像素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 高度（像素）。
    pub fn height(&self) -> u32 {
        self.height
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
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl ResourceData for Texture {
    fn type_uuid(&self) -> Uuid {
        TEXTURE_TYPE_UUID
    }
}

#[cfg(test)]
mod test {
    use super::*;

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
}
