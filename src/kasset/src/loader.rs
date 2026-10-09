//! 资源加载器。

use crate::{error::LoadError, io::ResourceIo, resource::ResourceData};
use kcore::uuid::Uuid;
use ktask::BoxedFuture;
use std::{path::PathBuf, sync::Arc};

/// 加载器返回的结果。
pub type LoaderResult = Result<Box<dyn ResourceData>, LoadError>;

/// 加载任务的类型擦除 future。
pub type BoxedLoaderFuture = BoxedFuture<'static, LoaderResult>;

/// 把某类文件解析成资源数据。
///
/// 实现示例见 `kasset` 的测试，或引擎里的纹理/网格加载器。
pub trait ResourceLoader: Send + Sync + 'static {
    /// 本加载器支持的扩展名（不含点号，大小写不敏感）。
    fn extensions(&self) -> &[&str];

    /// 产出的资源数据类型的 UUID。
    fn data_type_uuid(&self) -> Uuid;

    /// 执行加载。运行在 IO 线程池上，不要在这里做阻塞主线程的事。
    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture;

    /// 扩展名是否被支持，大小写不敏感。
    fn supports_extension(&self, extension: &str) -> bool {
        self.extensions()
            .iter()
            .any(|e| e.eq_ignore_ascii_case(extension))
    }
}

/// 已注册加载器的集合。
#[derive(Default)]
pub struct LoaderContainer {
    loaders: Vec<Arc<dyn ResourceLoader>>,
}

impl LoaderContainer {
    /// 创建空容器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个加载器。后注册的优先匹配，便于覆盖内置实现。
    pub fn add(&mut self, loader: impl ResourceLoader) {
        self.loaders.push(Arc::new(loader));
    }

    /// 按扩展名查找加载器。
    pub fn find(&self, extension: &str) -> Option<Arc<dyn ResourceLoader>> {
        self.loaders
            .iter()
            .rev()
            .find(|loader| loader.supports_extension(extension))
            .cloned()
    }

    /// 按文件名查找加载器，**复合扩展名优先**：`sky.hdr.jpg` 先找认 `hdr.jpg` 的，
    /// 没有再找认 `jpg` 的。返回加载器和匹配上的那个扩展名。
    ///
    /// 只看最后一段的话，认 `jpg` 的 Ultra HDR 加载器和普通贴图加载器只能二选一——
    /// 后注册的那个吃掉所有 `.jpg`，同一个程序里没法既读 HDR 全景图又读普通 JPEG 贴图。
    pub fn find_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<(Arc<dyn ResourceLoader>, String)> {
        let name = path.file_name()?.to_string_lossy().to_string();
        // 第一个点之后的每个后缀，长的在前：`a.hdr.jpg` → `hdr.jpg`、`jpg`。
        let mut candidates: Vec<&str> = name
            .match_indices('.')
            .map(|(at, _)| &name[at + 1..])
            .collect();
        candidates.retain(|candidate| !candidate.is_empty());
        candidates.into_iter().find_map(|extension| {
            self.find(extension)
                .map(|loader| (loader, extension.to_string()))
        })
    }

    /// 已注册的加载器数量。
    pub fn len(&self) -> usize {
        self.loaders.len()
    }

    /// 是否没有注册任何加载器。
    pub fn is_empty(&self) -> bool {
        self.loaders.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct Named(&'static [&'static str]);

    impl ResourceLoader for Named {
        fn extensions(&self) -> &[&str] {
            self.0
        }
        fn data_type_uuid(&self) -> Uuid {
            Uuid::nil()
        }
        fn load(&self, _path: PathBuf, _io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
            unreachable!()
        }
    }

    #[test]
    fn compound_extensions_win_over_the_last_segment() {
        let mut loaders = LoaderContainer::new();
        // 复合扩展名的先注册：不靠「后注册的优先」也要选中它。
        loaders.add(Named(&["hdr.jpg"]));
        loaders.add(Named(&["jpg", "png"]));
        let found = |path: &str| {
            loaders
                .find_for_path(Path::new(path))
                .map(|(_, extension)| extension)
        };
        assert_eq!(found("textures/sky_2k.hdr.jpg").as_deref(), Some("hdr.jpg"));
        assert_eq!(found("textures/floor.jpg").as_deref(), Some("jpg"));
        assert_eq!(found("textures/rgb-256x256.png").as_deref(), Some("png"));
        assert_eq!(found("textures/a.b.c.png").as_deref(), Some("png"));
        assert_eq!(found("textures/noext"), None);
    }
}
