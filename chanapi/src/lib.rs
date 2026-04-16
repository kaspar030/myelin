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
