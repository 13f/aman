// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct QueueDepth {
    pub high: i64,
    pub normal: i64,
    pub low: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub queue_depth: QueueDepth,
    pub throughput: u64,
    pub discarded: u64,
    pub duplicate: u64,
    pub subscription_count: i64,
    pub retry_queue_depth: i64,
    pub dlq_depth: usize,
    pub inflight_pipelines: usize,
    pub inflight_skills: usize,
    pub backpressure_level: String,
    pub plugin_health: Vec<PluginHealthEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginHealthEntry {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatusInfo {
    pub phase: String,
    pub ready: bool,
    pub live: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginEntry {
    pub name: String,
    pub version: Option<String>,
    pub loaded: bool,
    pub state: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DlqEntry {
    pub id: String,
    pub event_source: String,
    pub event_type: String,
    pub reason: String,
    pub retry_count: u32,
    pub enqueued_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriggerInfo {
    pub event_types: Vec<String>,
    pub sources: Vec<String>,
    pub priorities: Vec<String>,
    pub match_all: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub triggers: Vec<TriggerInfo>,
    pub concurrency: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowEntry {
    pub id: String,
    pub workflow_name: String,
    pub current_state: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SoulInfo {
    pub current_soul: Option<String>,
    pub last_changed: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransitionInfo {
    pub from: String,
    pub event: String,
    pub to: String,
    pub guard: Option<String>,
    pub has_action: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowDefInfo {
    pub name: String,
    pub states: Vec<String>,
    pub initial_state: String,
    pub final_states: Vec<String>,
    pub error_state: String,
    pub transitions: Vec<TransitionInfo>,
    pub state_timeouts: Vec<StateTimeoutInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateTimeoutInfo {
    pub state: String,
    pub timeout_ms: u64,
    pub on_timeout: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityEntry {
    pub capability: String,
    pub plugin: String,
    pub version: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeConfigInfo {
    pub runtime_dir: Option<String>,
    pub bind_addr: Option<String>,
    pub has_api_token: bool,
    pub risky_enabled: bool,
    pub skills_dir: Option<String>,
}

// ---------------------------------------------------------------------------
// Chat session models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionInfo {
    pub id: String,
    pub state: String,
    pub message_count: usize,
    pub created_at: i64,
    pub last_active_at: Option<i64>,
    /// Short title derived from the first user message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Session type: "ad-hoc", "persistent", "shared", "shared-sub", "branch", "role-play"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
    /// For branch sessions: the parent session ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// For branch sessions: the message ID where the branch was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_message_id: Option<String>,
    /// Optimistic lock version — incremented on each state-changing operation
    pub version: u64,
    /// Agent that owns this session.
    #[serde(default)]
    pub agent_id: String,
    /// True if the session can be safely deleted: it has no messages, or its
    /// agent replies match low-value patterns (idle signals, no-result batch
    /// jobs, etc.) per `is_low_value_reply`. Used by the UI to show/hide a
    /// per-session delete button.
    #[serde(default)]
    pub deletable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageEntry {
    #[serde(rename = "event_id")]
    pub id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    #[serde(rename = "timestamp_ms")]
    pub timestamp: i64,
    pub trace_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionState {
    pub session_id: String,
    pub state: String,
    pub state_version: u64,
    pub retry_count: u32,
    pub messages: Vec<ChatMessageEntry>,
    /// Session type: "ad-hoc", "persistent", "shared", "shared-sub", "branch", "role-play"
    pub session_type: String,
    /// Optimistic lock version — incremented on each state-changing operation
    pub version: u64,
}

// ---------------------------------------------------------------------------
// Multi-agent models (P2+)
// ---------------------------------------------------------------------------

/// A single LLM provider entry for IPC responses.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderEntry {
    pub key: String,
    pub display_name: String,
    pub base_url: String,
    pub has_api_key: bool,
}

/// A single notification entry for IPC responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationEntry {
    pub id: String,
    pub severity: String,
    pub category: String,
    pub created_at: i64,
    pub title: String,
    pub message: String,
    pub dismissed: bool,
    pub dismissible: bool,
    pub action_label: Option<String>,
    pub action_route: Option<String>,
    pub event_id: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadCount {
    pub count: usize,
}

/// A model entry returned by list_provider_models.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    /// Display name / global model ID.
    pub id: String,
    /// Provider-specific API model name.
    pub model_id: String,
}

/// A single agent entry for IPC responses (filesystem-based).
#[derive(Debug, Clone, Serialize)]
pub struct AgentEntry {
    pub key: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub soul_summary: String,
    pub session_count: u64,
    pub is_active: bool,
}

/// A code agent entry — external CLI coding tool.
#[derive(Debug, Clone, Serialize)]
pub struct CodeAgentEntry {
    pub key: String,
    pub display_name: String,
    pub command: String,
    pub description: String,
    pub available: bool,
}

/// A finance card entry — skill card displayed on the Home page Finance tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinanceCardEntry {
    pub skill_name: String,
    pub title: String,
    pub subtitle: String,
    pub icon: String,
}

/// A single emotion entry from an agent's `emotions/data.json`.
#[derive(Debug, Clone, Serialize)]
pub struct EmotionEntry {
    pub id: String,
    pub tags: Vec<String>,
    pub description: String,
    /// Base64-encoded data URL ready for `<img src="...">`.
    pub data_url: String,
}

/// Full emotions configuration parsed from `emotions/data.json`.
/// Returned to the frontend so it can map state → emotion image.
#[derive(Debug, Clone, Serialize)]
pub struct EmotionsConfig {
    pub img_ext: String,
    pub items: Vec<EmotionEntry>,
}

/// Agent runtime instance exposed via Tauri IPC.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInstanceInfo {
    pub agent_id: String,
    pub display_name: String,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub enabled: bool,
    pub active_session_id: Option<String>,
    /// Current anthropomorphic system state: "idle", "working", etc.
    #[serde(default)]
    pub system_state: String,
}

// ── MCP Server models ──────────────────────────────────────────────

/// An MCP server definition + runtime status (returned to the UI).
/// Mirrors `mcp_servers_fs::McpServerEntry`.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerEntry {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub auto_connect: bool,
    /// "global" or the agent key.
    pub source: String,
    /// Runtime status from gateway.
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub tool_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
