//! 代际竞技场（Generational Arena）—— 一种连续可增长数组，支持从中间删除元素而无需移位，
//! 因此不会使其他索引失效。
//!
//! Pool 是一段连续内存，包含固定大小的条目，每个条目可以是空闲或已占用状态。
//! 向 Pool 中放入对象时，会获得一个指向该对象的句柄。
//! 之后可以用该句柄借用对象的引用。
//! 句柄可以指向某个对象，也可以无效，这与原始指针类似，但有两点重要区别：
//!
//! 1) 可以在访问对象前检查句柄是否有效。
//! 2) 可以确认句柄仍然指向最初创建时的那个对象，而不是该位置上后来替换的对象。
//!    每个句柄存储一个称为 generation（代次）的字段，与条目共享；
//!    当条目和句柄的 generation 相同时，句柄才是有效的。
//!    这可以防止句柄索引有效但该位置的对象已被替换的情况。
//!
//! 连续内存块提高了内存操作效率——CPU 会逐块将数据加载到缓存中，
//! 避免了可能导致缓存失效的间接引用，即所谓的缓存友好性。

use crate::{reflect::prelude::*, visitor::prelude::*};
use std::any::type_name;
use std::cell::UnsafeCell;
use std::fmt::{Display, Formatter};
use std::{
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    ops::{Index, IndexMut},
};

pub mod handle;
pub mod multiborrow;
pub mod payload;

use crate::reflect::TypeInfo;
pub use handle::*;
pub use multiborrow::*;
pub use payload::*;

const INVALID_GENERATION: u32 = 0;

/// Pool 允许在连续内存块中创建任意数量的对象。
/// 相比在堆上逐个分配，创建和删除对象的速度更快。
/// 同时由于对象存储在连续内存中，访问效率更高（缓存友好）。
pub struct Pool<T, P = Option<T>>
where
    T: Sized,
    P: PayloadContainer<Element = T>,
{
    records: Vec<PoolRecord<T, P>>,
    free_stack: Vec<u32>,
}

impl<T: Sized + Debug, P: PayloadContainer<Element = T> + 'static> Debug for Pool<T, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("Pool");
        for (handle, value) in self.pair_iter() {
            s.field(&handle.to_string(), value);
        }
        s.finish()
    }
}

/// 该 trait 统一了对象池对象及其变体类型。
///
/// 若 T 是对象池的对象类型，则 [`ObjectOrVariant::convert_to_dest_type`] 直接返回对象本身。
///
/// 若 T 是对象池对象类型的某个变体，则返回该变体。
///
/// [`Pool`] 结构体通过该 trait 统一处理池对象与其变体的获取逻辑。
pub trait ObjectOrVariant<T> {
    fn convert_to_dest_type(object: &T) -> Option<&Self>;
    fn convert_to_dest_type_mut(object: &mut T) -> Option<&mut Self>;
}

/// 该 trait 是在子 crate 中间接实现 [`ObjectOrVariant`] 的辅助 trait。
///
/// 在子 crate 中为类型 `U` 实现 [`ObjectOrVariant`] 时，需要为 [`PhantomData<U>`] 实现 [`ObjectOrVariantHelper`]。
///
/// 这是因为 Rust 不支持 `impl<T> ForeignTrait<LocalType> for T`，
/// 但支持 `impl<T> ForeignTrait<LocalType> for ForeignType<T>`。
///
/// 详见 [此处](https://rust-lang.github.io/rfcs/2451-re-rebalancing-coherence.html#concrete-orphan-rules)。
///
/// 因此不能直接用 `impl<U: TraitBound> ObjectOrVariant<LocalType> for U`，
/// 但可以用 `impl<U: TraitBound> ObjectOrVariantHelper<LocalType, U> for PhantomData<U>`。
///
/// 通过间接跳转去掉 [`PhantomData<U>`]，使对象池方法的 API 更简洁：
/// 直接传入 `U` 而非 [`PhantomData<U>`]。
pub trait ObjectOrVariantHelper<T, U> {
    fn convert_to_dest_type_helper(object: &T) -> Option<&U>;
    fn convert_to_dest_type_helper_mut(object: &mut T) -> Option<&mut U>;
}

// 这是对象池类型的默认实现。
// 对于对象池对象的变体类型，需要逐一实现，因为 Rust 不支持多个覆盖实现（blanket impl）。
impl<T> ObjectOrVariantHelper<T, T> for PhantomData<T> {
    fn convert_to_dest_type_helper(object: &T) -> Option<&T> {
        Some(object)
    }
    fn convert_to_dest_type_helper_mut(object: &mut T) -> Option<&mut T> {
        Some(object)
    }
}

// 这个覆盖实现将实现了 ObjectOrVariantHelper 的类型自动包装为 ObjectOrVariant trait。
impl<T, U> ObjectOrVariant<T> for U
where
    PhantomData<U>: ObjectOrVariantHelper<T, U>,
{
    fn convert_to_dest_type(object: &T) -> Option<&Self> {
        PhantomData::<U>::convert_to_dest_type_helper(object)
    }
    fn convert_to_dest_type_mut(object: &mut T) -> Option<&mut Self> {
        PhantomData::<U>::convert_to_dest_type_helper_mut(object)
    }
}

impl<T, P> Reflect for Pool<T, P>
where
    T: Reflect + PartialEq,
    P: PayloadContainer<Element = T> + Reflect + PartialEq,
    Pool<T, P>: Clone,
{
    fn type_info() -> TypeInfo {
        TypeInfo {
            source_path: file!(),
            type_name: type_name::<Self>(),
            assembly_name: env!("CARGO_PKG_NAME"),
            doc_comment: "",
            derived_types: &[],
            type_uuid: combine_uuids(
                uuid!("1f615965-820a-4948-970d-8e99cd588006"),
                combine_uuids(T::type_info().type_uuid, P::type_info().type_uuid),
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
        (other as &dyn std::any::Any)
            .downcast_ref::<Self>()
            .map(|other| other == self)
    }

    #[inline]
    fn fields_ref(&self, func: &mut dyn FnMut(&[FieldRef])) {
        func(&[])
    }

    #[inline]
    fn fields_mut(&mut self, func: &mut dyn FnMut(&mut [FieldMut])) {
        func(&mut [])
    }

    #[inline]
    fn set(&mut self, value: Box<dyn Reflect>) -> Result<Box<dyn Reflect>, Box<dyn Reflect>> {
        let this = std::mem::replace(self, value.take()?);
        Ok(Box::new(this))
    }

    fn field_direct_ref(&self, _index: usize) -> Option<FieldRef<'_, '_>> {
        None
    }

    fn field_direct_mut(&mut self, _index: usize) -> Option<FieldMut<'_, '_>> {
        None
    }

    #[inline]
    fn as_array(&self) -> Option<&dyn ReflectArray> {
        Some(self)
    }

    #[inline]
    fn as_array_mut(&mut self) -> Option<&mut dyn ReflectArray> {
        Some(self)
    }
}

impl<T, P> ReflectArray for Pool<T, P>
where
    T: Reflect + PartialEq,
    P: PayloadContainer<Element = T> + Reflect + PartialEq,
    Pool<T, P>: Clone,
{
    #[inline]
    fn reflect_index(&self, index: usize) -> Option<&dyn Reflect> {
        self.at(index as u32).ok().map(|p| p as &dyn Reflect)
    }

    #[inline]
    fn reflect_index_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.at_mut(index as u32)
            .ok()
            .map(|p| p as &mut dyn Reflect)
    }

    #[inline]
    fn reflect_len(&self) -> usize {
        self.get_capacity() as usize
    }
}

impl<T, P> PartialEq for Pool<T, P>
where
    T: PartialEq,
    P: PayloadContainer<Element = T> + PartialEq,
{
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.records == other.records
    }
}

// 零：未借用。负值：可变借用数量；正值：不可变借用数量。
#[derive(Default, Debug)]
struct RefCounter(pub UnsafeCell<isize>);

unsafe impl Sync for RefCounter {}
unsafe impl Send for RefCounter {}

impl RefCounter {
    unsafe fn get(&self) -> isize { unsafe {
        *self.0.get()
    }}

    unsafe fn increment(&self) { unsafe {
        *self.0.get() += 1;
    }}

    unsafe fn decrement(&self) { unsafe {
        *self.0.get() -= 1;
    }}
}

#[derive(Debug)]
struct PoolRecord<T, P = Option<T>>
where
    T: Sized,
    P: PayloadContainer<Element = T>,
{
    ref_counter: RefCounter,
    // 代次编号，用于跟踪生命周期。仅当句柄的代次与记录的代次一致时，句柄才有效。
    // 注意：零是"无效代次"，用于 None 句柄。
    generation: u32,
    // 实际载荷。
    payload: Payload<P>,
}

impl<T, P> PartialEq for PoolRecord<T, P>
where
    T: PartialEq,
    P: PayloadContainer<Element = T> + PartialEq,
{
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.payload.get() == other.payload.get()
    }
}

impl<T, P> Default for PoolRecord<T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    #[inline]
    fn default() -> Self {
        Self {
            ref_counter: Default::default(),
            generation: INVALID_GENERATION,
            payload: Payload::new_empty(),
        }
    }
}

impl<T, P> Visit for PoolRecord<T, P>
where
    T: Visit + 'static,
    P: PayloadContainer<Element = T> + Visit,
{
    #[inline]
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.generation.visit("Generation", &mut region)?;
        self.payload.get_mut().visit("Payload", &mut region)?;

        Ok(())
    }
}

impl<T> Clone for Handle<T> {
    #[inline]
    fn clone(&self) -> Handle<T> {
        *self
    }
}

impl<T, P> Visit for Pool<T, P>
where
    T: Visit + 'static,
    P: PayloadContainer<Element = T> + Default + Visit + 'static,
{
    #[inline]
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;
        self.records.visit("Records", &mut region)?;
        self.free_stack.visit("FreeStack", &mut region)?;
        Ok(())
    }
}

impl<T, P> Default for Pool<T, P>
where
    T: 'static,
    P: PayloadContainer<Element = T> + 'static,
{
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// 取票（Ticket）用于临时取出对象，并承诺稍后归还。
/// 若 Ticket 被 drop 而未归还，则会 panic。
#[derive(Debug)]
pub struct Ticket<T> {
    index: u32,
    marker: PhantomData<T>,
}

impl<T> Drop for Ticket<T> {
    fn drop(&mut self) {
        panic!(
            "索引为 {} 的对象必须归还到它所属的对象池！\
            若不再需要该对象，请调用 Pool::forget_ticket。",
            self.index
        )
    }
}

impl<T, P> Clone for PoolRecord<T, P>
where
    T: Clone,
    P: PayloadContainer<Element = T> + Clone + 'static,
{
    #[inline]
    fn clone(&self) -> Self {
        Self {
            ref_counter: Default::default(),
            generation: self.generation,
            payload: self.payload.clone(),
        }
    }
}

impl<T, P> Clone for Pool<T, P>
where
    P: PayloadContainer<Element = T> + Clone + 'static,
    T: Clone,
{
    #[inline]
    fn clone(&self) -> Self {
        Self {
            records: self.records.clone(),
            free_stack: self.free_stack.clone(),
        }
    }
}

#[derive(PartialEq, Copy, Clone)]
pub enum PoolError {
    InvalidIndex(u32),
    InvalidGeneration(u32),
    InvalidType(ErasedHandle),
    Empty(ErasedHandle),
    NoSuchField(ErasedHandle),
    MutablyBorrowed(ErasedHandle),
    ImmutablyBorrowed(ErasedHandle),
    UnknownDependentObject(ErasedHandle),
}

impl std::error::Error for PoolError {}

impl Display for PoolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIndex(index) => {
                write!(
                    f,
                    "The index {index} of the handle is invalid and does not point to any object!"
                )
            }
            Self::InvalidGeneration(generation) => {
                write!(
                    f,
                    "The generation {generation} of the handle is invalid! It means that the object \
                    at the handle was freed and it position was taken by some other object."
                )
            }
            Self::InvalidType(handle) => {
                write!(
                    f,
                    "The type of the object at the handle {handle} is different from what was requested!"
                )
            }
            Self::Empty(handle) => {
                write!(f, "There's no object at {handle} handle.")
            }
            Self::UnknownDependentObject(handle) => {
                write!(
                    f,
                    "Unable to fetch the dependent object by handle {handle}, because the handle \
                is invalid!"
                )
            }
            Self::NoSuchField(handle) => write!(
                f,
                "An object at {handle} handle does not have such component.",
            ),
            Self::MutablyBorrowed(handle) => {
                write!(
                    f,
                    "An object at {handle} handle cannot be borrowed immutably, because it is \
                    already borrowed mutably."
                )
            }
            Self::ImmutablyBorrowed(handle) => {
                write!(
                    f,
                    "An object at {handle} handle cannot be borrowed mutably, because it is \
                    already borrowed immutably."
                )
            }
        }
    }
}

impl Debug for PoolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

impl<T, P> Pool<T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    #[inline]
    pub fn new() -> Self {
        Pool {
            records: Vec::new(),
            free_stack: Vec::new(),
        }
    }

    #[inline]
    pub fn with_capacity(capacity: u32) -> Self {
        let capacity = usize::try_from(capacity).expect("capacity overflowed usize");
        Pool {
            records: Vec::with_capacity(capacity),
            free_stack: Vec::new(),
        }
    }

    fn records_len(&self) -> u32 {
        u32::try_from(self.records.len()).expect("Number of records overflowed u32")
    }

    fn records_get(&self, index: u32) -> Result<&PoolRecord<T, P>, PoolError> {
        self.records
            .get(usize::try_from(index).expect("Index overflowed usize"))
            .ok_or(PoolError::InvalidIndex(index))
    }

    fn records_get_mut(&mut self, index: u32) -> Result<&mut PoolRecord<T, P>, PoolError> {
        self.records
            .get_mut(usize::try_from(index).expect("Index overflowed usize"))
            .ok_or(PoolError::InvalidIndex(index))
    }

    #[inline]
    pub fn try_get<U: ObjectOrVariant<T>>(&self, handle: Handle<U>) -> Result<&U, PoolError> {
        let handle = handle.transmute();
        let pool_object = self.try_borrow(handle)?;
        U::convert_to_dest_type(pool_object).ok_or(PoolError::InvalidType(handle.into()))
    }

    #[inline]
    pub fn try_get_mut<U: ObjectOrVariant<T>>(
        &mut self,
        handle: Handle<U>,
    ) -> Result<&mut U, PoolError> {
        let handle = handle.transmute();
        let pool_object = self.try_borrow_mut(handle)?;
        U::convert_to_dest_type_mut(pool_object).ok_or(PoolError::InvalidType(handle.into()))
    }

    #[inline]
    #[must_use]
    pub fn spawn(&mut self, payload: T) -> Handle<T> {
        self.spawn_with(|_| payload)
    }

    /// 尝试在指定位置向对象池放入对象。若对应条目已被占用，则返回 `Err(payload)`。
    ///
    /// # 性能
    ///
    /// 最坏情况下此方法的复杂度为 `O(n)`，其中 `n` 是对象池中的空闲记录数量。
    /// 在典型用例中，`n` 往往很小。需要注意的是，如果对象池已满且你要把对象放到池尾，
    /// 那么此方法的复杂度将是 `O(1)`。
    ///
    /// # Panic
    ///
    /// 如果索引已被占用或被保留（例如被 [`take_reserve`] 占用），则 panic。
    ///
    /// [`take_reserve`]: Pool::take_reserve
    #[inline]
    pub fn spawn_at(&mut self, index: u32, payload: T) -> Result<Handle<T>, T> {
        self.spawn_at_internal(index, INVALID_GENERATION, payload)
    }

    /// 尝试在指定句柄的位置向对象池放入对象。若对应条目已被占用，则返回 `Err(payload)`。
    ///
    /// # 性能
    ///
    /// 最坏情况下此方法的复杂度为 `O(n)`，其中 `n` 是对象池中的空闲记录数量。
    /// 在典型用例中，`n` 往往很小。需要注意的是，如果对象池已满且你要把对象放到池尾，
    /// 那么此方法的复杂度将是 `O(1)`。
    ///
    /// # Panic
    ///
    /// 如果索引已被占用或被保留（例如被 [`take_reserve`] 占用），则 panic。
    ///
    /// [`take_reserve`]: Pool::take_reserve
    #[inline]
    pub fn spawn_at_handle(&mut self, handle: Handle<T>, payload: T) -> Result<Handle<T>, T> {
        self.spawn_at_internal(handle.index, handle.generation, payload)
    }

    fn spawn_at_internal(
        &mut self,
        index: u32,
        desired_generation: u32,
        payload: T,
    ) -> Result<Handle<T>, T> {
        let index_usize = usize::try_from(index).expect("index overflowed usize");
        match self.records.get_mut(index_usize) {
            Some(record) => match record.payload.as_ref() {
                Some(_) => Err(payload),
                None => {
                    let position = self
                        .free_stack
                        .iter()
                        .rposition(|i| *i == index)
                        .expect("free_stack must contain the index of the empty record (most likely attempting to spawn at a reserved index)!");

                    self.free_stack.remove(position);

                    let generation = if desired_generation == INVALID_GENERATION {
                        record.generation + 1
                    } else {
                        desired_generation
                    };

                    record.generation = generation;
                    record.payload = Payload::new(payload);

                    Ok(Handle::new(index, generation))
                }
            },
            None => {
                // 生成缺失的记录以填补空洞。
                for i in self.records_len()..index {
                    self.records.push(PoolRecord {
                        ref_counter: Default::default(),
                        generation: 1,
                        payload: Payload::new_empty(),
                    });
                    self.free_stack.push(i);
                }

                let generation = if desired_generation == INVALID_GENERATION {
                    1
                } else {
                    desired_generation
                };

                self.records.push(PoolRecord {
                    ref_counter: Default::default(),
                    generation,
                    payload: Payload::new(payload),
                });

                Ok(Handle::new(index, generation))
            }
        }
    }

    #[inline]
    #[must_use]
    /// 以给定句柄为 key 来构造对象。
    /// 注意：函数执行完毕前，该句柄**尚不**有效。
    pub fn spawn_with<F: FnOnce(Handle<T>) -> T>(&mut self, callback: F) -> Handle<T> {
        if let Some(free_index) = self.free_stack.pop() {
            let record = self
                .records_get_mut(free_index)
                .expect("free stack contained invalid index");

            if record.payload.is_some() {
                panic!(
                    "尝试在已有载荷的池记录位置 spawn 对象！记录索引为 {free_index}"
                );
            }

            let generation = record.generation + 1;
            let handle = Handle {
                index: free_index,
                generation,
                type_marker: PhantomData,
            };

            let payload = callback(handle);

            record.generation = generation;
            record.payload.replace(payload);
            handle
        } else {
            // 无空闲记录，创建新记录
            let generation = 1;

            let handle = Handle {
                index: self.records.len() as u32,
                generation,
                type_marker: PhantomData,
            };

            let payload = callback(handle);

            let record = PoolRecord {
                ref_counter: Default::default(),
                generation,
                payload: Payload::new(payload),
            };

            self.records.push(record);

            handle
        }
    }

    #[inline]
    /// 异步构造对象（以给定句柄为 key）。
    /// 注意：函数执行完毕前，该句柄**尚不**有效。
    pub async fn spawn_with_async<F, Fut>(&mut self, callback: F) -> Handle<T>
    where
        F: FnOnce(Handle<T>) -> Fut,
        Fut: Future<Output = T>,
    {
        if let Some(free_index) = self.free_stack.pop() {
            let record = self
                .records_get_mut(free_index)
                .expect("free stack contained invalid index");

            if record.payload.is_some() {
                panic!(
                    "尝试在已有载荷的池记录位置 spawn 对象（async）！记录索引为 {free_index}"
                );
            }

            let generation = record.generation + 1;
            let handle = Handle {
                index: free_index,
                generation,
                type_marker: PhantomData,
            };

            let payload = callback(handle).await;

            record.generation = generation;
            record.payload.replace(payload);
            handle
        } else {
            // 没有空闲记录，创建新的记录。
            let generation = 1;

            let handle = Handle {
                index: self.records.len() as u32,
                generation,
                type_marker: PhantomData,
            };

            let payload = callback(handle).await;

            let record = PoolRecord {
                generation,
                ref_counter: Default::default(),
                payload: Payload::new(payload),
            };

            self.records.push(record);

            handle
        }
    }

    /// 预生成一组可用于后续 spawn 的句柄（不修改对象池）。
    /// 生成的句柄可配合 [`Self::spawn_at_handle`] 使用。
    #[inline]
    pub fn generate_free_handles(&self, amount: usize) -> Vec<Handle<T>> {
        let mut free_handles = Vec::with_capacity(amount);
        free_handles.extend(
            self.free_stack
                .iter()
                .rev()
                .take(amount)
                .map(|i| Handle::new(*i, self.records[*i as usize].generation + 1)),
        );
        if free_handles.len() < amount {
            let remainder = amount - free_handles.len();
            free_handles.extend(
                (self.records.len()..self.records.len() + remainder)
                    .map(|i| Handle::new(i as u32, 1)),
            );
        }
        free_handles
    }

    /// 返回下一个可用于 spawn 的句柄。该句柄保证指向对象池中的空槽位。
    /// 在调用此方法后立刻调用 [`Self::spawn_at_handle`] 必然成功。
    #[inline]
    pub fn next_free_handle(&self) -> Handle<T> {
        if let Some(index) = self.free_stack.last().cloned() {
            let generation = self.records[index as usize].generation + 1;
            Handle {
                index,
                generation,
                type_marker: PhantomData,
            }
        } else {
            Handle {
                index: self.records.len() as u32,
                generation: 1,
                type_marker: PhantomData,
            }
        }
    }

    /// 通过句柄借用对象的共享引用。
    ///
    /// # Panics
    ///
    /// 若句柄越界，或句柄的代次与池记录的代次不一致（即该位置的对象已被替换），则 panic。
    #[inline]
    #[must_use]
    pub fn borrow(&self, handle: Handle<T>) -> &T {
        self.try_borrow(handle).unwrap()
    }

    /// 通过句柄借用对象的可变引用。
    ///
    /// # Panics
    ///
    /// 若句柄越界，或句柄的代次与池记录的代次不一致，则 panic。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// let a = pool.spawn(1);
    /// let a = pool.borrow_mut(a);
    /// *a = 11;
    /// ```
    #[inline]
    #[must_use]
    pub fn borrow_mut(&mut self, handle: Handle<T>) -> &mut T {
        self.try_borrow_mut(handle).unwrap()
    }

    /// 通过句柄借用对象的共享引用。
    ///
    /// 若句柄越界或代次不匹配，则返回 Err。
    #[inline]
    pub fn try_borrow(&self, handle: Handle<T>) -> Result<&T, PoolError> {
        self.records_get(handle.index).and_then(|r| {
            if r.generation == handle.generation {
                r.payload.as_ref().ok_or(PoolError::Empty(handle.into()))
            } else {
                Err(PoolError::InvalidGeneration(handle.generation))
            }
        })
    }

    /// 通过句柄借用对象的可变引用。
    ///
    /// 若句柄越界或代次不匹配，则返回 Err。
    #[inline]
    pub fn try_borrow_mut(&mut self, handle: Handle<T>) -> Result<&mut T, PoolError> {
        self.records_get_mut(handle.index).and_then(|r| {
            if r.generation == handle.generation {
                r.payload.as_mut().ok_or(PoolError::Empty(handle.into()))
            } else {
                Err(PoolError::InvalidGeneration(handle.generation))
            }
        })
    }

    /// 同时借用两个对象的可变引用。仅当两个句柄不相同时才会成功。
    ///
    /// # Panics
    ///
    /// 参见 [`borrow_mut`](Self::borrow_mut)。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// let a = pool.spawn(1);
    /// let b = pool.spawn(2);
    /// let (a, b) = pool.borrow_two_mut((a, b));
    /// *a = 11;
    /// *b = 22;
    /// ```
    #[inline]
    #[must_use = "句柄集合不得被忽略"]
    pub fn borrow_two_mut(&mut self, handles: (Handle<T>, Handle<T>)) -> (&mut T, &mut T) {
        // 防止对同一记录给出两个可变引用。
        assert_ne!(handles.0.index, handles.1.index);
        unsafe {
            let this = self as *mut Self;
            ((*this).borrow_mut(handles.0), (*this).borrow_mut(handles.1))
        }
    }

    /// 同时借用三个对象的可变引用。仅当三个句柄各不相同时才会成功。
    ///
    /// # Panics
    ///
    /// 参见 [`borrow_mut`](Self::borrow_mut)。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// let a = pool.spawn(1);
    /// let b = pool.spawn(2);
    /// let c = pool.spawn(3);
    /// let (a, b, c) = pool.borrow_three_mut((a, b, c));
    /// *a = 11;
    /// *b = 22;
    /// *c = 33;
    /// ```
    #[inline]
    #[must_use = "句柄集合不得被忽略"]
    pub fn borrow_three_mut(
        &mut self,
        handles: (Handle<T>, Handle<T>, Handle<T>),
    ) -> (&mut T, &mut T, &mut T) {
        // 防止对同一记录给出可变引用。
        assert_ne!(handles.0.index, handles.1.index);
        assert_ne!(handles.0.index, handles.2.index);
        assert_ne!(handles.1.index, handles.2.index);
        unsafe {
            let this = self as *mut Self;
            (
                (*this).borrow_mut(handles.0),
                (*this).borrow_mut(handles.1),
                (*this).borrow_mut(handles.2),
            )
        }
    }

    /// 同时借用四个对象的可变引用。仅当四个句柄各不相同时才会成功。
    ///
    /// # Panics
    ///
    /// 参见 [`borrow_mut`](Self::borrow_mut)。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// let a = pool.spawn(1);
    /// let b = pool.spawn(2);
    /// let c = pool.spawn(3);
    /// let d = pool.spawn(4);
    /// let (a, b, c, d) = pool.borrow_four_mut((a, b, c, d));
    /// *a = 11;
    /// *b = 22;
    /// *c = 33;
    /// *d = 44;
    /// ```
    #[inline]
    #[must_use = "句柄集合不得被忽略"]
    pub fn borrow_four_mut(
        &mut self,
        handles: (Handle<T>, Handle<T>, Handle<T>, Handle<T>),
    ) -> (&mut T, &mut T, &mut T, &mut T) {
        // 防止对同一记录给出可变引用（const generics 尚未稳定，这里写法略显笨拙）。
        assert_ne!(handles.0.index, handles.1.index);
        assert_ne!(handles.0.index, handles.2.index);
        assert_ne!(handles.0.index, handles.3.index);
        assert_ne!(handles.1.index, handles.2.index);
        assert_ne!(handles.1.index, handles.3.index);
        assert_ne!(handles.2.index, handles.3.index);
        unsafe {
            let this = self as *mut Self;
            (
                (*this).borrow_mut(handles.0),
                (*this).borrow_mut(handles.1),
                (*this).borrow_mut(handles.2),
                (*this).borrow_mut(handles.3),
            )
        }
    }

    /// 尝试在同时持有第一个对象的可变引用时，通过存储在第一个对象中的句柄借用第二个对象。
    #[inline]
    pub fn try_borrow_dependant_mut<F>(
        &mut self,
        handle: Handle<T>,
        func: F,
    ) -> (Result<&mut T, PoolError>, Result<&mut T, PoolError>)
    where
        F: FnOnce(&T) -> Handle<T>,
    {
        let this = unsafe { &mut *(self as *mut Pool<T, P>) };
        let first = self.try_borrow_mut(handle);
        if let Ok(first_object) = first.as_ref() {
            let second_handle = func(first_object);
            if second_handle != handle {
                return (first, this.try_borrow_mut(second_handle));
            } else {
                return (first, Err(PoolError::MutablyBorrowed(second_handle.into())));
            }
        }

        (first, Err(PoolError::UnknownDependentObject(handle.into())))
    }

    /// 使用句柄将对象移出对象池。所有指向该对象的句柄将全部失效。
    ///
    /// # Panics
    ///
    /// 若给定句柄无效，则 panic。
    #[inline]
    pub fn free(&mut self, handle: Handle<T>) -> T {
        self.try_free(handle).unwrap()
    }

    /// 尝试使用句柄将对象移出对象池。若给定句柄无效，则返回 Err。
    /// 对象移出后，所有指向该对象的句柄将全部失效。
    #[inline]
    pub fn try_free(&mut self, handle: Handle<T>) -> Result<T, PoolError> {
        let index = usize::try_from(handle.index).expect("index overflowed usize");
        self.records
            .get_mut(index)
            .ok_or(PoolError::InvalidIndex(handle.index))
            .and_then(|record| {
                if record.generation == handle.generation {
                    if let Some(payload) = record.payload.take() {
                        self.free_stack.push(handle.index);
                        Ok(payload)
                    } else {
                        Err(PoolError::Empty(handle.into()))
                    }
                } else {
                    Err(PoolError::InvalidGeneration(handle.generation))
                }
            })
    }

    /// 使用句柄临时取出对象，并承诺将其归还。返回 (ticket, value) 对。
    /// **Ticket 必须用于归还对象！**
    ///
    /// # 动机
    ///
    /// 当你需要临时持有对象的所有权、对其进行操作后再放回对象池，
    /// 同时保持所有句柄有效，并允许在此期间向池中添加新对象而不覆盖该位置，
    /// 此方法非常有用。
    ///
    /// # 注意
    ///
    /// 对象取出期间，所有指向该对象的句柄将**暂时无效**！
    /// 该池记录会被预留给后续的 [`put_back`] 调用。
    /// 若丢失 ticket，将导致一个永远无法使用的空槽位。
    ///
    /// # Panics
    ///
    /// 若给定句柄无效，则 panic。
    ///
    /// [`put_back`]: Pool::put_back
    #[inline]
    pub fn take_reserve(&mut self, handle: Handle<T>) -> (Ticket<T>, T) {
        self.try_take_reserve(handle).unwrap()
    }

    /// 与 [`take_reserve`] 相同，但返回 Result 而非 panic。
    ///
    /// [`take_reserve`]: Pool::take_reserve
    #[inline]
    pub fn try_take_reserve(&mut self, handle: Handle<T>) -> Result<(Ticket<T>, T), PoolError> {
        let record = self.records_get_mut(handle.index)?;
        if record.generation == handle.generation {
            if let Some(payload) = record.payload.take() {
                let ticket = Ticket {
                    index: handle.index,
                    marker: PhantomData,
                };
                Ok((ticket, payload))
            } else {
                Err(PoolError::Empty(handle.into()))
            }
        } else {
            Err(PoolError::InvalidGeneration(handle.generation))
        }
    }

    /// 使用 ticket 将对象归还对象池。详见 [`take_reserve`]。
    ///
    /// [`take_reserve`]: Pool::take_reserve
    #[inline]
    pub fn put_back(&mut self, ticket: Ticket<T>, value: T) -> Handle<T> {
        let record = self
            .records_get_mut(ticket.index)
            .expect("Ticket index was invalid");
        let old = record.payload.replace(value);
        assert!(old.is_none());
        let handle = Handle::new(ticket.index, record.generation);
        std::mem::forget(ticket);
        handle
    }

    /// 放弃 ticket，使对应槽位重新可用。
    /// 当你不再需要归还对象，只想让槽位重新可用时使用此方法。
    #[inline]
    pub fn forget_ticket(&mut self, ticket: Ticket<T>) {
        self.free_stack.push(ticket.index);
        std::mem::forget(ticket);
    }

    /// 返回总容量（容量不等于实际对象数量！）
    #[inline]
    #[must_use]
    pub fn get_capacity(&self) -> u32 {
        u32::try_from(self.records.len()).expect("records.len() overflowed u32")
    }

    /// 销毁对象池中所有对象，所有句柄将全部失效。
    ///
    /// # 注意
    ///
    /// 若对象池中的对象之间存在相互引用（句柄），请谨慎使用此方法。
    /// 调用后所有句柄将失效，后续 [`borrow`](Self::borrow) 或 [`borrow_mut`](Self::borrow_mut)
    /// 的调用将 panic。
    #[inline]
    pub fn clear(&mut self) {
        self.records.clear();
        self.free_stack.clear();
    }

    #[inline]
    pub fn at_mut(&mut self, n: u32) -> Result<&mut T, PoolError> {
        self.records_get_mut(n).and_then(|rec| {
            rec.payload
                .as_mut()
                .ok_or(PoolError::Empty(ErasedHandle::new(n, 0)))
        })
    }

    #[inline]
    pub fn at(&self, n: u32) -> Result<&T, PoolError> {
        self.records_get(n).and_then(|rec| {
            rec.payload
                .get()
                .as_ref()
                .ok_or(PoolError::Empty(ErasedHandle::new(n, 0)))
        })
    }

    #[inline]
    #[must_use]
    pub fn handle_from_index(&self, n: u32) -> Handle<T> {
        if let Ok(record) = self.records_get(n) {
            if record.generation != INVALID_GENERATION {
                return Handle::new(n, record.generation);
            }
        }
        Handle::NONE
    }

    /// 返回对象池中存活对象的精确数量。
    ///
    /// 通过 [`take_reserve`] 预留的记录**不**计入其中。
    ///
    /// 此方法需遍历整个对象池，时间复杂度为 `O(n)`。
    ///
    /// 另见 [`total_count`]。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// pool.spawn(123);
    /// pool.spawn(321);
    /// assert_eq!(pool.alive_count(), 2);
    /// ```
    ///
    /// [`take_reserve`]: Pool::take_reserve
    /// [`total_count`]: Pool::total_count
    #[inline]
    #[must_use]
    pub fn alive_count(&self) -> u32 {
        let cnt = self.iter().count();
        u32::try_from(cnt).expect("alive_count overflowed u32")
    }

    /// 返回对象池中已分配对象的数量（含 [`take_reserve`] 预留的记录）。
    ///
    /// 此方法时间复杂度为 `O(1)`。
    ///
    /// 另见 [`alive_count`]。
    ///
    /// [`take_reserve`]: Pool::take_reserve
    /// [`alive_count`]: Pool::alive_count
    #[inline]
    pub fn total_count(&self) -> u32 {
        let free = u32::try_from(self.free_stack.len()).expect("free stack length overflowed u32");
        self.records_len() - free
    }

    #[inline]
    pub fn replace(&mut self, handle: Handle<T>, payload: T) -> Option<T> {
        let index_usize = usize::try_from(handle.index).expect("index overflowed usize");
        if let Some(record) = self.records.get_mut(index_usize) {
            if record.generation == handle.generation {
                self.free_stack.retain(|i| *i != handle.index);

                record.payload.replace(payload)
            } else {
                panic!("尝试使用悬空句柄替换对象池中的对象！句柄为 {:?}，但池记录的代次为 {}", handle, record.generation);
            }
        } else {
            None
        }
    }

    /// 返回对象池中第一个元素的共享引用（若存在）。
    pub fn first_ref(&self) -> Option<&T> {
        self.iter().next()
    }

    /// 返回对象池中第一个元素的可变引用（若存在）。
    pub fn first_mut(&mut self) -> Option<&mut T> {
        self.iter_mut().next()
    }

    /// 检查给定句柄是否指向某个对象。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// let handle = pool.spawn(123);
    /// assert_eq!(pool.is_valid_handle(handle), true)
    /// ```
    #[inline]
    pub fn is_valid_handle(&self, handle: Handle<impl ObjectOrVariant<T>>) -> bool {
        if let Ok(record) = self.records_get(handle.index) {
            record.payload.is_some() && record.generation == handle.generation
        } else {
            false
        }
    }

    /// 创建迭代器，遍历对象池中所有已占用的记录。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// pool.spawn(123);
    /// pool.spawn(321);
    /// let mut iter = pool.iter();
    /// assert_eq!(*iter.next().unwrap(), 123);
    /// assert_eq!(*iter.next().unwrap(), 321);
    /// ```
    #[must_use]
    #[inline]
    pub fn iter(&self) -> PoolIterator<'_, T, P> {
        unsafe {
            PoolIterator {
                ptr: self.records.as_ptr(),
                end: self.records.as_ptr().add(self.records.len()),
                marker: PhantomData,
            }
        }
    }

    /// 创建键值对迭代器，以 (handle, payload) 对的形式遍历已占用记录。
    /// 需要在遍历时获知记录句柄时非常有用。
    #[inline]
    pub fn pair_iter(&self) -> PoolPairIterator<'_, T, P> {
        PoolPairIterator {
            pool: self,
            current: 0,
        }
    }

    /// 创建可变迭代器，遍历对象池中所有已占用的记录，允许修改载荷。
    ///
    /// # Example
    ///
    /// ```
    /// use kcore::pool::Pool;
    /// let mut pool = Pool::<u32>::new();
    /// pool.spawn(123);
    /// pool.spawn(321);
    /// let mut iter = pool.iter_mut();
    /// assert_eq!(*iter.next().unwrap(), 123);
    /// assert_eq!(*iter.next().unwrap(), 321);
    /// ```
    #[must_use]
    #[inline]
    pub fn iter_mut(&mut self) -> PoolIteratorMut<'_, T, P> {
        unsafe {
            PoolIteratorMut {
                ptr: self.records.as_mut_ptr(),
                end: self.records.as_mut_ptr().add(self.records.len()),
                marker: PhantomData,
            }
        }
    }

    /// 创建可变键值对迭代器，以 (handle, payload) 对的形式遍历已占用记录。
    /// 需要在遍历时获知记录句柄时非常有用。
    #[inline]
    pub fn pair_iter_mut(&mut self) -> PoolPairIteratorMut<'_, T, P> {
        unsafe {
            PoolPairIteratorMut {
                current: 0,
                ptr: self.records.as_mut_ptr(),
                end: self.records.as_mut_ptr().add(self.records.len()),
                marker: PhantomData,
            }
        }
    }

    /// 保留满足 `pred` 条件的记录，删除其余记录。适用于按条件批量清除对象。
    #[inline]
    pub fn retain<F>(&mut self, mut pred: F)
    where
        F: FnMut(&T) -> bool,
    {
        for (i, record) in self.records.iter_mut().enumerate() {
            if record.generation == INVALID_GENERATION {
                continue;
            }

            let retain = if let Some(payload) = record.payload.as_ref() {
                pred(payload)
            } else {
                continue;
            };

            if !retain {
                self.free_stack.push(i as u32);
                record.payload.take(); // and Drop
            }
        }
    }

    /// 开始多借用，允许同时借用对象池中任意数量的**唯一**引用。
    /// 详见 [`MultiBorrowContext::try_get`]。
    #[inline]
    pub fn begin_multi_borrow(&mut self) -> MultiBorrowContext<'_, T, P> {
        MultiBorrowContext::new(self)
    }

    /// 移除对象池中所有元素。
    #[inline]
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        self.free_stack.clear();
        self.records.drain(..).filter_map(|mut r| r.payload.take())
    }

    fn end(&self) -> *const PoolRecord<T, P> {
        unsafe { self.records.as_ptr().add(self.records.len()) }
    }

    fn begin(&self) -> *const PoolRecord<T, P> {
        self.records.as_ptr()
    }

    #[inline]
    pub fn handle_of(&self, ptr: &T) -> Handle<T> {
        let begin = self.begin() as usize;
        let end = self.end() as usize;
        let val = ptr as *const T as usize;
        if val >= begin && val < end {
            let record_size = std::mem::size_of::<PoolRecord<T>>();
            let record_location = (val - core::mem::offset_of!(PoolRecord<T>, payload)) - begin;
            if record_location.is_multiple_of(record_size) {
                let index = record_location / record_size;
                let index = u32::try_from(index).expect("Index overflowed u32");
                return self.handle_from_index(index);
            }
        }
        Handle::NONE
    }
}

impl<T, P> Pool<T, P>
where
    T: Reflect,
    P: PayloadContainer<Element = T> + 'static,
{
    /// 尝试借用对象并获取其指定类型的分量（共享引用）。
    #[inline]
    pub fn try_get_or_field_ref<C>(&self, handle: Handle<T>) -> Result<&C, PoolError>
    where
        C: Reflect,
    {
        self.try_borrow(handle).and_then(|n| {
            (n as &dyn Reflect)
                .self_or_field_ref::<C>()
                .ok_or(PoolError::NoSuchField(handle.into()))
        })
    }

    /// 尝试借用对象并获取其指定类型的分量（可变引用）。
    #[inline]
    pub fn try_get_or_field_mut<C>(&mut self, handle: Handle<T>) -> Result<&mut C, PoolError>
    where
        C: Reflect,
    {
        self.try_borrow_mut(handle).and_then(|n| {
            (n as &mut dyn Reflect)
                .self_or_field_mut::<C>()
                .ok_or(PoolError::NoSuchField(handle.into()))
        })
    }
}

impl<T> FromIterator<T> for Pool<T>
where
    T: 'static,
{
    #[inline]
    fn from_iter<C: IntoIterator<Item = T>>(iter: C) -> Self {
        let iter = iter.into_iter();
        let (lower_bound, upper_bound) = iter.size_hint();
        let lower_bound = u32::try_from(lower_bound).expect("lower_bound overflowed u32");
        let upper_bound =
            upper_bound.map(|b| u32::try_from(b).expect("upper_bound overflowed u32"));
        let mut pool = Self::with_capacity(upper_bound.unwrap_or(lower_bound));
        for v in iter {
            let _ = pool.spawn(v);
        }
        pool
    }
}

impl<T, U, Container> Index<Handle<U>> for Pool<T, Container>
where
    T: 'static,
    U: ObjectOrVariant<T>,
    Container: PayloadContainer<Element = T> + 'static,
{
    type Output = U;
    #[inline]
    fn index(&self, index: Handle<U>) -> &Self::Output {
        self.try_get(index).expect("句柄必须有效！")
    }
}

impl<T, U, Container> IndexMut<Handle<U>> for Pool<T, Container>
where
    T: 'static,
    U: ObjectOrVariant<T>,
    Container: PayloadContainer<Element = T> + 'static,
{
    #[inline]
    fn index_mut(&mut self, index: Handle<U>) -> &mut Self::Output {
        self.try_get_mut(index).expect("句柄必须有效！")
    }
}

impl<'a, T, P> IntoIterator for &'a Pool<T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    type Item = &'a T;
    type IntoIter = PoolIterator<'a, T, P>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T, P> IntoIterator for &'a mut Pool<T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    type Item = &'a mut T;
    type IntoIter = PoolIteratorMut<'a, T, P>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

pub struct PoolIterator<'a, T, P>
where
    P: PayloadContainer<Element = T>,
{
    ptr: *const PoolRecord<T, P>,
    end: *const PoolRecord<T, P>,
    marker: PhantomData<&'a T>,
}

impl<'a, T, P> Iterator for PoolIterator<'a, T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            while self.ptr != self.end {
                let current = &*self.ptr;
                if let Some(payload) = current.payload.as_ref() {
                    self.ptr = self.ptr.offset(1);
                    return Some(payload);
                }
                self.ptr = self.ptr.offset(1);
            }

            None
        }
    }
}

pub struct PoolPairIterator<'a, T, P: PayloadContainer<Element = T>> {
    pool: &'a Pool<T, P>,
    current: usize,
}

impl<'a, T, P> Iterator for PoolPairIterator<'a, T, P>
where
    P: PayloadContainer<Element = T>,
{
    type Item = (Handle<T>, &'a T);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            {
                let record = self.pool.records.get(self.current)?;
                if let Some(payload) = record.payload.as_ref() {
                    let handle = Handle::new(self.current as u32, record.generation);
                    self.current += 1;
                    return Some((handle, payload));
                }
                self.current += 1;
            }
        }
    }
}

pub struct PoolIteratorMut<'a, T, P>
where
    P: PayloadContainer<Element = T>,
{
    ptr: *mut PoolRecord<T, P>,
    end: *mut PoolRecord<T, P>,
    marker: PhantomData<&'a mut T>,
}

impl<'a, T, P> Iterator for PoolIteratorMut<'a, T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    type Item = &'a mut T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            while self.ptr != self.end {
                let current = &mut *self.ptr;
                if let Some(payload) = current.payload.as_mut() {
                    self.ptr = self.ptr.offset(1);
                    return Some(payload);
                }
                self.ptr = self.ptr.offset(1);
            }

            None
        }
    }
}

pub struct PoolPairIteratorMut<'a, T, P>
where
    P: PayloadContainer<Element = T>,
{
    ptr: *mut PoolRecord<T, P>,
    end: *mut PoolRecord<T, P>,
    marker: PhantomData<&'a mut T>,
    current: usize,
}

impl<'a, T, P> Iterator for PoolPairIteratorMut<'a, T, P>
where
    P: PayloadContainer<Element = T> + 'static,
{
    type Item = (Handle<T>, &'a mut T);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            while self.ptr != self.end {
                let current = &mut *self.ptr;
                if let Some(payload) = current.payload.as_mut() {
                    let handle = Handle::new(self.current as u32, current.generation);
                    self.ptr = self.ptr.offset(1);
                    self.current += 1;
                    return Some((handle, payload));
                }
                self.ptr = self.ptr.offset(1);
                self.current += 1;
            }

            None
        }
    }
}

#[cfg(test)]
mod test {
    use crate::pool::PoolError;
    use crate::{
        pool::{AtomicHandle, Handle, Pool, PoolRecord, INVALID_GENERATION},
        visitor::{Visit, Visitor},
    };

    #[test]
    fn pool_sanity_tests() {
        let mut pool: Pool<String> = Pool::new();
        let foobar_handle = pool.spawn(String::from("Foobar"));
        assert_eq!(foobar_handle.index, 0);
        assert_ne!(foobar_handle.generation, INVALID_GENERATION);
        let foobar_handle_copy = foobar_handle;
        assert_eq!(foobar_handle.index, foobar_handle_copy.index);
        assert_eq!(foobar_handle.generation, foobar_handle_copy.generation);
        let baz_handle = pool.spawn(String::from("Baz"));
        assert_eq!(pool.borrow(foobar_handle), "Foobar");
        assert_eq!(pool.borrow(baz_handle), "Baz");
        pool.free(foobar_handle);
        assert!(!pool.is_valid_handle(foobar_handle_copy));
        assert!(pool.is_valid_handle(baz_handle));
        let at_foobar_index = pool.spawn(String::from("AtFoobarIndex"));
        assert_eq!(at_foobar_index.index, 0);
        assert_ne!(at_foobar_index.generation, INVALID_GENERATION);
        assert_eq!(pool.borrow(at_foobar_index), "AtFoobarIndex");
        let bar_handle = pool.spawn_with(|_handle| String::from("Bar"));
        assert_ne!(bar_handle.index, 0);
        assert_ne!(bar_handle.generation, INVALID_GENERATION);
        assert_eq!(pool.borrow(bar_handle), "Bar");
    }

    #[test]
    fn pool_iterator_mut_test() {
        let mut pool: Pool<String> = Pool::new();
        let foobar = pool.spawn("Foobar".to_string());
        let d = pool.spawn("Foo".to_string());
        pool.free(d);
        let baz = pool.spawn("Baz".to_string());
        for s in pool.iter() {
            println!("{s}");
        }
        for s in pool.iter_mut() {
            println!("{s}");
        }
        for s in &pool {
            println!("{s}");
        }
        for s in &mut pool {
            println!("{s}");
        }
        pool.free(foobar);
        pool.free(baz);
    }

    #[test]
    fn handle_of() {
        #[allow(dead_code)]
        struct Value {
            data: String,
        }

        let mut pool = Pool::<Value>::new();
        let foobar = pool.spawn(Value {
            data: "Foobar".to_string(),
        });
        let bar = pool.spawn(Value {
            data: "Bar".to_string(),
        });
        let baz = pool.spawn(Value {
            data: "Baz".to_string(),
        });
        assert_eq!(pool.handle_of(pool.borrow(foobar)), foobar);
        assert_eq!(pool.handle_of(pool.borrow(bar)), bar);
        assert_eq!(pool.handle_of(pool.borrow(baz)), baz);
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Payload;

    #[test]
    fn pool_test_spawn_at() {
        let mut pool = Pool::<Payload>::new();

        assert_eq!(pool.spawn_at(2, Payload), Ok(Handle::new(2, 1)));
        assert_eq!(pool.spawn_at(2, Payload), Err(Payload));
        assert_eq!(pool.records[0].payload.as_ref(), None);
        assert_eq!(pool.records[1].payload.as_ref(), None);
        assert_ne!(pool.records[2].payload.as_ref(), None);

        assert_eq!(pool.spawn_at(2, Payload), Err(Payload));

        pool.free(Handle::new(2, 1));

        assert_eq!(pool.spawn_at(2, Payload), Ok(Handle::new(2, 2)));

        assert_eq!(pool.spawn(Payload), Handle::<Payload>::new(1, 2));
        assert_eq!(pool.spawn(Payload), Handle::<Payload>::new(0, 2));
    }

    #[test]
    fn pool_test_try_free() {
        let mut pool = Pool::<Payload>::new();

        assert_eq!(
            pool.try_free(Handle::NONE),
            Err(PoolError::InvalidIndex(Handle::<Payload>::NONE.index))
        );
        assert_eq!(pool.free_stack.len(), 0);

        let handle = pool.spawn(Payload);
        assert_eq!(pool.try_free(handle), Ok(Payload));
        assert_eq!(pool.free_stack.len(), 1);
        assert_eq!(pool.try_free(handle), Err(PoolError::Empty(handle.into())));
        assert_eq!(pool.free_stack.len(), 1);
    }

    #[test]
    fn visit_for_pool_record() {
        let mut p = PoolRecord::<u32>::default();
        let mut visitor = Visitor::default();

        assert!(p.visit("name", &mut visitor).is_ok());
    }

    #[test]
    fn visit_for_pool() {
        let mut p = Pool::<u32>::default();
        let mut visitor = Visitor::default();

        assert!(p.visit("name", &mut visitor).is_ok());
    }

    #[test]
    fn default_for_pool() {
        assert_eq!(Pool::default(), Pool::<u32>::new());
    }

    #[test]
    fn pool_with_capacity() {
        let p = Pool::<u32>::with_capacity(1);
        assert_eq!(p.records, Vec::with_capacity(1));
        assert_eq!(p.free_stack, Vec::new())
    }

    #[test]
    fn pool_try_borrow() {
        let mut pool = Pool::<Payload>::new();
        let a = pool.spawn(Payload);
        let b = Handle::<Payload>::default();

        assert_eq!(pool.try_borrow(a), Ok(&Payload));
        assert_eq!(
            pool.try_borrow(b),
            Err(PoolError::InvalidGeneration(b.generation))
        );
    }

    #[test]
    fn pool_borrow_two_mut() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(1);
        let b = pool.spawn(2);
        let (a, b) = pool.borrow_two_mut((a, b));

        assert_eq!(a, &mut 1);
        assert_eq!(b, &mut 2);
    }

    #[test]
    fn pool_borrow_three_mut() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(1);
        let b = pool.spawn(2);
        let c = pool.spawn(3);
        let (a, b, c) = pool.borrow_three_mut((a, b, c));

        assert_eq!(a, &mut 1);
        assert_eq!(b, &mut 2);
        assert_eq!(c, &mut 3);
    }

    #[test]
    fn pool_borrow_four_mut() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(1);
        let b = pool.spawn(2);
        let c = pool.spawn(3);
        let d = pool.spawn(4);
        let (a, b, c, d) = pool.borrow_four_mut((a, b, c, d));

        assert_eq!(a, &mut 1);
        assert_eq!(b, &mut 2);
        assert_eq!(c, &mut 3);
        assert_eq!(d, &mut 4);
    }

    #[test]
    fn pool_try_borrow_dependant_mut() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let b = pool.spawn(5);

        assert_eq!(
            pool.try_borrow_dependant_mut(a, |_| b),
            (Ok(&mut 42), Ok(&mut 5))
        );

        assert_eq!(
            pool.try_borrow_dependant_mut(a, |_| a),
            (Ok(&mut 42), Err(PoolError::MutablyBorrowed(a.into())))
        );
    }

    #[test]
    fn pool_try_take_reserve() {
        let mut pool = Pool::<u32>::new();

        let a = Handle::<u32>::default();
        assert!(pool.try_take_reserve(a).is_err());

        let b = pool.spawn(42);

        let (ticket, payload) = pool.try_take_reserve(b).unwrap();
        assert_eq!(ticket.index, 0);
        assert_eq!(payload, 42);

        assert!(pool.try_take_reserve(a).is_err());
        assert!(pool.try_take_reserve(b).is_err());

        pool.forget_ticket(ticket);
    }

    #[test]
    fn pool_put_back() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let (ticket, value) = pool.take_reserve(a);
        let b = pool.put_back(ticket, value);

        assert_eq!(a, b);
    }

    #[test]
    fn pool_forget_ticket() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let (ticket, _) = pool.take_reserve(a);

        pool.forget_ticket(ticket);

        let b = pool.spawn(42);

        assert_eq!(a.index, b.index);
        assert_ne!(a.generation, b.generation);
    }

    #[test]
    fn pool_get_capacity() {
        let mut pool = Pool::<u32>::new();
        let _ = pool.spawn(42);
        let _ = pool.spawn(5);

        assert_eq!(pool.get_capacity(), 2);
    }

    #[test]
    fn pool_clear() {
        let mut pool = Pool::<u32>::new();
        let _ = pool.spawn(42);

        assert!(!pool.records.is_empty());

        pool.clear();

        assert!(pool.records.is_empty());
        assert!(pool.free_stack.is_empty());
    }

    #[test]
    fn pool_at_mut() {
        let mut pool = Pool::<u32>::new();
        let _ = pool.spawn(42);

        assert_eq!(pool.at_mut(0), Ok(&mut 42));
        assert_eq!(pool.at_mut(1), Err(PoolError::InvalidIndex(1)));
    }

    #[test]
    fn pool_at() {
        let mut pool = Pool::<u32>::new();
        let _ = pool.spawn(42);

        assert_eq!(pool.at(0), Ok(&42));
        assert_eq!(pool.at(1), Err(PoolError::InvalidIndex(1)));
    }

    #[test]
    fn pool_handle_from_index() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);

        assert_eq!(pool.handle_from_index(0), a);
        assert_eq!(pool.handle_from_index(1), Handle::<u32>::NONE);
    }

    #[test]
    fn pool_alive_count() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let _ = pool.spawn(5);
        let (ticket, _) = pool.take_reserve(a);
        pool.forget_ticket(ticket);

        assert_eq!(pool.alive_count(), 1);
    }

    #[test]
    fn pool_total_count() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let _ = pool.spawn(5);
        let (ticket, _) = pool.take_reserve(a);

        assert_eq!(pool.total_count(), 2);

        pool.forget_ticket(ticket);
    }

    #[test]
    fn pool_replace() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let b = Handle::<u32>::new(1, 1);

        assert_eq!(pool.replace(a, 5), Some(42));
        assert_eq!(pool.replace(b, 5), None);
    }

    #[test]
    fn pool_pair_iter() {
        let pool = Pool::<u32>::new();

        let iter = pool.pair_iter();

        assert_eq!(iter.pool, &pool);
        assert_eq!(iter.current, 0);
    }

    #[test]
    fn pool_pair_iter_mut() {
        let mut pool = Pool::<u32>::new();
        let _ = pool.spawn(42);

        let iter = pool.pair_iter_mut();

        assert_eq!(iter.current, 0);
        assert_eq!(iter.ptr, pool.records.as_mut_ptr());
    }

    #[test]
    fn index_for_pool() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let b = pool.spawn(5);

        assert_eq!(pool[a], 42);
        assert_eq!(pool[b], 5);
    }

    #[test]
    fn index_mut_for_pool() {
        let mut pool = Pool::<u32>::new();
        let a = pool.spawn(42);
        let b = pool.spawn(5);

        pool[a] = 15;

        assert_eq!(pool[a], 15);
        assert_eq!(pool[b], 5);
    }

    #[test]
    fn test_atomic_handle() {
        let handle = AtomicHandle::new(123, 321);
        assert!(handle.is_some());
        assert_eq!(handle.index(), 123);
        assert_eq!(handle.generation(), 321);

        let handle = AtomicHandle::default();
        assert!(handle.is_none());
    }

    #[test]
    fn test_generate_free_handles() {
        let mut pool = Pool::<u32>::new();

        let _ = pool.spawn(42);
        let b = pool.spawn(5);
        let _ = pool.spawn(228);

        pool.free(b);

        let h0 = Handle::<u32>::new(1, 2);
        let h1 = Handle::<u32>::new(3, 1);
        let h2 = Handle::<u32>::new(4, 1);
        let h3 = Handle::<u32>::new(5, 1);
        let h4 = Handle::<u32>::new(6, 1);

        let free_handles = pool.generate_free_handles(5);
        assert_eq!(free_handles, [h0, h1, h2, h3, h4]);

        // Spawn something on the generated handles.
        for (i, handle) in free_handles.into_iter().enumerate() {
            let instance_handle = pool.spawn_at_handle(handle, i as u32);
            assert_eq!(instance_handle, Ok(handle));
        }

        assert_eq!(pool[h0], 0);
        assert_eq!(pool[h1], 1);
        assert_eq!(pool[h2], 2);
        assert_eq!(pool[h3], 3);
        assert_eq!(pool[h4], 4);
    }

    #[test]
    fn test_spawn_consistent_with_generate_free_handles() {
        let mut pool = Pool::<u32>::new();

        let _ = pool.spawn(42);
        let b0 = pool.spawn(5);
        let b1 = pool.spawn(6);
        let b2 = pool.spawn(7);
        let _ = pool.spawn(228);

        pool.free(b0);
        pool.free(b1);
        pool.free(b2);

        let free_handles = pool.generate_free_handles(5);

        let mut spawn_handles = Vec::new();
        for i in 0..5 {
            spawn_handles.push(pool.spawn(i));
        }

        assert_eq!(free_handles, spawn_handles);
    }
}