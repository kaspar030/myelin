//! Framing layer: how bytes are chunked on the wire.
//!
//! A framer turns a stream of bytes into discrete frames (length-delimited
//! chunks). The framing layer is independent of serialization — it just
//! moves `&[u8]` in and out.

use std::io::{self, Read, Write};

/// Write a single frame to a writer.
pub trait FrameWriter {
    type Error;

    /// Write `data` as a single frame to `writer`.
    fn write_frame<W: Write>(&self, writer: &mut W, data: &[u8]) -> Result<(), Self::Error>;
}

/// Read a single frame from a reader.
pub trait FrameReader {
    type Error;

    /// Read one frame, returning the payload as a `Vec<u8>`.
    fn read_frame<R: Read>(&self, reader: &mut R) -> Result<Vec<u8>, Self::Error>;
}

/// Errors from length-prefix framing.
#[derive(Debug)]
pub enum FramingError {
    /// An I/O error occurred while reading or writing.
    Io(io::Error),
    /// The stream was closed (EOF when reading the length prefix).
    Closed,
}

impl core::fmt::Display for FramingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FramingError::Io(e) => write!(f, "I/O error: {e}"),
            FramingError::Closed => write!(f, "stream closed"),
        }
    }
}

impl From<io::Error> for FramingError {
    fn from(e: io::Error) -> Self {
        FramingError::Io(e)
    }
}

/// 4-byte little-endian u32 length-prefix framing.
///
/// Wire format: `[u32 LE length][payload bytes]`.
/// This replicates the framing used by the original `PostcardStream`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LengthPrefixed;

impl FrameWriter for LengthPrefixed {
    type Error = FramingError;

    fn write_frame<W: Write>(&self, writer: &mut W, data: &[u8]) -> Result<(), FramingError> {
        let len = data.len() as u32;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }
}

impl FrameReader for LengthPrefixed {
    type Error = FramingError;

    fn read_frame<R: Read>(&self, reader: &mut R) -> Result<Vec<u8>, FramingError> {
        // Read 4-byte length prefix.
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(FramingError::Closed);
            }
            Err(e) => return Err(FramingError::Io(e)),
        }
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read payload.
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)?;
        Ok(buf)
    }
}
