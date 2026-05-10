#![forbid(unsafe_code)]

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
