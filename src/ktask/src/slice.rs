use super::TaskPool;

/// Provides functions for mapping read-only slices across a provided [`TaskPool`].
pub trait ParallelSlice<T: Sync>: AsRef<[T]> {
    /// Splits the slice in chunks of size `chunks_size` or less and maps the chunks
    /// in parallel across the provided `task_pool`. One ktask is spawned in the ktask pool
    /// for every chunk.
    ///
    /// The iteration function takes the index of the chunk in the original slice as the
    /// first argument, and the chunk as the second argument.
    ///
    /// Returns a `Vec` of the mapped results in the same order as the input.
    ///
    /// # Example
    ///
    /// ```
    /// # use ktask::prelude::*;
    /// # use ktask::TaskPool;
    /// let task_pool = TaskPool::new();
    /// let counts = (0..10000).collect::<Vec<u32>>();
    /// let incremented = counts.par_chunk_map(&task_pool, 100, |_index, chunk| {
    ///   let mut results = Vec::new();
    ///   for count in chunk {
    ///     results.push(*count + 2);
    ///   }
    ///   results
    /// });
    /// # let flattened: Vec<_> = incremented.into_iter().flatten().collect();
    /// # assert_eq!(flattened, (2..10002).collect::<Vec<u32>>());
    /// ```
    ///
    /// # See Also
    ///
    /// - [`ParallelSliceMut::par_chunk_map_mut`] for mapping mutable slices.
    /// - [`ParallelSlice::par_splat_map`] for mapping when a specific chunk size is unknown.
    fn par_chunk_map<F, R>(&self, task_pool: &TaskPool, chunk_size: usize, f: F) -> Vec<R>
    where
        F: Fn(usize, &[T]) -> R + Send + Sync,
        R: Send + 'static,
    {
        let slice = self.as_ref();
        let f = &f;
        // 按 chunk_size 把切片切成若干不重叠的子切片，每个子切片起一个 scope 任务并行处理，
        // 结果由 task_pool.scope 按 spawn 顺序收集成 Vec 返回（顺序与 chunks 的顺序一致）。
        task_pool.scope(|scope| {
            for (index, chunk) in slice.chunks(chunk_size).enumerate() {
                scope.spawn(async move { f(index, chunk) });
            }
        })
    }

    /// Splits the slice into a maximum of `max_tasks` chunks, and maps the chunks in parallel
    /// across the provided `task_pool`. One ktask is spawned in the ktask pool for every chunk.
    ///
    /// If `max_tasks` is `None`, this function will attempt to use one chunk per thread in
    /// `task_pool`.
    ///
    /// The iteration function takes the index of the chunk in the original slice as the
    /// first argument, and the chunk as the second argument.
    ///
    /// Returns a `Vec` of the mapped results in the same order as the input.
    ///
    /// # Example
    ///
    /// ```
    /// # use ktask::prelude::*;
    /// # use ktask::TaskPool;
    /// let task_pool = TaskPool::new();
    /// let counts = (0..10000).collect::<Vec<u32>>();
    /// let incremented = counts.par_splat_map(&task_pool, None, |_index, chunk| {
    ///   let mut results = Vec::new();
    ///   for count in chunk {
    ///     results.push(*count + 2);
    ///   }
    ///   results
    /// });
    /// # let flattened: Vec<_> = incremented.into_iter().flatten().collect();
    /// # assert_eq!(flattened, (2..10002).collect::<Vec<u32>>());
    /// ```
    ///
    /// # See Also
    ///
    /// [`ParallelSliceMut::par_splat_map_mut`] for mapping mutable slices.
    /// [`ParallelSlice::par_chunk_map`] for mapping when a specific chunk size is desirable.
    fn par_splat_map<F, R>(&self, task_pool: &TaskPool, max_tasks: Option<usize>, f: F) -> Vec<R>
    where
        F: Fn(usize, &[T]) -> R + Send + Sync,
        R: Send + 'static,
    {
        let slice = self.as_ref();
        // 自动推算 chunk_size：默认让每个线程分到大约一个 chunk（len / thread_num），
        // 如果调用方指定了 max_tasks 上限，再取二者中较大的那个 chunk_size（即任务数更少的那种），
        // 保证最终任务数不超过 max_tasks；最外层 max(1, ..) 防止 chunk_size 算出 0（数组比线程数还短的情况）。
        let chunk_size = core::cmp::max(
            1,
            core::cmp::max(
                slice.len() / task_pool.thread_num(),
                slice.len() / max_tasks.unwrap_or(usize::MAX),
            ),
        );

        slice.par_chunk_map(task_pool, chunk_size, f)
    }
}

impl<S, T: Sync> ParallelSlice<T> for S where S: AsRef<[T]> {}

/// Provides functions for mapping mutable slices across a provided [`TaskPool`].
pub trait ParallelSliceMut<T: Send>: AsMut<[T]> {
    /// Splits the slice in chunks of size `chunks_size` or less and maps the chunks
    /// in parallel across the provided `task_pool`. One ktask is spawned in the ktask pool
    /// for every chunk.
    ///
    /// The iteration function takes the index of the chunk in the original slice as the
    /// first argument, and the chunk as the second argument.
    ///
    /// Returns a `Vec` of the mapped results in the same order as the input.
    ///
    /// # Example
    ///
    /// ```
    /// # use ktask::prelude::*;
    /// # use ktask::TaskPool;
    /// let task_pool = TaskPool::new();
    /// let mut counts = (0..10000).collect::<Vec<u32>>();
    /// let incremented = counts.par_chunk_map_mut(&task_pool, 100, |_index, chunk| {
    ///   let mut results = Vec::new();
    ///   for count in chunk {
    ///     *count += 5;
    ///     results.push(*count - 2);
    ///   }
    ///   results
    /// });
    ///
    /// assert_eq!(counts, (5..10005).collect::<Vec<u32>>());
    /// # let flattened: Vec<_> = incremented.into_iter().flatten().collect();
    /// # assert_eq!(flattened, (3..10003).collect::<Vec<u32>>());
    /// ```
    ///
    /// # See Also
    ///
    /// [`ParallelSlice::par_chunk_map`] for mapping immutable slices.
    /// [`ParallelSliceMut::par_splat_map_mut`] for mapping when a specific chunk size is unknown.
    fn par_chunk_map_mut<F, R>(&mut self, task_pool: &TaskPool, chunk_size: usize, f: F) -> Vec<R>
    where
        F: Fn(usize, &mut [T]) -> R + Send + Sync,
        R: Send + 'static,
    {
        let slice = self.as_mut();
        let f = &f;
        task_pool.scope(|scope| {
            for (index, chunk) in slice.chunks_mut(chunk_size).enumerate() {
                scope.spawn(async move { f(index, chunk) });
            }
        })
    }

    /// Splits the slice into a maximum of `max_tasks` chunks, and maps the chunks in parallel
    /// across the provided `task_pool`. One ktask is spawned in the ktask pool for every chunk.
    ///
    /// If `max_tasks` is `None`, this function will attempt to use one chunk per thread in
    /// `task_pool`.
    ///
    /// The iteration function takes the index of the chunk in the original slice as the
    /// first argument, and the chunk as the second argument.
    ///
    /// Returns a `Vec` of the mapped results in the same order as the input.
    ///
    /// # Example
    ///
    /// ```
    /// # use ktask::prelude::*;
    /// # use ktask::TaskPool;
    /// let task_pool = TaskPool::new();
    /// let mut counts = (0..10000).collect::<Vec<u32>>();
    /// let incremented = counts.par_splat_map_mut(&task_pool, None, |_index, chunk| {
    ///   let mut results = Vec::new();
    ///   for count in chunk {
    ///     *count += 5;
    ///     results.push(*count - 2);
    ///   }
    ///   results
    /// });
    ///
    /// assert_eq!(counts, (5..10005).collect::<Vec<u32>>());
    /// # let flattened: Vec<_> = incremented.into_iter().flatten().collect::<Vec<u32>>();
    /// # assert_eq!(flattened, (3..10003).collect::<Vec<u32>>());
    /// ```
    ///
    /// # See Also
    ///
    /// [`ParallelSlice::par_splat_map`] for mapping immutable slices.
    /// [`ParallelSliceMut::par_chunk_map_mut`] for mapping when a specific chunk size is desirable.
    fn par_splat_map_mut<F, R>(
        &mut self,
        task_pool: &TaskPool,
        max_tasks: Option<usize>,
        f: F,
    ) -> Vec<R>
    where
        F: Fn(usize, &mut [T]) -> R + Send + Sync,
        R: Send + 'static,
    {
        let mut slice = self.as_mut();
        let chunk_size = core::cmp::max(
            1,
            core::cmp::max(
                slice.len() / task_pool.thread_num(),
                slice.len() / max_tasks.unwrap_or(usize::MAX),
            ),
        );

        slice.par_chunk_map_mut(task_pool, chunk_size, f)
    }
}

impl<S, T: Send> ParallelSliceMut<T> for S where S: AsMut<[T]> {}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn test_par_chunks_map() {
        let v = vec![42; 1000];
        let task_pool = TaskPool::new();
        let outputs = v.par_splat_map(&task_pool, None, |_, numbers| -> i32 {
            numbers.iter().sum()
        });

        let mut sum = 0;
        for output in outputs {
            sum += output;
        }

        assert_eq!(sum, 1000 * 42);
    }

    #[test]
    fn test_par_chunks_map_mut() {
        let mut v = vec![42; 1000];
        let task_pool = TaskPool::new();

        let outputs = v.par_splat_map_mut(&task_pool, None, |_, numbers| -> i32 {
            for number in numbers.iter_mut() {
                *number *= 2;
            }
            numbers.iter().sum()
        });

        let mut sum = 0;
        for output in outputs {
            sum += output;
        }

        assert_eq!(sum, 1000 * 42 * 2);
        assert_eq!(v[0], 84);
    }

    #[test]
    fn test_par_chunks_map_index() {
        let v = vec![1; 1000];
        let task_pool = TaskPool::new();
        let outputs = v.par_chunk_map(&task_pool, 100, |index, numbers| -> i32 {
            numbers.iter().sum::<i32>() * index as i32
        });

        assert_eq!(outputs.iter().sum::<i32>(), 100 * (9 * 10) / 2);
    }
}