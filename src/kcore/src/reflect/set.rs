use crate::{blank_reflect, reflect::Reflect};
use std::{
    collections::HashSet,
    hash::{BuildHasher, Hash},
};

pub trait ReflectHashSet: Reflect {
    fn reflect_insert(&mut self, value: Box<dyn Reflect>) -> bool;
    fn reflect_len(&self) -> usize;
    fn reflect_contains(&self, value: &dyn Reflect) -> bool;
    fn reflect_get_at(&self, index: usize) -> Option<&dyn Reflect>;
    fn reflect_remove(&mut self, value: &dyn Reflect) -> bool;
}

impl<V, S> Reflect for HashSet<V, S>
where
    V: Reflect + Hash + Clone + Eq + PartialEq,
    S: BuildHasher + Clone + 'static,
{
    // TODO: combine uuids
    blank_reflect!("9f703c32-0816-4a84-abf6-366870a42cf2");

    fn as_hash_set(&self) -> Option<&dyn ReflectHashSet> {
        Some(self)
    }

    fn as_hash_set_mut(&mut self) -> Option<&mut dyn ReflectHashSet> {
        Some(self)
    }
}

impl<V, S> ReflectHashSet for HashSet<V, S>
where
    V: Reflect + Hash + Clone + Eq + PartialEq,
    S: BuildHasher + Clone + 'static,
{
    fn reflect_insert(&mut self, value: Box<dyn Reflect>) -> bool {
        match value.downcast::<V>() {
            Ok(value) => self.insert(*value),
            Err(_) => false,
        }
    }

    fn reflect_len(&self) -> usize {
        self.len()
    }

    fn reflect_contains(&self, value: &dyn Reflect) -> bool {
        match value.downcast_ref::<V>() {
            Some(value) => self.contains(value),
            None => false,
        }
    }

    fn reflect_get_at(&self, index: usize) -> Option<&dyn Reflect> {
        self.iter().nth(index).map(|v| v as &dyn Reflect)
    }

    fn reflect_remove(&mut self, value: &dyn Reflect) -> bool {
        match value.downcast_ref::<V>() {
            Some(value) => self.remove(value),
            None => false,
        }
    }
}