#![allow(clippy::type_complexity)]

use crate::reflect::blank_reflect;
use crate::reflect::Reflect;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

pub trait ReflectHashMap: Reflect {
    fn reflect_insert(
        &mut self,
        key: Box<dyn Reflect>,
        value: Box<dyn Reflect>,
    ) -> Result<Option<Box<dyn Reflect>>, (Box<dyn Reflect>, Box<dyn Reflect>)>;
    fn reflect_len(&self) -> usize;
    fn reflect_get(&self, key: &dyn Reflect) -> Option<&dyn Reflect>;
    fn reflect_get_mut(&mut self, key: &dyn Reflect) -> Option<&mut dyn Reflect>;
    fn reflect_get_nth_value_ref(&self, index: usize) -> Option<&dyn Reflect>;
    fn reflect_get_nth_value_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;
    fn reflect_get_at(&self, index: usize) -> Option<(&dyn Reflect, &dyn Reflect)>;
    fn reflect_get_at_mut(&mut self, index: usize) -> Option<(&dyn Reflect, &mut dyn Reflect)>;
    fn reflect_remove(&mut self, key: &dyn Reflect) -> Option<Box<dyn Reflect>>;
    fn reflect_replace_key(
        &mut self,
        old_key: &dyn Reflect,
        new_key: Box<dyn Reflect>,
    ) -> Result<(), Box<dyn Reflect>>;
}

impl<K, V, S> Reflect for HashMap<K, V, S>
where
    K: Reflect + Eq + Hash + Clone + PartialEq + 'static,
    V: Reflect + Clone + PartialEq,
    S: BuildHasher + Clone + 'static,
{
    // TODO: combine uuids
    blank_reflect!("56412555-c491-4f28-9604-4609b3f47e53");

    fn as_hash_map(&self) -> Option<&dyn ReflectHashMap> {
        Some(self)
    }

    fn as_hash_map_mut(&mut self) -> Option<&mut dyn ReflectHashMap> {
        Some(self)
    }
}

impl<K, V, S> ReflectHashMap for HashMap<K, V, S>
where
    K: Reflect + Eq + Hash + Clone + PartialEq + 'static,
    V: Reflect + Clone + PartialEq,
    S: BuildHasher + Clone + 'static,
{
    fn reflect_insert(
        &mut self,
        key: Box<dyn Reflect>,
        value: Box<dyn Reflect>,
    ) -> Result<Option<Box<dyn Reflect>>, (Box<dyn Reflect>, Box<dyn Reflect>)> {
        match key.downcast::<K>() {
            Ok(key) => match value.downcast::<V>() {
                Ok(value) => {
                    if let Some(previous) = self.insert(*key, *value) {
                        Ok(Some(Box::new(previous)))
                    } else {
                        Ok(None)
                    }
                }
                Err(value) => Err((key, value)),
            },
            Err(key) => Err((key, value)),
        }
    }

    fn reflect_len(&self) -> usize {
        self.len()
    }

    fn reflect_get(&self, key: &dyn Reflect) -> Option<&dyn Reflect> {
        match key.downcast_ref::<K>() {
            Some(key) => match self.get(key) {
                Some(value) => Some(value),
                None => None,
            },
            None => None,
        }
    }

    fn reflect_get_mut(&mut self, key: &dyn Reflect) -> Option<&mut dyn Reflect> {
        match key.downcast_ref::<K>() {
            Some(key) => match self.get_mut(key) {
                Some(value) => Some(value),
                None => None,
            },
            None => None,
        }
    }

    fn reflect_get_nth_value_ref(&self, index: usize) -> Option<&dyn Reflect> {
        self.values().nth(index).map(|v| v as &dyn Reflect)
    }

    fn reflect_get_nth_value_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.values_mut().nth(index).map(|v| v as &mut dyn Reflect)
    }

    fn reflect_get_at(&self, index: usize) -> Option<(&dyn Reflect, &dyn Reflect)> {
        self.iter()
            .nth(index)
            .map(|(k, v)| (k as &dyn Reflect, v as &dyn Reflect))
    }

    fn reflect_get_at_mut(&mut self, index: usize) -> Option<(&dyn Reflect, &mut dyn Reflect)> {
        self.iter_mut()
            .nth(index)
            .map(|(k, v)| (k as &dyn Reflect, v as &mut dyn Reflect))
    }

    fn reflect_remove(&mut self, key: &dyn Reflect) -> Option<Box<dyn Reflect>> {
        match key.downcast_ref::<K>() {
            Some(key) => self
                .remove(key)
                .map(|value| Box::new(value) as Box<dyn Reflect>),
            None => None,
        }
    }

    fn reflect_replace_key(
        &mut self,
        old_key: &dyn Reflect,
        new_key: Box<dyn Reflect>,
    ) -> Result<(), Box<dyn Reflect>> {
        if self.reflect_get(&*new_key).is_some() {
            return Err(new_key);
        }
        if let Some(value) = self.reflect_remove(old_key) {
            if let Err((k, _)) = self.reflect_insert(new_key, value) {
                Err(k)
            } else {
                Ok(())
            }
        } else {
            Err(new_key)
        }
    }
}