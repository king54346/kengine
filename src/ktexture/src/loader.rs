//! 纹理加载器：解码 PNG / JPEG。

use crate::{TEXTURE_TYPE_UUID, Texture};
use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::Uuid;
use std::{path::PathBuf, sync::Arc};

/// 把图片文件解码成 [`Texture`]。
///
/// 解码统一输出 RGBA8，与 [`Texture`] 的存储约定一致。
#[derive(Debug, Default, Clone, Copy)]
pub struct TextureLoader;

impl ResourceLoader for TextureLoader {
    fn extensions(&self) -> &[&str] {
        &["png", "jpg", "jpeg"]
    }

    fn data_type_uuid(&self) -> Uuid {
        TEXTURE_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;

            let texture = Texture::from_encoded(&bytes).map_err(LoadError::custom)?;

            klog::debug!(
                "纹理已解码：{} ({}×{})",
                path.display(),
                texture.width(),
                texture.height()
            );

            Ok(Box::new(texture) as Box<dyn ResourceData>)
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kasset::{MemoryResourceIo, ResourceManager};

    /// 构造一张 1×1 的红色 PNG。
    fn tiny_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("编码 PNG 失败");
        bytes
    }

    #[test]
    fn decodes_png_to_rgba() {
        let mut io = MemoryResourceIo::new();
        io.add("red.png", tiny_png());
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(TextureLoader);

        let texture = manager.request_blocking::<Texture>("red.png").unwrap();
        let data = texture.data_ref().unwrap();

        assert_eq!(data.width(), 1);
        assert_eq!(data.height(), 1);
        assert_eq!(data.data(), &[255, 0, 0, 255]);
    }

    #[test]
    fn corrupt_image_reports_error() {
        let mut io = MemoryResourceIo::new();
        io.add("broken.png", b"not a png".to_vec());
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(TextureLoader);

        let error = manager
            .request_blocking::<Texture>("broken.png")
            .expect_err("损坏的图片应当加载失败");

        assert!(matches!(error, LoadError::Custom(_)));
    }
}
