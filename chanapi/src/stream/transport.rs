//! Composed stream transport built from Framing × Encoding × ReplyRouting.
//!
//! `StreamTransport` implements [`ClientTransport`] and [`ServerTransport`]
//! by composing a framer, a codec, and a reply router over a read/write
//! byte stream.
//!
//! For [`Sequential`](super::routing::Sequential) routing, this is a simple
//! send-then-receive loop. For [`MuxedSlots`](super::routing::MuxedSlots)
//! routing, the transport manages concurrent requests via cooperative
//! demuxing: each `call()` reads from the stream and dispatches replies to
//! the correct slot.

use std::cell::RefCell;
use std::io::{Read, Write};

use core::marker::PhantomData;
use serde::{Deserialize, Serialize};

use crate::stream::codec::{Decoder, Encoder};
use crate::stream::framing::{FrameReader, FrameWriter};
use crate::stream::routing::{MuxedReplyToken, MuxedSlots, ReplyRouter, RouterSlotHandle, Sequential};
use crate::transport::{ClientTransport, ServerTransport};

/// A transport composed from a framer, codec, and reply router.
///
/// - `R` / `W`: reader / writer halves of the underlying byte stream.
/// - `Framer`: framing strategy (e.g., `LengthPrefixed`).
/// - `Codec`: serialization codec (e.g., `PostcardCodec`).
/// - `Router`: reply routing strategy (e.g., `Sequential`, `MuxedSlots`).
/// - `Req` / `Resp`: the request and response message types.
pub struct StreamTransport<R, W, Framer, Codec, Router, Req, Resp> {
    reader: RefCell<R>,
    writer: RefCell<W>,
    framer: Framer,
    codec: Codec,
    router: Router,
    _phantom: PhantomData<(Req, Resp)>,
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
            reader: RefCell::new(reader),
            writer: RefCell::new(writer),
            framer,
            codec,
            router,
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

// =========================================================================
// ClientTransport — Sequential
// =========================================================================

impl<R, W, Framer, Codec, Req, Resp> ClientTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, Sequential, Resp, Req>
where
    R: Read,
    W: Write,
    Framer: FrameWriter + FrameReader<Error = <Framer as FrameWriter>::Error>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
    <Framer as FrameWriter>::Error: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error = StreamTransportError<<Framer as FrameWriter>::Error, <Codec as Encoder>::Error>;

    /// Sequential call: send request, read reply. No routing overhead.
    ///
    /// # Cancel Safety
    ///
    /// This transport uses blocking I/O — the async functions resolve
    /// immediately and never yield. There are no `.await` cancel points,
    /// so cancellation cannot occur mid-call.
    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let bytes = self.codec.encode_to_vec(&req).map_err(StreamTransportError::Codec)?;
        {
            let mut writer = self.writer.borrow_mut();
            self.framer
                .write_frame(&mut *writer, &bytes)
                .map_err(StreamTransportError::Framing)?;
        }
        let buf = {
            let mut reader = self.reader.borrow_mut();
            self.framer
                .read_frame(&mut *reader)
                .map_err(StreamTransportError::Framing)?
        };
        self.codec.decode(&buf).map_err(StreamTransportError::Codec)
    }
}

// =========================================================================
// ClientTransport — MuxedSlots<N, BUF>
// =========================================================================

impl<R, W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp>
    ClientTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, MuxedSlots<N, BUF>, Resp, Req>
where
    R: Read,
    W: Write,
    Framer: FrameWriter + FrameReader<Error = <Framer as FrameWriter>::Error>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
    <Framer as FrameWriter>::Error: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error = StreamTransportError<<Framer as FrameWriter>::Error, <Codec as Encoder>::Error>;

    /// Muxed call: acquire slot → tag frame → send → cooperative demux → return.
    ///
    /// # Blocking I/O limitation
    ///
    /// The current framing layer uses blocking `std::io::Read`, so the
    /// cooperative demux loop does not yield between reads. True concurrent
    /// multiplexing requires an async framing layer (e.g.,
    /// `tokio::io::AsyncRead`). The routing layer ([`MuxedSlots`]) is fully
    /// concurrent and ready for async I/O — the transport integration is
    /// the current bottleneck.
    ///
    /// # Cancel Safety
    ///
    /// The slot guard is RAII — dropping the future frees the slot. If the
    /// request was already sent, the server reply is silently discarded
    /// when it arrives (the bitmap bit is cleared).
    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        // 1. Acquire a routing slot.
        let slot = self.router.acquire().await.map_err(|e| match e {})?;

        // 2. Encode the request.
        let payload = self.codec.encode_to_vec(&req).map_err(StreamTransportError::Codec)?;

        // 3. Build frame with routing header: [slot_id][payload].
        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(slot.slot_id());
        frame.extend_from_slice(&payload);

        // 4. Send frame (acquire writer lock).
        {
            let mut writer = self.writer.borrow_mut();
            self.framer
                .write_frame(&mut *writer, &frame)
                .map_err(StreamTransportError::Framing)?;
        }

        // 5. Cooperative demux: read frames and dispatch until our slot
        //    gets its reply.
        loop {
            // Check if our reply has already been delivered by another task.
            if let Some(data) = self.router.try_recv_slot(slot.slot_id()) {
                return self.codec.decode(data).map_err(StreamTransportError::Codec);
            }

            // Read next frame from the stream.
            let frame = {
                let mut reader = self.reader.borrow_mut();
                self.framer
                    .read_frame(&mut *reader)
                    .map_err(StreamTransportError::Framing)?
            };

            // Parse routing header.
            let reply_slot_id = MuxedSlots::<N, BUF>::parse_header(&frame[..1]);
            let reply_payload = &frame[1..];

            if reply_slot_id == slot.slot_id() {
                // This reply is ours — decode and return directly.
                return self.codec.decode(reply_payload).map_err(StreamTransportError::Codec);
            }

            // Not ours — deliver to the correct slot.
            self.router.deliver(reply_slot_id, reply_payload);
        }
    }
}

// =========================================================================
// ServerTransport — Sequential
// =========================================================================

impl<R, W, Framer, Codec, Req, Resp> ServerTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, Sequential, Req, Resp>
where
    R: Read,
    W: Write,
    Framer: FrameWriter + FrameReader<Error = <Framer as FrameWriter>::Error>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    <Framer as FrameWriter>::Error: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error = StreamTransportError<<Framer as FrameWriter>::Error, <Codec as Encoder>::Error>;
    type ReplyToken = StreamReplyToken<Sequential>;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let buf = {
            let mut reader = self.reader.borrow_mut();
            self.framer
                .read_frame(&mut *reader)
                .map_err(StreamTransportError::Framing)?
        };
        let req = self.codec.decode(&buf).map_err(StreamTransportError::Codec)?;
        Ok((req, StreamReplyToken::new()))
    }

    async fn reply(&self, _token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        let bytes = self.codec.encode_to_vec(&resp).map_err(StreamTransportError::Codec)?;
        let mut writer = self.writer.borrow_mut();
        self.framer
            .write_frame(&mut *writer, &bytes)
            .map_err(StreamTransportError::Framing)
    }
}

// =========================================================================
// ServerTransport — MuxedSlots<N, BUF>
// =========================================================================

impl<R, W, Framer, Codec, const N: usize, const BUF: usize, Req, Resp>
    ServerTransport<Req, Resp>
    for StreamTransport<R, W, Framer, Codec, MuxedSlots<N, BUF>, Req, Resp>
where
    R: Read,
    W: Write,
    Framer: FrameWriter + FrameReader<Error = <Framer as FrameWriter>::Error>,
    Codec: Encoder + Decoder<Error = <Codec as Encoder>::Error>,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    <Framer as FrameWriter>::Error: core::fmt::Debug,
    <Codec as Encoder>::Error: core::fmt::Debug,
{
    type Error = StreamTransportError<<Framer as FrameWriter>::Error, <Codec as Encoder>::Error>;
    type ReplyToken = MuxedReplyToken;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        // Read frame: [slot_id][payload].
        let frame = {
            let mut reader = self.reader.borrow_mut();
            self.framer
                .read_frame(&mut *reader)
                .map_err(StreamTransportError::Framing)?
        };

        let slot_id = MuxedSlots::<N, BUF>::parse_header(&frame[..1]);
        let payload = &frame[1..];

        let req = self.codec.decode(payload).map_err(StreamTransportError::Codec)?;
        Ok((req, MuxedReplyToken::new(slot_id)))
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        let payload = self.codec.encode_to_vec(&resp).map_err(StreamTransportError::Codec)?;

        // Build reply frame: [slot_id][payload].
        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(token.slot_id());
        frame.extend_from_slice(&payload);

        let mut writer = self.writer.borrow_mut();
        self.framer
            .write_frame(&mut *writer, &frame)
            .map_err(StreamTransportError::Framing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{LengthPrefixed, PostcardCodec, Sequential};
    use crate::stream::routing::MuxedSlots;
    use std::io::Cursor;

    /// Sequential round-trip: client sends, server receives, server replies, client reads.
    #[test]
    fn sequential_round_trip() {
        // Shared buffers simulating a bidirectional pipe.
        let client_to_server = Vec::new();
        let server_to_client = Vec::new();

        // Client writes to c2s, server reads from c2s.
        // Server writes to s2c, client reads from s2c.
        type T<R, W> = StreamTransport<R, W, LengthPrefixed, PostcardCodec, Sequential, u32, u32>;

        // --- client sends ---
        let client: T<Cursor<Vec<u8>>, Vec<u8>> =
            T::new(Cursor::new(server_to_client), client_to_server);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();

        // Encode the request.
        let req: u32 = 42;
        let req_bytes = PostcardCodec.encode_to_vec(&req).unwrap();
        {
            let mut w = client.writer.borrow_mut();
            LengthPrefixed.write_frame(&mut *w, &req_bytes).unwrap();
        }

        // Extract what the client wrote (the c2s buffer).
        let c2s_bytes = {
            let w = client.writer.borrow();
            w.clone()
        };

        // --- server receives ---
        let mut server: T<Cursor<Vec<u8>>, Vec<u8>> =
            T::new(Cursor::new(c2s_bytes), Vec::new());
        let (received_req, token) = rt.block_on(server.recv()).unwrap();
        assert_eq!(received_req, 42u32);

        // --- server replies ---
        let resp: u32 = 100;
        rt.block_on(server.reply(token, resp)).unwrap();

        let s2c_bytes = {
            let w = server.writer.borrow();
            w.clone()
        };

        // --- client reads reply ---
        {
            let mut r = client.reader.borrow_mut();
            *r = Cursor::new(s2c_bytes);
        }
        let reply_buf = {
            let mut r = client.reader.borrow_mut();
            LengthPrefixed.read_frame(&mut *r).unwrap()
        };
        let reply: u32 = PostcardCodec.decode(&reply_buf).unwrap();
        assert_eq!(reply, 100u32);
    }

    /// Muxed server round-trip: client sends frame with slot_id, server
    /// parses it, replies with same slot_id.
    #[test]
    fn muxed_server_round_trip() {
        type T<R, W> = StreamTransport<
            R, W, LengthPrefixed, PostcardCodec, MuxedSlots<4, 128>, u32, u32,
        >;

        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();

        // Simulate client sending a muxed frame: [slot_id=3][postcard(42)]
        let slot_id: u8 = 3;
        let payload = PostcardCodec.encode_to_vec(&42u32).unwrap();
        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(slot_id);
        frame.extend_from_slice(&payload);

        let mut c2s = Vec::new();
        LengthPrefixed.write_frame(&mut c2s, &frame).unwrap();

        // Server receives the muxed request.
        let mut server: T<Cursor<Vec<u8>>, Vec<u8>> =
            T::new(Cursor::new(c2s), Vec::new());
        let (req, token) = rt.block_on(server.recv()).unwrap();
        assert_eq!(req, 42u32);
        assert_eq!(token.slot_id(), 3);

        // Server replies.
        rt.block_on(server.reply(token, 100u32)).unwrap();

        // Read back the reply frame and verify it has the correct slot_id.
        let s2c = server.writer.borrow().clone();
        let reply_frame = LengthPrefixed.read_frame(&mut Cursor::new(s2c)).unwrap();
        assert_eq!(reply_frame[0], 3u8); // slot_id preserved
        let reply: u32 = PostcardCodec.decode(&reply_frame[1..]).unwrap();
        assert_eq!(reply, 100u32);
    }

    /// Muxed client-server end-to-end: client.call() sends a muxed request,
    /// a "server" (running on another thread) receives and replies, and the
    /// client gets the correct response.
    #[test]
    fn muxed_client_call_end_to_end() {
        use std::os::unix::net::UnixStream;

        let (client_sock, server_sock) = UnixStream::pair().unwrap();
        let client_r = client_sock.try_clone().unwrap();
        let client_w = client_sock;
        let server_r = server_sock.try_clone().unwrap();
        let server_w = server_sock;

        type ClientT = StreamTransport<
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
            LengthPrefixed, PostcardCodec, MuxedSlots<4, 128>,
            u32, // Resp (Incoming for client = Outgoing for server)
            u32, // Req  (Outgoing for client = Incoming for server)
        >;
        type ServerT = StreamTransport<
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
            LengthPrefixed, PostcardCodec, MuxedSlots<4, 128>,
            u32, // Req  (Incoming for server)
            u32, // Resp (Outgoing for server)
        >;

        // Server thread: read one request, reply with req * 2.
        let server_handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let mut server: ServerT = ServerT::new(server_r, server_w);
            let (req, token) = rt.block_on(server.recv()).unwrap();
            rt.block_on(server.reply(token, req * 2)).unwrap();
        });

        // Client: call with 21, expect 42.
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let client: ClientT = ClientT::new(client_r, client_w);
        let resp: u32 = rt.block_on(client.call(21u32)).unwrap();
        assert_eq!(resp, 42);

        server_handle.join().unwrap();
    }
}
