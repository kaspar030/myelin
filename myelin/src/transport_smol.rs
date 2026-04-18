//! Smol-compatible local transport, mirroring [`transport_tokio`](crate::transport_tokio).
//!
//! Uses [`async-channel`] for request mpsc + bounded(1) reply channels.
//! Behaviourally identical to the tokio local transport; differs only
//! in the underlying channel types.
//!
//! Enable via the `smol` feature.

use async_channel::{Receiver, Sender};

use crate::error::{CallError, TransportResult};
use crate::transport::{ClientTransport, ServerTransport};

/// The service endpoint — owns the channel, hands out client and server handles.
pub struct SmolService<Req, Resp> {
    tx: Sender<(Req, Sender<Resp>)>,
    rx: Option<Receiver<(Req, Sender<Resp>)>>,
}

impl<Req, Resp> SmolService<Req, Resp> {
    /// Create a new service with the given channel depth.
    pub fn new(channel_depth: usize) -> Self {
        let (tx, rx) = async_channel::bounded(channel_depth);
        Self { tx, rx: Some(rx) }
    }

    /// Get a client handle. Cloneable — multiple clients can share the same channel.
    pub fn client(&self) -> SmolClient<Req, Resp> {
        SmolClient {
            tx: self.tx.clone(),
        }
    }

    /// Take the server handle. Can only be called once.
    pub fn server(&mut self) -> SmolServer<Req, Resp> {
        SmolServer {
            rx: self.rx.take().expect("server() called more than once"),
        }
    }
}

/// The client's handle — cloneable, send requests into the service.
#[derive(Clone)]
pub struct SmolClient<Req, Resp> {
    tx: Sender<(Req, Sender<Resp>)>,
}

/// The server's handle — receive requests, send replies.
pub struct SmolServer<Req, Resp> {
    rx: Receiver<(Req, Sender<Resp>)>,
}

/// Errors from the smol local transport.
#[derive(Debug)]
pub enum SmolLocalError {
    /// The channel is closed (other side dropped).
    ChannelClosed,
}

impl core::fmt::Display for SmolLocalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SmolLocalError::ChannelClosed => write!(f, "channel closed"),
        }
    }
}

impl<T> TransportResult<T> for SmolLocalError {
    type Output = Result<T, CallError<SmolLocalError>>;

    fn into_output(result: Result<T, Self>) -> Self::Output {
        result.map_err(CallError::Transport)
    }
}

impl<Req, Resp> ClientTransport<Req, Resp> for SmolClient<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Error = SmolLocalError;

    /// # Cancel Safety
    ///
    /// Same contract as the tokio local transport:
    /// - Cancelled before `send`: no effect.
    /// - Cancelled after `send`: server does wasted work, reply
    ///   discarded when `reply.send()` returns `Err`.
    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        self.tx
            .send((req, reply_tx))
            .await
            .map_err(|_| SmolLocalError::ChannelClosed)?;
        reply_rx
            .recv()
            .await
            .map_err(|_| SmolLocalError::ChannelClosed)
    }
}

impl<Req, Resp> ServerTransport<Req, Resp> for SmolServer<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Error = SmolLocalError;
    type ReplyToken = Sender<Resp>;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        self.rx
            .recv()
            .await
            .map_err(|_| SmolLocalError::ChannelClosed)
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        token
            .send(resp)
            .await
            .map_err(|_| SmolLocalError::ChannelClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smol_service_round_trip() {
        futures_lite::future::block_on(async {
            let mut svc = SmolService::<u32, u32>::new(4);
            let client = svc.client();
            let mut server = svc.server();

            let server_task = async move {
                let (req, token) = server.recv().await.unwrap();
                server.reply(token, req * 2).await.unwrap();
            };
            let client_task = async move { client.call(21u32).await.unwrap() };

            let (resp, _) = futures_lite::future::zip(client_task, server_task).await;
            assert_eq!(resp, 42);
        });
    }

    #[test]
    fn smol_service_multiple_clients() {
        futures_lite::future::block_on(async {
            let mut svc = SmolService::<u32, u32>::new(8);
            let c1 = svc.client();
            let c2 = svc.client();
            let mut server = svc.server();

            let server_task = async move {
                for _ in 0..4 {
                    let (req, token) = server.recv().await.unwrap();
                    server.reply(token, req + 1).await.unwrap();
                }
            };
            let clients_task = async move {
                let a = c1.call(1u32);
                let b = c2.call(2u32);
                let c = c1.call(3u32);
                let d = c2.call(4u32);
                let ((r1, r2), (r3, r4)) = futures_lite::future::zip(
                    futures_lite::future::zip(a, b),
                    futures_lite::future::zip(c, d),
                )
                .await;
                (r1.unwrap(), r2.unwrap(), r3.unwrap(), r4.unwrap())
            };

            let (vals, _) = futures_lite::future::zip(clients_task, server_task).await;
            let mut v = [vals.0, vals.1, vals.2, vals.3];
            v.sort();
            assert_eq!(v, [2, 3, 4, 5]);
        });
    }
}
