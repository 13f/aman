use kernel::event::Event;
use kernel::types::Timestamp;
use kernel::{AmanResult, Error};
use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub struct DeadLetterEntry {
    pub id: String,
    pub event: Event,
    pub reason: String,
    pub retry_count: u32,
    pub original_retry_count: u32,
    pub enqueued_at: Timestamp,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlqRetryRecord {
    pub operator: String,
    pub timestamp: Timestamp,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DlqFilter {
    pub reason: Option<String>,
    pub source: Option<String>,
    pub event_type: Option<String>,
    pub limit: Option<usize>,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlqExpiryAlert {
    pub id: String,
    pub threshold_days: u32,
    pub days_remaining: u32,
}

pub trait DeadLetterQueue: Send + Sync {
    fn enqueue(&self, event: Event, reason: impl Into<String>, ttl_days: u64) -> AmanResult<String>;
    fn list(&self, filter: DlqFilter) -> AmanResult<Vec<DeadLetterEntry>>;
    fn retry(
        &self,
        id: &str,
        operator: impl Into<String>,
        reason: impl Into<String>,
    ) -> AmanResult<Event>;
    fn discard(&self, id: &str) -> AmanResult<DeadLetterEntry>;
    fn expiry_alerts(&self, now: Timestamp) -> AmanResult<Vec<DlqExpiryAlert>>;
    fn run_expiry(&self, now: Timestamp) -> AmanResult<usize>;
}

#[derive(Debug)]
pub struct InMemoryDeadLetterQueue {
    entries: Mutex<BTreeMap<String, DeadLetterEntry>>,
    archived: Mutex<Vec<DeadLetterEntry>>,
    retry_records: Mutex<BTreeMap<String, Vec<DlqRetryRecord>>>,
    alerted_thresholds: Mutex<BTreeMap<String, HashSet<u32>>>,
    max_manual_retries: u32,
}

impl Default for InMemoryDeadLetterQueue {
    fn default() -> Self {
        Self::new(5)
    }
}

impl InMemoryDeadLetterQueue {
    #[must_use]
    pub fn new(max_manual_retries: u32) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            archived: Mutex::new(Vec::new()),
            retry_records: Mutex::new(BTreeMap::new()),
            alerted_thresholds: Mutex::new(BTreeMap::new()),
            max_manual_retries,
        }
    }

    #[must_use]
    pub fn archived_entries(&self) -> Vec<DeadLetterEntry> {
        self.archived
            .lock()
            .expect("dlq archived mutex should not be poisoned")
            .clone()
    }

    #[must_use]
    pub fn retry_history(&self, id: &str) -> Vec<DlqRetryRecord> {
        self.retry_records
            .lock()
            .expect("dlq retry history mutex should not be poisoned")
            .get(id)
            .cloned()
            .unwrap_or_default()
    }
}

impl DeadLetterQueue for InMemoryDeadLetterQueue {
    fn enqueue(&self, event: Event, reason: impl Into<String>, ttl_days: u64) -> AmanResult<String> {
        let id = event.id.to_string();
        let now = Timestamp::now();
        let ttl_ms = ttl_days
            .saturating_mul(24)
            .saturating_mul(60)
            .saturating_mul(60)
            .saturating_mul(1_000);
        let expires_at = Timestamp::from_millis(now.as_millis().saturating_add(ttl_ms as i64));
        let entry = DeadLetterEntry {
            id: id.clone(),
            event,
            reason: reason.into(),
            retry_count: 0,
            original_retry_count: 0,
            enqueued_at: now,
            expires_at,
        };
        self.entries
            .lock()
            .expect("dlq entries mutex should not be poisoned")
            .insert(id.clone(), entry);
        Ok(id)
    }

    fn list(&self, filter: DlqFilter) -> AmanResult<Vec<DeadLetterEntry>> {
        let entries = self
            .entries
            .lock()
            .expect("dlq entries mutex should not be poisoned");
        let mut items = entries
            .values()
            .filter(|entry| {
                filter
                    .reason
                    .as_ref()
                    .is_none_or(|reason| entry.reason.eq_ignore_ascii_case(reason))
            })
            .filter(|entry| {
                filter
                    .source
                    .as_ref()
                    .is_none_or(|source| entry.event.source.as_str().eq_ignore_ascii_case(source))
            })
            .filter(|entry| {
                filter.event_type.as_ref().is_none_or(|event_type| {
                    entry
                        .event
                        .event_type
                        .as_str()
                        .eq_ignore_ascii_case(event_type)
                })
            })
            .cloned()
            .collect::<Vec<_>>();

        items.sort_by_key(|entry| entry.enqueued_at.as_millis());
        let start = filter.offset.min(items.len());
        let limit = filter.limit.unwrap_or(items.len().saturating_sub(start));
        Ok(items.into_iter().skip(start).take(limit).collect())
    }

    fn retry(
        &self,
        id: &str,
        operator: impl Into<String>,
        reason: impl Into<String>,
    ) -> AmanResult<Event> {
        let mut entries = self
            .entries
            .lock()
            .expect("dlq entries mutex should not be poisoned");
        let entry = entries.get_mut(id).ok_or_else(|| Error::NotFound {
            name: format!("dlq:{id}"),
        })?;
        if entry.retry_count >= self.max_manual_retries {
            return Err(Error::Unrecoverable {
                message: format!(
                    "DLQ entry `{id}` exceeded max_manual_retries={}",
                    self.max_manual_retries
                ),
            });
        }

        let previous = entry.retry_count;
        entry.original_retry_count = previous;
        entry.retry_count = 0;

        let record = DlqRetryRecord {
            operator: operator.into(),
            timestamp: Timestamp::now(),
            reason: reason.into(),
        };
        self.retry_records
            .lock()
            .expect("dlq retry history mutex should not be poisoned")
            .entry(id.to_owned())
            .or_default()
            .push(record);

        Ok(entry.event.clone())
    }

    fn discard(&self, id: &str) -> AmanResult<DeadLetterEntry> {
        let mut entries = self
            .entries
            .lock()
            .expect("dlq entries mutex should not be poisoned");
        let removed = entries.remove(id).ok_or_else(|| Error::NotFound {
            name: format!("dlq:{id}"),
        })?;
        self.alerted_thresholds
            .lock()
            .expect("dlq alert mutex should not be poisoned")
            .remove(id);
        Ok(removed)
    }

    fn expiry_alerts(&self, now: Timestamp) -> AmanResult<Vec<DlqExpiryAlert>> {
        const THRESHOLDS: [u32; 3] = [7, 3, 1];
        const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
        let entries = self
            .entries
            .lock()
            .expect("dlq entries mutex should not be poisoned");
        let mut alerted = self
            .alerted_thresholds
            .lock()
            .expect("dlq alert mutex should not be poisoned");
        let mut alerts = Vec::new();

        for (id, entry) in entries.iter() {
            let remaining_ms = entry.expires_at.as_millis().saturating_sub(now.as_millis());
            if remaining_ms <= 0 {
                continue;
            }
            let days_remaining = u32::try_from(((remaining_ms - 1) / DAY_MS) + 1).unwrap_or(u32::MAX);
            for threshold in THRESHOLDS {
                if days_remaining <= threshold {
                    let emitted = alerted.entry(id.clone()).or_default();
                    if !emitted.contains(&threshold) {
                        emitted.insert(threshold);
                        alerts.push(DlqExpiryAlert {
                            id: id.clone(),
                            threshold_days: threshold,
                            days_remaining,
                        });
                    }
                }
            }
        }
        Ok(alerts)
    }

    fn run_expiry(&self, now: Timestamp) -> AmanResult<usize> {
        let mut entries = self
            .entries
            .lock()
            .expect("dlq entries mutex should not be poisoned");
        let expired_ids = entries
            .iter()
            .filter(|(_, entry)| entry.expires_at.as_millis() <= now.as_millis())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();

        let mut archived = self
            .archived
            .lock()
            .expect("dlq archived mutex should not be poisoned");
        for id in &expired_ids {
            if let Some(entry) = entries.remove(id) {
                archived.push(entry);
            }
            self.alerted_thresholds
                .lock()
                .expect("dlq alert mutex should not be poisoned")
                .remove(id);
        }
        Ok(expired_ids.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{DeadLetterQueue, DlqFilter, InMemoryDeadLetterQueue};
    use kernel::event::{Event, EventType};
    use kernel::types::Timestamp;
    use serde_json::json;

    fn test_event() -> Event {
        Event::new("pipeline:test", EventType::MessageReceived, json!({"id": 1}))
    }

    #[test]
    fn enqueue_and_list_support_filters() {
        let dlq = InMemoryDeadLetterQueue::default();
        let _ = dlq.enqueue(test_event(), "pipeline_failed", 30).expect("enqueue");
        let _ = dlq
            .enqueue(
                Event::new("pipeline:other", EventType::TimerTick, json!({"id": 2})),
                "timeout",
                30,
            )
            .expect("enqueue");

        let filtered = dlq
            .list(DlqFilter {
                reason: Some("pipeline_failed".to_owned()),
                ..DlqFilter::default()
            })
            .expect("list");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].reason, "pipeline_failed");
    }

    #[test]
    fn retry_records_operator_history_and_returns_event() {
        let dlq = InMemoryDeadLetterQueue::default();
        let id = dlq.enqueue(test_event(), "pipeline_failed", 30).expect("enqueue");

        let event = dlq
            .retry(&id, "alice", "manual replay")
            .expect("retry should succeed");
        assert_eq!(event.source.as_str(), "pipeline:test");
        let history = dlq.retry_history(&id);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].operator, "alice");
    }

    #[test]
    fn discard_removes_entry() {
        let dlq = InMemoryDeadLetterQueue::default();
        let id = dlq.enqueue(test_event(), "pipeline_failed", 30).expect("enqueue");

        let removed = dlq.discard(&id).expect("discard should succeed");
        assert_eq!(removed.id, id);
        assert!(dlq.list(DlqFilter::default()).expect("list").is_empty());
    }

    #[test]
    fn run_expiry_moves_expired_entries_to_archive() {
        let dlq = InMemoryDeadLetterQueue::default();
        let id = dlq.enqueue(test_event(), "pipeline_failed", 0).expect("enqueue");
        let now = Timestamp::from_millis(Timestamp::now().as_millis().saturating_add(1));

        let expired = dlq.run_expiry(now).expect("run expiry");
        assert_eq!(expired, 1);
        assert!(dlq.list(DlqFilter::default()).expect("list").is_empty());
        assert_eq!(dlq.archived_entries().len(), 1);
        assert_eq!(dlq.archived_entries()[0].id, id);
    }

    #[test]
    fn expiry_alerts_emit_7d_3d_1d_thresholds_once() {
        let dlq = InMemoryDeadLetterQueue::default();
        let id = dlq
            .enqueue(test_event(), "pipeline_failed", 10)
            .expect("enqueue");
        let now = Timestamp::now();
        let mut entries = dlq
            .entries
            .lock()
            .expect("entries lock");
        entries
            .get_mut(&id)
            .expect("entry exists")
            .expires_at = Timestamp::from_millis(now.as_millis() + 2 * 24 * 60 * 60 * 1_000);
        drop(entries);

        let first = dlq.expiry_alerts(now).expect("first alerts");
        assert_eq!(first.len(), 2);
        assert!(first.iter().any(|item| item.threshold_days == 7));
        assert!(first.iter().any(|item| item.threshold_days == 3));

        let second = dlq.expiry_alerts(now).expect("second alerts");
        assert!(second.is_empty());
    }
}
