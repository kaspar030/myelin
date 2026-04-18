//! A tiny single-waiter async mutex for shared-reader / shared-writer
//! access inside a single task.
//!
//! Unlike `RefCell`, `LocalLock` guards can safely be held across `.await`
//! — the guard simply defers any other acquirer onto a [`AtomicWaker`].
//! Unlike a full async mutex (e.g. `tokio::sync::Mutex`), it's zero-dep
//! and uses a single waker slot: adequate for the "single task, multiple
//! ordered operations" pattern in chanapi's stream transports.
//!
//! # Single-waiter semantics
//!
//! Only one waiter is parked at a time; if two futures race to lock the
//! same cell, only the most recently registered waker is preserved.
//! This matches the usage pattern in `StreamTransport` where the cell is
//! held only briefly (one `read_frame` / `write_frame` at a time) and
//! callers naturally serialize. For multi-waiter correctness, upgrade to
//! a real async mutex.
//!
//! # Cancel safety
//!
//! Dropping the lock guard (or the acquire future before it resolves) is
//! cancel-safe — the `held` flag is restored on drop.

use core::cell::UnsafeCell;
use core::future::Future;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll};

use atomic_waker::AtomicWaker;

/// A single-waiter async lock around a `T`.
pub struct LocalLock<T> {
    held: AtomicBool,
    waker: AtomicWaker,
    value: UnsafeCell<T>,
}

// SAFETY: `LocalLock` serializes access to `value` through the `held`
// flag; the `Release`/`Acquire` pairing in `unlock`/`try_lock`
// establishes happens-before between consecutive guards.
unsafe impl<T: Send> Send for LocalLock<T> {}
unsafe impl<T: Send> Sync for LocalLock<T> {}

impl<T> LocalLock<T> {
    /// Create a new lock around `value`.
    pub const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            waker: AtomicWaker::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Consume the lock and return the inner value.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Try to acquire the lock without waiting. Returns `None` if it's
    /// currently held.
    pub fn try_lock(&self) -> Option<LocalLockGuard<'_, T>> {
        // CAS false → true.
        self.held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| LocalLockGuard { lock: self })
    }

    /// Acquire the lock, awaiting if held.
    pub fn lock(&self) -> LockFuture<'_, T> {
        LockFuture { lock: self }
    }

    fn unlock(&self) {
        self.held.store(false, Ordering::Release);
        self.waker.wake();
    }
}

/// RAII guard for [`LocalLock`].
pub struct LocalLockGuard<'a, T> {
    lock: &'a LocalLock<T>,
}

impl<T> Deref for LocalLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: `held` is true for the lifetime of this guard.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for LocalLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: `held` is true for the lifetime of this guard, and
        // `&mut self` guarantees exclusivity.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for LocalLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.unlock();
    }
}

/// Future returned by [`LocalLock::lock`].
pub struct LockFuture<'a, T> {
    lock: &'a LocalLock<T>,
}

impl<'a, T> Future for LockFuture<'a, T> {
    type Output = LocalLockGuard<'a, T>;

    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Fast path.
        if let Some(g) = self.lock.try_lock() {
            return Poll::Ready(g);
        }

        // Slow path: register waker, re-check.
        self.lock.waker.register(cx.waker());
        if let Some(g) = self.lock.try_lock() {
            return Poll::Ready(g);
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::pin;
    use core::task::{RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        const VT: RawWakerVTable = RawWakerVTable::new(|_| RAW, |_| {}, |_| {}, |_| {});
        const RAW: RawWaker = RawWaker::new(core::ptr::null(), &VT);
        unsafe { Waker::from_raw(RAW) }
    }

    fn run<F: Future>(mut fut: F) -> F::Output {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = unsafe { core::pin::Pin::new_unchecked(&mut fut) };
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn lock_and_unlock() {
        let lock = LocalLock::new(42i32);
        {
            let mut g = run(lock.lock());
            *g += 1;
        }
        let g = run(lock.lock());
        assert_eq!(*g, 43);
    }

    #[test]
    fn try_lock_returns_none_when_held() {
        let lock = LocalLock::new(0u8);
        let g = lock.try_lock().unwrap();
        assert!(lock.try_lock().is_none());
        drop(g);
        assert!(lock.try_lock().is_some());
    }

    #[test]
    fn lock_future_becomes_ready_after_drop() {
        let lock = LocalLock::new(0u8);
        let g = lock.try_lock().unwrap();

        // Build a pending lock future.
        let mut fut = pin!(lock.lock());
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

        drop(g);

        // Now the future should be ready.
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(_) => {}
            Poll::Pending => panic!("expected ready after unlock"),
        }
    }
}
