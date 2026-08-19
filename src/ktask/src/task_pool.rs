use std::{future::Future, marker::PhantomData, mem, panic::AssertUnwindSafe};
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    thread_local,
};

use crate::executor::FallibleTask;
use concurrent_queue::ConcurrentQueue;
use futures_lite::FutureExt;

use crate::{
    Task, block_on,
    thread_executor::{ThreadExecutor, ThreadExecutorTicker},
};

struct CallOnDrop(Option<Arc<dyn Fn() + Send + Sync + 'static>>);

impl Drop for CallOnDrop {
    fn drop(&mut self) {
        if let Some(call) = self.0.as_ref() {
            call();
        }
    }
}

/// Used to create a [`TaskPool`]
#[derive(Default)]
#[must_use]
pub struct TaskPoolBuilder {
    /// If set, we'll set up the thread pool to use at most `num_threads` threads.
    /// 否则使用系统的逻辑核心数
    num_threads: Option<usize>,
    /// If set, we'll use the given stack size rather than the system default
    stack_size: Option<usize>,
    /// Allows customizing the name of the threads - helpful for debugging. If set, threads will
    /// be named `<thread_name> (<thread_index>)`, i.e. `"MyThreadPool (2)"`.
    thread_name: Option<String>,
    /// Callback invoked when a thread is spawned
    /// 当一个线程被创建时调用的回调函数
    on_thread_spawn: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    /// Callback invoked when a thread is destroyed
    /// 当一个线程被销毁时调用的回调函数
    on_thread_destroy: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl TaskPoolBuilder {
    /// Creates a new [`TaskPoolBuilder`] instance
    pub fn new() -> Self {
        Self::default()
    }

    /// 重写 创建线程池的线程数。如果未设置，则默认为系统的逻辑核心数
    pub fn num_threads(mut self, num_threads: usize) -> Self {
        self.num_threads = Some(num_threads);
        self
    }

    /// Override the stack size of the threads created for the pool
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Override the name of the threads created for the pool. If set, threads will
    /// be named `<thread_name> (<thread_index>)`, i.e. `MyThreadPool (2)`
    pub fn thread_name(mut self, thread_name: String) -> Self {
        self.thread_name = Some(thread_name);
        self
    }

    /// Sets a callback that is invoked once for every created thread as it starts.
    ///
    /// This is called on the thread itself and has access to all thread-local storage.
    /// This will block running async tasks on the thread until the callback completes.
    pub fn on_thread_spawn(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        let arc = Arc::new(f);

        #[cfg(not(target_has_atomic = "ptr"))]
        #[expect(
            unsafe_code,
            reason = "unsized coercion is an unstable feature for non-std types"
        )]
        // SAFETY:
        // - Coercion from `impl Fn` to `dyn Fn` is valid
        // - `Arc::from_raw` receives a valid pointer from a previous call to `Arc::into_raw`
        let arc = unsafe {
            Arc::from_raw(Arc::into_raw(arc) as *const (dyn Fn() + Send + Sync + 'static))
        };

        self.on_thread_spawn = Some(arc);
        self
    }

    /// Sets a callback that is invoked once for every created thread as it terminates.
    /// 当一个线程被销毁时调用的回调函数
    ///
    /// This is called on the thread itself and has access to all thread-local storage.
    /// This will block thread termination until the callback completes.
    pub fn on_thread_destroy(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        let arc = Arc::new(f);

        #[cfg(not(target_has_atomic = "ptr"))]
        #[expect(
            unsafe_code,
            reason = "unsized coercion is an unstable feature for non-std types"
        )]
        // SAFETY:
        // - Coercion from `impl Fn` to `dyn Fn` is valid
        // - `Arc::from_raw` receives a valid pointer from a previous call to `Arc::into_raw`
        let arc = unsafe {
            Arc::from_raw(Arc::into_raw(arc) as *const (dyn Fn() + Send + Sync + 'static))
        };

        self.on_thread_destroy = Some(arc);
        self
    }

    /// Creates a new [`TaskPool`] based on the current options.
    pub fn build(self) -> TaskPool {
        TaskPool::new_internal(self)
    }
}

/// A thread pool for executing tasks.
///
/// While futures usually need to be polled to be executed, Bevy tasks are being
/// automatically driven by the pool on threads owned by the pool. The [`Task`]
/// future only needs to be polled in order to receive the result. (For that
/// purpose, it is often stored in a component or resource, see the
/// `async_compute` example.)
///
/// If the result is not required, one may also use [`Task::detach`] and the pool
/// will still execute a ktask, even if it is dropped.
#[derive(Debug)]
pub struct TaskPool {
    /// The executor for the pool.
    executor: Arc<crate::executor::Executor<'static>>,

    // The inner state of the pool.
    threads: Vec<JoinHandle<()>>,
    shutdown_tx: async_channel::Sender<()>,
}

impl TaskPool {
    // 每个线程各自独立的执行器：
    // LOCAL_EXECUTOR 用于 spawn_local（非 Send 任务，只在当前线程跑）；
    // THREAD_EXECUTOR 用于 scope()，代表"当前线程"这个执行域，可以被其他线程 spawn 任务进来。
    thread_local! {
        static LOCAL_EXECUTOR: crate::executor::LocalExecutor<'static> = const { crate::executor::LocalExecutor::new() };
        static THREAD_EXECUTOR: Arc<ThreadExecutor<'static>> = Arc::new(ThreadExecutor::new());
    }

    /// Each thread should only create one `ThreadExecutor`, otherwise, there are good chances they will deadlock
    pub fn get_thread_executor() -> Arc<ThreadExecutor<'static>> {
        Self::THREAD_EXECUTOR.with(Clone::clone)
    }

    /// Create a `TaskPool` with the default configuration.
    pub fn new() -> Self {
        TaskPoolBuilder::new().build()
    }

    fn new_internal(builder: TaskPoolBuilder) -> Self {
        // shutdown 用一个 channel 实现：TaskPool::drop 时关闭 sender，
        // 所有工作线程里 recv() 会立刻返回错误，从而跳出 tick_forever 循环、结束线程。
        let (shutdown_tx, shutdown_rx) = async_channel::unbounded::<()>();

        // 所有工作线程共享同一个全局 Executor（work-stealing：谁先跑完手头任务就去偷别的任务做）。
        let executor = Arc::new(crate::executor::Executor::new());

        let num_threads = builder
            .num_threads
            .unwrap_or_else(crate::available_parallelism);

        let threads = (0..num_threads)
            .map(|i| {
                let ex = Arc::clone(&executor);
                let shutdown_rx = shutdown_rx.clone();

                let thread_name = if let Some(thread_name) = builder.thread_name.as_deref() {
                    format!("{thread_name} ({i})")
                } else {
                    format!("TaskPool ({i})")
                };
                let mut thread_builder = thread::Builder::new().name(thread_name);

                if let Some(stack_size) = builder.stack_size {
                    thread_builder = thread_builder.stack_size(stack_size);
                }

                let on_thread_spawn = builder.on_thread_spawn.clone();
                let on_thread_destroy = builder.on_thread_destroy.clone();

                thread_builder
                    .spawn(move || {
                        TaskPool::LOCAL_EXECUTOR.with(|local_executor| {
                            if let Some(on_thread_spawn) = on_thread_spawn {
                                on_thread_spawn();
                                drop(on_thread_spawn);
                            }
                            // CallOnDrop 保证不管线程是正常退出还是 panic 退出，
                            // on_thread_destroy 回调都会在线程结束前执行一次。
                            let _destructor = CallOnDrop(on_thread_destroy);
                            // 外层 loop + catch_unwind：如果 ex.run() 内部某个任务 panic 导致
                            // executor.run 本身被 unwind 打断，就重新起一轮继续消化剩余任务，
                            // 避免一个任务 panic 就让整个工作线程死掉。
                            loop {
                                let res = std::panic::catch_unwind(|| {
                                    // tick_forever 不断推进本线程的 LOCAL_EXECUTOR（spawn_local 任务），
                                    // 同时 ex.run(...) 让本线程也去全局 Executor 里偷任务执行（work-stealing）。
                                    // 用 .or(shutdown_rx.recv()) 让 shutdown 信号能打断这个"永远运行"的 future。
                                    let tick_forever = async move {
                                        loop {
                                            local_executor.tick().await;
                                        }
                                    };
                                    block_on(ex.run(tick_forever.or(shutdown_rx.recv())))
                                });
                                if let Ok(value) = res {
                                    // Use unwrap_err because we expect a Closed error
                                    // 正常路径只会因为 shutdown_rx 收到关闭信号（Closed 错误）而返回，
                                    // 说明是主动关闭而不是内部逻辑错误，所以直接 unwrap_err 后跳出循环退出线程。
                                    value.unwrap_err();
                                    break;
                                }
                                // 如果是 catch_unwind 捕获到了 panic（Err 分支），则不 break，回到 loop 顶部重新运行，
                                // 让线程继续存活、继续消化任务队列。
                            }
                        });
                    })
                    .expect("Failed to spawn thread.")
            })
            .collect();

        Self {
            executor,
            threads,
            shutdown_tx,
        }
    }

    /// Return the number of threads owned by the ktask pool
    pub fn thread_num(&self) -> usize {
        self.threads.len()
    }

    /// Allows spawning non-`'static` futures on the thread pool. The function takes a callback,
    /// passing a scope object into it. The scope object provided to the callback can be used
    /// to spawn tasks. This function will await the completion of all tasks before returning.
    ///
    /// This is similar to [`thread::scope`] and `rayon::scope`.
    ///
    /// # Example
    ///
    /// ```
    /// use ktask::TaskPool;
    ///
    /// let pool = TaskPool::new();
    /// let mut x = 0;
    /// let results = pool.scope(|s| {
    ///     s.spawn(async {
    ///         // you can borrow the spawner inside a ktask and spawn tasks from within the ktask
    ///         s.spawn(async {
    ///             // borrow x and mutate it.
    ///             x = 2;
    ///             // return a value from the ktask
    ///             1
    ///         });
    ///         // return some other value from the first ktask
    ///         0
    ///     });
    /// });
    ///
    /// // The ordering of results is non-deterministic if you spawn from within tasks as above.
    /// // If you're doing this, you'll have to write your code to not depend on the ordering.
    /// assert!(results.contains(&0));
    /// assert!(results.contains(&1));
    ///
    /// // The ordering is deterministic if you only spawn directly from the closure function.
    /// let results = pool.scope(|s| {
    ///     s.spawn(async { 0 });
    ///     s.spawn(async { 1 });
    /// });
    /// assert_eq!(&results[..], &[0, 1]);
    ///
    /// // You can access x after scope runs, since it was only temporarily borrowed in the scope.
    /// assert_eq!(x, 2);
    /// ```
    ///
    /// # Lifetimes
    ///
    /// The [`Scope`] object takes two lifetimes: `'scope` and `'env`.
    ///
    /// The `'scope` lifetime represents the lifetime of the scope. That is the time during
    /// which the provided closure and tasks that are spawned into the scope are run.
    ///
    /// The `'env` lifetime represents the lifetime of whatever is borrowed by the scope.
    /// Thus this lifetime must outlive `'scope`.
    ///
    /// ```compile_fail
    /// use ktask::TaskPool;
    /// fn scope_escapes_closure() {
    ///     let pool = TaskPool::new();
    ///     let foo = Box::new(42);
    ///     pool.scope(|scope| {
    ///         std::thread::spawn(move || {
    ///             // UB. This could spawn on the scope after `.scope` returns and the internal Scope is dropped.
    ///             scope.spawn(async move {
    ///                 assert_eq!(*foo, 42);
    ///             });
    ///         });
    ///     });
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use ktask::TaskPool;
    /// fn cannot_borrow_from_closure() {
    ///     let pool = TaskPool::new();
    ///     pool.scope(|scope| {
    ///         let x = 1;
    ///         let y = &x;
    ///         scope.spawn(async move {
    ///             assert_eq!(*y, 1);
    ///         });
    ///     });
    /// }
    pub fn scope<'env, F, T>(&self, f: F) -> Vec<T>
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env, T>),
        T: Send + 'static,
    {
        Self::THREAD_EXECUTOR.with(|scope_executor| {
            self.scope_with_executor_inner(true, scope_executor, scope_executor, f)
        })
    }

    /// This allows passing an external executor to spawn tasks on. When you pass an external executor
    /// [`Scope::spawn_on_scope`] spawns is then run on the thread that [`ThreadExecutor`] is being ticked on.
    /// If [`None`] is passed the scope will use a [`ThreadExecutor`] that is ticked on the current thread.
    ///
    /// When `tick_task_pool_executor` is set to `true`, the multithreaded ktask stealing executor is ticked on the scope
    /// thread. Disabling this can be useful when finishing the scope is latency sensitive. Pulling tasks from
    /// global executor can run tasks unrelated to the scope and delay when the scope returns.
    ///
    /// See [`Self::scope`] for more details in general about how scopes work.
    pub fn scope_with_executor<'env, F, T>(
        &self,
        tick_task_pool_executor: bool,
        external_executor: Option<&ThreadExecutor>,
        f: F,
    ) -> Vec<T>
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env, T>),
        T: Send + 'static,
    {
        Self::THREAD_EXECUTOR.with(|scope_executor| {
            // If an `external_executor` is passed, use that. Otherwise, get the executor stored
            // in the `THREAD_EXECUTOR` thread local.
            if let Some(external_executor) = external_executor {
                self.scope_with_executor_inner(
                    tick_task_pool_executor,
                    external_executor,
                    scope_executor,
                    f,
                )
            } else {
                self.scope_with_executor_inner(
                    tick_task_pool_executor,
                    scope_executor,
                    scope_executor,
                    f,
                )
            }
        })
    }

    #[expect(unsafe_code, reason = "Required to transmute lifetimes.")]
    fn scope_with_executor_inner<'env, F, T>(
        &self,
        tick_task_pool_executor: bool,
        external_executor: &ThreadExecutor,
        scope_executor: &ThreadExecutor,
        f: F,
    ) -> Vec<T>
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env, T>),
        T: Send + 'static,
    {
        // 下面这段是本文件最"危险"也最核心的技巧：
        // `Scope::spawn` 要求闭包只需 'scope（可以借用调用者栈上的局部变量），
        // 但 executor.spawn 内部要求 future 是 'static。正常情况下编译器会拒绝编译。
        // 这里通过 mem::transmute 把 'scope 生命周期的引用强行"伪装"成 'env（约等于放宽/延长），
        // 骗过编译器的静态检查。
        // 之所以安全，是因为本函数末尾一定会 block_on 等待所有 spawn 出去的任务跑完
        // （或者被 Scope::drop 里的 cancel 取消掉）才返回，
        // 因此这些"伪造"的长生命周期引用实际上绝不会在真正的借用结束之后被访问。
        // SAFETY: This safety comment applies to all references transmuted to 'env.
        // Any futures spawned with these references need to return before this function completes.
        // This is guaranteed because we drive all the futures spawned onto the Scope
        // to completion in this function. However, rust has no way of knowing this so we
        // transmute the lifetimes to 'env here to appease the compiler as it is unable to validate safety.
        // Any usages of the references passed into `Scope` must be accessed through
        // the transmuted reference for the rest of this function.
        let executor: &crate::executor::Executor = &self.executor;
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let executor: &'env crate::executor::Executor = unsafe { mem::transmute(executor) };
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let external_executor: &'env ThreadExecutor<'env> =
            unsafe { mem::transmute(external_executor) };
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let scope_executor: &'env ThreadExecutor<'env> = unsafe { mem::transmute(scope_executor) };
        // spawned 队列用来收集所有通过 scope 生成的任务句柄，
        // fallible() 包装过，任务 panic 时 .await 得到 None 而不是直接扩散 panic，
        // 这样 get_results 可以统一捕获并在最后 resume_unwind。
        let spawned: ConcurrentQueue<FallibleTask<Result<T, Box<dyn core::any::Any + Send>>>> =
            ConcurrentQueue::unbounded();
        // shadow the variable so that the owned value cannot be used for the rest of the function
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let spawned: &'env ConcurrentQueue<
            FallibleTask<Result<T, Box<dyn core::any::Any + Send>>>,
        > = unsafe { mem::transmute(&spawned) };

        let scope = Scope {
            executor,
            external_executor,
            scope_executor,
            spawned,
            scope: PhantomData,
            env: PhantomData,
        };

        // shadow the variable so that the owned value cannot be used for the rest of the function
        // SAFETY: As above, all futures must complete in this function so we can change the lifetime
        let scope: &'env Scope<'_, 'env, T> = unsafe { mem::transmute(&scope) };

        // 执行用户传进来的闭包：闭包内部通过 scope.spawn(...) 等方法把任务塞进 spawned 队列，
        // 这一步只是"登记"任务，此时任务还没有开始被执行（下面才开始真正驱动执行器）。
        f(scope);

        if spawned.is_empty() {
            Vec::new()
        } else {
            block_on(async move {
                // get_results：不断从 spawned 队列里取出已完成的任务句柄，
                // 一旦所有任务都 pop 完并拿到结果，这个 future 就 Ready，从而让下面的 race (.or) 结束。
                let get_results = async {
                    let mut results = Vec::with_capacity(spawned.len());
                    while let Ok(task) = spawned.pop() {
                        if let Some(res) = task.await {
                            match res {
                                Ok(res) => results.push(res),
                                // 子任务 panic 了：在这里对外层线程重新 resume_unwind，
                                // 让调用 scope() 的代码感知到 panic（而不是被静默吞掉）。
                                Err(payload) => std::panic::resume_unwind(payload),
                            }
                        } else {
                            panic!("Failed to catch panic!");
                        }
                    }
                    results
                };

                // 如果线程池根本没有工作线程（比如 num_threads(0)），
                // 就必须由当前调用线程自己去 tick 全局 executor，否则任务永远不会被执行。
                let tick_task_pool_executor = tick_task_pool_executor || self.threads.is_empty();

                // we get this from a thread local so we should always be on the scope executors thread.
                // note: it is possible `scope_executor` and `external_executor` is the same executor,
                // in that case, we should only tick one of them, otherwise, it may cause deadlock.
                let scope_ticker = scope_executor.ticker().unwrap();
                let external_ticker = if !external_executor.is_same(scope_executor) {
                    external_executor.ticker()
                } else {
                    None
                };

                // 根据"是否有独立的外部执行器"和"是否需要顺带 tick 全局线程池"两个维度，
                // 选择 4 种不同组合的驱动策略（下面 4 个 execute_* 函数）。
                // 核心思路都一样：用 get_results.or(execute_forever) 让"结果齐了"和"持续 tick 执行器"
                // 两个 future 赛跑，谁先完成就返回——一旦 get_results 就绪，execute_forever 自动被丢弃。
                match (external_ticker, tick_task_pool_executor) {
                    (Some(external_ticker), true) => {
                        Self::execute_global_external_scope(
                            executor,
                            external_ticker,
                            scope_ticker,
                            get_results,
                        )
                        .await
                    }
                    (Some(external_ticker), false) => {
                        Self::execute_external_scope(external_ticker, scope_ticker, get_results)
                            .await
                    }
                    // either external_executor is none or it is same as scope_executor
                    (None, true) => {
                        Self::execute_global_scope(executor, scope_ticker, get_results).await
                    }
                    (None, false) => Self::execute_scope(scope_ticker, get_results).await,
                }
            })
        }
    }

    // 4 个 execute_* 函数功能类似，都是"无限循环地 tick 相关执行器"，区别只在于要 tick 哪些执行器：
    // - execute_global_external_scope：全局线程池 + 外部执行器 + scope 执行器，三者都 tick
    // - execute_external_scope：只 tick 外部执行器 + scope 执行器（不参与全局线程池抢任务）
    // - execute_global_scope：全局线程池 + scope 执行器
    // - execute_scope：只 tick scope 执行器（最省事，但结果可能等待最久，因为不帮忙抢全局任务）
    #[inline]
    async fn execute_global_external_scope<'scope, 'ticker, T>(
        executor: &'scope crate::executor::Executor<'scope>,
        external_ticker: ThreadExecutorTicker<'scope, 'ticker>,
        scope_ticker: ThreadExecutorTicker<'scope, 'ticker>,
        get_results: impl Future<Output = Vec<T>>,
    ) -> Vec<T> {
        // we restart the executors if a ktask errors. if a scoped
        // ktask errors it will panic the scope on the call to get_results
        // catch_unwind 包住 executor.run：某个非 scope 任务（比如全局线程池里别的任务）panic 时
        // 不会打断这里的循环，而是重新进入 loop 继续 tick；真正 scope 内任务的 panic
        // 会在 get_results 里被 resume_unwind，从而让 get_results.or(execute_forever) 整体结束。
        let execute_forever = async move {
            loop {
                let tick_forever = async {
                    loop {
                        external_ticker.tick().or(scope_ticker.tick()).await;
                    }
                };
                // we don't care if it errors. If a scoped ktask errors it will propagate
                // to get_results
                let _result = AssertUnwindSafe(executor.run(tick_forever))
                    .catch_unwind()
                    .await
                    .is_ok();
            }
        };
        get_results.or(execute_forever).await
    }

    #[inline]
    async fn execute_external_scope<'scope, 'ticker, T>(
        external_ticker: ThreadExecutorTicker<'scope, 'ticker>,
        scope_ticker: ThreadExecutorTicker<'scope, 'ticker>,
        get_results: impl Future<Output = Vec<T>>,
    ) -> Vec<T> {
        let execute_forever = async {
            loop {
                let tick_forever = async {
                    loop {
                        external_ticker.tick().or(scope_ticker.tick()).await;
                    }
                };
                let _result = AssertUnwindSafe(tick_forever).catch_unwind().await.is_ok();
            }
        };
        get_results.or(execute_forever).await
    }

    #[inline]
    async fn execute_global_scope<'scope, 'ticker, T>(
        executor: &'scope crate::executor::Executor<'scope>,
        scope_ticker: ThreadExecutorTicker<'scope, 'ticker>,
        get_results: impl Future<Output = Vec<T>>,
    ) -> Vec<T> {
        let execute_forever = async {
            loop {
                let tick_forever = async {
                    loop {
                        scope_ticker.tick().await;
                    }
                };
                let _result = AssertUnwindSafe(executor.run(tick_forever))
                    .catch_unwind()
                    .await
                    .is_ok();
            }
        };
        get_results.or(execute_forever).await
    }

    #[inline]
    async fn execute_scope<'scope, 'ticker, T>(
        scope_ticker: ThreadExecutorTicker<'scope, 'ticker>,
        get_results: impl Future<Output = Vec<T>>,
    ) -> Vec<T> {
        let execute_forever = async {
            loop {
                let tick_forever = async {
                    loop {
                        scope_ticker.tick().await;
                    }
                };
                let _result = AssertUnwindSafe(tick_forever).catch_unwind().await.is_ok();
            }
        };
        get_results.or(execute_forever).await
    }

    /// Spawns a static future onto the thread pool. The returned [`Task`] is a
    /// future that can be polled for the result. It can also be canceled and
    /// "detached", allowing the ktask to continue running even if dropped. In
    /// any case, the pool will execute the ktask even without polling by the
    /// end-user.
    ///
    /// If the provided future is non-`Send`, [`TaskPool::spawn_local`] should
    /// be used instead.
    pub fn spawn<T>(&self, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        self.executor.spawn(future)
    }

    /// Spawns a static future on the thread-local async executor for the
    /// current thread. The ktask will run entirely on the thread the ktask was
    /// spawned on.
    ///
    /// The returned [`Task`] is a future that can be polled for the
    /// result. It can also be canceled and "detached", allowing the ktask to
    /// continue running even if dropped. In any case, the pool will execute the
    /// ktask even without polling by the end-user.
    ///
    /// Users should generally prefer to use [`TaskPool::spawn`] instead,
    /// unless the provided future is not `Send`.
    pub fn spawn_local<T>(&self, future: impl Future<Output = T> + 'static) -> Task<T>
    where
        T: 'static,
    {
        TaskPool::LOCAL_EXECUTOR.with(|executor| executor.spawn(future))
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
        F: FnOnce(&crate::executor::LocalExecutor) -> R,
    {
        Self::LOCAL_EXECUTOR.with(f)
    }
}

impl Default for TaskPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        self.shutdown_tx.close();

        let panicking = thread::panicking();
        for join_handle in self.threads.drain(..) {
            let res = join_handle.join();
            if !panicking {
                res.expect("Task thread panicked while executing.");
            }
        }
    }
}

/// A [`TaskPool`] scope for running one or more non-`'static` futures.
///
/// For more information, see [`TaskPool::scope`].
#[derive(Debug)]
pub struct Scope<'scope, 'env: 'scope, T> {
    executor: &'scope crate::executor::Executor<'scope>,
    external_executor: &'scope ThreadExecutor<'scope>,
    scope_executor: &'scope ThreadExecutor<'scope>,
    spawned: &'scope ConcurrentQueue<FallibleTask<Result<T, Box<dyn core::any::Any + Send>>>>,
    // make `Scope` invariant over 'scope and 'env
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope, 'env, T: Send + 'scope> Scope<'scope, 'env, T> {
    /// Spawns a scoped future onto the thread pool. The scope *must* outlive
    /// the provided future. The results of the future will be returned as a part of
    /// [`TaskPool::scope`]'s return value.
    ///
    /// For futures that should run on the thread `scope` is called on [`Scope::spawn_on_scope`] should be used
    /// instead.
    ///
    /// For more information, see [`TaskPool::scope`].
    pub fn spawn<Fut: Future<Output = T> + 'scope + Send>(&self, f: Fut) {
        // catch_unwind 让子任务 panic 时返回 Err 而不是直接扩散，
        // .fallible() 再包一层让任务被取消/abort 时 .await 返回 None（对应 Scope::drop 里的 cancel 场景）。
        let task = self
            .executor
            .spawn(AssertUnwindSafe(f).catch_unwind())
            .fallible();
        // ConcurrentQueue only errors when closed or full, but we never
        // close and use an unbounded queue, so it is safe to unwrap
        self.spawned.push(task).unwrap();
    }

    /// Spawns a scoped future onto the thread the scope is run on. The scope *must* outlive
    /// the provided future. The results of the future will be returned as a part of
    /// [`TaskPool::scope`]'s return value.  Users should generally prefer to use
    /// [`Scope::spawn`] instead, unless the provided future needs to run on the scope's thread.
    ///
    /// For more information, see [`TaskPool::scope`].
    pub fn spawn_on_scope<Fut: Future<Output = T> + 'scope + Send>(&self, f: Fut) {
        let task = self
            .scope_executor
            .spawn(AssertUnwindSafe(f).catch_unwind())
            .fallible();
        // ConcurrentQueue only errors when closed or full, but we never
        // close and use an unbounded queue, so it is safe to unwrap
        self.spawned.push(task).unwrap();
    }

    /// Spawns a scoped future onto the thread of the external thread executor.
    /// This is typically the main thread. The scope *must* outlive
    /// the provided future. The results of the future will be returned as a part of
    /// [`TaskPool::scope`]'s return value.  Users should generally prefer to use
    /// [`Scope::spawn`] instead, unless the provided future needs to run on the external thread.
    ///
    /// For more information, see [`TaskPool::scope`].
    pub fn spawn_on_external<Fut: Future<Output = T> + 'scope + Send>(&self, f: Fut) {
        let task = self
            .external_executor
            .spawn(AssertUnwindSafe(f).catch_unwind())
            .fallible();
        // ConcurrentQueue only errors when closed or full, but we never
        // close and use an unbounded queue, so it is safe to unwrap
        self.spawned.push(task).unwrap();
    }
}

impl<'scope, 'env, T> Drop for Scope<'scope, 'env, T>
where
    T: 'scope,
{
    fn drop(&mut self) {
        // 兜底保护：如果 scope() 因为某种原因提前结束（例如 f(scope) 内部 panic，
        // 导致没走到正常的 get_results 流程），这里确保所有已经 spawn 出去、
        // 但还未被等待的任务都会被取消，不会有任务继续持有已经失效的借用去运行。
        block_on(async {
            while let Ok(task) = self.spawned.pop() {
                task.cancel().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::Barrier;

    #[test]
    fn test_spawn() {
        let pool = TaskPool::new();

        let foo = Box::new(42);
        let foo = &*foo;

        let count = Arc::new(AtomicI32::new(0));

        let outputs = pool.scope(|scope| {
            for _ in 0..100 {
                let count_clone = count.clone();
                scope.spawn(async move {
                    if *foo != 42 {
                        panic!("not 42!?!?")
                    } else {
                        count_clone.fetch_add(1, Ordering::Relaxed);
                        *foo
                    }
                });
            }
        });

        for output in &outputs {
            assert_eq!(*output, 42);
        }

        assert_eq!(outputs.len(), 100);
        assert_eq!(count.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_thread_callbacks() {
        let counter = Arc::new(AtomicI32::new(0));
        let start_counter = counter.clone();
        {
            let barrier = Arc::new(Barrier::new(11));
            let last_barrier = barrier.clone();
            // Build and immediately drop to terminate
            let _pool = TaskPoolBuilder::new()
                .num_threads(10)
                .on_thread_spawn(move || {
                    start_counter.fetch_add(1, Ordering::Relaxed);
                    barrier.clone().wait();
                })
                .build();
            last_barrier.wait();
            assert_eq!(10, counter.load(Ordering::Relaxed));
        }
        assert_eq!(10, counter.load(Ordering::Relaxed));
        let end_counter = counter.clone();
        {
            let _pool = TaskPoolBuilder::new()
                .num_threads(20)
                .on_thread_destroy(move || {
                    end_counter.fetch_sub(1, Ordering::Relaxed);
                })
                .build();
            assert_eq!(10, counter.load(Ordering::Relaxed));
        }
        assert_eq!(-10, counter.load(Ordering::Relaxed));
        let start_counter = counter.clone();
        let end_counter = counter.clone();
        {
            let barrier = Arc::new(Barrier::new(6));
            let last_barrier = barrier.clone();
            let _pool = TaskPoolBuilder::new()
                .num_threads(5)
                .on_thread_spawn(move || {
                    start_counter.fetch_add(1, Ordering::Relaxed);
                    barrier.wait();
                })
                .on_thread_destroy(move || {
                    end_counter.fetch_sub(1, Ordering::Relaxed);
                })
                .build();
            last_barrier.wait();
            assert_eq!(-5, counter.load(Ordering::Relaxed));
        }
        assert_eq!(-10, counter.load(Ordering::Relaxed));
    }

    #[test]
    fn test_mixed_spawn_on_scope_and_spawn() {
        let pool = TaskPool::new();

        let foo = Box::new(42);
        let foo = &*foo;

        let local_count = Arc::new(AtomicI32::new(0));
        let non_local_count = Arc::new(AtomicI32::new(0));

        let outputs = pool.scope(|scope| {
            for i in 0..100 {
                if i % 2 == 0 {
                    let count_clone = non_local_count.clone();
                    scope.spawn(async move {
                        if *foo != 42 {
                            panic!("not 42!?!?")
                        } else {
                            count_clone.fetch_add(1, Ordering::Relaxed);
                            *foo
                        }
                    });
                } else {
                    let count_clone = local_count.clone();
                    scope.spawn_on_scope(async move {
                        if *foo != 42 {
                            panic!("not 42!?!?")
                        } else {
                            count_clone.fetch_add(1, Ordering::Relaxed);
                            *foo
                        }
                    });
                }
            }
        });

        for output in &outputs {
            assert_eq!(*output, 42);
        }

        assert_eq!(outputs.len(), 100);
        assert_eq!(local_count.load(Ordering::Relaxed), 50);
        assert_eq!(non_local_count.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn test_thread_locality() {
        let pool = Arc::new(TaskPool::new());
        let count = Arc::new(AtomicI32::new(0));
        let barrier = Arc::new(Barrier::new(101));
        let thread_check_failed = Arc::new(AtomicBool::new(false));

        for _ in 0..100 {
            let inner_barrier = barrier.clone();
            let count_clone = count.clone();
            let inner_pool = pool.clone();
            let inner_thread_check_failed = thread_check_failed.clone();
            thread::spawn(move || {
                inner_pool.scope(|scope| {
                    let inner_count_clone = count_clone.clone();
                    scope.spawn(async move {
                        inner_count_clone.fetch_add(1, Ordering::Release);
                    });
                    let spawner = thread::current().id();
                    let inner_count_clone = count_clone.clone();
                    scope.spawn_on_scope(async move {
                        inner_count_clone.fetch_add(1, Ordering::Release);
                        if thread::current().id() != spawner {
                            // NOTE: This check is using an atomic rather than simply panicking the
                            // thread to avoid deadlocking the barrier on failure
                            inner_thread_check_failed.store(true, Ordering::Release);
                        }
                    });
                });
                inner_barrier.wait();
            });
        }
        barrier.wait();
        assert!(!thread_check_failed.load(Ordering::Acquire));
        assert_eq!(count.load(Ordering::Acquire), 200);
    }

    #[test]
    fn test_nested_spawn() {
        let pool = TaskPool::new();

        let foo = Box::new(42);
        let foo = &*foo;

        let count = Arc::new(AtomicI32::new(0));

        let outputs: Vec<i32> = pool.scope(|scope| {
            for _ in 0..10 {
                let count_clone = count.clone();
                scope.spawn(async move {
                    for _ in 0..10 {
                        let count_clone_clone = count_clone.clone();
                        scope.spawn(async move {
                            if *foo != 42 {
                                panic!("not 42!?!?")
                            } else {
                                count_clone_clone.fetch_add(1, Ordering::Relaxed);
                                *foo
                            }
                        });
                    }
                    *foo
                });
            }
        });

        for output in &outputs {
            assert_eq!(*output, 42);
        }

        // the inner loop runs 100 times and the outer one runs 10. 100 + 10
        assert_eq!(outputs.len(), 110);
        assert_eq!(count.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_nested_locality() {
        let pool = Arc::new(TaskPool::new());
        let count = Arc::new(AtomicI32::new(0));
        let barrier = Arc::new(Barrier::new(101));
        let thread_check_failed = Arc::new(AtomicBool::new(false));

        for _ in 0..100 {
            let inner_barrier = barrier.clone();
            let count_clone = count.clone();
            let inner_pool = pool.clone();
            let inner_thread_check_failed = thread_check_failed.clone();
            thread::spawn(move || {
                inner_pool.scope(|scope| {
                    let spawner = thread::current().id();
                    let inner_count_clone = count_clone.clone();
                    scope.spawn(async move {
                        inner_count_clone.fetch_add(1, Ordering::Release);

                        // spawning on the scope from another thread runs the futures on the scope's thread
                        scope.spawn_on_scope(async move {
                            inner_count_clone.fetch_add(1, Ordering::Release);
                            if thread::current().id() != spawner {
                                // NOTE: This check is using an atomic rather than simply panicking the
                                // thread to avoid deadlocking the barrier on failure
                                inner_thread_check_failed.store(true, Ordering::Release);
                            }
                        });
                    });
                });
                inner_barrier.wait();
            });
        }
        barrier.wait();
        assert!(!thread_check_failed.load(Ordering::Acquire));
        assert_eq!(count.load(Ordering::Acquire), 200);
    }

    // This test will often freeze on other executors.
    #[test]
    fn test_nested_scopes() {
        let pool = TaskPool::new();
        let count = Arc::new(AtomicI32::new(0));

        pool.scope(|scope| {
            scope.spawn(async {
                pool.scope(|scope| {
                    scope.spawn(async {
                        count.fetch_add(1, Ordering::Relaxed);
                    });
                });
            });
        });

        assert_eq!(count.load(Ordering::Acquire), 1);
    }
}
