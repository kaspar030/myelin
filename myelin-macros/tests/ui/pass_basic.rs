//! `#[myelin::service]` on a mixed async+sync trait with an explicit `api_id`.
#![allow(async_fn_in_trait, unexpected_cfgs)]

#[myelin::service(api_id = 0x1234)]
pub trait GreeterService {
    async fn greet(&self, name: String) -> String;
    fn health(&self) -> bool;
}

// Proves the attribute override is honoured.
const _: () = assert!(GREETER_API_ID == 0x1234);

fn main() {}
