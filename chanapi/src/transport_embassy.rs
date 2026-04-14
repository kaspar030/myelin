//! Embassy-based local transport: static `Channel` for requests, per-client `Signal` for replies.
//!
//! Moves Rust types directly — no serialization. All storage is static.
//!
//! ## Usage
//!
//! ```ignore
//! use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
//! use static_cell::StaticCell;
//!
//! // The service channel (just requests):
//! static SERVICE: EmbassyService<CriticalSectionRawMutex, MyReq, MyResp, 4> =
//!     EmbassyService::new();
//!
//! // In each client task, create a static client:
//! static CLIENT: StaticCell<EmbassyClient<CriticalSectionRawMutex, MyReq, MyResp, 4>> =
//!     StaticCell::new();
//! let client = CLIENT.init(SERVICE.client());
//!
//! // In server task:
//! let mut server = SERVICE.server();
//! ```

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_sync::signal::Signal;

use crate::transport::{ClientTransport, ServerTransport};

/// The service endpoint — just the request channel.
///
/// Place this in a `static`. Clients and server borrow from it.
///
/// - `M`: `RawMutex` implementation
/// - `Req`: request enum
/// - `Resp`: response enum
/// - `CHANNEL_DEPTH`: depth of the request channel
pub struct EmbassyService<M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize> {
    requests: Channel<M, (Req, &'static Signal<M, Resp>), CHANNEL_DEPTH>,
}

impl<M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    EmbassyService<M, Req, Resp, CHANNEL_DEPTH>
{
    /// Create a new service channel. Use in a `static`.
    pub const fn new() -> Self {
        Self {
            requests: Channel::new(),
        }
    }

    /// Create a client. The returned client must be placed in a `StaticCell`
    /// (or other `'static` storage) before use, because it contains the
    /// reply `Signal` that the server writes into.
    pub fn client(&self) -> EmbassyClient<'_, M, Req, Resp, CHANNEL_DEPTH> {
        EmbassyClient {
            sender: self.requests.sender(),
            reply: Signal::new(),
        }
    }

    /// Get a server handle.
    pub fn server(&self) -> EmbassyServer<'_, M, Req, Resp, CHANNEL_DEPTH> {
        EmbassyServer { transport: self }
    }
}

// -- Error --

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

// -- Client --

/// Client handle to an embassy service.
///
/// Each client owns its own reply `Signal`. Must live at a `'static` address
/// (e.g., in a `StaticCell`) because a reference to the signal is sent
/// through the channel with each request.
pub struct EmbassyClient<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize> {
    sender: Sender<'a, M, (Req, &'static Signal<M, Resp>), CHANNEL_DEPTH>,
    reply: Signal<M, Resp>,
}

impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    ClientTransport<Req, Resp> for EmbassyClient<'a, M, Req, Resp, CHANNEL_DEPTH>
{
    type Error = EmbassyLocalError;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        // Reset the signal so we don't see a stale value.
        self.reply.reset();

        // SAFETY: The caller guarantees this client lives in `'static` storage
        // (e.g., StaticCell). The signal reference is valid for the duration of
        // the request because we block on wait() below — the client cannot be
        // dropped while in-flight.
        let signal_ref: &'static Signal<M, Resp> =
            unsafe { core::mem::transmute::<&Signal<M, Resp>, &'static Signal<M, Resp>>(&self.reply) };

        // Send request with our reply signal.
        self.sender.send((req, signal_ref)).await;

        // Wait for the server to signal our reply.
        let resp = self.reply.wait().await;

        Ok(resp)
    }
}

// Delegate for &EmbassyClient so GreeterClient<&'static EmbassyClient<...>> works.
impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    ClientTransport<Req, Resp> for &EmbassyClient<'a, M, Req, Resp, CHANNEL_DEPTH>
{
    type Error = EmbassyLocalError;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        EmbassyClient::call(self, req).await
    }
}

// -- Server --

/// Server handle to an embassy service.
pub struct EmbassyServer<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize> {
    transport: &'a EmbassyService<M, Req, Resp, CHANNEL_DEPTH>,
}

/// Reply token for embassy transport — a reference to the caller's signal.
pub struct EmbassyReplyToken<M: RawMutex + 'static, Resp: 'static> {
    signal: &'static Signal<M, Resp>,
}

impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize> ServerTransport<Req, Resp>
    for EmbassyServer<'a, M, Req, Resp, CHANNEL_DEPTH>
{
    type Error = EmbassyLocalError;
    type ReplyToken = EmbassyReplyToken<M, Resp>;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let (req, signal) = self.transport.requests.receive().await;
        Ok((req, EmbassyReplyToken { signal }))
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        token.signal.signal(resp);
        Ok(())
    }
}
