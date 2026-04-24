//! Framing-independence test for `CborCodec`.
//!
//! Wires `StreamTransport<_, _, LengthPrefixed, CborCodec, MuxedSlots<_, _>, Req, Resp>`
//! end-to-end over an in-memory duplex pipe, proving the new codec slots
//! into the existing framing and routing layers unchanged.

#![cfg(all(feature = "cbor", feature = "io-test-utils"))]

use myelin::io::mem_pipe::{PipeReader, PipeWriter, duplex};
use myelin::stream::{CborCodec, LengthPrefixed, MuxedSlots, StreamTransport};
use myelin::transport::{ClientTransport, ServerTransport};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Req {
    op: String,
    value: i32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Resp {
    ok: bool,
    value: i32,
}

// The `ClientTransport<Req, Resp>` impl on `StreamTransport` flips the
// wire-type order: `StreamTransport<..., Resp, Req>` carries a client.
// The `ServerTransport<Req, Resp>` impl uses `StreamTransport<..., Req, Resp>`.
type ClientT = StreamTransport<
    PipeReader,
    PipeWriter,
    LengthPrefixed,
    CborCodec,
    MuxedSlots<4, 128>,
    Resp,
    Req,
>;
type ServerT = StreamTransport<
    PipeReader,
    PipeWriter,
    LengthPrefixed,
    CborCodec,
    MuxedSlots<4, 128>,
    Req,
    Resp,
>;

fn block_on<F: core::future::Future>(fut: F) -> F::Output {
    futures_lite::future::block_on(fut)
}

#[test]
fn cbor_codec_round_trips_over_stream_transport() {
    let ((r_a, w_a), (r_b, w_b)) = duplex();

    let client: ClientT = ClientT::new(r_a, w_a);
    let mut server: ServerT = ServerT::new(r_b, w_b);

    let resp = block_on(async {
        let server_fut = async {
            let (req, token) = server.recv().await.unwrap();
            assert_eq!(req.op, "double");
            assert_eq!(req.value, 21);
            server
                .reply(
                    token,
                    Resp {
                        ok: true,
                        value: req.value * 2,
                    },
                )
                .await
                .unwrap();
        };
        let client_fut = async {
            client
                .call(Req {
                    op: "double".into(),
                    value: 21,
                })
                .await
                .unwrap()
        };
        let (resp, _) = futures_lite::future::zip(client_fut, server_fut).await;
        resp
    });

    assert_eq!(
        resp,
        Resp {
            ok: true,
            value: 42,
        }
    );
}
