#![no_main]

use libfuzzer_sys::fuzz_target;

/// Fuzz `redact_sensitive_data` with arbitrary bytes to ensure it never panics.
///
/// The redactor handles arbitrary UTF-8 input; the key invariant is that
/// it must never panic regardless of what input it receives.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = kernel::redactor::redact_sensitive_data(s);
        // libfuzzer automatically detects panics.
    }
});
