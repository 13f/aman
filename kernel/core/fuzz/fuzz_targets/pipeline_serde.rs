#![no_main]

use libfuzzer_sys::fuzz_target;

/// Fuzz arbitrary JSON data against serde_json to ensure deserialization
/// of untrusted data never panics.
///
/// Also validates that round-trip (deserialize → serialize → deserialize)
/// does not crash.
fuzz_target!(|data: &[u8]| {
    // Attempt to deserialize arbitrary bytes as JSON.
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(data) {
        // Round-trip: serialize then deserialize should succeed.
        if let Ok(roundtripped) = serde_json::to_vec(&json) {
            let _ = serde_json::from_slice::<serde_json::Value>(&roundtripped);
        }
    }
    // libfuzzer automatically detects panics.
});
