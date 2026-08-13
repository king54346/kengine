use std::{cell::{RefCell, Cell}, future::Future, marker::PhantomData, mem, string::String, thread_local, vec::Vec};
use std::sync::Arc;

use crate::executor::LocalExecutor;
use crate::executor::LocalExecutor as Executor;
use crate::{block_on, Task};

thread_local! {
    static LOCAL_EXECUTOR: Executor<'static> = const { Executor::new() };
}

/// Used to create a [`TaskPool`].
#[derive(Debug, Default, Clone)]
pub struct TaskPoolBuilder {}

// 这是一个"空壳"结构体：单线程模式下没有多线程概念，
// 保留这个类型只是为了让上层代码（如 task_pool.rs 里 scope 相关签名）在两种 feature 下都能用同一套 API。
/// This is a dummy struct for wasm support to provide the same api as with the multithreaded
/// ktask pool. In the case of the multithreaded ktask pool this struct is used to spawn
/// tasks on a specific thread. But the wasm ktask pool just calls
/// `wasm_bindgen_futures::spawn_local` for spawning which just runs tasks on the main thread
/// and so the [`ThreadExecutor`] does nothing.
#[derive(Default)]
pub struct ThreadExecutor<'a>(PhantomData<&'a ()>);
impl<'a> ThreadExecutor<'a> {
    /// Creates a new `ThreadExecutor`
    pub fn new() -> Self {
        Self::default()
    }
}

impl TaskPoolBuilder {
    /// Creates a new `TaskPoolBuilder` instance
    pub fn new() -> Self {
        Self::default()
    }

    /// No op on the single threaded ktask pool
    pub fn num_threads(self, _num_threads: usize) -> Self {
        self
    }

    /// No op on the single threaded ktask pool
    pub fn stack_size(self, _stack_size: usize) -> Self {
        self
    }

    /// No op on the single threaded ktask pool
    pub fn thread_name(self, _thread_name: String) -> Self {
        self
    }

    /// No op on the single threaded ktask pool
    pub fn on_thread_spawn(self, _f: impl Fn() + Send + Sync + 'static) -> Self {
        self
    }

    /// No op on the single threaded ktask pool
    pub fn on_thread_destroy(self, _f: impl Fn() + Send + Sync + 'static) -> Self {
        self
    }

    /// Creates a new [`TaskPool`]
    pub fn build(self) -> TaskPool {
        TaskPool::new_internal()
    }
}

/// A thread pool for executing tasks. Tasks are futures that are being automatically driven by
/// the pool on threads owned by the pool. In this case - main thread only.
#[derive(Debug, Default, Clone)]
pub struct TaskPool {}

impl TaskPool {
    /// Just create a new `ThreadExecutor` for wasm
    pub fn get_thread_executor() -> Arc<ThreadExecutor<'static>> {
        Arc::new(ThreadExecutor::new())
    }

    /// Create a `TaskPool` with the default configuration.
    pub fn new() -> Self {
        TaskPoolBuilder::new().build()
    }

    fn new_internal() -> Self {
        Self {}
    }

    /// Return the number of threads owned by the ktask pool
    pub fn thread_num(&self) -> usize {
        1
    }

    /// Allows spawning non-`'static` futures on the thread pool. The function takes a callback,
    /// passing a scope object into it. The scope object provided to the callback can be used
    /// to spawn tasks. This function will await the completion of all tasks before returning.
    ///
    /// This is similar to `rayon::scope` and `crossbeam::scope`
    pub fn scope<'env, F, T>(&self, f: F) -> Vec<T>
    where
        F: for<'scope> FnOnce(&'scope mut Scope<'scope, 'env, T>),
        T: Send + 'static,
    {
        self.scope_with_executor(false, None, f)
    }

    /// Allows spawning non-`'static` futures on the thread pool. The function takes a callback,
    /// passing a scope object into it. The scope object provided to the callback can be used
    /// to spawn tasks. This function will await the completion of all tasks before returning.
    ///
    /// This is similar to `rayon::scope` and `crossbeam::scope`
    #[expect(unsafe_code, reason = "Required to transmute lifetimes.")]
    pub fn scope_with_executor<'env, F, T>(
        &self,
        _tick_task_pool_executor: bool,
        _thread_executor: Option<&ThreadExecutor>,
        f: F,
    ) -> Vec<T>
    where
        F: for<'scope> FnOnce(&'scope mut Scope<'scope, 'env, T>),
        T: Send + 'static,
    {
        // SAFETY: This safety comment applies to all references transmuted to 'env.
        // Any futures spawned with these references need to return before this function completes.
        // This is guaranteed because we drive all the futures spawned onto the Scope
        // to completion in this function. However, rust has no way of knowing this so we
        // transmute the lifetimes to 'env here to appease the compiler as it is unable to validate safety.
        // Any usages of the references passed into `Scope` must be accessed through
        // the transmuted reference for the rest of this function.

        // 和多线程版 task_pool.rs 里的技巧一样：用 transmute 把 'scope 引用伪装成 'env，
        // 骗过编译器，前提是本函数末尾一定会 block_on 等到 pending_tasks 归零才返回。
        let executor = LocalExecutor::new();
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let executor_ref: &'env LocalExecutor<'env> = unsafe { mem::transmute(&executor) };

        // results：按 spawn 顺序预留 slot（先 push None 占位），任务完成后再原地填入 Some(result)，
        // 这样即使任务是并发/交错完成的，最终收集结果的顺序也和 spawn 顺序一致。
        let results: RefCell<Vec<Option<T>>> = RefCell::new(Vec::new());
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let results_ref: &'env RefCell<Vec<Option<T>>> = unsafe { mem::transmute(&results) };

        // pending_tasks：还未完成的任务计数，归零即代表 scope 内所有任务都跑完了。
        let pending_tasks: Cell<usize> = Cell::new(0);
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let pending_tasks: &'env Cell<usize> = unsafe { mem::transmute(&pending_tasks) };

        let mut scope = Scope {
            executor_ref,
            pending_tasks,
            results_ref,
            scope: PhantomData,
            env: PhantomData,
        };

        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let scope_ref: &'env mut Scope<'_, 'env, T> = unsafe { mem::transmute(&mut scope) };

        f(scope_ref);

        // Wait until the scope is complete
        // 单线程场景没有其他线程帮忙推进任务，所以要显式 run executor 自己 tick，
        // 直到 pending_tasks 归零（每 tick 一轮就 yield_now 一次，把控制权交还给 executor 继续调度其他任务）。
        block_on(executor.run(async {
            while pending_tasks.get() != 0 {
                futures_lite::future::yield_now().await;
            }
        }));

        // 走到这里所有任务必然都已完成（pending_tasks == 0），所以 unwrap 是安全的。
        results
            .take()
            .into_iter()
            .map(|result| result.unwrap())
            .collect()
    }

    /// Spawns a static future onto the thread pool. The returned Task is a future, which can be polled
    /// to retrieve the output of the original future. Dropping the ktask will attempt to cancel it.
    /// It can also be "detached", allowing it to continue running without having to be polled by the
    /// end-user.
    ///
    /// If the provided future is non-`Send`, [`TaskPool::spawn_local`] should be used instead.
    pub fn spawn<T>(
        &self,
        future: impl Future<Output = T> + 'static + MaybeSend + MaybeSync,
    ) -> Task<T>
    where
        T: 'static + MaybeSend + MaybeSync,
    {
        LOCAL_EXECUTOR.with(|executor| {
            let task = executor.spawn(future);
            // Loop until all tasks are done
            while executor.try_tick() {}
            task
        })
    }

    /// Spawns a static future on the JS event loop. This is exactly the same as [`TaskPool::spawn`].
    pub fn spawn_local<T>(
        &self,
        future: impl Future<Output = T> + 'static + MaybeSend + MaybeSync,
    ) -> Task<T>
    where
        T: 'static + MaybeSend + MaybeSync,
    {
        self.spawn(future)
    }

    /// Runs a function with the local executor. Typically used to tick
    /// the local executor on the main thread as it needs to share time with
    /// other things.
    ///
    /// ```
    /// use ktask::TaskPool;
    ///
    /// TaskPool::new().with_local_executor(|local_executor| {
    ///     local_executor.try_tick();
    /// });
    /// ```
    pub fn with_local_executor<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Executor) -> R,
    {
        LOCAL_EXECUTOR.with(f)
    }
}

/// A `TaskPool` scope for running one or more non-`'static` futures.
///
/// For more information, see [`TaskPool::scope`].
#[derive(Debug)]
pub struct Scope<'scope, 'env: 'scope, T> {
    executor_ref: &'scope LocalExecutor<'scope>,
    // The number of pending tasks spawned on the scope
    pending_tasks: &'scope Cell<usize>,
    // Vector to gather results of all futures spawned during scope run
    results_ref: &'env RefCell<Vec<Option<T>>>,

    // make `Scope` invariant over 'scope and 'env
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope, 'env, T: Send + 'env> Scope<'scope, 'env, T> {
    /// Spawns a scoped future onto the executor. The scope *must* outlive
    /// the provided future. The results of the future will be returned as a part of
    /// [`TaskPool::scope`]'s return value.
    ///
    /// On the single threaded ktask pool, it just calls [`Scope::spawn_on_scope`].
    ///
    /// For more information, see [`TaskPool::scope`].
    pub fn spawn<Fut: Future<Output = T> + 'scope + MaybeSend>(&self, f: Fut) {
        self.spawn_on_scope(f);
    }

    /// Spawns a scoped future onto the executor. The scope *must* outlive
    /// the provided future. The results of the future will be returned as a part of
    /// [`TaskPool::scope`]'s return value.
    ///
    /// On the single threaded ktask pool, it just calls [`Scope::spawn_on_scope`].
    ///
    /// For more information, see [`TaskPool::scope`].
    pub fn spawn_on_external<Fut: Future<Output = T> + 'scope + MaybeSend>(&self, f: Fut) {
        self.spawn_on_scope(f);
    }

    /// Spawns a scoped future that runs on the thread the scope called from. The
    /// scope *must* outlive the provided future. The results of the future will be
    /// returned as a part of [`TaskPool::scope`]'s return value.
    ///
    /// For more information, see [`TaskPool::scope`].
    pub fn spawn_on_scope<Fut: Future<Output = T> + 'scope + MaybeSend>(&self, f: Fut) {
        // increment the number of pending tasks
        let pending_tasks = self.pending_tasks;
        pending_tasks.update(|i| i + 1);

        // add a spot to keep the result, and record the index
        let results_ref = self.results_ref;
        let mut results = results_ref.borrow_mut();
        let task_number = results.len();
        results.push(None);
        drop(results);

        // create the job closure
        // 把原始 future 包一层：真正跑完后把结果写回预留的 slot，再把 pending_tasks 计数减一，
        // 这样外层 scope() 里那个 "while pending_tasks != 0" 循环才能感知到任务已完成。
        let f = async move {
            let result = f.await;

            // store the result in the allocated slot
            let mut results = results_ref.borrow_mut();
            results[task_number] = Some(result);
            drop(results);

            // decrement the pending tasks count
            pending_tasks.update(|i| i - 1);
        };

        // spawn the job itself
        // .detach()：不需要保留 Task 句柄，任务由 executor 自己驱动直到完成，
        // 结果通过上面的闭包写回 results_ref，不依赖调用方 poll 这个 Task。
        self.executor_ref.spawn(f).detach();
    }
}

// 单线程模式下不需要真正的 Send/Sync 约束（因为一切都在同一个线程跑），
// 这两个 trait 给任意类型自动实现，充当"占位/空约束"，
// 让 spawn 签名在单线程和多线程两种实现下保持一致的写法（多线程版用真正的 Send）。
pub trait MaybeSend {}
impl<T> MaybeSend for T {}

pub trait MaybeSync {}
impl<T> MaybeSync for T {}

#[cfg(test)]
mod test {
    use std::{time, thread};

    use super::*;

    /// This test creates a scope with a single ktask that goes to sleep for a
    /// nontrivial amount of time. At one point, the scope would (incorrectly)
    /// return early under these conditions, causing a crash.
    ///
    /// The correct behavior is for the scope to block until the receiver is
    /// woken by the external thread.
    #[test]
    fn scoped_spawn() {
        let (sender, receiver) = async_channel::unbounded();
        let task_pool = TaskPool {};
        let _thread = thread::spawn(move || {
            let duration = time::Duration::from_millis(50);
            thread::sleep(duration);
            let _ = sender.send(0);
        });
        task_pool.scope(|scope| {
            scope.spawn(async {
                receiver.recv().await
            });
        });
    }
}