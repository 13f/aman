// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::types::BackpressureLevel;
use thiserror::Error;

pub type AmanResult<T> = Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("[AmanExistence] event bus is full")]
    BusFull,
    #[error("[AmanExistence] publish blocked by backpressure at level {level:?}")]
    BackpressureBlocked { level: BackpressureLevel },
    #[error("[AmanExistence] operation timed out")]
    Timeout,
    #[error("[AmanExistence] version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: String, found: String },
    #[error("[AmanExistence] dependency cycle detected: {path}")]
    CycleDetected { path: String },
    #[error("[AmanExistence] compensation failed: {message}")]
    CompensationFailed { message: String },
    #[error("[AmanExistence] unrecoverable error: {message}")]
    Unrecoverable { message: String },
    #[error("[AmanExistence] invalid configuration: {message}")]
    ConfigInvalid { message: String },
    #[error("[AmanExistence] secret could not be resolved: {key}")]
    SecretUnresolved { key: String },
    #[error("[AmanExistence] invalid retry backoff: {value}")]
    InvalidRetryBackoff { value: String },
    #[error("[AmanExistence] macro usage is invalid: {message}")]
    MacroUsage { message: String },
    #[error("[AmanExistence] invalid state transition: {message}")]
    InvalidStateTransition { message: String },
    #[error("[AmanExistence] resource already exists: {name}")]
    AlreadyExists { name: String },
    #[error("[AmanExistence] resource not found: {name}")]
    NotFound { name: String },
    #[error("[AmanExistence] permission denied: {message}")]
    PermissionDenied { message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("[AmanExistence] serde json error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl Error {
    #[must_use]
    pub fn config_invalid(message: impl Into<String>) -> Self {
        Self::ConfigInvalid {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;
    use crate::types::BackpressureLevel;

    #[test]
    fn provenance_prefix_present_in_all_variants() {
        // Every non-transparent error must contain the provenance marker.
        assert!(Error::BusFull.to_string().contains("[AmanExistence]"));
        assert!(Error::Timeout.to_string().contains("[AmanExistence]"));
        assert!(Error::NotFound { name: "x".into() }.to_string().contains("[AmanExistence]"));
        assert!(Error::ConfigInvalid { message: "bad".into() }.to_string().contains("[AmanExistence]"));
    }

    #[test]
    fn config_invalid_helper_preserves_message() {
        let error = Error::config_invalid("bad config");
        assert_eq!(
            error.to_string(),
            "[AmanExistence] invalid configuration: bad config"
        );
    }

    #[test]
    fn io_error_converts_into_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let error: Error = io_error.into();
        // Io variant uses #[error(transparent)], so no provenance prefix
        assert_eq!(error.to_string(), "missing");
    }

    #[test]
    fn serde_json_error_converts_into_error() {
        let json_error = serde_json::from_str::<serde_json::Value>("{not-json}")
            .expect_err("invalid json should fail");
        let error: Error = json_error.into();
        assert!(
            error.to_string().contains("key must be a string"),
            "unexpected serde error message: {error}"
        );
        assert!(error.to_string().contains("[AmanExistence]"));
    }

    #[test]
    fn formatted_errors_include_context() {
        let error = Error::VersionMismatch {
            expected: "1.0.0".to_owned(),
            found: "0.9.0".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "[AmanExistence] version mismatch: expected 1.0.0, found 0.9.0"
        );
    }

    #[test]
    fn backpressure_error_mentions_level() {
        let error = Error::BackpressureBlocked {
            level: BackpressureLevel::L3,
        };
        assert_eq!(
            error.to_string(),
            "[AmanExistence] publish blocked by backpressure at level L3"
        );
    }

    #[test]
    fn provenance_marker_type_exists() {
        // AmanExistence is a real type exported from lib.rs — removing it
        // breaks this test AND every crate that references it.
        let _marker = crate::AmanExistence;
        assert_eq!(crate::PROVENANCE, "AmanExistence");
    }
}
