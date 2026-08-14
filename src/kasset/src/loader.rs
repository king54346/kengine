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

    /// 已注册的加载器数量。
    pub fn len(&self) -> usize {
        self.loaders.len()
    }

    /// 是否没有注册任何加载器。
    pub fn is_empty(&self) -> bool {
        self.loaders.is_empty()
    }
}
