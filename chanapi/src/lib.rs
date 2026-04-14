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

pub mod error;
pub mod transport;

#[cfg(feature = "tokio")]
pub mod transport_tokio;

pub use error::CallError;
pub use transport::{ClientTransport, ServerTransport};
