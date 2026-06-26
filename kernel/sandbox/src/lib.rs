// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Platform-independent sandbox for subprocess plugins.
//!
//! Applies OS-level isolation (Landlock on Linux, Seatbelt on macOS) to restrict
//! filesystem access, network access, and resource consumption of plugin processes.
//!
//! ## Safety
//!
//! Sandbox application uses `std::process::Command::pre_exec()` which runs in
//! the forked child before `exec()`. The code in the closure must be async-signal-safe.
//! Landlock syscalls are designed to be signal-safe. The macOS path uses environment
//! variable injection (not syscalls in the child).

pub mod linux;
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Platform-independent sandbox configuration for a child process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Directories the process is allowed to read.
    #[serde(default)]
    pub allowed_read_dirs: Vec<PathBuf>,

    /// Directories the process is allowed to read and write.
    #[serde(default)]
    pub allowed_write_dirs: Vec<PathBuf>,

    /// Whether network access is allowed. Default: false.
    #[serde(default)]
    pub network_allowed: bool,

    /// Whether spawning child processes is allowed. Default: false.
    #[serde(default)]
    pub process_spawn_allowed: bool,

    /// Maximum memory the process may allocate, in megabytes. 0 = unlimited.
    /// Default: 500 MB.
    #[serde(default = "default_max_memory_mb")]
    pub max_memory_mb: u64,
}

const fn default_max_memory_mb() -> u64 {
    500
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allowed_read_dirs: Vec::new(),
            allowed_write_dirs: Vec::new(),
            network_allowed: false,
            process_spawn_allowed: false,
            max_memory_mb: 500,
        }
    }
}

/// Errors that can occur during sandbox application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// Sandboxing is not supported on this platform.
    Unsupported,
    /// The sandbox mechanism is available but could not be applied.
    ApplicationFailed(String),
    /// The kernel is too old to support the required sandbox features.
    KernelTooOld { required: String, found: String },
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "sandbox not supported on this platform"),
            Self::ApplicationFailed(msg) => write!(f, "sandbox application failed: {msg}"),
            Self::KernelTooOld { required, found } => {
                write!(
                    f,
                    "kernel too old: requires {required}, found {found}"
                )
            }
        }
    }
}

/// Attempt to apply sandbox restrictions. Designed to be called from within
/// a `Command::pre_exec()` closure.
///
/// | Platform | Mechanism            | Applied in          |
/// |----------|---------------------|---------------------|
/// | Linux    | Landlock LSM (5.13+) | `pre_exec()` (child) |
/// | macOS    | Seatbelt profile    | env var (parent)    |
/// | Windows  | See `windows` module | Before spawn (parent)|
///
/// ## Platform Notes
///
/// **Windows:** `apply_sandbox()` is a no-op — Job Objects and AppContainer
/// must be set up in the parent process before `CreateProcess`. Use
/// [`windows::WindowsSandbox`] directly for Windows isolation.
///
/// # Errors
/// Returns `SandboxError` if sandbox application fails. Callers may choose
/// to log and continue (fail-open) or abort the process (fail-closed).
pub fn apply_sandbox(config: &SandboxConfig) -> Result<(), SandboxError> {
    #[cfg(target_os = "linux")]
    {
        linux::apply_landlock(config)
    }

    #[cfg(target_os = "macos")]
    {
        macos::apply_seatbelt(config)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = config;
        tracing::warn!("sandbox not supported on this platform — plugin will run unsandboxed");
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        let _ = config;
        tracing::warn!(
            "apply_sandbox() is a no-op on Windows — use windows::WindowsSandbox \
             in the parent process before Command::spawn() for Job Object + \
             AppContainer isolation"
        );
        Ok(())
    }
}

/// Safely apply sandbox restrictions to a [`std::process::Command`] via
/// its `pre_exec` hook. This is a convenience wrapper around
/// [`apply_sandbox`] that handles the unsafe `pre_exec` call.
///
/// Only effective on Linux and macOS. On Windows and unsupported platforms
/// this is a no-op (use [`windows::WindowsSandbox`] directly for Windows).
pub fn apply_to_command(command: &mut std::process::Command, config: &SandboxConfig) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::process::CommandExt;
        let sb_config = config.clone();
        // Safety: pre_exec runs after fork(), before exec(). Landlock
        // and Seatbelt operations are async-signal-safe on their
        // respective platforms.
        unsafe {
            command.pre_exec(move || {
                apply_sandbox(&sb_config)
                    .map_err(|e| std::io::Error::other(format!("sandbox: {e}")))
            });
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = config;
        let _ = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_config_default_memory_is_500mb() {
        let config = SandboxConfig::default();
        assert_eq!(config.max_memory_mb, 500);
    }

    #[test]
    fn sandbox_config_default_denies_network() {
        let config = SandboxConfig::default();
        assert!(!config.network_allowed);
    }

    #[test]
    fn sandbox_config_default_denies_process_spawn() {
        let config = SandboxConfig::default();
        assert!(!config.process_spawn_allowed);
    }

    #[test]
    fn sandbox_config_serde_roundtrip() {
        let config = SandboxConfig {
            allowed_read_dirs: vec![PathBuf::from("/tmp/plugin")],
            allowed_write_dirs: vec![PathBuf::from("/tmp/plugin")],
            network_allowed: false,
            process_spawn_allowed: false,
            max_memory_mb: 500,
        };

        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: SandboxConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.allowed_read_dirs, config.allowed_read_dirs);
        assert_eq!(deserialized.max_memory_mb, 500);
    }
}
