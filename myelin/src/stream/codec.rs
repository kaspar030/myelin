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
#[cfg(feature = "postcard")]
#[derive(Debug, Default, Clone, Copy)]
pub struct PostcardCodec;

#[cfg(feature = "postcard")]
impl Encoder for PostcardCodec {
    type Error = postcard::Error;

    fn encode_to_vec<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(value)
    }
}

#[cfg(feature = "postcard")]
impl Decoder for PostcardCodec {
    type Error = postcard::Error;

    fn decode<T: for<'de> Deserialize<'de>>(&self, bytes: &[u8]) -> Result<T, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// CBOR codec — wraps `minicbor_serde::to_vec` / `minicbor_serde::from_slice`.
///
/// Produces a self-describing wire format, useful when the transport
/// must be debuggable (e.g. pretty-printed by external tools) or when
/// peers that don't share a compile-time type registry need to
/// inspect the payload (bridges to other protocols).
#[cfg(feature = "cbor")]
#[derive(Debug, Default, Clone, Copy)]
pub struct CborCodec;

/// Unified error for [`CborCodec`]. Wraps the distinct encode / decode
/// error types so `Encoder::Error == Decoder::Error`, which `StreamTransport`
/// requires.
#[cfg(feature = "cbor")]
#[derive(Debug)]
pub enum CborCodecError {
    /// Encoding failure. `minicbor-serde`'s writer is `Infallible`, so in
    /// practice this only surfaces serializer-side errors (e.g. a
    /// [`Serialize`] impl returning a custom error).
    Encode(minicbor_serde::error::EncodeError<core::convert::Infallible>),
    /// Decoding failure (malformed CBOR, type mismatch, trailing bytes, …).
    Decode(minicbor_serde::error::DecodeError),
}

#[cfg(feature = "cbor")]
impl core::fmt::Display for CborCodecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "cbor encode error: {e}"),
            Self::Decode(e) => write!(f, "cbor decode error: {e}"),
        }
    }
}

#[cfg(all(feature = "cbor", feature = "std"))]
impl std::error::Error for CborCodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(e) => Some(e),
            Self::Decode(e) => Some(e),
        }
    }
}

#[cfg(feature = "cbor")]
impl From<minicbor_serde::error::EncodeError<core::convert::Infallible>> for CborCodecError {
    fn from(e: minicbor_serde::error::EncodeError<core::convert::Infallible>) -> Self {
        Self::Encode(e)
    }
}

#[cfg(feature = "cbor")]
impl From<minicbor_serde::error::DecodeError> for CborCodecError {
    fn from(e: minicbor_serde::error::DecodeError) -> Self {
        Self::Decode(e)
    }
}

#[cfg(feature = "cbor")]
impl Encoder for CborCodec {
    type Error = CborCodecError;

    fn encode_to_vec<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Self::Error> {
        minicbor_serde::to_vec(value).map_err(CborCodecError::Encode)
    }
}

#[cfg(feature = "cbor")]
impl Decoder for CborCodec {
    type Error = CborCodecError;

    fn decode<T: for<'de> Deserialize<'de>>(&self, bytes: &[u8]) -> Result<T, Self::Error> {
        minicbor_serde::from_slice(bytes).map_err(CborCodecError::Decode)
    }
}

#[cfg(all(test, feature = "cbor"))]
mod cbor_tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Nested {
        id: u32,
        label: String,
        flags: Vec<bool>,
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Outer {
        name: String,
        maybe: Option<u64>,
        items: Vec<Nested>,
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Msg {
        Ping,
        Echo(String),
        Data { value: i64, tag: Option<String> },
    }

    fn round_trip<T>(v: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + core::fmt::Debug + PartialEq,
    {
        let bytes = CborCodec.encode_to_vec(v).expect("encode");
        let back: T = CborCodec.decode(&bytes).expect("decode");
        assert_eq!(&back, v);
    }

    #[test]
    fn round_trips_primitives() {
        round_trip(&0u8);
        round_trip(&u32::MAX);
        round_trip(&-12345i64);
        round_trip(&true);
        round_trip(&String::from("hello cbor"));
    }

    #[test]
    fn round_trips_collections() {
        let v: Vec<u32> = vec![1, 2, 3, 42];
        round_trip(&v);

        let none: Option<u32> = None;
        let some: Option<u32> = Some(7);
        round_trip(&none);
        round_trip(&some);
    }

    #[test]
    fn round_trips_nested_struct() {
        let v = Outer {
            name: "outer".into(),
            maybe: Some(99),
            items: vec![
                Nested {
                    id: 1,
                    label: "first".into(),
                    flags: vec![true, false, true],
                },
                Nested {
                    id: 2,
                    label: "second".into(),
                    flags: vec![],
                },
            ],
        };
        round_trip(&v);
    }

    #[test]
    fn round_trips_enum_variants() {
        round_trip(&Msg::Ping);
        round_trip(&Msg::Echo("hi".into()));
        round_trip(&Msg::Data {
            value: -42,
            tag: Some("x".into()),
        });
        round_trip(&Msg::Data {
            value: 0,
            tag: None,
        });
    }
}
