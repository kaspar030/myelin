//! Embassy-based local transport: static `Channel` for requests, per-client `Signal` for replies.
//!
//! Moves Rust types directly — no serialization. All storage is static.
//! The transport is infallible — channel send/receive and signal wait never fail.
//!
//! ## Usage
//!
//! ```ignore
//! use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
//!
//! // The service channel:
//! static SERVICE: EmbassyService<CriticalSectionRawMutex, MyReq, MyResp, 4> =
//!     EmbassyService::new();
//!
//! // In each client task — one line:
//! let client = MyClient::new(myelin::embassy_client!(&SERVICE));
//!
//! // In server task:
//! let mut server = SERVICE.server();
//! ```

mod client;
mod server;
mod service;

pub use client::EmbassyClient;
pub use server::{EmbassyReplyToken, EmbassyServer};
pub use service::EmbassyService;
