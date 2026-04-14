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

pub mod channel;
