use crate::reflect::{CastError, Reflect};
use std::any::TypeId;
use std::fmt;
use std::ops::Deref;

#[derive(Debug)]
pub struct FieldMetadata<'s> {
    /// 属性名称。
    pub name: &'s str,

    /// 人类可读的属性显示名称。
    pub display_name: &'s str,

    /// 属性标签，可用于按条件分组或按标签查找特定属性。
    pub tag: &'s str,

    /// 文档注释内容。
    pub doc: &'s str,

    /// 属性不可编辑（只读）。
    pub read_only: bool,

    /// 仅对动态集合（Vec 等）有效——表示集合大小不可修改，但集合内容项仍可修改。
    pub immutable_collection: bool,

    /// 属性的最小值。仅对数值属性有效！
    pub min_value: Option<f64>,

    /// 属性的最大值。仅对数值属性有效！
    pub max_value: Option<f64>,

    /// 属性的步进值。仅对数值属性有效！
    pub step: Option<f64>,

    /// 数值属性的最大小数位数。
    pub precision: Option<usize>,
}

pub struct FieldRef<'a, 'b> {
    /// 字段元数据的引用。
    pub metadata: &'a FieldMetadata<'b>,

    /// 属性实际值的引用。
    pub value: &'a dyn Reflect,
}

impl<'b> Deref for FieldRef<'_, 'b> {
    type Target = FieldMetadata<'b>;

    fn deref(&self) -> &Self::Target {
        self.metadata
    }
}

impl FieldRef<'_, '_> {
    /// 尝试将值转换为给定类型。
    pub fn cast_value<T: Reflect>(&self) -> Result<&T, CastError> {
        match self.value.downcast_ref::<T>() {
            Some(value) => Ok(value),
            None => Err(CastError::TypeMismatch {
                property_name: self.metadata.name.to_string(),
                expected_type_id: TypeId::of::<T>(),
                actual_type_id: self.value.type_id(),
            }),
        }
    }
}

impl fmt::Debug for FieldRef<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldInfo")
            .field("metadata", &self.metadata)
            .field("value", &format_args!("{:?}", self.value as *const _))
            .finish()
    }
}

impl PartialEq<Self> for FieldRef<'_, '_> {
    fn eq(&self, other: &Self) -> bool {
        let value_ptr_a = self.value as *const _ as *const ();
        let value_ptr_b = other.value as *const _ as *const ();

        std::ptr::eq(value_ptr_a, value_ptr_b)
    }
}

pub struct FieldMut<'a, 'b> {
    /// 字段元数据的引用。
    pub metadata: &'a FieldMetadata<'b>,

    /// 属性实际值的可变引用。这是"未经篡改"的引用——
    /// 即使 `field/fields/field_mut/fields_mut` 可能返回其他值的引用，
    /// `value` 保证是对真实字段的引用。
    pub value: &'a mut dyn Reflect,
}

impl<'b> Deref for FieldMut<'_, 'b> {
    type Target = FieldMetadata<'b>;

    fn deref(&self) -> &Self::Target {
        self.metadata
    }
}

impl fmt::Debug for FieldMut<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldInfo")
            .field("metadata", &self.metadata)
            .field("value", &format_args!("{:?}", self.value as *const _))
            .finish()
    }
}

impl PartialEq<Self> for FieldMut<'_, '_> {
    fn eq(&self, other: &Self) -> bool {
        let value_ptr_a = self.value as *const _ as *const ();
        let value_ptr_b = other.value as *const _ as *const ();

        std::ptr::eq(value_ptr_a, value_ptr_b)
    }
}