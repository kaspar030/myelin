//! Smoke test that `#[chanapi::service]` expands to code that compiles.
//!
//! Exercises:
//! - `{Stem}Request` / `{Stem}Response` enums (struct, unit, multi-arg).
//! - Async + sync trait variants.
//! - `{Stem}Client` / `{Stem}ClientSync`.
//! - `{stem}_dispatch` (mixed async/sync trait methods, including the
//!   "no `.await` for sync method" detail).
//! - `{stem}_dispatch_sync`.
//!
//! Full behaviour (compile-fail UI tests, real transports, etc.) is exercised
//! in later subtasks and in `testing-service`.

#![allow(async_fn_in_trait)]
// `chanapi` has no `serde` feature of its own; the macro emits
// `cfg_attr(feature = "serde", ...)` for downstream consumers. Silence the
// resulting cfg-check warning inside this in-crate test.
#![allow(unexpected_cfgs)]

use chanapi::transport::{ClientTransport, ServerTransport};
use chanapi::BlockOn;
use core::convert::Infallible;

#[chanapi::service]
pub trait GreeterService {
    async fn greet(&self, name: String) -> String;
    async fn health(&self) -> bool;
    fn reset(&self);
}

#[chanapi::service]
pub trait MathService {
    async fn add(&self, a: i32, b: i32) -> i64;
}

#[test]
fn generated_enums_exist() {
    let _ = GreeterRequest::Greet {
        name: String::from("world"),
    };
    let _ = GreeterRequest::Health;
    let _ = GreeterRequest::Reset;

    let _ = GreeterResponse::Greet(String::from("hi"));
    let _ = GreeterResponse::Health(true);
    let _ = GreeterResponse::Reset;

    let _ = MathRequest::Add { a: 1, b: 2 };
    let _ = MathResponse::Add(3);
}

struct MyAsync;
impl GreeterService for MyAsync {
    async fn greet(&self, name: String) -> String {
        name
    }
    async fn health(&self) -> bool {
        true
    }
    fn reset(&self) {}
}

struct MySync;
impl GreeterServiceSync for MySync {
    fn greet(&self, name: String) -> String {
        name
    }
    fn health(&self) -> bool {
        true
    }
    fn reset(&self) {}
}

#[test]
fn traits_are_implementable() {
    fn assert_async<T: GreeterService>() {}
    fn assert_sync<T: GreeterServiceSync>() {}
    assert_async::<MyAsync>();
    assert_sync::<MySync>();
}

// =============================================================================
// Dispatch — verify generated functions are callable and route correctly.
// =============================================================================

#[tokio::test]
async fn dispatch_routes_each_variant() {
    let svc = MyAsync;

    match greeter_dispatch(&svc, GreeterRequest::Greet { name: "x".into() }).await {
        GreeterResponse::Greet(s) => assert_eq!(s, "x"),
        _ => panic!("wrong variant"),
    }
    match greeter_dispatch(&svc, GreeterRequest::Health).await {
        GreeterResponse::Health(b) => assert!(b),
        _ => panic!("wrong variant"),
    }
    match greeter_dispatch(&svc, GreeterRequest::Reset).await {
        GreeterResponse::Reset => {}
        _ => panic!("wrong variant"),
    }
}

#[test]
fn dispatch_sync_routes_each_variant() {
    let svc = MySync;

    match greeter_dispatch_sync(&svc, GreeterRequest::Greet { name: "x".into() }) {
        GreeterResponse::Greet(s) => assert_eq!(s, "x"),
        _ => panic!("wrong variant"),
    }
    match greeter_dispatch_sync(&svc, GreeterRequest::Health) {
        GreeterResponse::Health(b) => assert!(b),
        _ => panic!("wrong variant"),
    }
    match greeter_dispatch_sync(&svc, GreeterRequest::Reset) {
        GreeterResponse::Reset => {}
        _ => panic!("wrong variant"),
    }
}

// =============================================================================
// Client — verify the generated client struct compiles and round-trips
// requests through a fake infallible in-memory transport.
// =============================================================================

/// Trivial single-shot transport: the next response is hard-coded by the test.
struct FakeClientTransport {
    next_response: std::sync::Mutex<Option<GreeterResponse>>,
}

impl FakeClientTransport {
    fn new(resp: GreeterResponse) -> Self {
        Self {
            next_response: std::sync::Mutex::new(Some(resp)),
        }
    }
}

impl ClientTransport<GreeterRequest, GreeterResponse> for FakeClientTransport {
    type Error = Infallible;

    async fn call(
        &self,
        _req: GreeterRequest,
    ) -> Result<GreeterResponse, Self::Error> {
        Ok(self.next_response.lock().unwrap().take().unwrap())
    }
}

#[tokio::test]
async fn client_round_trip_with_value_return() {
    let t = FakeClientTransport::new(GreeterResponse::Greet("hi".into()));
    let client = GreeterClient::new(t);
    // `Infallible: TransportResult<String>` ⇒ Output = String (no Result wrapper).
    let s: String = client.greet("ignored".into()).await;
    assert_eq!(s, "hi");
}

#[tokio::test]
async fn client_round_trip_with_unit_return() {
    let t = FakeClientTransport::new(GreeterResponse::Reset);
    let client = GreeterClient::new(t);
    // `Infallible: TransportResult<()>` ⇒ Output = ().
    let (): () = client.reset().await;
}

// =============================================================================
// ClientSync — verify it composes over a Client + a `BlockOn`.
// =============================================================================

struct TokioBlockOn;
impl BlockOn for TokioBlockOn {
    fn block_on<F: core::future::Future>(&self, fut: F) -> F::Output {
        // The chanapi BlockOn trait deliberately does not require Send; we use
        // a current-thread runtime to evaluate the future synchronously.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(fut)
    }
}

#[test]
fn client_sync_calls_through_block_on() {
    let t = FakeClientTransport::new(GreeterResponse::Health(true));
    let client = GreeterClient::new(t);
    let sync_client = GreeterClientSync::new(client, TokioBlockOn);
    let b: bool = sync_client.health();
    assert!(b);
}

// =============================================================================
// Serve — verify `{stem}_serve` and `{stem}_serve_sync` type-check; we don't
// drive the loops to completion (they're infinite by design).
// =============================================================================

/// A minimally-typed server transport whose recv never returns; we only need
/// the type to satisfy the bound at compile time.
struct NeverServer;

impl ServerTransport<GreeterRequest, GreeterResponse> for NeverServer {
    type Error = Infallible;
    type ReplyToken = ();

    async fn recv(&mut self) -> Result<(GreeterRequest, Self::ReplyToken), Self::Error> {
        // Park forever — we never invoke this in the test.
        std::future::pending().await
    }

    async fn reply(
        &self,
        _token: Self::ReplyToken,
        _resp: GreeterResponse,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[allow(dead_code)]
fn serve_signatures_compile() {
    // We only need to prove the call type-checks; we never await it.
    let _f1 = async {
        let svc = MyAsync;
        let mut t = NeverServer;
        let _: Result<(), Infallible> = greeter_serve(&svc, &mut t).await;
    };

    let _f2 = || {
        let svc = MySync;
        let mut t = NeverServer;
        let _: Result<(), Infallible> = greeter_serve_sync(&svc, &mut t, &TokioBlockOn);
    };
}

// =============================================================================
// Transport convenience type aliases (item 7).
//
// Under `feature = "tokio"` the generated `GreeterTokioService` must be a
// valid alias usable to construct an in-process transport.
// =============================================================================

#[cfg(feature = "tokio")]
#[tokio::test]
async fn tokio_service_alias_is_usable() {
    // Emitted by the macro.
    let mut svc: GreeterTokioService = GreeterTokioService::new(4);
    let client = svc.client();
    let mut server = svc.server();

    // Drive one round-trip to prove the alias is wired to the real
    // TokioService<Req, Resp> type.
    let svc_impl = MyAsync;
    let client_task = tokio::spawn(async move {
        let c = GreeterClient::new(client);
        c.health().await
    });

    // Server side — dispatch exactly one request then return.
    let (req, token) = server.recv().await.unwrap();
    let resp = greeter_dispatch(&svc_impl, req).await;
    server.reply(token, resp).await.unwrap();

    let b: bool = client_task.await.unwrap().unwrap();
    assert!(b);
}
