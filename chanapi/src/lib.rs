#![no_std]

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

pub mod block_on;
pub mod error;
pub mod transport;

#[cfg(feature = "tokio")]
pub mod transport_tokio;

#[cfg(feature = "embassy")]
pub mod transport_embassy;

pub use block_on::BlockOn;
pub use error::{CallError, TransportResult};
pub use transport::{ClientTransport, ServerTransport};
