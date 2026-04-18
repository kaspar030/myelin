//! Code generation for `#[myelin::service]`.
//!
//! All emitters take a [`ServiceTrait`] and return a
//! [`proc_macro2::TokenStream`]. The root macro stitches the results
//! together.

use convert_case::{Case, Casing};
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::{Ident, TraitItem};

use crate::parse::{ReturnKind, ServiceMethod, ServiceTrait};

/// `{Stem}Request` enum: one variant per method, struct-like fields.
pub fn request_enum(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let enum_ident = request_enum_ident(&svc.stem);

    let variants = svc.methods.iter().map(|m| {
        let v = &m.variant_ident;
        if m.args.is_empty() {
            // Unit variant — matches hand-written `GreeterRequest::Health`.
            quote! { #v }
        } else {
            let fields = m.args.iter().map(|a| {
                let name = &a.ident;
                let ty = &a.ty;
                quote! { #name: #ty }
            });
            quote! { #v { #(#fields),* } }
        }
    });

    quote! {
        #[derive(Debug)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #vis enum #enum_ident {
            #(#variants,)*
        }
    }
}

/// `{Stem}Response` enum: one tuple variant per method wrapping the
/// return type. Unit returns produce a unit variant.
pub fn response_enum(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let enum_ident = response_enum_ident(&svc.stem);

    let variants = svc.methods.iter().map(|m| {
        let v = &m.variant_ident;
        match &m.ret {
            ReturnKind::Unit => quote! { #v },
            ReturnKind::Type(ty) => quote! { #v(#ty) },
        }
    });

    quote! {
        #[derive(Debug)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #vis enum #enum_ident {
            #(#variants,)*
        }
    }
}

/// Re-emit the user's trait verbatim as the async trait.
pub fn async_trait(svc: &ServiceTrait) -> TokenStream {
    svc.item.to_token_stream()
}

/// `{STEM_UPPER}_API_ID: u16` constant with a doc-comment warning about the
/// 2^16-wide id space.
///
/// When `override_id` is `Some`, it is emitted verbatim; otherwise the
/// 16-bit FNV-1a hash of the full trait ident string (e.g. `GreeterService`,
/// **not** the stripped stem) is used.
pub fn api_id_const(svc: &ServiceTrait, override_id: Option<u16>) -> TokenStream {
    let vis = &svc.vis;
    let stem_upper = stem_upper(&svc.stem);
    let const_ident = Ident::new(&format!("{stem_upper}_API_ID"), svc.stem.span());

    let trait_ident = &svc.item.ident;
    let trait_ident_str = trait_ident.to_string();
    let stem_str = svc.stem.to_string();

    let value: u16 = override_id.unwrap_or_else(|| fnv1a16(&trait_ident_str));

    // Prebuild the doc string at macro-expansion time — no `concat!` needed
    // in the emitted code.
    let doc = format!(
        "Wire-level API identifier for the {stem_str} service.\n\
         \n\
         Default value is a 16-bit FNV-1a hash of the trait name.\n\
         Since the API id space is only 2^16 wide, collisions are possible across\n\
         unrelated services. For production deployments, override explicitly:\n\
         \n\
         ```ignore\n\
         #[myelin::service(api_id = 0x0001)]\n\
         pub trait {trait_ident_str} {{ /* ... */ }}\n\
         ```",
    );

    quote! {
        #[doc = #doc]
        #vis const #const_ident: u16 = #value;
    }
}

/// 16-bit FNV-1a hash: 32-bit FNV-1a, then XOR-fold top and bottom halves.
///
/// Computed at macro-expansion time; the emitted code contains only a `u16`
/// literal.
pub fn fnv1a16(s: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    ((h >> 16) as u16) ^ (h as u16)
}

/// `{Stem}ServiceSync` trait with every method de-asynced.
pub fn sync_trait(svc: &ServiceTrait) -> TokenStream {
    let mut cloned = svc.item.clone();
    cloned.ident = sync_trait_ident(&svc.stem);

    for item in cloned.items.iter_mut() {
        if let TraitItem::Fn(f) = item {
            f.sig.asyncness = None;
        }
    }

    cloned.to_token_stream()
}

// ----- ident helpers -----

fn request_enum_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}Request"), stem.span())
}

fn response_enum_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}Response"), stem.span())
}

fn sync_trait_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}ServiceSync"), stem.span())
}

fn client_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}Client"), stem.span())
}

fn client_sync_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}ClientSync"), stem.span())
}

fn stem_snake(stem: &Ident) -> Ident {
    let snake = stem.to_string().to_case(Case::Snake);
    Ident::new(&snake, stem.span())
}

fn dispatch_fn_ident(stem: &Ident) -> Ident {
    let snake = stem_snake(stem);
    Ident::new(&format!("{snake}_dispatch"), stem.span())
}

fn dispatch_sync_fn_ident(stem: &Ident) -> Ident {
    let snake = stem_snake(stem);
    Ident::new(&format!("{snake}_dispatch_sync"), stem.span())
}

fn serve_fn_ident(stem: &Ident) -> Ident {
    let snake = stem_snake(stem);
    Ident::new(&format!("{snake}_serve"), stem.span())
}

fn serve_sync_fn_ident(stem: &Ident) -> Ident {
    let snake = stem_snake(stem);
    Ident::new(&format!("{snake}_serve_sync"), stem.span())
}

fn tokio_service_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}TokioService"), stem.span())
}

fn embassy_service_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}EmbassyService"), stem.span())
}

fn embassy_client_transport_ident(stem: &Ident) -> Ident {
    Ident::new(&format!("{stem}EmbassyClientTransport"), stem.span())
}

fn embassy_macro_ident(stem: &Ident) -> Ident {
    let snake = stem_snake(stem);
    Ident::new(&format!("{snake}_embassy_service"), stem.span())
}

fn stem_upper(stem: &Ident) -> Ident {
    let upper = stem.to_string().to_case(Case::UpperSnake);
    Ident::new(&upper, stem.span())
}

fn embassy_static_prefix_ident(stem: &Ident) -> Ident {
    // Prefix used inside the paste!-concatenated static ident:
    // `[< __{STEM_UPPER}_SERVICE_ $name:upper >]`.
    Ident::new(&format!("__{}_SERVICE_", stem_upper(stem)), stem.span())
}

/// Return-type token stream usable as a `TransportResult<_>` type parameter.
/// Maps `ReturnKind::Unit` to `()`.
fn ret_ty_tokens(ret: &ReturnKind) -> TokenStream {
    match ret {
        ReturnKind::Unit => quote! { () },
        ReturnKind::Type(ty) => quote! { #ty },
    }
}

/// Build a `{Stem}Request::{Variant}` pattern/constructor given the method's args.
/// - Zero args  → unit form: `#req_ty::#variant`
/// - N args     → struct form with init shorthand: `#req_ty::#variant { a, b, .. }`
fn request_ctor(req_ty: &Ident, m: &ServiceMethod) -> TokenStream {
    let v = &m.variant_ident;
    if m.args.is_empty() {
        quote! { #req_ty::#v }
    } else {
        let names = m.args.iter().map(|a| &a.ident);
        quote! { #req_ty::#v { #(#names),* } }
    }
}

// =============================================================================
// Emitters
// =============================================================================

/// `{Stem}Client<T>` async client struct + one `async fn` per trait method.
///
/// Every client method is `async` regardless of whether the trait method was
/// async — the transport call is always async. Method bodies dispatch through
/// `T::call(req).await` and unwrap the matching `{Stem}Response::{Variant}`.
pub fn client_struct(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let req_ty = request_enum_ident(&svc.stem);
    let resp_ty = response_enum_ident(&svc.stem);
    let client_ty = client_ident(&svc.stem);

    let methods = svc.methods.iter().map(|m| {
        let name = &m.sig.ident;
        let variant = &m.variant_ident;
        let arg_decls = m.args.iter().map(|a| {
            let n = &a.ident;
            let t = &a.ty;
            quote! { #n: #t }
        });
        let ret_ty = ret_ty_tokens(&m.ret);
        let req_expr = request_ctor(&req_ty, m);
        // Match the response variant. Unit returns are emitted as unit
        // variants on the response enum (`Reset` not `Reset(())`), so we
        // pattern-match without a payload and produce `()`.
        let resp_match = match &m.ret {
            ReturnKind::Unit => quote! {
                match resp {
                    #resp_ty::#variant => (),
                    #[allow(unreachable_patterns)]
                    _ => unreachable!(),
                }
            },
            ReturnKind::Type(_) => quote! {
                match resp {
                    #resp_ty::#variant(v) => v,
                    #[allow(unreachable_patterns)]
                    _ => unreachable!(),
                }
            },
        };

        quote! {
            #vis async fn #name(&self, #(#arg_decls),*)
                -> <T::Error as ::myelin::TransportResult<#ret_ty>>::Output
            where
                T::Error: ::myelin::TransportResult<#ret_ty>,
            {
                let result = self.transport.call(#req_expr).await;
                ::myelin::TransportResult::into_output(result.map(|resp| #resp_match))
            }
        }
    });

    quote! {
        #vis struct #client_ty<T> {
            transport: T,
        }

        impl<T> #client_ty<T>
        where
            T: ::myelin::ClientTransport<#req_ty, #resp_ty>,
        {
            #vis fn new(transport: T) -> Self {
                Self { transport }
            }

            #(#methods)*
        }
    }
}

/// `{Stem}ClientSync<T, B>` blocking wrapper around `{Stem}Client<T>`.
pub fn client_sync_struct(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let req_ty = request_enum_ident(&svc.stem);
    let resp_ty = response_enum_ident(&svc.stem);
    let client_ty = client_ident(&svc.stem);
    let client_sync_ty = client_sync_ident(&svc.stem);

    let methods = svc.methods.iter().map(|m| {
        let name = &m.sig.ident;
        let arg_decls = m.args.iter().map(|a| {
            let n = &a.ident;
            let t = &a.ty;
            quote! { #n: #t }
        });
        let arg_names = m.args.iter().map(|a| &a.ident);
        let ret_ty = ret_ty_tokens(&m.ret);

        quote! {
            #vis fn #name(&self, #(#arg_decls),*)
                -> <T::Error as ::myelin::TransportResult<#ret_ty>>::Output
            where
                T::Error: ::myelin::TransportResult<#ret_ty>,
            {
                self.block_on.block_on(self.inner.#name(#(#arg_names),*))
            }
        }
    });

    quote! {
        #vis struct #client_sync_ty<T, B> {
            inner: #client_ty<T>,
            block_on: B,
        }

        impl<T, B> #client_sync_ty<T, B>
        where
            T: ::myelin::ClientTransport<#req_ty, #resp_ty>,
            B: ::myelin::BlockOn,
        {
            #vis fn new(inner: #client_ty<T>, block_on: B) -> Self {
                Self { inner, block_on }
            }

            #(#methods)*
        }
    }
}

/// `{stem}_dispatch` — `async fn` that matches a request and calls the async
/// service trait. Sync trait methods are called *without* `.await` to avoid a
/// spurious async state machine.
pub fn dispatch_fn(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let trait_ty = &svc.item.ident; // `{Stem}Service`
    let req_ty = request_enum_ident(&svc.stem);
    let resp_ty = response_enum_ident(&svc.stem);
    let fn_ident = dispatch_fn_ident(&svc.stem);

    let arms = svc.methods.iter().map(|m| {
        let name = &m.sig.ident;
        let variant = &m.variant_ident;
        let pat = request_ctor(&req_ty, m);
        let arg_names = m.args.iter().map(|a| &a.ident);
        let call_suffix: TokenStream = if m.is_async { quote!(.await) } else { quote!() };
        let body = match &m.ret {
            ReturnKind::Unit => quote! {
                {
                    svc.#name(#(#arg_names),*)#call_suffix;
                    #resp_ty::#variant
                }
            },
            ReturnKind::Type(_) => quote! {
                #resp_ty::#variant(svc.#name(#(#arg_names),*)#call_suffix)
            },
        };
        quote! { #pat => #body, }
    });

    quote! {
        #vis async fn #fn_ident<S: #trait_ty>(svc: &S, req: #req_ty) -> #resp_ty {
            match req {
                #(#arms)*
            }
        }
    }
}

/// `{stem}_dispatch_sync` — plain `fn` over the sync mirror trait. Every arm
/// calls `svc.method(...)` directly (no `.await`).
pub fn dispatch_sync_fn(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let trait_ty = sync_trait_ident(&svc.stem);
    let req_ty = request_enum_ident(&svc.stem);
    let resp_ty = response_enum_ident(&svc.stem);
    let fn_ident = dispatch_sync_fn_ident(&svc.stem);

    let arms = svc.methods.iter().map(|m| {
        let name = &m.sig.ident;
        let variant = &m.variant_ident;
        let pat = request_ctor(&req_ty, m);
        let arg_names = m.args.iter().map(|a| &a.ident);
        let body = match &m.ret {
            ReturnKind::Unit => quote! {
                {
                    svc.#name(#(#arg_names),*);
                    #resp_ty::#variant
                }
            },
            ReturnKind::Type(_) => quote! {
                #resp_ty::#variant(svc.#name(#(#arg_names),*))
            },
        };
        quote! { #pat => #body, }
    });

    quote! {
        #vis fn #fn_ident<S: #trait_ty>(svc: &S, req: #req_ty) -> #resp_ty {
            match req {
                #(#arms)*
            }
        }
    }
}

/// `{stem}_serve` — async receive/dispatch/reply loop.
pub fn serve_fn(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let trait_ty = &svc.item.ident;
    let req_ty = request_enum_ident(&svc.stem);
    let resp_ty = response_enum_ident(&svc.stem);
    let fn_ident = serve_fn_ident(&svc.stem);
    let dispatch = dispatch_fn_ident(&svc.stem);

    quote! {
        #vis async fn #fn_ident<S, T>(svc: &S, transport: &mut T) -> ::core::result::Result<(), T::Error>
        where
            S: #trait_ty,
            T: ::myelin::ServerTransport<#req_ty, #resp_ty>,
        {
            loop {
                let (req, token) = transport.recv().await?;
                let resp = #dispatch(svc, req).await;
                let _ = transport.reply(token, resp).await;
            }
        }
    }
}

/// `{stem}_serve_sync` — blocking receive/dispatch/reply loop using `BlockOn`.
pub fn serve_sync_fn(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let trait_ty = sync_trait_ident(&svc.stem);
    let req_ty = request_enum_ident(&svc.stem);
    let resp_ty = response_enum_ident(&svc.stem);
    let fn_ident = serve_sync_fn_ident(&svc.stem);
    let dispatch_sync = dispatch_sync_fn_ident(&svc.stem);

    quote! {
        #vis fn #fn_ident<S, T, B>(
            svc: &S,
            transport: &mut T,
            block_on: &B,
        ) -> ::core::result::Result<(), T::Error>
        where
            S: #trait_ty,
            T: ::myelin::ServerTransport<#req_ty, #resp_ty>,
            B: ::myelin::BlockOn,
        {
            loop {
                let (req, token) = block_on.block_on(transport.recv())?;
                let resp = #dispatch_sync(svc, req);
                let _ = block_on.block_on(transport.reply(token, resp));
            }
        }
    }
}

// =============================================================================
// Transport convenience aliases (item 7)
// =============================================================================

/// Emit per-transport type aliases:
///
/// ```ignore
/// #[cfg(feature = "tokio")]
/// pub type {Stem}TokioService =
///     ::myelin::transport_tokio::TokioService<{Stem}Request, {Stem}Response>;
///
/// #[cfg(feature = "embassy")]
/// pub type {Stem}EmbassyService<M, const CHANNEL_DEPTH: usize> =
///     ::myelin::transport_embassy::EmbassyService<
///         M, {Stem}Request, {Stem}Response, CHANNEL_DEPTH,
///     >;
///
/// #[cfg(feature = "embassy")]
/// pub type {Stem}EmbassyClientTransport<'a, M, const CHANNEL_DEPTH: usize> =
///     ::myelin::transport_embassy::EmbassyClient<
///         'a, M, {Stem}Request, {Stem}Response, CHANNEL_DEPTH,
///     >;
/// ```
///
/// The `#[cfg(feature = "...")]` attributes refer to features of the
/// *consuming* crate, not of `myelin-macros`. The consumer is expected to
/// forward those features to `myelin`'s matching features (typically via
/// `tokio = ["myelin/tokio"]` / `embassy = ["myelin/embassy"]`).
pub fn transport_aliases(svc: &ServiceTrait) -> TokenStream {
    let vis = &svc.vis;
    let req = request_enum_ident(&svc.stem);
    let resp = response_enum_ident(&svc.stem);

    let tokio_alias = tokio_service_ident(&svc.stem);
    let embassy_svc = embassy_service_ident(&svc.stem);
    let embassy_cli = embassy_client_transport_ident(&svc.stem);

    quote! {
        #[cfg(feature = "tokio")]
        #vis type #tokio_alias =
            ::myelin::transport_tokio::TokioService<#req, #resp>;

        #[cfg(feature = "embassy")]
        #vis type #embassy_svc<M, const CHANNEL_DEPTH: usize> =
            ::myelin::transport_embassy::EmbassyService<M, #req, #resp, CHANNEL_DEPTH>;

        #[cfg(feature = "embassy")]
        #vis type #embassy_cli<'a, M, const CHANNEL_DEPTH: usize> =
            ::myelin::transport_embassy::EmbassyClient<'a, M, #req, #resp, CHANNEL_DEPTH>;
    }
}

// =============================================================================
// Embassy instantiation `macro_rules!` (item 8)
// =============================================================================

/// Emit a `#[macro_export] macro_rules! {stem_snake}_embassy_service` under
/// `#[cfg(feature = "embassy")]`.
///
/// Matches `testing-service`'s hand-written version in shape, but with
/// stem-derived idents (`{Stem}Client`, `{Stem}ClientSync`, `{Stem}Request`,
/// `{Stem}Response`) baked in at proc-macro expansion time, so the nested
/// `*_client!` / `*_server!` / `*_client_sync!` macros resolve regardless of
/// invocation scope (as long as the trait's module is accessible).
///
/// Generated shape:
///
/// ```ignore
/// #[cfg(feature = "embassy")]
/// #[macro_export]
/// macro_rules! {stem_snake}_embassy_service {
///     ($name:ident, $mutex:ty, $depth:expr) => {
///         ::myelin::paste::paste! {
///             static [<__{STEM_UPPER}_SERVICE_ $name:upper>]:
///                 ::myelin::transport_embassy::EmbassyService<
///                     $mutex, {Stem}Request, {Stem}Response, $depth,
///                 > = ::myelin::transport_embassy::EmbassyService::new();
///
///             macro_rules! [<$name _client>] { () => {{
///                 static CELL: ::myelin::static_cell::StaticCell<
///                     ::myelin::transport_embassy::EmbassyClient<
///                         'static, $mutex, {Stem}Request, {Stem}Response, $depth,
///                     >,
///                 > = ::myelin::static_cell::StaticCell::new();
///                 {Stem}Client::new(&*CELL.init([<__{STEM_UPPER}_SERVICE_ $name:upper>].client()))
///             }}; }
///
///             macro_rules! [<$name _server>] { () => {
///                 [<__{STEM_UPPER}_SERVICE_ $name:upper>].server()
///             }; }
///
///             macro_rules! [<$name _client_sync>] { ($block_on:expr) => {{
///                 static CELL: ::myelin::static_cell::StaticCell<
///                     ::myelin::transport_embassy::EmbassyClient<
///                         'static, $mutex, {Stem}Request, {Stem}Response, $depth,
///                     >,
///                 > = ::myelin::static_cell::StaticCell::new();
///                 {Stem}ClientSync::new(
///                     {Stem}Client::new(&*CELL.init([<__{STEM_UPPER}_SERVICE_ $name:upper>].client())),
///                     $block_on,
///                 )
///             }}; }
///         }
///     };
/// }
/// ```
///
/// # Path convention
///
/// The emitted macro uses absolute `::myelin::...` paths (including for
/// `paste` and `static_cell`) so it works uniformly from any consumer crate
/// without requiring the consumer to re-export supporting crates. `myelin`
/// itself re-exports `paste` (always) and `static_cell` (under `embassy`).
///
/// # `$` / `#` collision
///
/// `quote!` uses `#var` for interpolation and leaves `$` untouched, so
/// literal `$name` / `$mutex` / `$depth` / `$block_on` in the body pass
/// through verbatim to the emitted `macro_rules!`.
pub fn embassy_instantiation(svc: &ServiceTrait) -> TokenStream {
    let stem = &svc.stem;
    let macro_ident = embassy_macro_ident(stem);
    let req = request_enum_ident(stem);
    let resp = response_enum_ident(stem);
    let client = client_ident(stem);
    let client_sync = client_sync_ident(stem);
    let static_prefix = embassy_static_prefix_ident(stem);

    quote! {
        #[cfg(feature = "embassy")]
        #[macro_export]
        macro_rules! #macro_ident {
            ($name:ident, $mutex:ty, $depth:expr) => {
                ::myelin::paste::paste! {
                    static [< #static_prefix $name:upper >]:
                        ::myelin::transport_embassy::EmbassyService<
                            $mutex,
                            #req,
                            #resp,
                            $depth,
                        > = ::myelin::transport_embassy::EmbassyService::new();

                    macro_rules! [< $name _client >] {
                        () => {{
                            static CELL: ::myelin::static_cell::StaticCell<
                                ::myelin::transport_embassy::EmbassyClient<
                                    'static,
                                    $mutex,
                                    #req,
                                    #resp,
                                    $depth,
                                >,
                            > = ::myelin::static_cell::StaticCell::new();
                            #client::new(
                                &*CELL.init(
                                    [< #static_prefix $name:upper >].client(),
                                ),
                            )
                        }};
                    }

                    macro_rules! [< $name _server >] {
                        () => {
                            [< #static_prefix $name:upper >].server()
                        };
                    }

                    macro_rules! [< $name _client_sync >] {
                        ($block_on:expr) => {{
                            static CELL: ::myelin::static_cell::StaticCell<
                                ::myelin::transport_embassy::EmbassyClient<
                                    'static,
                                    $mutex,
                                    #req,
                                    #resp,
                                    $depth,
                                >,
                            > = ::myelin::static_cell::StaticCell::new();
                            #client_sync::new(
                                #client::new(
                                    &*CELL.init(
                                        [< #static_prefix $name:upper >].client(),
                                    ),
                                ),
                                $block_on,
                            )
                        }};
                    }
                }
            };
        }
    }
}
