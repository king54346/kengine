use crate::reflect::Reflect;
use crate::variable::{InheritError, VariableFlags};
use std::any::TypeId;

pub trait ReflectInheritableVariable: Reflect {
    /// 尝试从父对象继承值。仅当当前变量**未被标记为已修改**时才会成功。
    fn try_inherit(
        &mut self,
        parent: &dyn ReflectInheritableVariable,
        ignored_types: &[TypeId],
    ) -> Result<Option<Box<dyn Reflect>>, InheritError>;

    /// 重置变量的已修改标志。
    fn reset_modified_flag(&mut self);

    /// 返回当前变量的标志位。
    fn flags(&self) -> VariableFlags;

    fn set_flags(&mut self, flags: VariableFlags);

    /// 返回值是否已被修改。
    fn is_modified(&self) -> bool;

    /// 将值标记为已修改，使其在属性继承时不被覆盖。
    fn mark_modified(&mut self);

    /// 返回包装值的可变引用，但不将变量本身标记为已修改。
    fn inner_value_mut(&mut self) -> &mut dyn Reflect;

    /// 返回包装值的共享引用，但不将变量本身标记为已修改。
    fn inner_value_ref(&self) -> &dyn Reflect;
}