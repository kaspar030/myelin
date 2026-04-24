//! Duplex stream transport: bidirectional RPC over one byte stream.
//!
//! A single pair of `(reader, writer)` is shared between:
//!
//! - Any number of [`DuplexClientHalf`]s, each implementing
//!   [`ClientTransport`] for a particular `(Req, Resp, api_id)` — used
//!   to *call* a peer-served API.
//! - Any number of [`DuplexServerHalf`]s, each implementing
//!   [`ServerTransport`] for a particular `(Req, Resp, api_id)` — used
//!   to *serve* an API to the peer.
//!
//! A background [`DuplexPump`] future drives the reader: it decodes each
//! incoming frame, looks at the `kind` byte (request vs response), and
//! dispatches accordingly — responses go to this peer's outgoing-call
//! slot router, requests land in the appropriate server half's inbox.
//!
//! The pump is just a future; myelin does not spawn it. The user's
//! runtime (smol, tokio, etc.) is responsible for running it — either
//! on a dedicated task (`smol::spawn(pump.run())`) or concurrently with
//! application logic via `futures::join!`.
//!
//! ## Wire format
//!
//! Each framed payload is:
//!
//! ```text
//! [u8 kind][u16 api_id LE][u8 slot_id][codec bytes]
//! ```
//!
//! - `kind`: `0` = request (caller → callee), `1` = response.
//! - `api_id`: identifies which registered API the frame belongs to.
//!   Client halves decide their own `api_id` at construction time.
//! - `slot_id`: echo-back identifier. For requests it's the caller's
//!   slot handle from its local [`MuxedSlots`]; responses carry the
//!   same value so the peer can route the reply.
//!
//! This layout is **distinct from** (and incompatible with) the muxed
//! `StreamTransport` wire format — duplex is its own transport, used
//! when both peers need to both call and serve on one stream.

use core::marker::PhantomData;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use atomic_waker::AtomicWaker;
use serde::{Deserialize, Serialize};

use crate::io::{AsyncBytesRead, AsyncBytesWrite, LocalLock};
use crate::stream::codec::{Decoder, Encoder};
use crate::stream::framing::{FrameReader, FrameWriter};
use crate::stream::routing::{MuxedReplyToken, MuxedSlots, ReplyRouter, RouterSlotHandle};
use crate::stream::transport::StreamTransportError;
use crate::transport::{ClientTransport, ServerTransport};

// ---------------------------------------------------------------------------
// Wire-format constants
// ---------------------------------------------------------------------------

/// Request frame kind: the sender is *calling* an API on the peer.
pub const KIND_REQUEST: u8 = 0;
/// Response frame kind: the sender is *replying* to an earlier request.
pub const KIND_RESPONSE: u8 = 1;

/// Header length in bytes: `[kind][api_id_lo][api_id_hi][slot_id]`.
pub const DUPLEX_HEADER_LEN: usize = 4;

/// Encode a duplex frame header and prepend it to `payload`.
///
/// Returns a fresh `Vec<u8>` sized to the complete framed payload.
pub fn encode_duplex_frame(kind: u8, api_id: u16, slot_id: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DUPLEX_HEADER_LEN + payload.len());
    out.push(kind);
    out.extend_from_slice(&api_id.to_le_bytes());
    out.push(slot_id);
    out.extend_from_slice(payload);
    out
}

/// Parsed duplex frame header.
#[derive(Debug, Clone, Copy)]
pub struct DuplexHeader {
    /// [`KIND_REQUEST`] or [`KIND_RESPONSE`].
    pub kind: u8,
    /// Target API identifier.
    pub api_id: u16,
    /// Caller's slot identifier (echo-back).
    pub slot_id: u8,
}

/// Errors from parsing a duplex header.
#[derive(Debug)]
pub enum DuplexFrameError {
    /// Frame shorter than [`DUPLEX_HEADER_LEN`].
    TooShort,
    /// `kind` byte was neither [`KIND_REQUEST`] nor [`KIND_RESPONSE`].
    UnknownKind(u8),
}

impl core::fmt::Display for DuplexFrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DuplexFrameError::TooShort => write!(f, "duplex frame too short for header"),
            DuplexFrameError::UnknownKind(k) => write!(f, "unknown duplex frame kind: {k}"),
        }
    }
}

/// Parse the header from a raw framed payload, returning it plus the
/// remaining codec bytes.
pub fn parse_duplex_frame(frame: &[u8]) -> Result<(DuplexHeader, &[u8]), DuplexFrameError> {
    if frame.len() < DUPLEX_HEADER_LEN {
        return Err(DuplexFrameError::TooShort);
    }
    let kind = frame[0];
    if kind != KIND_REQUEST && kind != KIND_RESPONSE {
        return Err(DuplexFrameError::UnknownKind(kind));
    }
    let api_id = u16::from_le_bytes([frame[1], frame[2]]);
    let slot_id = frame[3];
    Ok((
        DuplexHeader {
            kind,
            api_id,
            slot_id,
        },
        &frame[DUPLEX_HEADER_LEN..],
    ))
}

// ---------------------------------------------------------------------------
// Server inbox: a tiny async queue of raw (payload, slot_id) bytes
// ---------------------------------------------------------------------------

struct InboxInner {
    queue: std::sync::Mutex<VecDeque<(Vec<u8>, u8)>>,
    waker: AtomicWaker,
    closed: std::sync::Mutex<bool>,
}

/// Handle to a server inbox — a single-consumer async queue of
/// `(payload, slot_id)` tuples, fed by the duplex pump.
#[derive(Clone)]
pub struct ServerInbox {
    inner: Arc<InboxInner>,
}

impl ServerInbox {
    fn new() -> Self {
        Self {
            inner: Arc::new(InboxInner {
                queue: std::sync::Mutex::new(VecDeque::new()),
                waker: AtomicWaker::new(),
                closed: std::sync::Mutex::new(false),
            }),
        }
    }

    fn push(&self, payload: Vec<u8>, slot_id: u8) {
        self.inner
            .queue
            .lock()
            .unwrap()
            .push_back((payload, slot_id));
        self.inner.waker.wake();
    }

    fn close(&self) {
        *self.inner.closed.lock().unwrap() = true;
        self.inner.waker.wake();
    }

    async fn recv(&self) -> Option<(Vec<u8>, u8)> {
        core::future::poll_fn(|cx| {
            if let Some(v) = self.inner.queue.lock().unwrap().pop_front() {
                return core::task::Poll::Ready(Some(v));
            }
            self.inner.waker.register(cx.waker());
            if let Some(v) = self.inner.queue.lock().unwrap().pop_front() {
                return core::task::Poll::Ready(Some(v));
            }
            if *self.inner.closed.lock().unwrap() {
                return core::task::Poll::Ready(None);
            }
            core::task::Poll::Pending
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Shared state between the pump, client halves, and server halves
// ---------------------------------------------------------------------------

/// Shared fabric plumbing client halves, server halves, and the pump
/// together. Users obtain clones via [`DuplexStreamTransport::split`].
pub struct DuplexShared<W, Framer, Codec, const N: usize, const BUF: usize> {
    /// Shared writer behind a `LocalLock` so all halves (and the pump,
    /// though the pump never writes) can serialize outgoing frames.
    writer: LocalLock<W>,
    /// Outgoing-call slot router — our side's pending responses are
    /// delivered here by the pump.
    ///
    /// Stored in a [`Box`] so that constructing a `DuplexStreamTransport`
    /// does not have to materialise the full `N × BUF` slot array on the
    /// stack before moving it into this struct. See
    /// [`MuxedSlots::new_boxed`] for the underlying reason.
    slots: Box<MuxedSlots<N, BUF>>,
    /// Registered server inboxes, keyed by `api_id`.
    inboxes: std::sync::Mutex<HashMap<u16, ServerInbox>>,
    framer: Framer,
    codec: Codec,
}

impl<W, Framer, Codec, const N: usize, const BUF: usize> DuplexShared<W, Framer, Codec, N, BUF> {
    fn register_inbox(&self, api_id: u16) -> ServerInbox {
        let mut map = self.inboxes.lock().unwrap();
        if let Some(existing) = map.get(&api_id) {
            return existing.clone();
        }
        let inbox = ServerInbox::new();
        map.insert(api_id, inbox.clone());
        inbox
    }

    fn close_inboxes(&self) {
        let map = self.inboxes.lock().unwrap();
        for ib in map.values() {
            ib.close();
        }
    }
}

// ---------------------------------------------------------------------------
// DuplexStreamTransport — owning entry point
// ---------------------------------------------------------------------------

/// A duplex stream transport: owns `(reader, writer)` and vends
/// client/server halves plus a pump future.
///
/// Type parameters:
/// - `R` / `W`: async reader / writer halves of the byte stream.
/// - `Framer`: frame layer (e.g. [`LengthPrefixed`](crate::stream::LengthPrefixed)).
/// - `Codec`: serialisation codec (e.g. [`PostcardCodec`](crate::stream::PostcardCodec)).
/// - `N`: max concurrent outgoing calls from this side.
/// - `BUF`: per-slot reply buffer size.
pub struct DuplexStreamTransport<R, W, Framer, Codec, const N: usize, const BUF: usize> {
    reader: R,
    shared: Arc<DuplexShared<W, Framer, Codec, N, BUF>>,
}

impl<R, W, Framer, Codec, const N: usize, const BUF: usize>
    DuplexStreamTransport<R, W, Framer, Codec, N, BUF>
where
    Framer: Default,
    Codec: Default,
{
    /// New duplex transport with default framer/codec.
    pub fn new(reader: R, writer: W) -> Self {
        Self::with_layers(reader, writer, Framer::default(), Codec::default())
    }
}

impl<R, W, Framer, Codec, const N: usize, const BUF: usize>
    DuplexStreamTransport<R, W, Framer, Codec, N, BUF>
{
    /// New duplex transport with explicit framer/codec.
    pub fn with_layers(reader: R, writer: W, framer: Framer, codec: Codec) -> Self {
        Self {
            reader,
            shared: Arc::new(DuplexShared {
                writer: LocalLock::new(writer),
                slots: MuxedSlots::new_boxed(),
                inboxes: std::sync::Mutex::new(HashMap::new()),
                framer,
                codec,
            }),
        }
    }

    /// Register a server half for `api_id`. All incoming frames with
    /// this `api_id` and `kind == KIND_REQUEST` will be dispatched to
    /// the returned half's inbox.
    ///
    /// Must be called before [`pump`](Self::pump) starts running;
    /// registration after pump start is allowed but any request that
    /// arrives before registration returns a `UnknownApi` frame-drop
    /// in the pump.
    pub fn server_half<Req, Resp>(
        &self,
        api_id: u16,
    ) -> DuplexServerHalf<W, Framer, Codec, N, BUF, Req, Resp> {
        let inbox = self.shared.register_inbox(api_id);
        DuplexServerHalf {
            shared: self.shared.clone(),
            api_id,
            inbox,
            _phantom: PhantomData,
        }
    }

    /// Create a client half for calling the peer's API at `api_id`.
    pub fn client_half<Req, Resp>(
        &self,
        api_id: u16,
    ) -> DuplexClientHalf<W, Framer, Codec, N, BUF, Req, Resp> {
        DuplexClientHalf {
            shared: self.shared.clone(),
            api_id,
            _phantom: PhantomData,
        }
    }

    /// Split into the pump (consuming the reader) and a handle used to
    /// vend additional halves after the fact.
    #[allow(clippy::type_complexity)]
    pub fn split(
        self,
    ) -> (
        DuplexPump<R, W, Framer, Codec, N, BUF>,
        DuplexHandle<W, Framer, Codec, N, BUF>,
    ) {
        let shared = self.shared.clone();
        (
            DuplexPump {
                reader: self.reader,
                shared: self.shared,
            },
            DuplexHandle { shared },
        )
    }
}

// ---------------------------------------------------------------------------
// DuplexHandle — clone-friendly, half factory
// ---------------------------------------------------------------------------

/// A cheap-to-clone handle for registering server halves and minting
/// client halves after a [`DuplexStreamTransport`] has been split.
pub struct DuplexHandle<W, Framer, Codec, const N: usize, const BUF: usize> {
    shared: Arc<DuplexShared<W, Framer, Codec, N, BUF>>,
}

impl<W, Framer, Codec, const N: usize, const BUF: usize> Clone
    for DuplexHandle<W, Framer, Codec, N, BUF>
{
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<W, Framer, Codec, const N: usize, const BUF: usize> DuplexHandle<W, Framer, Codec, N, BUF> {
    /// Register a server half for `api_id`.
    pub fn server_half<Req, Resp>(
        &self,
        api_id: u16,
    ) -> DuplexServerHalf<W, Framer, Codec, N, BUF, Req, Resp> {
        let inbox = self.shared.register_inbox(api_id);
        DuplexServerHalf {
            shared: self.shared.clone(),
            api_id,
            inbox,
            _phantom: PhantomData,
        }
    }

    /// Create a client half for calling the peer's API at `api_id`.
    pub fn client_half<Req, Resp>(
        &self,
        api_id: u16,
    ) -> DuplexClientHalf<W, Framer, Codec, N, BUF, Req, Resp> {
        DuplexClientHalf {
            shared: self.shared.clone(),
            api_id,
            _phantom: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// DuplexPump — drives the reader, demultiplexes frames
// ---------------------------------------------------------------------------

/// The pump future. Run it concurrently with your application logic;
/// it terminates when the peer closes the stream (or on an I/O error).
pub struct DuplexPump<R, W, Framer, Codec, const N: usize, const BUF: usize> {
    reader: R,
    shared: Arc<DuplexShared<W, Framer, Codec, N, BUF>>,
}

/// Errors the pump can produce.
#[derive(Debug)]
pub enum DuplexPumpError<F> {
    /// Frame layer error.
    Framing(F),
    /// A frame had a malformed header.
    BadFrame(DuplexFrameError),
}

impl<F: core::fmt::Display> core::fmt::Display for DuplexPumpError<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DuplexPumpError::Framing(e) => write!(f, "{e}"),
            DuplexPumpError::BadFrame(e) => write!(f, "{e}"),
        }
    }
}

impl<R, W, Framer, Codec, const N: usize, const BUF: usize> DuplexPump<R, W, Framer, Codec, N, BUF>
where
    R: AsyncBytesRead,
    Framer: FrameReader,
{
    /// Drive the pump until the stream closes.
    pub async fn run(
        mut self,
    ) -> Result<(), DuplexPumpError<<Framer as FrameReader>::Error<R::Error>>> {
        let res = self.run_inner().await;
        // Signal all server halves that no more requests will arrive.
        self.shared.close_inboxes();
        res
    }

    async fn run_inner(
        &mut self,
    ) -> Result<(), DuplexPumpError<<Framer as FrameReader>::Error<R::Error>>> {
        loop {
            let frame = self
                .shared
                .framer
                .read_frame(&mut self.reader)
                .await
                .map_err(DuplexPumpError::Framing)?;

            let (hdr, payload) = parse_duplex_frame(&frame).map_err(DuplexPumpError::BadFrame)?;

            match hdr.kind {
                KIND_RESPONSE => {
                    self.shared.slots.deliver(hdr.slot_id, payload);
                }
                KIND_REQUEST => {
                    // Copy payload into an owned Vec for the inbox.
                    let owned = payload.to_vec();
                    let inbox = {
                        let map = self.shared.inboxes.lock().unwrap();
                        map.get(&hdr.api_id).cloned()
                    };
                    match inbox {
                        Some(inbox) => inbox.push(owned, hdr.slot_id),
                        None => {
                            // Unknown API — drop the request silently.
                            // A more featureful impl could send back an
                            // "UnknownApi" error frame; for now we just
                            // log to stderr in debug builds.
                            #[cfg(debug_assertions)]
                            eprintln!(
                                "duplex pump: dropping request for unknown api_id 0x{:04x}",
                                hdr.api_id
                            );
                        }
                    }
                }
                _ => unreachable!("parse_duplex_frame validates kind"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DuplexClientHalf — impl ClientTransport
// ---------------------------------------------------------------------------

/// Client half of a duplex transport: implements [`ClientTransport`]
/// for `(Req, Resp)` scoped to a fixed `api_id`.
pub struct DuplexClientHalf<W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp> {
    shared: Arc<DuplexShared<W, Framer, Codec, N, BUF>>,
    api_id: u16,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp> ClientTransport<Req, Resp>
    for DuplexClientHalf<W, Framer, Codec, N, BUF, Req, Resp>
where
    W: AsyncBytesWrite,
    Framer: FrameWriter,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
    <Framer as FrameWriter>::Error<W::Error>: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error =
        StreamTransportError<<Framer as FrameWriter>::Error<W::Error>, <Codec as Encoder>::Error>;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        // Acquire a slot for the reply.
        let slot = self.shared.slots.acquire().await.map_err(|e| match e {})?;

        // Encode and frame.
        let payload = self
            .shared
            .codec
            .encode_to_vec(&req)
            .map_err(StreamTransportError::Codec)?;
        let framed = encode_duplex_frame(KIND_REQUEST, self.api_id, slot.slot_id(), &payload);

        // Send.
        {
            let mut w = self.shared.writer.lock().await;
            self.shared
                .framer
                .write_frame(&mut *w, &framed)
                .await
                .map_err(StreamTransportError::Framing)?;
        }

        // Await reply via the slot.
        let bytes = slot.recv_reply().await;
        self.shared
            .codec
            .decode(bytes)
            .map_err(StreamTransportError::Codec)
    }
}

// ---------------------------------------------------------------------------
// DuplexServerHalf — impl ServerTransport
// ---------------------------------------------------------------------------

/// Server half of a duplex transport: implements [`ServerTransport`]
/// for `(Req, Resp)` on a fixed `api_id`.
pub struct DuplexServerHalf<W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp> {
    shared: Arc<DuplexShared<W, Framer, Codec, N, BUF>>,
    api_id: u16,
    inbox: ServerInbox,
    _phantom: PhantomData<(Req, Resp)>,
}

/// Error returned when the stream closes while the server is waiting
/// for the next request.
#[derive(Debug)]
pub struct DuplexStreamClosed;

impl core::fmt::Display for DuplexStreamClosed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("duplex stream closed")
    }
}

impl<W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp> ServerTransport<Req, Resp>
    for DuplexServerHalf<W, Framer, Codec, N, BUF, Req, Resp>
where
    W: AsyncBytesWrite,
    Framer: FrameWriter,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    <Framer as FrameWriter>::Error<W::Error>: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error =
        DuplexServerError<<Framer as FrameWriter>::Error<W::Error>, <Codec as Encoder>::Error>;
    type ReplyToken = MuxedReplyToken;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let (payload, slot_id) = self.inbox.recv().await.ok_or(DuplexServerError::Closed)?;
        let req = self
            .shared
            .codec
            .decode(&payload)
            .map_err(DuplexServerError::Codec)?;
        Ok((req, MuxedReplyToken::new(slot_id)))
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        let payload = self
            .shared
            .codec
            .encode_to_vec(&resp)
            .map_err(DuplexServerError::Codec)?;
        let framed = encode_duplex_frame(KIND_RESPONSE, self.api_id, token.slot_id(), &payload);
        let mut w = self.shared.writer.lock().await;
        self.shared
            .framer
            .write_frame(&mut *w, &framed)
            .await
            .map_err(DuplexServerError::Framing)
    }
}

/// Errors for [`DuplexServerHalf`].
#[derive(Debug)]
pub enum DuplexServerError<F, C> {
    /// Frame layer error.
    Framing(F),
    /// Codec error.
    Codec(C),
    /// The duplex stream closed before the next request arrived.
    Closed,
}

impl<F: core::fmt::Display, C: core::fmt::Display> core::fmt::Display for DuplexServerError<F, C> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DuplexServerError::Framing(e) => write!(f, "{e}"),
            DuplexServerError::Codec(e) => write!(f, "codec error: {e}"),
            DuplexServerError::Closed => write!(f, "duplex stream closed"),
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(all(test, feature = "postcard"))]
mod tests {
    use super::*;
    use crate::io::mem_pipe::duplex;
    use crate::stream::{LengthPrefixed, PostcardCodec};

    fn block_on<F: core::future::Future>(fut: F) -> F::Output {
        futures_lite::future::block_on(fut)
    }

    type DxT<R, W> = DuplexStreamTransport<R, W, LengthPrefixed, PostcardCodec, 4, 256>;

    const PING_API: u16 = 0x0001;
    const ECHO_API: u16 = 0x0002;

    #[test]
    fn duplex_header_round_trip() {
        let f = encode_duplex_frame(KIND_REQUEST, 0xBEEF, 7, b"hi");
        let (hdr, payload) = parse_duplex_frame(&f).unwrap();
        assert_eq!(hdr.kind, KIND_REQUEST);
        assert_eq!(hdr.api_id, 0xBEEF);
        assert_eq!(hdr.slot_id, 7);
        assert_eq!(payload, b"hi");
    }

    #[test]
    fn duplex_end_to_end_two_apis_both_directions() {
        let ((r_a, w_a), (r_b, w_b)) = duplex();

        let dx_a: DxT<_, _> = DxT::new(r_a, w_a);
        let dx_b: DxT<_, _> = DxT::new(r_b, w_b);

        // A serves PING, B serves ECHO. Each side has a client for the
        // other's API.
        let ping_server_a = dx_a.server_half::<u32, u32>(PING_API);
        let echo_client_a = dx_a.client_half::<String, String>(ECHO_API);

        let echo_server_b = dx_b.server_half::<String, String>(ECHO_API);
        let ping_client_b = dx_b.client_half::<u32, u32>(PING_API);

        let (pump_a, _h_a) = dx_a.split();
        let (pump_b, _h_b) = dx_b.split();

        block_on(async {
            // Server A loop: answer one PING request with req + 1.
            let mut ping_srv = ping_server_a;
            let server_a = async move {
                let (req, token) = ping_srv.recv().await.unwrap();
                ping_srv.reply(token, req + 1).await.unwrap();
            };

            // Server B loop: echo one ECHO request.
            let mut echo_srv = echo_server_b;
            let server_b = async move {
                let (req, token) = echo_srv.recv().await.unwrap();
                echo_srv.reply(token, format!("echo: {req}")).await.unwrap();
            };

            // Client A calls B.ECHO; client B calls A.PING — concurrently.
            let client_a = async { echo_client_a.call("hello".to_string()).await.unwrap() };
            let client_b = async { ping_client_b.call(41u32).await.unwrap() };

            let pump_a_fut = pump_a.run();
            let pump_b_fut = pump_b.run();

            // Run clients + servers; the pumps are driven concurrently
            // until clients finish, at which point we'd need to stop
            // them (they'd otherwise block on the next frame). We use
            // `futures_lite::future::or` with a short-circuit trick:
            // wrap servers + clients in one future and the pumps in
            // another, and let `or` return as soon as the clients+servers
            // complete.
            let work = async {
                let ((echo_resp, ping_resp), _) = futures_lite::future::zip(
                    futures_lite::future::zip(client_a, client_b),
                    futures_lite::future::zip(server_a, server_b),
                )
                .await;
                assert_eq!(echo_resp, "echo: hello");
                assert_eq!(ping_resp, 42);
            };

            // Race work against pumps; pumps never complete normally
            // (they block reading), so `or` returns once `work` finishes.
            futures_lite::future::or(work, async {
                let _ = futures_lite::future::zip(pump_a_fut, pump_b_fut).await;
            })
            .await;
        });
    }

    #[test]
    fn duplex_construction_on_restricted_stack() {
        // Mirror of `routing::tests::new_boxed_on_restricted_stack` at the
        // full `DuplexStreamTransport` layer. With `N = 32`, `BUF =
        // 131_072` the slot array alone is 4 MiB — impossible to build
        // on a 1 MiB thread stack unless `MuxedSlots` is heap-constructed
        // *and* the `DuplexShared::slots` field is boxed (so the slot
        // array is not re-materialised on the stack during
        // `DuplexShared` construction).
        type BigDx<R, W> =
            DuplexStreamTransport<R, W, LengthPrefixed, PostcardCodec, 32, 131_072>;

        std::thread::Builder::new()
            .stack_size(1 << 20) // 1 MiB
            .spawn(|| {
                let ((r_a, w_a), (_r_b, _w_b)) = duplex();
                let _dx: BigDx<_, _> = BigDx::new(r_a, w_a);
                // If we got here, construction stayed within the 1 MiB
                // stack budget — that is the whole point of the test.
            })
            .expect("spawn restricted-stack thread")
            .join()
            .expect("DuplexStreamTransport construction overflowed a 1 MiB stack");
    }
}
