//! Tokio-based local transport: `mpsc` for requests, `oneshot` for replies.
//!
//! Moves Rust types directly — no serialization.

use tokio::sync::{mpsc, oneshot};

use crate::transport::{ClientTransport, ServerTransport};

/// The client's handle — cloneable, send requests into the service.
#[derive(Clone)]
pub struct TokioClient<Req, Resp> {
    tx: mpsc::Sender<(Req, oneshot::Sender<Resp>)>,
}

/// The server's handle — receive requests, send replies.
pub struct TokioServer<Req, Resp> {
    rx: mpsc::Receiver<(Req, oneshot::Sender<Resp>)>,
}

/// Create a linked client/server pair.
pub fn create<Req, Resp>(channel_depth: usize) -> (TokioClient<Req, Resp>, TokioServer<Req, Resp>) {
    let (tx, rx) = mpsc::channel(channel_depth);
    (TokioClient { tx }, TokioServer { rx })
}

// -- Error --

/// Errors from the tokio local transport.
#[derive(Debug)]
pub enum TokioLocalError {
    /// The channel is closed (other side dropped).
    ChannelClosed,
}

impl core::fmt::Display for TokioLocalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TokioLocalError::ChannelClosed => write!(f, "channel closed"),
        }
    }
}

// -- Client --

impl<Req, Resp> ClientTransport<Req, Resp> for TokioClient<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Error = TokioLocalError;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send((req, tx))
            .await
            .map_err(|_| TokioLocalError::ChannelClosed)?;
        rx.await.map_err(|_| TokioLocalError::ChannelClosed)
    }
}

// -- Server --

impl<Req, Resp> ServerTransport<Req, Resp> for TokioServer<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Error = TokioLocalError;
    type ReplyToken = oneshot::Sender<Resp>;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        self.rx.recv().await.ok_or(TokioLocalError::ChannelClosed)
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        token.send(resp).map_err(|_| TokioLocalError::ChannelClosed)
    }
}
