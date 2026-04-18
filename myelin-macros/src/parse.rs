//! Parser & validator for `#[myelin::service]` traits.
//!
//! Converts a [`syn::ItemTrait`] into a [`ServiceTrait`] with all the
//! per-method information needed for code generation. All input validation
//! lives here; emit code can assume a well-formed [`ServiceTrait`].

use convert_case::{Case, Casing};
use proc_macro2::{Span, TokenStream};
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{
    FnArg, Ident, LitInt, Pat, PatIdent, ReturnType, TraitItem, TraitItemFn, Type, Visibility,
};

/// Parsed `#[myelin::service(...)]` attribute arguments.
///
/// Currently only `api_id = <int>` is recognised. Unknown keys are a hard
/// error so that additional knobs can be added later without silently
/// accepting typos.
#[derive(Default, Debug)]
pub struct ServiceArgs {
    /// Explicit `api_id = ...` override. `None` ⇒ emit the FNV-1a hash of
    /// the trait ident as the wire-level api id.
    pub api_id: Option<u16>,
}

impl ServiceArgs {
    /// Parse the attribute-arg token stream handed to the proc macro.
    ///
    /// An empty stream yields `ServiceArgs::default()` (i.e. every field
    /// `None`). Unknown keys, duplicate keys, and out-of-range values
    /// produce a `syn::Error` spanned at the offending token.
    pub fn parse(attr: TokenStream) -> syn::Result<Self> {
        let mut args = ServiceArgs::default();
        // Track the span of a previous `api_id = ...` so a duplicate can
        // point at *both* occurrences via `syn::Error::combine`.
        let mut api_id_span: Option<Span> = None;

        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("api_id") {
                let value: LitInt = meta.value()?.parse()?;
                let n: u64 = value.base10_parse()?;
                if n > u16::MAX as u64 {
                    return Err(syn::Error::new(
                        value.span(),
                        format!(
                            "myelin::service: api_id = {n} does not fit in u16 (max {})",
                            u16::MAX
                        ),
                    ));
                }
                if api_id_span.is_some() {
                    return Err(syn::Error::new(
                        value.span(),
                        "myelin::service: `api_id` specified more than once",
                    ));
                }
                api_id_span = Some(value.span());
                args.api_id = Some(n as u16);
                Ok(())
            } else {
                let name = meta
                    .path
                    .get_ident()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "<non-ident>".to_string());
                Err(meta.error(format_args!(
                    "myelin::service: unknown argument `{name}`; \
                     the only currently supported key is `api_id`",
                )))
            }
        });
        parser.parse2(attr)?;
        Ok(args)
    }
}

/// A validated service trait ready for code generation.
pub struct ServiceTrait {
    /// The original trait item, preserved verbatim for re-emission as the
    /// async trait.
    pub item: syn::ItemTrait,
    /// Visibility of the original trait — applied to all generated items.
    pub vis: Visibility,
    /// "Stem" derived by stripping a trailing `Service` from the trait ident
    /// (e.g. `GreeterService` → `Greeter`).
    pub stem: Ident,
    /// Validated method signatures.
    pub methods: Vec<ServiceMethod>,
}

/// One method of a service trait.
pub struct ServiceMethod {
    /// Verbatim signature copied from the trait. The method ident lives at
    /// `sig.ident`; client/dispatch/serve emit re-uses this directly.
    pub sig: syn::Signature,
    /// PascalCase variant ident, e.g. `Greet`, spanned at the original
    /// method ident.
    pub variant_ident: Ident,
    /// Method args (excluding `&self`).
    pub args: Vec<ServiceArg>,
    /// Return type kind.
    pub ret: ReturnKind,
    /// Whether the method was declared `async`. Drives the `.await` suffix
    /// emitted by `{stem}_dispatch` (sync trait methods are called without
    /// `.await` from the async dispatch fn to avoid a spurious state
    /// machine).
    pub is_async: bool,
}

/// One argument of a method.
pub struct ServiceArg {
    pub ident: Ident,
    pub ty: Type,
}

/// What the method returns.
pub enum ReturnKind {
    /// Implicit `()` (no `-> ...`).
    Unit,
    /// Anything else, captured verbatim.
    Type(Type),
}

impl ServiceTrait {
    /// Parse and validate a `syn::ItemTrait`.
    pub fn parse(item: syn::ItemTrait) -> syn::Result<Self> {
        let mut errors: Option<syn::Error> = None;

        // ----- trait-level validation -----

        if !item.generics.params.is_empty() {
            push_err(
                &mut errors,
                syn::Error::new(
                    item.generics.span(),
                    "myelin::service: trait generics are not supported in v1",
                ),
            );
        }
        if let Some(wc) = &item.generics.where_clause {
            push_err(
                &mut errors,
                syn::Error::new(
                    wc.span(),
                    "myelin::service: where-clauses on the trait are not supported",
                ),
            );
        }
        if !item.supertraits.is_empty() {
            push_err(
                &mut errors,
                syn::Error::new(
                    item.supertraits.span(),
                    "myelin::service: supertraits are not supported",
                ),
            );
        }
        if let Some(unsafety) = item.unsafety {
            push_err(
                &mut errors,
                syn::Error::new(
                    unsafety.span(),
                    "myelin::service: unsafe traits are not supported",
                ),
            );
        }
        if let Some(auto) = item.auto_token {
            push_err(
                &mut errors,
                syn::Error::new(
                    auto.span(),
                    "myelin::service: auto traits are not supported",
                ),
            );
        }

        // ----- stem derivation -----

        let stem = match derive_stem(&item.ident) {
            Ok(s) => s,
            Err(e) => {
                push_err(&mut errors, e);
                // We still try to keep parsing methods so the user sees more
                // diagnostics. Use a placeholder ident (won't be emitted
                // because we'll bail with the combined error).
                Ident::new("__myelin_stem_placeholder", item.ident.span())
            }
        };

        // ----- method validation -----

        let mut methods = Vec::new();
        for trait_item in &item.items {
            match trait_item {
                TraitItem::Fn(f) => match parse_method(f) {
                    Ok(m) => methods.push(m),
                    Err(e) => push_err(&mut errors, e),
                },
                other => push_err(
                    &mut errors,
                    syn::Error::new(
                        other.span(),
                        "myelin::service: only `fn` items are allowed in service traits",
                    ),
                ),
            }
        }

        if let Some(e) = errors {
            return Err(e);
        }

        Ok(Self {
            vis: item.vis.clone(),
            stem,
            methods,
            item,
        })
    }
}

fn derive_stem(ident: &Ident) -> syn::Result<Ident> {
    let s = ident.to_string();
    let stem = s.strip_suffix("Service").ok_or_else(|| {
        syn::Error::new(
            ident.span(),
            "myelin::service: trait name must end in `Service` (e.g. `GreeterService`)",
        )
    })?;
    if stem.is_empty() {
        return Err(syn::Error::new(
            ident.span(),
            "myelin::service: trait name must have a non-empty stem before `Service`",
        ));
    }
    Ok(Ident::new(stem, ident.span()))
}

fn parse_method(f: &TraitItemFn) -> syn::Result<ServiceMethod> {
    let sig = &f.sig;
    let mut errors: Option<syn::Error> = None;

    // No default body.
    if let Some(default) = &f.default {
        push_err(
            &mut errors,
            syn::Error::new(
                default.span(),
                "myelin::service: trait methods must not have a default body",
            ),
        );
    }

    // Reject signature modifiers we don't model.
    if let Some(c) = sig.constness {
        push_err(
            &mut errors,
            syn::Error::new(c.span(), "myelin::service: const fn not supported"),
        );
    }
    if let Some(u) = sig.unsafety {
        push_err(
            &mut errors,
            syn::Error::new(u.span(), "myelin::service: unsafe fn not supported"),
        );
    }
    if let Some(abi) = &sig.abi {
        push_err(
            &mut errors,
            syn::Error::new(abi.span(), "myelin::service: extern fn not supported"),
        );
    }
    if let Some(v) = &sig.variadic {
        push_err(
            &mut errors,
            syn::Error::new(v.span(), "myelin::service: variadic fn not supported"),
        );
    }
    if !sig.generics.params.is_empty() {
        push_err(
            &mut errors,
            syn::Error::new(
                sig.generics.span(),
                "myelin::service: method generics are not supported",
            ),
        );
    }
    if let Some(wc) = &sig.generics.where_clause {
        push_err(
            &mut errors,
            syn::Error::new(
                wc.span(),
                "myelin::service: where-clauses on methods are not supported",
            ),
        );
    }

    // First arg must be `&self`.
    let mut inputs = sig.inputs.iter();
    match inputs.next() {
        Some(FnArg::Receiver(r)) => {
            // Must be `&self` — borrow present, no `mut`, no explicit type, no lifetime.
            if r.reference.is_none() {
                push_err(
                    &mut errors,
                    syn::Error::new(
                        r.span(),
                        "myelin::service: receiver must be `&self` (not `self`)",
                    ),
                );
            }
            if r.mutability.is_some() {
                push_err(
                    &mut errors,
                    syn::Error::new(
                        r.span(),
                        "myelin::service: receiver must be `&self` (not `&mut self`)",
                    ),
                );
            }
            if let Some((_, Some(lt))) = &r.reference {
                push_err(
                    &mut errors,
                    syn::Error::new(
                        lt.span(),
                        "myelin::service: explicit lifetimes on `&self` not supported",
                    ),
                );
            }
            if r.colon_token.is_some() {
                push_err(
                    &mut errors,
                    syn::Error::new(
                        r.span(),
                        "myelin::service: receiver must be plain `&self` (no explicit type)",
                    ),
                );
            }
        }
        Some(other) => {
            push_err(
                &mut errors,
                syn::Error::new(
                    other.span(),
                    "myelin::service: first argument must be `&self`",
                ),
            );
        }
        None => {
            push_err(
                &mut errors,
                syn::Error::new(
                    sig.paren_token.span.span(),
                    "myelin::service: method must take `&self`",
                ),
            );
        }
    }

    // Remaining args must be plain `ident: OwnedType`.
    let mut args = Vec::new();
    for input in inputs {
        match input {
            FnArg::Receiver(r) => {
                push_err(
                    &mut errors,
                    syn::Error::new(
                        r.span(),
                        "myelin::service: receiver may only appear as the first argument",
                    ),
                );
            }
            FnArg::Typed(pt) => {
                let ident = match &*pt.pat {
                    Pat::Ident(PatIdent {
                        by_ref: None,
                        mutability: None,
                        ident,
                        subpat: None,
                        ..
                    }) => Some(ident.clone()),
                    other => {
                        push_err(
                            &mut errors,
                            syn::Error::new(
                                other.span(),
                                "myelin::service: argument must be a plain `ident: Type` \
                                 (no destructuring, `_`, `ref`, or `mut`)",
                            ),
                        );
                        None
                    }
                };
                if let Type::Reference(r) = &*pt.ty {
                    push_err(
                        &mut errors,
                        syn::Error::new(
                            r.span(),
                            "myelin::service: argument types must be owned \
                             (use `String`/`Vec<T>`/... instead of `&str`/`&[T]`)",
                        ),
                    );
                }
                if let Some(ident) = ident {
                    args.push(ServiceArg {
                        ident,
                        ty: (*pt.ty).clone(),
                    });
                }
            }
        }
    }

    // Return type.
    let ret = match &sig.output {
        ReturnType::Default => ReturnKind::Unit,
        ReturnType::Type(_, ty) => ReturnKind::Type((**ty).clone()),
    };

    let is_async = sig.asyncness.is_some();

    if let Some(e) = errors {
        return Err(e);
    }

    let variant_ident = method_to_variant(&sig.ident);

    Ok(ServiceMethod {
        sig: sig.clone(),
        variant_ident,
        args,
        ret,
        is_async,
    })
}

fn method_to_variant(method_ident: &Ident) -> Ident {
    let pascal = method_ident.to_string().to_case(Case::Pascal);
    Ident::new(&pascal, method_ident.span())
}

fn push_err(slot: &mut Option<syn::Error>, err: syn::Error) {
    match slot {
        Some(existing) => existing.combine(err),
        None => *slot = Some(err),
    }
}

// Span helper so call sites read consistently.
#[allow(dead_code)]
fn call_site() -> Span {
    Span::call_site()
}
