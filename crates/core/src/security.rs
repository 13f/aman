// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Security harness types: capability-based access control, approval persistence,
//! and sandbox configuration for the aman agent framework.

use crate::error::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Capabilities requested by a plugin. Each field represents a privilege
/// that must be explicitly approved by the operator before the plugin can
/// run. When a plugin is sandboxed (TrustLevel::Sandboxed), these caps
/// are enforced by the security harness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Whether the plugin can publish events to the event bus.
    #[serde(default)]
    pub can_publish_events: bool,

    /// Whether the plugin can subscribe to events from the event bus.
    #[serde(default)]
    pub can_subscribe_events: bool,

    /// Filesystem paths the plugin is allowed to read.
    #[serde(default)]
    pub allowed_read_paths: Vec<PathBuf>,

    /// Filesystem paths the plugin is allowed to read and write.
    #[serde(default)]
    pub allowed_write_paths: Vec<PathBuf>,

    /// Whether the plugin is allowed to make network connections.
    #[serde(default)]
    pub can_network: bool,

    /// Whether the plugin is allowed to spawn child processes.
    #[serde(default)]
    pub can_spawn_processes: bool,

    /// Maximum memory the plugin process may allocate, in megabytes.
    /// Default: 500 MB.
    #[serde(default = "default_max_memory_mb")]
    pub max_memory_mb: u64,

    /// Maximum CPU time the plugin may consume, in seconds.
    /// Default: 300 seconds (5 minutes).
    #[serde(default = "default_max_cpu_seconds")]
    pub max_cpu_seconds: u64,

    /// Maximum events the plugin may publish per second (token bucket rate).
    /// Default: 50 events/second.
    #[serde(default = "default_max_events_per_second")]
    pub max_events_per_second: f64,
}

const fn default_max_memory_mb() -> u64 {
    500
}

const fn default_max_cpu_seconds() -> u64 {
    300
}

fn default_max_events_per_second() -> f64 {
    50.0
}

// SAFETY: CapabilitySet's f64 fields (max_events_per_second) are always
// finite values set by the user or default (50.0). We never store NaN or
// infinity, so reflexivity of PartialEq holds and Eq is valid.
impl Eq for CapabilitySet {}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self {
            can_publish_events: false,
            can_subscribe_events: false,
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
            can_network: false,
            can_spawn_processes: false,
            max_memory_mb: default_max_memory_mb(),
            max_cpu_seconds: default_max_cpu_seconds(),
            max_events_per_second: default_max_events_per_second(),
        }
    }
}

impl CapabilitySet {
    /// Returns the list of human-readable capability names that are granted
    /// (i.e., boolean flags set to `true`).
    #[must_use]
    pub fn granted_boolean_caps(&self) -> Vec<&'static str> {
        let mut grants = Vec::new();
        if self.can_publish_events {
            grants.push("publish_events");
        }
        if self.can_subscribe_events {
            grants.push("subscribe_events");
        }
        if self.can_network {
            grants.push("network");
        }
        if self.can_spawn_processes {
            grants.push("spawn_processes");
        }
        grants
    }

    /// Returns a human-readable summary of all granted capabilities for
    /// display in approval prompts.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for cap in self.granted_boolean_caps() {
            lines.push(format!("  - {cap}"));
        }
        if !self.allowed_read_paths.is_empty() {
            lines.push(format!(
                "  - read paths: {:?}",
                self.allowed_read_paths
            ));
        }
        if !self.allowed_write_paths.is_empty() {
            lines.push(format!(
                "  - write paths: {:?}",
                self.allowed_write_paths
            ));
        }
        lines.push(format!("  - max memory: {} MB", self.max_memory_mb));
        lines.push(format!(
            "  - max CPU time: {} seconds",
            self.max_cpu_seconds
        ));
        lines.push(format!(
            "  - max events/sec: {}",
            self.max_events_per_second
        ));
        lines
    }

    /// Returns `true` if `other` is fully contained within `self` — i.e.,
    /// all capabilities requested by `other` are already granted by `self`.
    /// Used to determine whether a previously-approved capability set still
    /// covers the current request.
    #[must_use]
    pub fn contains(&self, other: &CapabilitySet) -> bool {
        // Boolean flags: if other requests it, self must have it
        if other.can_publish_events && !self.can_publish_events {
            return false;
        }
        if other.can_subscribe_events && !self.can_subscribe_events {
            return false;
        }
        if other.can_network && !self.can_network {
            return false;
        }
        if other.can_spawn_processes && !self.can_spawn_processes {
            return false;
        }

        // Path subsets: all paths other wants must be in self
        if !other
            .allowed_read_paths
            .iter()
            .all(|p| self.allowed_read_paths.contains(p))
        {
            return false;
        }
        if !other
            .allowed_write_paths
            .iter()
            .all(|p| self.allowed_write_paths.contains(p))
        {
            return false;
        }

        // Resource limits: other must not exceed self
        if other.max_memory_mb > self.max_memory_mb {
            return false;
        }
        if other.max_cpu_seconds > self.max_cpu_seconds {
            return false;
        }
        if other.max_events_per_second > self.max_events_per_second {
            return false;
        }

        true
    }

    /// Returns the capabilities in `other` that are NOT covered by `self`.
    /// Used to show the user what new capabilities are being requested.
    #[must_use]
    pub fn diff(&self, other: &CapabilitySet) -> CapabilitySet {
        CapabilitySet {
            can_publish_events: other.can_publish_events && !self.can_publish_events,
            can_subscribe_events: other.can_subscribe_events && !self.can_subscribe_events,
            allowed_read_paths: other
                .allowed_read_paths
                .iter()
                .filter(|p| !self.allowed_read_paths.contains(p))
                .cloned()
                .collect(),
            allowed_write_paths: other
                .allowed_write_paths
                .iter()
                .filter(|p| !self.allowed_write_paths.contains(p))
                .cloned()
                .collect(),
            can_network: other.can_network && !self.can_network,
            can_spawn_processes: other.can_spawn_processes && !self.can_spawn_processes,
            max_memory_mb: if other.max_memory_mb > self.max_memory_mb {
                other.max_memory_mb
            } else {
                self.max_memory_mb
            },
            max_cpu_seconds: if other.max_cpu_seconds > self.max_cpu_seconds {
                other.max_cpu_seconds
            } else {
                self.max_cpu_seconds
            },
            max_events_per_second: if other.max_events_per_second > self.max_events_per_second {
                other.max_events_per_second
            } else {
                self.max_events_per_second
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Approval persistence
// ---------------------------------------------------------------------------

/// Record of approved capabilities for a plugin, persisted to disk.
///
/// The payload (plugin_version + capabilities + metadata) is signed with
/// the runtime's secret key. The signature is stored alongside the data.
/// If the file is tampered with, the signature won't match and the
/// approval is rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedCapabilities {
    /// Plugin version at the time capabilities were approved.
    pub plugin_version: String,
    /// The capabilities that were approved.
    pub capabilities: CapabilitySet,
    /// Unix timestamp (milliseconds) when the approval was granted.
    pub approved_at_ms: u64,
    /// Who or what granted the approval (e.g., "user", "auto", "admin").
    pub approved_by: String,
    /// BLAKE3 keyed hash (32 bytes, hex-encoded) of the payload.
    /// Computed as keyed_hash(secret_key, plugin_version || capabilities_json || approved_at_ms || approved_by).
    /// Set by `ApprovalCache::save()` and verified by `ApprovalCache::load()`.
    pub signature: String,
}

/// Manages persisted capability approvals with cryptographic integrity.
///
/// ## Storage location
/// Approvals are stored at `~/.aman/approvals/{plugin_name}.yaml` —
/// **outside** any plugin's directory, so sandboxed plugins cannot
/// modify them even with write access to their own install directory.
///
/// ## Integrity protection
/// Each approval is signed with a BLAKE3 keyed hash using a runtime
/// secret key. The key is generated once on first startup and stored
/// at `~/.aman/.security-key` with `0o600` permissions. If a plugin
/// or user manually edits an approval file, the signature won't match
/// and the plugin is rejected.
#[derive(Debug, Clone)]
pub struct ApprovalCache {
    /// Directory where approval files are stored
    /// (`~/.aman/approvals/`).
    approvals_dir: PathBuf,
    /// Secret key for HMAC signing of approval files (32 bytes).
    secret_key: [u8; 32],
}

impl ApprovalCache {
    /// Create a new approval cache.
    ///
    /// `aman_root` is the aman config directory (`~/.aman/`).
    /// The secret key is loaded from `{aman_root}/.security-key` or
    /// generated if it doesn't exist.
    pub fn new(aman_root: PathBuf) -> AmanResult<Self> {
        let approvals_dir = aman_root.join("approvals");
        let secret_key = Self::load_or_create_key(&aman_root)?;
        Ok(Self {
            approvals_dir,
            secret_key,
        })
    }

    // ── Secret key management ──────────────────────────────────

    /// Returns the path to the secret key file.
    fn key_path(aman_root: &Path) -> PathBuf {
        aman_root.join(".security-key")
    }

    /// Load the runtime secret key, or generate and persist a new one.
    fn load_or_create_key(aman_root: &Path) -> AmanResult<[u8; 32]> {
        let key_path = Self::key_path(aman_root);

        if key_path.exists() {
            let key_bytes = std::fs::read(&key_path)?;
            if key_bytes.len() != 32 {
                return Err(Error::ConfigInvalid {
                    message: format!(
                        "security key at {} is corrupt (expected 32 bytes, got {})",
                        key_path.display(),
                        key_bytes.len()
                    ),
                });
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            return Ok(key);
        }

        // Generate a new random key from the OS CSPRNG
        let mut key = [0u8; 32];
        // Use /dev/urandom on Unix, or fall back to UUID-based entropy
        #[cfg(unix)]
        {
            use std::io::Read;
            let mut f = std::fs::File::open("/dev/urandom").map_err(|e| {
                Error::Unrecoverable {
                    message: format!("failed to open /dev/urandom for security key: {e}"),
                }
            })?;
            f.read_exact(&mut key).map_err(|e| Error::Unrecoverable {
                message: format!("failed to read security key from /dev/urandom: {e}"),
            })?;
        }
        #[cfg(not(unix))]
        {
            // Fallback: derive entropy from multiple UUID v7 + timestamp
            let mut seed = Vec::new();
            for _ in 0..4 {
                seed.extend_from_slice(uuid::Uuid::now_v7().as_bytes());
            }
            let hash = blake3::hash(&seed);
            key.copy_from_slice(&hash.as_bytes()[..32]);
        }

        // Persist with restrictive permissions
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&key_path, &key)?;

        // Set permissions to owner-only (0o600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&key_path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&key_path, perms);
            }
        }

        tracing::info!(
            path = %key_path.display(),
            "generated new runtime security key"
        );

        Ok(key)
    }

    // ── Signature helpers ──────────────────────────────────────

    /// Path to the approval file for a given plugin.
    #[doc(hidden)]
    pub fn approval_path(&self, plugin_name: &str) -> PathBuf {
        self.approvals_dir.join(format!("plugin__{plugin_name}.yaml"))
    }

    /// Compute a keyed hash over the approval payload (everything
    /// except the signature field itself).
    fn compute_signature(&self, caps: &ApprovedCapabilities) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.secret_key);
        hasher.update(caps.plugin_version.as_bytes());
        hasher.update(b"\x00");
        // Serialize capabilities deterministically for signing
        let caps_json =
            serde_json::to_string(&caps.capabilities).unwrap_or_default();
        hasher.update(caps_json.as_bytes());
        hasher.update(b"\x00");
        hasher.update(&caps.approved_at_ms.to_le_bytes());
        hasher.update(b"\x00");
        hasher.update(caps.approved_by.as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    // ── Public API ─────────────────────────────────────────────

    /// Load previously-approved capabilities for a plugin.
    /// Returns `None` if no approval file exists (first-time load).
    /// Returns an error if the signature is invalid (tampered file).
    pub fn load(&self, plugin_name: &str) -> AmanResult<Option<ApprovedCapabilities>> {
        let path = self.approval_path(plugin_name);
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path)?;
        let caps: ApprovedCapabilities =
            serde_json::from_str(&content).map_err(|error| {
                Error::ConfigInvalid {
                    message: format!(
                        "corrupt approval file for plugin '{}': {}",
                        plugin_name, error
                    ),
                }
            })?;

        // ── Verify signature ────────────────────────────────
        let expected = self.compute_signature(&caps);
        if caps.signature != expected {
            tracing::error!(
                plugin = %plugin_name,
                path = %path.display(),
                "approval file signature mismatch — file has been tampered with"
            );
            return Err(Error::SecurityViolation {
                message: format!(
                    "approval file for plugin '{}' has been tampered with (signature mismatch). \
                     Delete {} and re-approve the plugin to continue.",
                    plugin_name,
                    path.display()
                ),
            });
        }

        Ok(Some(caps))
    }

    /// Persist approved capabilities for a plugin.
    ///
    /// The signature is computed and set automatically before writing.
    /// Callers should leave `caps.signature` empty; it will be populated.
    pub fn save(
        &self,
        plugin_name: &str,
        caps: &mut ApprovedCapabilities,
    ) -> AmanResult<()> {
        // Always compute a fresh signature before persisting
        caps.signature = self.compute_signature(caps);

        let path = self.approval_path(plugin_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(caps).map_err(|error| {
            Error::ConfigInvalid {
                message: format!("failed to serialize approval: {}", error),
            }
        })?;

        // Write atomically: temp file then rename
        let tmp_path = path.with_extension("tmp");
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        tracing::info!(
            plugin = %plugin_name,
            path = %path.display(),
            "capability approval saved with signature"
        );

        Ok(())
    }

    /// Check whether the requested capabilities are already approved.
    ///
    /// Returns:
    /// - `Ok(None)` — already approved (requested caps are subset of approved caps,
    ///   and plugin version matches). No re-approval needed.
    /// - `Ok(Some(caps))` — not yet approved, or version changed, or new caps needed.
    ///   `caps` is the set of capabilities that need approval.
    pub fn check_approval(
        &self,
        plugin_name: &str,
        requested: &CapabilitySet,
        plugin_version: &semver::Version,
    ) -> AmanResult<Option<CapabilitySet>> {
        let existing = self.load(plugin_name)?;
        match existing {
            Some(approved) => {
                if approved.plugin_version == plugin_version.to_string()
                    && approved.capabilities.contains(requested)
                {
                    // Same version, no new caps — auto-approve
                    Ok(None)
                } else {
                    // Version changed or new caps — compute what's new
                    let new_caps = approved.capabilities.diff(requested);
                    Ok(Some(new_caps))
                }
            }
            None => {
                // First time — all requested caps need approval
                Ok(Some(requested.clone()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn capability_set_contains_subset() {
        let base = CapabilitySet {
            can_publish_events: true,
            can_network: true,
            max_memory_mb: 500,
            max_events_per_second: 50.0,
            ..CapabilitySet::default()
        };

        let subset = CapabilitySet {
            can_publish_events: true,
            max_memory_mb: 200,
            max_events_per_second: 30.0,
            ..CapabilitySet::default()
        };

        assert!(base.contains(&subset));
    }

    #[test]
    fn capability_set_rejects_escalation() {
        let base = CapabilitySet {
            max_memory_mb: 200,
            ..CapabilitySet::default()
        };

        let escalation = CapabilitySet {
            max_memory_mb: 600,
            ..CapabilitySet::default()
        };

        assert!(!base.contains(&escalation));
    }

    #[test]
    fn capability_set_rejects_new_boolean_flag() {
        let base = CapabilitySet::default();
        let escalation = CapabilitySet {
            can_network: true,
            ..CapabilitySet::default()
        };
        assert!(!base.contains(&escalation));
    }

    #[test]
    fn diff_identifies_new_capabilities() {
        let old = CapabilitySet::default();
        let new = CapabilitySet {
            can_publish_events: true,
            allowed_read_paths: vec![PathBuf::from("/data")],
            max_memory_mb: 500,
            ..CapabilitySet::default()
        };

        let diff = old.diff(&new);
        assert!(diff.can_publish_events);
        assert_eq!(diff.allowed_read_paths, vec![PathBuf::from("/data")]);
        assert_eq!(diff.max_memory_mb, 500);
    }

    #[test]
    fn approval_cache_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "aman-approval-test-{}",
            uuid::Uuid::now_v7()
        ));
        let cache = ApprovalCache::new(tmp.clone()).expect("create cache");

        let caps = CapabilitySet {
            can_publish_events: true,
            max_memory_mb: 500,
            ..CapabilitySet::default()
        };

        let mut approved = ApprovedCapabilities {
            plugin_version: "1.0.0".to_owned(),
            capabilities: caps.clone(),
            approved_at_ms: 1000,
            approved_by: "test".to_owned(),
            signature: String::new(),
        };

        cache.save("test-plugin", &mut approved).expect("save");
        let loaded = cache.load("test-plugin").expect("load").expect("exists");
        assert_eq!(loaded.plugin_version, "1.0.0");
        assert!(loaded.capabilities.can_publish_events);

        // Clean up
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_approval_auto_approves_when_superset() {
        let tmp = std::env::temp_dir().join(format!(
            "aman-approval-auto-{}",
            uuid::Uuid::now_v7()
        ));
        let cache = ApprovalCache::new(tmp.clone()).expect("create cache");

        let mut approved = ApprovedCapabilities {
            plugin_version: "1.0.0".to_owned(),
            capabilities: CapabilitySet {
                can_publish_events: true,
                max_memory_mb: 500,
                ..CapabilitySet::default()
            },
            approved_at_ms: 1000,
            approved_by: "test".to_owned(),
            signature: String::new(),
        };
        cache.save("test-plugin", &mut approved).expect("save");

        // Same requested caps should auto-approve
        let requested = CapabilitySet {
            can_publish_events: true,
            max_memory_mb: 300,
            ..CapabilitySet::default()
        };
        let version = semver::Version::new(1, 0, 0);
        let result = cache
            .check_approval("test-plugin", &requested, &version)
            .expect("check");
        assert!(result.is_none(), "should auto-approve: no new caps");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_approval_requires_reapproval_for_new_caps() {
        let tmp = std::env::temp_dir().join(format!(
            "aman-approval-re-{}",
            uuid::Uuid::now_v7()
        ));
        let cache = ApprovalCache::new(tmp.clone()).expect("create cache");

        let mut approved = ApprovedCapabilities {
            plugin_version: "1.0.0".to_owned(),
            capabilities: CapabilitySet::default(),
            approved_at_ms: 1000,
            approved_by: "test".to_owned(),
            signature: String::new(),
        };
        cache.save("test-plugin", &mut approved).expect("save");

        let requested = CapabilitySet {
            can_network: true,
            ..CapabilitySet::default()
        };
        let version = semver::Version::new(1, 0, 0);
        let result = cache
            .check_approval("test-plugin", &requested, &version)
            .expect("check");
        assert!(result.is_some(), "should require re-approval for new caps");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_approval_requires_reapproval_for_version_change() {
        let tmp = std::env::temp_dir().join(format!(
            "aman-approval-ver-{}",
            uuid::Uuid::now_v7()
        ));
        let cache = ApprovalCache::new(tmp.clone()).expect("create cache");

        let mut approved = ApprovedCapabilities {
            plugin_version: "1.0.0".to_owned(),
            capabilities: CapabilitySet::default(),
            approved_at_ms: 1000,
            approved_by: "test".to_owned(),
            signature: String::new(),
        };
        cache.save("test-plugin", &mut approved).expect("save");

        let requested = CapabilitySet::default();
        let version = semver::Version::new(2, 0, 0);
        let result = cache
            .check_approval("test-plugin", &requested, &version)
            .expect("check");
        assert!(
            result.is_some(),
            "should require re-approval for version change"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn tampered_approval_file_is_rejected() {
        let tmp = std::env::temp_dir().join(format!(
            "aman-approval-tamper-{}",
            uuid::Uuid::now_v7()
        ));
        let cache = ApprovalCache::new(tmp.clone()).expect("create cache");

        let mut approved = ApprovedCapabilities {
            plugin_version: "1.0.0".to_owned(),
            capabilities: CapabilitySet {
                can_publish_events: true,
                max_memory_mb: 500,
                ..CapabilitySet::default()
            },
            approved_at_ms: 1000,
            approved_by: "test".to_owned(),
            signature: String::new(),
        };
        cache.save("test-plugin", &mut approved).expect("save");

        // Verify normal load works
        assert!(cache.load("test-plugin").expect("load").is_some());

        // Tamper: manually rewrite the file with escalated capabilities
        // but keep the old (now invalid) signature
        let tampered = ApprovedCapabilities {
            plugin_version: "1.0.0".to_owned(),
            capabilities: CapabilitySet {
                can_publish_events: true,
                can_network: true,              // ESCALATED: network access!
                can_spawn_processes: true,       // ESCALATED: process spawn!
                max_memory_mb: 99999,           // ESCALATED: unlimited memory!
                ..CapabilitySet::default()
            },
            approved_at_ms: 1000,
            approved_by: "test".to_owned(),
            signature: approved.signature.clone(), // old signature
        };

        let path = cache.approval_path("test-plugin");
        std::fs::write(&path, serde_json::to_string_pretty(&tampered).unwrap())
            .expect("write tampered file");

        // Load should reject due to signature mismatch
        let err = cache.load("test-plugin").expect_err("tampered file should be rejected");
        assert!(
            err.to_string().contains("tampered"),
            "error should mention tampering, got: {err}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn first_time_load_requires_approval() {
        let tmp = std::env::temp_dir().join(format!(
            "aman-approval-first-{}",
            uuid::Uuid::now_v7()
        ));
        let cache = ApprovalCache::new(tmp.clone()).expect("create cache");

        let requested = CapabilitySet {
            can_publish_events: true,
            ..CapabilitySet::default()
        };
        let version = semver::Version::new(1, 0, 0);
        let result = cache
            .check_approval("new-plugin", &requested, &version)
            .expect("check");
        assert!(
            result.is_some(),
            "first-time load should require approval"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn capability_set_default_memory_is_500mb() {
        let caps = CapabilitySet::default();
        assert_eq!(caps.max_memory_mb, 500);
    }

    #[test]
    fn capability_set_default_events_per_second_is_50() {
        let caps = CapabilitySet::default();
        assert!((caps.max_events_per_second - 50.0).abs() < f64::EPSILON);
    }
}
