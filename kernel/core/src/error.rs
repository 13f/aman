// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::types::BackpressureLevel;
use thiserror::Error;

pub type AmanResult<T> = Result<T, Error>;

/// Semantic category for message-bearing error variants.
///
/// Can be used to programmatically inspect error *categories* without
/// matching on variant names or parsing display strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    CompensationFailed,
    Unrecoverable,
    ConfigInvalid,
    MacroUsage,
    PermissionDenied,
    SecurityViolation,
    SandboxError,
    InvalidStateTransition,
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompensationFailed => write!(f, "compensation failed"),
            Self::Unrecoverable => write!(f, "unrecoverable error"),
            Self::ConfigInvalid => write!(f, "invalid configuration"),
            Self::MacroUsage => write!(f, "macro usage is invalid"),
            Self::PermissionDenied => write!(f, "permission denied"),
            Self::SecurityViolation => write!(f, "security violation"),
            Self::SandboxError => write!(f, "sandbox error"),
            Self::InvalidStateTransition => write!(f, "invalid state transition"),
        }
    }
}

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
    #[error("[AmanExistence] rate limited: source {source_id}, retry after {retry_after_ms}ms")]
    RateLimited { source_id: String, retry_after_ms: u64 },
    #[error("[AmanExistence] security violation: {message}")]
    SecurityViolation { message: String },
    #[error("[AmanExistence] sandbox error: {message}")]
    SandboxError { message: String },
}

impl Error {
    #[must_use]
    pub fn config_invalid(message: impl Into<String>) -> Self {
        Self::ConfigInvalid {
            message: message.into(),
        }
    }

    /// Return the semantic [`ErrorKind`] for message-bearing variants.
    ///
    /// Returns `None` for structural variants (`NotFound`, `Io`, etc.)
    /// whose semantics are already encoded in their variant names.
    #[must_use]
    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::CompensationFailed { .. } => Some(ErrorKind::CompensationFailed),
            Self::Unrecoverable { .. } => Some(ErrorKind::Unrecoverable),
            Self::ConfigInvalid { .. } => Some(ErrorKind::ConfigInvalid),
            Self::MacroUsage { .. } => Some(ErrorKind::MacroUsage),
            Self::PermissionDenied { .. } => Some(ErrorKind::PermissionDenied),
            Self::SecurityViolation { .. } => Some(ErrorKind::SecurityViolation),
            Self::SandboxError { .. } => Some(ErrorKind::SandboxError),
            Self::InvalidStateTransition { .. } => Some(ErrorKind::InvalidStateTransition),
            _ => None,
        }
    }

    /// Return the i18n key for this error variant's user-facing description.
    ///
    /// The base `[AmanExistence]` prefix and structural variants (`Io`, `SerdeJson`)
    /// return `None` since they either have a fixed prefix or are not user-facing.
    /// Error variants that carry a user-visible message return the corresponding
    /// `i18n::key::ERROR_*` constant so the caller can produce a localized message.
    #[must_use]
    pub fn i18n_key(&self) -> Option<&'static str> {
        match self {
            Self::BusFull => Some(i18n::key::ERROR_BUS_FULL),
            Self::Timeout => Some(i18n::key::ERROR_TIMEOUT),
            Self::VersionMismatch { .. } => Some(i18n::key::ERROR_VERSION_MISMATCH),
            Self::CycleDetected { .. } => Some(i18n::key::ERROR_CYCLE_DETECTED),
            Self::CompensationFailed { .. } => Some(i18n::key::ERROR_COMPENSATION_FAILED),
            Self::Unrecoverable { .. } => Some(i18n::key::ERROR_UNRECOVERABLE),
            Self::ConfigInvalid { .. } => Some(i18n::key::ERROR_CONFIG_INVALID),
            Self::SecretUnresolved { .. } => Some(i18n::key::ERROR_SECRET_UNRESOLVED),
            Self::InvalidRetryBackoff { .. } => Some(i18n::key::ERROR_INVALID_RETRY_BACKOFF),
            Self::AlreadyExists { .. } => Some(i18n::key::ERROR_ALREADY_EXISTS),
            Self::NotFound { .. } => Some(i18n::key::ERROR_NOT_FOUND),
            Self::RateLimited { .. } => Some(i18n::key::ERROR_RATE_LIMITED),
            Self::SecurityViolation { .. } => Some(i18n::key::ERROR_SECURITY_VIOLATION),
            Self::SandboxError { .. } => Some(i18n::key::ERROR_SANDBOX),
            // Structural variants (Io, SerdeJson) and MacroUsage/PermissionDenied/
            // InvalidStateTransition do not have a dedicated user-facing message.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, ErrorKind};
    use crate::types::BackpressureLevel;

    #[test]
    fn provenance_prefix_present_in_all_variants() {
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
    fn error_kind_is_accessible() {
        let error = Error::config_invalid("test");
        assert_eq!(error.kind(), Some(ErrorKind::ConfigInvalid));
        assert_eq!(Error::NotFound { name: "x".into() }.kind(), None);
    }

    #[test]
    fn io_error_converts_into_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let error: Error = io_error.into();
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
        let _marker = crate::AmanExistence;
        assert_eq!(crate::PROVENANCE, "AmanExistence");
    }
}
