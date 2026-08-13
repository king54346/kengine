#[cfg(test)]
mod tests {
    use ktask::TaskPool;

    #[test]
    fn spawns_and_completes_parallel_tasks() {
        let pool = TaskPool::new();
        let mut results = pool.scope(|scope| {
            for i in 0..8 {
                scope.spawn(async move { i * i });
            }
        });
        results.sort_unstable();

        let expected: Vec<i32> = (0..8).map(|i| i * i).collect();
        assert_eq!(results, expected);
    }

    #[test]
    fn block_on_resolves_a_future() {
        let value = ktask::block_on(async { 21 + 21 });
        assert_eq!(value, 42);
    }
}
