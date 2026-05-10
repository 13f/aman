use crate::types::{DedupKey, DeliveryGuarantee, Priority, SourceId, Timestamp, TraceId};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventType {
    FileCreated,
    FileChanged,
    FileDeleted,
    CronTick,
    TimerTick,
    Heartbeat,
    MessageReceived,
    WebhookReceived,
    SystemSignal,
    WorkflowStateChanged,
    SkillLoaded,
    SkillReloaded,
    Custom(String),
}

impl EventType {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::FileCreated => "file_created",
            Self::FileChanged => "file_changed",
            Self::FileDeleted => "file_deleted",
            Self::CronTick => "cron_tick",
            Self::TimerTick => "timer_tick",
            Self::Heartbeat => "heartbeat",
            Self::MessageReceived => "message_received",
            Self::WebhookReceived => "webhook_received",
            Self::SystemSignal => "system_signal",
            Self::WorkflowStateChanged => "workflow_state_changed",
            Self::SkillLoaded => "skill_loaded",
            Self::SkillReloaded => "skill_reloaded",
            Self::Custom(value) => value.as_str(),
        }
    }
}

impl From<String> for EventType {
    fn from(value: String) -> Self {
        match value.as_str() {
            "file_created" => Self::FileCreated,
            "file_changed" => Self::FileChanged,
            "file_deleted" => Self::FileDeleted,
            "cron_tick" => Self::CronTick,
            "timer_tick" => Self::TimerTick,
            "heartbeat" => Self::Heartbeat,
            "message_received" => Self::MessageReceived,
            "webhook_received" => Self::WebhookReceived,
            "system_signal" => Self::SystemSignal,
            "workflow_state_changed" => Self::WorkflowStateChanged,
            "skill_loaded" => Self::SkillLoaded,
            "skill_reloaded" => Self::SkillReloaded,
            _ => Self::Custom(value),
        }
    }
}

impl From<&str> for EventType {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

impl Serialize for EventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(D::Error::custom("event type cannot be empty"));
        }
        Ok(Self::from(value))
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMetadata {
    pub trace_id: TraceId,
    pub parent_event_id: Option<Uuid>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub ttl_ms: Option<u64>,
    pub lifespan_ms: Option<u64>,
    pub created_at: Timestamp,
}

impl EventMetadata {
    #[must_use]
    pub fn ttl(&self) -> Option<Duration> {
        self.ttl_ms.map(Duration::from_millis)
    }

    #[must_use]
    pub fn lifespan(&self) -> Option<Duration> {
        self.lifespan_ms.map(Duration::from_millis)
    }
}

impl Default for EventMetadata {
    fn default() -> Self {
        Self {
            trace_id: TraceId::new(),
            parent_event_id: None,
            retry_count: 0,
            max_retries: 0,
            ttl_ms: None,
            lifespan_ms: None,
            created_at: Timestamp::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub source: SourceId,
    #[serde(alias = "type")]
    pub event_type: EventType,
    pub timestamp: Timestamp,
    pub priority: Priority,
    pub delivery: DeliveryGuarantee,
    pub dedup_key: Option<DedupKey>,
    pub payload: Value,
    pub metadata: EventMetadata,
}

impl Event {
    #[must_use]
    pub fn new(source: impl Into<SourceId>, event_type: EventType, payload: Value) -> Self {
        let timestamp = Timestamp::now();
        let metadata = EventMetadata {
            trace_id: TraceId::new(),
            created_at: timestamp,
            ..EventMetadata::default()
        };
        let mut event = Self {
            id: Uuid::now_v7(),
            source: source.into(),
            event_type,
            timestamp,
            priority: Priority::default(),
            delivery: DeliveryGuarantee::default(),
            dedup_key: None,
            payload,
            metadata,
        };
        event.dedup_key = DedupKey::from_event(&event);
        event
    }

    #[must_use]
    pub fn with_delivery(mut self, delivery: DeliveryGuarantee) -> Self {
        self.delivery = delivery;
        self.dedup_key = DedupKey::from_event(&self);
        self
    }

    /// Returns `true` if the event has exceeded its time-to-live or lifespan.
    ///
    /// Expiration semantics:
    /// - If `ttl_ms` is set and the event's age exceeds it → expired.
    /// - If `lifespan_ms` is set and the event's age exceeds it → expired.
    /// - If both are set, **either** exceeding causes expiration (OR semantics).
    /// - If neither is set, the event never expires.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        let age_ms = now
            .as_millis()
            .saturating_sub(self.metadata.created_at.as_millis()) as u64;

        if let Some(ttl_ms) = self.metadata.ttl_ms {
            if age_ms > ttl_ms {
                return true;
            }
        }

        if let Some(lifespan_ms) = self.metadata.lifespan_ms {
            return age_ms > lifespan_ms;
        }

        false
    }
}

impl DedupKey {
    #[must_use]
    pub fn from_event(event: &Event) -> Option<Self> {
        if matches!(event.delivery, DeliveryGuarantee::AtMostOnce) {
            return None;
        }

        // UUID v7 version number is 7 (stored in bits 48-51 of the UUID)
        let version = (event.id.as_bytes()[6] >> 4) as u32;
        if version == 7 {
            return Some(Self::new(event.id.to_string()));
        }

        let payload_hash = blake3::hash(event.payload.to_string().as_bytes());
        let value = format!(
            "{}:{}:{}",
            event.source.as_str(),
            event.event_type.as_str(),
            payload_hash.to_hex()
        );

        Some(Self::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventType};
    use crate::types::{DeliveryGuarantee, Timestamp};
    use serde_json::json;

    #[test]
    fn serializes_event_type_as_event_type_field() {
        let event = Event::new("timer:heartbeat", EventType::Heartbeat, json!({"ok": true}));
        let serialized = serde_json::to_value(event).expect("event serializes");
        assert_eq!(serialized.get("event_type"), Some(&json!("heartbeat")));
    }

    #[test]
    fn deserializes_type_alias() {
        let raw = json!({
            "id": uuid::Uuid::now_v7(),
            "source": "timer:heartbeat",
            "type": "heartbeat",
            "timestamp": 1,
            "priority": "normal",
            "delivery": "at_least_once",
            "dedup_key": null,
            "payload": {},
            "metadata": {
                "trace_id": uuid::Uuid::now_v7(),
                "parent_event_id": null,
                "retry_count": 0,
                "max_retries": 0,
                "ttl_ms": null,
                "lifespan_ms": null,
                "created_at": 1
            }
        });

        let event: Event = serde_json::from_value(raw).expect("event deserializes");
        assert_eq!(event.event_type, EventType::Heartbeat);
    }

    #[test]
    fn skips_dedup_for_at_most_once() {
        let event = Event::new("webhook:test", EventType::WebhookReceived, json!({}))
            .with_delivery(DeliveryGuarantee::AtMostOnce);
        assert!(event.dedup_key.is_none());
    }

    #[test]
    fn reports_expired_when_ttl_elapsed() {
        let mut event = Event::new("timer:test", EventType::TimerTick, json!({}));
        event.metadata.ttl_ms = Some(10);
        event.metadata.created_at = Timestamp::from_millis(0);
        assert!(event.is_expired(Timestamp::from_millis(11)));
    }
}
