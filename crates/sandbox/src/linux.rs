// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Landlock sandbox for Linux (kernel 5.13+).
//!
//! # Safety
//!
//! This module uses raw libc syscalls to invoke Landlock. All unsafe blocks
//! are confined to well-defined, small scopes with explicit safety comments.
//! Landlock syscalls are async-signal-safe by design and are the only way to
//! apply Landlock rulesets from userspace (no higher-level Rust API exists).
//! The sandbox crate overrides the workspace `deny(unsafe_code)` lint.
//!
//! On non-Linux platforms, most items here are conditionally compiled away.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports, unused_variables))]
//!
//! Landlock is a Linux Security Module that allows unprivileged processes to
//! restrict their own access to the filesystem (and network, since kernel 6.7).
//! It is designed to be composable and safe — restrictions are irreversible
//! once applied, and each restriction can only narrow access, never widen it.
//!
//! ## References
//! - <https://docs.kernel.org/userspace-api/landlock.html>
//! - <https://landlock.io/>

use super::{SandboxConfig, SandboxError};
use std::path::Path;

// Landlock ABI versions (stable since Linux 5.13)
mod abi {
    pub const V1: u32 = 1;
    // V2 (Linux 5.19) is not yet stabilized in the kernel ABI docs
    pub const V3: u32 = 3;
    pub const V4: u32 = 4;
}

// Landlock access right flags for filesystem (ABI V1-V4)
mod access {
    pub const FS_EXECUTE: u64 = 1 << 0;
    pub const FS_WRITE_FILE: u64 = 1 << 1;
    pub const FS_READ_FILE: u64 = 1 << 2;
    pub const FS_READ_DIR: u64 = 1 << 3;
    pub const FS_REMOVE_DIR: u64 = 1 << 4;
    pub const FS_REMOVE_FILE: u64 = 1 << 5;
    pub const FS_MAKE_CHAR: u64 = 1 << 6;
    pub const FS_MAKE_DIR: u64 = 1 << 7;
    pub const FS_MAKE_REG: u64 = 1 << 8;
    pub const FS_MAKE_SOCK: u64 = 1 << 9;
    pub const FS_MAKE_FIFO: u64 = 1 << 10;
    pub const FS_MAKE_BLOCK: u64 = 1 << 11;
    pub const FS_MAKE_SYM: u64 = 1 << 12;
    pub const FS_REFER: u64 = 1 << 13;
    pub const FS_TRUNCATE: u64 = 1 << 14;

    /// All read access needed: read file content + list directory contents.
    pub const FS_READ: u64 = FS_READ_FILE | FS_READ_DIR;
    /// All write access needed: create/write/remove/truncate.
    pub const FS_RW: u64 = FS_READ
        | FS_WRITE_FILE
        | FS_REMOVE_DIR
        | FS_REMOVE_FILE
        | FS_MAKE_DIR
        | FS_MAKE_REG
        | FS_TRUNCATE;

    /// The default deny set: restrict everything we know about.
    pub fn default_handled() -> u64 {
        FS_EXECUTE
            | FS_WRITE_FILE
            | FS_READ_FILE
            | FS_READ_DIR
            | FS_REMOVE_DIR
            | FS_REMOVE_FILE
            | FS_MAKE_CHAR
            | FS_MAKE_DIR
            | FS_MAKE_REG
            | FS_MAKE_SOCK
            | FS_MAKE_FIFO
            | FS_MAKE_BLOCK
            | FS_MAKE_SYM
            | FS_REFER
            | FS_TRUNCATE
    }
}

/// Query the Landlock ABI version supported by the running kernel.
fn landlock_abi_version() -> u32 {
    // Landlock ABI is exposed via prctl(PR_GET_LANDLOCK_ABI) or
    // by trying to create a ruleset with version=N and checking EINVAL.
    // We probe by attempting to create a ruleset with descending ABI versions.

    // Since we can't call prctl directly without libc bindings that may not
    // be available, we detect by checking /proc/sys/kernel/unprivileged_bpf_disabled
    // or simply probe the syscall. On kernels without Landlock, the syscall
    // returns ENOSYS.
    //
    // For safety, we use a simple detection: try creating a ruleset with V4,
    // fall back to V3, V2, V1. This is done in apply_landlock.

    // NOTE: The actual syscall probe requires raw syscalls. We use the
    // `libc` crate approach via the syscall number.
    // Landlock syscall numbers: x86_64=444, aarch64=444, arm=444 (consistent)

    #[cfg(target_arch = "x86_64")]
    const LANDLOCK_SYSCALL: isize = 444;
    #[cfg(target_arch = "aarch64")]
    const LANDLOCK_SYSCALL: isize = 444;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const LANDLOCK_SYSCALL: isize = -1;

    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        // Probe: try creating a minimal ruleset with V4
        let ruleset_attr = landlock_ruleset_attr {
            handled_access_fs: 0,
            handled_access_net: 0,
        };

        let fd = unsafe {
            libc::syscall(
                LANDLOCK_SYSCALL,
                0, // LANDLOCK_CMD_CREATE_RULESET = 0
                &ruleset_attr as *const _,
                std::mem::size_of::<landlock_ruleset_attr>(),
                0, // flags
            )
        };

        if fd >= 0 {
            // Clean up
            let _ = unsafe { libc::close(fd as i32) };
            // We got a valid fd — kernel supports Landlock. Return V4.
            return abi::V4;
        }

        let errno = -fd as i32;
        match errno {
            libc::ENOSYS => 0,       // Landlock not compiled into kernel
            libc::EOPNOTSUPP => 0,   // Landlock disabled
            libc::EINVAL => {
                // Try V3
                let ruleset_attr_v3 = landlock_ruleset_attr {
                    handled_access_fs: 0,
                    handled_access_net: 0,
                };
                let fd3 = unsafe {
                    libc::syscall(
                        LANDLOCK_SYSCALL,
                        0,
                        &ruleset_attr_v3 as *const _,
                        std::mem::size_of::<landlock_ruleset_attr>(),
                        0,
                    )
                };
                if fd3 >= 0 {
                    let _ = unsafe { libc::close(fd3 as i32) };
                    return abi::V3;
                }
                // Try V1
                let attr_v1_size = std::mem::size_of::<u64>() * 1; // only handled_access_fs
                let fd1 = unsafe {
                    libc::syscall(
                        LANDLOCK_SYSCALL,
                        0,
                        &access::default_handled() as *const u64 as *const std::ffi::c_void,
                        attr_v1_size,
                        0,
                    )
                };
                if fd1 >= 0 {
                    let _ = unsafe { libc::close(fd1 as i32) };
                    return abi::V1;
                }
                0 // can't determine
            }
            _ => 0,
        }
    }

    #[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
    {
        0
    }
}

// Landlock ruleset attribute structure
#[repr(C)]
struct landlock_ruleset_attr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

// Landlock path beneath rule attribute
#[repr(C)]
struct landlock_path_beneath_attr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Apply Landlock filesystem restrictions.
///
/// This function is designed to be called from within a `pre_exec()` closure.
/// It uses raw `libc` syscalls which are async-signal-safe. Once applied,
/// the restrictions are irreversible for the process and all its descendants.
///
/// # Errors
/// Returns `SandboxError` if:
/// - Landlock is not supported (kernel < 5.13)
/// - The ruleset creation fails
/// - Any path rule cannot be applied
pub fn apply_landlock(config: &SandboxConfig) -> Result<(), SandboxError> {
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let abi = landlock_abi_version();
        if abi == 0 {
            return Err(SandboxError::KernelTooOld {
                required: "Linux 5.13+ with Landlock enabled".to_owned(),
                found: "Landlock not available".to_owned(),
            });
        }

        // Step 1: Create a ruleset that handles all filesystem access rights
        let handled = access::default_handled();
        let ruleset_attr = landlock_ruleset_attr {
            handled_access_fs: handled,
            handled_access_net: 0, // network Landlock requires 6.7
        };

        let ruleset_fd = create_ruleset(&ruleset_attr, 0)?;

        // Step 2: Add path-beneath rules for allowed directories
        for dir in &config.allowed_read_dirs {
            add_path_rule(ruleset_fd, dir, access::FS_READ)?;
        }
        for dir in &config.allowed_write_dirs {
            add_path_rule(ruleset_fd, dir, access::FS_RW)?;
        }

        // Step 3: Enforce the ruleset (irreversible from this point)
        enforce_ruleset(ruleset_fd)?;

        // Close the ruleset fd (no longer needed after enforcement)
        unsafe {
            libc::close(ruleset_fd);
        }

        tracing::info!(
            read_dirs = config.allowed_read_dirs.len(),
            write_dirs = config.allowed_write_dirs.len(),
            "Landlock sandbox applied successfully"
        );

        Ok(())
    }

    #[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
    {
        let _ = config;
        Err(SandboxError::Unsupported)
    }
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn create_ruleset(
    attr: &landlock_ruleset_attr,
    flags: u32,
) -> Result<i32, SandboxError> {
    const LANDLOCK_SYSCALL: isize = 444;
    let fd = unsafe {
        libc::syscall(
            LANDLOCK_SYSCALL,
            0, // CREATE_RULESET
            attr as *const _,
            std::mem::size_of::<landlock_ruleset_attr>(),
            flags as usize,
        )
    };
    if fd < 0 {
        let errno = -fd;
        return Err(SandboxError::ApplicationFailed(format!(
            "landlock_create_ruleset failed: errno={errno}"
        )));
    }
    Ok(fd as i32)
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn add_path_rule(
    ruleset_fd: i32,
    path: &Path,
    access: u64,
) -> Result<(), SandboxError> {
    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    // Open the directory to get an fd
    let dir_fd = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(path)
    {
        Ok(f) => f.into_raw_fd(),
        Err(e) => {
            return Err(SandboxError::ApplicationFailed(format!(
                "cannot open '{}' for Landlock rule: {e}",
                path.display()
            )));
        }
    };

    let path_attr = landlock_path_beneath_attr {
        allowed_access: access,
        parent_fd: dir_fd,
    };

    const LANDLOCK_SYSCALL: isize = 444;
    let result = unsafe {
        libc::syscall(
            LANDLOCK_SYSCALL,
            1, // ADD_RULE
            ruleset_fd as usize,
            1, // LANDLOCK_RULE_PATH_BENEATH
            &path_attr as *const _,
            0, // flags
        )
    };

    // Close the dir fd regardless of success
    unsafe {
        libc::close(dir_fd);
    }

    if result != 0 {
        let errno = -result as i32;
        return Err(SandboxError::ApplicationFailed(format!(
            "landlock_add_rule failed for '{}': errno={errno}",
            path.display()
        )));
    }

    Ok(())
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn enforce_ruleset(ruleset_fd: i32) -> Result<(), SandboxError> {
    // PR_SET_NO_NEW_PRIVS = 36, must be called before enforcing Landlock
    // (prevents privilege escalation via setuid binaries)
    let result = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if result != 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return Err(SandboxError::ApplicationFailed(format!(
            "prctl(PR_SET_NO_NEW_PRIVS) failed: errno={errno}"
        )));
    }

    const LANDLOCK_SYSCALL: isize = 444;
    let result = unsafe {
        libc::syscall(
            LANDLOCK_SYSCALL,
            2, // RESTRICT_SELF
            ruleset_fd as usize,
            0, // flags
        )
    };

    if result != 0 {
        let errno = -result as i32;
        return Err(SandboxError::ApplicationFailed(format!(
            "landlock_restrict_self failed: errno={errno}"
        )));
    }

    Ok(())
}
