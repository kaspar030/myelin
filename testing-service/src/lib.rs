#![no_std]
#![allow(async_fn_in_trait)]

//! Greeter service definition.
//!
//! This is a hand-written example of what `#[chanapi::service]` would generate.
//! The trait, request/response enums, client struct, and server dispatch loop
//! are all transport-generic.

use chanapi::transport::{ClientTransport, ServerTransport};
use chanapi::CallError;

// Re-exports for macro use.
#[cfg(feature = "embassy")]
#[doc(hidden)]
pub use static_cell;
#[doc(hidden)]
pub use paste;

// ===== The trait (source of truth) =====

// #[chanapi::service]
// pub trait GreeterService {
//     async fn greet(&self, name: String) -> String;
//     async fn health(&self) -> bool;
// }

// ===== What the macro would generate =====

// -- Request / Response enums --

pub enum GreeterRequest {
    Greet { name: heapless::String<64> },
    Health,
}

pub enum GreeterResponse {
    Greet(heapless::String<64>),
    Health(bool),
}

// -- Client struct --

pub struct GreeterClient<T> {
    transport: T,
}

impl<T> GreeterClient<T>
where
    T: ClientTransport<GreeterRequest, GreeterResponse>,
{
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub async fn greet(
        &self,
        name: &str,
    ) -> Result<heapless::String<64>, CallError<T::Error>> {
        let mut s = heapless::String::new();
        // truncate if too long
        let _ = s.push_str(name);
        let resp = self
            .transport
            .call(GreeterRequest::Greet { name: s })
            .await
            .map_err(CallError::Transport)?;

        match resp {
            GreeterResponse::Greet(s) => Ok(s),
            _ => unreachable!(),
        }
    }

    pub async fn health(&self) -> Result<bool, CallError<T::Error>> {
        let resp = self
            .transport
            .call(GreeterRequest::Health)
            .await
            .map_err(CallError::Transport)?;

        match resp {
            GreeterResponse::Health(b) => Ok(b),
            _ => unreachable!(),
        }
    }
}

// -- Embassy convenience types (what the macro would generate) --

#[cfg(feature = "embassy")]
/// Type alias for the embassy service endpoint.
pub type GreeterEmbassyService<M, const CHANNEL_DEPTH: usize> =
    chanapi::transport_embassy::EmbassyService<M, GreeterRequest, GreeterResponse, CHANNEL_DEPTH>;

#[cfg(feature = "embassy")]
/// Type alias for the embassy client transport.
pub type GreeterEmbassyClientTransport<'a, M, const CHANNEL_DEPTH: usize> =
    chanapi::transport_embassy::EmbassyClient<'a, M, GreeterRequest, GreeterResponse, CHANNEL_DEPTH>;

/// Instantiate a Greeter embassy service and generate helper macros.
///
/// Takes a name prefix to allow multiple instances of the same service type.
///
/// Creates:
/// - `<name>_client!()` — creates a `GreeterClient` connected to the service (call once per task)
/// - `<name>_server!()` — creates a server handle for the dispatch loop
///
/// # Example
///
/// ```ignore
/// greeter_embassy_service!(casual, CriticalSectionRawMutex, 2);
/// greeter_embassy_service!(formal, CriticalSectionRawMutex, 2);
///
/// // In client tasks:
/// let casual = casual_client!();
/// let formal = formal_client!();
///
/// // In server tasks:
/// let mut server = casual_server!();
/// greeter_serve(&casual_impl, &mut server).await;
/// ```
#[cfg(feature = "embassy")]
#[macro_export]
macro_rules! greeter_embassy_service {
    ($name:ident, $mutex:ty, $depth:expr) => {
        $crate::paste::paste! {
            static [<__GREETER_SERVICE_ $name:upper>]: $crate::GreeterEmbassyService<$mutex, $depth> =
                $crate::GreeterEmbassyService::new();

            /// Connect a client to this greeter service instance. Call once per task.
            macro_rules! [<$name _client>] {
                () => {{
                    static CELL: $crate::static_cell::StaticCell<
                        $crate::GreeterEmbassyClientTransport<'static, $mutex, $depth>,
                    > = $crate::static_cell::StaticCell::new();
                    $crate::GreeterClient::new(&*CELL.init([<__GREETER_SERVICE_ $name:upper>].client()))
                }};
            }

            /// Get a server handle for this greeter service instance.
            macro_rules! [<$name _server>] {
                () => {
                    [<__GREETER_SERVICE_ $name:upper>].server()
                };
            }
        }
    };
}

// -- Service implementation trait --

pub trait GreeterService {
    async fn greet(&self, name: &str) -> heapless::String<64>;
    async fn health(&self) -> bool;
}

// -- Server dispatch --

/// Run the service loop: receive requests, dispatch to the implementation,
/// send responses.
pub async fn greeter_serve<S, T>(svc: &S, transport: &mut T) -> Result<(), T::Error>
where
    S: GreeterService,
    T: ServerTransport<GreeterRequest, GreeterResponse>,
{
    loop {
        let (req, token) = transport.recv().await?;
        let resp = match req {
            GreeterRequest::Greet { name } => {
                GreeterResponse::Greet(svc.greet(name.as_str()).await)
            }
            GreeterRequest::Health => GreeterResponse::Health(svc.health().await),
        };
        let _ = transport.reply(token, resp).await;
    }
}
