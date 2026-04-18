//! Adapter: implement [`AsyncBytesRead`]/[`AsyncBytesWrite`] for any
//! type bounded by [`futures_io::AsyncRead`]/[`AsyncWrite`].
//!
//! This covers smol's `smol::Async<T>`, async-std, and any
//! `futures`-io-compatible stream. Enable via the `futures-io` feature.
//!
//! The adapters are hand-rolled poll loops — we depend only on the
//! `futures-io` trait crate, not on `futures-lite` or `futures` in
//! non-dev builds.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::io;

use futures_io::{AsyncRead, AsyncWrite};

use super::{AsyncBytesRead, AsyncBytesWrite};

/// Newtype: wrap a `futures_io::AsyncRead` as an [`AsyncBytesRead`].
pub struct FuturesIoReader<R>(pub R);

impl<R> FuturesIoReader<R> {
    /// Wrap `inner`.
    pub const fn new(inner: R) -> Self {
        Self(inner)
    }

    /// Unwrap.
    pub fn into_inner(self) -> R {
        self.0
    }
}

impl<R: AsyncRead + Unpin> AsyncBytesRead for FuturesIoReader<R> {
    type Error = io::Error;

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), io::Error> {
        ReadExactFuture { reader: &mut self.0, buf, filled: 0 }.await
    }
}

struct ReadExactFuture<'a, R: ?Sized> {
    reader: &'a mut R,
    buf: &'a mut [u8],
    filled: usize,
}

impl<'a, R: AsyncRead + Unpin + ?Sized> Future for ReadExactFuture<'a, R> {
    type Output = Result<(), io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        while this.filled < this.buf.len() {
            let slice = &mut this.buf[this.filled..];
            match Pin::new(&mut *this.reader).poll_read(cx, slice) {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "read_exact: EOF",
                    )));
                }
                Poll::Ready(Ok(n)) => {
                    this.filled += n;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }
}

/// Newtype: wrap a `futures_io::AsyncWrite` as an [`AsyncBytesWrite`].
pub struct FuturesIoWriter<W>(pub W);

impl<W> FuturesIoWriter<W> {
    /// Wrap `inner`.
    pub const fn new(inner: W) -> Self {
        Self(inner)
    }

    /// Unwrap.
    pub fn into_inner(self) -> W {
        self.0
    }
}

impl<W: AsyncWrite + Unpin> AsyncBytesWrite for FuturesIoWriter<W> {
    type Error = io::Error;

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), io::Error> {
        WriteAllFuture { writer: &mut self.0, buf, written: 0 }.await
    }

    async fn flush(&mut self) -> Result<(), io::Error> {
        FlushFuture { writer: &mut self.0 }.await
    }
}

struct WriteAllFuture<'a, W: ?Sized> {
    writer: &'a mut W,
    buf: &'a [u8],
    written: usize,
}

impl<'a, W: AsyncWrite + Unpin + ?Sized> Future for WriteAllFuture<'a, W> {
    type Output = Result<(), io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        while this.written < this.buf.len() {
            let slice = &this.buf[this.written..];
            match Pin::new(&mut *this.writer).poll_write(cx, slice) {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write_all: wrote zero",
                    )));
                }
                Poll::Ready(Ok(n)) => this.written += n,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }
}

struct FlushFuture<'a, W: ?Sized> {
    writer: &'a mut W,
}

impl<'a, W: AsyncWrite + Unpin + ?Sized> Future for FlushFuture<'a, W> {
    type Output = Result<(), io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        Pin::new(&mut *this.writer).poll_flush(cx)
    }
}
