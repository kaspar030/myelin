//! Composed stream transport built from Framing × Encoding × ReplyRouting.
//!
//! `StreamTransport` implements [`ClientTransport`] and [`ServerTransport`]
//! by composing a framer, a codec, and a reply router over a read/write
//! byte stream.

use std::cell::RefCell;
use std::io::{Read, Write};

use core::marker::PhantomData;
use serde::{Deserialize, Serialize};

use crate::stream::codec::{Decoder, Encoder};
use crate::stream::framing::{FrameReader, FrameWriter};
use crate::stream::routing::ReplyRouter;
use crate::transport::{ClientTransport, ServerTransport};

/// A transport composed from a framer, codec, and reply router.
///
/// - `R` / `W`: reader / writer halves of the underlying byte stream.
/// - `Framer`: framing strategy (e.g., `LengthPrefixed`).
/// - `Codec`: serialization codec (e.g., `PostcardCodec`).
/// - `Router`: reply routing strategy (e.g., `Sequential`).
/// - `Req` / `Resp`: the request and response message types.
pub struct StreamTransport<R, W, Framer, Codec, Router, Req, Resp> {
    inner: RefCell<StreamTransportInner<R, W>>,
    framer: Framer,
    codec: Codec,
    _router: Router,
    _phantom: PhantomData<(Req, Resp)>,
}

struct StreamTransportInner<R, W> {
    reader: R,
    writer: W,
}

/// Constructor when all layers implement `Default` (e.g., unit/ZST types).
impl<R, W, Framer, Codec, Router, Req, Resp>
    StreamTransport<R, W, Framer, Codec, Router, Req, Resp>
where
    Framer: Default,
    Codec: Default,
    Router: Default,
{
    /// Create a new transport with default layer instances.
    ///
    /// This is the primary constructor for transports built from zero-sized
    /// layers like `LengthPrefixed`, `PostcardCodec`, and `Sequential`.
    pub fn new(reader: R, writer: W) -> Self {
        Self::with_layers(reader, writer, Framer::default(), Codec::default(), Router::default())
    }
}

impl<R, W, Framer, Codec, Router, Req, Resp>
    StreamTransport<R, W, Framer, Codec, Router, Req, Resp>
{
    /// Create a new transport with explicitly provided layer instances.
    pub fn with_layers(
        reader: R,
        writer: W,
        framer: Framer,
        codec: Codec,
        router: Router,
    ) -> Self {
        Self {
            inner: RefCell::new(StreamTransportInner { reader, writer }),
            framer,
            codec,
            _router: router,
            _phantom: PhantomData,
        }
    }
}

/// Errors from a `StreamTransport`.
///
/// Wraps the framing and codec errors into a single enum.
#[derive(Debug)]
pub enum StreamTransportError<F, C> {
    /// An error from the framing layer.
    Framing(F),
    /// An error from the codec layer.
    Codec(C),
}

impl<F: core::fmt::Display, C: core::fmt::Display> core::fmt::Display
    for StreamTransportError<F, C>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StreamTransportError::Framing(e) => write!(f, "{e}"),
            StreamTransportError::Codec(e) => write!(f, "codec error: {e}"),
        }
    }
}

impl<T, F: core::fmt::Debug, C: core::fmt::Debug> crate::TransportResult<T>
    for StreamTransportError<F, C>
{
    type Output = Result<T, crate::CallError<StreamTransportError<F, C>>>;

    fn into_output(result: Result<T, Self>) -> Self::Output {
        result.map_err(crate::CallError::Transport)
    }
}

/// Reply token for stream transports.
///
/// For `Sequential` routing, this is a unit-like marker — the reply goes
/// to the same stream. Future routers may embed a slot ID here.
pub struct StreamReplyToken<Router> {
    _phantom: PhantomData<Router>,
}

impl<Router> StreamReplyToken<Router> {
    fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

// -- ClientTransport: sends Req (= Outgoing), receives Resp (= Incoming) --

impl<R, W, Framer, Codec, Router, Req, Resp> ClientTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, Router, Resp, Req>
where
    R: Read,
    W: Write,
    Framer: FrameWriter + FrameReader<Error = <Framer as FrameWriter>::Error>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Router: ReplyRouter,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
    <Framer as FrameWriter>::Error: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error = StreamTransportError<<Framer as FrameWriter>::Error, <Codec as Encoder>::Error>;

    /// # Cancel Safety
    ///
    /// This transport uses blocking I/O — the async functions resolve
    /// immediately and never yield. There are no `.await` cancel points,
    /// so cancellation cannot occur mid-call.
    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let bytes = self.codec.encode_to_vec(&req).map_err(StreamTransportError::Codec)?;
        {
            let mut inner = self.inner.borrow_mut();
            self.framer
                .write_frame(&mut inner.writer, &bytes)
                .map_err(StreamTransportError::Framing)?;
        }
        let buf = {
            let mut inner = self.inner.borrow_mut();
            self.framer
                .read_frame(&mut inner.reader)
                .map_err(StreamTransportError::Framing)?
        };
        self.codec.decode(&buf).map_err(StreamTransportError::Codec)
    }
}

// -- ServerTransport: receives Req (= Incoming), sends Resp (= Outgoing) --

impl<R, W, Framer, Codec, Router, Req, Resp> ServerTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, Router, Req, Resp>
where
    R: Read,
    W: Write,
    Framer: FrameWriter + FrameReader<Error = <Framer as FrameWriter>::Error>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Router: ReplyRouter,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    <Framer as FrameWriter>::Error: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error = StreamTransportError<<Framer as FrameWriter>::Error, <Codec as Encoder>::Error>;
    type ReplyToken = StreamReplyToken<Router>;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let buf = {
            let mut inner = self.inner.borrow_mut();
            self.framer
                .read_frame(&mut inner.reader)
                .map_err(StreamTransportError::Framing)?
        };
        let req = self.codec.decode(&buf).map_err(StreamTransportError::Codec)?;
        Ok((req, StreamReplyToken::new()))
    }

    async fn reply(&self, _token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        let bytes = self.codec.encode_to_vec(&resp).map_err(StreamTransportError::Codec)?;
        let mut inner = self.inner.borrow_mut();
        self.framer
            .write_frame(&mut inner.writer, &bytes)
            .map_err(StreamTransportError::Framing)
    }
}
