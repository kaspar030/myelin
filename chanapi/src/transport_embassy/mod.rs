//! Embassy-based local transport: static `Channel` for requests, per-client `Signal` for replies.
//!
//! Moves Rust types directly — no serialization. All storage is static.
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
//! let client = MyClient::new(chanapi::embassy_client!(&SERVICE));
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

/// Errors from the embassy local transport.
#[derive(Debug)]
pub enum EmbassyLocalError {
    // Currently infallible for local transport, but reserved for future use.
}

impl core::fmt::Display for EmbassyLocalError {
    fn fmt(&self, _f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Ok(())
    }
}
