#![no_std]
#![allow(async_fn_in_trait)]

//! Greeter and Math service definitions.
//!
//! These are hand-written examples of what `#[chanapi::service]` would generate.
//! The traits, request/response enums, client structs, and server dispatch
//! are all transport-generic.

use chanapi::transport::{ClientTransport, ServerTransport};
use chanapi::TransportResult;

// =============================================================================
// Wire-level API identifiers
// =============================================================================

/// Wire-level API identifier for the Greeter service.
///
/// Used by the mux layer to route frames to the correct handler when
/// multiple APIs share a single byte stream.
pub const GREETER_API_ID: u16 = 0x0001;

/// Wire-level API identifier for the Math service.
pub const MATH_API_ID: u16 = 0x0002;

// Re-exports for macro use.
#[cfg(feature = "embassy")]
#[doc(hidden)]
pub use static_cell;
#[doc(hidden)]
pub use paste;

// =============================================================================
// Greeter Service
// =============================================================================

// ===== The trait (source of truth) =====

// #[chanapi::service]
// pub trait GreeterService {
//     async fn greet(&self, name: String) -> String;
//     async fn health(&self) -> bool;
// }

// ===== What the macro would generate =====

// -- Request / Response enums --

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GreeterRequest {
    Greet { name: heapless::String<64> },
    Health,
}

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    ) -> <T::Error as TransportResult<heapless::String<64>>>::Output
    where
        T::Error: TransportResult<heapless::String<64>>,
    {
        let mut s = heapless::String::new();
        // truncate if too long
        let _ = s.push_str(name);
        let result = self
            .transport
            .call(GreeterRequest::Greet { name: s })
            .await;

        TransportResult::into_output(result.map(|resp| match resp {
            GreeterResponse::Greet(s) => s,
            _ => unreachable!(),
        }))
    }

    pub async fn health(&self) -> <T::Error as TransportResult<bool>>::Output
    where
        T::Error: TransportResult<bool>,
    {
        let result = self
            .transport
            .call(GreeterRequest::Health)
            .await;

        TransportResult::into_output(result.map(|resp| match resp {
            GreeterResponse::Health(b) => b,
            _ => unreachable!(),
        }))
    }
}

// -- Sync (blocking) client struct --

/// A synchronous wrapper around [`GreeterClient`] for use from threads.
///
/// Wraps each async call with a [`BlockOn`] implementation.
pub struct GreeterClientSync<T, B> {
    inner: GreeterClient<T>,
    block_on: B,
}

impl<T, B> GreeterClientSync<T, B>
where
    T: ClientTransport<GreeterRequest, GreeterResponse>,
    B: chanapi::BlockOn,
{
    pub fn new(inner: GreeterClient<T>, block_on: B) -> Self {
        Self { inner, block_on }
    }

    pub fn greet(
        &self,
        name: &str,
    ) -> <T::Error as TransportResult<heapless::String<64>>>::Output
    where
        T::Error: TransportResult<heapless::String<64>>,
    {
        self.block_on.block_on(self.inner.greet(name))
    }

    pub fn health(&self) -> <T::Error as TransportResult<bool>>::Output
    where
        T::Error: TransportResult<bool>,
    {
        self.block_on.block_on(self.inner.health())
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

// -- Tokio convenience types (what the macro would generate) --

#[cfg(feature = "tokio")]
/// Type alias for the tokio service endpoint.
pub type GreeterTokioService =
    chanapi::transport_tokio::TokioService<GreeterRequest, GreeterResponse>;

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

            /// Connect a sync (blocking) client to this greeter service instance.
            /// Takes a `block_on` function that polls a future to completion.
            /// Call once per thread.
            macro_rules! [<$name _client_sync>] {
                ($block_on:expr) => {{
                    static CELL: $crate::static_cell::StaticCell<
                        $crate::GreeterEmbassyClientTransport<'static, $mutex, $depth>,
                    > = $crate::static_cell::StaticCell::new();
                    $crate::GreeterClientSync::new(
                        $crate::GreeterClient::new(&*CELL.init([<__GREETER_SERVICE_ $name:upper>].client())),
                        $block_on,
                    )
                }};
            }
        }
    };
}

// -- Service implementation traits --

/// Async service trait — implement this for async server tasks.
pub trait GreeterService {
    async fn greet(&self, name: &str) -> heapless::String<64>;
    async fn health(&self) -> bool;
}

/// Sync service trait — implement this for blocking server threads.
pub trait GreeterServiceSync {
    fn greet(&self, name: &str) -> heapless::String<64>;
    fn health(&self) -> bool;
}

// -- Server dispatch --

/// Run the async service loop: receive requests, dispatch to the implementation,
/// send responses.
pub async fn greeter_serve<S, T>(svc: &S, transport: &mut T) -> Result<(), T::Error>
where
    S: GreeterService,
    T: ServerTransport<GreeterRequest, GreeterResponse>,
{
    loop {
        let (req, token) = transport.recv().await?;
        let resp = greeter_dispatch(svc, req).await;
        let _ = transport.reply(token, resp).await;
    }
}

/// Run the sync service loop: uses `BlockOn` for transport operations,
/// calls sync service methods directly.
///
/// Suitable for running a service in a thread.
pub fn greeter_serve_sync<S, T, B>(svc: &S, transport: &mut T, block_on: &B) -> Result<(), T::Error>
where
    S: GreeterServiceSync,
    T: ServerTransport<GreeterRequest, GreeterResponse>,
    B: chanapi::BlockOn,
{
    loop {
        let (req, token) = block_on.block_on(transport.recv())?;
        let resp = greeter_dispatch_sync(svc, req);
        let _ = block_on.block_on(transport.reply(token, resp));
    }
}

/// Dispatch a single greeter request to the async service implementation.
pub async fn greeter_dispatch<S: GreeterService>(svc: &S, req: GreeterRequest) -> GreeterResponse {
    match req {
        GreeterRequest::Greet { name } => {
            GreeterResponse::Greet(svc.greet(name.as_str()).await)
        }
        GreeterRequest::Health => GreeterResponse::Health(svc.health().await),
    }
}

/// Dispatch a single greeter request to the sync service implementation.
pub fn greeter_dispatch_sync<S: GreeterServiceSync>(svc: &S, req: GreeterRequest) -> GreeterResponse {
    match req {
        GreeterRequest::Greet { name } => {
            GreeterResponse::Greet(svc.greet(name.as_str()))
        }
        GreeterRequest::Health => GreeterResponse::Health(svc.health()),
    }
}

// =============================================================================
// Math Service
// =============================================================================

// #[chanapi::service]
// pub trait MathService {
//     async fn add(&self, a: i32, b: i32) -> i64;
//     async fn multiply(&self, a: i32, b: i32) -> i64;
// }

// -- Request / Response enums --

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MathRequest {
    Add { a: i32, b: i32 },
    Multiply { a: i32, b: i32 },
}

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MathResponse {
    Add(i64),
    Multiply(i64),
}

// -- Client struct --

pub struct MathClient<T> {
    transport: T,
}

impl<T> MathClient<T>
where
    T: ClientTransport<MathRequest, MathResponse>,
{
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub async fn add(
        &self,
        a: i32,
        b: i32,
    ) -> <T::Error as TransportResult<i64>>::Output
    where
        T::Error: TransportResult<i64>,
    {
        let result = self
            .transport
            .call(MathRequest::Add { a, b })
            .await;

        TransportResult::into_output(result.map(|resp| match resp {
            MathResponse::Add(v) => v,
            _ => unreachable!(),
        }))
    }

    pub async fn multiply(
        &self,
        a: i32,
        b: i32,
    ) -> <T::Error as TransportResult<i64>>::Output
    where
        T::Error: TransportResult<i64>,
    {
        let result = self
            .transport
            .call(MathRequest::Multiply { a, b })
            .await;

        TransportResult::into_output(result.map(|resp| match resp {
            MathResponse::Multiply(v) => v,
            _ => unreachable!(),
        }))
    }
}

// -- Sync (blocking) client struct --

pub struct MathClientSync<T, B> {
    inner: MathClient<T>,
    block_on: B,
}

impl<T, B> MathClientSync<T, B>
where
    T: ClientTransport<MathRequest, MathResponse>,
    B: chanapi::BlockOn,
{
    pub fn new(inner: MathClient<T>, block_on: B) -> Self {
        Self { inner, block_on }
    }

    pub fn add(
        &self,
        a: i32,
        b: i32,
    ) -> <T::Error as TransportResult<i64>>::Output
    where
        T::Error: TransportResult<i64>,
    {
        self.block_on.block_on(self.inner.add(a, b))
    }

    pub fn multiply(
        &self,
        a: i32,
        b: i32,
    ) -> <T::Error as TransportResult<i64>>::Output
    where
        T::Error: TransportResult<i64>,
    {
        self.block_on.block_on(self.inner.multiply(a, b))
    }
}

// -- Embassy convenience types --

#[cfg(feature = "embassy")]
pub type MathEmbassyService<M, const CHANNEL_DEPTH: usize> =
    chanapi::transport_embassy::EmbassyService<M, MathRequest, MathResponse, CHANNEL_DEPTH>;

#[cfg(feature = "embassy")]
pub type MathEmbassyClientTransport<'a, M, const CHANNEL_DEPTH: usize> =
    chanapi::transport_embassy::EmbassyClient<'a, M, MathRequest, MathResponse, CHANNEL_DEPTH>;

// -- Tokio convenience types --

#[cfg(feature = "tokio")]
pub type MathTokioService =
    chanapi::transport_tokio::TokioService<MathRequest, MathResponse>;

#[cfg(feature = "embassy")]
#[macro_export]
macro_rules! math_embassy_service {
    ($name:ident, $mutex:ty, $depth:expr) => {
        $crate::paste::paste! {
            static [<__MATH_SERVICE_ $name:upper>]: $crate::MathEmbassyService<$mutex, $depth> =
                $crate::MathEmbassyService::new();

            macro_rules! [<$name _client>] {
                () => {{
                    static CELL: $crate::static_cell::StaticCell<
                        $crate::MathEmbassyClientTransport<'static, $mutex, $depth>,
                    > = $crate::static_cell::StaticCell::new();
                    $crate::MathClient::new(&*CELL.init([<__MATH_SERVICE_ $name:upper>].client()))
                }};
            }

            macro_rules! [<$name _server>] {
                () => {
                    [<__MATH_SERVICE_ $name:upper>].server()
                };
            }

            macro_rules! [<$name _client_sync>] {
                ($block_on:expr) => {{
                    static CELL: $crate::static_cell::StaticCell<
                        $crate::MathEmbassyClientTransport<'static, $mutex, $depth>,
                    > = $crate::static_cell::StaticCell::new();
                    $crate::MathClientSync::new(
                        $crate::MathClient::new(&*CELL.init([<__MATH_SERVICE_ $name:upper>].client())),
                        $block_on,
                    )
                }};
            }
        }
    };
}

// -- Service implementation traits --

/// Async service trait for math operations.
pub trait MathService {
    async fn add(&self, a: i32, b: i32) -> i64;
    async fn multiply(&self, a: i32, b: i32) -> i64;
}

/// Sync service trait for math operations.
pub trait MathServiceSync {
    fn add(&self, a: i32, b: i32) -> i64;
    fn multiply(&self, a: i32, b: i32) -> i64;
}

// -- Server dispatch --

/// Run the async math service loop.
pub async fn math_serve<S, T>(svc: &S, transport: &mut T) -> Result<(), T::Error>
where
    S: MathService,
    T: ServerTransport<MathRequest, MathResponse>,
{
    loop {
        let (req, token) = transport.recv().await?;
        let resp = math_dispatch(svc, req).await;
        let _ = transport.reply(token, resp).await;
    }
}

/// Run the sync math service loop.
pub fn math_serve_sync<S, T, B>(svc: &S, transport: &mut T, block_on: &B) -> Result<(), T::Error>
where
    S: MathServiceSync,
    T: ServerTransport<MathRequest, MathResponse>,
    B: chanapi::BlockOn,
{
    loop {
        let (req, token) = block_on.block_on(transport.recv())?;
        let resp = math_dispatch_sync(svc, req);
        let _ = block_on.block_on(transport.reply(token, resp));
    }
}

/// Dispatch a single math request to the async service implementation.
pub async fn math_dispatch<S: MathService>(svc: &S, req: MathRequest) -> MathResponse {
    match req {
        MathRequest::Add { a, b } => MathResponse::Add(svc.add(a, b).await),
        MathRequest::Multiply { a, b } => MathResponse::Multiply(svc.multiply(a, b).await),
    }
}

/// Dispatch a single math request to the sync service implementation.
pub fn math_dispatch_sync<S: MathServiceSync>(svc: &S, req: MathRequest) -> MathResponse {
    match req {
        MathRequest::Add { a, b } => MathResponse::Add(svc.add(a, b)),
        MathRequest::Multiply { a, b } => MathResponse::Multiply(svc.multiply(a, b)),
    }
}

// =============================================================================
// Composed Service: Greeter + Math
// =============================================================================

chanapi::compose_service!(
    Combined,
    [Greeter, greeter_dispatch, greeter_dispatch_sync],
    [Math, math_dispatch, math_dispatch_sync],
);
