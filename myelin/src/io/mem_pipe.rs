//! An in-memory duplex pipe for testing: two `(reader, writer)` pairs
//! that cross-connect so writes on side A become reads on side B, and
//! vice versa.
//!
//! Backed by a `VecDeque<u8>` under a parking-lot-free plain `Mutex`,
//! with a waker slot per read side. When a reader polls and finds the
//! buffer empty, it parks its waker; the opposing writer wakes it after
//! appending.
//!
//! Not intended for production — just for transport tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use atomic_waker::AtomicWaker;

use super::{AsyncBytesRead, AsyncBytesWrite};

struct PipeInner {
    buf: Mutex<VecDeque<u8>>,
    read_waker: AtomicWaker,
    /// Set when the writer side is dropped; reader returns EOF afterwards.
    closed: Mutex<bool>,
}

impl PipeInner {
    fn new() -> Self {
        Self {
            buf: Mutex::new(VecDeque::new()),
            read_waker: AtomicWaker::new(),
            closed: Mutex::new(false),
        }
    }
}

/// Reader side of a half-duplex pipe.
pub struct PipeReader {
    inner: Arc<PipeInner>,
}

/// Writer side of a half-duplex pipe.
pub struct PipeWriter {
    inner: Arc<PipeInner>,
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        *self.inner.closed.lock().unwrap() = true;
        self.inner.read_waker.wake();
    }
}

/// Error type for `MemPipe` I/O.
#[derive(Debug)]
pub enum PipeError {
    /// Opposite end was dropped.
    Closed,
}

impl core::fmt::Display for PipeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PipeError::Closed => write!(f, "pipe closed"),
        }
    }
}

impl std::error::Error for PipeError {}

/// Create a half-duplex in-memory pipe.
pub fn pipe() -> (PipeReader, PipeWriter) {
    let inner = Arc::new(PipeInner::new());
    (PipeReader { inner: inner.clone() }, PipeWriter { inner })
}

/// Create a full-duplex in-memory pipe: returns `((r_a, w_a), (r_b, w_b))`
/// such that bytes written to `w_a` become readable on `r_b`, and bytes
/// written to `w_b` become readable on `r_a`.
pub fn duplex() -> ((PipeReader, PipeWriter), (PipeReader, PipeWriter)) {
    let (r_b, w_a) = pipe(); // writes on w_a → reads on r_b
    let (r_a, w_b) = pipe(); // writes on w_b → reads on r_a
    ((r_a, w_a), (r_b, w_b))
}

impl AsyncBytesRead for PipeReader {
    type Error = PipeError;

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), PipeError> {
        let mut filled = 0;
        core::future::poll_fn(|cx| {
            loop {
                if filled == buf.len() {
                    return Poll::Ready(Ok(()));
                }
                let mut guard = self.inner.buf.lock().unwrap();
                while filled < buf.len() {
                    match guard.pop_front() {
                        Some(b) => {
                            buf[filled] = b;
                            filled += 1;
                        }
                        None => break,
                    }
                }
                if filled == buf.len() {
                    return Poll::Ready(Ok(()));
                }
                drop(guard);
                self.inner.read_waker.register(cx.waker());
                let guard = self.inner.buf.lock().unwrap();
                if !guard.is_empty() {
                    drop(guard);
                    continue;
                }
                if *self.inner.closed.lock().unwrap() {
                    return Poll::Ready(Err(PipeError::Closed));
                }
                return Poll::Pending;
            }
        })
        .await
    }
}

// Removed: the hand-rolled `ReadExact` future had lifetime-capture
// trouble with `impl Future`. Using an `async fn` with `poll_fn` is
// both simpler and side-steps the issue.

impl AsyncBytesWrite for PipeWriter {
    type Error = PipeError;

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), PipeError> {
        let mut guard = self.inner.buf.lock().unwrap();
        guard.extend(buf.iter().copied());
        drop(guard);
        self.inner.read_waker.wake();
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), PipeError> {
        Ok(())
    }
}
