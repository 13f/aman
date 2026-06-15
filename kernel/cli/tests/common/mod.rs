// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Shared helpers for CLI integration tests.

use std::path::PathBuf;

/// Return the path to the `aman-cli` binary.
///
/// Cargo sets `CARGO_BIN_EXE_aman_cli` when it builds the binary alongside the
/// tests, but it is not available when running a single integration test target
/// in isolation. This helper falls back to a path derived from
/// `CARGO_MANIFEST_DIR` and the active `PROFILE` so that individual tests can
/// still locate the freshly-built binary.
pub fn aman_cli_bin() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_aman_cli") {
        return PathBuf::from(path);
    }
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    path.push("../../target");
    path.push(profile);
    path.push("aman-cli");
    path
}
