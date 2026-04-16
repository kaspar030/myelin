//! Postcard transport over any `Read + Write` stream.
//!
//! Serializes requests/responses with postcard, uses length-prefix framing
//! (4-byte little-endian u32 length, then payload).
//! Works over stdio, TCP, serial — anything implementing `std::io::Read + Write`.
//!
//! This module is a backward-compatible re-export layer built on the
//! composable [`stream`](crate::stream) module.

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
pub type PostcardStream<R, W, Incoming, Outgoing> =
    StreamTransport<R, W, LengthPrefixed, PostcardCodec, Sequential, Incoming, Outgoing>;

/// Errors from the postcard stream transport.
///
/// Alias for [`StreamTransportError`] specialized to length-prefix framing
/// and postcard encoding errors.
pub type PostcardStreamError = StreamTransportError<FramingError, postcard::Error>;

/// Reply token for PostcardStream — just a marker, reply goes to the same stream.
pub type PostcardReplyToken = StreamReplyToken<Sequential>;
