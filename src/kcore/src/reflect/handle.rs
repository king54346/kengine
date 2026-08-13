use crate::pool::ErasedHandle;
use crate::reflect::Reflect;
use std::any::TypeId;

pub trait ReflectHandle: Reflect {
    fn reflect_inner_type_id(&self) -> TypeId;
    fn reflect_inner_type_name(&self) -> &'static str;
    fn reflect_is_some(&self) -> bool;
    fn reflect_set_index(&mut self, index: u32);
    fn reflect_index(&self) -> u32;
    fn reflect_set_generation(&mut self, generation: u32);
    fn reflect_generation(&self) -> u32;
    fn reflect_as_erased(&self) -> ErasedHandle;
}