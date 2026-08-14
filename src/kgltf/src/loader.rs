//! glTF 资源加载器。

use crate::{importer, model::MODEL_TYPE_UUID};
use kasset::{BoxedLoaderFuture, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::Uuid;
use std::{path::PathBuf, sync::Arc};

/// 加载 `.gltf` 与 `.glb`。
#[derive(Debug, Default, Clone, Copy)]
pub struct GltfLoader;

impl ResourceLoader for GltfLoader {
    fn extensions(&self) -> &[&str] {
        &["gltf", "glb"]
    }

    fn data_type_uuid(&self) -> Uuid {
        MODEL_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let model = importer::import(bytes, path, io).await?;
            Ok(Box::new(model) as Box<dyn ResourceData>)
        })
    }
}
