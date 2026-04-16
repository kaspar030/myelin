#![cfg_attr(not(feature = "std"), no_std)]

//! # chanapi
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
//! - `transport_embassy` (feature `embassy`) — embassy static channels + signals.
//! - `transport_postcard` (feature `postcard`) — postcard serialization over any
//!   `Read + Write` stream, length-prefix framing.
//!
//! ## Cancel Safety
//!
//! All chanapi transports provide the following cancel safety guarantees:
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

#[cfg(feature = "tokio")]
pub mod transport_tokio;

#[cfg(feature = "embassy")]
pub mod transport_embassy;

#[cfg(feature = "postcard")]
pub mod stream;

#[cfg(feature = "postcard")]
pub mod transport_postcard;

pub use block_on::BlockOn;
pub use error::{CallError, TransportResult};
pub use transport::{ClientTransport, ServerTransport};

// Re-export paste for use by compose_service! macro.
#[doc(hidden)]
pub use paste::paste;

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
/// chanapi::compose_service!(
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
