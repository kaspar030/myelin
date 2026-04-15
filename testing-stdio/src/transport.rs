//! Postcard transport over any `Read + Write` stream.
//!
//! Serializes requests/responses with postcard, uses length-prefix framing
//! (4-byte little-endian u32 length, then payload).
//! Works over stdio, TCP, serial — anything implementing `std::io::Read + Write`.

use std::cell::RefCell;
use std::io::{self, Read, Write};

use chanapi::transport::{ClientTransport, ServerTransport};
use serde::{Deserialize, Serialize};

/// Postcard transport over a byte stream with length-prefix framing.
///
/// Generic over the read/write halves and the message types.
pub struct PostcardStream<R, W, Incoming, Outgoing> {
    inner: RefCell<PostcardStreamInner<R, W>>,
    _phantom: core::marker::PhantomData<(Incoming, Outgoing)>,
}

struct PostcardStreamInner<R, W> {
    reader: R,
    writer: W,
}

/// Errors from the postcard stream transport.
#[derive(Debug)]
pub enum PostcardStreamError {
    Io(io::Error),
    Postcard(postcard::Error),
    /// The stream was closed (EOF).
    Closed,
}

impl core::fmt::Display for PostcardStreamError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PostcardStreamError::Io(e) => write!(f, "I/O error: {e}"),
            PostcardStreamError::Postcard(e) => write!(f, "postcard error: {e}"),
            PostcardStreamError::Closed => write!(f, "stream closed"),
        }
    }
}

// TransportResult: any T wrapped in Result<T, CallError<PostcardStreamError>>
impl<T> chanapi::TransportResult<T> for PostcardStreamError {
    type Output = Result<T, chanapi::CallError<PostcardStreamError>>;

    fn into_output(result: Result<T, Self>) -> Self::Output {
        result.map_err(chanapi::CallError::Transport)
    }
}

impl<R, W, Incoming, Outgoing> PostcardStream<R, W, Incoming, Outgoing>
where
    R: Read,
    W: Write,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            inner: RefCell::new(PostcardStreamInner { reader, writer }),
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<R: Read, W: Write> PostcardStreamInner<R, W> {
    /// Send a message: serialize with postcard, write [u32 LE length][payload].
    fn send<T: Serialize>(&mut self, msg: &T) -> Result<(), PostcardStreamError> {
        let bytes =
            postcard::to_stdvec(msg).map_err(PostcardStreamError::Postcard)?;
        let len = bytes.len() as u32;
        self.writer
            .write_all(&len.to_le_bytes())
            .map_err(PostcardStreamError::Io)?;
        self.writer
            .write_all(&bytes)
            .map_err(PostcardStreamError::Io)?;
        self.writer.flush().map_err(PostcardStreamError::Io)?;
        Ok(())
    }

    /// Receive a message: read [u32 LE length][payload], deserialize with postcard.
    fn receive<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T, PostcardStreamError> {
        // Read length prefix.
        let mut len_buf = [0u8; 4];
        match self.reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(PostcardStreamError::Closed);
            }
            Err(e) => return Err(PostcardStreamError::Io(e)),
        }
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read payload.
        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .map_err(PostcardStreamError::Io)?;

        postcard::from_bytes(&buf).map_err(PostcardStreamError::Postcard)
    }
}

// -- Client transport: sends Outgoing requests, receives Incoming responses --

impl<R, W, Req, Resp> ClientTransport<Req, Resp> for PostcardStream<R, W, Resp, Req>
where
    R: Read,
    W: Write,
    Req: Serialize,
    Resp: for<'de> Deserialize<'de>,
{
    type Error = PostcardStreamError;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let mut inner = self.inner.borrow_mut();
        inner.send(&req)?;
        inner.receive()
    }
}

// -- Server transport: receives Incoming requests, sends Outgoing responses --

/// Reply token for PostcardStream — just a marker, reply goes to the same stream.
pub struct PostcardReplyToken;

impl<R, W, Req, Resp> ServerTransport<Req, Resp> for PostcardStream<R, W, Req, Resp>
where
    R: Read,
    W: Write,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
{
    type Error = PostcardStreamError;
    type ReplyToken = PostcardReplyToken;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let req = self.inner.borrow_mut().receive()?;
        Ok((req, PostcardReplyToken))
    }

    async fn reply(&self, _token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        self.inner.borrow_mut().send(&resp)
    }
}
