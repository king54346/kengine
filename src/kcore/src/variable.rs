//! 带附加标志的变量包装器，允许从父对象（如 prefab）继承值。

use crate::{
    reflect::{prelude::*, ReflectHandle, ReflectHashSet},
    visitor::prelude::*,
};
use bitflags::bitflags;
use std::{
    any::TypeId,
    cell::Cell,
    fmt::{Debug, Display, Formatter},
    ops::{Deref, DerefMut},
};

bitflags! {
    /// 变量可能拥有的标志集合。
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    pub struct VariableFlags: u8 {
    /// 无标志。
    const NONE = 0;
    /// 变量已被外部修改。
    const MODIFIED = 0b0000_0001;
    /// 变量需要与数据模型中对应的变量同步。
    const NEED_SYNC = 0b0000_0010;
    }
}

/// 属性继承失败时的错误。
#[derive(Debug)]
pub enum InheritError {
    /// 属性类型不匹配。
    TypesMismatch {
        /// 左侧属性的类型。
        left_type: TypeId,
        /// 右侧属性的类型。
        right_type: TypeId,
    },
}

impl Display for InheritError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            InheritError::TypesMismatch {
                left_type,
                right_type,
            } => write!(
                f,
                "unable to inherit a value: types mismatch ({left_type:?} vs {right_type:?})"
            ),
        }
    }
}

impl std::error::Error for InheritError {}

/// 带附加标志的变量包装器，用于追踪初始值是否在运行时被修改。
/// 常用于基于 prefab 的工作流：未被用户修改的变量从父 prefab 继承值，
/// 而已修改的变量保持自己的值。
#[derive(Debug)]
pub struct InheritableVariable<T> {
    value: T,
    flags: Cell<VariableFlags>,
}

impl<T: Clone> Clone for InheritableVariable<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            flags: self.flags.clone(),
        }
    }
}

impl<T> From<T> for InheritableVariable<T> {
    fn from(v: T) -> Self {
        InheritableVariable::new_modified(v)
    }
}

impl<T: PartialEq> PartialEq for InheritableVariable<T> {
    fn eq(&self, other: &Self) -> bool {
        // `flags` 有意排除在外，它只是内部账本信息，不是值的实际内容。
        self.value.eq(&other.value)
    }
}

impl<T: Eq> Eq for InheritableVariable<T> {}

impl<T: Default> Default for InheritableVariable<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            flags: Cell::new(VariableFlags::NONE),
        }
    }
}

impl<T> InheritableVariable<T> {
    /// 创建一个已标记为修改过的新变量，因此它会保留自己的值，
    /// 而不是从父对象继承。
    pub fn new_modified(value: T) -> Self {
        Self {
            value,
            flags: Cell::new(VariableFlags::MODIFIED),
        }
    }

    /// 创建一个**未**标记为修改过的新变量，因此它会从父对象继承值。
    pub fn new_non_modified(value: T) -> Self {
        Self {
            value,
            flags: Cell::new(VariableFlags::NONE),
        }
    }

    /// 使用给定标志创建新变量。
    pub fn new_with_flags(value: T, flags: VariableFlags) -> Self {
        Self {
            value,
            flags: Cell::new(flags),
        }
    }

    /// 替换值并将变量标记为已修改，返回旧值。
    pub fn set_value_and_mark_modified(&mut self, value: T) -> T {
        self.mark_modified_and_need_sync();
        std::mem::replace(&mut self.value, value)
    }

    /// 替换值但不将变量标记为已修改，返回旧值。
    pub fn set_value_silent(&mut self, value: T) -> T {
        std::mem::replace(&mut self.value, value)
    }

    /// 如果变量已被修改并且在继承时不应被覆盖，则返回 `true`。
    pub fn is_modified(&self) -> bool {
        self.flags.get().contains(VariableFlags::MODIFIED)
    }

    /// 返回被包装值的共享引用。
    pub fn get_value_ref(&self) -> &T {
        &self.value
    }

    /// 返回被包装值的可变引用，并将变量标记为已修改。
    pub fn get_value_mut_and_mark_modified(&mut self) -> &mut T {
        self.mark_modified_and_need_sync();
        &mut self.value
    }

    /// 返回被包装值的可变引用，**但不**将变量标记为已修改。
    pub fn get_value_mut_silent(&mut self) -> &mut T {
        &mut self.value
    }

    /// 消耗包装器并返回其中的值。
    pub fn take(self) -> T {
        self.value
    }

    fn mark_modified_and_need_sync(&mut self) {
        self.flags
            .get_mut()
            .insert(VariableFlags::MODIFIED | VariableFlags::NEED_SYNC);
    }
}

impl<T> Deref for InheritableVariable<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for InheritableVariable<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.mark_modified_and_need_sync();
        &mut self.value
    }
}

impl<T: Visit> Visit for InheritableVariable<T> {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut bits = self.flags.get().bits();
        bits.visit("Flags", &mut region)?;
        self.flags.set(VariableFlags::from_bits_truncate(bits));

        self.value.visit("Value", &mut region)?;

        Ok(())
    }
}

static VALUE_METADATA: FieldMetadata = FieldMetadata {
    name: "Content",
    display_name: "Content",
    tag: "",
    read_only: false,
    immutable_collection: false,
    min_value: None,
    max_value: None,
    step: None,
    precision: None,
    doc: "",
};

impl<T: Reflect + Clone + PartialEq> Reflect for InheritableVariable<T> {
    fn type_info() -> TypeInfo {
        TypeInfo {
            source_path: file!(),
            type_name: std::any::type_name::<Self>(),
            assembly_name: env!("CARGO_PKG_NAME"),
            doc_comment: "",
            derived_types: &[],
            type_uuid: combine_uuids(
                uuid!("2f3d4e6a-8b1c-4d5e-9f70-8a1b2c3d4e5f"),
                T::type_info().type_uuid,
            ),
        }
    }

    fn type_info_ref(&self) -> TypeInfo {
        Self::type_info()
    }

    fn try_clone_box(&self) -> Option<Box<dyn Reflect>> {
        Some(Box::new(self.clone()))
    }

    fn try_compare(&self, other: &dyn Reflect) -> Option<bool> {
        (other as &dyn Any)
            .downcast_ref::<Self>()
            .map(|other| other == self)
    }

    fn fields_ref(&self, func: &mut dyn FnMut(&[FieldRef])) {
        func(&[FieldRef {
            metadata: &VALUE_METADATA,
            value: &self.value,
        }])
    }

    fn fields_mut(&mut self, func: &mut dyn FnMut(&mut [FieldMut])) {
        // Accessing a field mutably is an explicit modification of the wrapped value.
        self.mark_modified_and_need_sync();
        func(&mut [FieldMut {
            metadata: &VALUE_METADATA,
            value: &mut self.value,
        }])
    }

    fn set(&mut self, value: Box<dyn Reflect>) -> Result<Box<dyn Reflect>, Box<dyn Reflect>> {
        let this = std::mem::replace(self, value.take()?);
        Ok(Box::new(this))
    }

    fn field_direct_ref(&self, index: usize) -> Option<FieldRef<'_, '_>> {
        (index == 0).then_some(FieldRef {
            metadata: &VALUE_METADATA,
            value: &self.value,
        })
    }

    fn field_direct_mut(&mut self, index: usize) -> Option<FieldMut<'_, '_>> {
        if index != 0 {
            return None;
        }
        self.mark_modified_and_need_sync();
        Some(FieldMut {
            metadata: &VALUE_METADATA,
            value: &mut self.value,
        })
    }

    fn as_inheritable_variable(&self) -> Option<&dyn ReflectInheritableVariable> {
        Some(self)
    }

    fn as_inheritable_variable_mut(&mut self) -> Option<&mut dyn ReflectInheritableVariable> {
        Some(self)
    }

    // 剩余的 `as_*` 访问器会委托给被包装值，
    // 这样一个可继承的集合在反射中仍然表现得像集合。

    fn as_array(&self) -> Option<&dyn ReflectArray> {
        self.value.as_array()
    }

    fn as_array_mut(&mut self) -> Option<&mut dyn ReflectArray> {
        self.value.as_array_mut()
    }

    fn as_list(&self) -> Option<&dyn ReflectList> {
        self.value.as_list()
    }

    fn as_list_mut(&mut self) -> Option<&mut dyn ReflectList> {
        self.value.as_list_mut()
    }

    fn as_hash_map(&self) -> Option<&dyn ReflectHashMap> {
        self.value.as_hash_map()
    }

    fn as_hash_map_mut(&mut self) -> Option<&mut dyn ReflectHashMap> {
        self.value.as_hash_map_mut()
    }

    fn as_hash_set(&self) -> Option<&dyn ReflectHashSet> {
        self.value.as_hash_set()
    }

    fn as_hash_set_mut(&mut self) -> Option<&mut dyn ReflectHashSet> {
        self.value.as_hash_set_mut()
    }

    fn as_handle(&self) -> Option<&dyn ReflectHandle> {
        self.value.as_handle()
    }

    fn as_handle_mut(&mut self) -> Option<&mut dyn ReflectHandle> {
        self.value.as_handle_mut()
    }
}

impl<T: Reflect + Clone + PartialEq> ReflectInheritableVariable for InheritableVariable<T> {
    fn try_inherit(
        &mut self,
        parent: &dyn ReflectInheritableVariable,
        _ignored_types: &[TypeId],
    ) -> Result<Option<Box<dyn Reflect>>, InheritError> {
        if self.is_modified() {
            return Ok(None);
        }

        let parent_value = parent.inner_value_ref();
        let Some(parent_value) = parent_value.downcast_ref::<T>() else {
            return Err(InheritError::TypesMismatch {
                left_type: TypeId::of::<T>(),
                right_type: parent_value.type_id(),
            });
        };

        let previous = std::mem::replace(&mut self.value, parent_value.clone());
        Ok(Some(Box::new(previous)))
    }

    fn reset_modified_flag(&mut self) {
        self.flags.get_mut().remove(VariableFlags::MODIFIED);
    }

    fn flags(&self) -> VariableFlags {
        self.flags.get()
    }

    fn set_flags(&mut self, flags: VariableFlags) {
        self.flags.set(flags)
    }

    fn is_modified(&self) -> bool {
        self.flags.get().contains(VariableFlags::MODIFIED)
    }

    fn mark_modified(&mut self) {
        self.flags.get_mut().insert(VariableFlags::MODIFIED)
    }

    fn inner_value_mut(&mut self) -> &mut dyn Reflect {
        &mut self.value
    }

    fn inner_value_ref(&self) -> &dyn Reflect {
        &self.value
    }
}
