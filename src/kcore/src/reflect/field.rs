use crate::reflect::{CastError, Reflect};
use std::any::TypeId;
use std::fmt;
use std::ops::Deref;

#[derive(Debug)]
pub struct FieldMetadata<'s> {
    /// A name of the property.
    pub name: &'s str,

    /// A human-readable name of the property.
    pub display_name: &'s str,

    /// Tag of the property. Could be used to group properties by a certain criteria or to find a
    /// specific property by its tag.
    pub tag: &'s str,

    /// Doc comment content.
    pub doc: &'s str,

    /// A property is not meant to be edited.
    pub read_only: bool,

    /// Only for dynamic collections (Vec, etc) - means that its size cannot be changed, however the
    /// _items_ of the collection can still be changed.
    pub immutable_collection: bool,

    /// A minimal value of the property. Works only with numeric properties!
    pub min_value: Option<f64>,

    /// A minimal value of the property. Works only with numeric properties!
    pub max_value: Option<f64>,

    /// A minimal value of the property. Works only with numeric properties!
    pub step: Option<f64>,

    /// Maximum amount of decimal places for a numeric property.
    pub precision: Option<usize>,
}

pub struct FieldRef<'a, 'b> {
    /// A reference to field's metadata.
    pub metadata: &'a FieldMetadata<'b>,

    /// An reference to the actual value of the property.
    pub value: &'a dyn Reflect,
}

impl<'b> Deref for FieldRef<'_, 'b> {
    type Target = FieldMetadata<'b>;

    fn deref(&self) -> &Self::Target {
        self.metadata
    }
}

impl FieldRef<'_, '_> {
    /// Tries to cast a value to a given type.
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
    /// A reference to field's metadata.
    pub metadata: &'a FieldMetadata<'b>,

    /// An reference to the actual value of the property. This is "non-mangled" reference, which
    /// means that while `field/fields/field_mut/fields_mut` might return a reference to other value,
    /// than the actual field, the `value` is guaranteed to be a reference to the real value.
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