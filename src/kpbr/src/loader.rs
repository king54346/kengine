//! `.hdr` 的资源加载器。

use crate::hdr::HdrImage;
use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::{Uuid, uuid};
use std::{path::PathBuf, sync::Arc};

/// [`HdrImage`] 的资源类型 id。
pub const HDR_TYPE_UUID: Uuid = uuid!("6e4b6ad7-0f0e-4a2e-9f9d-2d9b3a2f5c11");

impl ResourceData for HdrImage {
    fn type_uuid(&self) -> Uuid {
        HDR_TYPE_UUID
    }
}

/// 加载 Radiance `.hdr` 全景图。
pub struct HdrLoader;

impl ResourceLoader for HdrLoader {
    fn extensions(&self) -> &[&str] {
        &["hdr"]
    }

    fn data_type_uuid(&self) -> Uuid {
        HDR_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let image = HdrImage::decode(&bytes).map_err(LoadError::custom)?;

            klog::debug!(
                "HDR 已解码：{} ({}×{})",
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
    fn the_loader_claims_the_hdr_extension() {
        assert_eq!(HdrLoader.extensions(), &["hdr"]);
        assert_eq!(HdrLoader.data_type_uuid(), HDR_TYPE_UUID);
    }

    #[test]
    fn the_type_uuid_is_stable() {
        // 类型 id 变了的话，已经存盘的场景里对 HDR 的引用会全部失配。
        let image = HdrImage::decode(
            b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 2 +X 2\n\
              \x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80",
        )
        .expect("该能解码");
        assert_eq!(image.type_uuid(), HDR_TYPE_UUID);
    }
}
