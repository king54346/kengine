//! 序列化/反序列化过程中可能发生的错误。

use crate::io::FileError;
use crate::visitor::Visitor;
use base64::DecodeError;
use std::num::{ParseFloatError, ParseIntError};
use std::{
    error::Error,
    fmt::{Display, Formatter},
    string::FromUtf8Error,
};

/// 读写 [`crate::visitor::Visitor`] 时可能发生的错误。
#[derive(Debug)]
pub enum VisitError {
    /// 多种原因导致的错误集合（当有多种访问方式且均失败时产生）。
    Multiple(Vec<VisitError>),
    /// 读写 Visitor 数据文件时发生的 I/O 错误。
    Io(std::io::Error),
    /// 字段以字节形式编码时，字节前缀用于标识字段类型；
    /// 此错误表示解码时在该字节位置发现了未知值。
    UnknownFieldType(u8),
    /// 在读取模式下访问一个不存在的字段名。
    FieldDoesNotExist(String),
    /// 在写入模式下访问一个已存在的字段名（重复）。
    FieldAlreadyExists(String),
    /// 在写入模式下进入一个已存在的区域（重复）。
    RegionAlreadyExists(String),
    /// 当前节点句柄无效，不指向任何真实节点。
    InvalidCurrentNode,
    /// 在读取模式下访问字段时，该字段原本是以不同类型写入的。
    FieldTypeDoesNotMatch {
        /// 预期的 [`crate::visitor::FieldKind`] 变体名，例如 "FieldKind::F64"。
        expected: &'static str,
        /// 实际 [`crate::visitor::FieldKind`] 的 Debug 表示。
        actual: String,
    },
    /// 在读取模式下进入一个不存在的区域。
    RegionDoesNotExist(String),
    /// Visitor 尝试离开当前节点，但没有当前节点（正常情况下不应发生）。
    NoActiveNode,
    /// Visitor 数据开头缺少魔数（[`crate::Visitor::MAGIC_BINARY_CURRENT`] 或
    /// [`crate::Visitor::MAGIC_ASCII_CURRENT`]）。
    NotSupportedFormat,
    /// 某段字节序列不是合法的 UTF-8 格式。
    InvalidName,
    /// Visitor 数据可能存在自引用（如多个 `Rc` 指向同一共享值）。
    /// 此时 Visitor 只存储一次数据，后续引用回指到首次出现位置。
    /// 此错误表示某个引用回指到了类型不匹配的值。
    TypeMismatch {
        /// 发生错误时正在访问的类型。
        expected: &'static str,
        /// `Rc` 或 `Arc` 中实际存储的类型。
        actual: &'static str,
    },
    /// 尝试访问一个已被可变借用的 RefCell。
    RefCellAlreadyMutableBorrowed,
    /// 纯文本错误消息，可表示几乎任何情况。
    User(String),
    /// `Rc` 和 `Arc` 在 Visitor 数据中存储基于内部指针的 ID；
    /// 此错误表示读取时发现某个 ID 值为 0。
    UnexpectedRcNullIndex,
    /// 尝试访问互斥锁时发生毒化错误。
    PoisonedMutex,
    /// 尝试从文件解码 Visitor 数据时遇到文件加载错误。
    FileLoadError(FileError),
    /// 整数解析错误。
    ParseIntError(ParseIntError),
    /// 浮点数解析错误。
    ParseFloatError(ParseFloatError),
    /// Base64 数据解码错误。
    DecodeError(DecodeError),
    /// UUID 字符串解析错误。
    UuidError(uuid::Error),
    /// 任意错误。
    Any(Box<dyn Error + Send + Sync>),
    /// 未处理的枚举变体（通常意味着非穷举匹配中缺少某个分支）。
    UnhandledEnumVariant,
}

impl Error for VisitError {}

impl VisitError {
    /// 创建包含字段名和当前 Visitor 节点路径的 [`VisitError::FieldDoesNotExist`]。
    pub fn field_does_not_exist(name: &str, visitor: &Visitor) -> Self {
        Self::FieldDoesNotExist(visitor.breadcrumbs() + " > " + name)
    }
    /// 将两个错误合并为一个 [`VisitError::Multiple`]。
    pub fn multiple(self, other: Self) -> Self {
        match (self, other) {
            (Self::Multiple(mut a), Self::Multiple(mut b)) => {
                a.append(&mut b);
                Self::Multiple(a)
            }
            (Self::Multiple(mut a), b) => {
                a.push(b);
                Self::Multiple(a)
            }
            (a, Self::Multiple(mut b)) => {
                b.push(a);
                Self::Multiple(b)
            }
            (a, b) => Self::Multiple(vec![a, b]),
        }
    }
}

impl Display for VisitError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::Multiple(errs) => {
                write!(f, "多个错误：[")?;
                for err in errs {
                    write!(f, "{err};")?;
                }
                write!(f, "]")
            }
            Self::Io(io) => write!(f, "I/O 错误：{io}"),
            Self::UnknownFieldType(type_index) => write!(f, "未知字段类型 {type_index}"),
            Self::FieldDoesNotExist(name) => write!(f, "字段不存在：{name}"),
            Self::FieldAlreadyExists(name) => write!(f, "字段已存在：{name}"),
            Self::RegionAlreadyExists(name) => write!(f, "区域已存在：{name}"),
            Self::InvalidCurrentNode => write!(f, "无效的当前节点"),
            Self::FieldTypeDoesNotMatch { expected, actual } => {
                write!(f, "字段类型不匹配。预期：{expected}，实际：{actual}")
            }
            Self::RegionDoesNotExist(name) => write!(f, "区域不存在：{name}"),
            Self::NoActiveNode => write!(f, "无活跃节点"),
            Self::NotSupportedFormat => write!(f, "不支持的格式"),
            Self::InvalidName => write!(f, "非法名称"),
            Self::TypeMismatch { expected, actual } => {
                write!(f, "类型不匹配。预期：{expected}，实际：{actual}")
            }
            Self::RefCellAlreadyMutableBorrowed => write!(f, "RefCell 已被可变借用"),
            Self::User(msg) => write!(f, "用户自定义错误：{msg}"),
            Self::UnexpectedRcNullIndex => write!(f, "Rc 空索引（意外）"),
            Self::PoisonedMutex => write!(f, "尝试锁定已毒化的互斥锁"),
            Self::FileLoadError(e) => write!(f, "文件加载错误：{e:?}"),
            Self::ParseIntError(e) => write!(f, "无法解析整数：{e:?}"),
            Self::ParseFloatError(e) => write!(f, "无法解析浮点数：{e:?}"),
            Self::DecodeError(e) => write!(f, "Base64 解码错误：{e:?}"),
            Self::UuidError(e) => write!(f, "UUID 错误：{e:?}"),
            Self::Any(e) => write!(f, "{e}"),
            VisitError::UnhandledEnumVariant => write!(f, "未处理的枚举变体"),
        }
    }
}

impl<T> From<std::sync::PoisonError<std::sync::MutexGuard<'_, T>>> for VisitError {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<'_, T>>) -> Self {
        Self::PoisonedMutex
    }
}

impl<T> From<std::sync::PoisonError<&mut T>> for VisitError {
    fn from(_: std::sync::PoisonError<&mut T>) -> Self {
        Self::PoisonedMutex
    }
}

impl<T> From<std::sync::PoisonError<std::sync::RwLockWriteGuard<'_, T>>> for VisitError {
    fn from(_: std::sync::PoisonError<std::sync::RwLockWriteGuard<'_, T>>) -> Self {
        Self::PoisonedMutex
    }
}

impl From<std::io::Error> for VisitError {
    fn from(io_err: std::io::Error) -> Self {
        Self::Io(io_err)
    }
}

impl From<FromUtf8Error> for VisitError {
    fn from(_: FromUtf8Error) -> Self {
        Self::InvalidName
    }
}

impl From<String> for VisitError {
    fn from(s: String) -> Self {
        Self::User(s)
    }
}

impl From<FileError> for VisitError {
    fn from(e: FileError) -> Self {
        Self::FileLoadError(e)
    }
}

impl From<ParseIntError> for VisitError {
    fn from(value: ParseIntError) -> Self {
        Self::ParseIntError(value)
    }
}

impl From<ParseFloatError> for VisitError {
    fn from(value: ParseFloatError) -> Self {
        Self::ParseFloatError(value)
    }
}

impl From<DecodeError> for VisitError {
    fn from(value: DecodeError) -> Self {
        Self::DecodeError(value)
    }
}

impl From<uuid::Error> for VisitError {
    fn from(value: uuid::Error) -> Self {
        Self::UuidError(value)
    }
}

impl From<Box<dyn Error + Send + Sync>> for VisitError {
    fn from(value: Box<dyn Error + Send + Sync>) -> Self {
        Self::Any(value)
    }
}
