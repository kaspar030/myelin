//! Adapter: implement [`AsyncBytesRead`]/[`AsyncBytesWrite`] for any
//! type bounded by [`tokio::io::AsyncRead`]/[`AsyncWrite`].
//!
//! Enable via the `tokio-io` feature. Independent from (and composable
//! with) the `tokio` feature, which is for in-process channel transport
//! only.

use core::future::Future;
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{AsyncBytesRead, AsyncBytesWrite};

/// Newtype: wrap a `tokio::io::AsyncRead` as an [`AsyncBytesRead`].
pub struct TokioIoReader<R>(pub R);

impl<R> TokioIoReader<R> {
    pub const fn new(inner: R) -> Self { Self(inner) }
    pub fn into_inner(self) -> R { self.0 }
}

impl<R: AsyncRead + Unpin> AsyncBytesRead for TokioIoReader<R> {
    type Error = io::Error;

    fn read_exact(&mut self, buf: &mut [u8])
        -> impl Future<Output = Result<(), io::Error>>
    {
        async move {
            self.0.read_exact(buf).await.map(|_| ())
        }
    }
}

/// Newtype: wrap a `tokio::io::AsyncWrite` as an [`AsyncBytesWrite`].
pub struct TokioIoWriter<W>(pub W);

impl<W> TokioIoWriter<W> {
    pub const fn new(inner: W) -> Self { Self(inner) }
    pub fn into_inner(self) -> W { self.0 }
}

impl<W: AsyncWrite + Unpin> AsyncBytesWrite for TokioIoWriter<W> {
    type Error = io::Error;

    fn write_all(&mut self, buf: &[u8])
        -> impl Future<Output = Result<(), io::Error>>
    {
        async move { self.0.write_all(buf).await }
    }

    fn flush(&mut self) -> impl Future<Output = Result<(), io::Error>> {
        async move { self.0.flush().await }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{AsyncBytesRead, AsyncBytesWrite};
    use crate::stream::{LengthPrefixed, PostcardCodec, MuxedSlots, StreamTransport};
    use crate::transport::{ClientTransport, ServerTransport};

    #[tokio::test]
    async fn read_exact_and_write_all_round_trip() {
        let (a, b) = tokio::io::duplex(64);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut aw = TokioIoWriter::new(aw);
        let mut br = TokioIoReader::new(br);
        let payload = [1u8, 2, 3, 4, 5];
        aw.write_all(&payload).await.unwrap();
        aw.flush().await.unwrap();
        let mut buf = [0u8; 5];
        br.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, payload);
        drop(ar);
        drop(bw);
    }

    #[tokio::test]
    async fn muxed_stream_over_tokio_duplex() {
        // Integration test: replaces the old UnixStream-based test.
        // Muxed client/server over a tokio in-memory duplex pipe.
        let (a, b) = tokio::io::duplex(1024);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);

        type ClientT = StreamTransport<
            TokioIoReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
            TokioIoWriter<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
            LengthPrefixed, PostcardCodec,
            MuxedSlots<4, 128>, u32, u32,
        >;
        type ServerT = StreamTransport<
            TokioIoReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
            TokioIoWriter<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
            LengthPrefixed, PostcardCodec,
            MuxedSlots<4, 128>, u32, u32,
        >;

        let client: ClientT = ClientT::new(TokioIoReader::new(ar), TokioIoWriter::new(aw));
        let mut server: ServerT = ServerT::new(TokioIoReader::new(br), TokioIoWriter::new(bw));

        let server_task = tokio::spawn(async move {
            let (req, token) = server.recv().await.unwrap();
            server.reply(token, req * 3).await.unwrap();
        });

        let resp = client.call(14u32).await.unwrap();
        assert_eq!(resp, 42);
        server_task.await.unwrap();
    }
}
