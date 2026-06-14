// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! `#[derive(Noop)]` — generates a no-op `PluginExportRegistrar` impl.
//!
//! The `plugin` crate's `PluginExportRegistrar` trait has 10 methods
//! (5 `register_*` + 5 `unregister_*` pairs, one per export kind). A
//! registrar that does nothing — `NoopPluginRegistrar` — needs 40
//! lines of boilerplate to spell out the trait, and a typo in any
//! method name would silently miss the trait. This derive replaces
//! the boilerplate with one line:
//!
//! ```ignore
//! #[derive(Default, macros::Noop)]
//! pub struct NoopPluginRegistrar;
//! ```
//!
//! The generated impl is identical to a hand-written one — all 10
//! methods take their arguments by value/borrow, ignore them, and
//! return `Ok(())`. The trait is referenced as `crate::PluginExportRegistrar`
//! so the derive only works inside the `plugin` crate (where the
//! trait is defined). A future generalization could accept the
//! trait path as an attribute argument.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{parse2, ItemStruct};

/// Expand `#[derive(Noop)]` on a struct into a no-op
/// `PluginExportRegistrar` impl.
pub fn expand(input: TokenStream) -> TokenStream {
    let parsed = match parse2::<ItemStruct>(input) {
        Ok(s) => s,
        Err(e) => return e.to_compile_error(),
    };

    let name = &parsed.ident;
    let (impl_generics, ty_generics, where_clause) = parsed.generics.split_for_impl();

    quote! {
        impl #impl_generics crate::PluginExportRegistrar for #name #ty_generics #where_clause {
            fn register_skill(&self, _skill: ::std::sync::Arc<dyn kernel::skill::Skill>) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn unregister_skill(&self, _skill_name: &str) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn register_tool(&self, _tool: ::std::sync::Arc<dyn kernel::tool::Tool>) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn unregister_tool(&self, _tool_name: &str) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn register_event_source(&self, _source: ::std::sync::Arc<dyn kernel::source::EventSource>) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn unregister_event_source(&self, _source_id: &str) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn register_hook(&self, _hook: ::std::sync::Arc<dyn kernel::hook::Hook>) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn unregister_hook(&self, _hook_name: &str) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn register_memory_provider(&self, _provider: ::std::sync::Arc<dyn kernel::memory::MemoryProvider>) -> kernel::AmanResult<()> {
                Ok(())
            }
            fn unregister_memory_provider(&self, _provider_name: &str) -> kernel::AmanResult<()> {
                Ok(())
            }
        }
    }
}
