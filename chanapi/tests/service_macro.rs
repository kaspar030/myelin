//! Smoke test that `#[chanapi::service]` expands to code that compiles.
//!
//! This is intentionally minimal — full behaviour (compile-fail UI tests,
//! real transports, etc.) is exercised in later subtasks and in
//! `testing-service`.

#![allow(async_fn_in_trait)]
// `chanapi` has no `serde` feature of its own; the macro emits
// `cfg_attr(feature = "serde", ...)` for downstream consumers. Silence the
// resulting cfg-check warning inside this in-crate test.
#![allow(unexpected_cfgs)]

#[chanapi::service]
pub trait GreeterService {
    async fn greet(&self, name: String) -> String;
    async fn health(&self) -> bool;
    fn reset(&self);
}

#[test]
fn generated_enums_exist() {
    // Construct both enums to prove the macro emitted them with the
    // expected variant shapes.
    let _ = GreeterRequest::Greet {
        name: String::from("world"),
    };
    let _ = GreeterRequest::Health;
    let _ = GreeterRequest::Reset;

    let _ = GreeterResponse::Greet(String::from("hi"));
    let _ = GreeterResponse::Health(true);
    let _ = GreeterResponse::Reset;
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
