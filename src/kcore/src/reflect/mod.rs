//! 运行时反射

mod array;
mod error;
mod field;
mod handle;
mod impls;
mod inherit;
mod macros;
mod map;
mod set;

use crate::sstorage::ImmutableString;

pub use array::*;
pub use error::*;
pub use field::*;
pub use kcore_derive::Reflect;
pub use handle::*;
pub use inherit::*;
pub use macros::*;
pub use map::*;
use std::{
    any::{Any, TypeId},
    fmt::Debug,
    mem::ManuallyDrop,
};
use uuid::Uuid;
pub use crate::reflect::set::ReflectHashSet;

pub mod prelude {
    pub use super::{
        combine_uuids, FieldMetadata, FieldMut, FieldRef, Reflect, ReflectArray, ReflectHashMap,
        ReflectInheritableVariable, ReflectList, ResolvePath, SetFieldByPathError, SetFieldError,
        TypeInfo,
    };
    pub use crate::uuid::{uuid, Uuid};
    pub use std::any::Any;
}

#[inline]
pub fn combine_uuids(a: Uuid, b: Uuid) -> Uuid {
    let mut combined_bytes = a.into_bytes();

    for (src, dest) in b.into_bytes().into_iter().zip(combined_bytes.iter_mut()) {
        *dest ^= src;
    }

    Uuid::from_bytes(combined_bytes)
}

/// 可在运行时查询的类型信息集合。
pub struct TypeInfo {
    /// 定义该类型的文件路径，通常为 `file!()` 宏的返回值。
    pub source_path: &'static str,

    /// 类型的人类可读名称，通常为 `std::any::type_name::<T>()` 的结果。
    pub type_name: &'static str,

    /// 实现此 trait 的类型所在的程序集（crate）名称。
    ///
    /// ## 实现者注意
    ///
    /// 建议使用过程宏（`#[derive(Reflect)]`）来确保此字段包含正确的程序集名称。
    /// 或者使用 `env!("CARGO_PKG_NAME")`。
    pub assembly_name: &'static str,

    /// 类型的文档注释。
    pub doc_comment: &'static str,

    /// 派生自此类型的类型集合。"派生"不是 OOP 意义上的子类，
    /// 而是该类型与其他类型之间的"链接"，通常用于将实际类型与包装类型（如 `Box<dyn SomeTrait>`）关联。
    pub derived_types: &'static [TypeId],

    /// 类型的唯一标识符。
    pub type_uuid: Uuid,
}

/// 运行时反射 trait。
///
/// ## 代码生成
///
/// 可通过 `#[reflect(...)]` 属性宏在类型和字段上进行代码生成。
///
/// ### 类型属性
///
/// - `#[reflect(hide_all)]` — 隐藏所有字段。
/// - `#[reflect(bounds)]` — 添加类型约束，例如 `#[reflect(bounds = "T: Reflect + Clone")]`。
/// - `#[reflect(non_cloneable)]` — 阻止宏生成 `try_clone_box` 实现（适用于不可克隆的类型）。
/// - `#[reflect(non_comparable)]` — 阻止宏生成 `try_compare` 实现（适用于未实现 `PartialEq` 的类型）。
/// - `#[reflect(derived_type = "Type")]` — 将当前类型标记为 `Type` 的子类型。
/// - `#[reflect(type_uuid = "uuid")]` — 为类型分配唯一标识符。
/// - `#[reflect(ignore_generics_type_uuid)]` — 阻止宏将泛型参数的 UUID 与自身 UUID 合并。
///
/// ### 直接访问 vs 间接访问
///
/// 有两种字段访问方式：
/// - **间接访问**（推荐）：通过闭包访问，支持具有内部可变性的类型（Mutex、RefCell 等）。
/// - **直接访问**：直接返回字段引用，性能更好，但不支持内部可变性类型。
///
/// ### 字段属性
///
/// - `#[reflect(hidden)]` — 从反射中隐藏字段。
/// - `#[reflect(setter = "foo")]` — 设置 `set_field` 使用的自定义 setter 方法。
/// - `#[reflect(deref)]` — 通过 `deref` + `deref_mut` 代理字段访问（适用于 newtype 对象）。
/// - `#[reflect(field = "foo")]` — 设置字段只读访问方法。
/// - `#[reflect(field_mut = "foo")]` — 设置字段可变访问方法。
/// - `#[reflect(name = "name")]` — 覆盖字段名称。
/// - `#[reflect(display_name = "name")]` — 设置字段的人类可读名称。
/// - `#[reflect(tag = "tag")]` — 设置字段的任意字符串标签。
/// - `#[reflect(read_only)]` — 标记字段为只读（仅提示，不阻止反射 API 修改）。
/// - `#[reflect(immutable_collection)]` — 仅对动态集合有效，表示大小不可修改。
/// - `#[reflect(min_value = "0.0")]` — 字段最小值（仅限数值字段）。
/// - `#[reflect(max_value = "1.0")]` — 字段最大值（仅限数值字段）。
/// - `#[reflect(step = "0.1")]` — 字段步进值（仅限数值字段）。
/// - `#[reflect(precision = "3")]` — 数值字段的最大小数位数。
///
/// ### 克隆
///
/// 默认情况下，宏假定你的类型实现了 `Clone` 并生成 `try_clone_box` 实现。
/// 若类型无法实现 `Clone`，请添加 `#[reflect(non_cloneable)]`。
///
/// ### PartialEq
///
/// 默认情况下，宏假定你的类型实现了 `PartialEq` 并生成 `try_compare` 实现。
/// 若类型无法实现 `PartialEq`，请添加 `#[reflect(non_comparable)]`。
///
/// ### 类型 UUID
///
/// 每个实现 `Reflect` 的类型必须提供唯一的 UUID，通过 `#[reflect(type_uuid = "...")]` 添加。
/// 请确保 UUID 在整个项目中是唯一的，否则引擎在序列化 trait 对象时可能产生混淆。
///
/// ## 附加 Trait 约束
///
/// `Reflect` 要求类型实现 `Debug` trait，用于将值转换为字符串表示。
pub trait Reflect: Any + Debug {
    /// 返回实现此 trait 的类型信息。
    fn type_info() -> TypeInfo
    where
        Self: Sized;

    /// 返回实现此 trait 的类型信息。
    fn type_info_ref(&self) -> TypeInfo;

    /// 尝试克隆对象并以 boxed trait 对象返回。不可克隆的对象会返回 [`None`]。
    fn try_clone_box(&self) -> Option<Box<dyn Reflect>>;

    /// 尝试将此对象与另一个对象比较。若底层类型实现了 [`PartialEq`]，则返回 `Some(bool)`；
    /// 否则返回 `None`，或当类型标记了 `#[reflect(non_comparable)]` 时也返回 `None`。
    fn try_compare(&self, other: &dyn Reflect) -> Option<bool>;

    /// 使用包含对象所有字段描述的切片引用调用给定闭包。
    fn fields_ref(&self, func: &mut dyn FnMut(&[FieldRef]));

    /// 使用包含对象所有字段描述的切片引用调用给定闭包。
    fn fields_mut(&mut self, func: &mut dyn FnMut(&mut [FieldMut]));

    /// 用指定值替换自身。若调用成功，返回 `Ok(previous_value)`；否则返回 `Err(specified_value)`。
    fn set(&mut self, value: Box<dyn Reflect>) -> Result<Box<dyn Reflect>, Box<dyn Reflect>>;

    /// 尝试获取指定索引处字段的共享引用。以下两种情况返回 [`None`]：
    /// 1) 类型不存在该字段
    /// 2) 类型使用内部可变性。此类类型（Mutex、RefCell 等）通常需要持有锁守卫
    ///    才能访问内部数据。本方法直接返回字段引用，但如果返回锁守卫则需要装箱，
    ///    这在大多数情况下会损害性能。若需要处理具有内部可变性的类型，请改用
    ///    [`Reflect::fields_ref`]。
    fn field_direct_ref(&self, index: usize) -> Option<FieldRef<'_, '_>>;

    /// 尝试获取指定索引处字段的可变引用。以下两种情况返回 [`None`]：
    /// 1) 类型不存在该字段
    /// 2) 类型使用内部可变性。此类类型（Mutex、RefCell 等）通常需要持有锁守卫
    ///    才能访问内部数据。本方法直接返回字段引用，但如果返回锁守卫则需要装箱，
    ///    这在大多数情况下会损害性能。若需要处理具有内部可变性的类型，请改用
    ///    [`Reflect::fields_mut`]。
    fn field_direct_mut(&mut self, index: usize) -> Option<FieldMut<'_, '_>>;

    /// 返回字段总数。
    fn fields_count(&self) -> usize {
        let mut count = 0;
        self.fields_ref(&mut |fields| count = fields.len());
        count
    }

    /// 尝试按名称查找字段并设置其值。以下两种情况会失败：
    /// 1) 字段不存在（或通过 `#[reflect(hidden)]` 被隐藏）。
    /// 2) 指定值的类型与字段类型不匹配。
    #[allow(clippy::type_complexity)]
    fn set_field(
        &mut self,
        field_name: &str,
        value: Box<dyn Reflect>,
        func: &mut dyn FnMut(Result<Box<dyn Reflect>, SetFieldError>),
    ) {
        let mut opt_value = Some(value);
        self.find_field_mut(field_name, &mut move |field| {
            let value = opt_value.take().unwrap();
            match field {
                Some(f) => func(f.set(value).map_err(|value| SetFieldError::InvalidValue {
                    field_type_name: f.type_info_ref().type_name,
                    value,
                })),
                None => func(Err(SetFieldError::NoSuchField {
                    name: field_name.to_string(),
                    value,
                })),
            };
        });
    }

    /// 尝试按名称查找字段，以结果（`Some(field)` 或 `None`）调用指定函数。
    fn find_field(&self, name: &str, func: &mut dyn FnMut(Option<&dyn Reflect>)) {
        self.fields_ref(&mut |fields| {
            func(
                fields
                    .iter()
                    .find(|field| field.name == name)
                    .map(|field| field.value),
            )
        });
    }

    /// 尝试按名称查找字段（可变版本），以结果调用指定函数。
    fn find_field_mut(&mut self, name: &str, func: &mut dyn FnMut(Option<&mut dyn Reflect>)) {
        self.fields_mut(&mut |fields| {
            func(
                fields
                    .iter_mut()
                    .find(|field| field.name == name)
                    .map(|field| &mut *field.value),
            )
        });
    }

    fn as_array(&self) -> Option<&dyn ReflectArray> {
        None
    }

    fn as_array_mut(&mut self) -> Option<&mut dyn ReflectArray> {
        None
    }

    fn as_list(&self) -> Option<&dyn ReflectList> {
        None
    }

    fn as_list_mut(&mut self) -> Option<&mut dyn ReflectList> {
        None
    }

    fn as_inheritable_variable(&self) -> Option<&dyn ReflectInheritableVariable> {
        None
    }

    fn as_inheritable_variable_mut(&mut self) -> Option<&mut dyn ReflectInheritableVariable> {
        None
    }

    fn as_hash_map(&self) -> Option<&dyn ReflectHashMap> {
        None
    }

    fn as_hash_map_mut(&mut self) -> Option<&mut dyn ReflectHashMap> {
        None
    }

    fn as_hash_set(&self) -> Option<&dyn ReflectHashSet> {
        None
    }

    fn as_hash_set_mut(&mut self) -> Option<&mut dyn ReflectHashSet> {
        None
    }

    fn as_handle(&self) -> Option<&dyn ReflectHandle> {
        None
    }

    fn as_handle_mut(&mut self) -> Option<&mut dyn ReflectHandle> {
        None
    }
}

pub fn make_hash_map_key(key: &dyn Reflect) -> String {
    // TODO: Here we just using `Debug` impl to obtain string representation for keys. This is
    // fine for most cases in the engine.
    let mut key_str = format!("{key:?}");

    let is_key_string =
        key.downcast_ref::<String>().is_some() || key.downcast_ref::<ImmutableString>().is_some();

    if is_key_string {
        // Strip quotes at the beginning and the end, because Debug impl for String adds
        // quotes at the beginning and the end, but we want raw value.
        // TODO: This is unreliable mechanism.
        key_str.remove(0);
        key_str.pop();
    }

    key_str
}

/// Type-erased API
impl dyn Reflect {
    pub fn downcast<T: Reflect>(self: Box<dyn Reflect>) -> Result<Box<T>, Box<dyn Reflect>> {
        if self.is::<T>() {
            Ok((self as Box<dyn Any>).downcast().unwrap())
        } else {
            Err(self)
        }
    }

    pub fn take<T: Reflect>(self: Box<dyn Reflect>) -> Result<T, Box<dyn Reflect>> {
        self.downcast::<T>().map(|value| *value)
    }

    #[inline]
    pub fn is<T: Reflect>(&self) -> bool {
        self.type_id() == TypeId::of::<T>()
    }

    #[inline]
    pub fn downcast_ref<T: Reflect>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref::<T>()
    }

    #[inline]
    pub fn downcast_mut<T: Reflect>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut::<T>()
    }

    /// 尝试查找第一个给定类型的字段。本方法内部会使用 [`Reflect::field_direct_ref`]，
    /// 因此会受到其所有限制。
    #[inline]
    pub fn first_field_ref<T: Reflect>(&self) -> Option<&T> {
        let count = self.fields_count();

        for i in 0..count {
            if let Some(field) = self.field_direct_ref(i) {
                if let Some(typed_field) = (field.value as &dyn Any).downcast_ref::<T>() {
                    return Some(typed_field);
                }
            }
        }

        None
    }

    /// 尝试查找第一个给定类型的字段。本方法内部会使用 [`Reflect::field_direct_ref`]，
    /// 因此会受到其所有限制。
    #[inline]
    pub fn first_field_mut<T: Reflect>(&mut self) -> Option<&mut T> {
        let count = self.fields_count();

        for i in 0..count {
            // SAFETY: Current implementation of borrow checker is just dumb. When a reborrow of self
            // happens in every iteration of the loop, it assigns a new lifetime to the new reference.
            // This way the returned reference has a different lifetime than in the method definition.
            // The following unsafe block reborrows self with the correct lifetime, while the initial
            // reference is not used so this is absolutely safe.
            let this = unsafe { &mut *(self as *mut Self) };
            if let Some(field) = this.field_direct_mut(i) {
                if let Some(typed_field) = (field.value as &mut dyn Any).downcast_mut::<T>() {
                    return Some(typed_field);
                }
            }
        }

        None
    }

    /// 尝试将自身向下转型为指定类型；若失败，则尝试查找该类型的字段。
    pub fn self_or_field_ref<T: Reflect>(&self) -> Option<&T> {
        if let Some(value) = (self as &dyn Any).downcast_ref::<T>() {
            Some(value)
        } else {
            self.first_field_ref()
        }
    }

    /// 尝试将自身向下转型为指定类型；若失败，则尝试查找该类型的字段。
    pub fn self_or_field_mut<T: Reflect>(&mut self) -> Option<&mut T> {
        // SAFETY: See the comment in `first_field_mut_of_type` method.
        let this = unsafe { &mut *(self as *mut Self) };
        if let Some(value) = (self as &mut dyn Any).downcast_mut::<T>() {
            Some(value)
        } else {
            this.first_field_mut()
        }
    }

    /// 按路径设置给定对象中的字段。此方法始终使用 [`Reflect::set_field`]，
    /// 也就是说它总会调用自定义属性 setter。
    #[inline]
    pub fn set_field_by_path<'p>(
        &mut self,
        path: &'p str,
        value: Box<dyn Reflect>,
        func: &mut dyn FnMut(Result<Box<dyn Reflect>, SetFieldByPathError<'p>>),
    ) {
        if let Some(separator_position) = path.rfind('.') {
            let mut opt_value = Some(value);
            let parent_path = &path[..separator_position];
            let field = &path[(separator_position + 1)..];
            self.resolve_path_mut(parent_path, &mut |result| match result {
                Err(reason) => {
                    func(Err(SetFieldByPathError::InvalidPath {
                        reason,
                        value: opt_value.take().unwrap(),
                    }));
                }
                Ok(property) => {
                    property.set_field(field, opt_value.take().unwrap(), &mut |result| match result
                    {
                        Ok(value) => func(Ok(value)),
                        Err(err) => func(Err(SetFieldByPathError::SetFieldError(err))),
                    })
                }
            });
        } else {
            self.set_field(path, value, &mut |result| match result {
                Ok(value) => func(Ok(value)),
                Err(err) => func(Err(SetFieldByPathError::SetFieldError(err))),
            });
        }
    }

    pub fn enumerate_fields_recursively<F>(&self, func: &mut F, ignored_types: &[TypeId])
    where
        F: FnMut(&str, Option<&FieldRef>, &dyn Reflect),
    {
        self.enumerate_fields_recursively_internal("", None, func, ignored_types)
    }

    fn enumerate_fields_recursively_internal<F>(
        &self,
        path: &str,
        field_info: Option<&FieldRef>,
        func: &mut F,
        ignored_types: &[TypeId],
    ) where
        F: FnMut(&str, Option<&FieldRef>, &dyn Reflect),
    {
        if ignored_types.contains(&self.type_id()) {
            return;
        }

        let mut done = false;

        if let Some(variable) = self.as_inheritable_variable() {
            // Inner variable might also contain inheritable variables, so continue iterating.
            variable
                .inner_value_ref()
                .enumerate_fields_recursively_internal(path, field_info, func, ignored_types);

            done = true;
        }

        if done {
            return;
        }

        func(path, field_info, self);

        if let Some(array) = self.as_array() {
            for i in 0..array.reflect_len() {
                if let Some(item) = array.reflect_index(i) {
                    let item_path = format!("{path}[{i}]");

                    item.enumerate_fields_recursively_internal(
                        &item_path,
                        field_info,
                        func,
                        ignored_types,
                    );
                }
            }

            done = true;
        }

        if done {
            return;
        }

        if let Some(hash_map) = self.as_hash_map() {
            for i in 0..hash_map.reflect_len() {
                if let Some((key, value)) = hash_map.reflect_get_at(i) {
                    let key_str = make_hash_map_key(key);

                    let item_path = format!("{path}[{key_str}]");

                    value.enumerate_fields_recursively_internal(
                        &item_path,
                        field_info,
                        func,
                        ignored_types,
                    );
                }
            }

            done = true;
        }

        if done {
            return;
        }

        self.fields_ref(&mut |fields| {
            for field in fields {
                let compound_path;
                let field_path = if path.is_empty() {
                    field.metadata.name
                } else {
                    compound_path = format!("{}.{}", path, field.metadata.name);
                    &compound_path
                };

                field.value.enumerate_fields_recursively_internal(
                    field_path,
                    Some(field),
                    func,
                    ignored_types,
                );
            }
        })
    }

    pub fn apply_recursively<F>(&self, func: &mut F, ignored_types: &[TypeId])
    where
        F: FnMut(&dyn Reflect),
    {
        if ignored_types.contains(&(*self).type_id()) {
            return;
        }

        func(self);

        let mut done = false;

        if let Some(variable) = self.as_inheritable_variable() {
            // Inner variable might also contain inheritable variables, so continue iterating.
            variable
                .inner_value_ref()
                .apply_recursively(func, ignored_types);

            done = true;
        }

        if done {
            return;
        }

        if let Some(array) = self.as_array() {
            for i in 0..array.reflect_len() {
                if let Some(item) = array.reflect_index(i) {
                    item.apply_recursively(func, ignored_types);
                }
            }

            done = true;
        }

        if done {
            return;
        }

        if let Some(hash_map) = self.as_hash_map() {
            for i in 0..hash_map.reflect_len() {
                if let Some(item) = hash_map.reflect_get_nth_value_ref(i) {
                    item.apply_recursively(func, ignored_types);
                }
            }

            done = true;
        }

        if done {
            return;
        }

        self.fields_ref(&mut |fields| {
            for field_info_ref in fields {
                field_info_ref.value.apply_recursively(func, ignored_types);
            }
        })
    }

    pub fn apply_recursively_mut<F>(&mut self, func: &mut F, ignored_types: &[TypeId])
    where
        F: FnMut(&mut dyn Reflect),
    {
        if ignored_types.contains(&(*self).type_id()) {
            return;
        }

        func(self);

        let mut done = false;

        if let Some(variable) = self.as_inheritable_variable_mut() {
            // Inner variable might also contain inheritable variables, so continue iterating.
            variable
                .inner_value_mut()
                .apply_recursively_mut(func, ignored_types);

            done = true;
        }

        if done {
            return;
        }

        if let Some(array) = self.as_array_mut() {
            for i in 0..array.reflect_len() {
                if let Some(item) = array.reflect_index_mut(i) {
                    item.apply_recursively_mut(func, ignored_types);
                }
            }

            done = true;
        }

        if done {
            return;
        }

        if let Some(hash_map) = self.as_hash_map_mut() {
            for i in 0..hash_map.reflect_len() {
                if let Some(item) = hash_map.reflect_get_nth_value_mut(i) {
                    item.apply_recursively_mut(func, ignored_types);
                }
            }

            done = true;
        }

        if done {
            return;
        }

        self.fields_mut(&mut |fields| {
            for field_info_mut in fields {
                (*field_info_mut.value).apply_recursively_mut(func, ignored_types);
            }
        })
    }
}

pub trait ResolvePath {
    fn resolve_path<'p>(
        &self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&dyn Reflect, ReflectPathError<'p>>),
    );

    fn resolve_path_mut<'p>(
        &mut self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&mut dyn Reflect, ReflectPathError<'p>>),
    );

    fn get_resolve_path<'p, T: Reflect>(
        &self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&T, ReflectPathError<'p>>),
    ) {
        self.resolve_path(path, &mut |resolve_result| {
            match resolve_result {
                Ok(value) => {
                    match value.downcast_ref::<T>() {
                        Some(value) => {
                            func(Ok(value));
                        }
                        None => {
                            func(Err(ReflectPathError::InvalidDowncast));
                        }
                    };
                }
                Err(err) => {
                    func(Err(err));
                }
            };
        })
    }

    fn get_resolve_path_mut<'p, T: Reflect>(
        &mut self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&mut T, ReflectPathError<'p>>),
    ) {
        self.resolve_path_mut(path, &mut |result| match result {
            Ok(value) => match value.downcast_mut() {
                Some(value) => func(Ok(value)),
                None => func(Err(ReflectPathError::InvalidDowncast)),
            },
            Err(err) => func(Err(err)),
        })
    }
}

impl<T: Reflect> ResolvePath for T {
    fn resolve_path<'p>(
        &self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&dyn Reflect, ReflectPathError<'p>>),
    ) {
        (self as &dyn Reflect).resolve_path(path, func)
    }

    fn resolve_path_mut<'p>(
        &mut self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&mut dyn Reflect, ReflectPathError<'p>>),
    ) {
        (self as &mut dyn Reflect).resolve_path_mut(path, func)
    }
}

/// Splits property path into individual components.
pub fn path_to_components(path: &str) -> Vec<Component<'_>> {
    let mut components = Vec::new();
    let mut current_path = path;
    while let Ok((component, sub_path)) = Component::next(current_path) {
        if let Component::Field(field) = component {
            if field.is_empty() {
                break;
            }
        }
        current_path = sub_path;
        components.push(component);
    }
    components
}

/// Helper methods over [`Reflect`] types
pub trait GetField {
    fn get_field<T: 'static>(&self, name: &str, func: &mut dyn FnMut(Option<&T>));

    fn get_field_mut<T: 'static>(&mut self, _name: &str, func: &mut dyn FnMut(Option<&mut T>));
}

impl<R: Reflect> GetField for R {
    fn get_field<T: 'static>(&self, name: &str, func: &mut dyn FnMut(Option<&T>)) {
        self.find_field(name, &mut |opt_field| match opt_field {
            None => func(None),
            Some(field) => func((field as &dyn Any).downcast_ref()),
        })
    }

    fn get_field_mut<T: 'static>(&mut self, name: &str, func: &mut dyn FnMut(Option<&mut T>)) {
        self.find_field_mut(name, &mut |opt_field| match opt_field {
            None => func(None),
            Some(field) => func((field as &mut dyn Any).downcast_mut()),
        })
    }
}

// --------------------------------------------------------------------------------
// impl dyn Trait
// --------------------------------------------------------------------------------

// SAFETY: String usage is safe in immutable contexts only. Calling `ManuallyDrop::drop`
// (running strings destructor) on the returned value will cause crash!
unsafe fn make_fake_string_from_slice(string: &str) -> ManuallyDrop<String> { unsafe {
    ManuallyDrop::new(String::from_utf8_unchecked(Vec::from_raw_parts(
        string.as_bytes().as_ptr() as *mut _,
        string.len(),
        string.len(),
    )))
}}

fn try_fetch_by_str_path_ref<'a>(
    hash_map: &'a dyn ReflectHashMap,
    path: &str,
) -> Option<&'a dyn Reflect> {
    // Create fake string here first, this is needed to avoid memory allocations.
    // SAFETY: We won't drop the fake string or mutate it.
    let fake_string_key = unsafe { make_fake_string_from_slice(path) };

    match hash_map.reflect_get(&*fake_string_key) {
        Some(value) => Some(value),
        None => hash_map.reflect_get(&ImmutableString::new(path) as &dyn Reflect),
    }
}

fn try_fetch_by_str_path_mut<'a>(
    hash_map: &'a mut dyn ReflectHashMap,
    path: &str,
) -> Option<&'a mut dyn Reflect> {
    // Create fake string here first, this is needed to avoid memory allocations..
    // SAFETY: We won't drop the fake string or mutate it.
    let fake_string_key = unsafe { make_fake_string_from_slice(path) };

    let hash_map2 = unsafe { &mut *(hash_map as *mut dyn ReflectHashMap) };

    match hash_map.reflect_get_mut(&*fake_string_key) {
        Some(value) => Some(value),
        None => hash_map2.reflect_get_mut(&ImmutableString::new(path) as &dyn Reflect),
    }
}

/// Simple path parser / reflect path component
pub enum Component<'p> {
    Field(&'p str),
    Index(&'p str),
}

impl<'p> Component<'p> {
    fn next(mut path: &'p str) -> Result<(Self, &'p str), ReflectPathError<'p>> {
        // Discard the first comma:
        if path.bytes().next() == Some(b'.') {
            path = &path[1..];
        }

        let mut bytes = path.bytes().enumerate();
        while let Some((i, b)) = bytes.next() {
            if b == b'.' {
                let (l, r) = path.split_at(i);
                return Ok((Self::Field(l), &r[1..]));
            }

            if b == b'[' {
                if i != 0 {
                    // delimit the field access
                    let (l, r) = path.split_at(i);
                    return Ok((Self::Field(l), r));
                }

                // find ']'
                if let Some((end, _)) = bytes.find(|(_, b)| *b == b']') {
                    let l = &path[1..end];
                    let r = &path[end + 1..];
                    return Ok((Self::Index(l), r));
                } else {
                    return Err(ReflectPathError::UnclosedBrackets { s: path });
                }
            }
        }

        // NOTE: the `path` can be empty
        Ok((Self::Field(path), ""))
    }

    fn resolve(
        &self,
        reflect: &dyn Reflect,
        func: &mut dyn FnMut(Result<&dyn Reflect, ReflectPathError<'p>>),
    ) {
        match self {
            Self::Field(path) => reflect.find_field(path, &mut |field| {
                func(field.ok_or(ReflectPathError::UnknownField { s: path }))
            }),
            Self::Index(path) => match reflect.as_array() {
                Some(array) => match path.parse::<usize>() {
                    Ok(index) => match array.reflect_index(index) {
                        None => func(Err(ReflectPathError::NoItemForIndex { s: path })),
                        Some(value) => func(Ok(value)),
                    },
                    Err(_) => func(Err(ReflectPathError::InvalidIndexSyntax { s: path })),
                },
                None => match reflect.as_hash_map() {
                    Some(hash_map) => func(
                        try_fetch_by_str_path_ref(hash_map, path)
                            .ok_or(ReflectPathError::NoItemForIndex { s: path }),
                    ),
                    None => func(Err(ReflectPathError::NotAnArray)),
                },
            },
        }
    }

    fn resolve_mut(
        &self,
        reflect: &mut dyn Reflect,
        func: &mut dyn FnMut(Result<&mut dyn Reflect, ReflectPathError<'p>>),
    ) {
        match self {
            Self::Field(path) => reflect.find_field_mut(path, &mut |field| {
                func(field.ok_or(ReflectPathError::UnknownField { s: path }))
            }),
            Self::Index(path) => {
                let mut succeeded = true;
                match reflect.as_array_mut() {
                    Some(list) => match path.parse::<usize>() {
                        Ok(index) => match list.reflect_index_mut(index) {
                            None => func(Err(ReflectPathError::NoItemForIndex { s: path })),
                            Some(value) => func(Ok(value)),
                        },
                        Err(_) => func(Err(ReflectPathError::InvalidIndexSyntax { s: path })),
                    },
                    None => succeeded = false,
                }

                if !succeeded {
                    match reflect.as_hash_map_mut() {
                        Some(hash_map) => func(
                            try_fetch_by_str_path_mut(hash_map, path)
                                .ok_or(ReflectPathError::NoItemForIndex { s: path }),
                        ),
                        None => func(Err(ReflectPathError::NotAnArray)),
                    }
                }
            }
        }
    }
}

impl ResolvePath for dyn Reflect {
    fn resolve_path<'p>(
        &self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&dyn Reflect, ReflectPathError<'p>>),
    ) {
        match Component::next(path) {
            Ok((component, r)) => component.resolve(self, &mut |result| match result {
                Ok(child) => {
                    if r.is_empty() {
                        func(Ok(child))
                    } else {
                        child.resolve_path(r, func)
                    }
                }
                Err(err) => func(Err(err)),
            }),
            Err(err) => func(Err(err)),
        }
    }

    fn resolve_path_mut<'p>(
        &mut self,
        path: &'p str,
        func: &mut dyn FnMut(Result<&mut dyn Reflect, ReflectPathError<'p>>),
    ) {
        match Component::next(path) {
            Ok((component, r)) => component.resolve_mut(self, &mut |result| match result {
                Ok(child) => {
                    if r.is_empty() {
                        func(Ok(child))
                    } else {
                        child.resolve_path_mut(r, func)
                    }
                }
                Err(err) => func(Err(err)),
            }),
            Err(err) => func(Err(err)),
        }
    }
}

pub fn is_path_to_array_element(path: &str) -> bool {
    path.ends_with(']')
}

// Make it a trait?
impl dyn ReflectList {
    pub fn get_reflect_index<T: Reflect>(&self, index: usize, func: &mut dyn FnMut(Option<&T>)) {
        if let Some(reflect) = self.reflect_index(index) {
            func(reflect.downcast_ref())
        } else {
            func(None)
        }
    }

    pub fn get_reflect_index_mut<T: Reflect>(
        &mut self,
        index: usize,
        func: &mut dyn FnMut(Option<&mut T>),
    ) {
        if let Some(reflect) = self.reflect_index_mut(index) {
            func(reflect.downcast_mut())
        } else {
            func(None)
        }
    }
}

#[cfg(test)]
mod test {
    use super::prelude::*;
    use crate::variable::InheritableVariable;
    use std::any::TypeId;
    use std::collections::HashMap;

    #[derive(Reflect, Clone, Default, PartialEq, Debug)]
    #[reflect(type_uuid = "407bcd52-9603-4436-b16c-638c8f6ea97e")]
    enum Enum {
        #[default]
        Empty,
        Stuff {
            field: u32,
        },
    }

    #[derive(Reflect, Clone, Default, Debug, PartialEq)]
    #[reflect(type_uuid = "97718a85-3901-407e-9347-b684c0047743")]
    struct Foo {
        enum_field: InheritableVariable<Enum>,
        bar: Bar,
        baz: f32,
        collection: Vec<Item>,
        hash_map: HashMap<String, Item>,
    }

    #[derive(Reflect, Clone, Default, Debug, PartialEq)]
    #[reflect(type_uuid = "9465ea72-dd27-43f3-8ccf-b634e4b2887f")]
    struct Item {
        payload: u32,
    }

    #[derive(Reflect, Clone, Default, Debug, PartialEq)]
    #[reflect(type_uuid = "8fb478ac-e4c4-4762-a43a-451147e6e509")]
    struct Bar {
        stuff: String,
    }

    #[test]
    fn enumerate_fields_recursively() {
        let baz = 123.321;

        let foo = Foo {
            enum_field: Enum::Stuff { field: 123 }.into(),
            bar: Default::default(),
            baz,
            collection: vec![Item::default()],
            hash_map: [("Foobar".to_string(), Item::default())].into(),
        };

        let mut names = Vec::new();
        (&foo as &dyn Reflect).enumerate_fields_recursively(
            &mut |path, _, _| {
                names.push(path.to_string());
            },
            &[],
        );

        foo.resolve_path("enum_field.Content.Stuff@field", &mut |result| {
            let enum_field = result.expect("the field must exist!");
            assert_eq!(
                *enum_field
                    .downcast_ref::<u32>()
                    .expect("the type must be u32"),
                123
            );
        });

        assert_eq!(names[0], "");
        assert_eq!(names[1], "enum_field");
        assert_eq!(names[2], "enum_field.Stuff@field");
        assert_eq!(names[3], "bar");
        assert_eq!(names[4], "bar.stuff");
        assert_eq!(names[5], "baz");
        assert_eq!(names[6], "collection");
        assert_eq!(names[7], "collection[0]");
        assert_eq!(names[8], "collection[0].payload");
        assert_eq!(names[9], "hash_map");
        assert_eq!(names[10], "hash_map[Foobar]");
        assert_eq!(names[11], "hash_map[Foobar].payload");

        assert_eq!(foo.fields_count(), 5);

        assert_eq!(
            (&foo as &dyn Reflect).first_field_ref::<f32>().unwrap(),
            &baz
        );
    }

    #[derive(Reflect, Clone, PartialEq, Debug)]
    #[reflect(
        derived_type = "Derived",
        type_uuid = "8c093dc1-fd18-45ff-97bc-8d2364b7ed30"
    )]
    struct Base;

    #[allow(dead_code)]
    struct Derived(Box<Base>);

    #[test]
    fn test_derived() {
        let base = Base;
        assert_eq!(
            base.type_info_ref().derived_types,
            &[TypeId::of::<Derived>()]
        )
    }
}