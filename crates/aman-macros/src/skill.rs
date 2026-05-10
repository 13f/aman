use proc_macro2::TokenStream;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Error, Item, Meta, Token};

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    match expand_inner(attr, item.clone()) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

fn expand_inner(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let args = parser.parse2(attr)?;

    if let Some(first) = args.first() {
        return Err(Error::new_spanned(
            first,
            "#[aman_skill] does not accept arguments in M1",
        ));
    }

    let parsed = syn::parse2::<Item>(item.clone())?;
    match parsed {
        Item::Fn(_) | Item::Impl(_) | Item::Struct(_) => Ok(item),
        other => Err(Error::new_spanned(
            other,
            "#[aman_skill] can only be applied to a function, impl block, or struct",
        )),
    }
}
