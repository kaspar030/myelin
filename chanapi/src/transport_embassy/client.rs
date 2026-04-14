//! Embassy client — owns a reply Signal, sends requests through a Sender.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Sender;
use embassy_sync::signal::Signal;

use super::EmbassyLocalError;
use crate::transport::ClientTransport;

/// Client handle to an embassy service.
///
/// Each client owns its own reply `Signal`. Must live at a `'static` address
/// (e.g., via [`embassy_client!`](crate::embassy_client)) because a reference
/// to the signal is sent through the channel with each request.
pub struct EmbassyClient<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
{
    sender: Sender<'a, M, (Req, &'static Signal<M, Resp>), CHANNEL_DEPTH>,
    reply: Signal<M, Resp>,
}

impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    EmbassyClient<'a, M, Req, Resp, CHANNEL_DEPTH>
{
    pub(crate) fn new(
        sender: Sender<'a, M, (Req, &'static Signal<M, Resp>), CHANNEL_DEPTH>,
    ) -> Self {
        Self {
            sender,
            reply: Signal::new(),
        }
    }
}

impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    ClientTransport<Req, Resp> for EmbassyClient<'a, M, Req, Resp, CHANNEL_DEPTH>
{
    type Error = EmbassyLocalError;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        self.reply.reset();

        // SAFETY: The caller guarantees this client lives in `'static` storage
        // (e.g., StaticCell). We block on wait() below, so the client cannot
        // be dropped while in-flight.
        let signal_ref: &'static Signal<M, Resp> =
            unsafe { &*core::ptr::from_ref(&self.reply) };

        self.sender.send((req, signal_ref)).await;
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
