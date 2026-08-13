use std::future::Future;

// 非 wasm 平台：多线程环境下 future 必须能跨线程传递，所以要求 Send。
/// Use [`ConditionalSend`] to mark an optional Send trait bound. Useful as on certain platforms (eg. Wasm),
/// futures aren't Send.
#[cfg(not(target_arch = "wasm32"))]
pub trait ConditionalSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> ConditionalSend for T {}

// wasm 平台是单线程的（浏览器 JS 事件循环），future 不需要 Send，所以这里放宽约束，
// 让同一套 API 在两个平台上都能编译通过。
/// Use [`ConditionalSend`] to mark an optional Send trait bound. Useful as on certain platforms (eg. Wasm),
/// futures aren't Send.
#[cfg(target_arch = "wasm32")]
pub trait ConditionalSend {}
#[cfg(target_arch = "wasm32")]
impl<T> ConditionalSend for T {}

/// Use [`ConditionalSendFuture`] for a future with an optional Send trait bound, as on certain platforms (eg. Wasm),
/// futures aren't Send.
pub trait ConditionalSendFuture: Future + ConditionalSend {}

// 给所有满足条件的类型自动实现 ConditionalSendFuture（blanket impl），无需手动实现。
impl<T: Future + ConditionalSend> ConditionalSendFuture for T {}

/// An owned and dynamically typed Future used when you can't statically type your result or need to add some indirection.
// 类型擦除后的堆分配 future：Pin<Box<dyn ...>>，用于返回值类型不方便写具体泛型的场景（如 trait 对象、递归 async）。
pub type BoxedFuture<'a, T> = std::pin::Pin<Box<dyn ConditionalSendFuture<Output = T> + 'a>>;

// Modules
mod executor; // 最底层：封装 async-executor / async-task
pub mod futures; // future 相关的小工具函数
mod iter; // 并行迭代器 ParallelIterator
mod slice; // 给切片提供并行 map 方法
mod usages; // 全局单例任务池（Compute/AsyncCompute/Io）

// 多线程模式下使用真正的线程池实现
#[cfg(feature = "multi_threaded")]
mod task_pool;
#[cfg(feature = "multi_threaded")]
mod thread_executor;

// 单线程模式（如 wasm，或关闭 multi_threaded feature）下用退化实现，
// 对外暴露相同的类型名（TaskPool/Scope/ThreadExecutor），调用方代码无需区分。
#[cfg(not(feature = "multi_threaded"))]
mod single_threaded_task_pool;

// Exports
pub use async_task::Task;
pub use iter::ParallelIterator;
pub use slice::{ParallelSlice, ParallelSliceMut};
pub use usages::{AsyncComputeTaskPool, ComputeTaskPool, IoTaskPool};
// 只有多线程模式才需要手动 tick 全局池子上的本地任务，单线程模式下没有这个函数（见 usages.rs）。
#[cfg(feature = "multi_threaded")]
pub use usages::tick_global_task_pools_on_main_thread;

pub use futures_lite;
pub use futures_lite::future::{block_on, poll_once};

// 根据 feature 二选一导出：外部代码统一 `use ktask::{TaskPool, Scope, ...}`，
// 不需要关心当前是多线程实现还是单线程实现。
#[cfg(feature = "multi_threaded")]
pub use task_pool::{Scope, TaskPool, TaskPoolBuilder};
#[cfg(feature = "multi_threaded")]
pub use thread_executor::{ThreadExecutor, ThreadExecutorTicker};

#[cfg(not(feature = "multi_threaded"))]
pub use single_threaded_task_pool::{Scope, TaskPool, TaskPoolBuilder, ThreadExecutor};

/// The tasks prelude.
///
/// This includes the most common types in this crate, re-exported for your convenience.
pub mod prelude {
    #[doc(hidden)]
    pub use crate::{
        block_on,
        iter::ParallelIterator,
        slice::{ParallelSlice, ParallelSliceMut},
        usages::{AsyncComputeTaskPool, ComputeTaskPool, IoTaskPool},
    };
}

/// Gets the logical CPU core count available to the current process.
///
/// This is identical to `std::thread::available_parallelism`, except
/// it will return a default value of 1 if it internally errors out.
///
/// This will always return at least 1.
pub fn available_parallelism() -> usize {
    // std 版本查询失败时（例如某些沙箱/容器环境）直接兜底为 1，
    // 保证调用方永远拿到一个可用的正数，不需要额外处理 Result。
    std::thread::available_parallelism()
        .map(std::num::NonZero::<usize>::get)
        .unwrap_or(1)
}
