//! Embassy server — receives requests, dispatches replies through caller's Signal.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::signal::Signal;

use super::service::EmbassyService;
use super::EmbassyLocalError;
use crate::transport::ServerTransport;

/// Server handle to an embassy service.
pub struct EmbassyServer<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
{
    transport: &'a EmbassyService<M, Req, Resp, CHANNEL_DEPTH>,
}

impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    EmbassyServer<'a, M, Req, Resp, CHANNEL_DEPTH>
{
    pub(crate) fn new(transport: &'a EmbassyService<M, Req, Resp, CHANNEL_DEPTH>) -> Self {
        Self { transport }
    }
}

/// Reply token — a reference to the caller's signal.
pub struct EmbassyReplyToken<M: RawMutex + 'static, Resp: 'static> {
    signal: &'static Signal<M, Resp>,
}

impl<'a, M: RawMutex + 'static, Req, Resp: 'static, const CHANNEL_DEPTH: usize>
    ServerTransport<Req, Resp> for EmbassyServer<'a, M, Req, Resp, CHANNEL_DEPTH>
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
