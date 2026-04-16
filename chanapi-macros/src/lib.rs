//! Proc macros for the [`chanapi`](https://docs.rs/chanapi) crate.
//!
//! This crate is an implementation detail: always invoke its macros via the
//! `chanapi` re-exports (e.g. `#[chanapi::service]`).

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
///
/// The "stem" is derived by stripping a trailing `Service` from the trait
/// name (e.g. `GreeterService` → `Greeter`). The trait name must end in
/// `Service`; anything else is a compile error.
///
/// Both enums derive `Debug`, and derive `serde::Serialize` and
/// `serde::Deserialize` when the downstream crate's `serde` feature is
/// enabled.
///
/// # Input constraints
///
/// - Method receiver must be plain `&self`.
/// - No trait generics, lifetimes, supertraits, or where-clauses.
/// - Arg patterns must be `ident: Type`.
/// - Argument types must be owned (no `&T`/`&mut T`).
/// - Return types are passed through verbatim.
///
/// # Unstable API surface
///
/// Additional items (clients, dispatch, serve, transport aliases, embassy
/// glue, `api_id` constant) are emitted by later subtasks in this workstream
/// and are not produced by the current revision.
#[proc_macro_attribute]
pub fn service(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // Subtask 1: `attr` is ignored. Subtask 4 will parse `api_id = ...`.

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
            quote! {
                #req
                #resp
                #async_t
                #sync_t
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
    use super::{emit, parse::ServiceTrait};

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
        assert!(got.contains("Maybe (Result < String , u32 >)"), "got: {got}");
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
}
