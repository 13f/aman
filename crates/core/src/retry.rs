use crate::error::Error;
use crate::types::CompensationStrategy;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub retry_backoff: RetryBackoff,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            retry_backoff: RetryBackoff::Exponential,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetryBackoff {
    Immediate,
    Fixed(u64),
    #[default]
    Exponential,
    Sequence(Vec<u64>),
}

impl RetryBackoff {
    #[must_use]
    pub fn first_delay(&self) -> Option<Duration> {
        match self {
            Self::Immediate => Some(Duration::from_millis(0)),
            Self::Fixed(delay_ms) => Some(Duration::from_millis(*delay_ms)),
            Self::Exponential => Some(Duration::from_millis(100)),
            Self::Sequence(delays) => delays
                .first()
                .map(|delay_ms| Duration::from_millis(*delay_ms)),
        }
    }
}

impl FromStr for RetryBackoff {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim();

        if normalized.eq_ignore_ascii_case("immediate") {
            return Ok(Self::Immediate);
        }

        if normalized.eq_ignore_ascii_case("exponential") {
            return Ok(Self::Exponential);
        }

        if let Some(delay) = normalized.strip_prefix("fixed:") {
            let delay_ms = delay
                .parse::<u64>()
                .map_err(|_| Error::InvalidRetryBackoff {
                    value: value.to_owned(),
                })?;
            return Ok(Self::Fixed(delay_ms));
        }

        if let Some(sequence) = normalized.strip_prefix("sequence:") {
            let delays = sequence
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| {
                    part.parse::<u64>().map_err(|_| Error::InvalidRetryBackoff {
                        value: value.to_owned(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            if delays.is_empty() {
                return Err(Error::InvalidRetryBackoff {
                    value: value.to_owned(),
                });
            }

            return Ok(Self::Sequence(delays));
        }

        Err(Error::InvalidRetryBackoff {
            value: value.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompensationContract {
    pub strategy: CompensationStrategy,
    pub idempotent: bool,
    pub timeout_sec: u64,
    pub retry_count: u32,
    pub retry_backoff: RetryBackoff,
    pub on_failure: Option<String>,
}

impl Default for CompensationContract {
    fn default() -> Self {
        Self {
            strategy: CompensationStrategy::ReverseOrder,
            idempotent: true,
            timeout_sec: 30,
            retry_count: 3,
            retry_backoff: RetryBackoff::Exponential,
            on_failure: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RetryBackoff;
    use std::str::FromStr;

    #[test]
    fn parses_fixed_backoff() {
        assert_eq!(
            RetryBackoff::from_str("fixed:250").unwrap(),
            RetryBackoff::Fixed(250)
        );
    }

    #[test]
    fn parses_sequence_backoff() {
        assert_eq!(
            RetryBackoff::from_str("sequence:100, 500, 2000").unwrap(),
            RetryBackoff::Sequence(vec![100, 500, 2000])
        );
    }

    #[test]
    fn rejects_invalid_backoff() {
        assert!(RetryBackoff::from_str("fixed:oops").is_err());
    }
}
