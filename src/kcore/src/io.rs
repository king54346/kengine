//! 文件加载。提供异步 API，使文件访问在原生平台和 Web 平台上保持统一
//!（Web 平台不支持同步文件访问）。

use std::{
    fmt::{Display, Formatter},
    path::Path,
};

/// 加载文件时可能出现的错误。
#[derive(Debug)]
pub enum FileError {
    /// 发生了 I/O 错误。
    Io(std::io::Error),
    /// 发生了平台相关的错误。
    Custom(String),
}

impl Display for FileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Io(err) => Display::fmt(err, f),
            FileError::Custom(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FileError::Io(err) => Some(err),
            FileError::Custom(_) => None,
        }
    }
}

impl From<std::io::Error> for FileError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// 读取给定路径文件的全部内容。
pub async fn load_file<P: AsRef<Path>>(path: P) -> Result<Vec<u8>, FileError> {
    Ok(std::fs::read(path)?)
}

/// 如果给定路径指向已存在的条目，则返回 `true`。
pub fn exists<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().exists()
}

/// 如果给定路径指向目录，则返回 `true`。
pub fn is_dir<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().is_dir()
}

/// 如果给定路径指向普通文件，则返回 `true`。
pub fn is_file<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().is_file()
}
