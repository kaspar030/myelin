#![cfg_attr(not(feature = "std"), no_std)]

//! # myelin
//!
//! Define async service APIs as traits, communicate over channels.
//!
//! The trait definition is the single source of truth. A proc macro generates
//! the channel plumbing: request/response enums, client stubs, and server
//! dispatch. Transport and serialization are pluggable — local in-process
//! channels just move `Send` structs; cross-boundary transports serialize
//! transparently.
//!
//! ## Transports
//!
//! - [`ClientTransport`] — client side: make a request, get a response.
//! - [`ServerTransport`] — server side: receive requests, send responses.
//!
//! Implementations:
//! - `transport_tokio` (feature `tokio`) — tokio mpsc + oneshot, local, no serialization.
//! - `transport_smol` (feature `smol`) — async-channel mpsc + bounded(1) reply
//!   channel, local, no serialization.
//! - `transport_embassy` (feature `embassy`) — embassy static channels + signals.
//! - `transport_postcard` (feature `postcard`) — postcard serialization over any
//!   sync `Read + Write` stream, length-prefix framing. Internally wraps
//!   the sync I/O in [`io::BlockingIo`] so the async transport stack runs
//!   each read/write inline — suitable for stdio-style binaries driven
//!   by a trivial [`BlockOn`].
//!
//! ## Async I/O
//!
//! The stream transport ([`stream::StreamTransport`]) operates on
//! [`io::AsyncBytesRead`] / [`io::AsyncBytesWrite`] — myelin's own
//! runtime-neutral async byte-stream traits (just `read_exact`,
//! `write_all`, `flush`). Bring your own reader/writer by implementing
//! those traits, or use one of the feature-gated adapters:
//!
//! - [`io::BlockingIo`] — wrap any sync `std::io::Read`/`Write`. The
//!   async methods resolve inline; used by `transport_postcard` for
//!   stdio transport.
//! - [`io::futures_io`] (feature `futures-io`) — adapt any type bounded
//!   by `futures_io::AsyncRead` / `AsyncWrite`. Covers smol's
//!   `smol::Async<T>`, async-std, and anything in the futures-io
//!   ecosystem.
//! - [`io::tokio_io`] (feature `tokio-io`) — adapt `tokio::io::AsyncRead`
//!   / `AsyncWrite`. Independent of the `tokio` feature (channels-only
//!   transport).
//!
//! Inside myelin core, shared access to a reader/writer between
//! concurrent async tasks is mediated by a small single-waiter async
//! mutex, [`io::LocalLock`]. It replaces the old `RefCell`-based
//! scheme (unsound across `.await`) and is cancel-safe.
//!
//! ## Duplex transport
//!
//! For peers that both call *and* serve over one byte stream, use
//! [`stream::DuplexStreamTransport`]. It owns one `(reader, writer)`
//! pair and vends:
//!
//! - [`stream::DuplexClientHalf`] — `impl ClientTransport` per API
//!   that this peer calls into.
//! - [`stream::DuplexServerHalf`] — `impl ServerTransport` per API
//!   that this peer serves.
//! - [`stream::DuplexPump`] — a `run()` future the user's runtime
//!   spawns; it drives the reader and demultiplexes frames into
//!   either the slot router (responses) or a per-api_id server inbox
//!   (requests).
//!
//! Wire format inside each framed payload: `[u8 kind][u16 api_id LE]
//! [u8 slot_id][codec bytes]`. `kind` is `0` (request) or `1`
//! (response); `api_id` identifies the API; `slot_id` is the caller's
//! echo-back value. See [`stream::duplex`] for details.
//!
//! Example (sketch, full example in `tests/duplex_smol_integration.rs`):
//!
//! ```ignore
//! use myelin::io::futures_io::{FuturesIoReader, FuturesIoWriter};
//! use myelin::stream::{DuplexStreamTransport, LengthPrefixed, PostcardCodec};
//!
//! type Dx<R, W> = DuplexStreamTransport<R, W, LengthPrefixed, PostcardCodec, 8, 512>;
//!
//! # fn main() {
//! # let (r, w): (FuturesIoReader<smol::io::Empty>, FuturesIoWriter<smol::io::Sink>) = panic!();
//! let dx: Dx<_, _> = Dx::new(r, w);
//! let server = dx.server_half::<u32, u32>(0x0001);
//! let client = dx.client_half::<u32, u32>(0x0002);
//! let (pump, _handle) = dx.split();
//! smol::block_on(async {
//!     // Run pump concurrently with your server loop and client calls.
//!     let _ = futures_lite::future::zip(pump.run(), async { /* ... */ }).await;
//! });
//! # }
//! ```
//!
//! ## Cancel Safety
//!
//! All myelin transports provide the following cancel safety guarantees:
//!
//! ### Cancelling a client call is always safe
//!
//! A client's `call()` future can be dropped at any `.await` point without
//! corrupting the transport or leaking state.
//!
//! - **Dropped before the request is sent:** No effect. The request was never
//!   enqueued and no server-side resources are consumed.
//!
//! - **Dropped after the request is sent, before the reply is received:** The
//!   server will still process the request and produce a response, but that
//!   response is discarded. This means the server does *wasted work*, but
//!   there is no protocol corruption, no leaked memory, and no poisoned state.
//!   The client can immediately make another call.
//!
//! ### Cancelling a server task affects in-flight clients
//!
//! If the server task/thread is cancelled or shut down, clients with in-flight
//! requests will observe a transport-specific outcome:
//!
//! - **Tokio:** The client receives a `ChannelClosed` error (the mpsc/oneshot
//!   senders are dropped).
//! - **Embassy:** The client's `Signal::wait()` will never complete — the client
//!   hangs. Avoid cancelling embassy server tasks while clients are in-flight.
//! - **PostcardStream:** The underlying I/O stream is closed, producing an I/O
//!   error on the client side.
//!
//! ### Summary
//!
//! | Scenario | Result |
//! |---|---|
//! | Client cancelled before send | Clean, no effect |
//! | Client cancelled after send | Server does wasted work, reply discarded |
//! | Server cancelled | In-flight clients get an error (tokio/postcard) or hang (embassy) |

pub mod block_on;
pub mod error;
pub mod transport;

#[cfg(feature = "std")]
pub mod io;

#[cfg(feature = "tokio")]
pub mod transport_tokio;

#[cfg(feature = "embassy")]
pub mod transport_embassy;

#[cfg(any(feature = "postcard", feature = "cbor"))]
pub mod stream;

#[cfg(feature = "postcard")]
pub mod transport_postcard;

#[cfg(feature = "smol")]
pub mod transport_smol;

pub use block_on::BlockOn;
pub use error::{CallError, TransportResult};
pub use transport::{ClientTransport, ServerTransport};

/// Generate channel-API plumbing from an async trait definition.
///
/// See the [`myelin_macros::service`] documentation for the full input
/// contract and emitted items.
pub use myelin_macros::service;

// Re-export paste for use by compose_service! and #[myelin::service] macros.
//
// These two lines re-export, under the name `paste`:
//   - the `paste!` *macro* (in the macro namespace), so that existing
//     `$crate::paste!` invocations in `compose_service!` continue to work.
//   - the `paste` *crate* (in the type/module namespace), so that code emitted
//     by `#[myelin::service]` can reference `::myelin::paste::paste!`.
// Macros and items live in separate namespaces, so the two `paste` names
// coexist without conflict.
#[doc(hidden)]
pub use ::paste;
#[doc(hidden)]
pub use ::paste::paste;

// Re-export static_cell under the `embassy` feature so that code emitted by
// `#[myelin::service]` can refer to `::myelin::static_cell::StaticCell`
// without the consumer having to depend on (or re-export) `static_cell`
// themselves. The absolute path is part of the emitted `macro_rules!` body
// and matches the "no crate-override" policy of v1.
#[cfg(feature = "embassy")]
#[doc(hidden)]
pub use static_cell;

/// Compose multiple service APIs into a single multiplexed service.
///
/// Generates combined request/response enums, a client that provides typed
/// accessors for each sub-service, dispatch serve functions, and convenience
/// type aliases for tokio/embassy transports.
///
/// Each component service must provide:
/// - `{Service}Request` / `{Service}Response` enums
/// - `{Service}Service` async trait and `{Service}ServiceSync` sync trait
/// - `{Service}Client` / `{Service}ClientSync` structs
/// - `{service}_dispatch` async function: `(&S, {Service}Request) -> {Service}Response`
/// - `{service}_dispatch_sync` sync function: `(&S, {Service}Request) -> {Service}Response`
///
/// The dispatch functions handle a single request→response, unlike the serve
/// functions which run an infinite loop.
///
/// # Usage
///
/// ```ignore
/// myelin::compose_service!(
///     Combined,
///     [Greeter, greeter_dispatch, greeter_dispatch_sync],
///     [Math, math_dispatch, math_dispatch_sync],
/// );
/// ```
///
/// This generates:
/// - `CombinedRequest` / `CombinedResponse` wrapper enums
/// - `CombinedClient<T>` with `.greeter()` and `.math()` accessors
/// - `CombinedClientSync<T, B>` with `.greeter()` and `.math()` accessors
/// - `combined_serve()` and `combined_serve_sync()` dispatch loop functions
/// - `CombinedTokioService` type alias (behind `tokio` feature)
/// - Embassy types (behind `embassy` feature)
#[macro_export]
macro_rules! compose_service {
    (
        $name:ident,
        $([$svc:ident, $dispatch_fn:ident, $dispatch_sync_fn:ident]),+ $(,)?
    ) => {
        $crate::paste! {
            // ===== Request enum =====
            #[derive(Debug)]
            #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
            pub enum [<$name Request>] {
                $(
                    $svc([<$svc Request>]),
                )+
            }

            // ===== Response enum =====
            #[derive(Debug)]
            #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
            pub enum [<$name Response>] {
                $(
                    $svc([<$svc Response>]),
                )+
            }

            // ===== Client adapter per component =====
            //
            // Each adapter wraps a shared reference to the composed transport
            // and implements ClientTransport<ComponentReq, ComponentResp> by
            // wrapping/unwrapping through the composed enums.
            $(
                pub struct [<$name $svc Adapter>]<T> {
                    transport: T,
                }

                impl<T> $crate::ClientTransport<[<$svc Request>], [<$svc Response>]>
                    for [<$name $svc Adapter>]<T>
                where
                    T: $crate::ClientTransport<[<$name Request>], [<$name Response>]>,
                {
                    type Error = T::Error;

                    async fn call(
                        &self,
                        req: [<$svc Request>],
                    ) -> Result<[<$svc Response>], Self::Error> {
                        let resp = self
                            .transport
                            .call([<$name Request>]::$svc(req))
                            .await?;
                        match resp {
                            [<$name Response>]::$svc(inner) => Ok(inner),
                            _ => unreachable!("composed response variant mismatch"),
                        }
                    }
                }
            )+

            // ===== Composed async client =====
            pub struct [<$name Client>]<T> {
                transport: T,
            }

            impl<T> [<$name Client>]<T>
            where
                T: $crate::ClientTransport<[<$name Request>], [<$name Response>]>,
            {
                pub fn new(transport: T) -> Self {
                    Self { transport }
                }

                $(
                    pub fn [<$svc:lower>](&self) -> [<$svc Client>]<[<$name $svc Adapter>]<&T>> {
                        [<$svc Client>]::new([<$name $svc Adapter>] {
                            transport: &self.transport,
                        })
                    }
                )+
            }

            // ===== Composed sync client =====
            pub struct [<$name ClientSync>]<T, B> {
                transport: T,
                block_on: B,
            }

            impl<T, B> [<$name ClientSync>]<T, B>
            where
                T: $crate::ClientTransport<[<$name Request>], [<$name Response>]>,
                B: $crate::BlockOn,
            {
                pub fn new(client: [<$name Client>]<T>, block_on: B) -> Self {
                    Self {
                        transport: client.transport,
                        block_on,
                    }
                }

                $(
                    pub fn [<$svc:lower>](&self) -> [<$svc ClientSync>]<[<$name $svc Adapter>]<&T>, &B> {
                        [<$svc ClientSync>]::new(
                            [<$svc Client>]::new([<$name $svc Adapter>] {
                                transport: &self.transport,
                            }),
                            &self.block_on,
                        )
                    }
                )+
            }

            // ===== Async serve function =====
            pub async fn [<$name:snake _serve>]<S, T>(
                svc: &S,
                transport: &mut T,
            ) -> Result<(), T::Error>
            where
                S: $([<$svc Service>] +)+ Sized,
                T: $crate::ServerTransport<[<$name Request>], [<$name Response>]>,
            {
                loop {
                    let (req, token) = transport.recv().await?;
                    let resp = match req {
                        $(
                            [<$name Request>]::$svc(inner) => {
                                [<$name Response>]::$svc($dispatch_fn(svc, inner).await)
                            }
                        )+
                    };
                    let _ = transport.reply(token, resp).await;
                }
            }

            // ===== Sync serve function =====
            pub fn [<$name:snake _serve_sync>]<S, T, B>(
                svc: &S,
                transport: &mut T,
                block_on: &B,
            ) -> Result<(), T::Error>
            where
                S: $([<$svc ServiceSync>] +)+ Sized,
                T: $crate::ServerTransport<[<$name Request>], [<$name Response>]>,
                B: $crate::BlockOn,
            {
                loop {
                    let (req, token) = block_on.block_on(transport.recv())?;
                    let resp = match req {
                        $(
                            [<$name Request>]::$svc(inner) => {
                                [<$name Response>]::$svc($dispatch_sync_fn(svc, inner))
                            }
                        )+
                    };
                    let _ = block_on.block_on(transport.reply(token, resp));
                }
            }

            // ===== Tokio convenience types =====
            #[cfg(feature = "tokio")]
            pub type [<$name TokioService>] =
                $crate::transport_tokio::TokioService<[<$name Request>], [<$name Response>]>;

            // ===== Embassy convenience types =====
            #[cfg(feature = "embassy")]
            pub type [<$name EmbassyService>]<M, const CHANNEL_DEPTH: usize> =
                $crate::transport_embassy::EmbassyService<
                    M,
                    [<$name Request>],
                    [<$name Response>],
                    CHANNEL_DEPTH,
                >;

            #[cfg(feature = "embassy")]
            pub type [<$name EmbassyClientTransport>]<'a, M, const CHANNEL_DEPTH: usize> =
                $crate::transport_embassy::EmbassyClient<
                    'a,
                    M,
                    [<$name Request>],
                    [<$name Response>],
                    CHANNEL_DEPTH,
                >;

            // Note: Embassy service instantiation macros (like `combined_embassy_service!`)
            // cannot be generated by `compose_service!` due to macro_rules nesting
            // limitations with `paste!`. Hand-write them in the service crate if needed,
            // or use the generated type aliases directly.
        }
    };
}
