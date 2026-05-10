use thiserror::Error;

pub type AmanResult<T> = Result<T, AmanError>;

#[derive(Debug, Error)]
pub enum AmanError {
    #[error("event bus is full")]
    BusFull,
    #[error("operation timed out")]
    Timeout,
    #[error("version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: String, found: String },
    #[error("dependency cycle detected: {path}")]
    CycleDetected { path: String },
    #[error("compensation failed: {message}")]
    CompensationFailed { message: String },
    #[error("unrecoverable error: {message}")]
    Unrecoverable { message: String },
    #[error("invalid configuration: {message}")]
    ConfigInvalid { message: String },
    #[error("secret could not be resolved: {key}")]
    SecretUnresolved { key: String },
    #[error("invalid retry backoff: {value}")]
    InvalidRetryBackoff { value: String },
    #[error("macro usage is invalid: {message}")]
    MacroUsage { message: String },
    #[error("invalid state transition: {message}")]
    InvalidStateTransition { message: String },
    #[error("resource already exists: {name}")]
    AlreadyExists { name: String },
    #[error("resource not found: {name}")]
    NotFound { name: String },
    #[error("permission denied: {message}")]
    PermissionDenied { message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("serde json error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl AmanError {
    #[must_use]
    pub fn config_invalid(message: impl Into<String>) -> Self {
        Self::ConfigInvalid {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AmanError;

    #[test]
    fn config_invalid_helper_preserves_message() {
        let error = AmanError::config_invalid("bad config");
        assert_eq!(error.to_string(), "invalid configuration: bad config");
    }

    #[test]
    fn io_error_converts_into_aman_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let error: AmanError = io_error.into();
        assert_eq!(error.to_string(), "missing");
    }

    #[test]
    fn serde_json_error_converts_into_aman_error() {
        let json_error = serde_json::from_str::<serde_json::Value>("{not-json}")
            .expect_err("invalid json should fail");
        let error: AmanError = json_error.into();
        assert!(
            error.to_string().contains("key must be a string"),
            "unexpected serde error message: {error}"
        );
    }

    #[test]
    fn formatted_errors_include_context() {
        let error = AmanError::VersionMismatch {
            expected: "1.0.0".to_owned(),
            found: "0.9.0".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "version mismatch: expected 1.0.0, found 0.9.0"
        );
    }
}
