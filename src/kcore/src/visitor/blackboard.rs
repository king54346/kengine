//! Blackboard is a container for arbitrary, shared data that is available during serialization and
//! deserialization. See [`Blackboard`] docs for more info.

use fxhash::FxHashMap;
use std::{
    any::{Any, TypeId},
    sync::Arc,
};

/// Blackboard is a container for arbitrary, shared data that is available during serialization and
/// deserialization. The main use of the blackboard is to pass some "global" data to the serializer.
/// For example, to deserialize a trait object, some sort of container with constructors is needed
/// that will create an object instance by its type uuid. Such a container can be passed to the
/// serializer using the blackboard.
#[derive(Default)]
pub struct Blackboard {
    items: FxHashMap<TypeId, Arc<dyn Any>>,
}

impl Blackboard {
    /// Creates a new empty blackboard.
    pub fn new() -> Self {
        Self {
            items: Default::default(),
        }
    }

    /// Registers a shared object in the blackboard. There could be only one object of the given
    /// type at the same time.
    pub fn register<T: Any>(&mut self, value: Arc<T>) {
        self.items.insert(TypeId::of::<T>(), value);
    }

    /// Tries to find an object of the given type in the blackboard.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.items
            .get(&TypeId::of::<T>())
            .and_then(|v| (**v).downcast_ref::<T>())
    }

    /// Returns inner hash map.
    pub fn inner(&self) -> &FxHashMap<TypeId, Arc<dyn Any>> {
        &self.items
    }

    /// Returns inner hash map.
    pub fn inner_mut(&mut self) -> &mut FxHashMap<TypeId, Arc<dyn Any>> {
        &mut self.items
    }
}
