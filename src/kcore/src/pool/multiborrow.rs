use super::{Handle, ObjectOrVariant, PayloadContainer, Pool, PoolError, RefCounter};
use std::{
    cell::RefCell,
    cmp::Ordering,
    fmt::{Debug, Formatter},
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

pub struct Ref<'a, 'b, T>
where
    T: ?Sized,
{
    data: &'a T,
    ref_counter: &'a RefCounter,
    phantom: PhantomData<&'b ()>,
}

impl<T> Debug for Ref<'_, '_, T>
where
    T: ?Sized + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.data, f)
    }
}

impl<T> Deref for Ref<'_, '_, T>
where
    T: ?Sized,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> Drop for Ref<'_, '_, T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        // SAFETY：安全，因为此引用的生命周期由借用检查器管理，
        // 不可能超过对应的 pool 记录的生命周期。
        unsafe {
            self.ref_counter.decrement();
        }
    }
}

pub struct RefMut<'a, 'b, T>
where
    T: ?Sized,
{
    data: &'a mut T,
    ref_counter: &'a RefCounter,
    phantom: PhantomData<&'b ()>,
}

impl<T> Debug for RefMut<'_, '_, T>
where
    T: ?Sized + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.data, f)
    }
}

impl<T> Deref for RefMut<'_, '_, T>
where
    T: ?Sized,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> DerefMut for RefMut<'_, '_, T>
where
    T: ?Sized,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

impl<T> Drop for RefMut<'_, '_, T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        // SAFETY：安全，原因同上。
        unsafe {
            self.ref_counter.increment();
        }
    }
}

/// 多借用上下文，允许同时获取对象池中任意数量的**唯一**引用。
pub struct MultiBorrowContext<'a, T, P = Option<T>>
where
    T: Sized,
    P: PayloadContainer<Element = T> + 'static,
{
    pool: &'a mut Pool<T, P>,
    free_indices: RefCell<Vec<u32>>,
}

impl<T, P> Drop for MultiBorrowContext<'_, T, P>
where
    T: Sized,
    P: PayloadContainer<Element = T> + 'static,
{
    fn drop(&mut self) {
        self.pool
            .free_stack
            .extend_from_slice(&self.free_indices.borrow())
    }
}

impl<'a, T, P> MultiBorrowContext<'a, T, P>
where
    T: Sized,
    P: PayloadContainer<Element = T> + 'static,
{
    #[inline]
    pub fn new(pool: &'a mut Pool<T, P>) -> Self {
        Self {
            pool,
            free_indices: Default::default(),
        }
    }

    #[inline]
    fn try_get_internal<'b: 'a, C, F>(
        &'b self,
        handle: Handle<T>,
        func: F,
    ) -> Result<Ref<'a, 'b, C>, PoolError>
    where
        C: ?Sized,
        F: FnOnce(&T) -> Result<&C, PoolError>,
    {
        let record = self.pool.records_get(handle.index)?;

        if handle.generation != record.generation {
            return Err(PoolError::InvalidGeneration(handle.generation));
        }

        let current_ref_count = unsafe { record.ref_counter.get() };
        if current_ref_count < 0 {
            return Err(PoolError::MutablyBorrowed(handle.into()));
        }

        // SAFETY：已通过前面的检查强制借用规则。
        let payload_container = unsafe { &*record.payload.0.get() };

        let Some(payload) = payload_container.as_ref() else {
            return Err(PoolError::Empty(handle.into()));
        };

        unsafe {
            record.ref_counter.increment();
        }

        Ok(Ref {
            data: func(payload)?,
            ref_counter: &record.ref_counter,
            phantom: PhantomData,
        })
    }

    /// 尝试获取指定句柄对应池元素的不可变引用。以下两种情况会失败：
    ///
    /// 1) 该元素已被可变借用——Rust 安全规则禁止。
    /// 2) 给定句柄无效。
    #[inline]
    pub fn try_get<'b, U>(&'b self, handle: Handle<U>) -> Result<Ref<'a, 'b, U>, PoolError>
    where
        'b: 'a,
        U: ObjectOrVariant<T>,
    {
        self.try_get_internal(handle.to_base(), |obj| {
            U::convert_to_dest_type(obj).ok_or(PoolError::InvalidType(handle.into()))
        })
    }

    #[inline]
    pub fn get<'b, U>(&'b self, handle: Handle<U>) -> Ref<'a, 'b, U>
    where
        'b: 'a,
        U: ObjectOrVariant<T>,
    {
        self.try_get(handle).unwrap()
    }

    #[inline]
    fn try_get_mut_internal<'b: 'a, C, F>(
        &'b self,
        handle: Handle<T>,
        func: F,
    ) -> Result<RefMut<'a, 'b, C>, PoolError>
    where
        C: ?Sized,
        F: FnOnce(&mut T) -> Result<&mut C, PoolError>,
    {
        let record = self.pool.records_get(handle.index)?;

        if handle.generation != record.generation {
            return Err(PoolError::InvalidGeneration(handle.generation));
        }

        // SAFETY: It is safe to access the counter because of borrow checker guarantees that
        // the record is alive.
        let current_ref_count = unsafe { record.ref_counter.get() };
        match current_ref_count.cmp(&0) {
            Ordering::Less => {
                return Err(PoolError::MutablyBorrowed(handle.into()));
            }
            Ordering::Greater => {
                return Err(PoolError::ImmutablyBorrowed(handle.into()));
            }
            _ => (),
        }

        // SAFETY：已通过前面的检查强制借用规则。
        let payload_container = unsafe { &mut *record.payload.0.get() };

        let Some(payload) = payload_container.as_mut() else {
            return Err(PoolError::Empty(handle.into()));
        };

        // SAFETY：借用检查器保证记录仍然存活，因此访问计数器是安全的。
        unsafe {
            record.ref_counter.decrement();
        }

        Ok(RefMut {
            data: func(payload)?,
            ref_counter: &record.ref_counter,
            phantom: PhantomData,
        })
    }

    #[inline]
    pub fn try_get_mut<'b, U>(&'b self, handle: Handle<U>) -> Result<RefMut<'a, 'b, U>, PoolError>
    where
        'b: 'a,
        U: ObjectOrVariant<T>,
    {
        self.try_get_mut_internal(handle.to_base(), |obj| {
            U::convert_to_dest_type_mut(obj).ok_or(PoolError::InvalidType(handle.into()))
        })
    }

    #[inline]
    pub fn get_mut<'b, U>(&'b self, handle: Handle<U>) -> RefMut<'a, 'b, U>
    where
        'b: 'a,
        U: ObjectOrVariant<T>,
    {
        self.try_get_mut(handle).unwrap()
    }

    #[inline]
    pub fn free(&self, handle: Handle<T>) -> Result<T, PoolError> {
        let record = self.pool.records_get(handle.index)?;

        if handle.generation != record.generation {
            return Err(PoolError::InvalidGeneration(handle.generation));
        }

        // 释放前记录不能处于借用状态。
        // SAFETY：借用检查器保证记录仍然存活，因此访问计数器是安全的。
        let current_ref_count = unsafe { record.ref_counter.get() };
        match current_ref_count.cmp(&0) {
            Ordering::Less => {
                return Err(PoolError::MutablyBorrowed(handle.into()));
            }
            Ordering::Greater => {
                return Err(PoolError::ImmutablyBorrowed(handle.into()));
            }
            _ => (),
        }

        // SAFETY：已通过前面的检查强制借用规则。
        let payload_container = unsafe { &mut *record.payload.0.get() };

        let Some(payload) = payload_container.take() else {
            return Err(PoolError::Empty(handle.into()));
        };

        self.free_indices.borrow_mut().push(handle.index);

        Ok(payload)
    }
}

#[cfg(test)]
mod test {
    use super::PoolError;
    use crate::pool::Pool;

    #[derive(PartialEq, Clone, Copy, Debug)]
    struct MyPayload(u32);

    #[test]
    fn test_multi_borrow_context() {
        let mut pool = Pool::<MyPayload>::new();

        let mut val_a = MyPayload(123);
        let mut val_b = MyPayload(321);
        let mut val_c = MyPayload(42);
        let val_d = MyPayload(666);

        let a = pool.spawn(val_a);
        let b = pool.spawn(val_b);
        let c = pool.spawn(val_c);
        let d = pool.spawn(val_d);

        pool.free(d);

        let ctx = pool.begin_multi_borrow();

        // 测试空槽位。
        {
            assert_eq!(
                ctx.try_get(d).as_deref(),
                Err(PoolError::Empty(d.into())).as_ref()
            );
            assert_eq!(
                ctx.try_get_mut(d).as_deref_mut(),
                Err(PoolError::Empty(d.into())).as_mut()
            );
        }

        // 测试对同一元素的多个不可变借用。
        {
            let ref_a_1 = ctx.try_get(a);
            let ref_a_2 = ctx.try_get(a);
            assert_eq!(ref_a_1.as_deref(), Ok(&val_a));
            assert_eq!(ref_a_2.as_deref(), Ok(&val_a));
        }

        // 测试先不可变借用、再尝试可变借用同一元素。
        {
            let ref_a_1 = ctx.try_get(a);
            assert_eq!(unsafe { ref_a_1.as_ref().unwrap().ref_counter.get() }, 1);
            let ref_a_2 = ctx.try_get(a);
            assert_eq!(unsafe { ref_a_2.as_ref().unwrap().ref_counter.get() }, 2);

            assert_eq!(ref_a_1.as_deref(), Ok(&val_a));
            assert_eq!(ref_a_2.as_deref(), Ok(&val_a));
            assert_eq!(
                ctx.try_get_mut(a).as_deref(),
                Err(PoolError::ImmutablyBorrowed(a.into())).as_ref()
            );

            drop(ref_a_1);
            drop(ref_a_2);

            let mut mut_ref_a_1 = ctx.try_get_mut(a);
            assert_eq!(mut_ref_a_1.as_deref_mut(), Ok(&mut val_a));

            assert_eq!(
                unsafe { mut_ref_a_1.as_ref().unwrap().ref_counter.get() },
                -1
            );
        }

        // 测试不可变借用与可变借用混合。
        {
            // 对同一元素取两个不可变引用。
            let ref_a_1 = ctx.try_get(a);
            let ref_a_2 = ctx.try_get(a);
            assert_eq!(ref_a_1.as_deref(), Ok(&val_a));
            assert_eq!(ref_a_2.as_deref(), Ok(&val_a));

            // 对另一元素取可变引用。
            let mut ref_b_1 = ctx.try_get_mut(b);
            let mut ref_b_2 = ctx.try_get_mut(b);
            assert_eq!(ref_b_1.as_deref_mut(), Ok(&mut val_b));
            assert_eq!(
                ref_b_2.as_deref_mut(),
                Err(PoolError::MutablyBorrowed(b.into())).as_mut()
            );

            let mut ref_c_1 = ctx.try_get_mut(c);
            let mut ref_c_2 = ctx.try_get_mut(c);
            assert_eq!(ref_c_1.as_deref_mut(), Ok(&mut val_c));
            assert_eq!(
                ref_c_2.as_deref_mut(),
                Err(PoolError::MutablyBorrowed(c.into())).as_mut()
            );
        }
    }
}
