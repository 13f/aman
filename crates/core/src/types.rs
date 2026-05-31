// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    #[must_use]
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let millis = i64::try_from(millis).unwrap_or(i64::MAX);
        Self(millis)
    }

    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceId(Uuid);

impl TraceId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SourceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for SourceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct DedupKey(String);

impl DedupKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DedupKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    High,
    #[default]
    Normal,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGuarantee {
    AtMostOnce,
    #[default]
    AtLeastOnce,
    ExactlyOnce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Timer,
    File,
    Network,
    Webhook,
    Data,
    Platform,
    #[default]
    Custom,
    /// 未知/未初始化（to_u8() 返回 0，适用于 AtomicU8 安全默认值）
    Unknown,
    /// 聊天对话来源
    Chat,
}

impl SourceType {
    /// 返回稳定的 u8 表示，用于 AtomicU8 存储。
    /// Unknown → 0（安全默认值），其余按声明顺序。
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Timer => 1,
            Self::File => 2,
            Self::Network => 3,
            Self::Webhook => 4,
            Self::Data => 5,
            Self::Platform => 6,
            Self::Custom => 7,
            Self::Chat => 8,
        }
    }

    /// 从 u8 恢复 SourceType。未知值返回 Unknown。
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Unknown,
            1 => Self::Timer,
            2 => Self::File,
            3 => Self::Network,
            4 => Self::Webhook,
            5 => Self::Data,
            6 => Self::Platform,
            7 => Self::Custom,
            8 => Self::Chat,
            _ => Self::Unknown,
        }
    }

    /// 是否是聊天来源。
    #[must_use]
    pub fn is_chat(self) -> bool {
        matches!(self, Self::Chat)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyModel {
    #[default]
    Serial,
    Parallel,
    Limited(usize),
}

/// Declares how tool calls should be scheduled relative to each other.
///
/// Tools declare their own execution model — the runtime uses this metadata
/// to automatically parallelize Independent calls while keeping Stateful
/// and SideEffect calls serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModel {
    /// Tool calls are independent of each other — can run concurrently.
    /// Examples: read, grep, find, list, web_search, http(GET).
    #[default]
    Independent,
    /// Tool calls may depend on earlier results — must be serial.
    /// Examples: write (may create a file that a later edit targets), edit, db.
    Stateful,
    /// Tool has irreversible external effects — needs explicit ordering.
    /// Examples: exec, deploy, delete.
    SideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolMode {
    #[default]
    Local,
    Remote,
    Container,
    Sandbox,
}

/// Trust level for event sources and plugins.
///
/// Used to gate sensitive operations — sandboxed sources are restricted
/// from publishing sensitive event types and their capabilities are
/// enforced by the security harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Fully trusted — internal system components, no restrictions.
    Trusted,
    /// Untrusted — user-provided but reviewed; moderate restrictions.
    #[default]
    Untrusted,
    /// Sandboxed — isolated plugin/hook; strict resource limits and
    /// event publishing restrictions enforced by the security harness.
    Sandboxed,
}

impl TrustLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
            Self::Sandboxed => "sandboxed",
        }
    }

    /// Returns `true` if this trust level is allowed to publish sensitive events.
    #[must_use]
    pub const fn can_publish_sensitive(self) -> bool {
        matches!(self, Self::Trusted)
    }

    /// Returns `true` if this source requires sandbox enforcement.
    #[must_use]
    pub const fn requires_sandbox(self) -> bool {
        matches!(self, Self::Sandboxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BackpressureLevel {
    #[default]
    Normal,
    #[serde(rename = "l1")]
    L1,
    #[serde(rename = "l2")]
    L2,
    #[serde(rename = "l3")]
    L3,
    #[serde(rename = "l4a")]
    L4A,
    #[serde(rename = "l4b")]
    L4B,
    #[serde(rename = "critical")]
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    #[default]
    Ok,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompensationStrategy {
    #[default]
    ReverseOrder,
}

#[cfg(test)]
mod tests {
    use super::{
        BackpressureLevel, CompensationStrategy, ConcurrencyModel, DedupKey, DeliveryGuarantee,
        ExecutionModel, HealthStatus, Priority, SourceId, SourceType, Timestamp, ToolMode, TraceId,
    };
    use serde_json::json;

    #[test]
    fn timestamp_round_trips_millis() {
        let timestamp = Timestamp::from_millis(42);
        assert_eq!(timestamp.as_millis(), 42);
        assert_eq!(timestamp.to_string(), "42");
    }

    #[test]
    fn timestamp_now_is_non_negative() {
        assert!(Timestamp::now().as_millis() >= 0);
    }

    #[test]
    fn trace_id_default_is_v7_uuid() {
        let trace_id = TraceId::default();
        let uuid = trace_id.into_inner();
        assert_eq!(uuid.get_version_num(), 7);
        assert_eq!(trace_id.to_string(), uuid.to_string());
    }

    #[test]
    fn source_id_and_dedup_key_preserve_values() {
        let source_id = SourceId::new("timer:heartbeat");
        let dedup_key = DedupKey::new("source:type:hash");

        assert_eq!(source_id.as_str(), "timer:heartbeat");
        assert_eq!(source_id.to_string(), "timer:heartbeat");
        assert_eq!(dedup_key.as_str(), "source:type:hash");
        assert_eq!(dedup_key.to_string(), "source:type:hash");
    }

    #[test]
    fn enums_serialize_with_expected_schema_names() {
        assert_eq!(
            serde_json::to_value(Priority::Normal).expect("serialize"),
            json!("normal")
        );
        assert_eq!(
            serde_json::to_value(DeliveryGuarantee::AtLeastOnce).expect("serialize"),
            json!("at_least_once")
        );
        assert_eq!(
            serde_json::to_value(SourceType::Webhook).expect("serialize"),
            json!("webhook")
        );
        assert_eq!(
            serde_json::to_value(ConcurrencyModel::Limited(4)).expect("serialize"),
            json!({"limited": 4})
        );
        assert_eq!(
            serde_json::to_value(ToolMode::Sandbox).expect("serialize"),
            json!("sandbox")
        );
        assert_eq!(
            serde_json::to_value(BackpressureLevel::L4A).expect("serialize"),
            json!("l4a")
        );
        assert_eq!(
            serde_json::to_value(HealthStatus::Degraded).expect("serialize"),
            json!("degraded")
        );
        assert_eq!(
            serde_json::to_value(CompensationStrategy::ReverseOrder).expect("serialize"),
            json!("reverse_order")
        );
        assert_eq!(
            serde_json::to_value(ExecutionModel::Independent).expect("serialize"),
            json!("independent")
        );
        assert_eq!(
            serde_json::to_value(ExecutionModel::Stateful).expect("serialize"),
            json!("stateful")
        );
        assert_eq!(
            serde_json::to_value(ExecutionModel::SideEffect).expect("serialize"),
            json!("side_effect")
        );
    }

    #[test]
    fn enums_deserialize_expected_schema_names() {
        assert_eq!(
            serde_json::from_value::<Priority>(json!("high")).expect("deserialize"),
            Priority::High
        );
        assert_eq!(
            serde_json::from_value::<DeliveryGuarantee>(json!("exactly_once"))
                .expect("deserialize"),
            DeliveryGuarantee::ExactlyOnce
        );
        assert_eq!(
            serde_json::from_value::<SourceType>(json!("platform")).expect("deserialize"),
            SourceType::Platform
        );
        assert_eq!(
            serde_json::from_value::<ConcurrencyModel>(json!({"limited": 2})).expect("deserialize"),
            ConcurrencyModel::Limited(2)
        );
        assert_eq!(
            serde_json::from_value::<ToolMode>(json!("remote")).expect("deserialize"),
            ToolMode::Remote
        );
        assert_eq!(
            serde_json::from_value::<BackpressureLevel>(json!("critical")).expect("deserialize"),
            BackpressureLevel::Critical
        );
        assert_eq!(
            serde_json::from_value::<HealthStatus>(json!("failed")).expect("deserialize"),
            HealthStatus::Failed
        );
        assert_eq!(
            serde_json::from_value::<ExecutionModel>(json!("independent")).expect("deserialize"),
            ExecutionModel::Independent
        );
        assert_eq!(
            serde_json::from_value::<ExecutionModel>(json!("stateful")).expect("deserialize"),
            ExecutionModel::Stateful
        );
    }
}
