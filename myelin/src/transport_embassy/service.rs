//! The service endpoint — just the request channel.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

use super::client::EmbassyClient;
use super::server::EmbassyServer;

/// The service endpoint — just the request channel.
///
/// Place this in a `static`. Clients and server borrow from it.
pub struct EmbassyService<M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize> {
    pub(crate) requests: Channel<M, (Req, &'static Signal<M, Resp>), CHANNEL_DEPTH>,
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

    /// Create a client. The returned value must be placed in `'static` storage
    /// (e.g., via [`embassy_client!`](crate::embassy_client)) before use.
    pub fn client(&self) -> EmbassyClient<'_, M, Req, Resp, CHANNEL_DEPTH> {
        EmbassyClient::new(self.requests.sender())
    }

    /// Get a server handle.
    pub fn server(&self) -> EmbassyServer<'_, M, Req, Resp, CHANNEL_DEPTH> {
        EmbassyServer::new(self)
    }
}
