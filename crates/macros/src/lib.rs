#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use proc_macro::TokenStream;

mod plugin;
mod skill;

#[proc_macro_attribute]
pub fn skill(attr: TokenStream, item: TokenStream) -> TokenStream {
    skill::expand(attr.into(), item.into()).into()
}

#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    plugin::expand(attr.into(), item.into()).into()
}
