//! Utilities for working with [`Future`]s.
use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

/// Consumes a future, polls it once, and immediately returns the output
/// or returns `None` if it wasn't ready yet.
///
/// This will cancel the future if it's not ready.
pub fn now_or_never<F: Future>(future: F) -> Option<F::Output> {
    // Waker::noop()：用一个"什么都不做"的假唤醒器，因为这里只 poll 一次，不会真正挂起等待被唤醒。
    // pin!(future)：future 需要 Pin 才能被 poll，这里在栈上原地 pin 住，避免堆分配。
    // 若未就绪（Pending），future 直接在函数结束时被 drop（相当于取消）。
    let mut cx = Context::from_waker(Waker::noop());
    match pin!(future).poll(&mut cx) {
        Poll::Ready(x) => Some(x),
        _ => None,
    }
}

/// Polls a future once, and returns the output if ready
/// or returns `None` if it wasn't ready yet.
pub fn check_ready<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    now_or_never(future)
}