//! Provides a fundamental executor primitive appropriate for the target platform
//! and feature set selected.
//! By default, the `async_executor` feature will be enabled, which will rely on
//! [`async-executor`] for the underlying implementation. This requires `std`
//!
//! [`async-executor`]: https://crates.io/crates/async-executor

use derive_more::{Deref, DerefMut};
use std::{
    fmt,
    panic::{RefUnwindSafe, UnwindSafe},
};

// 直接复用 async-executor 库的实现，这里只是起别名，方便后面包一层。
type ExecutorInner<'a> = async_executor::Executor<'a>;
type LocalExecutorInner<'a> = async_executor::LocalExecutor<'a>;

// FallibleTask：轮询会返回 Option（ktask 内部 panic 时得到 None，而不是直接 panic 扩散），
// 供 task_pool.rs 里 Scope::spawn 捕获子任务 panic 时使用。
#[cfg(feature = "multi_threaded")]
pub use async_task::FallibleTask;

/// Wrapper around a multi-threading-aware async executor.
/// Spawning will generally require tasks to be `Send` and `Sync` to allow multiple
/// threads to send/receive/advance tasks.
///
/// If you require an executor _without_ the `Send` and `Sync` requirements, consider
/// using [`LocalExecutor`] instead.
// 用 #[derive(Deref, DerefMut)] 让 Executor 直接暴露内部 async_executor::Executor 的所有方法
// （如 spawn/run/tick），外部代码用起来和直接用 async_executor 没有区别。
#[derive(Deref, DerefMut, Default)]
pub struct Executor<'a>(ExecutorInner<'a>);

/// Wrapper around a single-threaded async executor.
/// Spawning wont generally require tasks to be `Send` and `Sync`, at the cost of
/// this executor itself not being `Send` or `Sync`. This makes it unsuitable for
/// global statics.
///
/// If need to store an executor in a global static, or send across threads,
/// consider using [`Executor`] instead.
#[derive(Deref, DerefMut, Default)]
pub struct LocalExecutor<'a>(LocalExecutorInner<'a>);

impl Executor<'_> {
    /// Construct a new [`Executor`]
    #[expect(clippy::allow_attributes, reason = "This lint may not always trigger.")]
    #[allow(dead_code, reason = "not all feature flags require this function")]
    pub const fn new() -> Self {
        Self(ExecutorInner::new())
    }
}

impl LocalExecutor<'_> {
    /// Construct a new [`LocalExecutor`]
    #[expect(clippy::allow_attributes, reason = "This lint may not always trigger.")]
    #[allow(dead_code, reason = "not all feature flags require this function")]
    pub const fn new() -> Self {
        Self(LocalExecutorInner::new())
    }
}

impl UnwindSafe for Executor<'_> {}

impl RefUnwindSafe for Executor<'_> {}

impl UnwindSafe for LocalExecutor<'_> {}

impl RefUnwindSafe for LocalExecutor<'_> {}

impl fmt::Debug for Executor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor").finish()
    }
}

impl fmt::Debug for LocalExecutor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalExecutor").finish()
    }
}
