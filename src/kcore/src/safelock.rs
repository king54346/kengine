//! 对 Mutex 类对象的扩展，提供 `safe_lock` 方法以替代 `lock`，
//! 并为加锁操作设置超时时间，防止因死锁导致程序永久卡死。

use parking_lot::{Mutex, MutexGuard};
#[allow(unused_imports)]
use std::sync::TryLockError;
use std::time::Duration;

#[allow(dead_code)]
const PANIC_MESSAGE: &str = "lock timeout";

/// 可加锁对象的 trait，若加锁时间过长则主动 panic。
/// 相比永久冻结，panic 是更可接受的行为。
pub trait SafeLock {
    const TIMEOUT: Duration = Duration::from_secs(10);
    type Output<'a>
    where
        Self: 'a;
    /// 尝试加锁，并限制最长等待时间；若超时则 panic，而不是永久阻塞。
    fn safe_lock(&self) -> Self::Output<'_>;
}

impl<T: ?Sized> SafeLock for Mutex<T> {
    type Output<'a>
        = MutexGuard<'a, T>
    where
        T: 'a;
    #[cfg(target_arch = "wasm32")]
    fn safe_lock(&self) -> Self::Output<'_> {
        self.lock()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn safe_lock(&self) -> Self::Output<'_> {
        self.try_lock_for(Self::TIMEOUT).expect(PANIC_MESSAGE)
    }
}

impl<T: ?Sized> SafeLock for std::sync::Mutex<T> {
    type Output<'a>
        = std::sync::LockResult<std::sync::MutexGuard<'a, T>>
    where
        T: 'a;

    #[cfg(target_arch = "wasm32")]
    fn safe_lock(&self) -> Self::Output<'_> {
        self.lock()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn safe_lock(&self) -> Self::Output<'_> {
        let start = std::time::Instant::now();
        loop {
            match self.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::WouldBlock) => (),
                Err(TryLockError::Poisoned(err)) => return Err(err),
            }
            std::thread::yield_now();
            if start.elapsed() > Self::TIMEOUT {
                std::panic::panic_any(PANIC_MESSAGE);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{
        any::Any,
        panic::{AssertUnwindSafe, catch_unwind},
    };
    fn panic_to_string(message: Box<dyn Any>) -> Option<String> {
        match message.downcast_ref::<&str>() {
            Some(&str) => Some(str.into()),
            None => message.downcast::<String>().ok().map(|s| *s),
        }
    }
    #[test]
    fn successful_lock_parking_lot() {
        let mutex = Mutex::new(());
        drop(mutex.safe_lock());
    }
    #[test]
    fn successful_lock_std() {
        let mutex = std::sync::Mutex::new(());
        drop(mutex.safe_lock());
    }
    #[test]
    fn failed_lock_parking_lot() {
        let mutex = Mutex::new(());
        let _guard = mutex.safe_lock();
        let panic_message = catch_unwind(AssertUnwindSafe(|| mutex.safe_lock()))
            .expect_err("safe_lock did not panic");
        let Some(message) = panic_to_string(panic_message) else {
            panic!("safe_lock panicked with wrong type");
        };
        assert_eq!(message, PANIC_MESSAGE);
    }
    #[test]
    fn failed_lock_std() {
        let mutex = std::sync::Mutex::new(());
        let _guard = mutex.safe_lock();
        let panic_message =
            catch_unwind(|| mutex.safe_lock()).expect_err("safe_lock did not panic");
        let Some(message) = panic_to_string(panic_message) else {
            panic!("safe_lock panicked with wrong type");
        };
        assert_eq!(message, PANIC_MESSAGE);
    }
}
