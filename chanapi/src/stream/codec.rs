//! Encoding layer: how types become bytes.
//!
//! A codec serializes/deserializes Rust types to/from byte slices.
//! The encoding layer is independent of framing — it just converts
//! between `T` and `&[u8]` / `Vec<u8>`.

use serde::{Deserialize, Serialize};

/// Encode a value into bytes.
pub trait Encoder {
    type Error;

    /// Serialize `value` into a new `Vec<u8>`.
    fn encode_to_vec<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Self::Error>;
}

/// Decode a value from bytes.
pub trait Decoder {
    type Error;

    /// Deserialize a `T` from the given byte slice.
    fn decode<T: for<'de> Deserialize<'de>>(&self, bytes: &[u8]) -> Result<T, Self::Error>;
}

/// Postcard codec — wraps `postcard::to_stdvec` / `postcard::from_bytes`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PostcardCodec;

impl Encoder for PostcardCodec {
    type Error = postcard::Error;

    fn encode_to_vec<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(value)
    }
}

impl Decoder for PostcardCodec {
    type Error = postcard::Error;

    fn decode<T: for<'de> Deserialize<'de>>(&self, bytes: &[u8]) -> Result<T, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
