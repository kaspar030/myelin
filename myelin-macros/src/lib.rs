//! Proc macros for the [`myelin`](https://docs.rs/myelin) crate.
//!
//! This crate is an implementation detail: always invoke its macros via the
//! `myelin` re-exports (e.g. `#[myelin::service]`). See the
//! [`service`] attribute below for the full input/output contract.
//!
//! # Worked example
//!
//! ```ignore
//! use myelin::service;
//!
//! #[service(api_id = 0x0001)]
//! pub trait GreeterService {
//!     async fn greet(&self, name: String) -> String;
//!     fn health(&self) -> bool;   // sync — dispatched without `.await`
//! }
//! ```
//!
//! expands (roughly) to:
//!
//! - `GreeterRequest` / `GreeterResponse` enums (serde-gated `#[derive]`s)
//! - `GreeterService` (async, preserved verbatim) + `GreeterServiceSync`
//!   (sync mirror — `async` stripped from every method)
//! - `GreeterClient<T>` + `GreeterClientSync<T, B>` with one client method
//!   per trait method
//! - `greeter_dispatch` / `greeter_dispatch_sync` and
//!   `greeter_serve` / `greeter_serve_sync`
//! - `GreeterTokioService` / `GreeterEmbassyService<M, N>` /
//!   `GreeterEmbassyClientTransport<'a, M, N>` type aliases
//!   (feature-gated in the consuming crate)
//! - `greeter_embassy_service!` `macro_rules!` (embassy-gated) for static
//!   instantiation, with nested `*_client!` / `*_server!` / `*_client_sync!`
//!   helpers
//! - `GREETER_API_ID: u16 = 0x0001` (or an FNV-1a hash of the trait ident
//!   when the `api_id` argument is omitted)
//!
//! Multiple services can be multiplexed onto a single transport with
//! [`myelin::compose_service!`].
//!
//! Methods may be either `async fn` or plain `fn`; in the generated async
//! dispatch, sync methods are called directly (no spurious `.await`).
//! Cross-service clients always expose `async` method stubs regardless of
//! whether the underlying trait method was sync, because the transport is
//! always async.

mod emit;
mod parse;

use proc_macro::TokenStream;
use quote::quote;

/// Generate channel-API plumbing from a trait definition.
///
/// Applied to an async-style trait, emits:
///
/// 1. `{Stem}Request` enum — one struct-like variant per method, whose fields
///    are the method arguments by name and type.
/// 2. `{Stem}Response` enum — one tuple variant per method wrapping the
///    method's return type (unit returns produce a unit variant).
/// 3. The original trait, preserved verbatim.
/// 4. `{Stem}ServiceSync` — a sync mirror of the trait with every method's
///    `async` stripped.
/// 5. `{Stem}Client<T>` — async client struct with one `async fn` per trait
///    method that calls `T::call(...).await` and unwraps the matching
///    response variant.
/// 6. `{Stem}ClientSync<T, B>` — sync wrapper around `{Stem}Client<T>` that
///    blocks each async call via `B: BlockOn`.
/// 7. `{stem}_dispatch` / `{stem}_dispatch_sync` — match on a request and
///    invoke the corresponding service trait method, returning a response.
/// 8. `{stem}_serve` / `{stem}_serve_sync` — `recv → dispatch → reply` loops
///    over a `ServerTransport`.
/// 9. Transport convenience type aliases, emitted unconditionally in the
///    output (no `#[cfg]` attributes). Whether they appear at all is
///    decided at proc-macro build time by `myelin-macros`'s own
///    `tokio`/`embassy` features — `myelin` forwards its own
///    `tokio`/`embassy` features to these, so Cargo's per-package feature
///    unification keeps the emission and the supporting transport modules
///    in `myelin` in lockstep.
///    - `{Stem}TokioService` — `TokioService<{Stem}Request, {Stem}Response>`
///    - `{Stem}EmbassyService<M, const CHANNEL_DEPTH: usize>`
///    - `{Stem}EmbassyClientTransport<'a, M, const CHANNEL_DEPTH: usize>`
/// 10. `{stem}_embassy_service!` — a `#[macro_export] macro_rules!` that
///     instantiates a static embassy service plus three nested
///     `*_client!` / `*_server!` / `*_client_sync!` helper macros. Takes
///     `($name:ident, $mutex:ty, $depth:expr)`. Like item 9, this is
///     emitted unconditionally in the output when the `embassy` feature of
///     `myelin-macros` itself is on (i.e. when `myelin/embassy` is on).
/// 11. `{STEM_UPPER}_API_ID: u16` — wire-level API identifier. Defaults to a
///     16-bit FNV-1a hash of the trait ident; override with
///     `#[myelin::service(api_id = 0x1234)]`. Since the id space is only
///     2^16 wide, collisions are possible across unrelated services — for
///     production deployments override explicitly.
///
/// The "stem" is derived by stripping a trailing `Service` from the trait
/// name (e.g. `GreeterService` → `Greeter`). The trait name must end in
/// `Service`; anything else is a compile error.
///
/// Both enums derive `Debug`, and derive `serde::Serialize` and
/// `serde::Deserialize` when the downstream crate's `serde` feature is
/// enabled.
///
/// # Transport feature gating
///
/// Items 9 and 10 carry **no** `#[cfg]` attributes in the emitted tokens.
/// Their presence is controlled entirely at proc-macro build time via
/// `myelin-macros`'s own `tokio`/`embassy` features. In the supported
/// usage pattern (depend on `myelin`, invoke `#[myelin::service]` via the
/// re-export), enabling `myelin/tokio` or `myelin/embassy` automatically
/// flips the matching `myelin-macros` feature through Cargo's feature
/// forwards, so the emitted aliases / `macro_rules!` appear exactly when
/// the supporting `::myelin::transport_tokio` / `::myelin::transport_embassy`
/// modules are available. Downstream crates therefore do **not** need to
/// declare `tokio` / `embassy` features of their own.
///
/// The embassy instantiation `macro_rules!` refers to `::myelin::paste` and
/// `::myelin::static_cell` by absolute path. `myelin` re-exports `paste`
/// unconditionally and `static_cell` under its own `embassy` feature, so no
/// additional consumer-side wiring is required.
///
/// # Input constraints
///
/// - Method receiver must be plain `&self`.
/// - No trait generics, lifetimes, supertraits, or where-clauses.
/// - Arg patterns must be `ident: Type`.
/// - Argument types must be owned (no `&T`/`&mut T`).
/// - Return types are passed through verbatim.
///
/// # Attribute arguments
///
/// Currently recognised:
///
/// - `api_id = <int literal>` — override the wire-level API id constant.
///   Must fit in a `u16`. Accepts hex (`0x1234`), decimal, or underscored
///   literals. When omitted, a 16-bit FNV-1a hash of the trait ident is
///   used.
///
/// Any other key is a hard error; this is intentional so future knobs can
/// be added without silently accepting typos.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match parse::ServiceArgs::parse(attr.into()) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    let item_trait = match syn::parse::<syn::ItemTrait>(item) {
        Ok(t) => t,
        Err(e) => return e.to_compile_error().into(),
    };

    match parse::ServiceTrait::parse(item_trait) {
        Ok(svc) => {
            let req = emit::request_enum(&svc);
            let resp = emit::response_enum(&svc);
            let async_t = emit::async_trait(&svc);
            let sync_t = emit::sync_trait(&svc);
            let client = emit::client_struct(&svc);
            let client_sync = emit::client_sync_struct(&svc);
            let dispatch = emit::dispatch_fn(&svc);
            let dispatch_sync = emit::dispatch_sync_fn(&svc);
            let serve = emit::serve_fn(&svc);
            let serve_sync = emit::serve_sync_fn(&svc);

            // Transport aliases / instantiation macros are gated on
            // `myelin-macros`'s own `tokio`/`embassy` features so the
            // emitted tokens don't carry `#[cfg]` attributes (see the
            // docstring above). When the feature is off, substitute an
            // empty `TokenStream`.
            #[cfg(feature = "tokio")]
            let tokio_alias = emit::tokio_transport_alias(&svc);
            #[cfg(not(feature = "tokio"))]
            let tokio_alias = quote! {};

            #[cfg(feature = "embassy")]
            let embassy_aliases = emit::embassy_transport_aliases(&svc);
            #[cfg(not(feature = "embassy"))]
            let embassy_aliases = quote! {};

            #[cfg(feature = "embassy")]
            let embassy_instantiation = emit::embassy_instantiation(&svc);
            #[cfg(not(feature = "embassy"))]
            let embassy_instantiation = quote! {};

            let api_id = emit::api_id_const(&svc, args.api_id);
            quote! {
                #req
                #resp
                #async_t
                #sync_t
                #client
                #client_sync
                #dispatch
                #dispatch_sync
                #serve
                #serve_sync
                #tokio_alias
                #embassy_aliases
                #embassy_instantiation
                #api_id
            }
            .into()
        }
        Err(e) => e.to_compile_error().into(),
    }
}

// =============================================================================
// Unit tests — exercise the parse+emit pipeline directly, since proc macros
// cannot be invoked from within their own crate. UI (`trybuild`) tests live
// downstream in subtask 5.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::{
        emit,
        parse::{ServiceArgs, ServiceTrait},
    };
    use quote::quote;

    fn canon(ts: proc_macro2::TokenStream) -> String {
        ts.to_string()
    }

    // ----- happy path -----

    #[test]
    fn request_enum_shape() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
                async fn health(&self) -> bool;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::request_enum(&svc));
        assert!(got.contains("enum GreeterRequest"), "got: {got}");
        assert!(got.contains("Greet { name : String }"), "got: {got}");
        // Zero-arg method → unit variant (no braces).
        assert!(got.contains("Health"), "got: {got}");
        assert!(!got.contains("Health {"), "got: {got}");
        // Derives present.
        assert!(got.contains("derive (Debug)"), "got: {got}");
        assert!(
            got.contains("serde :: Serialize , serde :: Deserialize"),
            "got: {got}"
        );
    }

    #[test]
    fn response_enum_shape() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
                async fn health(&self) -> bool;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::response_enum(&svc));
        assert!(got.contains("enum GreeterResponse"), "got: {got}");
        assert!(got.contains("Greet (String)"), "got: {got}");
        assert!(got.contains("Health (bool)"), "got: {got}");
    }

    #[test]
    fn response_unit_variant_for_unit_return() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait PingService {
                fn ping(&self);
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::response_enum(&svc));
        assert!(got.contains("enum PingResponse"), "got: {got}");
        assert!(got.contains("Ping"), "got: {got}");
        // No tuple payload for unit return.
        assert!(!got.contains("Ping ("), "got: {got}");
    }

    #[test]
    fn async_trait_is_preserved_verbatim() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
                fn health(&self) -> bool;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::async_trait(&svc));
        assert!(got.contains("trait GreeterService"), "got: {got}");
        assert!(got.contains("async fn greet"), "got: {got}");
        assert!(got.contains("fn health"), "got: {got}");
    }

    #[test]
    fn sync_trait_strips_async_and_renames() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
                fn health(&self) -> bool;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::sync_trait(&svc));
        assert!(got.contains("trait GreeterServiceSync"), "got: {got}");
        assert!(!got.contains("async fn"), "got: {got}");
        assert!(got.contains("fn greet"), "got: {got}");
        assert!(got.contains("fn health"), "got: {got}");
    }

    #[test]
    fn snake_case_method_to_pascal_variant() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait FooService {
                async fn do_thing(&self, n: u32) -> u32;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::request_enum(&svc));
        assert!(got.contains("DoThing { n : u32 }"), "got: {got}");
    }

    #[test]
    fn result_return_type_passed_through() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait FooService {
                async fn maybe(&self) -> Result<String, u32>;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::response_enum(&svc));
        assert!(
            got.contains("Maybe (Result < String , u32 >)"),
            "got: {got}"
        );
    }

    #[test]
    fn preserves_complex_arg_type_verbatim() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait FooService {
                async fn put(&self, s: heapless::String<64>) -> bool;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::request_enum(&svc));
        assert!(
            got.contains("Put { s : heapless :: String < 64 > }"),
            "got: {got}"
        );
    }

    #[test]
    fn visibility_propagates_to_generated_items() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub(crate) trait FooService {
                fn a(&self) -> u32;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        assert!(
            canon(emit::request_enum(&svc)).starts_with("# [derive"),
            "expected attrs before vis"
        );
        assert!(canon(emit::request_enum(&svc)).contains("pub (crate) enum FooRequest"));
        assert!(canon(emit::response_enum(&svc)).contains("pub (crate) enum FooResponse"));
        assert!(canon(emit::sync_trait(&svc)).contains("pub (crate) trait FooServiceSync"));
    }

    // ----- error paths -----

    fn err(item: syn::ItemTrait) -> String {
        match ServiceTrait::parse(item) {
            Ok(_) => panic!("expected parse error"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn rejects_non_service_trait_name() {
        let msg = err(syn::parse_quote! {
            pub trait Greeter {
                fn a(&self);
            }
        });
        assert!(msg.contains("must end in `Service`"), "msg: {msg}");
    }

    #[test]
    fn rejects_empty_stem() {
        let msg = err(syn::parse_quote! {
            pub trait Service {
                fn a(&self);
            }
        });
        assert!(msg.contains("non-empty stem"), "msg: {msg}");
    }

    #[test]
    fn rejects_mut_self() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(&mut self);
            }
        });
        assert!(msg.contains("&self"), "msg: {msg}");
    }

    #[test]
    fn rejects_self_by_value() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(self);
            }
        });
        assert!(msg.contains("&self"), "msg: {msg}");
    }

    #[test]
    fn rejects_missing_receiver() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(x: u32);
            }
        });
        assert!(msg.contains("&self"), "msg: {msg}");
    }

    #[test]
    fn rejects_borrowed_arg() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(&self, name: &str);
            }
        });
        assert!(msg.contains("owned"), "msg: {msg}");
    }

    #[test]
    fn rejects_destructured_arg() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(&self, (x, y): (u32, u32));
            }
        });
        assert!(msg.contains("plain `ident: Type`"), "msg: {msg}");
    }

    #[test]
    fn rejects_underscore_arg() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(&self, _: u32);
            }
        });
        assert!(msg.contains("plain `ident: Type`"), "msg: {msg}");
    }

    #[test]
    fn rejects_mut_arg() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(&self, mut x: u32);
            }
        });
        assert!(msg.contains("plain `ident: Type`"), "msg: {msg}");
    }

    #[test]
    fn rejects_trait_generics() {
        let msg = err(syn::parse_quote! {
            pub trait FooService<T> {
                fn a(&self) -> T;
            }
        });
        assert!(msg.contains("trait generics"), "msg: {msg}");
    }

    #[test]
    fn rejects_trait_lifetime() {
        let msg = err(syn::parse_quote! {
            pub trait FooService<'a> {
                fn a(&self);
            }
        });
        // Lifetimes are generic params → caught by trait generics rule.
        assert!(msg.contains("trait generics"), "msg: {msg}");
    }

    #[test]
    fn rejects_where_clause() {
        let msg = err(syn::parse_quote! {
            pub trait FooService where Self: Sized {
                fn a(&self);
            }
        });
        assert!(msg.contains("where-clauses"), "msg: {msg}");
    }

    #[test]
    fn rejects_supertraits() {
        let msg = err(syn::parse_quote! {
            pub trait FooService: Sized {
                fn a(&self);
            }
        });
        assert!(msg.contains("supertraits"), "msg: {msg}");
    }

    #[test]
    fn rejects_method_generics() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a<T>(&self, t: T);
            }
        });
        assert!(msg.contains("method generics"), "msg: {msg}");
    }

    #[test]
    fn rejects_default_body() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                fn a(&self) -> u32 { 0 }
            }
        });
        assert!(msg.contains("default body"), "msg: {msg}");
    }

    #[test]
    fn rejects_non_fn_trait_items() {
        let msg = err(syn::parse_quote! {
            pub trait FooService {
                type Assoc;
                fn a(&self);
            }
        });
        assert!(msg.contains("only `fn` items"), "msg: {msg}");
    }

    // =========================================================================
    // Emit tests for client / client-sync / dispatch / serve.
    // We assert shape via substring matching against the canonical
    // (whitespace-padded) `TokenStream::to_string()` output. Compile-level
    // verification happens downstream in subtask 5's trybuild fixtures.
    // =========================================================================

    fn greeter() -> ServiceTrait {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
                fn health(&self) -> bool;
            }
        };
        ServiceTrait::parse(item).unwrap()
    }

    #[test]
    fn client_struct_shape() {
        let svc = greeter();
        let got = canon(emit::client_struct(&svc));
        assert!(got.contains("pub struct GreeterClient < T >"), "got: {got}");
        assert!(
            got.contains(":: myelin :: ClientTransport < GreeterRequest , GreeterResponse >"),
            "got: {got}"
        );
        assert!(got.contains("pub fn new (transport : T)"), "got: {got}");
        // Async client method even for sync trait method.
        assert!(
            got.contains("pub async fn greet (& self , name : String)"),
            "got: {got}"
        );
        assert!(got.contains("pub async fn health (& self ,)"), "got: {got}");
        // Return-type shape uses `TransportResult<Ret>::Output`.
        assert!(
            got.contains("< T :: Error as :: myelin :: TransportResult < String >> :: Output"),
            "got: {got}"
        );
        assert!(
            got.contains("< T :: Error as :: myelin :: TransportResult < bool >> :: Output"),
            "got: {got}"
        );
        // Body dispatches through transport.call.
        assert!(
            got.contains("self . transport . call (GreeterRequest :: Greet { name }) . await"),
            "got: {got}"
        );
        assert!(
            got.contains("self . transport . call (GreeterRequest :: Health) . await"),
            "got: {got}"
        );
        assert!(
            got.contains("GreeterResponse :: Greet (v) => v"),
            "got: {got}"
        );
    }

    #[test]
    fn client_struct_unit_return_uses_unit_param() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait PingService {
                fn ping(&self);
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::client_struct(&svc));
        // Unit return → TransportResult<()>.
        assert!(
            got.contains(":: myelin :: TransportResult < () >"),
            "got: {got}"
        );
        // Match arm pattern must NOT have a payload — response variant is unit.
        assert!(
            got.contains("PingResponse :: Ping => ()"),
            "expected unit-variant pattern; got: {got}"
        );
        assert!(
            !got.contains("PingResponse :: Ping (v)"),
            "must not pattern-match a payload on unit response variant; got: {got}"
        );
    }

    #[test]
    fn client_sync_struct_shape() {
        let svc = greeter();
        let got = canon(emit::client_sync_struct(&svc));
        assert!(
            got.contains("pub struct GreeterClientSync < T , B >"),
            "got: {got}"
        );
        assert!(got.contains("inner : GreeterClient < T >"), "got: {got}");
        assert!(got.contains("block_on : B"), "got: {got}");
        assert!(got.contains(": :: myelin :: BlockOn"), "got: {got}");
        // Sync wrapper methods are plain `fn` with same return-type bound.
        assert!(
            got.contains("pub fn greet (& self , name : String)"),
            "got: {got}"
        );
        assert!(
            got.contains("self . block_on . block_on (self . inner . greet (name))"),
            "got: {got}"
        );
        // Where-clause carries through.
        assert!(
            got.contains("T :: Error : :: myelin :: TransportResult < String >"),
            "got: {got}"
        );
    }

    #[test]
    fn dispatch_fn_async_and_sync_method_calls() {
        let svc = greeter();
        let got = canon(emit::dispatch_fn(&svc));
        assert!(
            got.contains("pub async fn greeter_dispatch < S : GreeterService >"),
            "got: {got}"
        );
        assert!(got.contains("req : GreeterRequest"), "got: {got}");
        // Async trait method gets `.await`.
        assert!(
            got.contains("GreeterResponse :: Greet (svc . greet (name) . await)"),
            "got: {got}"
        );
        // Sync trait method must NOT get `.await` from the async dispatch fn.
        assert!(
            got.contains("GreeterResponse :: Health (svc . health ())"),
            "got: {got}"
        );
        assert!(
            !got.contains("svc . health () . await"),
            "sync method must not be awaited; got: {got}"
        );
    }

    #[test]
    fn dispatch_sync_fn_no_await_anywhere() {
        let svc = greeter();
        let got = canon(emit::dispatch_sync_fn(&svc));
        assert!(
            got.contains("pub fn greeter_dispatch_sync < S : GreeterServiceSync >"),
            "got: {got}"
        );
        assert!(!got.contains(". await"), "got: {got}");
        assert!(
            got.contains("GreeterResponse :: Greet (svc . greet (name))"),
            "got: {got}"
        );
        assert!(
            got.contains("GreeterResponse :: Health (svc . health ())"),
            "got: {got}"
        );
    }

    #[test]
    fn dispatch_unit_return_emits_unit_variant() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait PingService {
                async fn ping(&self);
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::dispatch_fn(&svc));
        // Side-effecting call followed by unit variant — never `Ping(())`.
        assert!(got.contains("svc . ping () . await ;"), "got: {got}");
        assert!(got.contains("PingResponse :: Ping"), "got: {got}");
        assert!(!got.contains("PingResponse :: Ping ("), "got: {got}");
    }

    #[test]
    fn dispatch_multi_arg_struct_pattern() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait MathService {
                async fn add(&self, a: i32, b: i32) -> i64;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::dispatch_fn(&svc));
        assert!(got.contains("MathRequest :: Add { a , b }"), "got: {got}");
        assert!(
            got.contains("MathResponse :: Add (svc . add (a , b) . await)"),
            "got: {got}"
        );
    }

    #[test]
    fn serve_fn_shape() {
        let svc = greeter();
        let got = canon(emit::serve_fn(&svc));
        assert!(
            got.contains("pub async fn greeter_serve < S , T >"),
            "got: {got}"
        );
        assert!(got.contains("S : GreeterService"), "got: {got}");
        assert!(
            got.contains("T : :: myelin :: ServerTransport < GreeterRequest , GreeterResponse >"),
            "got: {got}"
        );
        assert!(got.contains("transport . recv () . await ?"), "got: {got}");
        assert!(
            got.contains("greeter_dispatch (svc , req) . await"),
            "got: {got}"
        );
        assert!(
            got.contains("transport . reply (token , resp) . await"),
            "got: {got}"
        );
    }

    #[test]
    fn serve_sync_fn_shape() {
        let svc = greeter();
        let got = canon(emit::serve_sync_fn(&svc));
        assert!(
            got.contains("pub fn greeter_serve_sync < S , T , B >"),
            "got: {got}"
        );
        assert!(got.contains("S : GreeterServiceSync"), "got: {got}");
        assert!(got.contains("B : :: myelin :: BlockOn"), "got: {got}");
        // recv & reply are wrapped in block_on; dispatch_sync called directly.
        assert!(
            got.contains("block_on . block_on (transport . recv ()) ?"),
            "got: {got}"
        );
        assert!(
            got.contains("greeter_dispatch_sync (svc , req)"),
            "got: {got}"
        );
        assert!(
            got.contains("block_on . block_on (transport . reply (token , resp))"),
            "got: {got}"
        );
    }

    #[test]
    fn snake_case_stem_for_dispatch_idents() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait FooBarService {
                fn a(&self) -> u32;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        assert!(
            canon(emit::dispatch_fn(&svc)).contains("fn foo_bar_dispatch <"),
            "expected snake_case stem in dispatch fn ident"
        );
        assert!(
            canon(emit::serve_fn(&svc)).contains("fn foo_bar_serve <"),
            "expected snake_case stem in serve fn ident"
        );
    }

    #[test]
    fn visibility_propagates_to_clients_and_dispatch() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub(crate) trait FooService {
                fn a(&self) -> u32;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        assert!(
            canon(emit::client_struct(&svc)).contains("pub (crate) struct FooClient"),
            "vis on client struct"
        );
        assert!(
            canon(emit::client_sync_struct(&svc)).contains("pub (crate) struct FooClientSync"),
            "vis on sync client struct"
        );
        assert!(
            canon(emit::dispatch_fn(&svc)).contains("pub (crate) async fn foo_dispatch"),
            "vis on dispatch fn"
        );
        assert!(
            canon(emit::serve_sync_fn(&svc)).contains("pub (crate) fn foo_serve_sync"),
            "vis on serve_sync fn"
        );
    }

    // =========================================================================
    // Transport aliases (item 7).
    //
    // The new emit functions (`tokio_transport_alias` /
    // `embassy_transport_aliases`) only exist when `myelin-macros` is built
    // with its own matching feature, so the tests live inside feature-gated
    // submodules.
    // =========================================================================

    #[cfg(feature = "tokio")]
    mod tokio_transport_alias_tests {
        use super::*;

        #[test]
        fn tokio_transport_alias_shape() {
            let svc = greeter();
            let got = canon(emit::tokio_transport_alias(&svc));
            // No `#[cfg]` attribute in the emitted tokens.
            assert!(!got.contains("# [cfg (feature"), "got: {got}");
            assert!(
                got.contains(
                    "pub type GreeterTokioService = \
                     :: myelin :: transport_tokio :: TokioService < GreeterRequest , GreeterResponse >"
                ),
                "got: {got}"
            );
        }

        #[test]
        fn tokio_transport_alias_visibility_propagates() {
            let item: syn::ItemTrait = syn::parse_quote! {
                pub(crate) trait FooService {
                    fn a(&self) -> u32;
                }
            };
            let svc = ServiceTrait::parse(item).unwrap();
            let got = canon(emit::tokio_transport_alias(&svc));
            assert!(!got.contains("# [cfg (feature"), "got: {got}");
            assert!(
                got.contains("pub (crate) type FooTokioService"),
                "got: {got}"
            );
        }
    }

    #[cfg(feature = "embassy")]
    mod embassy_transport_aliases_tests {
        use super::*;

        #[test]
        fn embassy_transport_aliases_shape() {
            let svc = greeter();
            let got = canon(emit::embassy_transport_aliases(&svc));
            // No `#[cfg]` attribute in the emitted tokens.
            assert!(!got.contains("# [cfg (feature"), "got: {got}");
            assert!(
                got.contains(
                    "pub type GreeterEmbassyService < M , const CHANNEL_DEPTH : usize > = \
                     :: myelin :: transport_embassy :: EmbassyService < \
                     M , GreeterRequest , GreeterResponse , CHANNEL_DEPTH >"
                ),
                "got: {got}"
            );
            assert!(
                got.contains(
                    "pub type GreeterEmbassyClientTransport < 'a , M , const CHANNEL_DEPTH : usize > = \
                     :: myelin :: transport_embassy :: EmbassyClient < \
                     'a , M , GreeterRequest , GreeterResponse , CHANNEL_DEPTH >"
                ),
                "got: {got}"
            );
        }

        #[test]
        fn embassy_transport_aliases_visibility_propagates() {
            let item: syn::ItemTrait = syn::parse_quote! {
                pub(crate) trait FooService {
                    fn a(&self) -> u32;
                }
            };
            let svc = ServiceTrait::parse(item).unwrap();
            let got = canon(emit::embassy_transport_aliases(&svc));
            assert!(!got.contains("# [cfg (feature"), "got: {got}");
            assert!(
                got.contains("pub (crate) type FooEmbassyService"),
                "got: {got}"
            );
            assert!(
                got.contains("pub (crate) type FooEmbassyClientTransport"),
                "got: {got}"
            );
        }
    }

    // =========================================================================
    // Embassy instantiation `macro_rules!` (item 8).
    //
    // We assert via substring matching. The resulting token stream contains
    // literal `$` metavars and nested `macro_rules!` declarations which only
    // become meaningful once a downstream crate invokes them; compile-level
    // verification lives in subtask 5's downstream test fixtures.
    //
    // `emit::embassy_instantiation` only exists when `myelin-macros` is
    // built with its own `embassy` feature, so these tests live inside a
    // feature-gated submodule.
    // =========================================================================

    #[cfg(feature = "embassy")]
    mod embassy_instantiation_tests {
        use super::*;

        #[test]
        fn embassy_instantiation_outer_macro_shape() {
            let svc = greeter();
            let got = canon(emit::embassy_instantiation(&svc));
            // No `#[cfg]` attribute on the emitted macro — the gating
            // happens at proc-macro build time, not at consumer build time.
            assert!(
                !got.contains("# [cfg (feature = \"embassy\")]"),
                "expected no `#[cfg]` on emitted macro; got: {got}"
            );
            assert!(got.contains("# [macro_export]"), "got: {got}");
            assert!(
                got.contains("macro_rules ! greeter_embassy_service"),
                "got: {got}"
            );
            // Outer arm pattern.
            assert!(
                got.contains("($ name : ident , $ mutex : ty , $ depth : expr)"),
                "got: {got}"
            );
            // `paste` is reached via ::myelin::paste::paste!.
            assert!(
                got.contains(":: myelin :: paste :: paste !"),
                "expected absolute ::myelin::paste::paste! path; got: {got}"
            );
        }

        #[test]
        fn embassy_instantiation_static_service_cell() {
            let svc = greeter();
            let got = canon(emit::embassy_instantiation(&svc));
            // Static service ident prefix pasted with $name:upper.
            assert!(
                got.contains("[< __GREETER_SERVICE_ $ name : upper >]"),
                "got: {got}"
            );
            // Full EmbassyService<…> type with stem-derived req/resp baked in.
            assert!(
                got.contains(
                    ":: myelin :: transport_embassy :: EmbassyService < \
                     $ mutex , GreeterRequest , GreeterResponse , $ depth , >"
                ),
                "got: {got}"
            );
            assert!(
                got.contains(":: myelin :: transport_embassy :: EmbassyService :: new ()"),
                "got: {got}"
            );
        }

        #[test]
        fn embassy_instantiation_nested_client_macro() {
            let svc = greeter();
            let got = canon(emit::embassy_instantiation(&svc));
            assert!(
                got.contains("macro_rules ! [< $ name _client >]"),
                "got: {got}"
            );
            // StaticCell via absolute ::myelin::static_cell::StaticCell path.
            assert!(
                got.contains(":: myelin :: static_cell :: StaticCell"),
                "got: {got}"
            );
            // Baked-in client ident.
            assert!(
                got.contains(
                    "GreeterClient :: new (& * CELL . init \
                     ([< __GREETER_SERVICE_ $ name : upper >] . client () ,) ,)"
                ),
                "got: {got}"
            );
        }

        #[test]
        fn embassy_instantiation_nested_server_macro() {
            let svc = greeter();
            let got = canon(emit::embassy_instantiation(&svc));
            assert!(
                got.contains("macro_rules ! [< $ name _server >]"),
                "got: {got}"
            );
            assert!(
                got.contains("[< __GREETER_SERVICE_ $ name : upper >] . server ()"),
                "got: {got}"
            );
        }

        #[test]
        fn embassy_instantiation_nested_client_sync_macro() {
            let svc = greeter();
            let got = canon(emit::embassy_instantiation(&svc));
            assert!(
                got.contains("macro_rules ! [< $ name _client_sync >]"),
                "got: {got}"
            );
            assert!(got.contains("($ block_on : expr)"), "got: {got}");
            // Baked-in sync client + async client refs.
            assert!(got.contains("GreeterClientSync :: new ("), "got: {got}");
            assert!(
                got.contains("GreeterClient :: new (& * CELL . init"),
                "got: {got}"
            );
            assert!(got.contains("$ block_on ,"), "got: {got}");
        }

        #[test]
        fn embassy_instantiation_snake_and_screaming_stem() {
            let item: syn::ItemTrait = syn::parse_quote! {
                pub trait FooBarService {
                    async fn a(&self) -> u32;
                }
            };
            let svc = ServiceTrait::parse(item).unwrap();
            let got = canon(emit::embassy_instantiation(&svc));
            assert!(
                got.contains("macro_rules ! foo_bar_embassy_service"),
                "expected snake_case stem in outer macro ident; got: {got}"
            );
            assert!(
                got.contains("[< __FOO_BAR_SERVICE_ $ name : upper >]"),
                "expected SCREAMING_SNAKE stem in static prefix; got: {got}"
            );
            assert!(
                got.contains("FooBarClient :: new"),
                "expected PascalCase stem in nested client body; got: {got}"
            );
            assert!(got.contains("FooBarClientSync :: new"), "got: {got}");
        }
    }

    // ----- subtask 4: api_id constant & attribute parsing -----

    /// Known-good FNV-1a16 values. Regression-pins the hash function so
    /// that future refactors don't silently shift the wire id of existing
    /// services. (Spot-checked by hand-tracing the algorithm.)
    #[test]
    fn fnv1a16_known_values() {
        // Both values are non-zero and distinct — the doc-comment on the
        // emitted constant promises both properties for "reasonable" trait
        // names.
        let greeter = emit::fnv1a16("GreeterService");
        let math = emit::fnv1a16("MathService");
        assert_ne!(greeter, 0);
        assert_ne!(math, 0);
        assert_ne!(greeter, math);

        // Pinned literals — recompute and paste if you intentionally change
        // the hash definition.
        assert_eq!(emit::fnv1a16("GreeterService"), 0x237d);
        assert_eq!(emit::fnv1a16("MathService"), 0x90b0);
        assert_eq!(emit::fnv1a16(""), 0x1cd9); // FNV offset basis folded
    }

    #[test]
    fn api_id_const_default_hash() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::api_id_const(&svc, None));
        // Constant ident is SCREAMING_SNAKE of the stem + `_API_ID`.
        assert!(
            got.contains("pub const GREETER_API_ID : u16 ="),
            "got: {got}"
        );
        // Default value is the FNV-1a hash of the full trait ident.
        let want = emit::fnv1a16("GreeterService");
        assert!(
            got.contains(&format!("= {want}u16")) || got.contains(&format!("= {want} u16")),
            "expected literal `{want}u16` in: {got}"
        );
    }

    #[test]
    fn api_id_const_override() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                async fn greet(&self, name: String) -> String;
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::api_id_const(&svc, Some(0x1234)));
        assert!(
            got.contains("pub const GREETER_API_ID : u16 ="),
            "got: {got}"
        );
        // The literal lands verbatim in the tokens as `4660u16` (decimal
        // form used by quote! for integer literals).
        assert!(
            got.contains("4660u16") || got.contains("4660 u16"),
            "got: {got}"
        );
    }

    #[test]
    fn api_id_const_multi_word_stem() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait FooBarService {
                fn ping(&self);
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::api_id_const(&svc, None));
        assert!(
            got.contains("pub const FOO_BAR_API_ID : u16 ="),
            "expected SCREAMING_SNAKE multi-word stem; got: {got}"
        );
    }

    #[test]
    fn api_id_const_carries_doc_comment() {
        let item: syn::ItemTrait = syn::parse_quote! {
            pub trait GreeterService {
                fn ping(&self);
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::api_id_const(&svc, None));
        // Doc comment mentions the stem *and* points users at the override.
        assert!(
            got.contains("Wire-level API identifier for the Greeter service"),
            "got: {got}"
        );
        assert!(got.contains("api_id = 0x0001"), "got: {got}");
    }

    #[test]
    fn api_id_const_preserves_visibility() {
        // Private trait (no `pub`) should yield a private const.
        let item: syn::ItemTrait = syn::parse_quote! {
            trait GreeterService {
                fn ping(&self);
            }
        };
        let svc = ServiceTrait::parse(item).unwrap();
        let got = canon(emit::api_id_const(&svc, None));
        assert!(got.contains("const GREETER_API_ID : u16 ="), "got: {got}");
        assert!(
            !got.contains("pub const GREETER_API_ID"),
            "expected private const; got: {got}"
        );
    }

    // ----- attribute parser -----

    fn parse_args(ts: proc_macro2::TokenStream) -> syn::Result<ServiceArgs> {
        ServiceArgs::parse(ts)
    }

    #[test]
    fn service_args_empty() {
        let args = parse_args(quote! {}).unwrap();
        assert!(args.api_id.is_none());
    }

    #[test]
    fn service_args_hex_api_id() {
        let args = parse_args(quote! { api_id = 0x1234 }).unwrap();
        assert_eq!(args.api_id, Some(0x1234));
    }

    #[test]
    fn service_args_decimal_api_id() {
        let args = parse_args(quote! { api_id = 42 }).unwrap();
        assert_eq!(args.api_id, Some(42));
    }

    #[test]
    fn service_args_api_id_max() {
        let args = parse_args(quote! { api_id = 0xffff }).unwrap();
        assert_eq!(args.api_id, Some(u16::MAX));
    }

    #[test]
    fn service_args_api_id_overflow() {
        let err = parse_args(quote! { api_id = 70000 }).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not fit in u16"), "got: {msg}");
    }

    #[test]
    fn service_args_unknown_key() {
        let err = parse_args(quote! { unknown = 1 }).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown"), "got: {msg}");
    }

    #[test]
    fn service_args_duplicate_api_id() {
        let err = parse_args(quote! { api_id = 1, api_id = 2 }).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("more than once"), "got: {msg}");
    }

    #[test]
    fn service_args_api_id_non_integer_literal() {
        // A string literal in the value position is rejected by `LitInt`.
        let err = parse_args(quote! { api_id = "nope" }).unwrap_err();
        // Error message comes from `syn` itself; we just want *some* error.
        let _ = err.to_string();
    }
}
