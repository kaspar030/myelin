#![no_std]
#![allow(async_fn_in_trait)]

//! Greeter and Math service definitions.
//!
//! The traits are the single source of truth — [`#[myelin::service]`](myelin::service)
//! generates the channel plumbing (request/response enums, clients, dispatch,
//! serve loops, transport aliases, embassy instantiation macros, and the
//! wire-level `*_API_ID` constants).

// `String` appears verbatim in the trait signatures, so the macro-emitted
// code references it as an unqualified name. We pull it in via `alloc`
// (the crate is `#![no_std]`).
extern crate alloc;
use alloc::string::String;

#[myelin::service(api_id = 0x0001)]
pub trait GreeterService {
    async fn greet(&self, name: String) -> String;
    fn health(&self) -> bool; // sync — exercises mixed-mode dispatch codegen
}

#[myelin::service(api_id = 0x0002)]
pub trait MathService {
    async fn add(&self, a: i32, b: i32) -> i64;
    async fn multiply(&self, a: i32, b: i32) -> i64;
}

myelin::compose_service!(
    Combined,
    [Greeter, greeter_dispatch, greeter_dispatch_sync],
    [Math, math_dispatch, math_dispatch_sync],
);
