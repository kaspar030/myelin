//! Stack-overflow regression test for
//! [`StreamTransport::with_boxed_router`].
//!
//! Builds a `StreamTransport<_, _, LengthPrefixed, PostcardCodec,
//! MuxedSlots<1, 1_048_576>, _, _, Box<MuxedSlots<1, 1_048_576>>>` on a
//! deliberately-restricted thread stack (512 KiB), round-trips a small
//! request/reply across an in-memory duplex, and asserts the thread did
//! not panic.
//!
//! The router alone is 1 MiB \u2014 twice the worker stack size \u2014 so any
//! construction path that materialises the slot array on the stack
//! before reaching the `StreamTransport` field would stack-overflow.
//! Patterned after `routing::tests::new_boxed_on_restricted_stack`.

#![cfg(all(feature = "postcard", feature = "io-test-utils"))]

use myelin::io::mem_pipe::{PipeReader, PipeWriter, duplex};
use myelin::stream::{LengthPrefixed, MuxedSlots, PostcardCodec, StreamTransport};
use myelin::transport::{ClientTransport, ServerTransport};

const N: usize = 1;
const BUF: usize = 1_048_576; // 1 MiB

// The `ClientTransport<Req, Resp>` impl flips the wire-type order:
// `StreamTransport<..., Resp, Req, _>` is the client side; the server
// side uses `StreamTransport<..., Req, Resp, _>`.
type Router = MuxedSlots<N, BUF>;
type ClientT = StreamTransport<
    PipeReader,
    PipeWriter,
    LengthPrefixed,
    PostcardCodec,
    Router,
    u32, // Resp
    u32, // Req
    Box<Router>,
>;
type ServerT = StreamTransport<
    PipeReader,
    PipeWriter,
    LengthPrefixed,
    PostcardCodec,
    Router,
    u32, // Req
    u32, // Resp
    Box<Router>,
>;

#[test]
fn boxed_round_trip_on_restricted_stack() {
    // 512 KiB \u2014 less than half the 1 MiB router. If the router (or any
    // intermediate copy of it) ever lands on the stack, the spawned
    // thread aborts with a stack overflow and `.join()` returns Err.
    const STACK_SIZE: usize = 512 * 1024;

    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            // 1. Heap-build both routers \u2014 never touches the stack.
            let client_router: Box<Router> = MuxedSlots::<N, BUF>::new_boxed();
            let server_router: Box<Router> = MuxedSlots::<N, BUF>::new_boxed();

            // 2. Wire up an in-memory duplex.
            let ((r_a, w_a), (r_b, w_b)) = duplex();

            // 3. Build both transport ends with the boxed constructor.
            let client: ClientT = StreamTransport::with_boxed_router(
                r_a,
                w_a,
                LengthPrefixed,
                PostcardCodec,
                client_router,
            );
            let mut server: ServerT = StreamTransport::with_boxed_router(
                r_b,
                w_b,
                LengthPrefixed,
                PostcardCodec,
                server_router,
            );

            // 4. Round-trip a small request/reply.
            let result = futures_lite::future::block_on(async {
                let server_fut = async {
                    let (req, token) = server.recv().await.unwrap();
                    server.reply(token, req * 2).await.unwrap();
                };
                let client_fut = async { client.call(21u32).await.unwrap() };
                let (resp, _) = futures_lite::future::zip(client_fut, server_fut).await;
                resp
            });

            assert_eq!(result, 42);
        })
        .expect("spawn restricted-stack worker thread")
        .join()
        .expect("restricted-stack thread panicked (likely stack overflow)");
}
