#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use proc_macro::TokenStream;

mod config_patch;
mod noop;
mod plugin;
mod skill;

/// Derive `ConfigPatch` for a struct, generating a `Partial*` patch type and a
/// recursive `merge` implementation. See [`config_patch`] for details.
#[proc_macro_derive(ConfigPatch)]
pub fn config_patch_derive(input: TokenStream) -> TokenStream {
    config_patch::expand(input.into()).into()
}

#[proc_macro_attribute]
pub fn skill(attr: TokenStream, item: TokenStream) -> TokenStream {
    skill::expand(attr.into(), item.into()).into()
}

#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    plugin::expand(attr.into(), item.into()).into()
}

/// Generate a no-op `PluginExportRegistrar` impl for the annotated
/// struct. See [`noop`] for details.
#[proc_macro_derive(Noop)]
pub fn noop_derive(input: TokenStream) -> TokenStream {
    noop::expand(input.into()).into()
}
