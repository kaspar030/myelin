//! Cross-cutting integration test: `DuplexStreamTransport` over a
//! smol `Async<UnixStream>` pair, with two services running in each
//! direction.
//!
//! This exercises the whole stack: async I/O traits, `futures-io`
//! adapter, duplex demultiplexer, muxed slots, and postcard codec —
//! all driven by smol's single-threaded executor via
//! `futures_lite::future::block_on`.

#![cfg(all(feature = "smol", feature = "futures-io", feature = "postcard"))]

use myelin::io::futures_io::{FuturesIoReader, FuturesIoWriter};
use myelin::stream::{DuplexStreamTransport, LengthPrefixed, PostcardCodec};
use myelin::transport::{ClientTransport, ServerTransport};

use serde::{Deserialize, Serialize};
use smol::Async;
use std::os::unix::net::UnixStream;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PingReq(u32);
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PingResp(u32);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct EchoReq(String);
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct EchoResp(String);

const PING_API: u16 = 0x0010;
const ECHO_API: u16 = 0x0020;

type DxT<R, W> = DuplexStreamTransport<R, W, LengthPrefixed, PostcardCodec, 8, 512>;

#[test]
fn smol_duplex_two_services_both_directions() {
    let (sa, sb) = UnixStream::pair().expect("UnixStream::pair");
    sa.set_nonblocking(true).unwrap();
    sb.set_nonblocking(true).unwrap();
    let sa = Async::new(sa).unwrap();
    let sb = Async::new(sb).unwrap();

    // Split each Async<UnixStream> into independent read/write halves.
    // `Async<UnixStream>` doesn't expose a split, but we can clone the
    // underlying FD for reads and writes — UnixStream is safe to
    // duplicate for one reader + one writer concurrently.
    let sa_writer = sa.get_ref().try_clone().unwrap();
    sa_writer.set_nonblocking(true).unwrap();
    let sa_writer = Async::new(sa_writer).unwrap();

    let sb_writer = sb.get_ref().try_clone().unwrap();
    sb_writer.set_nonblocking(true).unwrap();
    let sb_writer = Async::new(sb_writer).unwrap();

    // Each side: reader = original Async<UnixStream>, writer = cloned one.
    let r_a = FuturesIoReader::new(sa);
    let w_a = FuturesIoWriter::new(sa_writer);
    let r_b = FuturesIoReader::new(sb);
    let w_b = FuturesIoWriter::new(sb_writer);

    let dx_a: DxT<_, _> = DxT::new(r_a, w_a);
    let dx_b: DxT<_, _> = DxT::new(r_b, w_b);

    // A serves PING; B serves ECHO.
    let ping_server_a = dx_a.server_half::<PingReq, PingResp>(PING_API);
    let echo_client_a = dx_a.client_half::<EchoReq, EchoResp>(ECHO_API);

    let echo_server_b = dx_b.server_half::<EchoReq, EchoResp>(ECHO_API);
    let ping_client_b = dx_b.client_half::<PingReq, PingResp>(PING_API);

    let (pump_a, _) = dx_a.split();
    let (pump_b, _) = dx_b.split();

    futures_lite::future::block_on(async {
        let mut ping_srv = ping_server_a;
        let server_a = async move {
            // Serve 3 ping requests.
            for _ in 0..3 {
                let (req, token) = ping_srv.recv().await.unwrap();
                ping_srv.reply(token, PingResp(req.0 + 100)).await.unwrap();
            }
        };

        let mut echo_srv = echo_server_b;
        let server_b = async move {
            for _ in 0..3 {
                let (req, token) = echo_srv.recv().await.unwrap();
                echo_srv
                    .reply(token, EchoResp(format!("echo: {}", req.0)))
                    .await
                    .unwrap();
            }
        };

        let client_a = async {
            // Call B.ECHO 3 times concurrently.
            let r1 = echo_client_a.call(EchoReq("one".to_string()));
            let r2 = echo_client_a.call(EchoReq("two".to_string()));
            let r3 = echo_client_a.call(EchoReq("three".to_string()));
            let ((r1, r2), r3) =
                futures_lite::future::zip(futures_lite::future::zip(r1, r2), r3).await;
            (r1.unwrap(), r2.unwrap(), r3.unwrap())
        };

        let client_b = async {
            let r1 = ping_client_b.call(PingReq(1));
            let r2 = ping_client_b.call(PingReq(2));
            let r3 = ping_client_b.call(PingReq(3));
            let ((r1, r2), r3) =
                futures_lite::future::zip(futures_lite::future::zip(r1, r2), r3).await;
            (r1.unwrap(), r2.unwrap(), r3.unwrap())
        };

        let work = async {
            let ((echos, pings), _) = futures_lite::future::zip(
                futures_lite::future::zip(client_a, client_b),
                futures_lite::future::zip(server_a, server_b),
            )
            .await;
            let mut echo_msgs = [echos.0.0, echos.1.0, echos.2.0];
            echo_msgs.sort();
            assert_eq!(
                echo_msgs,
                [
                    "echo: one".to_string(),
                    "echo: three".to_string(),
                    "echo: two".to_string()
                ]
            );
            let mut ping_vals = [pings.0.0, pings.1.0, pings.2.0];
            ping_vals.sort();
            assert_eq!(ping_vals, [101, 102, 103]);
        };

        // Pumps never complete normally. `or` returns when work does.
        futures_lite::future::or(work, async {
            let _ = futures_lite::future::zip(pump_a.run(), pump_b.run()).await;
        })
        .await;
    });
}
