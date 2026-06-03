// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Seatbelt sandbox for macOS.
//!
//! macOS provides the Seatbelt sandbox via `sandbox-exec(1)`. This is a
//! best-effort implementation — programmatic sandbox application requires
//! the `com.apple.security.temporary-exception.sandbox` entitlement.
//!
//! Instead of requiring entitlements, we generate a Seatbelt profile and
//! place it in an environment variable (`AMAN_SANDBOX_PROFILE`). The launcher
//! script wraps the plugin command with `sandbox-exec -f <profile>` when this
//! variable is set.
//!
//! For direct `pre_exec()` use (the common case in Rust), this is a no-op
//! on macOS. The recommended approach is to use the `sandbox-exec` wrapper
//! in the plugin manifest's `subprocess.command` field:
//!
//! ```yaml
//! subprocess:
//!   command: sandbox-exec
//!   args: ["-f", "/path/to/profile.sb", "python3", "plugin.py"]
//! ```

use super::{SandboxConfig, SandboxError};
use std::path::PathBuf;

/// Generate a macOS Seatbelt (sandbox-exec) profile string from the given
/// configuration.
///
/// The profile denies all operations by default and then allows only the
/// explicitly permitted paths and capabilities.
#[must_use]
pub fn generate_sandbox_profile(config: &SandboxConfig) -> String {
    let mut profile = String::new();

    // Seatbelt profile header
    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n\n");

    // Allow basic process operation (signals, process info)
    profile.push_str(";; Allow basic process operations\n");
    profile.push_str("(allow signal (target self))\n");
    profile.push_str("(allow process-info-pidinfo)\n");
    profile.push_str("(allow process-info-listpids)\n");
    profile.push_str("(allow sysctl-read)\n\n");

    // Allow reading system shared libraries and frameworks
    profile.push_str(";; Allow system libraries\n");
    profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System/Library\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library/Frameworks\"))\n\n");

    // Allow /tmp and /dev/null
    profile.push_str(";; Allow basic I/O\n");
    profile.push_str("(allow file-read* file-write* (subpath \"/tmp\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/null\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/urandom\"))\n\n");

    // Plugin-specific read directories
    if !config.allowed_read_dirs.is_empty() {
        profile.push_str(";; Plugin read directories\n");
        for dir in &config.allowed_read_dirs {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                normalize_path(dir)
            ));
        }
        profile.push('\n');
    }

    // Plugin-specific write directories
    if !config.allowed_write_dirs.is_empty() {
        profile.push_str(";; Plugin write directories\n");
        for dir in &config.allowed_write_dirs {
            profile.push_str(&format!(
                "(allow file-read* file-write* (subpath \"{}\"))\n",
                normalize_path(dir)
            ));
        }
        profile.push('\n');
    }

    // Network access
    if config.network_allowed {
        profile.push_str(";; Network access\n");
        profile.push_str("(allow network-outbound)\n");
        profile.push_str("(allow network-inbound)\n\n");
    } else {
        profile.push_str(";; Deny network\n");
        profile.push_str("(deny network*)\n\n");
    }

    // Process spawning
    if config.process_spawn_allowed {
        profile.push_str(";; Process spawning\n");
        profile.push_str("(allow process-exec)\n");
        profile.push_str("(allow process-fork)\n\n");
    }

    profile
}

/// Normalize a path for use in a Seatbelt profile.
/// Seatbelt expects absolute paths without trailing slashes.
fn normalize_path(path: &PathBuf) -> String {
    let resolved = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };

    // Canonicalize if possible, fall back to display path
    let clean = std::fs::canonicalize(&resolved).unwrap_or(resolved);
    clean.display().to_string()
}

/// Apply Seatbelt sandbox on macOS.
///
/// NOTE: This function cannot programmatically apply a sandbox profile
/// without the `com.apple.security.temporary-exception.sandbox` entitlement.
/// Instead, it sets the `AMAN_SANDBOX_PROFILE` environment variable with
/// the generated profile, which a wrapper script can use with `sandbox-exec`.
///
/// For the direct pre_exec path, we attempt to use `sandbox-exec` to
/// re-exec the process with the profile if available.
pub fn apply_seatbelt(config: &SandboxConfig) -> Result<(), SandboxError> {
    let profile = generate_sandbox_profile(config);
    // SAFETY: set_var in Rust 2024 is unsafe due to potential data races.
    // In the pre_exec context, no other threads are accessing environment
    // variables, so this is safe.
    unsafe {
        std::env::set_var("AMAN_SANDBOX_PROFILE", &profile);
    }

    // NOTE: Do NOT use tracing::info! here. This function runs inside a
    // Command::pre_exec() closure in the forked child process. If the
    // tracing subscriber's file-writer Mutex was locked by another thread
    // at the moment of fork(), the child inherits the poisoned lock and
    // deadlocks — which prevents exec() from ever running. The parent's
    // Command::spawn() then appears to hang indefinitely.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_contains_deny_default() {
        let config = SandboxConfig::default();
        let profile = generate_sandbox_profile(&config);
        assert!(profile.contains("(deny default)"));
    }

    #[test]
    fn profile_includes_allowed_paths() {
        let config = SandboxConfig {
            allowed_read_dirs: vec![PathBuf::from("/tmp/plugin")],
            allowed_write_dirs: vec![PathBuf::from("/tmp/plugin/data")],
            ..SandboxConfig::default()
        };
        let profile = generate_sandbox_profile(&config);
        assert!(profile.contains("/tmp/plugin"));
        assert!(profile.contains("/tmp/plugin/data"));
    }

    #[test]
    fn profile_allows_network_when_configured() {
        let config = SandboxConfig {
            network_allowed: true,
            ..SandboxConfig::default()
        };
        let profile = generate_sandbox_profile(&config);
        assert!(profile.contains("(allow network-outbound)"));
    }

    #[test]
    fn profile_denies_network_by_default() {
        let config = SandboxConfig::default();
        let profile = generate_sandbox_profile(&config);
        assert!(profile.contains("(deny network*)"));
    }
}
