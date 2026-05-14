#![forbid(unsafe_code)]
#![doc = "Configuration model and layered loader for the Aman agent framework."]

use kernel::event::{Event, EventType};
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
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
    pub drain_timeout_sec: u64,
    pub tool_timeout_sec: u64,
}

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
    pub wal_sync: String,
    pub checkpoint_interval: u64,
}

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
    pub mode: BusMode,
    pub max_queue_size: usize,
    pub persistence: Option<PersistenceConfig>,
}

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
    pub enforce_dependency_check: bool,
}

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
    pub notify_on_complete: bool,
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
    pub definitions: Vec<WorkflowDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub struct SecurityConfig {
    pub risky_capabilities_enabled: bool,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    pub runtime: RuntimeConfig,
    pub event_bus: EventBusConfig,
    pub plugin: PluginConfig,
    pub source: SourceConfig,
    pub workflow: WorkflowConfig,
    pub security: SecurityConfig,
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialAgentConfig {
    pub runtime: Option<PartialRuntimeConfig>,
    pub event_bus: Option<PartialEventBusConfig>,
    pub plugin: Option<PartialPluginConfig>,
    pub source: Option<PartialSourceConfig>,
    pub workflow: Option<PartialWorkflowConfig>,
    pub security: Option<PartialSecurityConfig>,
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
    use super::{AgentConfig, BusMode, ConfigLoader, ConfigReloader};
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
}
