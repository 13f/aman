#![forbid(unsafe_code)]
#![doc = "Configuration model and layered loader for the Aman agent framework."]

use idle::IdlePersonality;
use kernel::event::{Event, EventType};
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BusMode {
    #[default]
    InMemory,
    Persistent,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout_sec: u64,
    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_sec: u64,
}

fn default_drain_timeout() -> u64 { 30 }
fn default_tool_timeout() -> u64 { 60 }

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            drain_timeout_sec: 30,
            tool_timeout_sec: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceConfig {
    #[serde(default = "default_wal_sync")]
    pub wal_sync: String,
    #[serde(default = "default_checkpoint_interval")]
    pub checkpoint_interval: u64,
}

fn default_wal_sync() -> String { "fsync".to_string() }
fn default_checkpoint_interval() -> u64 { 500 }

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            wal_sync: "fsync".to_string(),
            checkpoint_interval: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventBusConfig {
    #[serde(default)]
    pub mode: BusMode,
    #[serde(default = "default_max_queue_size")]
    pub max_queue_size: usize,
    #[serde(default)]
    pub persistence: Option<PersistenceConfig>,
}

fn default_max_queue_size() -> usize { 10_000 }

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            mode: BusMode::Persistent,
            max_queue_size: 10_000,
            persistence: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfig {
    #[serde(default = "default_enforce_dep")]
    pub enforce_dependency_check: bool,
}

fn default_enforce_dep() -> bool { true }

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enforce_dependency_check: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub struct SourceConfig {
    #[serde(default)]
    pub notify_on_complete: bool,
    #[serde(default)]
    pub watch_patterns: Vec<String>,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    pub states: Vec<String>,
    pub initial_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkflowConfig {
    #[serde(default)]
    pub definitions: Vec<WorkflowDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub struct SecurityConfig {
    #[serde(default)]
    pub risky_capabilities_enabled: bool,
}

// ── Idle State System config (M2.3) ───────────────────────────

/// Reflection 处理器配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflectionConfig {
    #[serde(default = "default_reflection_enabled")]
    pub enabled: bool,
    #[serde(default = "default_reflection_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_reflection_check_items")]
    pub check_items: Vec<String>,
}

fn default_reflection_enabled() -> bool { true }
fn default_reflection_timeout_secs() -> u64 { 30 }
fn default_reflection_check_items() -> Vec<String> {
    vec!["chain_tasks".into(), "immediate_errors".into(), "lessons_learned".into()]
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: 30,
            check_items: vec!["chain_tasks".into(), "immediate_errors".into(), "lessons_learned".into()],
        }
    }
}

/// Arousal 系统配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArousalConfig {
    #[serde(default = "default_arousal_initial")]
    pub initial_value: f64,
    #[serde(default = "default_arousal_half_life_secs")]
    pub half_life_secs: f64,
}

fn default_arousal_initial() -> f64 { 1.0 }
fn default_arousal_half_life_secs() -> f64 { 900.0 }

impl Default for ArousalConfig {
    fn default() -> Self {
        Self {
            initial_value: 1.0,
            half_life_secs: 900.0,
        }
    }
}

/// Idle 上下文配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleContextConfig {
    #[serde(default = "default_max_output_buffer")]
    pub max_output_buffer: usize,
}

fn default_max_output_buffer() -> usize { 10 }

impl Default for IdleContextConfig {
    fn default() -> Self {
        Self { max_output_buffer: 10 }
    }
}

/// Sleep 子配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepConfig {
    #[serde(default = "default_short_term_retention_days")]
    pub short_term_retention_days: u64,
    #[serde(default = "default_cache_expiry_days")]
    pub cache_expiry_days: u64,
    #[serde(default = "default_max_cpu_seconds")]
    pub max_cpu_seconds: u64,
}

fn default_short_term_retention_days() -> u64 { 7 }
fn default_cache_expiry_days() -> u64 { 30 }
fn default_max_cpu_seconds() -> u64 { 300 }

impl Default for SleepConfig {
    fn default() -> Self {
        Self {
            short_term_retention_days: 7,
            cache_expiry_days: 30,
            max_cpu_seconds: 300,
        }
    }
}

/// Exploration 子配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorationConfig {
    #[serde(default = "default_api_rate_per_minute")]
    pub api_rate_per_minute: u32,
    #[serde(default = "default_exploration_fallback")]
    pub on_quota_exhausted: String,
}

fn default_api_rate_per_minute() -> u32 { 10 }
fn default_exploration_fallback() -> String { "fallback".into() }

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            api_rate_per_minute: 10,
            on_quota_exhausted: "fallback".into(),
        }
    }
}

/// Incubation 子配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncubationConfig {
    #[serde(default = "default_max_concurrent_incubation")]
    pub max_concurrent: u32,
    #[serde(default)]
    pub enabled: bool,
}

fn default_max_concurrent_incubation() -> u32 { 1 }

impl Default for IncubationConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            enabled: false,
        }
    }
}

/// 顶级 Idle 段配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub reflection: ReflectionConfig,
    #[serde(default)]
    pub personality: IdlePersonality,
    #[serde(default)]
    pub arousal: ArousalConfig,
    #[serde(default)]
    pub context: IdleContextConfig,
    #[serde(default)]
    pub sleep: SleepConfig,
    #[serde(default)]
    pub exploration: ExplorationConfig,
    #[serde(default)]
    pub incubation: IncubationConfig,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reflection: ReflectionConfig::default(),
            personality: IdlePersonality::default(),
            arousal: ArousalConfig::default(),
            context: IdleContextConfig::default(),
            sleep: SleepConfig::default(),
            exploration: ExplorationConfig::default(),
            incubation: IncubationConfig::default(),
        }
    }
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub event_bus: EventBusConfig,
    #[serde(default)]
    pub plugin: PluginConfig,
    #[serde(default)]
    pub source: SourceConfig,
    #[serde(default)]
    pub workflow: WorkflowConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub idle: IdleConfig,
}

// ── Multi-Agent config (P1) ──────────────────────────────────────

/// Single LLM provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub display_name: String,
    pub base_url: String,
    /// Optional inline API key. Checked after Keychain and env var fallbacks.
    /// Use `$KEYCHAIN:aman.providers.<provider>.api_key` or
    /// `$ENV:AMAN_PROVIDER_<PROVIDER>_API_KEY` for secret management.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Default LLM model configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultModelConfig {
    pub default: String,
    pub provider: String,
    pub base_url: String,
}

/// Single agent entry in the multi-agent config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntryConfig {
    pub display_name: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_override: Option<String>,
}

/// Top-level multi-agent configuration that wraps the existing runtime
/// config (`AgentConfig`) alongside `providers`, `model`, and `agents`.
///
/// YAML layout:
/// ```yaml
/// event_bus: ...       # flattened from runtime (AgentConfig)
/// providers:
///   openai:
///     display_name: OpenAI
///     base_url: https://api.openai.com/v1
/// model:
///   default: gpt-5
///   provider: openai
///   base_url: https://api.openai.com/v1
/// agents:
///   cortana:
///     display_name: Cortana
///     provider: openai
///     model: gpt-5
///     system_prompt_override: null
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AmanConfig {
    #[serde(flatten, default)]
    pub runtime: AgentConfig,
    #[serde(default, deserialize_with = "deserialize_null_map")]
    pub providers: HashMap<String, ProviderConfig>,
    pub model: Option<DefaultModelConfig>,
    #[serde(default, deserialize_with = "deserialize_null_map")]
    pub agents: HashMap<String, AgentEntryConfig>,
}

/// Deserialize a HashMap from a YAML map, treating null/absent as empty.
fn deserialize_null_map<'de, D, K, V>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: serde::Deserialize<'de> + std::hash::Hash + Eq,
    V: serde::Deserialize<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum MaybeMap<V> {
        Some(V),
        Null,
    }

    match MaybeMap::<HashMap<K, V>>::deserialize(deserializer)? {
        MaybeMap::Some(m) => Ok(m),
        MaybeMap::Null => Ok(HashMap::new()),
    }
}

impl AmanConfig {
    /// Validate the entire multi-agent config, returning warnings.
    /// Checks runtime validity + provider/agent key rules + provider references.
    pub fn validate_full(&self) -> AmanResult<Vec<String>> {
        let mut warnings = self.runtime.validate()?;

        for key in self.providers.keys() {
            if !is_valid_identifier(key) {
                return Err(Error::config_invalid(format!(
                    "Provider key '{key}' 只能包含英文字母、数字、下划线、短横线"
                )));
            }
        }

        for key in self.agents.keys() {
            if !is_valid_identifier(key) {
                return Err(Error::config_invalid(format!(
                    "Agent key '{key}' 只能包含英文字母、数字、下划线、短横线"
                )));
            }
        }

        for (agent_key, agent) in &self.agents {
            if !self.providers.contains_key(&agent.provider) {
                warnings.push(format!(
                    "Agent '{agent_key}' 的 provider '{}' 未在 providers 中定义",
                    agent.provider
                ));
            }
        }

        Ok(warnings)
    }

    /// Load config from `~/.aman/config.yaml`.
    pub fn from_default_path() -> AmanResult<Self> {
        let path = default_config_path();
        Self::from_file(&path)
    }

    /// Load and validate config from an explicit file path.
    pub fn from_file(path: &Path) -> AmanResult<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| Error::config_invalid(format!("读取 {}: {e}", path.display())))?;
        let config: AmanConfig = serde_yaml::from_str(&content)
            .map_err(|e| Error::config_invalid(format!("解析 config.yaml 失败: {e}")))?;
        config.validate_full()?;
        Ok(config)
    }

    /// Serialize and write config to disk, creating parent directories if needed.
    pub fn save(&self, path: &Path) -> AmanResult<()> {
        let content = serde_yaml::to_string(self)
            .map_err(|e| Error::config_invalid(format!("序列化配置失败: {e}")))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &content)?;
        Ok(())
    }
}

/// Provider/Agent key must be non-empty and contain only ASCII alphanumeric,
/// underscore or hyphen.
pub fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn default_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".aman").join("config.yaml")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigAuditRecord {
    pub layer: String,
    pub detail: String,
    pub changed_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSnapshotMeta {
    pub loaded_at_ms: u128,
    pub source_chain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLoadResult {
    pub config: AgentConfig,
    pub warnings: Vec<String>,
    pub audit: Vec<ConfigAuditRecord>,
    pub meta: ConfigSnapshotMeta,
}

pub struct ConfigReloader {
    file_path: Option<PathBuf>,
    runtime_override_path: Option<PathBuf>,
    last_config: Option<AgentConfig>,
}

impl ConfigReloader {
    #[must_use]
    pub fn new(file_path: Option<PathBuf>, runtime_override_path: Option<PathBuf>) -> Self {
        Self {
            file_path,
            runtime_override_path,
            last_config: None,
        }
    }

    pub fn load_initial(&mut self) -> AmanResult<ConfigLoadResult> {
        let loaded = ConfigLoader::load(
            self.file_path.as_deref(),
            self.runtime_override_path.as_deref(),
        )?;
        self.last_config = Some(loaded.config.clone());
        Ok(loaded)
    }

    pub fn reload_if_changed(&mut self) -> AmanResult<Option<Event>> {
        let loaded = ConfigLoader::load(
            self.file_path.as_deref(),
            self.runtime_override_path.as_deref(),
        )?;

        let Some(previous) = &self.last_config else {
            self.last_config = Some(loaded.config.clone());
            return Ok(Some(build_config_changed_event(&loaded, Vec::new())));
        };

        let changed_fields = diff_changed_fields(previous, &loaded.config)?;
        if changed_fields.is_empty() {
            return Ok(None);
        }

        self.last_config = Some(loaded.config.clone());
        Ok(Some(build_config_changed_event(&loaded, changed_fields)))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartialAgentConfig {
    pub runtime: Option<PartialRuntimeConfig>,
    pub event_bus: Option<PartialEventBusConfig>,
    pub plugin: Option<PartialPluginConfig>,
    pub source: Option<PartialSourceConfig>,
    pub workflow: Option<PartialWorkflowConfig>,
    pub security: Option<PartialSecurityConfig>,
    pub idle: Option<PartialIdleConfig>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialRuntimeConfig {
    pub drain_timeout_sec: Option<u64>,
    pub tool_timeout_sec: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialPersistenceConfig {
    pub wal_sync: Option<String>,
    pub checkpoint_interval: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialEventBusConfig {
    pub mode: Option<BusMode>,
    pub max_queue_size: Option<usize>,
    pub persistence: Option<PartialPersistenceConfig>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialPluginConfig {
    pub enforce_dependency_check: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialSourceConfig {
    pub notify_on_complete: Option<bool>,
    pub watch_patterns: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialWorkflowConfig {
    pub definitions: Option<Vec<WorkflowDefinition>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialSecurityConfig {
    pub risky_capabilities_enabled: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartialIdleConfig {
    pub enabled: Option<bool>,
    pub personality: Option<IdlePersonality>,
    pub reflection: Option<PartialReflectionConfig>,
    pub arousal: Option<PartialArousalConfig>,
    pub context: Option<PartialIdleContextConfig>,
    pub sleep: Option<PartialSleepConfig>,
    pub exploration: Option<PartialExplorationConfig>,
    pub incubation: Option<PartialIncubationConfig>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialReflectionConfig {
    pub enabled: Option<bool>,
    pub timeout_secs: Option<u64>,
    pub check_items: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartialArousalConfig {
    pub initial_value: Option<f64>,
    pub half_life_secs: Option<f64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialIdleContextConfig {
    pub max_output_buffer: Option<usize>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialSleepConfig {
    pub short_term_retention_days: Option<u64>,
    pub cache_expiry_days: Option<u64>,
    pub max_cpu_seconds: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialExplorationConfig {
    pub api_rate_per_minute: Option<u32>,
    pub on_quota_exhausted: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialIncubationConfig {
    pub max_concurrent: Option<u32>,
    pub enabled: Option<bool>,
}

pub struct ConfigLoader;

impl ConfigLoader {
    pub fn load(
        file_path: Option<&Path>,
        runtime_override_path: Option<&Path>,
    ) -> AmanResult<ConfigLoadResult> {
        let mut config = AgentConfig::default();
        let mut audit = vec![ConfigAuditRecord {
            layer: "default".to_string(),
            detail: "apply hard-coded defaults".to_string(),
            changed_fields: Vec::new(),
        }];

        if let Some(path) = file_path {
            let before = config.clone();
            let partial = load_partial_from_yaml(path)?;
            config.merge(partial);
            let changed_fields = diff_changed_fields(&before, &config)?;
            audit.push(ConfigAuditRecord {
                layer: "file".to_string(),
                detail: format!("merge {}", path.display()),
                changed_fields,
            });
        }

        let env_patch = load_env_patch()?;
        if env_patch != PartialAgentConfig::default() {
            let before = config.clone();
            config.merge(env_patch);
            let changed_fields = diff_changed_fields(&before, &config)?;
            audit.push(ConfigAuditRecord {
                layer: "env".to_string(),
                detail: "merge AMAN_* overrides".to_string(),
                changed_fields,
            });
        }

        if let Some(path) = runtime_override_path {
            let before = config.clone();
            let partial = load_partial_from_yaml(path)?;
            config.merge(partial);
            let changed_fields = diff_changed_fields(&before, &config)?;
            audit.push(ConfigAuditRecord {
                layer: "runtime_override".to_string(),
                detail: format!("merge {}", path.display()),
                changed_fields,
            });
        }

        let warnings = config.validate()?;
        let source_chain = audit
            .iter()
            .map(|entry| entry.layer.clone())
            .collect::<Vec<_>>();
        let loaded_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        Ok(ConfigLoadResult {
            config,
            warnings,
            audit,
            meta: ConfigSnapshotMeta {
                loaded_at_ms,
                source_chain,
            },
        })
    }
}

impl AgentConfig {
    pub fn merge(&mut self, patch: PartialAgentConfig) {
        if let Some(runtime) = patch.runtime {
            if let Some(v) = runtime.drain_timeout_sec {
                self.runtime.drain_timeout_sec = v;
            }
            if let Some(v) = runtime.tool_timeout_sec {
                self.runtime.tool_timeout_sec = v;
            }
        }

        if let Some(event_bus) = patch.event_bus {
            if let Some(mode) = event_bus.mode {
                self.event_bus.mode = mode;
            }
            if let Some(max_queue_size) = event_bus.max_queue_size {
                self.event_bus.max_queue_size = max_queue_size;
            }
            if let Some(persistence_patch) = event_bus.persistence {
                let persistence = self
                    .event_bus
                    .persistence
                    .get_or_insert_with(PersistenceConfig::default);
                if let Some(wal_sync) = persistence_patch.wal_sync {
                    persistence.wal_sync = wal_sync;
                }
                if let Some(checkpoint_interval) = persistence_patch.checkpoint_interval {
                    persistence.checkpoint_interval = checkpoint_interval;
                }
            }
        }

        if let Some(plugin) = patch.plugin
            && let Some(value) = plugin.enforce_dependency_check
        {
            self.plugin.enforce_dependency_check = value;
        }

        if let Some(source) = patch.source {
            if let Some(value) = source.notify_on_complete {
                self.source.notify_on_complete = value;
            }
            if let Some(patterns) = source.watch_patterns {
                self.source.watch_patterns = patterns;
            }
        }

        if let Some(workflow) = patch.workflow
            && let Some(definitions) = workflow.definitions
        {
            self.workflow.definitions = definitions;
        }

        if let Some(security) = patch.security
            && let Some(value) = security.risky_capabilities_enabled
        {
            self.security.risky_capabilities_enabled = value;
        }

        if let Some(idle) = patch.idle {
            if let Some(v) = idle.enabled {
                self.idle.enabled = v;
            }
            if let Some(v) = idle.personality {
                self.idle.personality = v;
            }
            if let Some(reflection) = idle.reflection {
                if let Some(v) = reflection.enabled {
                    self.idle.reflection.enabled = v;
                }
                if let Some(v) = reflection.timeout_secs {
                    self.idle.reflection.timeout_secs = v;
                }
                if let Some(v) = reflection.check_items {
                    self.idle.reflection.check_items = v;
                }
            }
            if let Some(arousal) = idle.arousal {
                if let Some(v) = arousal.initial_value {
                    self.idle.arousal.initial_value = v;
                }
                if let Some(v) = arousal.half_life_secs {
                    self.idle.arousal.half_life_secs = v;
                }
            }
            if let Some(context) = idle.context {
                if let Some(v) = context.max_output_buffer {
                    self.idle.context.max_output_buffer = v;
                }
            }
            if let Some(sleep) = idle.sleep {
                if let Some(v) = sleep.short_term_retention_days {
                    self.idle.sleep.short_term_retention_days = v;
                }
                if let Some(v) = sleep.cache_expiry_days {
                    self.idle.sleep.cache_expiry_days = v;
                }
                if let Some(v) = sleep.max_cpu_seconds {
                    self.idle.sleep.max_cpu_seconds = v;
                }
            }
            if let Some(exploration) = idle.exploration {
                if let Some(v) = exploration.api_rate_per_minute {
                    self.idle.exploration.api_rate_per_minute = v;
                }
                if let Some(v) = exploration.on_quota_exhausted {
                    self.idle.exploration.on_quota_exhausted = v;
                }
            }
            if let Some(incubation) = idle.incubation {
                if let Some(v) = incubation.max_concurrent {
                    self.idle.incubation.max_concurrent = v;
                }
                if let Some(v) = incubation.enabled {
                    self.idle.incubation.enabled = v;
                }
            }
        }
    }

    pub fn validate(&self) -> AmanResult<Vec<String>> {
        if self.event_bus.mode == BusMode::InMemory && self.event_bus.persistence.is_some() {
            return Err(Error::config_invalid(
                "event_bus.mode=in_memory 不允许配置 event_bus.persistence",
            ));
        }

        if self.runtime.drain_timeout_sec >= self.runtime.tool_timeout_sec {
            return Err(Error::config_invalid(
                "runtime.drain_timeout_sec 必须小于 runtime.tool_timeout_sec",
            ));
        }

        if self.source.notify_on_complete && !self.source.watch_patterns.is_empty() {
            return Err(Error::config_invalid(
                "source.notify_on_complete 与 source.watch_patterns 互斥",
            ));
        }

        // ── Idle config validation ──────────────────────────────
        if self.idle.enabled {
            // allowed_kinds ⊆ enabled_kinds
            let personality = &self.idle.personality;
            for allowed in &personality.chat_mode.allowed_kinds {
                if !personality.enabled_kinds.contains(allowed) {
                    return Err(Error::config_invalid(format!(
                        "idle.personality.chat_mode.allowed_kinds 包含 {allowed:?}，但不在 enabled_kinds 中"
                    )));
                }
            }
            // reflection_breaker.max_consecutive >= 1
            if personality.reflection_breaker.max_consecutive < 1 {
                return Err(Error::config_invalid(
                    "idle.personality.reflection_breaker.max_consecutive 必须 >= 1",
                ));
            }
        }

        let mut warnings = Vec::new();
        for def in &self.workflow.definitions {
            if !def.states.iter().any(|state| state == &def.initial_state) {
                return Err(Error::config_invalid(format!(
                    "workflow {} 的 initial_state={} 不在 states 中",
                    def.name, def.initial_state
                )));
            }

            let mut normalized = HashSet::new();
            for state in &def.states {
                let upper = state.to_ascii_uppercase();
                if !normalized.insert(upper) {
                    warnings.push(format!(
                        "workflow {} 存在状态名大小写不一致，建议统一命名",
                        def.name
                    ));
                    break;
                }
            }
        }

        Ok(warnings)
    }
}

fn load_partial_from_yaml(path: &Path) -> AmanResult<PartialAgentConfig> {
    let content = fs::read_to_string(path)?;
    let partial = serde_yaml::from_str::<PartialAgentConfig>(&content)
        .map_err(|err| Error::config_invalid(format!("配置解析失败({}): {err}", path.display())))?;
    Ok(partial)
}

fn load_env_patch() -> AmanResult<PartialAgentConfig> {
    load_env_patch_from_iter(std::env::vars())
}

fn load_env_patch_from_iter<I>(vars: I) -> AmanResult<PartialAgentConfig>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut patch = PartialAgentConfig::default();
    for (key, value) in vars {
        match key.as_str() {
            "AMAN_EVENT_BUS_MODE" => {
                let mode = parse_bus_mode(&value)?;
                patch
                    .event_bus
                    .get_or_insert_with(PartialEventBusConfig::default)
                    .mode = Some(mode);
            }
            "AMAN_EVENT_BUS_MAX_QUEUE_SIZE" => {
                let parsed = value.parse::<usize>().map_err(|_| {
                    Error::config_invalid("AMAN_EVENT_BUS_MAX_QUEUE_SIZE 必须是整数")
                })?;
                patch
                    .event_bus
                    .get_or_insert_with(PartialEventBusConfig::default)
                    .max_queue_size = Some(parsed);
            }
            "AMAN_RUNTIME_DRAIN_TIMEOUT_SEC" => {
                let parsed = value.parse::<u64>().map_err(|_| {
                    Error::config_invalid("AMAN_RUNTIME_DRAIN_TIMEOUT_SEC 必须是整数")
                })?;
                patch
                    .runtime
                    .get_or_insert_with(PartialRuntimeConfig::default)
                    .drain_timeout_sec = Some(parsed);
            }
            "AMAN_RUNTIME_TOOL_TIMEOUT_SEC" => {
                let parsed = value.parse::<u64>().map_err(|_| {
                    Error::config_invalid("AMAN_RUNTIME_TOOL_TIMEOUT_SEC 必须是整数")
                })?;
                patch
                    .runtime
                    .get_or_insert_with(PartialRuntimeConfig::default)
                    .tool_timeout_sec = Some(parsed);
            }
            "AMAN_SOURCE_NOTIFY_ON_COMPLETE" => {
                let parsed = parse_bool(&value, "AMAN_SOURCE_NOTIFY_ON_COMPLETE")?;
                patch
                    .source
                    .get_or_insert_with(PartialSourceConfig::default)
                    .notify_on_complete = Some(parsed);
            }
            "AMAN_SOURCE_WATCH_PATTERNS" => {
                let parsed = value
                    .split(',')
                    .map(str::trim)
                    .filter(|it| !it.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                patch
                    .source
                    .get_or_insert_with(PartialSourceConfig::default)
                    .watch_patterns = Some(parsed);
            }
            "AMAN_SECURITY_RISKY_CAPABILITIES_ENABLED" => {
                let parsed = parse_bool(&value, "AMAN_SECURITY_RISKY_CAPABILITIES_ENABLED")?;
                patch
                    .security
                    .get_or_insert_with(PartialSecurityConfig::default)
                    .risky_capabilities_enabled = Some(parsed);
            }
            _ => {}
        }
    }
    Ok(patch)
}

fn parse_bus_mode(raw: &str) -> AmanResult<BusMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "in_memory" => Ok(BusMode::InMemory),
        "persistent" => Ok(BusMode::Persistent),
        _ => Err(Error::config_invalid(format!(
            "AMAN_EVENT_BUS_MODE 仅支持 in_memory|persistent，收到 {raw}"
        ))),
    }
}

fn parse_bool(raw: &str, field: &str) -> AmanResult<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(Error::config_invalid(format!("{field} 必须是布尔值"))),
    }
}

fn diff_changed_fields(before: &AgentConfig, after: &AgentConfig) -> AmanResult<Vec<String>> {
    let before_value = serde_json::to_value(before)?;
    let after_value = serde_json::to_value(after)?;
    let mut changed = Vec::new();
    collect_changed_fields("", &before_value, &after_value, &mut changed);
    changed.sort();
    changed.dedup();
    Ok(changed)
}

fn build_config_changed_event(loaded: &ConfigLoadResult, changed_fields: Vec<String>) -> Event {
    let payload = serde_json::json!({
        "changed_fields": changed_fields,
        "meta": {
            "loaded_at_ms": loaded.meta.loaded_at_ms,
            "source_chain": loaded.meta.source_chain,
        }
    });
    Event::new("config", EventType::ConfigChanged, payload)
}

fn collect_changed_fields(path: &str, before: &Value, after: &Value, output: &mut Vec<String>) {
    match (before, after) {
        (Value::Object(lhs), Value::Object(rhs)) => {
            let mut keys = lhs.keys().cloned().collect::<Vec<_>>();
            keys.extend(rhs.keys().cloned());
            keys.sort();
            keys.dedup();
            for key in keys {
                let next_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let left = lhs.get(&key).unwrap_or(&Value::Null);
                let right = rhs.get(&key).unwrap_or(&Value::Null);
                collect_changed_fields(&next_path, left, right, output);
            }
        }
        _ => {
            if before != after {
                output.push(path.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentConfig, AmanConfig, BusMode, ConfigLoader, ConfigReloader};
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn write_temp_file(content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("aman-config-{}.yaml", Uuid::now_v7()));
        fs::write(&path, content).expect("should write temp config file");
        path
    }

    #[test]
    fn layered_loader_honors_precedence() {
        let file = write_temp_file(
            r#"
event_bus:
  mode: persistent
  max_queue_size: 200
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 80
"#,
        );
        let runtime_override = write_temp_file(
            r#"
event_bus:
  max_queue_size: 400
"#,
        );
        let env_patch = super::load_env_patch_from_iter(vec![(
            "AMAN_EVENT_BUS_MAX_QUEUE_SIZE".to_string(),
            "300".to_string(),
        )])
        .expect("env patch should parse");
        let mut expected = AgentConfig::default();
        expected.merge(super::load_partial_from_yaml(&file).expect("file patch should parse"));
        expected.merge(env_patch);
        expected.merge(
            super::load_partial_from_yaml(&runtime_override).expect("runtime patch should parse"),
        );
        let loaded = ConfigLoader::load(Some(&file), Some(&runtime_override))
            .expect("load should succeed");
        assert_eq!(loaded.config.event_bus.max_queue_size, expected.event_bus.max_queue_size);
        assert_eq!(loaded.config.event_bus.mode, BusMode::Persistent);
        assert_eq!(loaded.config.event_bus.max_queue_size, 400);
        assert!(
            loaded
                .audit
                .iter()
                .any(|record| record.changed_fields.iter().any(|f| f == "event_bus.max_queue_size")),
            "expected changed field to be audited"
        );
        assert!(
            loaded.meta.source_chain.first().is_some_and(|layer| layer == "default"),
            "default layer should always be present first"
        );
        assert!(
            loaded.meta.source_chain.iter().any(|layer| layer == "file"),
            "file layer should be recorded"
        );
        assert!(
            loaded
                .meta
                .source_chain
                .iter()
                .any(|layer| layer == "runtime_override"),
            "runtime override layer should be recorded"
        );
        let _ = fs::remove_file(file);
        let _ = fs::remove_file(runtime_override);
    }

    #[test]
    fn validate_rejects_in_memory_with_persistence() {
        let file = write_temp_file(
            r#"
event_bus:
  mode: in_memory
  persistence:
    wal_sync: fsync
    checkpoint_interval: 100
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 20
"#,
        );

        let error = ConfigLoader::load(Some(&file), None).expect_err("should fail");
        assert!(
            error
                .to_string()
                .contains("event_bus.mode=in_memory 不允许配置 event_bus.persistence"),
            "unexpected error: {error}"
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn validate_rejects_invalid_workflow_initial_state() {
        let mut config = AgentConfig::default();
        config.runtime.drain_timeout_sec = 10;
        config.runtime.tool_timeout_sec = 20;
        config.workflow.definitions = vec![super::WorkflowDefinition {
            name: "approval".to_string(),
            states: vec!["PENDING".to_string(), "APPROVED".to_string()],
            initial_state: "REVIEWING".to_string(),
        }];

        let error = config.validate().expect_err("should fail");
        assert!(
            error.to_string().contains("initial_state=REVIEWING 不在 states 中"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_rejects_mutual_exclusive_source_fields() {
        let mut config = AgentConfig::default();
        config.runtime.drain_timeout_sec = 10;
        config.runtime.tool_timeout_sec = 20;
        config.source.notify_on_complete = true;
        config.source.watch_patterns = vec!["*.txt".to_string()];

        let error = config.validate().expect_err("should fail");
        assert!(
            error
                .to_string()
                .contains("source.notify_on_complete 与 source.watch_patterns 互斥"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn config_reloader_emits_config_changed_event_when_file_updates() {
        let file = write_temp_file(
            r#"
event_bus:
  mode: persistent
  max_queue_size: 200
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 80
"#,
        );
        let mut reloader = ConfigReloader::new(Some(file.clone()), None);
        reloader.load_initial().expect("initial load should succeed");

        fs::write(
            &file,
            r#"
event_bus:
  mode: persistent
  max_queue_size: 201
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 80
"#,
        )
        .expect("update config file");

        let event = reloader
            .reload_if_changed()
            .expect("reload should succeed")
            .expect("should emit change event");
        assert_eq!(event.event_type.as_str(), "config_changed");
        let changed = event
            .payload
            .get("changed_fields")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            changed.iter().any(|field| field == "event_bus.max_queue_size"),
            "expected event_bus.max_queue_size change, got {changed:?}"
        );

        let _ = fs::remove_file(file);
    }

    // ── Multi-agent config (P1) tests ────────────────────────

    #[test]
    fn is_valid_identifier_accepts_legal_keys() {
        assert!(super::is_valid_identifier("cortana"));
        assert!(super::is_valid_identifier("deepseek-v4-pro"));
        assert!(super::is_valid_identifier("my_agent_1"));
        assert!(super::is_valid_identifier("a"));
        assert!(super::is_valid_identifier("ABC-123_def"));
    }

    #[test]
    fn is_valid_identifier_rejects_illegal_keys() {
        assert!(!super::is_valid_identifier(""));
        assert!(!super::is_valid_identifier("my agent"));
        assert!(!super::is_valid_identifier("agent/foo"));
        assert!(!super::is_valid_identifier("space key"));
        assert!(!super::is_valid_identifier("中文"));
        assert!(!super::is_valid_identifier("emoji🔥"));
    }

    #[test]
    fn aman_config_validate_full_rejects_invalid_provider_key() {
        let mut config = AmanConfig::default();
        config.runtime.runtime.drain_timeout_sec = 10;
        config.runtime.runtime.tool_timeout_sec = 20;
        config.providers.insert(
            "bad provider!".to_string(),
            super::ProviderConfig {
                display_name: "Bad".to_string(),
                base_url: "https://example.com".to_string(),
                api_key: None,
            },
        );

        let error = config.validate_full().expect_err("should fail");
        assert!(
            error.to_string().contains("只能包含英文字母"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn aman_config_validate_full_rejects_invalid_agent_key() {
        let mut config = AmanConfig::default();
        config.runtime.runtime.drain_timeout_sec = 10;
        config.runtime.runtime.tool_timeout_sec = 20;
        config.providers.insert(
            "valid".to_string(),
            super::ProviderConfig {
                display_name: "Valid".to_string(),
                base_url: "https://example.com".to_string(),
                api_key: None,
            },
        );
        config.agents.insert(
            "".to_string(),
            super::AgentEntryConfig {
                display_name: "Empty".to_string(),
                provider: "valid".to_string(),
                model: "gpt-5".to_string(),
                system_prompt_override: None,
            },
        );

        let error = config.validate_full().expect_err("should fail");
        assert!(
            error.to_string().contains("只能包含英文字母"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn aman_config_validate_full_warns_missing_provider() {
        let mut config = AmanConfig::default();
        config.runtime.runtime.drain_timeout_sec = 10;
        config.runtime.runtime.tool_timeout_sec = 20;
        config.agents.insert(
            "orphan".to_string(),
            super::AgentEntryConfig {
                display_name: "Orphan".to_string(),
                provider: "nonexistent".to_string(),
                model: "gpt-5".to_string(),
                system_prompt_override: None,
            },
        );

        let warnings = config.validate_full().expect("should warn not error");
        assert!(
            warnings.iter().any(|w| w.contains("orphan") && w.contains("nonexistent")),
            "expected warning about missing provider, got {warnings:?}"
        );
    }

    #[test]
    fn aman_config_validate_full_passes_healthy_config() {
        let mut config = AmanConfig::default();
        config.runtime.runtime.drain_timeout_sec = 10;
        config.runtime.runtime.tool_timeout_sec = 20;
        config.providers.insert(
            "openai".to_string(),
            super::ProviderConfig {
                display_name: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: None,
            },
        );
        config.model = Some(super::DefaultModelConfig {
            default: "gpt-5".to_string(),
            provider: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
        });
        config.agents.insert(
            "cortana".to_string(),
            super::AgentEntryConfig {
                display_name: "Cortana".to_string(),
                provider: "openai".to_string(),
                model: "gpt-5".to_string(),
                system_prompt_override: None,
            },
        );

        let warnings = config.validate_full().expect("should pass");
        assert!(warnings.is_empty(), "expected no warnings, got {warnings:?}");
    }

    #[test]
    fn aman_config_save_and_from_file_roundtrip() {
        let yaml = r#"
runtime:
  drain_timeout_sec: 30
  tool_timeout_sec: 60
event_bus:
  mode: persistent
  max_queue_size: 500
providers:
  openai:
    display_name: OpenAI
    base_url: https://api.openai.com/v1
model:
  default: gpt-5
  provider: openai
  base_url: https://api.openai.com/v1
agents:
  cortana:
    display_name: Cortana
    provider: openai
    model: gpt-5
"#;
        let path = write_temp_file(yaml);
        let config = AmanConfig::from_file(&path).expect("should parse config");

        assert_eq!(config.runtime.event_bus.mode, BusMode::Persistent);
        assert_eq!(config.runtime.event_bus.max_queue_size, 500);
        assert_eq!(config.providers.len(), 1);
        assert!(config.providers.contains_key("openai"));
        assert_eq!(
            config.providers["openai"].display_name,
            "OpenAI"
        );
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents["cortana"].provider, "openai");

        // Round-trip: save → re-read
        let save_path = write_temp_file("");
        config.save(&save_path).expect("should save");
        let reloaded = AmanConfig::from_file(&save_path).expect("should re-parse");
        assert_eq!(reloaded.providers.len(), 1);
        assert_eq!(reloaded.agents.len(), 1);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(save_path);
    }

    #[test]
    fn aman_config_default_is_empty() {
        let config = AmanConfig::default();
        assert!(config.providers.is_empty());
        assert!(config.model.is_none());
        assert!(config.agents.is_empty());
    }

    // ── Idle config tests ───────────────────────────────────────

    #[test]
    fn idle_config_defaults() {
        let mut config = AgentConfig::default();
        config.runtime.drain_timeout_sec = 10;
        config.runtime.tool_timeout_sec = 20;
        let warnings = config.validate().expect("default idle config should validate");
        assert!(
            warnings.iter().all(|w| !w.contains("idle")),
            "no idle-related warnings expected: {warnings:?}"
        );
        assert!(config.idle.enabled);
        assert!((config.idle.arousal.initial_value - 1.0).abs() < f64::EPSILON);
        assert!((config.idle.arousal.half_life_secs - 900.0).abs() < f64::EPSILON);
        assert_eq!(config.idle.context.max_output_buffer, 10);
        assert_eq!(config.idle.reflection.timeout_secs, 30);
        assert_eq!(config.idle.sleep.short_term_retention_days, 7);
        assert_eq!(config.idle.exploration.api_rate_per_minute, 10);
    }

    #[test]
    fn idle_config_rejects_allowed_kinds_not_in_enabled() {
        let yaml = r#"
idle:
  enabled: true
  personality:
    enabled_kinds: [daze, boredom]
    depth_schedule:
      - [0, daze]
      - [1, boredom]
    poll_interval:
      interval_secs: 5.0
    poll_relaxation: none
    chat_mode:
      allowed_kinds: [exploration]
      grace_period_secs: 30
      poll_interval:
        interval_secs: 2.0
    reflection_breaker:
      max_consecutive: 5
      cooldown_secs: 300
    context_isolation:
      pollute_chat_history: false
      suspend_on_user_input: true
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 20
"#;
        let err = ConfigLoader::load(Some(&write_temp_file(yaml)), None)
            .expect_err("should reject allowed_kinds not in enabled_kinds");
        assert!(
            err.to_string().contains("allowed_kinds"),
            "expected validation error about allowed_kinds, got: {err}"
        );
    }

    #[test]
    fn idle_config_rejects_zero_max_consecutive() {
        let yaml = r#"
idle:
  enabled: true
  personality:
    enabled_kinds: [daze, boredom]
    depth_schedule:
      - [0, daze]
      - [1, boredom]
    poll_interval:
      interval_secs: 5.0
    poll_relaxation: none
    chat_mode:
      allowed_kinds: [daze]
      grace_period_secs: 30
      poll_interval:
        interval_secs: 2.0
    reflection_breaker:
      max_consecutive: 0
      cooldown_secs: 300
    context_isolation:
      pollute_chat_history: false
      suspend_on_user_input: true
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 20
"#;
        let err = ConfigLoader::load(Some(&write_temp_file(yaml)), None)
            .expect_err("should reject max_consecutive=0");
        assert!(
            err.to_string().contains("max_consecutive"),
            "expected validation error about max_consecutive, got: {err}"
        );
    }

    #[test]
    fn idle_config_disabled_skips_validation() {
        let yaml = r#"
idle:
  enabled: false
  personality:
    enabled_kinds: [daze]
    depth_schedule:
      - [0, daze]
    poll_interval:
      interval_secs: 5.0
    poll_relaxation: none
    chat_mode:
      allowed_kinds: [exploration]
      grace_period_secs: 30
      poll_interval:
        interval_secs: 2.0
    reflection_breaker:
      max_consecutive: 0
      cooldown_secs: 300
    context_isolation:
      pollute_chat_history: false
      suspend_on_user_input: true
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 20
"#;
        // When idle is disabled, validation should pass despite bad allowed_kinds
        let result = ConfigLoader::load(Some(&write_temp_file(yaml)), None);
        assert!(result.is_ok(), "disabled idle config should validate: {:?}", result.err());
    }
}
