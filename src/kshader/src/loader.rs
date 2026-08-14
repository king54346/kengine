//! 着色器加载器。

use crate::{SHADER_TYPE_UUID, Shader};
use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::Uuid;
use std::{path::PathBuf, sync::Arc};

/// 加载 `.wgsl` 文件并校验。
#[derive(Debug, Default, Clone, Copy)]
pub struct ShaderLoader;

impl ResourceLoader for ShaderLoader {
    fn extensions(&self) -> &[&str] {
        &["wgsl"]
    }

    fn data_type_uuid(&self) -> Uuid {
        SHADER_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let source = String::from_utf8(bytes).map_err(LoadError::custom)?;

            let shader = Shader::from_wgsl(source).map_err(LoadError::custom)?;
            klog::debug!(
                "着色器已校验：{}（入口点 {}）",
                path.display(),
                shader.entry_points().len()
            );

            Ok(Box::new(shader) as Box<dyn ResourceData>)
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kasset::{MemoryResourceIo, ResourceManager};

    fn manager_with(path: &str, source: &str) -> ResourceManager {
        let mut io = MemoryResourceIo::new();
        io.add(path, source);
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(ShaderLoader);
        manager
    }

    #[test]
    fn loads_and_validates() {
        let manager = manager_with(
            "ok.wgsl",
            "@fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
        );

        let shader = manager.request_blocking::<Shader>("ok.wgsl").unwrap();

        assert_eq!(shader.data_ref().unwrap().fragment_entry(), Some("fs_main"));
    }

    #[test]
    fn invalid_shader_fails_to_load() {
        let manager = manager_with("bad.wgsl", "@fragment fn broken( {");

        let error = manager
            .request_blocking::<Shader>("bad.wgsl")
            .expect_err("非法着色器应当加载失败");

        // 错误在加载期就暴露，而不是等到创建管线时才崩。
        assert!(error.to_string().contains("WGSL"));
    }
}
