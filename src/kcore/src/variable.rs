//! A wrapper for a variable that hold additional flags, allowing the variable to be
//! inherited from a parent (a prefab, for example).

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
    /// A set of possible variable flags.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    pub struct VariableFlags: u8 {
        /// Nothing.
        const NONE = 0;
        /// A variable was externally modified.
        const MODIFIED = 0b0000_0001;
        /// A variable must be synced with respective variable from data model.
        const NEED_SYNC = 0b0000_0010;
    }
}

/// An error that can occur while inheriting a property.
#[derive(Debug)]
pub enum InheritError {
    /// Types of properties mismatch.
    TypesMismatch {
        /// Type of left property.
        left_type: TypeId,
        /// Type of right property.
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

/// A wrapper for a variable that hold additional flag, that tells that initial value was changed
/// at runtime. Such wrapper is used in a prefab-based workflow: a variable that was not touched
/// by a user inherits its value from the parent prefab, while a modified one keeps its own value.
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
        // `flags` intentionally excluded, they're a bookkeeping detail rather than
        // part of the value's identity.
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
    /// Creates a new variable that is marked as modified, so it will keep its own value
    /// instead of inheriting one from a parent.
    pub fn new_modified(value: T) -> Self {
        Self {
            value,
            flags: Cell::new(VariableFlags::MODIFIED),
        }
    }

    /// Creates a new variable that is **not** marked as modified, so it will inherit its
    /// value from a parent.
    pub fn new_non_modified(value: T) -> Self {
        Self {
            value,
            flags: Cell::new(VariableFlags::NONE),
        }
    }

    /// Creates a new variable with the given flags.
    pub fn new_with_flags(value: T, flags: VariableFlags) -> Self {
        Self {
            value,
            flags: Cell::new(flags),
        }
    }

    /// Replaces the value and marks the variable as modified, returning the previous value.
    pub fn set_value_and_mark_modified(&mut self, value: T) -> T {
        self.mark_modified_and_need_sync();
        std::mem::replace(&mut self.value, value)
    }

    /// Replaces the value without marking the variable as modified, returning the previous value.
    pub fn set_value_silent(&mut self, value: T) -> T {
        std::mem::replace(&mut self.value, value)
    }

    /// Returns `true` if the variable was modified and should not be overwritten during inheritance.
    pub fn is_modified(&self) -> bool {
        self.flags.get().contains(VariableFlags::MODIFIED)
    }

    /// Returns a shared reference to the wrapped value.
    pub fn get_value_ref(&self) -> &T {
        &self.value
    }

    /// Returns a mutable reference to the wrapped value and marks the variable as modified.
    pub fn get_value_mut_and_mark_modified(&mut self) -> &mut T {
        self.mark_modified_and_need_sync();
        &mut self.value
    }

    /// Returns a mutable reference to the wrapped value **without** marking the variable
    /// as modified.
    pub fn get_value_mut_silent(&mut self) -> &mut T {
        &mut self.value
    }

    /// Consumes the wrapper and returns the wrapped value.
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

    // The remaining `as_*` accessors delegate to the wrapped value so that an inheritable
    // collection still behaves like a collection through reflection.

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
