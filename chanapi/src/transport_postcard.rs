//! Postcard transport over any `Read + Write` stream.
//!
//! Serializes requests/responses with postcard, uses length-prefix framing
//! (4-byte little-endian u32 length, then payload).
//! Works over stdio, TCP, serial — anything implementing `std::io::Read + Write`.
//!
//! The stream transport operates on chanapi's async I/O traits
//! ([`AsyncBytesRead`](crate::io::AsyncBytesRead) /
//! [`AsyncBytesWrite`](crate::io::AsyncBytesWrite)); `PostcardStream`
//! adapts plain sync `Read`/`Write` into those traits via the
//! [`BlockingIo`](crate::io::BlockingIo) newtype. The `async fn` bodies
//! therefore never yield (the sync I/O completes inline) and can be run
//! with the trivial `BlockOn` used by `testing-stdio` and friends.

use crate::io::BlockingIo;
use crate::stream::{
    FramingError, LengthPrefixed, PostcardCodec, Sequential, StreamReplyToken, StreamTransport,
    StreamTransportError,
};

/// Postcard transport over a byte stream with length-prefix framing.
///
/// Type alias over [`StreamTransport`] with:
/// - [`LengthPrefixed`] framing (4-byte LE u32 length prefix)
/// - [`PostcardCodec`] serialization
/// - [`Sequential`] reply routing (one request at a time)
///
/// `R` and `W` are plain `std::io::Read` / `Write` types — they are wrapped
/// in a [`BlockingIo`] adapter internally so the async stack runs them as
/// synchronous no-yield operations.
pub type PostcardStream<R, W, Incoming, Outgoing> = StreamTransport<
    BlockingIo<R>,
    BlockingIo<W>,
    LengthPrefixed,
    PostcardCodec,
    Sequential,
    Incoming,
    Outgoing,
>;

/// Errors from the postcard stream transport.
///
/// Alias for [`StreamTransportError`] specialized to length-prefix framing
/// (over `std::io::Error`) and postcard encoding errors.
pub type PostcardStreamError =
    StreamTransportError<FramingError<std::io::Error>, postcard::Error>;

/// Reply token for PostcardStream — just a marker, reply goes to the same stream.
pub type PostcardReplyToken = StreamReplyToken<Sequential>;

/// Construct a [`PostcardStream`] from sync `Read`/`Write` halves.
///
/// Convenience wrapper so callers don't need to spell `BlockingIo::new` themselves.
pub fn new_postcard_stream<R, W, Incoming, Outgoing>(
    reader: R,
    writer: W,
) -> PostcardStream<R, W, Incoming, Outgoing>
where
    R: std::io::Read,
    W: std::io::Write,
{
    PostcardStream::new(BlockingIo::new(reader), BlockingIo::new(writer))
}
