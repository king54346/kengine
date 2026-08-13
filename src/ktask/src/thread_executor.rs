use core::marker::PhantomData;
use std::thread::{self, ThreadId};

use crate::executor::Executor;
use async_task::Task;
use futures_lite::Future;

/// An executor that can only be ticked on the thread it was instantiated on. But
/// can spawn `Send` tasks from other threads.
///
/// # Example
/// ```
/// # use std::sync::{Arc, atomic::{AtomicI32, Ordering}};
/// use ktask::ThreadExecutor;
///
/// let thread_executor = ThreadExecutor::new();
/// let count = Arc::new(AtomicI32::new(0));
///
/// // create some owned values that can be moved into another thread
/// let count_clone = count.clone();
///
/// std::thread::scope(|scope| {
///     scope.spawn(|| {
///         // we cannot get the ticker from another thread
///         let not_thread_ticker = thread_executor.ticker();
///         assert!(not_thread_ticker.is_none());
///
///         // but we can spawn tasks from another thread
///         thread_executor.spawn(async move {
///             count_clone.fetch_add(1, Ordering::Relaxed);
///         }).detach();
///     });
/// });
///
/// // the tasks do not make progress unless the executor is manually ticked
/// assert_eq!(count.load(Ordering::Relaxed), 0);
///
/// // tick the ticker until ktask finishes
/// let thread_ticker = thread_executor.ticker().unwrap();
/// thread_ticker.try_tick();
/// assert_eq!(count.load(Ordering::Relaxed), 1);
/// ```
#[derive(Debug)]
pub struct ThreadExecutor<'task> {
    executor: Executor<'task>,
    // 记录创建该执行器时所在的线程 ID，用来在 ticker() 里做归属检查
    thread_id: ThreadId,
}

impl<'task> Default for ThreadExecutor<'task> {
    fn default() -> Self {
        Self {
            executor: Executor::new(),
            thread_id: thread::current().id(),
        }
    }
}

impl<'task> ThreadExecutor<'task> {
    /// create a new [`ThreadExecutor`]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a ktask on the thread executor
    // 任意线程都可以调用 spawn 把任务塞进队列（future 要求 Send），
    // 但这些任务不会自动执行，必须由持有该执行器的线程调用 ticker().tick() 来推进。
    pub fn spawn<T: Send + 'task>(
        &self,
        future: impl Future<Output = T> + Send + 'task,
    ) -> Task<T> {
        self.executor.spawn(future)
    }

    /// Gets the [`ThreadExecutorTicker`] for this executor.
    /// Use this to tick the executor.
    /// It only returns the ticker if it's on the thread the executor was created on
    /// and returns `None` otherwise.
    // 只有在"创建执行器的那个线程"上调用才能拿到 Ticker（否则返回 None），
    // 这是保证 ThreadExecutor 只在归属线程上被推进的关键检查。
    // Ticker 内部用 PhantomData<*const ()> 标记为 !Send/!Sync，防止被传到其他线程使用。
    pub fn ticker<'ticker>(&'ticker self) -> Option<ThreadExecutorTicker<'task, 'ticker>> {
        if thread::current().id() == self.thread_id {
            return Some(ThreadExecutorTicker {
                executor: self,
                _marker: PhantomData,
            });
        }
        None
    }

    /// Returns true if `self` and `other`'s executor is same
    pub fn is_same(&self, other: &Self) -> bool {
        core::ptr::eq(self, other)
    }
}

/// Used to tick the [`ThreadExecutor`]. The executor does not
/// make progress unless it is manually ticked on the thread it was
/// created on.
#[derive(Debug)]
pub struct ThreadExecutorTicker<'task, 'ticker> {
    executor: &'ticker ThreadExecutor<'task>,
    // make type not send or sync
    _marker: PhantomData<*const ()>,
}

impl<'task, 'ticker> ThreadExecutorTicker<'task, 'ticker> {
    /// Tick the thread executor.
    pub async fn tick(&self) {
        self.executor.executor.tick().await;
    }

    /// Synchronously try to tick a ktask on the executor.
    /// Returns false if does not find a ktask to tick.
    pub fn try_tick(&self) -> bool {
        self.executor.executor.try_tick()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_ticker() {
        let executor = Arc::new(ThreadExecutor::new());
        let ticker = executor.ticker();
        assert!(ticker.is_some());

        thread::scope(|s| {
            s.spawn(|| {
                let ticker = executor.ticker();
                assert!(ticker.is_none());
            });
        });
    }
}