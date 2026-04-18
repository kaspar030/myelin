//! Blocking-I/O adapter: wrap a `std::io::Read`/`Write` so it implements
//! [`AsyncBytesRead`]/[`AsyncBytesWrite`].
//!
//! The `async fn` methods run the sync operation inline — they never
//! yield to a runtime. This is intended for:
//!
//! - `transport_postcard::PostcardStream` over stdio/TCP in synchronous
//!   programs that poll-run with a trivial [`BlockOn`] (see
//!   `chanapi::block_on::BlockOn`).
//! - Unit tests that thread bytes through `std::io::Cursor`.
//!
//! Do **not** use this over a real async runtime's byte stream: the
//! blocking `read`/`write` calls would stall the executor thread. For
//! that, use [`crate::io::futures_io`] or [`crate::io::tokio_io`].

use std::io::{self, Read, Write};

use super::{AsyncBytesRead, AsyncBytesWrite};

/// Newtype adapter: `BlockingIo<T>` implements the async byte traits for
/// any sync `Read`/`Write`.
#[derive(Debug, Default, Clone, Copy)]
pub struct BlockingIo<T>(pub T);

impl<T> BlockingIo<T> {
    /// Wrap `inner`.
    pub const fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Return a mutable reference to the wrapped value.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }

    /// Unwrap.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: Read> AsyncBytesRead for BlockingIo<T> {
    type Error = io::Error;

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), io::Error> {
        self.0.read_exact(buf)
    }
}

impl<T: Write> AsyncBytesWrite for BlockingIo<T> {
    type Error = io::Error;

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), io::Error> {
        self.0.write_all(buf)
    }

    async fn flush(&mut self) -> Result<(), io::Error> {
        self.0.flush()
    }
}
