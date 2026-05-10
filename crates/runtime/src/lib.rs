#![forbid(unsafe_code)]
#![doc = "Placeholder crate for the Aman agent framework."]

/// Returns the crate name for smoke tests and early integration wiring.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}
