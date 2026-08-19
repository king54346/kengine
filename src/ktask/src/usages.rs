use super::TaskPool;
use std::ops::Deref;
use std::sync::OnceLock;

// 用宏批量生成三个全局单例任务池类型（ComputeTaskPool / AsyncComputeTaskPool / IoTaskPool），
// 每个类型内部都用 OnceLock 存一份全局唯一实例，并通过 Deref 到 TaskPool，
// 用起来就像直接用 TaskPool 一样，但保证全局只初始化一次。
macro_rules! taskpool {
    ($(#[$attr:meta])* ($static:ident, $type:ident)) => {
        static $static: OnceLock<$type> = OnceLock::new();

        $(#[$attr])*
        #[derive(Debug)]
        pub struct $type(TaskPool);

        impl $type {
            #[doc = concat!(" Gets the global [`", stringify!($type), "`] instance, or initializes it with `f`.")]
            pub fn get_or_init(f: impl FnOnce() -> TaskPool) -> &'static Self {
                $static.get_or_init(|| Self(f()))
            }

            #[doc = concat!(" Attempts to get the global [`", stringify!($type), "`] instance, \
                or returns `None` if it is not initialized.")]
            pub fn try_get() -> Option<&'static Self> {
                $static.get()
            }

            #[doc = concat!(" Gets the global [`", stringify!($type), "`] instance.")]
            #[doc = ""]
            #[doc = " # Panics"]
            #[doc = " Panics if the global instance has not been initialized yet."]
            pub fn get() -> &'static Self {
                $static.get().expect(
                    concat!(
                        "The ",
                        stringify!($type),
                        " has not been initialized yet. Please call ",
                        stringify!($type),
                        "::get_or_init beforehand."
                    )
                )
            }
        }

        impl Deref for $type {
            type Target = TaskPool;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
}

taskpool! {
    /// A newtype for a ktask pool for CPU-intensive work that must be completed to
    /// deliver the next frame
    ///
    /// See [`TaskPool`] documentation for details on Bevy tasks.
    /// [`AsyncComputeTaskPool`] should be preferred if the work does not have to be
    /// completed before the next frame.
    (COMPUTE_TASK_POOL, ComputeTaskPool)
}

taskpool! {
    /// A newtype for a ktask pool for CPU-intensive work that may span across multiple frames
    ///
    /// See [`TaskPool`] documentation for details on Bevy tasks.
    /// Use [`ComputeTaskPool`] if the work must be complete before advancing to the next frame.
    (ASYNC_COMPUTE_TASK_POOL, AsyncComputeTaskPool)
}

taskpool! {
    /// A newtype for a ktask pool for IO-intensive work (i.e. tasks that spend very little time in a
    /// "woken" state)
    ///
    /// See [`TaskPool`] documentation for details on Bevy tasks.
    (IO_TASK_POOL, IoTaskPool)
}

/// A function used to tick the global tasks pools on the main thread.
/// This will run a maximum of 100 local tasks per executor per call to this function.
///
/// # Warning
///
/// This function *must* be called on the main thread, or the ktask pools will not be updated appropriately.
// 只有多线程模式才需要手动 tick：单线程实现（single_threaded_task_pool.rs）里 spawn/spawn_local
// 会在调用时原地把本地 executor try_tick 到底，任务提交后立刻自己跑完，不需要外部再驱动。
// 因此在非 multi_threaded（例如 web/wasm）场景下这个函数没有存在的意义，直接不编译进去。
#[cfg(feature = "multi_threaded")]
pub fn tick_global_task_pools_on_main_thread() {
    // 三层嵌套 with_local_executor 只是为了同时拿到三个池子在"主线程"上的本地执行器引用，
    // 然后在最内层循环里交替 try_tick 各自最多 100 次，
    // 从而推进那些通过 spawn_local 提交、只能在主线程跑的任务（例如涉及非 Send 资源的任务）。
    COMPUTE_TASK_POOL
        .get()
        .unwrap()
        .with_local_executor(|compute_local_executor| {
            ASYNC_COMPUTE_TASK_POOL
                .get()
                .unwrap()
                .with_local_executor(|async_local_executor| {
                    IO_TASK_POOL
                        .get()
                        .unwrap()
                        .with_local_executor(|io_local_executor| {
                            for _ in 0..100 {
                                compute_local_executor.try_tick();
                                async_local_executor.try_tick();
                                io_local_executor.try_tick();
                            }
                        });
                });
        });
}
