use kernel::types::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: String,
    pub timestamp_ms: i64,
    pub operator: String,
    pub action: String,
    pub target: String,
    pub outcome: String,
    pub detail: String,
}

pub struct AuditLogger {
    inner: Mutex<VecDeque<AuditRecord>>,
    cap: usize,
}

impl AuditLogger {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            cap: cap.max(1),
        }
    }

    pub fn record(
        &self,
        operator: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
        outcome: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let timestamp_ms = Timestamp::now().as_millis();
        let id = format!("{timestamp_ms}-{}", uuid::Uuid::now_v7());
        let mut inner = self.inner.lock().expect("audit log mutex");
        inner.push_back(AuditRecord {
            id,
            timestamp_ms,
            operator: operator.into(),
            action: action.into(),
            target: target.into(),
            outcome: outcome.into(),
            detail: detail.into(),
        });
        while inner.len() > self.cap {
            inner.pop_front();
        }
    }

    #[must_use]
    pub fn list(
        &self,
        action: Option<&str>,
        operator: Option<&str>,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        offset: usize,
        limit: usize,
    ) -> Vec<AuditRecord> {
        let inner = self.inner.lock().expect("audit log mutex");
        inner
            .iter()
            .filter(|item| action.is_none_or(|a| item.action.eq_ignore_ascii_case(a)))
            .filter(|item| operator.is_none_or(|o| item.operator.eq_ignore_ascii_case(o)))
            .filter(|item| since_ms.is_none_or(|s| item.timestamp_ms >= s))
            .filter(|item| until_ms.is_none_or(|u| item.timestamp_ms <= u))
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }
}
