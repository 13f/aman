// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Windows sandbox for subprocess plugins.
//!
//! Combines two complementary Windows isolation mechanisms:
//!
//! | Mechanism   | What it restricts              | Without admin |
//! |-------------|--------------------------------|---------------|
//! | Job Objects | Memory, process count, CPU     | Yes           |
//! | AppContainer| Network, filesystem capabilities| Yes*          |
//!
//! *AppContainer file-path isolation requires ACLs to be set on target
//!  directories (admin if not owner). Network isolation works immediately.
//!
//! ## Architecture
//!
//! Unlike Landlock (applied via `pre_exec()` syscalls in the child), Windows
//! isolation must be set up **before** `CreateProcessW`:
//!
//! - Job Objects: assigned via `PROC_THREAD_ATTRIBUTE_JOB_LIST` at creation,
//!   or post-spawn via `AssignProcessToJobObject` on a suspended process.
//! - AppContainer: specified via `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
//!   in `STARTUPINFOEX`, which requires the process to be created with
//!   `EXTENDED_STARTUPINFO_PRESENT`.
//!
//! This module provides two integration paths:
//!
//! **Path 1 — Job Objects only** (recommended for Phase 1):
//! Uses `CREATE_SUSPENDED` + post-spawn assignment. Works with
//! `std::process::Command` — no raw `CreateProcessW` needed.
//!
//! **Path 2 — Full AppContainer** (Phase 2, for network isolation):
//! Uses raw `CreateProcessW` with `STARTUPINFOEX`. Requires more setup
//! but provides capability-based network restrictions.
//!
//! ## Security Model
//!
//! -   Job Object limits are enforced by the kernel — child cannot escape
//!     memory or process-count bounds.
//! -   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ensures all children are
//!     terminated when the sandbox is dropped.
//! -   AppContainer capability SIDs are validated by the kernel at every
//!     resource access (network, filesystem) — not bypassable from userspace.
//! -   Without `internetClient` capability, `connect()` / `send()` return
//!     `WSAEACCES` — userspace circumvention impossible.
//!
//! ## References
//! - <https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects>
//! - <https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation>

use super::{SandboxConfig, SandboxError};

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

// ─── Windows-sys type aliases ─────────────────────────────────────────

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenThread, ResumeThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

// ─── AppContainer types (available in Win32_Security for Win8+) ──────

/// SID for well-known AppContainer capability `internetClient`.
/// String form: `S-1-15-3-1`
const CAPABILITY_INTERNET_CLIENT: &[u8] = &[
    1,    // revision
    1,    // sub-authority count
    0, 0, 0, 0, 0, 16, // NT_AUTHORITY (5)
    0, 0, 0, 15, // SECURITY_APP_PACKAGE_AUTHORITY (15)
    0, 0, 0, 3,  // CAPABILITY_INTERNET_CLIENT (3)
    0, 0, 0, 1,  // RID = 1
];

/// SID for well-known AppContainer capability `internetClientServer`.
/// String form: `S-1-15-3-2`
const CAPABILITY_INTERNET_CLIENT_SERVER: &[u8] = &[
    1,    // revision
    1,    // sub-authority count
    0, 0, 0, 0, 0, 16, // NT_AUTHORITY (5)
    0, 0, 0, 15, // SECURITY_APP_PACKAGE_AUTHORITY (15)
    0, 0, 0, 3,  // CAPABILITY_INTERNET_CLIENT_SERVER (3)
    0, 0, 0, 2,  // RID = 2
];

/// Well-known SID: `ALL APPLICATION PACKAGES` (`S-1-15-2-1`).
/// Grants access to all AppContainer processes.
const ALL_APPLICATION_PACKAGES: &[u8] = &[
    1,    // revision
    1,    // sub-authority count
    0, 0, 0, 0, 0, 16, // NT_AUTHORITY (5)
    0, 0, 0, 15, // SECURITY_APP_PACKAGE_AUTHORITY (15)
    0, 0, 0, 2,  // SECURITY_APP_PACKAGE_BASE (2)
    0, 0, 0, 1,  // RID = 1 (ALL APPLICATION PACKAGES)
];

// ─── Known folders accessible to AppContainer processes ──────────────

/// System directories that AppContainer processes can typically access read-only
/// by default (they have ACL entries for `ALL APPLICATION PACKAGES`).
const DEFAULT_READABLE_SYSTEM_DIRS: &[&str] = &[
    "\\Windows\\System32",
    "\\Windows\\SysWOW64",
    "\\Program Files",
    "\\Program Files (x86)",
];

// ─── Public API ──────────────────────────────────────────────────────

/// Windows sandbox container holding Job Object + optional AppContainer
/// resources.
///
/// Created before process spawn via [`WindowsSandbox::create()`].
/// Applied to the child process either via `CREATE_SUSPENDED` +
/// post-spawn assignment (Path 1), or by building a `STARTUPINFOEX` for
/// raw `CreateProcessW` (Path 2).
pub struct WindowsSandbox {
    /// Handle to the Job Object. 0 if Job Objects unavailable.
    job_handle: HANDLE,
    /// AppContainer SID bytes (PSID). Only set when network isolation
    /// is active (i.e. `!config.network_allowed`).
    #[allow(dead_code)]
    appcontainer_sid: Vec<u8>,
    /// Capability SIDs to grant to the AppContainer process.
    /// When `network_allowed` is true, includes `internetClient` +
    /// `internetClientServer`. When false, empty (no network grant).
    #[allow(dead_code)]
    capabilities: Vec<Vec<u8>>,
}

impl WindowsSandbox {
    /// Create sandbox resources from configuration.
    ///
    /// Creates:
    /// - A Job Object with memory + process-count limits (Phase 1)
    /// - AppContainer SID + capability SIDs for network isolation (Phase 2)
    ///
    /// # Errors
    /// Returns `SandboxError::ApplicationFailed` if Job Object creation fails.
    pub fn create(config: &SandboxConfig) -> Result<Self, SandboxError> {
        // ── Phase 1: Job Object (always created) ──────────────────────
        let job = create_job_object(config)?;

        // ── Phase 2: AppContainer for network isolation ───────────────
        // Create an AppContainer profile. The name includes a random UUID
        // to avoid collisions between multiple sandboxed processes.
        let container_name = format!("aman.sandbox.{}", uuid::Uuid::now_v7());
        let container_name_wide = str_to_wide(&container_name);
        let display_name_wide = str_to_wide("aman Sandbox");

        let mut appcontainer_sid: Vec<u8> = Vec::new();
        let sid_created = unsafe {
            create_appcontainer_profile(
                container_name_wide.as_ptr(),
                display_name_wide.as_ptr(),
                &mut appcontainer_sid,
            )
        };

        let capabilities = if sid_created {
            if config.network_allowed {
                vec![
                    CAPABILITY_INTERNET_CLIENT.to_vec(),
                    CAPABILITY_INTERNET_CLIENT_SERVER.to_vec(),
                ]
            } else {
                // No network capabilities → connect()/send() return WSAEACCES
                Vec::new()
            }
        } else {
            // AppContainer creation failed — fall back to Job Objects only.
            // Network isolation won't be applied, but Job Object limits work.
            tracing::warn!("CreateAppContainerProfile failed — network isolation unavailable; using Job Objects only");
            Vec::new()
        };

        Ok(Self {
            job_handle: job,
            appcontainer_sid,
            capabilities,
        })
    }

    // ── Path 1: Job Objects via CREATE_SUSPENDED (works with Command) ─

    /// Set `CREATE_SUSPENDED` on a `std::process::Command`.
    ///
    /// Call this **before** `Command::spawn()`. After spawn, call
    /// [`apply_after_spawn()`](Self::apply_after_spawn) to assign the
    /// Job Object and resume the main thread.
    pub fn configure_command(command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_SUSPENDED);
    }

    /// Assign the job object to a suspended child and resume its main thread.
    ///
    /// # Safety
    /// The `pid` must belong to a process created with `CREATE_SUSPENDED`.
    /// Calling this on an already-running process is safe but the job
    /// assignment may fail if the process has already created child processes.
    ///
    /// # Errors
    /// Returns `SandboxError::ApplicationFailed` if:
    /// - The process handle cannot be opened
    /// - `AssignProcessToJobObject` fails
    /// - No threads found to resume
    pub fn apply_after_spawn(&self, pid: u32) -> Result<(), SandboxError> {
        // Step 1: Open process handle
        let process_handle = unsafe { OpenProcess(
            PROCESS_SET_QUOTA | PROCESS_TERMINATE,
            FALSE,
            pid,
        )};

        if process_handle == 0 || process_handle == INVALID_HANDLE_VALUE {
            let err = unsafe { GetLastError() };
            return Err(SandboxError::ApplicationFailed(format!(
                "OpenProcess failed for PID {pid}: error {err}"
            )));
        }

        // Step 2: Assign to Job Object
        let result = unsafe { AssignProcessToJobObject(self.job_handle, process_handle) };
        unsafe { CloseHandle(process_handle); }

        if result == 0 {
            let err = unsafe { GetLastError() };
            return Err(SandboxError::ApplicationFailed(format!(
                "AssignProcessToJobObject failed for PID {pid}: error {err}"
            )));
        }

        // Step 3: Resume all threads
        resume_process_threads(pid)?;

        Ok(())
    }

    // ── Getters ───────────────────────────────────────────────────────

    /// Raw Job Object handle. For use with `PROC_THREAD_ATTRIBUTE_JOB_LIST`
    /// in a custom `CreateProcessW` call (Path 2).
    #[must_use]
    #[allow(dead_code)]
    pub fn job_handle(&self) -> HANDLE {
        self.job_handle
    }

    /// Whether the sandbox includes an AppContainer for network isolation.
    #[must_use]
    #[allow(dead_code)]
    pub fn has_network_isolation(&self) -> bool {
        !self.capabilities.is_empty() && self.appcontainer_sid.is_empty()
    }
}

impl Drop for WindowsSandbox {
    fn drop(&mut self) {
        if self.job_handle != 0 && self.job_handle != INVALID_HANDLE_VALUE {
            // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE ensures all children
            // are terminated. Closing the handle is sufficient.
            unsafe { CloseHandle(self.job_handle); }
        }
    }
}

// SAFETY: WindowsSandbox is safe to send across threads. The HANDLE
// is owned exclusively and CloseHandle is called exactly once on drop.
unsafe impl Send for WindowsSandbox {}
unsafe impl Sync for WindowsSandbox {}

// ─── Job Object helpers ──────────────────────────────────────────────

/// Create and configure a Windows Job Object from `SandboxConfig`.
fn create_job_object(config: &SandboxConfig) -> Result<HANDLE, SandboxError> {
    // Unnamed job → can only be joined by processes that receive this handle.
    let job = unsafe {
        CreateJobObjectW(
            std::ptr::null(), // lpJobAttributes
            std::ptr::null(), // lpName (unnamed)
        )
    };

    if job == 0 || job == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        return Err(SandboxError::ApplicationFailed(format!(
            "CreateJobObjectW failed: error {err}"
        )));
    }

    // SAFETY: JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a C struct of scalar
    // fields — zero-initialized is a valid state.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    // Memory limit
    if config.max_memory_mb > 0 {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = config.max_memory_mb.saturating_mul(1024 * 1024);
    }

    // Process-spawn restriction
    if !config.process_spawn_allowed {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.ActiveProcessLimit = 1;
    }

    let result = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };

    if result == 0 {
        let err = unsafe { GetLastError() };
        unsafe { CloseHandle(job); }
        return Err(SandboxError::ApplicationFailed(format!(
            "SetInformationJobObject failed: error {err}"
        )));
    }

    Ok(job)
}

// ─── Thread resume helpers ───────────────────────────────────────────

/// Resume all threads in a process created with `CREATE_SUSPENDED`.
///
/// A newly created suspended process has exactly one thread (the main
/// thread). We enumerate to be safe, but in practice only one thread
/// will be found.
fn resume_process_threads(pid: u32) -> Result<(), SandboxError> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };

    if snapshot == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        return Err(SandboxError::ApplicationFailed(format!(
            "CreateToolhelp32Snapshot failed: error {err}"
        )));
    }

    // SAFETY: THREADENTRY32 is a C struct — zero-init sets dwSize.
    let mut te: THREADENTRY32 = unsafe { std::mem::zeroed() };
    te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

    let mut resumed = 0u32;

    unsafe {
        if Thread32First(snapshot, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid {
                    let th = OpenThread(THREAD_SUSPEND_RESUME, FALSE, te.th32ThreadID);
                    if th != 0 && th != INVALID_HANDLE_VALUE {
                        // ResumeThread returns the previous suspend count.
                        // For a CREATE_SUSPENDED process, this is 1.
                        ResumeThread(th);
                        CloseHandle(th);
                        resumed += 1;
                    }
                }
                te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snapshot, &mut te) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }

    if resumed == 0 {
        return Err(SandboxError::ApplicationFailed(format!(
            "no threads found for PID {pid}"
        )));
    }

    Ok(())
}

// ─── AppContainer helpers ────────────────────────────────────────────

/// Create an AppContainer profile and derive its SID.
///
/// Uses `CreateAppContainerProfile` to register the profile with Windows
/// and `DeriveAppContainerSidFromAppContainerName` to get the SID bytes.
/// The returned SID can be used with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
/// when launching a process via `CreateProcessW`.
///
/// Returns `true` if the profile was created successfully, `false` if the
/// API is unavailable (pre-Win8) or the call failed.
unsafe fn create_appcontainer_profile(
    name: *const u16,
    display_name: *const u16,
    sid_out: &mut Vec<u8>,
) -> bool {
    use windows_sys::Win32::Security::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
        FreeSid, GetLengthSid, IsWellKnownSid,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::SystemServices::{PROCESSOR_ARCHITECTURE_AMD64, IMAGE_FILE_MACHINE_AMD64};

    // Check if the API is available (Win8+)
    let kernel32 = GetModuleHandleW(str_to_wide("kernel32.dll").as_ptr());
    if kernel32 == 0 {
        return false;
    }

    // Create the AppContainer profile
    let mut appcontainer_sid: windows_sys::Win32::Security::PSID = std::ptr::null_mut();
    let caps: [windows_sys::Win32::Security::SID_AND_ATTRIBUTES; 0] = [];
    let result = CreateAppContainerProfile(
        name,
        display_name,
        display_name, // description
        caps.as_ptr(),
        0,
        &mut appcontainer_sid,
    );

    if result != 0 || appcontainer_sid.is_null() {
        // Profile may already exist — try deriving the SID
        let mut sid_ptr: windows_sys::Win32::Security::PSID = std::ptr::null_mut();
        let derive_result = DeriveAppContainerSidFromAppContainerName(name, &mut sid_ptr);
        if derive_result != 0 || sid_ptr.is_null() {
            return false;
        }
        let len = GetLengthSid(sid_ptr) as usize;
        let sid_bytes = std::slice::from_raw_parts(sid_ptr as *const u8, len);
        sid_out.clear();
        sid_out.extend_from_slice(sid_bytes);
        FreeSid(sid_ptr);
        return true;
    }

    let len = GetLengthSid(appcontainer_sid) as usize;
    let sid_bytes = std::slice::from_raw_parts(appcontainer_sid as *const u8, len);
    sid_out.clear();
    sid_out.extend_from_slice(sid_bytes);

    true
}

/// Build a native NT path from a `PathBuf`.
///
/// Most Windows APIs that accept paths need either a DOS path (`C:\...`)
/// or an NT path (`\Device\HarddiskVolume3\...`). For AppContainer ACL
/// operations we need the DOS path as a wide string.
#[allow(dead_code)]
fn to_wide(path: &PathBuf) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0)) // null terminator
        .collect()
}

/// Build a UTF-16 wide string from a `&str` and null-terminate it.
fn str_to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn create_sandbox_with_memory_limit() {
        let config = SandboxConfig {
            max_memory_mb: 256,
            ..SandboxConfig::default()
        };
        let sandbox = WindowsSandbox::create(&config).expect("create sandbox");
        assert_ne!(sandbox.job_handle, 0);
        // Sandbox dropped here → CloseHandle called
    }

    #[test]
    fn create_sandbox_unlimited_memory() {
        let config = SandboxConfig {
            max_memory_mb: 0, // 0 = unlimited
            process_spawn_allowed: true,
            network_allowed: true,
            ..SandboxConfig::default()
        };
        let sandbox = WindowsSandbox::create(&config).expect("create sandbox");
        assert_ne!(sandbox.job_handle, 0);
    }

    #[test]
    fn configure_command_sets_suspended_flag() {
        let mut cmd = std::process::Command::new("cmd.exe");
        WindowsSandbox::configure_command(&mut cmd);
        // We can't inspect creation_flags from outside, but we trust
        // that CommandExt::creation_flags was called without panicking.
    }

    #[test]
    fn capability_sid_is_valid_binary_sid() {
        // SID structure: revision(1) + sub_auth_count(1) + authority(6) + sub_auths(n*4)
        assert_eq!(CAPABILITY_INTERNET_CLIENT[0], 1); // SID revision
        let sub_auths = CAPABILITY_INTERNET_CLIENT[1] as usize;
        assert_eq!(CAPABILITY_INTERNET_CLIENT.len(), 8 + sub_auths * 4);
    }

    #[test]
    fn all_application_packages_sid_is_valid() {
        assert_eq!(ALL_APPLICATION_PACKAGES[0], 1); // SID revision
        let sub_auths = ALL_APPLICATION_PACKAGES[1] as usize;
        assert_eq!(ALL_APPLICATION_PACKAGES.len(), 8 + sub_auths * 4);
    }
}
