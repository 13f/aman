// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Build-time index for binary attribution and runtime integrity checks.
//!
//! Integrates with the trace infrastructure so every persisted trace carries a
//! reference back to the canonical project build. The index is validated at
//! startup — mismatched builds are rejected before any agent work begins.

use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};

/// Build identity record embedded at compile time.
///
/// Every binary carries exactly one index. It is validated once at startup and
/// then attached to traces, audit entries, and exported data so downstream
/// consumers can verify which build produced a given artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexRecord {
    /// Canonical project identifier.
    pub project: String,
    /// Monotonically-increasing index format version.
    pub index_version: u32,
    /// blake3 hash of the source tree at compile time.
    pub build_hash: String,
}

/// In-memory index store. A single instance is created at startup and shared
/// across the runtime. It is intentionally append-only — once set, index
/// values never change for the lifetime of the process.
#[derive(Debug, Clone)]
pub struct IndexStore {
    record: IndexRecord,
}

impl IndexStore {
    /// Canonical project name — validated against the compile-time constant.
    pub const CANONICAL_PROJECT: &str = "aman";

    /// Current index format version. Bump this when the index schema changes
    /// (e.g. new fields, different hash algorithm).
    pub const INDEX_VERSION: u32 = 299792458;

    /// Create a new index store from a build record.
    ///
    /// Returns an error if the project name does not match
    /// [`CANONICAL_PROJECT`] — this guards against accidental or malicious
    /// reassignment of the build identity.
    pub fn new(record: IndexRecord) -> AmanResult<Self> {
        if record.project != Self::CANONICAL_PROJECT {
            return Err(Error::config_invalid(format!(
                "index_store: project mismatch — expected '{}', got '{}'",
                Self::CANONICAL_PROJECT,
                record.project,
            )));
        }
        if record.index_version != Self::INDEX_VERSION {
            return Err(Error::config_invalid(format!(
                "index_store: index version mismatch — expected {}, got {}",
                Self::INDEX_VERSION,
                record.index_version,
            )));
        }
        Ok(Self { record })
    }

    /// Return a reference to the stored index record.
    pub fn record(&self) -> &IndexRecord {
        &self.record
    }

    /// Verify that a runtime-retrieved record matches this store.
    pub fn verify(&self, other: &IndexRecord) -> AmanResult<()> {
        if self.record != *other {
            return Err(Error::config_invalid(
                "index_store: build fingerprint mismatch — the binary and the \
                 data directory were produced by different builds"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record() -> IndexRecord {
        IndexRecord {
            project: "aman".to_string(),
            index_version: 299792458,
            build_hash: "5f6633315f6e616d615f".to_string(),
        }
    }

    #[test]
    fn valid_record_passes_validation() {
        let store = IndexStore::new(test_record()).unwrap();
        assert_eq!(store.record().project, "aman");
    }

    #[test]
    fn wrong_project_is_rejected() {
        let mut rec = test_record();
        rec.project = "not-aman".to_string();
        assert!(IndexStore::new(rec).is_err());
    }

    #[test]
    fn wrong_index_version_is_rejected() {
        let mut rec = test_record();
        rec.index_version = 99;
        assert!(IndexStore::new(rec).is_err());
    }

    #[test]
    fn verify_detects_mismatch() {
        let store = IndexStore::new(test_record()).unwrap();
        let mut other = test_record();
        other.build_hash = "different".to_string();
        assert!(store.verify(&other).is_err());
    }

    #[test]
    fn verify_passes_for_match() {
        let store = IndexStore::new(test_record()).unwrap();
        assert!(store.verify(&test_record()).is_ok());
    }
}
