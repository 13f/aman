use serde::Serialize;

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
