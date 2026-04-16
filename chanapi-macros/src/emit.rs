//! Code generation for `#[chanapi::service]`.
//!
//! All emitters take a [`ServiceTrait`] and return a
//! [`proc_macro2::TokenStream`]. The root macro stitches the results
//! together.

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

// Currently unused by sibling modules, kept private to discourage leak.
#[allow(dead_code)]
fn method_variant_idents<'a>(svc: &'a ServiceTrait) -> impl Iterator<Item = &'a Ident> + 'a {
    svc.methods.iter().map(|m: &ServiceMethod| &m.variant_ident)
}
