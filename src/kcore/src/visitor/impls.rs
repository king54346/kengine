use crate::{
    replace_slashes,
    visitor::{
        BinaryBlob, Field, Visit, VisitResult, Visitor, error::VisitError, field::FieldKind,
    },
};
use nalgebra::{Matrix2, Matrix3, Matrix4, UnitComplex, UnitQuaternion, Vector2, Vector3, Vector4};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    hash::{BuildHasher, Hash},
    ops::{DerefMut, Range},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use uuid::Uuid;

macro_rules! impl_visit_as_field {
    ($type_name:ty, $($kind:tt)*) => {
        impl Visit for $type_name {
            fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
                if visitor.reading {
                    if let Some(field) = visitor.find_field(name) {
                        match field.kind {
                            $($kind)*(data) => {
                                *self = data.clone();
                                Ok(())
                            },
                            _ => Err(VisitError::FieldTypeDoesNotMatch {
                                expected: stringify!($($kind)*),
                                actual: format!("{:?}", field.kind),
                            }),
                        }
                    } else {
                        Err(VisitError::field_does_not_exist(name, visitor))
                    }
                } else if visitor.find_field(name).is_some() {
                    Err(VisitError::FieldAlreadyExists(name.to_owned()))
                } else {
                    let node = visitor.current_node();
                    node.fields.push(Field::new(name, $($kind)*(self.clone())));
                    Ok(())
                }
            }
        }
    };
}

impl_visit_as_field!(u64, FieldKind::U64);
impl_visit_as_field!(i64, FieldKind::I64);
impl_visit_as_field!(u32, FieldKind::U32);
impl_visit_as_field!(i32, FieldKind::I32);
impl_visit_as_field!(u16, FieldKind::U16);
impl_visit_as_field!(i16, FieldKind::I16);
impl_visit_as_field!(u8, FieldKind::U8);
impl_visit_as_field!(i8, FieldKind::I8);
impl_visit_as_field!(f32, FieldKind::F32);
impl_visit_as_field!(f64, FieldKind::F64);
impl_visit_as_field!(UnitQuaternion<f32>, FieldKind::UnitQuaternion);
impl_visit_as_field!(Matrix4<f32>, FieldKind::Matrix4);
impl_visit_as_field!(bool, FieldKind::Bool);
impl_visit_as_field!(Matrix3<f32>, FieldKind::Matrix3);
impl_visit_as_field!(Uuid, FieldKind::Uuid);
impl_visit_as_field!(UnitComplex<f32>, FieldKind::UnitComplex);
impl_visit_as_field!(Matrix2<f32>, FieldKind::Matrix2);

impl_visit_as_field!(Vector2<f32>, FieldKind::Vector2F32);
impl_visit_as_field!(Vector3<f32>, FieldKind::Vector3F32);
impl_visit_as_field!(Vector4<f32>, FieldKind::Vector4F32);

impl_visit_as_field!(Vector2<f64>, FieldKind::Vector2F64);
impl_visit_as_field!(Vector3<f64>, FieldKind::Vector3F64);
impl_visit_as_field!(Vector4<f64>, FieldKind::Vector4F64);

impl_visit_as_field!(Vector2<i8>, FieldKind::Vector2I8);
impl_visit_as_field!(Vector3<i8>, FieldKind::Vector3I8);
impl_visit_as_field!(Vector4<i8>, FieldKind::Vector4I8);

impl_visit_as_field!(Vector2<u8>, FieldKind::Vector2U8);
impl_visit_as_field!(Vector3<u8>, FieldKind::Vector3U8);
impl_visit_as_field!(Vector4<u8>, FieldKind::Vector4U8);

impl_visit_as_field!(Vector2<i16>, FieldKind::Vector2I16);
impl_visit_as_field!(Vector3<i16>, FieldKind::Vector3I16);
impl_visit_as_field!(Vector4<i16>, FieldKind::Vector4I16);

impl_visit_as_field!(Vector2<u16>, FieldKind::Vector2U16);
impl_visit_as_field!(Vector3<u16>, FieldKind::Vector3U16);
impl_visit_as_field!(Vector4<u16>, FieldKind::Vector4U16);

impl_visit_as_field!(Vector2<i32>, FieldKind::Vector2I32);
impl_visit_as_field!(Vector3<i32>, FieldKind::Vector3I32);
impl_visit_as_field!(Vector4<i32>, FieldKind::Vector4I32);

impl_visit_as_field!(Vector2<u32>, FieldKind::Vector2U32);
impl_visit_as_field!(Vector3<u32>, FieldKind::Vector3U32);
impl_visit_as_field!(Vector4<u32>, FieldKind::Vector4U32);

impl_visit_as_field!(Vector2<i64>, FieldKind::Vector2I64);
impl_visit_as_field!(Vector3<i64>, FieldKind::Vector3I64);
impl_visit_as_field!(Vector4<i64>, FieldKind::Vector4I64);

impl_visit_as_field!(Vector2<u64>, FieldKind::Vector2U64);
impl_visit_as_field!(Vector3<u64>, FieldKind::Vector3U64);
impl_visit_as_field!(Vector4<u64>, FieldKind::Vector4U64);

impl<T> Visit for RefCell<T>
where
    T: Visit + 'static,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        if let Ok(mut data) = self.try_borrow_mut() {
            data.visit(name, visitor)
        } else {
            Err(VisitError::RefCellAlreadyMutableBorrowed)
        }
    }
}

impl<T> Visit for Vec<T>
where
    T: Default + Visit + 'static,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut len = self.len() as u32;
        len.visit("Length", &mut region)?;

        fn make_name(i: usize) -> String {
            format!("Item{i}")
        }

        if region.reading {
            self.clear();
            for index in 0..len as usize {
                let mut object = T::default();
                object.visit(&make_name(index), &mut region)?;
                self.push(object);
            }
        } else {
            for (index, item) in self.iter_mut().enumerate() {
                item.visit(&make_name(index), &mut region)?;
            }
        }

        Ok(())
    }
}

impl<T> Visit for Option<T>
where
    T: Default + Visit + 'static,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut is_some = u8::from(self.is_some());
        is_some.visit("IsSome", &mut region)?;

        if is_some != 0 {
            if region.reading {
                let mut value = T::default();
                value.visit("Data", &mut region)?;
                *self = Some(value);
            } else {
                self.as_mut().unwrap().visit("Data", &mut region)?;
            }
        } else if region.reading {
            *self = None;
        }

        Ok(())
    }
}

fn read_old_string_format(name: &str, visitor: &mut Visitor) -> Result<String, VisitError> {
    let mut region = visitor.enter_region(name)?;

    let mut len = 0u32;
    len.visit("Length", &mut region)?;

    let mut data = Vec::new();
    let mut proxy = BinaryBlob { vec: &mut data };
    proxy.visit("Data", &mut region)?;

    Ok(String::from_utf8(data)?)
}

impl Visit for String {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        if visitor.reading {
            if let Ok(old_string) = read_old_string_format(name, visitor) {
                *self = old_string;
                Ok(())
            } else if let Some(field) = visitor.find_field(name) {
                match field.kind {
                    FieldKind::String(ref string) => {
                        *self = string.clone();
                        Ok(())
                    }
                    _ => Err(VisitError::FieldTypeDoesNotMatch {
                        expected: stringify!(FieldKind::String),
                        actual: format!("{:?}", field.kind),
                    }),
                }
            } else {
                Err(VisitError::field_does_not_exist(name, visitor))
            }
        } else if visitor.find_field(name).is_some() {
            Err(VisitError::FieldAlreadyExists(name.to_owned()))
        } else {
            let node = visitor.current_node();
            node.fields
                .push(Field::new(name, FieldKind::String(self.clone())));
            Ok(())
        }
    }
}

impl Visit for PathBuf {
    #[allow(clippy::needless_borrows_for_generic_args)] // Fix your shit first before releasing.
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        // We have to replace Windows back slashes \ to forward / to make paths portable
        // across all OSes.
        let portable_path = replace_slashes(&self);

        let bytes = if let Some(path_str) = portable_path.as_os_str().to_str() {
            path_str.as_bytes()
        } else {
            return Err(VisitError::InvalidName);
        };

        let mut len = bytes.len() as u32;
        len.visit("Length", &mut region)?;

        let mut data = if region.reading {
            Vec::new()
        } else {
            Vec::from(bytes)
        };

        let mut proxy = BinaryBlob { vec: &mut data };
        proxy.visit("Data", &mut region)?;

        if region.reading {
            *self = PathBuf::from(String::from_utf8(data)?);
        }

        Ok(())
    }
}

impl<T> Visit for Cell<T>
where
    T: Copy + Clone + Visit + 'static,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut value = self.get();
        value.visit(name, visitor)?;
        if visitor.is_reading() {
            self.set(value);
        }
        Ok(())
    }
}

impl<T> Visit for Rc<T>
where
    T: Visit + 'static,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        if region.reading {
            let mut id = 0u64;
            id.visit("Id", &mut region)?;
            if id == 0 {
                return Err(VisitError::UnexpectedRcNullIndex);
            }
            if let Some(ptr) = region.rc_map.get(&id) {
                if let Ok(res) = Rc::downcast::<T>(ptr.clone()) {
                    *self = res;
                } else {
                    return Err(VisitError::TypeMismatch {
                        expected: std::any::type_name::<T>(),
                        actual: region.type_name_map.get(&id).unwrap_or(&"MISSING"),
                    });
                }
            } else {
                region.type_name_map.insert(id, std::any::type_name::<T>());
                // 先在独占状态下把内容填好，再放进共享表。放进表里意味着别处可以
                // clone 到同一份数据，那时再取 `&mut` 就是别名访问（未定义行为）。
                let result = match rc_to_mut(self) {
                    Some(data) => data.visit("RcData", &mut region),
                    None => Err(VisitError::SharedReferenceNotUnique(
                        std::any::type_name::<T>(),
                    )),
                };
                // 有些场景下 visit 只是试探性的：失败后还会换条路继续走，因此必须把
                // 刚才登记的条目撤掉，让 Visitor 回到干净状态。
                if result.is_err() {
                    region.type_name_map.remove(&id);
                    return result;
                }
                region.rc_map.insert(id, self.clone());
            }
        } else {
            let (mut id, serialize_data) = region.rc_id(self);
            id.visit("Id", &mut region)?;
            if serialize_data {
                match rc_to_mut(self) {
                    Some(data) => data.visit("RcData", &mut region)?,
                    None => {
                        return Err(VisitError::SharedReferenceNotUnique(
                            std::any::type_name::<T>(),
                        ))
                    }
                }
            }
        }

        Ok(())
    }
}

impl<T> Visit for Mutex<T>
where
    T: Visit + Send,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        self.get_mut()?.visit(name, visitor)
    }
}

impl<T> Visit for parking_lot::Mutex<T>
where
    T: Visit + Send,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        self.get_mut().visit(name, visitor)
    }
}

impl<T> Visit for Box<T>
where
    T: Visit,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        self.deref_mut().visit(name, visitor)
    }
}

impl<T> Visit for RwLock<T>
where
    T: Visit + Send,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        self.write()?.visit(name, visitor)
    }
}

/// 安全地取得 `Rc` 内容的独占可变借用。
///
/// `Visit` 的签名是 `&mut self`，读写都要可变借用其内容。一旦 `Rc` 被多处共享
/// （强引用计数 > 1），就无法在不违反别名规则的前提下取得 `&mut T`——旧实现用
/// 裸指针硬转（未定义行为），这里改为显式返回 `None`，由调用方报错。
fn rc_to_mut<T>(rc: &mut Rc<T>) -> Option<&mut T> {
    Rc::get_mut(rc)
}

/// 安全地取得 `Arc` 内容的独占可变借用（理由见 [`rc_to_mut`]）。
fn arc_to_mut<T>(arc: &mut Arc<T>) -> Option<&mut T> {
    Arc::get_mut(arc)
}

impl<T> Visit for Arc<T>
where
    T: Visit + Send + Sync + Any,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        if region.reading {
            let mut id = 0u64;
            id.visit("Id", &mut region)?;
            if id == 0 {
                return Err(VisitError::UnexpectedRcNullIndex);
            }
            if let Some(ptr) = &mut region.arc_map.get(&id) {
                if let Ok(res) = Arc::downcast::<T>(ptr.clone()) {
                    *self = res;
                } else {
                    return Err(VisitError::TypeMismatch {
                        expected: std::any::type_name::<T>(),
                        actual: region.type_name_map.get(&id).unwrap_or(&"MISSING"),
                    });
                }
            } else {
                region.type_name_map.insert(id, std::any::type_name::<T>());
                // 先在独占状态下把内容填好，再放进共享表（理由见 `Visit for Rc`）。
                let result = match arc_to_mut(self) {
                    Some(data) => data.visit("ArcData", &mut region),
                    None => Err(VisitError::SharedReferenceNotUnique(
                        std::any::type_name::<T>(),
                    )),
                };
                if result.is_err() {
                    region.type_name_map.remove(&id);
                    return result;
                }
                region.arc_map.insert(id, self.clone());
            }
        } else {
            let (mut id, serialize_data) = region.arc_id(self);
            id.visit("Id", &mut region)?;
            if serialize_data {
                match arc_to_mut(self) {
                    Some(data) => data.visit("ArcData", &mut region)?,
                    None => {
                        return Err(VisitError::SharedReferenceNotUnique(
                            std::any::type_name::<T>(),
                        ))
                    }
                }
            }
        }

        Ok(())
    }
}

impl<T> Visit for std::rc::Weak<T>
where
    T: Default + Visit + Any,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        if region.reading {
            let mut id = 0u64;
            id.visit("Id", &mut region)?;

            if id != 0 {
                if let Some(ptr) = &mut region.rc_map.get(&id) {
                    if let Ok(res) = Rc::downcast::<T>(ptr.clone()) {
                        *self = Rc::downgrade(&res);
                    } else {
                        return Err(VisitError::TypeMismatch {
                            expected: std::any::type_name::<T>(),
                            actual: region.type_name_map.get(&id).unwrap_or(&"MISSING"),
                        });
                    }
                } else {
                    // 新建一个独占的 `Rc`，填好内容后再放进共享表并降级成 Weak。
                    let mut rc = Rc::new(T::default());
                    region.type_name_map.insert(id, std::any::type_name::<T>());
                    let result = match rc_to_mut(&mut rc) {
                        Some(data) => data.visit("RcData", &mut region),
                        None => Err(VisitError::SharedReferenceNotUnique(
                            std::any::type_name::<T>(),
                        )),
                    };
                    if result.is_err() {
                        region.type_name_map.remove(&id);
                        return result;
                    }
                    region.rc_map.insert(id, rc.clone());
                    *self = Rc::downgrade(&rc);
                }
            }
        } else if let Some(rc) = std::rc::Weak::upgrade(self) {
            let (mut id, serialize_data) = region.rc_id(&rc);
            id.visit("Id", &mut region)?;
            if serialize_data {
                // 从 Weak 升级出来的 `Rc` 必然与数据的所有者共享（升级本身就多出一个
                // 强引用），无法独占访问其内容，这里没有安全路径，只能报错。
                return Err(VisitError::SharedReferenceNotUnique(std::any::type_name::<T>()));
            }
        } else {
            let mut index = 0u64;
            index.visit("Id", &mut region)?;
        }

        Ok(())
    }
}

impl<T> Visit for std::sync::Weak<T>
where
    T: Default + Visit + Send + Sync + 'static,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        if region.reading {
            let mut id = 0u64;
            id.visit("Id", &mut region)?;

            if id != 0 {
                if let Some(ptr) = region.arc_map.get(&id) {
                    if let Ok(res) = Arc::downcast::<T>(ptr.clone()) {
                        *self = Arc::downgrade(&res);
                    } else {
                        return Err(VisitError::TypeMismatch {
                            expected: std::any::type_name::<T>(),
                            actual: region.type_name_map.get(&id).unwrap_or(&"MISSING"),
                        });
                    }
                } else {
                    // 新建一个独占的 `Arc`，填好内容后再放进共享表并降级成 Weak。
                    let mut arc = Arc::new(T::default());
                    region.type_name_map.insert(id, std::any::type_name::<T>());
                    let result = match arc_to_mut(&mut arc) {
                        Some(data) => data.visit("ArcData", &mut region),
                        None => Err(VisitError::SharedReferenceNotUnique(
                            std::any::type_name::<T>(),
                        )),
                    };
                    if result.is_err() {
                        region.type_name_map.remove(&id);
                        return result;
                    }
                    region.arc_map.insert(id, arc.clone());
                    *self = Arc::downgrade(&arc);
                }
            }
        } else if let Some(arc) = std::sync::Weak::upgrade(self) {
            let (mut id, serialize_data) = region.arc_id(&arc);
            id.visit("Id", &mut region)?;
            if serialize_data {
                // 从 Weak 升级出来的 `Arc` 必然与数据的所有者共享，无法独占访问。
                return Err(VisitError::SharedReferenceNotUnique(std::any::type_name::<T>()));
            }
        } else {
            let mut index = 0u64;
            index.visit("Id", &mut region)?;
        }

        Ok(())
    }
}

impl<K, V, S> Visit for HashMap<K, V, S>
where
    K: Visit + Default + Clone + Hash + Eq,
    V: Visit + Default,
    S: BuildHasher + Clone,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut count = self.len() as u32;
        count.visit("Count", &mut region)?;

        if region.is_reading() {
            self.clear();
            for i in 0..(count as usize) {
                let name = format!("Item{i}");

                let mut region = region.enter_region(name.as_str())?;

                let mut key = K::default();
                key.visit("Key", &mut region)?;

                let mut value = V::default();
                value.visit("Value", &mut region)?;

                self.insert(key, value);
            }
        } else {
            for (i, (key, value)) in self.iter_mut().enumerate() {
                let name = format!("Item{i}");

                let mut region = region.enter_region(name.as_str())?;

                let mut key = key.clone();
                key.visit("Key", &mut region)?;

                value.visit("Value", &mut region)?;
            }
        }

        Ok(())
    }
}

impl<K, S> Visit for HashSet<K, S>
where
    K: Visit + Default + Clone + Hash + Eq,
    S: BuildHasher + Clone,
{
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut count = self.len() as u32;
        count.visit("Count", &mut region)?;

        if region.is_reading() {
            self.clear();
            for i in 0..(count as usize) {
                let name = format!("Item{i}");

                let mut region = region.enter_region(name.as_str())?;

                let mut key = K::default();
                key.visit("Key", &mut region)?;

                self.insert(key);
            }
        } else {
            for (i, mut key) in self.clone().into_iter().enumerate() {
                let name = format!("Item{i}");

                let mut region = region.enter_region(name.as_str())?;

                key.visit("Key", &mut region)?;
            }
        }

        Ok(())
    }
}

impl<T: Default + Visit, const SIZE: usize> Visit for [T; SIZE] {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut len = SIZE as u32;
        len.visit("Length", &mut region)?;

        if region.reading {
            if len > SIZE as u32 {
                return VisitResult::Err(VisitError::User(format!(
                    "Not enough space in static array, got {len}, needed {SIZE}!"
                )));
            }

            for index in 0..len {
                let region_name = format!("Item{index}");
                let mut region = region.enter_region(region_name.as_str())?;
                let mut object = T::default();
                object.visit("ItemData", &mut region)?;
                self[index as usize] = object;
            }
        } else {
            for (index, item) in self.iter_mut().enumerate() {
                let region_name = format!("Item{index}");
                let mut region = region.enter_region(region_name.as_str())?;
                item.visit("ItemData", &mut region)?;
            }
        }

        Ok(())
    }
}

impl Visit for Duration {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut secs: u64 = self.as_secs();
        let mut nanos: u32 = self.subsec_nanos();

        secs.visit("Secs", &mut region)?;
        nanos.visit("Nanos", &mut region)?;

        if region.is_reading() {
            *self = Duration::new(secs, nanos);
        }

        Ok(())
    }
}

impl Visit for char {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut bytes = *self as u32;
        bytes.visit(name, visitor)?;
        if visitor.is_reading() {
            *self = char::from_u32(bytes).unwrap();
        }
        Ok(())
    }
}

impl<T: Visit> Visit for Range<T> {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.start.visit("Start", &mut region)?;
        self.end.visit("End", &mut region)?;

        Ok(())
    }
}

impl Visit for usize {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut this = *self as u64;
        this.visit(name, visitor)?;
        if visitor.is_reading() {
            *self = this as usize;
        }
        Ok(())
    }
}

impl Visit for isize {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut this = *self as i64;
        this.visit(name, visitor)?;
        if visitor.is_reading() {
            *self = this as isize;
        }
        Ok(())
    }
}
