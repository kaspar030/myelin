//! Async byte-stream I/O traits owned by myelin.
//!
//! These traits are the minimal surface the stream framing layer needs:
//! `read_exact`, `write_all`, `flush`. They return `impl Future` so they
//! can be implemented by any async runtime's byte-stream primitives.
//!
//! myelin core stays runtime-neutral: nothing here depends on tokio,
//! smol, or `futures`. Adapters for specific runtimes live in sibling
//! modules gated on feature flags:
//!
//! - [`futures_io`] — for any `futures::io::AsyncRead`/`AsyncWrite` type
//!   (feature `futures-io`). Covers smol's `Async<T>`, async-std, etc.
//! - [`tokio_io`] — for `tokio::io::AsyncRead`/`AsyncWrite` types
//!   (feature `tokio-io`).
//! - [`blocking`] — for plain `std::io::Read`/`Write` types; the async
//!   methods are synchronous no-`await` wrappers. Useful for stdio-based
//!   binaries and tests, not for any real async runtime.
//!
//! ## Why not use the `futures-io` trait crate directly?
//!
//! Using our own trait lets every adapter be a thin newtype without
//! pulling `futures-io` into myelin core as a mandatory dep, and keeps
//! the error type flexible (each impl picks its own `Error` type rather
//! than being pinned to `std::io::Error`).

use core::future::Future;

pub mod blocking;
pub mod local_lock;

#[cfg(any(test, feature = "io-test-utils"))]
pub mod cursor;

#[cfg(any(test, feature = "io-test-utils"))]
pub mod mem_pipe;

#[cfg(feature = "futures-io")]
pub mod futures_io;

#[cfg(feature = "tokio-io")]
pub mod tokio_io;

pub use blocking::BlockingIo;
pub use local_lock::LocalLock;

/// An asynchronous byte-stream reader.
///
/// Implementations must provide `read_exact`: fill the buffer completely
/// or return an error. EOF mid-read is an error (the convention matches
/// `std::io::Read::read_exact`).
pub trait AsyncBytesRead {
    /// The error type produced by this reader.
    type Error;

    /// Read exactly `buf.len()` bytes into `buf`.
    ///
    /// Returns `Err` if EOF is reached before the buffer is filled.
    fn read_exact(&mut self, buf: &mut [u8]) -> impl Future<Output = Result<(), Self::Error>>;
}

/// An asynchronous byte-stream writer.
pub trait AsyncBytesWrite {
    /// The error type produced by this writer.
    type Error;

    /// Write all of `buf` to the underlying stream.
    fn write_all(&mut self, buf: &[u8]) -> impl Future<Output = Result<(), Self::Error>>;

    /// Flush any buffered bytes to the underlying stream.
    fn flush(&mut self) -> impl Future<Output = Result<(), Self::Error>>;
}

// Blanket impl: `&mut T` is a reader/writer if `T` is. This makes it easy
// to pass a borrowed reader/writer into helpers without giving up ownership.
impl<T: AsyncBytesRead + ?Sized> AsyncBytesRead for &mut T {
    type Error = T::Error;

    fn read_exact(&mut self, buf: &mut [u8]) -> impl Future<Output = Result<(), Self::Error>> {
        (**self).read_exact(buf)
    }
}

impl<T: AsyncBytesWrite + ?Sized> AsyncBytesWrite for &mut T {
    type Error = T::Error;

    fn write_all(&mut self, buf: &[u8]) -> impl Future<Output = Result<(), Self::Error>> {
        (**self).write_all(buf)
    }

    fn flush(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        (**self).flush()
    }
}
