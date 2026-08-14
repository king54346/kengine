//! 资源加载错误。

use std::{error::Error, fmt, path::PathBuf, sync::Arc};

/// 资源加载失败的原因。
///
/// 内部用 `Arc` 持有，因此可以廉价克隆——同一个资源句柄可能被多处查询错误。
#[derive(Debug, Clone)]
pub enum LoadError {
    /// 没有能处理该扩展名的加载器。
    NoLoader {
        /// 请求的路径。
        path: PathBuf,
        /// 提取到的扩展名，无扩展名时为空串。
        extension: String,
    },
    /// 读取文件失败。
    Io {
        /// 请求的路径。
        path: PathBuf,
        /// 底层 IO 错误。
        source: Arc<std::io::Error>,
    },
    /// 加载器自身报告的错误（格式非法、解析失败等）。
    Custom(Arc<dyn Error + Send + Sync>),
}

impl LoadError {
    /// 把任意错误包装成 [`LoadError::Custom`]。
    pub fn custom<E: Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Custom(Arc::new(error))
    }

    /// 用一段文本构造错误。
    pub fn message(text: impl Into<String>) -> Self {
        Self::Custom(Arc::new(MessageError(text.into())))
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLoader { path, extension } => {
                write!(
                    f,
                    "没有注册能处理扩展名 `{extension}` 的加载器（路径：{}）",
                    path.display()
                )
            }
            Self::Io { path, source } => {
                write!(f, "读取 {} 失败：{source}", path.display())
            }
            Self::Custom(error) => write!(f, "{error}"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(&**source),
            Self::Custom(error) => Some(&**error),
            Self::NoLoader { .. } => None,
        }
    }
}

/// [`LoadError::message`] 内部使用的简单文本错误。
#[derive(Debug)]
struct MessageError(String);

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for MessageError {}
