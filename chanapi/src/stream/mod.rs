//! Layered stream transport: Framing × Encoding × ReplyRouting.
//!
//! This module decomposes the monolithic postcard-over-stream transport
//! into three composable layers:
//!
//! - **Framing** ([`framing`]) — how bytes are chunked on the wire.
//! - **Encoding** ([`codec`]) — how types become bytes.
//! - **Reply routing** ([`routing`]) — how responses are matched to callers.
//!
//! The layers are composed into a single [`StreamTransport`] that implements
//! [`ClientTransport`](crate::transport::ClientTransport) and
//! [`ServerTransport`](crate::transport::ServerTransport).

pub mod codec;
pub mod framing;
pub mod routing;
pub mod transport;

pub use codec::{Decoder, Encoder, PostcardCodec};
pub use framing::{FrameReader, FrameWriter, FramingError, LengthPrefixed};
pub use routing::{ReplyRouter, Sequential};
pub use transport::{StreamReplyToken, StreamTransport, StreamTransportError};
