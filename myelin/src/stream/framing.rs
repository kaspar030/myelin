//! Framing layer: how bytes are chunked on the wire.
//!
//! A framer turns a stream of bytes into discrete frames (length-delimited
//! chunks). The framing layer is independent of serialization — it just
//! moves `&[u8]` in and out.
//!
//! This layer is async: it reads/writes through
//! [`AsyncBytesRead`](crate::io::AsyncBytesRead) /
//! [`AsyncBytesWrite`](crate::io::AsyncBytesWrite) rather than sync
//! `std::io`. Framing error is generic over the underlying reader /
//! writer error so the layer stays runtime-neutral.

use core::future::Future;

use crate::io::{AsyncBytesRead, AsyncBytesWrite};

/// Write a single frame to an async writer.
pub trait FrameWriter {
    /// Framing-level error (wraps the writer's `Error`).
    type Error<WE>;

    /// Write `data` as a single frame to `writer`.
    fn write_frame<W: AsyncBytesWrite>(
        &self,
        writer: &mut W,
        data: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error<W::Error>>>;
}

/// Read a single frame from an async reader.
pub trait FrameReader {
    /// Framing-level error (wraps the reader's `Error`).
    type Error<RE>;

    /// Read one frame, returning the payload as a `Vec<u8>`.
    fn read_frame<R: AsyncBytesRead>(
        &self,
        reader: &mut R,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error<R::Error>>>;
}

/// Errors from length-prefix framing, generic over the underlying
/// reader / writer error `E`.
#[derive(Debug)]
pub enum FramingError<E> {
    /// An I/O error occurred while reading or writing.
    Io(E),
    /// The stream was closed mid-read.
    Closed,
}

impl<E: core::fmt::Display> core::fmt::Display for FramingError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FramingError::Io(e) => write!(f, "I/O error: {e}"),
            FramingError::Closed => write!(f, "stream closed"),
        }
    }
}

// Convenience: `std::io::Error` converts to `FramingError<std::io::Error>`.
// UnexpectedEof becomes `Closed`, everything else is `Io`.
impl From<std::io::Error> for FramingError<std::io::Error> {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FramingError::Closed
        } else {
            FramingError::Io(e)
        }
    }
}

/// 4-byte little-endian u32 length-prefix framing.
///
/// Wire format: `[u32 LE length][payload bytes]`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LengthPrefixed;

impl FrameWriter for LengthPrefixed {
    type Error<WE> = FramingError<WE>;

    async fn write_frame<W: AsyncBytesWrite>(
        &self,
        writer: &mut W,
        data: &[u8],
    ) -> Result<(), FramingError<W::Error>> {
        let len = data.len() as u32;
        writer
            .write_all(&len.to_le_bytes())
            .await
            .map_err(FramingError::Io)?;
        writer.write_all(data).await.map_err(FramingError::Io)?;
        writer.flush().await.map_err(FramingError::Io)?;
        Ok(())
    }
}

impl FrameReader for LengthPrefixed {
    type Error<RE> = FramingError<RE>;

    async fn read_frame<R: AsyncBytesRead>(
        &self,
        reader: &mut R,
    ) -> Result<Vec<u8>, FramingError<R::Error>> {
        // Read 4-byte length prefix.
        let mut len_buf = [0u8; 4];
        // Note: readers that distinguish EOF should map it to an error
        // whose `Display` or kind lets us detect it. For `std::io::Error`
        // UnexpectedEof, `BlockingIo` surfaces that verbatim; callers
        // wanting to treat EOF as `Closed` should match on the wrapped
        // error. We can't do a blanket translation here without taking
        // a dependency on `std::io::ErrorKind`.
        reader.read_exact(&mut len_buf).await.map_err(FramingError::Io)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read payload.
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await.map_err(FramingError::Io)?;
        Ok(buf)
    }
}
