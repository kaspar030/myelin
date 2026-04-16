//! The core transport traits.
//!
//! Split into [`ClientTransport`] and [`ServerTransport`] because the client
//! and server sides of the reply mechanism are fundamentally different:
//!
//! - Client: creates a reply slot, sends request + reply token, awaits response.
//! - Server: receives request + reply token, sends response through it.

use core::future::Future;

/// Client side of a service transport.
///
/// The generated client struct holds an impl of this and delegates each
/// method call through it.
///
/// # Cancel Safety
///
/// Implementations must ensure that dropping the future returned by [`call`](Self::call)
/// at any `.await` point does not corrupt the transport or leak state. The server
/// may do wasted work if a request was already sent, but the client must be
/// reusable immediately after cancellation.
pub trait ClientTransport<Req, Resp> {
    /// Transport-specific error.
    type Error;

    /// Make a request and await the response.
    ///
    /// This bundles the full lifecycle: acquire reply slot → send request →
    /// await reply. Bundling it lets the transport optimize (e.g., tokio can
    /// create the oneshot and send in one step without exposing the token).
    ///
    /// # Cancel Safety
    ///
    /// Must be cancel-safe. See the trait-level documentation.
    fn call(&self, req: Req) -> impl Future<Output = Result<Resp, Self::Error>>;
}

// Blanket impl: &T is a ClientTransport if T is.
impl<T, Req, Resp> ClientTransport<Req, Resp> for &T
where
    T: ClientTransport<Req, Resp>,
{
    type Error = T::Error;

    fn call(&self, req: Req) -> impl Future<Output = Result<Resp, Self::Error>> {
        (**self).call(req)
    }
}

/// Server side of a service transport.
///
/// The generated server dispatch loop uses this to receive requests and
/// send responses.
pub trait ServerTransport<Req, Resp> {
    /// Transport-specific error.
    type Error;

    /// An opaque token the server uses to reply to a specific request.
    type ReplyToken;

    /// Receive the next request and its reply token.
    fn recv(&mut self) -> impl Future<Output = Result<(Req, Self::ReplyToken), Self::Error>>;

    /// Send a response back to the caller.
    fn reply(
        &self,
        token: Self::ReplyToken,
        resp: Resp,
    ) -> impl Future<Output = Result<(), Self::Error>>;
}
