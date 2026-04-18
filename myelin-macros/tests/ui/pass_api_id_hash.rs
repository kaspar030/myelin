//! `#[myelin::service]` without `api_id` — the constant is derived from the
//! FNV-1a hash of the trait ident. We only assert it's non-zero; the exact
//! value is regression-pinned in the `myelin-macros` unit tests.
#![allow(async_fn_in_trait, unexpected_cfgs)]

#[myelin::service]
pub trait GreeterService {
    async fn greet(&self, name: String) -> String;
    fn health(&self) -> bool;
}

const _: () = assert!(GREETER_API_ID != 0);

fn main() {}
