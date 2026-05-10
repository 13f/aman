#![forbid(unsafe_code)]
#![doc = "Tauri desktop integration crate for Aman (M12 placeholder)."]

/// Returns the crate name for smoke tests and early integration wiring.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}
