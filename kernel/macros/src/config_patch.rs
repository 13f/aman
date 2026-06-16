// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! `#[derive(ConfigPatch)]` — generates a `Partial*` struct and a `ConfigPatch`
//! implementation for layered config merging.
//!
//! For a struct `Foo`, the macro emits:
//!
//! * `PartialFoo` with every field wrapped in `Option<<T as ConfigPatch>::Patch>`.
//! * `impl ConfigPatch for Foo` that recursively calls `ConfigPatch::merge` on
//!   each field when the corresponding patch field is `Some`.
//!
//! The generated code relies on the `ConfigPatch` trait being available at the
//! call site as `crate::patch::ConfigPatch` (when used inside the `config` crate).
//! Primitive/external types must therefore implement `ConfigPatch` with
//! `Patch = Self` (replace semantics), while nested config structs that also
//! derive `ConfigPatch` use `Patch = PartialSelf`.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{parse2, Attribute, Field, Fields, ItemStruct, Meta};

/// Returns `true` if the attribute is `#[derive(...)]`.
fn is_derive_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("derive")
}

/// Strip `serde(default)` / `serde(default = "...")` attributes from a field.
///
/// The generated `Partial*` fields are already `Option<T>` and default to `None`,
/// so per-field `serde(default)` functions (which return `T`, not `Option<T>`)
/// would not type-check. Other serde attributes such as `alias`, `rename`, and
/// `flatten` are preserved so the patch struct deserializes the same keys.
fn clean_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|&attr| {
            if !attr.path().is_ident("serde") {
                return true;
            }
            // Keep the attribute unless it is exactly `default` or `default = "..."`.
            match &attr.meta {
                Meta::List(list) => {
                    let mut keep = true;
                    if let Ok(nested) = list.parse_args_with(
                        syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                    ) {
                        for meta in nested {
                            if let Meta::Path(path) = meta {
                                if path.is_ident("default") {
                                    keep = false;
                                }
                            } else if let Meta::NameValue(nv) = meta
                                && nv.path.is_ident("default")
                            {
                                keep = false;
                            }
                        }
                    }
                    keep
                }
                _ => true,
            }
        })
        .cloned()
        .collect()
}

pub fn expand(input: TokenStream) -> TokenStream {
    let parsed = match parse2::<ItemStruct>(input) {
        Ok(s) => s,
        Err(e) => return e.to_compile_error(),
    };

    if !matches!(parsed.fields, Fields::Named(_)) {
        return syn::Error::new_spanned(
            parsed,
            "ConfigPatch only supports structs with named fields",
        )
        .to_compile_error();
    }

    let name = &parsed.ident;
    let vis = &parsed.vis;
    let partial_name = format_ident!("Partial{name}");

    // Copy non-derive struct attributes (e.g. serde(rename_all)) to the partial.
    let struct_attrs: Vec<_> = parsed
        .attrs
        .iter()
        .filter(|a| !is_derive_attr(a))
        .collect();

    let fields: Vec<&Field> = match &parsed.fields {
        Fields::Named(n) => n.named.iter().collect(),
        _ => unreachable!(),
    };

    let mut partial_fields = Vec::new();
    let mut merge_arms = Vec::new();
    let mut field_idents = Vec::new();

    for f in fields {
        let field_ident = f.ident.as_ref().expect("named field");
        let field_vis = &f.vis;
        let field_attrs = clean_attrs(&f.attrs);
        let field_ty = &f.ty;

        field_idents.push(field_ident);

        partial_fields.push(quote! {
            #(#field_attrs)*
            #field_vis #field_ident: ::core::option::Option<<#field_ty as crate::patch::ConfigPatch>::Patch>,
        });

        merge_arms.push(quote! {
            if let ::core::option::Option::Some(__patch) = #field_ident {
                crate::patch::ConfigPatch::merge(&mut self.#field_ident, __patch);
            }
        });
    }

    quote! {
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::cmp::PartialEq,
            ::serde::Serialize,
            ::serde::Deserialize,
            ::core::default::Default
        )]
        #(#struct_attrs)*
        #vis struct #partial_name {
            #(#partial_fields)*
        }

        impl crate::patch::ConfigPatch for #name {
            type Patch = #partial_name;

            fn merge(&mut self, __patch: #partial_name) {
                let #partial_name { #(#field_idents),* } = __patch;
                #(#merge_arms)*
            }

            fn from_patch(__patch: #partial_name) -> Self
            where
                Self: ::core::default::Default,
            {
                let mut __target = Self::default();
                __target.merge(__patch);
                __target
            }
        }
    }
}
