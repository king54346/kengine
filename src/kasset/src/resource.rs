//! 资源数据 trait 与类型化资源句柄。

use crate::{
    error::LoadError,
    state::{ResourceState, UntypedResource},
};
use kcore::uuid::Uuid;
use parking_lot::{MappedMutexGuard, MutexGuard};
use std::{
    any::Any,
    fmt::{self, Debug},
    future::Future,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::Deref,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

/// 可作为资源存储的数据。
///
/// `type_uuid` 用于跨运行期识别资源类型（序列化、加载器匹配），
/// 语义与 [`kcore::reflect`] 的 `type_uuid` 一致。
pub trait ResourceData: Any + Debug + Send + Sync {
    /// 该资源类型的稳定唯一标识。
    fn type_uuid(&self) -> Uuid;
}

/// 带类型的资源句柄。
///
/// 克隆廉价，所有克隆共享同一份底层数据。
pub struct Resource<T: ResourceData> {
    untyped: UntypedResource,
    _marker: PhantomData<T>,
}

impl<T: ResourceData> Clone for Resource<T> {
    fn clone(&self) -> Self {
        Self {
            untyped: self.untyped.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: ResourceData> Resource<T> {
    /// 从非类型化句柄构造。类型是否匹配要等资源就绪后才能验证。
    pub fn from_untyped(untyped: UntypedResource) -> Self {
        Self {
            untyped,
            _marker: PhantomData,
        }
    }

    /// 用现成数据构造一个已就绪的资源。
    pub fn new_ok(path: impl Into<std::path::PathBuf>, data: T) -> Self {
        Self::from_untyped(UntypedResource::new_ok(path, Box::new(data)))
    }

    /// 取出底层的非类型化句柄。
    pub fn untyped(&self) -> &UntypedResource {
        &self.untyped
    }

    /// 资源路径。
    pub fn path(&self) -> &Path {
        self.untyped.path()
    }

    /// 是否仍在加载。
    pub fn is_loading(&self) -> bool {
        self.untyped.is_loading()
    }

    /// 是否已就绪。注意：类型不匹配时状态仍是 `Ok`，但 [`Resource::data_ref`] 会返回 [`None`]。
    pub fn is_ok(&self) -> bool {
        self.untyped.is_ok()
    }

    /// 是否加载失败。
    pub fn is_failed(&self) -> bool {
        self.untyped.is_failed()
    }

    /// 失败原因。
    pub fn error(&self) -> Option<LoadError> {
        self.untyped.error()
    }

    /// 访问资源数据。
    ///
    /// 资源仍在加载、加载失败、或实际类型与 `T` 不符时返回 [`None`]。
    /// 返回的守卫持有内部锁，请勿长期持有。
    pub fn data_ref(&self) -> Option<ResourceDataRef<'_, T>> {
        let guard = self.untyped.state();
        MutexGuard::try_map(guard, |state| match state {
            ResourceState::Ok { data } => (data.as_mut() as &mut dyn Any).downcast_mut::<T>(),
            _ => None,
        })
        .ok()
        .map(|guard| ResourceDataRef { guard })
    }
}

impl<T: ResourceData> Debug for Resource<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Resource<{}>({:?})", std::any::type_name::<T>(), self.untyped)
    }
}

impl<T: ResourceData> PartialEq for Resource<T> {
    fn eq(&self, other: &Self) -> bool {
        self.untyped == other.untyped
    }
}

impl<T: ResourceData> Eq for Resource<T> {}

impl<T: ResourceData> Hash for Resource<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.untyped.hash(state)
    }
}

/// 等待资源加载完成。
///
/// ```no_run
/// # use kasset::prelude::*;
/// # fn demo<T: ResourceData>(resource: Resource<T>) {
/// let ready = ktask::block_on(resource);
/// # }
/// ```
impl<T: ResourceData> Future for Resource<T> {
    type Output = Result<Resource<T>, LoadError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // add_waker 在资源仍处于 Pending 时注册回调并返回 true。
        if self.untyped.add_waker(cx.waker()) {
            return Poll::Pending;
        }

        match self.untyped.error() {
            Some(error) => Poll::Ready(Err(error)),
            None => Poll::Ready(Ok(self.clone())),
        }
    }
}

/// [`Resource::data_ref`] 返回的守卫，解引用即可拿到资源数据。
pub struct ResourceDataRef<'a, T: ResourceData> {
    guard: MappedMutexGuard<'a, T>,
}

impl<T: ResourceData> Deref for ResourceDataRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ResourceData + Debug> Debug for ResourceDataRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&*self.guard, f)
    }
}
