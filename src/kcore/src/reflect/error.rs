use crate::reflect::Reflect;
use std::any::TypeId;
use std::fmt;
use std::fmt::{Display, Formatter};

/// 路径字符串查询失败时返回的错误。
#[derive(Debug, PartialEq, Eq)]
pub enum ReflectPathError<'a> {
    // 语法错误
    UnclosedBrackets { s: &'a str },
    InvalidIndexSyntax { s: &'a str },

    // 访问错误
    UnknownField { s: &'a str },
    NoItemForIndex { s: &'a str },

    // 类型转换错误
    InvalidDowncast,
    NotAnArray,
}

impl std::error::Error for ReflectPathError<'_> {}

impl Display for ReflectPathError<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ReflectPathError::UnclosedBrackets { s } => {
                write!(f, "括号未关闭：`{s}`")
            }
            ReflectPathError::InvalidIndexSyntax { s } => {
                write!(f, "非法索引语法：`{s}`")
            }
            ReflectPathError::UnknownField { s } => {
                write!(f, "未知字段：`{s}`")
            }
            ReflectPathError::NoItemForIndex { s } => {
                write!(f, "索引无对应元素：`{s}`")
            }
            ReflectPathError::InvalidDowncast => {
                write!(
                    f,
                    "路径解析后向下转型目标类型失败"
                )
            }
            ReflectPathError::NotAnArray => {
                write!(f, "尝试解析索引访问，但该 reflect 类型未实现列表 API")
            }
        }
    }
}

/// 类型转换失败时的错误。
#[derive(Debug)]
pub enum CastError {
    /// 给定类型与预期类型不匹配。
    TypeMismatch {
        /// 字段名称。
        property_name: String,

        /// 预期类型标识符。
        expected_type_id: TypeId,

        /// 实际类型标识符。
        actual_type_id: TypeId,
    },
}

impl std::error::Error for CastError {}

impl Display for CastError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            CastError::TypeMismatch { property_name, .. } => {
                write!(
                    f,
                    "属性 {property_name:?} 的类型与预期不匹配"
                )
            }
        }
    }
}

#[derive(Debug)]
pub enum SetFieldError {
    NoSuchField {
        name: String,
        value: Box<dyn Reflect>,
    },
    InvalidValue {
        field_type_name: &'static str,
        value: Box<dyn Reflect>,
    },
}

impl std::error::Error for SetFieldError {}

impl Display for SetFieldError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SetFieldError::NoSuchField { name, value } => {
                write!(f, "值 {value:?} 不存在字段 {name:?}")
            }
            SetFieldError::InvalidValue {
                field_type_name,
                value,
            } => write!(
                f,
                "字段类型 {field_type_name} 的值无效：{value:?}"
            ),
        }
    }
}

#[derive(Debug)]
pub enum SetFieldByPathError<'p> {
    InvalidPath {
        value: Box<dyn Reflect>,
        reason: ReflectPathError<'p>,
    },
    InvalidValue {
        field_type_name: &'static str,
        value: Box<dyn Reflect>,
    },
    SetFieldError(SetFieldError),
}

impl std::error::Error for SetFieldByPathError<'_> {}

impl Display for SetFieldByPathError<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SetFieldByPathError::InvalidPath { value, reason } => {
                write!(f, "无效路径：{value:?}。原因：{reason}")
            }
            SetFieldByPathError::InvalidValue {
                field_type_name,
                value,
            } => {
                write!(f, "无效值：{value:?}。类型：{field_type_name}")
            }
            SetFieldByPathError::SetFieldError(set_field_error) => Display::fmt(set_field_error, f),
        }
    }
}