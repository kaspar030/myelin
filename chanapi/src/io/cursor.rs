//! `std::io::Cursor`-backed test helpers for the async I/O traits.
//!
//! These are identical to [`crate::io::BlockingIo`] over a `Cursor` —
//! the type alias below is just a shorthand the unit tests use.

use std::io::Cursor;

use super::blocking::BlockingIo;

/// A `BlockingIo<Cursor<Vec<u8>>>`, useful for in-memory reads in tests.
pub type CursorRead = BlockingIo<Cursor<Vec<u8>>>;

/// A `BlockingIo<Vec<u8>>` (writable), useful for in-memory writes in tests.
pub type CursorWrite = BlockingIo<Vec<u8>>;

/// Construct a `CursorRead` from bytes.
pub fn cursor_read(bytes: Vec<u8>) -> CursorRead {
    BlockingIo(Cursor::new(bytes))
}

/// Construct an empty `CursorWrite`.
pub fn cursor_write() -> CursorWrite {
    BlockingIo(Vec::new())
}
