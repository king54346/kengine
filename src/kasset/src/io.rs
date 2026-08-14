//! 资源 IO 抽象。
//!
//! 加载器不直接碰 `std::fs`，而是通过 [`ResourceIo`] 读取字节。
//! 这样将来接入资源包、网络或虚拟文件系统时，加载器代码无需改动。

use crate::error::LoadError;
use ktask::BoxedFuture;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// 资源字节的来源。
pub trait ResourceIo: Send + Sync + 'static {
    /// 读取整个文件。
    fn load_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, LoadError>>;

    /// 路径是否存在。
    fn exists(&self, path: &Path) -> bool;
}

/// 直接读取本地文件系统的实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct FsResourceIo;

impl ResourceIo for FsResourceIo {
    fn load_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, LoadError>> {
        Box::pin(async move {
            std::fs::read(path).map_err(|source| LoadError::Io {
                path: path.to_path_buf(),
                source: Arc::new(source),
            })
        })
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// 内存文件系统，主要用于测试与内嵌资源。
#[derive(Debug, Default)]
pub struct MemoryResourceIo {
    files: fxhash::FxHashMap<PathBuf, Vec<u8>>,
}

impl MemoryResourceIo {
    /// 创建空的内存文件系统。
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入一个虚拟文件。
    pub fn add(&mut self, path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) {
        self.files.insert(path.into(), contents.into());
    }

    /// 链式写入。
    pub fn with(mut self, path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) -> Self {
        self.add(path, contents);
        self
    }
}

impl ResourceIo for MemoryResourceIo {
    fn load_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, LoadError>> {
        Box::pin(async move {
            self.files.get(path).cloned().ok_or_else(|| LoadError::Io {
                path: path.to_path_buf(),
                source: Arc::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "内存文件系统中不存在该路径",
                )),
            })
        })
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
}
