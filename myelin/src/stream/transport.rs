//! Composed stream transport built from Framing × Encoding × ReplyRouting.
//!
//! `StreamTransport` implements [`ClientTransport`] and [`ServerTransport`]
//! by composing a framer, a codec, and a reply router over an async
//! byte stream (a reader and a writer).
//!
//! Single-owner-at-a-time access to the reader and writer is mediated by
//! [`LocalLock`] — this replaces the old `RefCell`-based shared access
//! (which was unsound for real async I/O since `RefMut` can't be held
//! across `.await`). `LocalLock` is cancel-safe and zero-dep.
//!
//! For [`Sequential`](super::routing::Sequential) routing, this is a
//! simple send-then-receive loop. For
//! [`MuxedSlots`](super::routing::MuxedSlots) routing, the transport
//! manages concurrent requests via cooperative demuxing: each `call()`
//! locks the reader, reads a frame, and dispatches replies to the
//! correct slot.

use core::marker::PhantomData;
use serde::{Deserialize, Serialize};

use crate::io::{AsyncBytesRead, AsyncBytesWrite, LocalLock};
use crate::stream::codec::{Decoder, Encoder};
use crate::stream::framing::{FrameReader, FrameWriter};
use crate::stream::routing::{
    MuxedReplyToken, MuxedSlots, ReplyRouter, RouterSlotHandle, Sequential,
};
use crate::transport::{ClientTransport, ServerTransport};

/// Storage strategy for the reply router inside [`StreamTransport`].
///
/// Implemented for the concrete routers shipped by this crate
/// ([`Sequential`] and [`MuxedSlots<N, BUF>`]) both inline and behind a
/// [`Box`]. The trait exists so a single `StreamTransport` definition
/// can carry either an inline-stored or a heap-stored router without
/// changing call sites — every protocol impl reaches the underlying
/// router via [`RouterStorage::router`].
///
/// The boxed form is intended for routers whose state is too large to
/// materialise on the stack during construction. See
/// [`MuxedSlots::new_boxed`](super::routing::MuxedSlots::new_boxed) and
/// [`StreamTransport::with_boxed_router`].
///
/// # Why explicit impls
///
/// The natural shape — blanket impls `impl<R: ReplyRouter> RouterStorage
/// for R` and `impl<R: ReplyRouter> RouterStorage for Box<R>` — would
/// overlap because a downstream crate is permitted to add
/// `impl ReplyRouter for Box<SomeRouter>` (the orphan rules allow it for
/// types it owns). Instead, [`RouterStorage`] is implemented explicitly
/// for the four concrete combinations used in this crate. Third-party
/// `ReplyRouter` impls that want to plug into [`StreamTransport`] can
/// add their own `RouterStorage` impls in the same crate as the router
/// type.
pub trait RouterStorage {
    /// The underlying router type — what hot-path code dispatches
    /// through.
    type Router: ReplyRouter;

    /// Borrow the stored router.
    fn router(&self) -> &Self::Router;
}

impl RouterStorage for Sequential {
    type Router = Sequential;
    #[inline]
    fn router(&self) -> &Self::Router {
        self
    }
}

impl RouterStorage for Box<Sequential> {
    type Router = Sequential;
    #[inline]
    fn router(&self) -> &Self::Router {
        self
    }
}

impl<const N: usize, const BUF: usize> RouterStorage for MuxedSlots<N, BUF> {
    type Router = MuxedSlots<N, BUF>;
    #[inline]
    fn router(&self) -> &Self::Router {
        self
    }
}

impl<const N: usize, const BUF: usize> RouterStorage for Box<MuxedSlots<N, BUF>> {
    type Router = MuxedSlots<N, BUF>;
    #[inline]
    fn router(&self) -> &Self::Router {
        self
    }
}

/// A transport composed from a framer, codec, and reply router.
///
/// - `R` / `W`: async reader / writer halves of the underlying byte stream.
/// - `Framer`: framing strategy (e.g., `LengthPrefixed`).
/// - `Codec`: serialization codec (e.g., `PostcardCodec`).
/// - `Router`: reply routing strategy (e.g., `Sequential`, `MuxedSlots`).
/// - `Req` / `Resp`: the request and response message types.
/// - `S`: storage for the router — either the router inline (the
///   default, `S = Router`) or `Box<Router>` (heap-allocated). Use
///   [`with_boxed_router`](Self::with_boxed_router) to construct the
///   boxed form; see that constructor for when to prefer it.
pub struct StreamTransport<R, W, Framer, Codec, Router, Req, Resp, S = Router>
where
    S: RouterStorage<Router = Router>,
{
    reader: LocalLock<R>,
    writer: LocalLock<W>,
    framer: Framer,
    codec: Codec,
    router: S,
    _phantom: PhantomData<(Router, Req, Resp)>,
}

impl<R, W, Framer, Codec, Router, Req, Resp> StreamTransport<R, W, Framer, Codec, Router, Req, Resp>
where
    Framer: Default,
    Codec: Default,
    Router: Default + RouterStorage<Router = Router>,
{
    /// Create a new transport with default layer instances.
    ///
    /// Stores the router inline. For routers whose state is too large
    /// to materialise on the stack (e.g. `MuxedSlots<N, BUF>` with
    /// large `BUF`), prefer
    /// [`with_boxed_router`](Self::with_boxed_router) and a heap-built
    /// router from
    /// [`MuxedSlots::new_boxed`](super::routing::MuxedSlots::new_boxed).
    pub fn new(reader: R, writer: W) -> Self {
        Self::with_layers(
            reader,
            writer,
            Framer::default(),
            Codec::default(),
            Router::default(),
        )
    }
}

impl<R, W, Framer, Codec, Router, Req, Resp>
    StreamTransport<R, W, Framer, Codec, Router, Req, Resp>
where
    Router: RouterStorage<Router = Router>,
{
    /// Create a new transport with explicitly provided layer instances.
    ///
    /// The `router` is stored inline (i.e. by value). When the router's
    /// state is large — e.g. `MuxedSlots<N, BUF>` with `BUF ≥ 256 KiB`
    /// — building it on the stack first risks a stack overflow at
    /// typical runtime worker-thread stack sizes (≈ 1–2 MiB). Use
    /// [`with_boxed_router`](Self::with_boxed_router) together with
    /// [`MuxedSlots::new_boxed`](super::routing::MuxedSlots::new_boxed)
    /// in that case.
    pub fn with_layers(reader: R, writer: W, framer: Framer, codec: Codec, router: Router) -> Self {
        Self {
            reader: LocalLock::new(reader),
            writer: LocalLock::new(writer),
            framer,
            codec,
            router,
            _phantom: PhantomData,
        }
    }
}

impl<R, W, Framer, Codec, Router, Req, Resp>
    StreamTransport<R, W, Framer, Codec, Router, Req, Resp, Box<Router>>
where
    Router: ReplyRouter,
    Box<Router>: RouterStorage<Router = Router>,
{
    /// Create a new transport that owns its router on the heap.
    ///
    /// Pair this with
    /// [`MuxedSlots::new_boxed`](super::routing::MuxedSlots::new_boxed)
    /// to build a `MuxedSlots<N, BUF>` whose `N × BUF` slot array
    /// never lives on the stack:
    ///
    /// ```ignore
    /// use myelin::stream::{LengthPrefixed, MuxedSlots, PostcardCodec, StreamTransport};
    ///
    /// let router = MuxedSlots::<1, 1_048_576>::new_boxed();
    /// let transport: StreamTransport<_, _, LengthPrefixed, PostcardCodec, _, Req, Resp, _> =
    ///     StreamTransport::with_boxed_router(reader, writer, LengthPrefixed, PostcardCodec, router);
    /// ```
    ///
    /// The hot paths dispatch through [`Box`]'s auto-deref — access
    /// pattern is identical to the inline form.
    pub fn with_boxed_router(
        reader: R,
        writer: W,
        framer: Framer,
        codec: Codec,
        router: Box<Router>,
    ) -> Self {
        Self {
            reader: LocalLock::new(reader),
            writer: LocalLock::new(writer),
            framer,
            codec,
            router,
            _phantom: PhantomData,
        }
    }
}

/// Errors from a `StreamTransport`.
///
/// Wraps the framing and codec errors into a single enum. The framing
/// error is generic over the reader-or-writer error channel.
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

// Framing error over reader's stream. We also constrain the writer so its
// framing error type equals the reader's — all concrete adapters
// (`BlockingIo`, `FuturesIoReader/Writer`, `TokioIoReader/Writer`,
// `PipeReader/Writer`) share the same `Error` type for both halves.
type FramingRE<Framer, R> = <Framer as FrameReader>::Error<<R as AsyncBytesRead>::Error>;
type FramingWE<Framer, W> = <Framer as FrameWriter>::Error<<W as AsyncBytesWrite>::Error>;

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

// =========================================================================
// ClientTransport — Sequential
// =========================================================================

impl<R, W, Framer, Codec, Req, Resp, S> ClientTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, Sequential, Resp, Req, S>
where
    R: AsyncBytesRead,
    W: AsyncBytesWrite,
    Framer: FrameWriter + FrameReader,
    <Framer as FrameWriter>::Error<W::Error>: Into<FramingWE<Framer, W>>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
    FramingWE<Framer, W>: core::fmt::Debug,
    FramingRE<Framer, R>: core::fmt::Debug + From<FramingWE<Framer, W>>,
    <Codec as Encoder>::Error: core::fmt::Debug,
    S: RouterStorage<Router = Sequential>,
{
    type Error = StreamTransportError<FramingRE<Framer, R>, <Codec as Encoder>::Error>;

    /// Sequential call: send request, read reply. No routing overhead.
    ///
    /// # Cancel Safety
    ///
    /// The `LocalLock` guards are dropped on cancellation; however,
    /// dropping mid-write may leave a partially-written frame on the
    /// wire, desynchronising the stream. Callers should treat an
    /// interrupted call as unrecoverable for this transport.
    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let bytes = self
            .codec
            .encode_to_vec(&req)
            .map_err(StreamTransportError::Codec)?;
        {
            let mut writer = self.writer.lock().await;
            self.framer
                .write_frame(&mut *writer, &bytes)
                .await
                .map_err(|e| {
                    StreamTransportError::<FramingRE<Framer, R>, _>::Framing(
                        <FramingRE<Framer, R> as From<FramingWE<Framer, W>>>::from(e),
                    )
                })?;
        }
        let buf = {
            let mut reader = self.reader.lock().await;
            self.framer
                .read_frame(&mut *reader)
                .await
                .map_err(StreamTransportError::Framing)?
        };
        self.codec.decode(&buf).map_err(StreamTransportError::Codec)
    }
}

// =========================================================================
// ClientTransport — MuxedSlots<N, BUF>
// =========================================================================

impl<R, W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp, S> ClientTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, MuxedSlots<N, BUF>, Resp, Req, S>
where
    R: AsyncBytesRead,
    W: AsyncBytesWrite,
    Framer: FrameWriter + FrameReader,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
    FramingWE<Framer, W>: core::fmt::Debug,
    FramingRE<Framer, R>: core::fmt::Debug + From<FramingWE<Framer, W>>,
    <Codec as Encoder>::Error: core::fmt::Debug,
    S: RouterStorage<Router = MuxedSlots<N, BUF>>,
{
    type Error = StreamTransportError<FramingRE<Framer, R>, <Codec as Encoder>::Error>;

    /// Muxed call: acquire slot → tag frame → send → cooperative demux → return.
    ///
    /// The I/O is now genuinely async: `LocalLock` is released between
    /// frames so other tasks on the same reader/writer pair can make
    /// progress.
    ///
    /// # Waiter behaviour
    ///
    /// Only one task at a time is reading from the shared reader lock
    /// inside the demux loop; others park on the `LocalLock` and are
    /// woken once the current reader releases. This is adequate for
    /// the typical "several concurrent calls through one stream"
    /// pattern. For heavier fan-in, prefer the
    /// [`DuplexStreamTransport`](crate::stream::DuplexStreamTransport)
    /// which uses a dedicated pump task.
    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        // 1. Acquire a routing slot.
        let slot = self.router.router().acquire().await.map_err(|e| match e {})?;

        // 2. Encode the request.
        let payload = self
            .codec
            .encode_to_vec(&req)
            .map_err(StreamTransportError::Codec)?;

        // 3. Build frame with routing header: [slot_id][payload].
        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(slot.slot_id());
        frame.extend_from_slice(&payload);

        // 4. Send frame (acquire writer lock).
        {
            let mut writer = self.writer.lock().await;
            self.framer
                .write_frame(&mut *writer, &frame)
                .await
                .map_err(|e| {
                    StreamTransportError::<FramingRE<Framer, R>, _>::Framing(
                        <FramingRE<Framer, R> as From<FramingWE<Framer, W>>>::from(e),
                    )
                })?;
        }

        // 5. Cooperative demux: read frames and dispatch until our slot
        //    gets its reply.
        loop {
            // Check if our reply has already been delivered by another task.
            if let Some(data) = self.router.router().try_recv_slot(slot.slot_id()) {
                return self.codec.decode(data).map_err(StreamTransportError::Codec);
            }

            // Read next frame from the stream.
            let frame = {
                let mut reader = self.reader.lock().await;
                // Re-check after acquiring the reader — a concurrent
                // task may have delivered our reply while we waited.
                if let Some(data) = self.router.router().try_recv_slot(slot.slot_id()) {
                    return self.codec.decode(data).map_err(StreamTransportError::Codec);
                }
                self.framer
                    .read_frame(&mut *reader)
                    .await
                    .map_err(StreamTransportError::Framing)?
            };

            // Parse routing header.
            let reply_slot_id = MuxedSlots::<N, BUF>::parse_header(&frame[..1]);
            let reply_payload = &frame[1..];

            if reply_slot_id == slot.slot_id() {
                // This reply is ours — decode and return directly.
                return self
                    .codec
                    .decode(reply_payload)
                    .map_err(StreamTransportError::Codec);
            }

            // Not ours — deliver to the correct slot.
            self.router.router().deliver(reply_slot_id, reply_payload);
        }
    }
}

// =========================================================================
// ServerTransport — Sequential
// =========================================================================

impl<R, W, Framer, Codec, Req, Resp, S> ServerTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, Sequential, Req, Resp, S>
where
    R: AsyncBytesRead,
    W: AsyncBytesWrite,
    Framer: FrameWriter + FrameReader,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    FramingWE<Framer, W>: core::fmt::Debug,
    FramingRE<Framer, R>: core::fmt::Debug + From<FramingWE<Framer, W>>,
    <Codec as Encoder>::Error: core::fmt::Debug,
    S: RouterStorage<Router = Sequential>,
{
    type Error = StreamTransportError<FramingRE<Framer, R>, <Codec as Encoder>::Error>;
    type ReplyToken = StreamReplyToken<Sequential>;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let buf = {
            let mut reader = self.reader.lock().await;
            self.framer
                .read_frame(&mut *reader)
                .await
                .map_err(StreamTransportError::Framing)?
        };
        let req = self
            .codec
            .decode(&buf)
            .map_err(StreamTransportError::Codec)?;
        Ok((req, StreamReplyToken::new()))
    }

    async fn reply(&self, _token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        let bytes = self
            .codec
            .encode_to_vec(&resp)
            .map_err(StreamTransportError::Codec)?;
        let mut writer = self.writer.lock().await;
        self.framer
            .write_frame(&mut *writer, &bytes)
            .await
            .map_err(|e| {
                StreamTransportError::<FramingRE<Framer, R>, _>::Framing(
                    <FramingRE<Framer, R> as From<FramingWE<Framer, W>>>::from(e),
                )
            })
    }
}

// =========================================================================
// ServerTransport — MuxedSlots<N, BUF>
// =========================================================================

impl<R, W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp, S> ServerTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, MuxedSlots<N, BUF>, Req, Resp, S>
where
    R: AsyncBytesRead,
    W: AsyncBytesWrite,
    Framer: FrameWriter + FrameReader,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    FramingWE<Framer, W>: core::fmt::Debug,
    FramingRE<Framer, R>: core::fmt::Debug + From<FramingWE<Framer, W>>,
    <Codec as Encoder>::Error: core::fmt::Debug,
    S: RouterStorage<Router = MuxedSlots<N, BUF>>,
{
    type Error = StreamTransportError<FramingRE<Framer, R>, <Codec as Encoder>::Error>;
    type ReplyToken = MuxedReplyToken;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let frame = {
            let mut reader = self.reader.lock().await;
            self.framer
                .read_frame(&mut *reader)
                .await
                .map_err(StreamTransportError::Framing)?
        };

        let slot_id = MuxedSlots::<N, BUF>::parse_header(&frame[..1]);
        let payload = &frame[1..];

        let req = self
            .codec
            .decode(payload)
            .map_err(StreamTransportError::Codec)?;
        Ok((req, MuxedReplyToken::new(slot_id)))
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        let payload = self
            .codec
            .encode_to_vec(&resp)
            .map_err(StreamTransportError::Codec)?;

        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(token.slot_id());
        frame.extend_from_slice(&payload);

        let mut writer = self.writer.lock().await;
        self.framer
            .write_frame(&mut *writer, &frame)
            .await
            .map_err(|e| {
                StreamTransportError::<FramingRE<Framer, R>, _>::Framing(
                    <FramingRE<Framer, R> as From<FramingWE<Framer, W>>>::from(e),
                )
            })
    }
}

#[cfg(all(test, feature = "postcard"))]
mod tests {
    use super::*;
    use crate::io::cursor::{cursor_read, cursor_write};
    use crate::io::mem_pipe::duplex;
    use crate::stream::routing::MuxedSlots;
    use crate::stream::{LengthPrefixed, PostcardCodec, Sequential};

    fn block_on<F: core::future::Future>(fut: F) -> F::Output {
        futures_lite::future::block_on(fut)
    }

    #[test]
    fn sequential_round_trip_via_cursors() {
        // Encode a request bytestream, feed it through a server, capture
        // the reply, feed that through a client reader.
        type ServerT = StreamTransport<
            crate::io::cursor::CursorRead,
            crate::io::cursor::CursorWrite,
            LengthPrefixed,
            PostcardCodec,
            Sequential,
            u32, // Req
            u32, // Resp
        >;
        type ClientT = StreamTransport<
            crate::io::cursor::CursorRead,
            crate::io::cursor::CursorWrite,
            LengthPrefixed,
            PostcardCodec,
            Sequential,
            u32, // Incoming (Resp)
            u32, // Outgoing (Req)
        >;

        // Client writes req=42 into its writer buffer.
        let client: ClientT = ClientT::new(cursor_read(Vec::new()), cursor_write());
        let mut w = block_on(client.writer.lock());
        let bytes = PostcardCodec.encode_to_vec(&42u32).unwrap();
        block_on(LengthPrefixed.write_frame(&mut *w, &bytes)).unwrap();
        let c2s = w.0.clone();
        drop(w);

        // Server receives.
        let mut server: ServerT = ServerT::new(cursor_read(c2s), cursor_write());
        let (req, token) = block_on(server.recv()).unwrap();
        assert_eq!(req, 42u32);
        block_on(server.reply(token, 100u32)).unwrap();

        let s2c = block_on(server.writer.lock()).0.clone();

        // Client reads the reply.
        *block_on(client.reader.lock()) = cursor_read(s2c);
        let buf = {
            let mut r = block_on(client.reader.lock());
            block_on(LengthPrefixed.read_frame(&mut *r)).unwrap()
        };
        let reply: u32 = PostcardCodec.decode(&buf).unwrap();
        assert_eq!(reply, 100);
    }

    #[test]
    fn muxed_server_round_trip() {
        type T = StreamTransport<
            crate::io::cursor::CursorRead,
            crate::io::cursor::CursorWrite,
            LengthPrefixed,
            PostcardCodec,
            MuxedSlots<4, 128>,
            u32,
            u32,
        >;

        // Simulate client sending a muxed frame: [slot_id=3][postcard(42)]
        let slot_id: u8 = 3;
        let payload = PostcardCodec.encode_to_vec(&42u32).unwrap();
        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(slot_id);
        frame.extend_from_slice(&payload);

        let mut c2s_writer = cursor_write();
        block_on(LengthPrefixed.write_frame(&mut c2s_writer, &frame)).unwrap();
        let c2s_bytes = c2s_writer.0;

        // Server receives the muxed request.
        let mut server: T = T::new(cursor_read(c2s_bytes), cursor_write());
        let (req, token) = block_on(server.recv()).unwrap();
        assert_eq!(req, 42u32);
        assert_eq!(token.slot_id(), 3);

        block_on(server.reply(token, 100u32)).unwrap();

        // Read back the reply frame and verify slot_id preserved.
        let s2c_bytes = block_on(server.writer.lock()).0.clone();
        let mut s2c_reader = cursor_read(s2c_bytes);
        let reply_frame = block_on(LengthPrefixed.read_frame(&mut s2c_reader)).unwrap();
        assert_eq!(reply_frame[0], 3u8);
        let reply: u32 = PostcardCodec.decode(&reply_frame[1..]).unwrap();
        assert_eq!(reply, 100u32);
    }

    #[test]
    fn muxed_client_call_end_to_end_over_mem_pipe() {
        use crate::io::mem_pipe::{PipeReader, PipeWriter};

        type ClientT = StreamTransport<
            PipeReader,
            PipeWriter,
            LengthPrefixed,
            PostcardCodec,
            MuxedSlots<4, 128>,
            u32,
            u32,
        >;
        type ServerT = StreamTransport<
            PipeReader,
            PipeWriter,
            LengthPrefixed,
            PostcardCodec,
            MuxedSlots<4, 128>,
            u32,
            u32,
        >;

        let ((r_a, w_a), (r_b, w_b)) = duplex();

        let client: ClientT = ClientT::new(r_a, w_a);
        let mut server: ServerT = ServerT::new(r_b, w_b);

        // Run server and client concurrently on a single executor.
        let result = block_on(async {
            let server_fut = async {
                let (req, token) = server.recv().await.unwrap();
                server.reply(token, req * 2).await.unwrap();
            };
            let client_fut = async { client.call(21u32).await.unwrap() };
            let (resp, _) = futures_lite::future::zip(client_fut, server_fut).await;
            resp
        });

        assert_eq!(result, 42);
    }
}
