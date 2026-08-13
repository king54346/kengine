lib.rs              入口：定义 Trait、模块导出、CPU 核心数工具
executor.rs          最底层：封装 async-executor（多线程 / 单线程）
thread_executor.rs   只能在创建它的线程上"tick"，但可以从其他线程 spawn 任务
task_pool.rs          多线程版 TaskPool（feature = "multi_threaded"）
single_threaded_task_pool.rs   单线程版 TaskPool（不开 multi_threaded 时，例如 wasm）
usages.rs            定义 3 个全局单例任务池：ComputeTaskPool / AsyncComputeTaskPool / IoTaskPool
slice.rs             给 &[T] / &mut [T] 提供 par_chunk_map 等并行 map 方法
futures.rs           小工具：立即 poll 一次 future
iter/                并行迭代器（ParallelIterator）

lib.rs
  - 定义 ConditionalSend：在非 wasm 平台要求 Send，在 wasm 平台不要求（因为 wasm 单线程，future 不需要 Send）。
  - 根据 multi_threaded feature 条件编译，导出对应的 TaskPool/ThreadExecutor 实现。
  - available_parallelism()：获取逻辑 CPU 核心数，出错时兜底返回 1。

executor.rs
  - 对 async_executor::Executor（多线程可用）和 async_executor::LocalExecutor（单线程，无需 Send）做了一层薄封装（Deref/DerefMut），是整个库真正驱动 future 执行的引擎。

thread_executor.rs
  - ThreadExecutor：只能在创建它的那个线程上被 tick 推进，但其他线程可以往里面 spawn 任务（要求 Send）。常用于主线程场景，比如渲染必须发生在主线程，但任务可以从工作线程提交进来。

task_pool.rs（多线程模式，核心文件）
  - TaskPoolBuilder：配置线程数、栈大小、线程名、线程创建/销毁回调。
  - TaskPool：真正持有一组 JoinHandle 工作线程，每个线程内部跑一个 loop 不断 tick 本地 executor，并从全局 executor 里"偷"任务执行（work-stealing 模型）。
  - TaskPool::spawn：把 'static + Send 的 future 扔进全局多线程执行器。
  - TaskPool::spawn_local：把非 Send 的 future 扔到调用线程自己的本地执行器。
  - TaskPool::scope：类似 std::thread::scope / rayon::scope，允许在闭包里 spawn 非 'static 的（可以借用局部变量的）future，并阻塞等所有任务跑完再返回结果。里面用了不安全的生命周期 transmute 技巧（把 'scope 伪装成
  'env/'static）来骗过编译器，但靠"函数返回前一定 block_on 等完所有任务"来保证安全。

single_threaded_task_pool.rs
  - 提供和上面几乎一样的 API（TaskPool、Scope、ThreadExecutor 等），但内部全部退化为单线程实现（用于 wasm 或关闭 multi_threaded feature 时），保证上层代码不用关心是否多线程。

usages.rs
  3种池子代码层面没有任何强制区分逻辑——是否把任务放对池子，完全靠开发者自觉遵守这个语义约定，运行时不会检查。  轮询获取的时候优先级 ComputeTaskPool>AsyncComputeTaskPool>IoTaskPool
  - 用宏 taskpool! 生成 3 个全局单例：
  - ComputeTaskPool：CPU 密集型、必须在下一帧前完成的工作。
  - AsyncComputeTaskPool：CPU 密集但可以跨多帧完成的工作。
  - IoTaskPool：IO 密集型（大部分时间在等待）工作。
  - tick_global_task_pools_on_main_thread()：必须在主线程调用，每次最多 tick 100 次本地任务，用于推进这些全局任务池上挂的本地（non-Send）任务。

slice.rs
  - ParallelSlice / ParallelSliceMut trait，给切片加 par_chunk_map / par_splat_map（及 mut 版本）：把切片按 chunk 拆开，每个 chunk 起一个任务并行处理，结果按顺序收集成 Vec。

futures.rs
  - now_or_never / check_ready：给定一个 future，poll 一次，就绪则返回结果，否则返回 None（不阻塞、不等待）。

iter/（adapters.rs + mod.rs）
  - 实现 ParallelIterator，类似 rayon 的并行迭代器，基于上面的 TaskPool 做并行处理。



executor 层负责跑 future，task_pool 层负责管理线程和调度，slice/iter 层在此基础上提供好用的并行编程 API