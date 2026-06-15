#![deny(unsafe_code)]
#![doc = "Plugin manifest, dependency graph, and lifecycle loader for aman."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

pub mod bridge;

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use kernel::context::{BaseContext, PluginContext, PluginTrackedResources};
use kernel::hook::Hook;
use kernel::memory::MemoryProvider;
use kernel::plugin::{Plugin, PluginDependency};
use kernel::security::{ApprovalCache, CapabilitySet, TemplateContext};
use kernel::skill::Skill;
use sandbox::SandboxConfig;
use kernel::source::EventSource;
use kernel::tool::Tool;
use kernel::types::{TraceId, TrustLevel};
use kernel::{AmanResult, Error};
use futures::future::{select, Either};
use futures::pin_mut;
use futures_timer::Delay;
use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::Archive;
use uuid::Uuid;
use wasmtime::{Config as WasmConfig, Engine, Instance, Module, Store};

/// Default subprocess timeout in milliseconds (30 seconds).
const PLUGIN_TIMEOUT_MS: u64 = 30_000;
/// Maximum WASM manifest and stack size in bytes (1 MB).
const MAX_MANIFEST_SIZE: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: Version,
    #[serde(default)]
    pub depends_on: Vec<PluginDependency>,
    #[serde(default)]
    pub lifecycle: PluginLifecycleConfig,
    #[serde(default)]
    pub exports: PluginExports,
    pub config_schema: Option<Value>,
    #[serde(default)]
    pub isolation: Option<PluginIsolationMode>,
    #[serde(default)]
    pub subprocess: Option<SubprocessPluginConfig>,
    #[serde(default)]
    pub wasm_path: Option<String>,
    /// Optional list of capabilities this plugin provides (e.g., "chat", "session_management").
    /// Added for LLM Chat capability framework. §2 Decision 2.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional UI declaration for pages and events this plugin contributes.
    #[serde(default)]
    pub ui: Option<UiDeclaration>,
    /// Script runtime for subprocess plugins (e.g. "python3", "node", "bash").
    /// When set, overrides `subprocess.command` with the runtime value.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Minimum runtime version (semver range, e.g. ">=3.11").
    #[serde(default)]
    pub min_version: Option<String>,
    /// Entrypoint script relative to the plugin directory.
    #[serde(default)]
    pub entrypoint: Option<PathBuf>,
    /// Security manifest declaring requested capabilities and trust level.
    /// When absent, the plugin runs with minimal (default) capabilities.
    #[serde(default)]
    pub security: Option<PluginSecurityManifest>,
}

/// Security manifest within a plugin's declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSecurityManifest {
    /// Capabilities the plugin requests. Must be approved by the operator
    /// on first load. Subsequent loads auto-approve if no new caps appear.
    #[serde(default)]
    pub requested_capabilities: CapabilitySet,
    /// Minimum trust level at which the plugin may run.
    #[serde(default)]
    pub minimum_trust_level: Option<TrustLevel>,
}

/// Declares UI pages and events contributed by a plugin.
/// Added for LLM Chat capability framework. §2 Decision 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiDeclaration {
    #[serde(default)]
    pub pages: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginLifecycleConfig {
    pub auto_start: bool,
}

/// The set of capabilities a plugin declares it exports (in
/// `PluginManifest::exports`) and the set actually registered
/// at runtime (in `LoadedPlugin::exports`). Both stages use
/// the same shape — a list of skill/tool/event_source/hook/
/// memory_provider names — so P3-21 from
/// docs/code-review-20260614.md unifies them as a single type
/// (`RegisteredExports`) with `PluginExports` kept as a
/// type alias for the manifest side.
///
/// The alias preserves downstream call sites
/// (`PluginManifest::exports: PluginExports { ... }`) without
/// any consumer needing to change.
pub type PluginExports = RegisteredExports;

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: Version::new(0, 0, 0),
            depends_on: Vec::new(),
            lifecycle: PluginLifecycleConfig::default(),
            exports: PluginExports::default(),
            config_schema: None,
            isolation: None,
            subprocess: None,
            wasm_path: None,
            capabilities: Vec::new(),
            ui: None,
            runtime: None,
            min_version: None,
            entrypoint: None,
            security: None,
        }
    }
}

impl PluginManifest {
    pub fn parse(content: &str) -> AmanResult<Self> {
        serde_yaml::from_str::<Self>(content).map_err(|error| Error::ConfigInvalid {
            message: format!("invalid plugin manifest yaml: {error}"),
        })
    }

    pub fn from_file(path: &Path) -> AmanResult<Self> {
        let content = fs::read_to_string(path)?;
        Self::parse(&content)
    }
}

impl std::str::FromStr for PluginManifest {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[must_use]
pub fn normalize_version_req(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains(',') {
        return trimmed.to_owned();
    }
    trimmed.split_whitespace().collect::<Vec<_>>().join(",")
}

pub fn version_matches_range(version: &Version, range: &str) -> AmanResult<bool> {
    let normalized = normalize_version_req(range);
    if normalized.is_empty() {
        return Ok(true);
    }
    let req = VersionReq::parse(&normalized).map_err(|error| Error::ConfigInvalid {
        message: format!("invalid semver range `{range}`: {error}"),
    })?;
    Ok(req.matches(version))
}

#[derive(Debug, Clone)]
pub struct DependencyGraph {
    manifests: HashMap<String, PluginManifest>,
}

impl DependencyGraph {
    pub fn new(manifests: Vec<PluginManifest>) -> AmanResult<Self> {
        let mut indexed = HashMap::new();
        for manifest in manifests {
            if indexed.contains_key(&manifest.name) {
                return Err(Error::AlreadyExists {
                    name: format!("plugin:{}", manifest.name),
                });
            }
            indexed.insert(manifest.name.clone(), manifest);
        }
        Ok(Self { manifests: indexed })
    }

    pub fn topological_order(&self) -> AmanResult<Vec<String>> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Visit {
            Visiting,
            Visited,
        }

        fn dfs(
            node: &str,
            graph: &DependencyGraph,
            marks: &mut HashMap<String, Visit>,
            stack: &mut Vec<String>,
            order: &mut Vec<String>,
        ) -> AmanResult<()> {
            if let Some(mark) = marks.get(node).copied() {
                return match mark {
                    Visit::Visited => Ok(()),
                    Visit::Visiting => {
                        let start = stack.iter().position(|item| item == node).unwrap_or(0);
                        let mut cycle = stack[start..].to_vec();
                        cycle.push(node.to_owned());
                        Err(Error::CycleDetected {
                            path: cycle.join(" -> "),
                        })
                    }
                };
            }

            marks.insert(node.to_owned(), Visit::Visiting);
            stack.push(node.to_owned());

            let manifest = graph
                .manifests
                .get(node)
                .ok_or_else(|| Error::NotFound {
                    name: format!("plugin:{node}"),
                })?;

            for dep in &manifest.depends_on {
                let dep_manifest = graph
                    .manifests
                    .get(&dep.name)
                    .ok_or_else(|| Error::NotFound {
                        name: format!("plugin dependency {} for {}", dep.name, manifest.name),
                    })?;
                if !version_matches_range(&dep_manifest.version, &dep.version_range)? {
                    return Err(Error::VersionMismatch {
                        expected: dep.version_range.clone(),
                        found: dep_manifest.version.to_string(),
                    });
                }
                dfs(&dep.name, graph, marks, stack, order)?;
            }

            stack.pop();
            marks.insert(node.to_owned(), Visit::Visited);
            order.push(node.to_owned());
            Ok(())
        }

        let mut names = self.manifests.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let mut marks = HashMap::new();
        let mut stack = Vec::new();
        let mut order = Vec::new();
        for name in names {
            if !marks.contains_key(&name) {
                dfs(&name, self, &mut marks, &mut stack, &mut order)?;
            }
        }
        Ok(order)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginIsolationMode {
    InProcess,
    Subprocess,
    Wasm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubprocessPluginConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default = "default_subprocess_timeout_ms")]
    pub timeout_ms: u64,
}

const fn default_subprocess_timeout_ms() -> u64 {
    PLUGIN_TIMEOUT_MS
}

#[derive(Debug, Clone)]
pub struct SubprocessPluginClient {
    config: SubprocessPluginConfig,
}

impl SubprocessPluginClient {
    #[must_use]
    pub fn new(config: SubprocessPluginConfig) -> Self {
        Self { config }
    }

    pub fn on_load(&self, plugin_name: &str, version: &Version) -> AmanResult<Value> {
        self.invoke(
            "aman_plugin_on_load",
            serde_json::json!({
                "plugin_name": plugin_name,
                "version": version.to_string(),
            }),
        )
    }

    pub fn on_unload(&self, plugin_name: &str) -> AmanResult<Value> {
        self.invoke(
            "aman_plugin_on_unload",
            serde_json::json!({
                "plugin_name": plugin_name,
            }),
        )
    }

    pub fn on_dependency_unloading(&self, plugin_name: &str, dependency: &str) -> AmanResult<Value> {
        self.invoke(
            "aman_plugin_on_dependency_unloading",
            serde_json::json!({
                "plugin_name": plugin_name,
                "dependency": dependency,
            }),
        )
    }

    pub fn invoke(&self, method: &str, params: Value) -> AmanResult<Value> {
        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &self.config.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| Error::Unrecoverable {
            message: format!("failed to spawn subprocess plugin command: {error}"),
        })?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        if let Some(stdin) = child.stdin.as_mut() {
            let payload = format!("{request}\n");
            stdin
                .write_all(payload.as_bytes())
                .map_err(|error| Error::Unrecoverable {
                    message: format!("failed to write subprocess request: {error}"),
                })?;
        } else {
            return Err(Error::Unrecoverable {
                message: "subprocess stdin not available".to_owned(),
            });
        }

        let stdout = child.stdout.take().ok_or_else(|| Error::Unrecoverable {
            message: "subprocess stdout not available".to_owned(),
        })?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|error| Error::Unrecoverable {
            message: format!("failed to read subprocess response: {error}"),
        })?;
        if bytes == 0 {
            let stderr = child.wait_with_output().ok().and_then(|out| {
                let text = String::from_utf8(out.stderr).ok()?;
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text)
                }
            });
            return Err(Error::Unrecoverable {
                message: format!(
                    "subprocess returned empty response{}",
                    stderr
                        .as_deref()
                        .map(|msg| format!(", stderr: {}", msg.trim()))
                        .unwrap_or_default()
                ),
            });
        }

        let response: JsonRpcResponse = serde_json::from_str(line.trim()).map_err(|error| {
            Error::ConfigInvalid {
                message: format!("invalid subprocess json-rpc response: {error}"),
            }
        })?;
        if response.jsonrpc != "2.0" {
            return Err(Error::ConfigInvalid {
                message: format!("unsupported jsonrpc version: {}", response.jsonrpc),
            });
        }
        if response.id != 1 {
            return Err(Error::ConfigInvalid {
                message: format!("unexpected jsonrpc response id: {}", response.id),
            });
        }
        if let Some(error) = response.error {
            return Err(Error::Unrecoverable {
                message: format!("subprocess rpc error {}: {}", error.code, error.message),
            });
        }
        Ok(response.result.unwrap_or(Value::Null))
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: u64,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ── WASM Security Configuration ───────────────────────────────────────

/// Security constraints for WASM plugin execution.
///
/// These limits are enforced by the wasmtime runtime via fuel metering and
/// epoch-based interruption. Once fuel is exhausted or the epoch deadline is
/// reached, the WASM module is trapped and the plugin is terminated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmSecurityConfig {
    /// Maximum linear memory the WASM module may allocate, in bytes.
    /// Default: 500 MB (524_288_000 bytes).
    #[serde(default = "default_wasm_max_memory_bytes")]
    pub max_memory_bytes: u64,

    /// Maximum number of table elements (for indirect call tables).
    /// Default: 10_000.
    #[serde(default = "default_wasm_max_table_elements")]
    pub max_table_elements: u32,

    /// Total fuel units allocated to the module. Each WASM instruction
    /// consumes one fuel unit. When fuel reaches zero, the module is trapped.
    /// Default: 100_000_000 (100M instructions).
    #[serde(default = "default_wasm_max_fuel")]
    pub max_fuel: u64,

    /// Epoch counter tick limit. The host increments the epoch counter
    /// periodically; when it reaches this threshold, the WASM module is
    /// interrupted. Default: 1_000_000.
    #[serde(default = "default_wasm_epoch_ticks")]
    pub epoch_interruption_ticks: u64,
}

const fn default_wasm_max_memory_bytes() -> u64 { 524_288_000 } // 500 MB
const fn default_wasm_max_table_elements() -> u32 { 10_000 }
const fn default_wasm_max_fuel() -> u64 { 100_000_000 }
const fn default_wasm_epoch_ticks() -> u64 { 1_000_000 }

impl Default for WasmSecurityConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: default_wasm_max_memory_bytes(),
            max_table_elements: default_wasm_max_table_elements(),
            max_fuel: default_wasm_max_fuel(),
            epoch_interruption_ticks: default_wasm_epoch_ticks(),
        }
    }
}

#[derive(Clone)]
pub struct WasmPluginRuntime {
    engine: Engine,
    module: Module,
    security_config: WasmSecurityConfig,
}

impl WasmPluginRuntime {
    pub fn from_wasm_bytes(wasm_bytes: &[u8], security_config: Option<WasmSecurityConfig>) -> AmanResult<Self> {
        let sec = security_config.unwrap_or_default();

        let mut config = WasmConfig::new();
        // Enable fuel metering — each WASM instruction burns 1 fuel unit
        config.consume_fuel(true);
        // Enable epoch-based interruption for runaway modules
        config.epoch_interruption(true);
        // Stack depth limit
        config.max_wasm_stack(MAX_MANIFEST_SIZE); // 1 MB stack

        let engine = Engine::new(&config).map_err(|error| Error::ConfigInvalid {
            message: format!("failed to create wasmtime engine with security config: {error}"),
        })?;
        let module = Module::new(&engine, wasm_bytes).map_err(|error| Error::ConfigInvalid {
            message: format!("failed to compile wasm module: {error}"),
        })?;
        Ok(Self { engine, module, security_config: sec })
    }

    pub fn on_load(&self) -> AmanResult<()> {
        let result = self.invoke_i32_export("aman_skill_on_load")?;
        if result != 0 {
            return Err(Error::Unrecoverable {
                message: format!("aman_skill_on_load returned non-zero status: {result}"),
            });
        }
        Ok(())
    }

    pub fn on_unload(&self) -> AmanResult<()> {
        let result = self.invoke_i32_export("aman_skill_on_unload")?;
        if result != 0 {
            return Err(Error::Unrecoverable {
                message: format!("aman_skill_on_unload returned non-zero status: {result}"),
            });
        }
        Ok(())
    }

    pub fn execute_skill(&self) -> AmanResult<i32> {
        self.invoke_i32_export("aman_skill_execute")
    }

    fn instantiate(&self) -> AmanResult<(Store<()>, Instance)> {
        let mut store = Store::new(&self.engine, ());

        // Seed fuel budget — once exhausted, the module traps.
        // 100M fuel units ≈ 100M WASM instructions before forced termination.
        store.set_fuel(self.security_config.max_fuel).map_err(|error| Error::ConfigInvalid {
            message: format!("failed to seed wasm fuel: {error}"),
        })?;

        // Set epoch deadline for interruption.
        // The host increments the epoch counter periodically; when it
        // reaches this threshold, the WASM module is interrupted.
        store.set_epoch_deadline(self.security_config.epoch_interruption_ticks);

        let instance = Instance::new(&mut store, &self.module, &[]).map_err(|error| {
            Error::ConfigInvalid {
                message: format!("failed to instantiate wasm module: {error}"),
            }
        })?;
        Ok((store, instance))
    }

    fn invoke_i32_export(&self, export_name: &str) -> AmanResult<i32> {
        let (mut store, instance) = self.instantiate()?;
        let function = instance
            .get_typed_func::<(), i32>(&mut store, export_name)
            .map_err(|error| Error::ConfigInvalid {
                message: format!("missing or invalid export `{export_name}`: {error}"),
            })?;

        // Reset epoch deadline before each call
        store.set_epoch_deadline(self.security_config.epoch_interruption_ticks);

        function.call(&mut store, ()).map_err(|error| Error::Unrecoverable {
            message: format!("wasm export `{export_name}` execution failed: {error}"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginLifecycleState {
    Loaded,
    Enabled,
    Running,
    Paused,
    Disabled,
    Shutdown,
}

/// A plugin ready to be loaded, with the variant dictating which
/// isolation mode the gateway will use.
///
/// P3-21 from docs/code-review-20260614.md: the previous flat
/// struct exposed `Box<dyn Plugin> + PluginIsolationMode flag +
/// 3 Option<*>` as independent public fields, leaving the
/// invariant "an InProcess candidate has `subprocess = None`
/// and `wasm_module_bytes = None`; a Subprocess candidate has
/// the Box and the config; a Wasm candidate has the Box and
/// the bytes" entirely implicit and unenforced. Encoding as
/// an enum makes the variants explicit: each variant carries
/// only the data it actually needs, and `load_plugin_inner`
/// dispatches on the variant directly (no more "what does
/// `isolation = InProcess` but `subprocess = Some(_)` even
/// mean?" questions).
pub enum PluginCandidate {
    /// In-process plugin: the trait object is loaded and ready
    /// to call `on_load` synchronously at load time.
    InProcess {
        manifest: PluginManifest,
        plugin: Box<dyn Plugin>,
    },
    /// Subprocess-isolated plugin: the gateway will spawn a
    /// subprocess when the plugin is loaded. The `stub` is a
    /// lightweight `Box<dyn Plugin>` used only by the discovery
    /// / pre-registration path (so `register_exports` sees a
    /// consistent set of names); the actual plugin logic lives
    /// in the subprocess spawned via `config`.
    Subprocess {
        manifest: PluginManifest,
        config: SubprocessPluginConfig,
        stub: Box<dyn Plugin>,
    },
    /// WASM-isolated plugin: the gateway will instantiate the
    /// module from `bytes` at load time. Same stub-vs-actual
    /// split as `Subprocess`.
    Wasm {
        manifest: PluginManifest,
        bytes: Vec<u8>,
        stub: Box<dyn Plugin>,
    },
}

/// Get the manifest name from a `PluginCandidate` variant.
pub fn plugin_manifest_name(c: &PluginCandidate) -> &str {
    match c {
        PluginCandidate::InProcess { manifest, .. } => &manifest.name,
        PluginCandidate::Subprocess { manifest, .. } => &manifest.name,
        PluginCandidate::Wasm { manifest, .. } => &manifest.name,
    }
}

/// Get the manifest version from a `PluginCandidate` variant.
pub fn plugin_manifest_version(c: &PluginCandidate) -> &semver::Version {
    match c {
        PluginCandidate::InProcess { manifest, .. } => &manifest.version,
        PluginCandidate::Subprocess { manifest, .. } => &manifest.version,
        PluginCandidate::Wasm { manifest, .. } => &manifest.version,
    }
}

/// Get the security manifest from a `PluginCandidate` variant.
pub fn plugin_manifest_security(c: &PluginCandidate) -> &Option<PluginSecurityManifest> {
    match c {
        PluginCandidate::InProcess { manifest, .. } => &manifest.security,
        PluginCandidate::Subprocess { manifest, .. } => &manifest.security,
        PluginCandidate::Wasm { manifest, .. } => &manifest.security,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstallResult {
    pub manifest: PluginManifest,
    pub install_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PluginInstaller {
    plugins_root: PathBuf,
}

impl PluginInstaller {
    #[must_use]
    pub fn new(plugins_root: PathBuf) -> Self {
        Self { plugins_root }
    }

    pub fn install_from_archive(&self, archive_path: &Path) -> AmanResult<PluginInstallResult> {
        fs::create_dir_all(&self.plugins_root)?;
        let staging = self
            .plugins_root
            .join(".staging")
            .join(Uuid::now_v7().to_string());
        fs::create_dir_all(&staging)?;

        let extracted = (|| -> AmanResult<PluginInstallResult> {
            let archive_file = fs::File::open(archive_path)?;
            let decoder = GzDecoder::new(archive_file);
            let mut archive = Archive::new(decoder);
            archive.unpack(&staging).map_err(|error| Error::ConfigInvalid {
                message: format!("invalid plugin archive: {error}"),
            })?;

            let manifests = parse_plugin_manifest_paths(&staging);
            if manifests.len() != 1 {
                return Err(Error::ConfigInvalid {
                    message: format!(
                        "plugin archive must contain exactly one plugin.yaml, found {}",
                        manifests.len()
                    ),
                });
            }
            let manifest_path = manifests
                .first()
                .map(PathBuf::from)
                .ok_or_else(|| Error::ConfigInvalid {
                    message: "plugin archive is missing plugin.yaml".to_owned(),
                })?;
            let manifest = PluginManifest::from_file(&manifest_path)?;
            let source_dir = manifest_path
                .parent()
                .map(PathBuf::from)
                .ok_or_else(|| Error::ConfigInvalid {
                    message: "plugin manifest path has no parent".to_owned(),
                })?;

            let install_dir = self.plugins_root.join(&manifest.name);
            if install_dir.exists() {
                return Err(Error::AlreadyExists {
                    name: format!("plugin:{}", manifest.name),
                });
            }

            if let Some(parent) = install_dir.parent() {
                fs::create_dir_all(parent)?;
            }
            move_or_copy_dir(&source_dir, &install_dir)?;
            Ok(PluginInstallResult {
                manifest,
                install_dir,
            })
        })();

        let _ = fs::remove_dir_all(&staging);
        extracted
    }

    pub fn install_from_archive_bytes(&self, archive_bytes: &[u8]) -> AmanResult<PluginInstallResult> {
        fs::create_dir_all(&self.plugins_root)?;
        let upload_dir = self.plugins_root.join(".uploads");
        fs::create_dir_all(&upload_dir)?;
        let archive_path = upload_dir.join(format!("{}.tar.gz", Uuid::now_v7()));
        fs::write(&archive_path, archive_bytes)?;
        let installed = self.install_from_archive(&archive_path);
        let _ = fs::remove_file(&archive_path);
        installed
    }

    pub async fn uninstall(
        &self,
        loader: Option<&mut PluginLoader>,
        plugin_name: &str,
    ) -> AmanResult<()> {
        if let Some(loader) = loader
            && loader.state_of(plugin_name).is_some() {
                loader.unload_plugin(plugin_name).await?;
            }
        self.remove_plugin_files(plugin_name)
    }

    pub fn remove_plugin_files(&self, plugin_name: &str) -> AmanResult<()> {
        let target = self.plugins_root.join(plugin_name);
        if !target.exists() {
            return Err(Error::NotFound {
                name: format!("plugin:{plugin_name}"),
            });
        }
        fs::remove_dir_all(target)?;
        Ok(())
    }
}

#[derive(Clone)]
struct PluginHttpState {
    installer: Arc<PluginInstaller>,
}

#[derive(Debug, Serialize)]
struct InstallPluginResponse {
    plugin_name: String,
    version: String,
    install_dir: String,
}

#[derive(Debug, Serialize)]
struct ApiErrorResponse {
    error: String,
}

pub fn plugin_management_router(installer: Arc<PluginInstaller>) -> Router {
    Router::new()
        .route("/plugin/install", post(install_plugin_handler))
        .with_state(PluginHttpState { installer })
}

async fn install_plugin_handler(
    State(state): State<PluginHttpState>,
    mut multipart: Multipart,
) -> Response {
    let mut archive_bytes = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let is_plugin_field = field.name() == Some("plugin");
                let filename_match = field
                    .file_name()
                    .map(|name| name.ends_with(".tar.gz"))
                    .unwrap_or(false);
                if is_plugin_field || filename_match {
                    match field.bytes().await {
                        Ok(bytes) => {
                            archive_bytes = Some(bytes.to_vec());
                            break;
                        }
                        Err(error) => {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(ApiErrorResponse {
                                    error: format!("failed to read multipart field: {error}"),
                                }),
                            )
                                .into_response();
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiErrorResponse {
                        error: format!("invalid multipart payload: {error}"),
                    }),
                )
                    .into_response();
            }
        }
    }

    let Some(archive_bytes) = archive_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "multipart must contain `plugin` file field".to_owned(),
            }),
        )
            .into_response();
    };

    let installer = Arc::clone(&state.installer);
    let install_result =
        tokio::task::spawn_blocking(move || installer.install_from_archive_bytes(&archive_bytes))
            .await;

    match install_result {
        Ok(Ok(installed)) => (
            StatusCode::OK,
            Json(InstallPluginResponse {
                plugin_name: installed.manifest.name,
                version: installed.manifest.version.to_string(),
                install_dir: installed.install_dir.display().to_string(),
            }),
        )
            .into_response(),
        Ok(Err(Error::AlreadyExists { name })) => (
            StatusCode::CONFLICT,
            Json(ApiErrorResponse {
                error: format!("plugin already exists: {name}"),
            }),
        )
            .into_response(),
        Ok(Err(Error::ConfigInvalid { message })) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse { error: message }),
        )
            .into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("install task join error: {error}"),
            }),
        )
            .into_response(),
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RegisteredExports {
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub event_sources: Vec<String>,
    pub hooks: Vec<String>,
    pub memory_providers: Vec<String>,
}

enum LoadedPluginRuntime {
    InProcess(Box<dyn Plugin>),
    Subprocess(Arc<bridge::SubprocessPluginBridge>),
    Wasm(WasmPluginRuntime),
}

struct LoadedPlugin {
    manifest: PluginManifest,
    runtime: LoadedPluginRuntime,
    state: PluginLifecycleState,
    exports: RegisteredExports,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLoaderConfig {
    pub unload_timeout: Duration,
    pub unstable_after_timeouts: u8,
    /// Path to `~/.aman/` — used to resolve `${aman.data_dir}` templates
    /// in plugin capability paths. Defaults to `$HOME/.aman`.
    pub aman_data_dir: PathBuf,
}

impl Default for PluginLoaderConfig {
    fn default() -> Self {
        let aman_data_dir = std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(".aman"))
            .unwrap_or_else(|_| PathBuf::from("/tmp/.aman"));
        Self {
            unload_timeout: Duration::from_secs(30),
            unstable_after_timeouts: 3,
            aman_data_dir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PluginHealth {
    pub consecutive_unload_timeouts: u8,
    pub unstable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginAuditEventType {
    LoadStarted,
    LoadSucceeded,
    LoadFailed,
    OnLoadInterrupted,
    RollbackStarted,
    RollbackReleased,
    UnloadStarted,
    UnloadSucceeded,
    UnloadTimeout,
    DependencyUnloadingNotified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAuditEvent {
    pub timestamp_ms: u64,
    pub plugin_name: String,
    pub event_type: PluginAuditEventType,
    pub message: String,
}

pub trait PluginAuditLogger: Send + Sync {
    fn record(&self, event: PluginAuditEvent);
}

#[derive(Default)]
pub struct NoopPluginAuditLogger;

impl PluginAuditLogger for NoopPluginAuditLogger {
    fn record(&self, _event: PluginAuditEvent) {}
}

#[derive(Default)]
pub struct InMemoryPluginAuditLogger {
    events: Mutex<Vec<PluginAuditEvent>>,
}

impl InMemoryPluginAuditLogger {
    #[must_use]
    pub fn events(&self) -> Vec<PluginAuditEvent> {
        self.events.lock().expect("plugin audit events lock").clone()
    }
}

impl PluginAuditLogger for InMemoryPluginAuditLogger {
    fn record(&self, event: PluginAuditEvent) {
        self.events
            .lock()
            .expect("plugin audit events lock")
            .push(event);
    }
}

pub trait PluginExportRegistrar: Send + Sync {
    fn register_skill(&self, skill: Arc<dyn Skill>) -> AmanResult<()>;
    fn unregister_skill(&self, skill_name: &str) -> AmanResult<()>;

    fn register_tool(&self, tool: Arc<dyn Tool>) -> AmanResult<()>;
    fn unregister_tool(&self, tool_name: &str) -> AmanResult<()>;

    fn register_event_source(&self, source: Arc<dyn EventSource>) -> AmanResult<()>;
    fn unregister_event_source(&self, source_id: &str) -> AmanResult<()>;

    fn register_hook(&self, hook: Arc<dyn Hook>) -> AmanResult<()>;
    fn unregister_hook(&self, hook_name: &str) -> AmanResult<()>;

    fn register_memory_provider(&self, provider: Arc<dyn MemoryProvider>) -> AmanResult<()>;
    fn unregister_memory_provider(&self, provider_name: &str) -> AmanResult<()>;
}

#[derive(Default, macros::Noop)]
pub struct NoopPluginRegistrar;

pub struct PluginLoader {
    registrar: Arc<dyn PluginExportRegistrar>,
    config: PluginLoaderConfig,
    audit_logger: Arc<dyn PluginAuditLogger>,
    method_handler: Arc<dyn kernel::plugin::JsonRpcMethodHandler>,
    loaded: HashMap<String, LoadedPlugin>,
    load_order: Vec<String>,
    health: HashMap<String, PluginHealth>,
    /// Optional approval cache for capability-based access control.
    approval_cache: Option<ApprovalCache>,
}

/// Generates iteration blocks for registering plugin exports.
///
/// Each entry is a tuple: `(plural_method, field, register_method, name_method)`.
/// On first error the loop rolls back already-registered items via
/// `unregister_exports` and returns the error.
macro_rules! register_exports_block {
    ($self:expr, $plugin:expr, $exports:expr,
     $(($plural_method:ident, $field:ident, $register_method:ident, $name_method:ident)),+ $(,)?) => {
        $(
            for item in $plugin.$plural_method() {
                $exports.$field.push(item.$name_method().to_owned());
                if let Err(error) = $self.registrar.$register_method(item) {
                    $self.unregister_exports(&$exports, $plugin.name())?;
                    return Err(error);
                }
            }
        )+
    };
}

/// Generates iteration blocks for unregistering plugin exports.
///
/// Each entry is a tuple: `(field, unregister_method)`.
/// Iteration order is reversed relative to registration (containers before
/// resources). The first error is saved and returned after all items are processed.
macro_rules! unregister_exports_block {
    ($self:expr, $exports:expr,
     $(($field:ident, $unregister_method:ident)),+ $(,)?) => {
        let mut first_error: Option<kernel::Error> = None;
        $(
            for item_name in &$exports.$field {
                if let Err(error) = $self.registrar.$unregister_method(item_name)
                    && first_error.is_none() {
                        first_error = Some(error);
                    }
            }
        )+
        if let Some(error) = first_error {
            return Err(error);
        }
    };
}

impl PluginLoader {
    #[must_use]
    pub fn new(registrar: Arc<dyn PluginExportRegistrar>) -> Self {
        Self::with_config(registrar, PluginLoaderConfig::default())
    }

    #[must_use]
    pub fn with_config(registrar: Arc<dyn PluginExportRegistrar>, config: PluginLoaderConfig) -> Self {
        Self {
            registrar,
            config,
            audit_logger: Arc::new(NoopPluginAuditLogger),
            method_handler: Arc::new(kernel::plugin::NoopJsonRpcHandler),
            loaded: HashMap::new(),
            load_order: Vec::new(),
            health: HashMap::new(),
            approval_cache: None,
        }
    }

    #[must_use]
    pub fn with_audit_logger(mut self, audit_logger: Arc<dyn PluginAuditLogger>) -> Self {
        self.audit_logger = audit_logger;
        self
    }

    #[must_use]
    pub fn with_method_handler(mut self, handler: Arc<dyn kernel::plugin::JsonRpcMethodHandler>) -> Self {
        self.method_handler = handler;
        self
    }

    #[must_use]
    pub fn with_approval_cache(mut self, cache: ApprovalCache) -> Self {
        self.approval_cache = Some(cache);
        self
    }

    #[must_use]
    pub fn loaded_plugins(&self) -> Vec<String> {
        let mut names = self.loaded.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    #[must_use]
    pub fn state_of(&self, plugin_name: &str) -> Option<PluginLifecycleState> {
        self.loaded.get(plugin_name).map(|entry| entry.state)
    }

    #[must_use]
    pub fn health_of(&self, plugin_name: &str) -> Option<PluginHealth> {
        self.health.get(plugin_name).copied()
    }

    /// Returns all loaded plugin manifests (for UI page listing, etc.).
    #[must_use]
    pub fn loaded_manifests(&self) -> Vec<&PluginManifest> {
        self.loaded.values().map(|entry| &entry.manifest).collect()
    }

    /// Collect HTTP routes from all running plugins.
    #[must_use]
    pub fn collect_routes(&self) -> Vec<axum::Router<()>> {
        let mut routers = Vec::new();
        for loaded in self.loaded.values() {
            if let LoadedPluginRuntime::Subprocess(bridge) = &loaded.runtime {
                let router = bridge::build_subprocess_router(Arc::clone(bridge));
                routers.push(router);
            }
        }
        routers
    }

    /// Collect all capabilities declared by running plugins.
    /// Returns a map of capability name → list of plugin entries providing it.
    pub fn collect_capabilities(&self) -> HashMap<String, Vec<(String, String)>> {
        let mut caps: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (name, loaded) in &self.loaded {
            if loaded.state == PluginLifecycleState::Running {
                for capability in &loaded.manifest.capabilities {
                    let version = loaded.manifest.version.to_string();
                    caps.entry(capability.clone())
                        .or_default()
                        .push((name.clone(), version));
                }
            }
        }
        caps
    }

    #[must_use]
    pub fn is_unstable(&self, plugin_name: &str) -> bool {
        self.health_of(plugin_name).map(|h| h.unstable).unwrap_or(false)
    }

    pub fn pause_plugin(&mut self, plugin_name: &str) -> AmanResult<()> {
        let loaded = self.loaded.get_mut(plugin_name).ok_or_else(|| Error::NotFound {
            name: format!("plugin:{plugin_name}"),
        })?;
        if loaded.state != PluginLifecycleState::Running {
            return Err(Error::InvalidStateTransition {
                message: format!(
                    "plugin `{plugin_name}` is not running, current state is {:?}",
                    loaded.state
                ),
            });
        }
        loaded.state = PluginLifecycleState::Paused;
        Ok(())
    }

    pub fn resume_plugin(&mut self, plugin_name: &str) -> AmanResult<()> {
        let loaded = self.loaded.get_mut(plugin_name).ok_or_else(|| Error::NotFound {
            name: format!("plugin:{plugin_name}"),
        })?;
        if loaded.state != PluginLifecycleState::Paused {
            return Err(Error::InvalidStateTransition {
                message: format!(
                    "plugin `{plugin_name}` is not paused, current state is {:?}",
                    loaded.state
                ),
            });
        }
        loaded.state = PluginLifecycleState::Running;
        Ok(())
    }

    pub fn disable_plugin(&mut self, plugin_name: &str) -> AmanResult<()> {
        let loaded = self.loaded.get_mut(plugin_name).ok_or_else(|| Error::NotFound {
            name: format!("plugin:{plugin_name}"),
        })?;
        if loaded.state == PluginLifecycleState::Shutdown {
            return Err(Error::InvalidStateTransition {
                message: format!("plugin `{plugin_name}` is already shutdown"),
            });
        }
        loaded.state = PluginLifecycleState::Disabled;
        Ok(())
    }

    pub fn enable_plugin(&mut self, plugin_name: &str) -> AmanResult<()> {
        let loaded = self.loaded.get_mut(plugin_name).ok_or_else(|| Error::NotFound {
            name: format!("plugin:{plugin_name}"),
        })?;
        if loaded.state != PluginLifecycleState::Disabled {
            return Err(Error::InvalidStateTransition {
                message: format!(
                    "plugin `{plugin_name}` is not disabled, current state is {:?}",
                    loaded.state
                ),
            });
        }
        loaded.state = PluginLifecycleState::Enabled;
        loaded.state = PluginLifecycleState::Running;
        Ok(())
    }

    /// Load a single plugin dynamically (post-startup).
    ///
    /// Validates that all declared dependencies are already loaded, then
    /// delegates to the shared inner loading routine. Use this to load
    /// plugins that were deferred pending capability approval.
    ///
    /// # Errors
    /// Returns `NotFound` if a declared dependency is not yet loaded.
    pub async fn load_plugin(&mut self, candidate: PluginCandidate) -> AmanResult<()> {
        let manifest = match &candidate {
            PluginCandidate::InProcess { manifest, .. } => manifest,
            PluginCandidate::Subprocess { manifest, .. } => manifest,
            PluginCandidate::Wasm { manifest, .. } => manifest,
        };
        let plugin_name = manifest.name.clone();

        // Validate dependencies are already loaded
        for dep in &manifest.depends_on {
            if !self.loaded.contains_key(&dep.name) {
                return Err(Error::NotFound {
                    name: format!(
                        "dependency plugin:{} required by plugin:{} is not loaded",
                        dep.name, plugin_name
                    ),
                });
            }
        }

        if self.loaded.contains_key(&plugin_name) {
            return Err(Error::AlreadyExists {
                name: format!("plugin:{plugin_name}"),
            });
        }

        self.audit(&plugin_name, PluginAuditEventType::LoadStarted, "starting dynamic load");
        self.load_plugin_inner(&plugin_name, candidate).await?;
        self.audit(&plugin_name, PluginAuditEventType::LoadSucceeded, "dynamic load completed");

        tracing::info!(
            plugin = %plugin_name,
            "plugin loaded dynamically after capability approval"
        );
        Ok(())
    }

    /// Shared inner routine: validate, create runtime, register exports,
    /// and insert into the loaded map. Does NOT check dependencies or
    /// handle batch rollback — callers are responsible for those.
    async fn load_plugin_inner(
        &mut self,
        plugin_name: &str,
        candidate: PluginCandidate,
    ) -> AmanResult<()> {
        // The name/version check uses the variant's stub Box<dyn Plugin>
        // — for InProcess it's the real plugin, for Subprocess/Wasm
        // it's a discovery-path stub. The validation contract is the
        // same: stub name/version must match the manifest.
        let (manifest, stub) = match &candidate {
            PluginCandidate::InProcess { manifest, plugin } => (manifest.clone(), plugin.as_ref()),
            PluginCandidate::Subprocess { manifest, stub, .. } => (manifest.clone(), stub.as_ref()),
            PluginCandidate::Wasm { manifest, stub, .. } => (manifest.clone(), stub.as_ref()),
        };
        if stub.name() != manifest.name {
            self.audit(
                plugin_name,
                PluginAuditEventType::LoadFailed,
                "plugin implementation name mismatch",
            );
            return Err(Error::ConfigInvalid {
                message: format!(
                    "plugin implementation name `{}` does not match manifest `{}`",
                    stub.name(),
                    manifest.name
                ),
            });
        }

        if stub.version() != &manifest.version {
            self.audit(
                plugin_name,
                PluginAuditEventType::LoadFailed,
                "plugin implementation version mismatch",
            );
            return Err(Error::VersionMismatch {
                expected: manifest.version.to_string(),
                found: stub.version().to_string(),
            });
        }

        let (runtime, exports) = match candidate {
            PluginCandidate::InProcess { manifest, mut plugin } => {
                let ctx = PluginContext {
                    base: BaseContext::new(TraceId::new()),
                    plugin_name: Some(manifest.name.clone()),
                    ..PluginContext::default()
                };
                let tracker = Arc::clone(&ctx.resource_tracker);
                if let Err(error) = plugin.on_load(ctx).await {
                    let released = release_tracked_resources(&tracker);
                    self.audit(
                        plugin_name,
                        PluginAuditEventType::OnLoadInterrupted,
                        format!(
                            "on_load failed: {error}; released resources fds={}, dbs={}, paths={}",
                            released.fds.len(),
                            released.dbs.len(),
                            released.paths.len()
                        ),
                    );
                    return Err(error);
                }

                let exports = match self.register_exports(plugin.as_ref()) {
                    Ok(exports) => exports,
                    Err(error) => {
                        let _ = self.unregister_exports(&RegisteredExports::default(), plugin_name);
                        let released = release_tracked_resources(&tracker);
                        let _ = plugin.on_unload().await;
                        self.audit(
                            plugin_name,
                            PluginAuditEventType::OnLoadInterrupted,
                            format!(
                                "register exports failed after on_load: {error}; released resources fds={}, dbs={}, paths={}",
                                released.fds.len(),
                                released.dbs.len(),
                                released.paths.len()
                            ),
                        );
                        return Err(error);
                    }
                };
                (LoadedPluginRuntime::InProcess(plugin), exports)
            }
            PluginCandidate::Subprocess {
                manifest,
                mut config,
                stub: _,
            } => {
                // Auto-derive subprocess config from manifest runtime/entrypoint if needed
                if let Some(runtime) = &manifest.runtime {
                    if config.command.is_empty() {
                        config.command = runtime.clone();
                    }
                    if config.args.is_empty()
                        && let Some(entrypoint) = &manifest.entrypoint
                    {
                        config.args = vec![entrypoint.to_string_lossy().to_string()];
                    }
                }

                // Derive sandbox config from manifest security (approved capabilities).
                // When loading via the approval flow, the manifest already reflects
                // the user-approved capabilities.
                let sandbox_config = manifest.security.as_ref().map(|sec| {
                    let caps = &sec.requested_capabilities;
                    // Resolve ${var} templates to concrete paths. Project-specific
                    // vars (${project.work_dir}, ${project.root}) are None at load
                    // time — those paths are skipped and added when the project
                    // context becomes available.
                    let plugin_data_dir = self.config.aman_data_dir
                        .join("plugins")
                        .join(plugin_name);
                    let ctx = TemplateContext {
                        project_work_dir: None,
                        project_root: None,
                        aman_data_dir: self.config.aman_data_dir.clone(),
                        plugin_data_dir,
                    };
                    let (read_dirs, write_dirs) = caps.resolve_paths(&ctx);
                    SandboxConfig {
                        allowed_read_dirs: read_dirs,
                        allowed_write_dirs: write_dirs,
                        network_allowed: caps.flags.can_network,
                        process_spawn_allowed: caps.flags.can_spawn_processes,
                        max_memory_mb: caps.limits.max_memory_mb,
                    }
                });

                let bridge = bridge::SubprocessPluginBridge::spawn(
                    plugin_name,
                    &config,
                    None,
                    Arc::clone(&self.method_handler),
                    sandbox_config,
                )?;

                if let Err(error) = bridge.on_load(&manifest.version) {
                    self.audit(
                        plugin_name,
                        PluginAuditEventType::OnLoadInterrupted,
                        format!("subprocess on_load failed: {error}"),
                    );
                    bridge.shutdown();
                    return Err(error);
                }
                (LoadedPluginRuntime::Subprocess(bridge), RegisteredExports::default())
            }
            PluginCandidate::Wasm {
                manifest,
                bytes,
                stub: _,
            } => {
                if has_manifest_exports(&manifest) {
                    return Err(Error::ConfigInvalid {
                        message: "wasm plugin exports bridging is not implemented yet".to_owned(),
                    });
                }
                let wasm_security = manifest.security.as_ref().map(|s| {
                    let caps = &s.requested_capabilities;
                    WasmSecurityConfig {
                        max_memory_bytes: caps.limits.max_memory_mb * MAX_MANIFEST_SIZE as u64,
                        ..WasmSecurityConfig::default()
                    }
                });
                let runtime = WasmPluginRuntime::from_wasm_bytes(&bytes, wasm_security)?;
                runtime.on_load()?;
                (LoadedPluginRuntime::Wasm(runtime), RegisteredExports::default())
            }
        };

        let loaded = LoadedPlugin {
            manifest,
            runtime,
            state: PluginLifecycleState::Running,
            exports,
        };
        self.loaded.insert(plugin_name.to_owned(), loaded);
        self.load_order.push(plugin_name.to_owned());
        self.health.entry(plugin_name.to_owned()).or_default();
        Ok(())
    }

    /// Check whether a plugin is already loaded.
    #[must_use]
    pub fn is_loaded(&self, plugin_name: &str) -> bool {
        self.loaded.contains_key(plugin_name)
    }

    pub async fn load_all(&mut self, candidates: Vec<PluginCandidate>) -> AmanResult<Vec<String>> {
        if !self.loaded.is_empty() {
            return Err(Error::InvalidStateTransition {
                message: "plugin loader already contains loaded plugins".to_owned(),
            });
        }
        let graph = DependencyGraph::new(
            candidates
                .iter()
                .map(|candidate| match candidate {
                    PluginCandidate::InProcess { manifest, .. } => manifest,
                    PluginCandidate::Subprocess { manifest, .. } => manifest,
                    PluginCandidate::Wasm { manifest, .. } => manifest,
                })
                .cloned()
                .collect(),
        )?;
        let order = graph.topological_order()?;

        let mut by_name = HashMap::new();
        for candidate in candidates {
            let name = match &candidate {
                PluginCandidate::InProcess { manifest, .. } => &manifest.name,
                PluginCandidate::Subprocess { manifest, .. } => &manifest.name,
                PluginCandidate::Wasm { manifest, .. } => &manifest.name,
            }
            .clone();
            if by_name.insert(name.clone(), candidate).is_some() {
                return Err(Error::AlreadyExists {
                    name: format!("plugin:{name}"),
                });
            }
        }

        let mut loaded_now = Vec::new();
        for plugin_name in &order {
            let candidate = by_name.remove(plugin_name).ok_or_else(|| Error::NotFound {
                name: format!("plugin:{plugin_name}"),
            })?;
            self.audit(plugin_name, PluginAuditEventType::LoadStarted, "starting load");

            if let Err(error) = self.load_plugin_inner(plugin_name, candidate).await {
                self.rollback_loaded(&loaded_now).await?;
                return Err(error);
            }

            self.audit(plugin_name, PluginAuditEventType::LoadSucceeded, "load completed");
            loaded_now.push(plugin_name.clone());
        }
        Ok(order)
    }

    pub async fn unload_all(&mut self) -> AmanResult<()> {
        let names = self
            .load_order
            .iter()
            .rev()
            .cloned()
            .collect::<Vec<String>>();
        let mut first_error = None;
        for name in names {
            if let Err(error) = self.unload_plugin(&name).await
                && first_error.is_none() {
                    first_error = Some(error);
                }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    pub async fn unload_plugin(&mut self, plugin_name: &str) -> AmanResult<()> {
        let dependents = self.dependents_of(plugin_name);
        for dependent_name in dependents {
            if let Some(dependent) = self.loaded.get_mut(&dependent_name) {
                match &mut dependent.runtime {
                    LoadedPluginRuntime::InProcess(plugin) => {
                        let _ = plugin.on_dependency_unloading(plugin_name).await;
                    }
                    LoadedPluginRuntime::Subprocess(_) => {
                        // Subprocess bridge doesn't support dependency unloading notifications
                    }
                    LoadedPluginRuntime::Wasm(_) => {}
                }
                self.audit(
                    &dependent_name,
                    PluginAuditEventType::DependencyUnloadingNotified,
                    format!("dependency {plugin_name} is unloading"),
                );
            }
        }

        let mut loaded = self.loaded.remove(plugin_name).ok_or_else(|| Error::NotFound {
            name: format!("plugin:{plugin_name}"),
        })?;
        self.load_order.retain(|name| name != plugin_name);
        self.audit(
            plugin_name,
            PluginAuditEventType::UnloadStarted,
            "starting unload",
        );
        self.unregister_exports(&loaded.exports, plugin_name)?;
        match &mut loaded.runtime {
            LoadedPluginRuntime::InProcess(plugin) => {
                if let Err(error) = unload_with_timeout(plugin, self.config.unload_timeout).await {
                    if matches!(error, Error::Timeout) {
                        self.bump_unload_timeout(plugin_name);
                        self.audit(
                            plugin_name,
                            PluginAuditEventType::UnloadTimeout,
                            format!("unload timed out after {:?}", self.config.unload_timeout),
                        );
                    }
                    return Err(error);
                }
            }
            LoadedPluginRuntime::Subprocess(bridge) => {
                bridge.on_unload()?;
                bridge.shutdown();
            }
            LoadedPluginRuntime::Wasm(runtime) => {
                runtime.on_unload()?;
            }
        }
        self.reset_unload_timeout(plugin_name);
        loaded.state = PluginLifecycleState::Shutdown;
        self.audit(
            plugin_name,
            PluginAuditEventType::UnloadSucceeded,
            "unload completed",
        );
        Ok(())
    }

    async fn rollback_loaded(&mut self, loaded_now: &[String]) -> AmanResult<()> {
        let mut first_error = None;
        self.audit(
            "plugin-loader",
            PluginAuditEventType::RollbackStarted,
            format!("rolling back {} plugins", loaded_now.len()),
        );
        for plugin_name in loaded_now.iter().rev() {
            if let Err(error) = self.unload_plugin(plugin_name).await
                && first_error.is_none() {
                    first_error = Some(error);
                }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    fn register_exports(&self, plugin: &dyn Plugin) -> AmanResult<RegisteredExports> {
        let mut exports = RegisteredExports::default();
        register_exports_block!(self, plugin, exports,
            (skills, skills, register_skill, name),
            (tools, tools, register_tool, name),
            (event_sources, event_sources, register_event_source, id),
            (hooks, hooks, register_hook, name),
            (memory_providers, memory_providers, register_memory_provider, name)
        );
        Ok(exports)
    }

    fn unregister_exports(&self, exports: &RegisteredExports, plugin_name: &str) -> AmanResult<()> {
        unregister_exports_block!(self, exports,
            (memory_providers, unregister_memory_provider),
            (hooks, unregister_hook),
            (event_sources, unregister_event_source),
            (tools, unregister_tool),
            (skills, unregister_skill)
        );
        self.audit(
            plugin_name,
            PluginAuditEventType::RollbackReleased,
            format!(
                "released exports skills={}, tools={}, event_sources={}, hooks={}, memory_providers={}",
                exports.skills.len(),
                exports.tools.len(),
                exports.event_sources.len(),
                exports.hooks.len(),
                exports.memory_providers.len()
            ),
        );
        Ok(())
    }

    fn dependents_of(&self, plugin_name: &str) -> Vec<String> {
        self.loaded
            .iter()
            .filter_map(|(name, loaded)| {
                let depends = loaded
                    .manifest
                    .depends_on
                    .iter()
                    .any(|dep| dep.name == plugin_name);
                if depends {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn bump_unload_timeout(&mut self, plugin_name: &str) {
        let health = self.health.entry(plugin_name.to_owned()).or_default();
        health.consecutive_unload_timeouts = health.consecutive_unload_timeouts.saturating_add(1);
        if health.consecutive_unload_timeouts >= self.config.unstable_after_timeouts {
            health.unstable = true;
        }
    }

    fn reset_unload_timeout(&mut self, plugin_name: &str) {
        if let Some(health) = self.health.get_mut(plugin_name) {
            health.consecutive_unload_timeouts = 0;
        }
    }

    fn audit(
        &self,
        plugin_name: &str,
        event_type: PluginAuditEventType,
        message: impl Into<String>,
    ) {
        self.audit_logger.record(PluginAuditEvent {
            timestamp_ms: now_millis(),
            plugin_name: plugin_name.to_owned(),
            event_type,
            message: message.into(),
        });
    }
}

async fn unload_with_timeout(plugin: &mut Box<dyn Plugin>, timeout: Duration) -> AmanResult<()> {
    let unload = plugin.on_unload();
    let deadline = Delay::new(timeout);
    pin_mut!(unload);
    pin_mut!(deadline);
    match select(unload, deadline).await {
        Either::Left((result, _)) => result,
        Either::Right((_elapsed, _)) => Err(Error::Timeout),
    }
}

fn now_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn release_tracked_resources(
    tracker: &Arc<Mutex<kernel::context::PluginResourceTracker>>,
) -> PluginTrackedResources {
    let mut guard = tracker.lock().expect("plugin resource tracker lock");
    std::mem::take(&mut guard.resources)
}

#[must_use]
fn has_manifest_exports(manifest: &PluginManifest) -> bool {
    !(manifest.exports.skills.is_empty()
        && manifest.exports.tools.is_empty()
        && manifest.exports.event_sources.is_empty()
        && manifest.exports.hooks.is_empty())
}

#[must_use]
fn parse_plugin_manifest_paths(root: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, found: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let next = entry.path();
            if next.is_dir() {
                walk(&next, found);
                continue;
            }
            if next.file_name().and_then(|name| name.to_str()) == Some("plugin.yaml") {
                found.insert(next);
            }
        }
    }

    let mut found = BTreeSet::new();
    walk(root, &mut found);
    found.into_iter().collect()
}

fn move_or_copy_dir(from: &Path, to: &Path) -> AmanResult<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_dir_recursive(from, to)?;
            fs::remove_dir_all(from)?;
            Ok(())
        }
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) -> AmanResult<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = to.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

#[must_use]
pub fn parse_plugin_manifests(root: &Path) -> Vec<String> {
    parse_plugin_manifest_paths(root)
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

#[must_use]
pub fn validate_manifest_exports(manifest: &PluginManifest) -> bool {
    let mut all = HashSet::new();
    let unique_skills = manifest.exports.skills.iter().all(|name| all.insert(format!("s:{name}")));
    let unique_tools = manifest.exports.tools.iter().all(|name| all.insert(format!("t:{name}")));
    let unique_sources = manifest
        .exports
        .event_sources
        .iter()
        .all(|name| all.insert(format!("e:{name}")));
    let unique_hooks = manifest
        .exports
        .hooks
        .iter()
        .all(|name| all.insert(format!("h:{name}")));
    unique_skills && unique_tools && unique_sources && unique_hooks
}

// ---------------------------------------------------------------------------
// SubprocessStubPlugin — minimal Plugin impl for subprocess-only plugins
// ---------------------------------------------------------------------------

/// A stub Plugin implementation for subprocess plugins that have no Rust code.
/// All logic lives in the subprocess and communicates via the JSON-RPC bridge.
pub(crate) struct SubprocessStubPlugin {
    name: String,
    version: Version,
}

#[async_trait::async_trait]
impl Plugin for SubprocessStubPlugin {
    fn name(&self) -> &str { &self.name }
    fn version(&self) -> &Version { &self.version }
    fn dependencies(&self) -> &[PluginDependency] { &[] }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> { Ok(()) }
    async fn on_unload(&mut self) -> AmanResult<()> { Ok(()) }
    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> { Ok(()) }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> { vec![] }
    fn skills(&self) -> Vec<Arc<dyn Skill>> { vec![] }
    fn tools(&self) -> Vec<Arc<dyn Tool>> { vec![] }
    fn hooks(&self) -> Vec<Arc<dyn Hook>> { vec![] }
    fn memory_providers(&self) -> Vec<Arc<dyn MemoryProvider>> { vec![] }
}

// ---------------------------------------------------------------------------
// Plugin discovery from filesystem
// ---------------------------------------------------------------------------

/// Scan a directory for plugin subdirectories containing `plugin.yaml` manifests,
/// and return PluginCandidates for any subprocess plugins found.
pub fn discover_subprocess_plugins(plugins_dir: &Path) -> Vec<PluginCandidate> {
    let mut candidates = Vec::new();

    let entries = match fs::read_dir(plugins_dir) {
        Ok(e) => e,
        Err(_) => return candidates,
    };

    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }

        let manifest_path = plugin_dir.join("plugin.yaml");
        if !manifest_path.is_file() {
            continue;
        }

        let manifest = match PluginManifest::from_file(&manifest_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    dir = %plugin_dir.display(),
                    error = %e,
                    "failed to parse plugin manifest, skipping"
                );
                continue;
            }
        };

        // Only discover subprocess plugins from the filesystem
        let isolation = manifest.isolation.unwrap_or(PluginIsolationMode::InProcess);
        if isolation != PluginIsolationMode::Subprocess {
            continue;
        }

        let subprocess_config = manifest.subprocess.clone().or_else(|| {
            manifest.runtime.as_ref().map(|runtime: &String| {
                let args = manifest
                    .entrypoint
                    .as_ref()
                    .map(|ep: &PathBuf| vec![ep.to_string_lossy().to_string()])
                    .unwrap_or_default();
                SubprocessPluginConfig {
                    command: runtime.clone(),
                    args,
                    cwd: Some(plugin_dir.clone()),
                    timeout_ms: PLUGIN_TIMEOUT_MS,
                }
            })
        });

        let candidate = PluginCandidate::Subprocess {
            manifest: manifest.clone(),
            config: subprocess_config.expect("subprocess config resolved above"),
            stub: Box::new(SubprocessStubPlugin {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
            }),
        };

        tracing::info!(
            name = %manifest.name,
            dir = %plugin_dir.display(),
            "discovered subprocess plugin"
        );
        candidates.push(candidate);
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_version_req, parse_plugin_manifests, plugin_management_router,
        validate_manifest_exports, DependencyGraph, InMemoryPluginAuditLogger, MemoryProvider,
        NoopPluginRegistrar, PluginAuditEventType, PluginCandidate, PluginExportRegistrar,
        PluginInstaller, PluginIsolationMode, PluginLifecycleState, PluginLoader, PluginLoaderConfig,
        PluginManifest, SubprocessPluginClient, SubprocessPluginConfig, WasmPluginRuntime,
        PLUGIN_TIMEOUT_MS,
    };
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use futures_timer::Delay;
    use kernel::context::{PluginContext, SkillContext, SourceContext, ToolContext};
    use kernel::event::{Event, EventType};
    use kernel::hook::Hook;
    use kernel::plugin::{Plugin, PluginDependency};
    use kernel::schema::JsonSchema;
    use kernel::skill::{Skill, TriggerCondition};
    use kernel::source::EventSource;
    use kernel::tool::Tool;
    use kernel::types::{HealthStatus, SourceType, ToolMode, TraceId};
    use kernel::{AmanResult, Error};
    use semver::Version;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tower::ServiceExt;

    struct DummySkill {
        name: String,
    }

    #[async_trait::async_trait]
    impl Skill for DummySkill {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &Version {
            static VERSION: std::sync::LazyLock<Version> =
                std::sync::LazyLock::new(|| Version::new(0, 1, 0));
            &VERSION
        }
        fn description(&self) -> &str {
            "dummy skill"
        }
        fn triggers(&self) -> &[TriggerCondition] {
            &[]
        }
        async fn execute(&self, _event: Event, _ctx: SkillContext) -> AmanResult<()> {
            Ok(())
        }
    }

    struct DummyTool {
        name: String,
    }

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }
        fn parameters(&self) -> &JsonSchema {
            static SCHEMA: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &SCHEMA
        }
        fn returns(&self) -> &JsonSchema {
            static SCHEMA: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &SCHEMA
        }
        async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            Ok(json!({"ok": true}))
        }
    }

    struct DummySource {
        id: String,
    }

    #[async_trait::async_trait]
    impl EventSource for DummySource {
        fn id(&self) -> &str {
            &self.id
        }
        fn source_type(&self) -> SourceType {
            SourceType::Custom
        }
        async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
            Ok(())
        }
        async fn shutdown(&mut self) -> AmanResult<()> {
            Ok(())
        }
        fn health(&self) -> HealthStatus {
            HealthStatus::Ok
        }
    }

    struct TestPlugin {
        name: String,
        version: Version,
        deps: Vec<PluginDependency>,
        skills: Vec<Arc<dyn Skill>>,
        tools: Vec<Arc<dyn Tool>>,
        sources: Vec<Arc<dyn EventSource>>,
        load_calls: Arc<Mutex<usize>>,
        unload_calls: Arc<Mutex<usize>>,
        unload_delay_ms: u64,
        unload_log: Arc<Mutex<Vec<String>>>,
        dependency_notifications: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &Version {
            &self.version
        }
        fn dependencies(&self) -> &[PluginDependency] {
            &self.deps
        }

        async fn on_load(&mut self, ctx: PluginContext) -> AmanResult<()> {
            ctx.track_fd(3);
            ctx.track_db(format!("db://{}", self.name));
            ctx.track_path(format!("/tmp/{}/socket", self.name));
            let mut calls = self.load_calls.lock().expect("load_calls lock");
            *calls += 1;
            Ok(())
        }

        async fn on_unload(&mut self) -> AmanResult<()> {
            if self.unload_delay_ms > 0 {
                Delay::new(Duration::from_millis(self.unload_delay_ms)).await;
            }
            let mut calls = self.unload_calls.lock().expect("unload_calls lock");
            *calls += 1;
            self.unload_log
                .lock()
                .expect("unload_log lock")
                .push(self.name.clone());
            Ok(())
        }

        async fn on_dependency_unloading(&self, dep_name: &str) -> AmanResult<()> {
            self.dependency_notifications
                .lock()
                .expect("dependency_notifications lock")
                .push(dep_name.to_owned());
            Ok(())
        }

        fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
            self.sources.clone()
        }
        fn skills(&self) -> Vec<Arc<dyn Skill>> {
            self.skills.clone()
        }
        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            self.tools.clone()
        }
    }

    #[derive(Default)]
    struct RecordingRegistrar {
        skills: Mutex<BTreeSet<String>>,
        tools: Mutex<BTreeSet<String>>,
        sources: Mutex<BTreeSet<String>>,
        hooks: Mutex<BTreeSet<String>>,
        memory_providers: Mutex<BTreeSet<String>>,
        fail_skill: Mutex<Option<String>>,
    }

    impl RecordingRegistrar {
        fn registered_skills(&self) -> Vec<String> {
            self.skills
                .lock()
                .expect("skills lock")
                .iter()
                .cloned()
                .collect()
        }
    }

    impl PluginExportRegistrar for RecordingRegistrar {
        fn register_skill(&self, skill: Arc<dyn Skill>) -> AmanResult<()> {
            if let Some(fail_name) = self.fail_skill.lock().expect("fail_skill lock").clone()
                && skill.name() == fail_name
            {
                return Err(Error::Unrecoverable {
                    message: "forced register failure".to_owned(),
                });
            }
            self.skills
                .lock()
                .expect("skills lock")
                .insert(skill.name().to_owned());
            Ok(())
        }

        fn unregister_skill(&self, skill_name: &str) -> AmanResult<()> {
            self.skills.lock().expect("skills lock").remove(skill_name);
            Ok(())
        }

        fn register_tool(&self, tool: Arc<dyn Tool>) -> AmanResult<()> {
            self.tools
                .lock()
                .expect("tools lock")
                .insert(tool.name().to_owned());
            Ok(())
        }

        fn unregister_tool(&self, tool_name: &str) -> AmanResult<()> {
            self.tools.lock().expect("tools lock").remove(tool_name);
            Ok(())
        }

        fn register_event_source(&self, source: Arc<dyn EventSource>) -> AmanResult<()> {
            self.sources
                .lock()
                .expect("sources lock")
                .insert(source.id().to_owned());
            Ok(())
        }

        fn unregister_event_source(&self, source_id: &str) -> AmanResult<()> {
            self.sources.lock().expect("sources lock").remove(source_id);
            Ok(())
        }

        fn register_hook(&self, hook: Arc<dyn Hook>) -> AmanResult<()> {
            self.hooks
                .lock()
                .expect("hooks lock")
                .insert(hook.name().to_owned());
            Ok(())
        }

        fn unregister_hook(&self, hook_name: &str) -> AmanResult<()> {
            self.hooks.lock().expect("hooks lock").remove(hook_name);
            Ok(())
        }

        fn register_memory_provider(&self, provider: Arc<dyn MemoryProvider>) -> AmanResult<()> {
            self.memory_providers
                .lock()
                .expect("memory_providers lock")
                .insert(provider.name().to_owned());
            Ok(())
        }

        fn unregister_memory_provider(&self, provider_name: &str) -> AmanResult<()> {
            self.memory_providers
                .lock()
                .expect("memory_providers lock")
                .remove(provider_name);
            Ok(())
        }
    }

    fn plugin_candidate(
        name: &str,
        version: Version,
        deps: Vec<PluginDependency>,
        skill_name: Option<&str>,
    ) -> PluginCandidate {
        plugin_candidate_with_options(
            name,
            version,
            deps,
            skill_name,
            0,
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        )
    }

    fn plugin_candidate_with_options(
        name: &str,
        version: Version,
        deps: Vec<PluginDependency>,
        skill_name: Option<&str>,
        unload_delay_ms: u64,
        unload_log: Arc<Mutex<Vec<String>>>,
        dependency_notifications: Arc<Mutex<Vec<String>>>,
    ) -> PluginCandidate {
        let skills = skill_name
            .into_iter()
            .map(|name| Arc::new(DummySkill { name: name.into() }) as Arc<dyn Skill>)
            .collect::<Vec<_>>();
        let tools = vec![Arc::new(DummyTool {
            name: format!("{name}-tool"),
        }) as Arc<dyn Tool>];
        let sources = vec![Arc::new(DummySource {
            id: format!("{name}-source"),
        }) as Arc<dyn EventSource>];
        PluginCandidate::InProcess {
            manifest: PluginManifest {
                name: name.to_owned(),
                version: version.clone(),
                depends_on: deps.clone(),
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports {
                    skills: skills.iter().map(|item| item.name().to_owned()).collect(),
                    tools: tools.iter().map(|item| item.name().to_owned()).collect(),
                    event_sources: sources.iter().map(|item| item.id().to_owned()).collect(),
                    ..Default::default()
                },
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
                capabilities: vec![],
                ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
            plugin: Box::new(TestPlugin {
                name: name.to_owned(),
                version,
                deps,
                skills,
                tools,
                sources,
                load_calls: Arc::new(Mutex::new(0)),
                unload_calls: Arc::new(Mutex::new(0)),
                unload_delay_ms,
                unload_log,
                dependency_notifications,
            }),
        }
    }

    fn isolated_plugin_candidate(
        name: &str,
        version: Version,
        isolation: PluginIsolationMode,
        subprocess: Option<SubprocessPluginConfig>,
        wasm_module_bytes: Option<Vec<u8>>,
    ) -> PluginCandidate {
        let manifest = PluginManifest {
            name: name.to_owned(),
            version: version.clone(),
            depends_on: vec![],
            lifecycle: super::PluginLifecycleConfig::default(),
            exports: super::PluginExports::default(),
            config_schema: None,
            isolation: None,
            subprocess: None,
            wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
            security: None,
        };
        let stub = Box::new(TestPlugin {
            name: name.to_owned(),
            version,
            deps: vec![],
            skills: vec![],
            tools: vec![],
            sources: vec![],
            load_calls: Arc::new(Mutex::new(0)),
            unload_calls: Arc::new(Mutex::new(0)),
            unload_delay_ms: 0,
            unload_log: Arc::new(Mutex::new(Vec::new())),
            dependency_notifications: Arc::new(Mutex::new(Vec::new())),
        });
        match isolation {
            PluginIsolationMode::InProcess => PluginCandidate::InProcess {
                manifest,
                plugin: stub,
            },
            PluginIsolationMode::Subprocess => PluginCandidate::Subprocess {
                manifest,
                config: subprocess
                    .expect("subprocess config required for Subprocess variant"),
                stub,
            },
            PluginIsolationMode::Wasm => PluginCandidate::Wasm {
                manifest,
                bytes: wasm_module_bytes
                    .expect("wasm bytes required for Wasm variant"),
                stub,
            },
        }
    }

    #[test]
    fn manifest_parsing_supports_required_fields() {
        let yaml = r#"
name: invoice-plugin
version: 1.2.0
depends_on:
  - name: core-plugin
    version_range: ">=1.0 <2.0"
lifecycle:
  auto_start: true
exports:
  skills: ["invoice-skill"]
  tools: ["invoice-tool"]
  event_sources: ["invoice-source"]
config_schema:
  type: object
  properties:
    enabled:
      type: boolean
"#;
        let manifest = PluginManifest::parse(yaml).expect("manifest parses");
        assert_eq!(manifest.name, "invoice-plugin");
        assert_eq!(manifest.version, Version::new(1, 2, 0));
        assert_eq!(manifest.depends_on.len(), 1);
        assert!(validate_manifest_exports(&manifest));
    }

    #[test]
    fn dependency_graph_supports_topological_order_and_cycle_detection() {
        let graph = DependencyGraph::new(vec![
            PluginManifest {
                name: "c".to_owned(),
                version: Version::new(1, 0, 0),
                depends_on: vec![],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
                capabilities: vec![],
                ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
            PluginManifest {
                name: "b".to_owned(),
                version: Version::new(1, 0, 0),
                depends_on: vec![PluginDependency {
                    name: "c".to_owned(),
                    version_range: ">=1.0 <2.0".to_owned(),
                }],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
            PluginManifest {
                name: "a".to_owned(),
                version: Version::new(1, 0, 0),
                depends_on: vec![PluginDependency {
                    name: "b".to_owned(),
                    version_range: ">=1.0 <2.0".to_owned(),
                }],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
        ])
        .expect("graph creates");
        let order = graph.topological_order().expect("topological order resolves");
        assert_eq!(order, vec!["c".to_owned(), "b".to_owned(), "a".to_owned()]);

        let cycle_graph = DependencyGraph::new(vec![
            PluginManifest {
                name: "a".to_owned(),
                version: Version::new(1, 0, 0),
                depends_on: vec![PluginDependency {
                    name: "b".to_owned(),
                    version_range: "*".to_owned(),
                }],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
            PluginManifest {
                name: "b".to_owned(),
                version: Version::new(1, 0, 0),
                depends_on: vec![PluginDependency {
                    name: "a".to_owned(),
                    version_range: "*".to_owned(),
                }],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
        ])
        .expect("graph creates");
        let error = cycle_graph
            .topological_order()
            .expect_err("cycle should be rejected");
        assert!(matches!(error, Error::CycleDetected { .. }));
    }

    #[test]
    fn dependency_graph_rejects_missing_dependency_and_version_mismatch() {
        let missing = DependencyGraph::new(vec![PluginManifest {
            name: "a".to_owned(),
            version: Version::new(1, 0, 0),
            depends_on: vec![PluginDependency {
                name: "b".to_owned(),
                version_range: ">=1.0".to_owned(),
            }],
            lifecycle: super::PluginLifecycleConfig::default(),
            exports: super::PluginExports::default(),
            config_schema: None,
            isolation: None,
            subprocess: None,
            wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
        }])
        .expect("graph creates");
        let missing_error = missing
            .topological_order()
            .expect_err("missing dependency should fail");
        assert!(matches!(missing_error, Error::NotFound { .. }));

        let mismatch = DependencyGraph::new(vec![
            PluginManifest {
                name: "b".to_owned(),
                version: Version::new(2, 0, 0),
                depends_on: vec![],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
                capabilities: vec![],
                ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
            PluginManifest {
                name: "a".to_owned(),
                version: Version::new(1, 0, 0),
                depends_on: vec![PluginDependency {
                    name: "b".to_owned(),
                    version_range: ">=1.0 <2.0".to_owned(),
                }],
                lifecycle: super::PluginLifecycleConfig::default(),
                exports: super::PluginExports::default(),
                config_schema: None,
                isolation: None,
                subprocess: None,
                wasm_path: None,
            capabilities: vec![],
            ui: None, runtime: None, min_version: None, entrypoint: None,
                security: None,
            },
        ])
        .expect("graph creates");
        let mismatch_error = mismatch
            .topological_order()
            .expect_err("version mismatch should fail");
        assert!(matches!(mismatch_error, Error::VersionMismatch { .. }));
    }

    #[test]
    fn plugin_loader_loads_and_unloads_in_dependency_order() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::new(registrar.clone());
            let order = loader
                .load_all(vec![
                    plugin_candidate("a", Version::new(1, 0, 0), vec![], Some("a-skill")),
                    plugin_candidate(
                        "b",
                        Version::new(1, 0, 0),
                        vec![PluginDependency {
                            name: "a".to_owned(),
                            version_range: ">=1.0 <2.0".to_owned(),
                        }],
                        Some("b-skill"),
                    ),
                ])
                .await
                .expect("load plugins");

            assert_eq!(order, vec!["a".to_owned(), "b".to_owned()]);
            assert_eq!(
                loader.state_of("a"),
                Some(super::PluginLifecycleState::Running)
            );
            assert_eq!(
                loader.state_of("b"),
                Some(super::PluginLifecycleState::Running)
            );
            assert_eq!(
                registrar.registered_skills(),
                vec!["a-skill".to_owned(), "b-skill".to_owned()]
            );

            loader.unload_all().await.expect("unload all");
            assert!(loader.loaded_plugins().is_empty());
            assert!(registrar.registered_skills().is_empty());
        });
    }

    #[test]
    fn plugin_loader_rolls_back_when_export_registration_fails() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            *registrar.fail_skill.lock().expect("fail_skill lock") = Some("b-skill".to_owned());
            let mut loader = PluginLoader::new(registrar.clone());
            let error = loader
                .load_all(vec![
                    plugin_candidate("a", Version::new(1, 0, 0), vec![], Some("a-skill")),
                    plugin_candidate(
                        "b",
                        Version::new(1, 0, 0),
                        vec![PluginDependency {
                            name: "a".to_owned(),
                            version_range: ">=1.0".to_owned(),
                        }],
                        Some("b-skill"),
                    ),
                ])
                .await
                .expect_err("registration failure should fail loading");

            assert!(matches!(error, Error::Unrecoverable { .. }));
            assert!(loader.loaded_plugins().is_empty());
            assert!(registrar.registered_skills().is_empty());
        });
    }

    #[test]
    fn parse_plugin_manifest_files_from_directory_tree() {
        let root = std::env::temp_dir().join(format!("aman-plugin-discovery-{}", Version::new(1, 0, 0)));
        let nested = root.join("nested/plugin-a");
        std::fs::create_dir_all(&nested).expect("create nested path");
        let direct = root.join("plugin-b");
        std::fs::create_dir_all(&direct).expect("create direct path");
        std::fs::write(
            nested.join("plugin.yaml"),
            "name: plugin-a\nversion: 0.1.0\n",
        )
        .expect("write nested plugin yaml");
        std::fs::write(
            direct.join("plugin.yaml"),
            "name: plugin-b\nversion: 0.1.0\n",
        )
        .expect("write direct plugin yaml");
        std::fs::write(root.join("README.md"), "ignore").expect("write ignore file");

        let found = parse_plugin_manifests(&root);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|path| path.ends_with("plugin.yaml")));
    }

    #[test]
    fn helpers_normalize_range_and_noop_registrar() {
        assert_eq!(normalize_version_req(">=1.0 <2.0"), ">=1.0,<2.0");
        assert_eq!(normalize_version_req(">=1.0, <2.0"), ">=1.0, <2.0");
        let registrar = NoopPluginRegistrar;
        let skill = Arc::new(DummySkill {
            name: "noop".to_owned(),
        }) as Arc<dyn Skill>;
        registrar
            .register_skill(skill.clone())
            .expect("register with noop");
        registrar
            .unregister_skill(skill.name())
            .expect("unregister with noop");
    }

    #[test]
    fn lifecycle_supports_pause_resume_and_disable_enable() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::new(registrar);
            loader
                .load_all(vec![plugin_candidate(
                    "stateful",
                    Version::new(1, 0, 0),
                    vec![],
                    Some("stateful-skill"),
                )])
                .await
                .expect("load plugin");

            assert_eq!(loader.state_of("stateful"), Some(PluginLifecycleState::Running));
            loader.pause_plugin("stateful").expect("pause");
            assert_eq!(loader.state_of("stateful"), Some(PluginLifecycleState::Paused));
            loader.resume_plugin("stateful").expect("resume");
            assert_eq!(loader.state_of("stateful"), Some(PluginLifecycleState::Running));
            loader.disable_plugin("stateful").expect("disable");
            assert_eq!(loader.state_of("stateful"), Some(PluginLifecycleState::Disabled));
            loader.enable_plugin("stateful").expect("enable");
            assert_eq!(loader.state_of("stateful"), Some(PluginLifecycleState::Running));
        });
    }

    #[test]
    fn unload_notifies_dependents_and_uses_reverse_topology() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::new(registrar);
            let unload_log = Arc::new(Mutex::new(Vec::new()));
            let dep_notes = Arc::new(Mutex::new(Vec::new()));

            loader
                .load_all(vec![
                    plugin_candidate_with_options(
                        "a",
                        Version::new(1, 0, 0),
                        vec![],
                        Some("a-skill"),
                        0,
                        unload_log.clone(),
                        Arc::new(Mutex::new(Vec::new())),
                    ),
                    plugin_candidate_with_options(
                        "b",
                        Version::new(1, 0, 0),
                        vec![PluginDependency {
                            name: "a".to_owned(),
                            version_range: ">=1.0 <2.0".to_owned(),
                        }],
                        Some("b-skill"),
                        0,
                        unload_log.clone(),
                        dep_notes.clone(),
                    ),
                ])
                .await
                .expect("load plugins");

            loader.unload_plugin("a").await.expect("unload dependency");
            assert_eq!(dep_notes.lock().expect("dep notes lock").as_slice(), &["a"]);

            loader.unload_all().await.expect("unload rest");
            assert_eq!(
                unload_log.lock().expect("unload log lock").as_slice(),
                &["a", "b"]
            );
        });
    }

    #[test]
    fn marks_plugin_unstable_after_three_unload_timeouts() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::with_config(
                registrar,
                PluginLoaderConfig {
                    unload_timeout: Duration::from_millis(10),
                    unstable_after_timeouts: 3,
                    ..Default::default()
                },
            );
            let unload_log = Arc::new(Mutex::new(Vec::new()));

            for _ in 0..3 {
                loader
                    .load_all(vec![plugin_candidate_with_options(
                        "slow",
                        Version::new(1, 0, 0),
                        vec![],
                        Some("slow-skill"),
                        50,
                        unload_log.clone(),
                        Arc::new(Mutex::new(Vec::new())),
                    )])
                    .await
                    .expect("load slow plugin");

                let error = loader
                    .unload_plugin("slow")
                    .await
                    .expect_err("timeout expected");
                assert!(matches!(error, Error::Timeout));
            }

            let health = loader.health_of("slow").expect("health exists");
            assert_eq!(health.consecutive_unload_timeouts, 3);
            assert!(health.unstable);
            assert!(loader.is_unstable("slow"));
            assert!(unload_log.lock().expect("unload log lock").is_empty());
        });
    }

    #[test]
    fn audit_logs_on_load_interrupted_and_rollback_release() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            *registrar.fail_skill.lock().expect("fail_skill lock") = Some("b-skill".to_owned());
            let audit = Arc::new(InMemoryPluginAuditLogger::default());
            let mut loader = PluginLoader::new(registrar).with_audit_logger(audit.clone());

            let error = loader
                .load_all(vec![
                    plugin_candidate("a", Version::new(1, 0, 0), vec![], Some("a-skill")),
                    plugin_candidate(
                        "b",
                        Version::new(1, 0, 0),
                        vec![PluginDependency {
                            name: "a".to_owned(),
                            version_range: ">=1.0".to_owned(),
                        }],
                        Some("b-skill"),
                    ),
                ])
                .await
                .expect_err("should fail");
            assert!(matches!(error, Error::Unrecoverable { .. }));

            let events = audit.events();
            assert!(events
                .iter()
                .any(|e| e.event_type == PluginAuditEventType::OnLoadInterrupted
                    && e.plugin_name == "b"
                    && e.message.contains("released resources fds=1, dbs=1, paths=1")));
            assert!(events
                .iter()
                .any(|e| e.event_type == PluginAuditEventType::RollbackReleased
                    && e.plugin_name == "a"));
        });
    }

    #[test]
    fn audit_logs_warning_on_unload_timeout() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let audit = Arc::new(InMemoryPluginAuditLogger::default());
            let mut loader = PluginLoader::with_config(
                registrar,
                PluginLoaderConfig {
                    unload_timeout: Duration::from_millis(5),
                    unstable_after_timeouts: 1,
                    ..Default::default()
                },
            )
            .with_audit_logger(audit.clone());

            loader
                .load_all(vec![plugin_candidate_with_options(
                    "slow-audit",
                    Version::new(1, 0, 0),
                    vec![],
                    Some("slow-skill"),
                    50,
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(Mutex::new(Vec::new())),
                )])
                .await
                .expect("load slow plugin");

            let error = loader
                .unload_plugin("slow-audit")
                .await
                .expect_err("timeout expected");
            assert!(matches!(error, Error::Timeout));

            let events = audit.events();
            assert!(events
                .iter()
                .any(|e| e.event_type == PluginAuditEventType::UnloadTimeout
                    && e.plugin_name == "slow-audit"));
        });
    }

    #[test]
    fn subprocess_client_invokes_json_rpc_successfully() {
        let script = r#"
import json, sys
while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line)
    res = {
      "jsonrpc": "2.0",
      "id": req.get("id", 1),
      "result": {
        "method": req.get("method"),
        "plugin_name": req.get("params", {}).get("plugin_name")
      }
    }
    print(json.dumps(res))
    sys.stdout.flush()
"#;
        let client = SubprocessPluginClient::new(SubprocessPluginConfig {
            command: "python3".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            cwd: None,
            timeout_ms: PLUGIN_TIMEOUT_MS,
        });
        let result = client
            .on_load("subproc", &Version::new(1, 0, 0))
            .expect("rpc success");
        assert_eq!(result["method"], serde_json::json!("aman_plugin_on_load"));
        assert_eq!(result["plugin_name"], serde_json::json!("subproc"));
    }

    #[test]
    fn subprocess_client_surfaces_rpc_error() {
        let script = r#"
import json, sys
while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line)
    res = {
      "jsonrpc": "2.0",
      "id": req.get("id", 1),
      "error": {
        "code": -32000,
        "message": "boom"
      }
    }
    print(json.dumps(res))
    sys.stdout.flush()
"#;
        let client = SubprocessPluginClient::new(SubprocessPluginConfig {
            command: "python3".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            cwd: None,
            timeout_ms: PLUGIN_TIMEOUT_MS,
        });
        let error = client
            .on_unload("subproc")
            .expect_err("rpc should fail");
        assert!(matches!(error, Error::Unrecoverable { .. }));
        assert!(error.to_string().contains("subprocess rpc error"));
    }

    #[test]
    fn wasm_runtime_loads_module_and_executes_skill() {
        let wasm_bytes = wat::parse_str(
            r#"(module
                (func (export "aman_skill_on_load") (result i32)
                  i32.const 0)
                (func (export "aman_skill_on_unload") (result i32)
                  i32.const 0)
                (func (export "aman_skill_execute") (result i32)
                  i32.const 7)
            )"#,
        )
        .expect("parse wat");
        let runtime = WasmPluginRuntime::from_wasm_bytes(&wasm_bytes, None).expect("build wasm runtime");
        runtime.on_load().expect("on_load");
        let result = runtime.execute_skill().expect("execute");
        assert_eq!(result, 7);
        runtime.on_unload().expect("on_unload");
    }

    #[test]
    fn wasm_runtime_requires_expected_exports() {
        let wasm_bytes = wat::parse_str(
            r#"(module
                (func (export "aman_skill_on_load") (result i32)
                  i32.const 0)
            )"#,
        )
        .expect("parse wat");
        let runtime = WasmPluginRuntime::from_wasm_bytes(&wasm_bytes, None).expect("build wasm runtime");
        let error = runtime
            .execute_skill()
            .expect_err("missing execute export should fail");
        assert!(matches!(error, Error::ConfigInvalid { .. }));
        assert!(error.to_string().contains("aman_skill_execute"));
    }

    #[test]
    fn loader_supports_subprocess_isolation_lifecycle() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::new(registrar);
            let script = r#"
import json, sys
while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line)
    res = {"jsonrpc":"2.0","id": req.get("id", 1), "result":{"ok":True}}
    print(json.dumps(res))
    sys.stdout.flush()
"#;
            let candidate = isolated_plugin_candidate(
                "subproc-loader",
                Version::new(1, 0, 0),
                PluginIsolationMode::Subprocess,
                Some(SubprocessPluginConfig {
                    command: "python3".to_owned(),
                    args: vec!["-c".to_owned(), script.to_owned()],
                    cwd: None,
                    timeout_ms: PLUGIN_TIMEOUT_MS,
                }),
                None,
            );
            let order = loader.load_all(vec![candidate]).await.expect("load subprocess");
            assert_eq!(order, vec!["subproc-loader".to_owned()]);
            assert_eq!(
                loader.state_of("subproc-loader"),
                Some(PluginLifecycleState::Running)
            );
            loader
                .unload_plugin("subproc-loader")
                .await
                .expect("unload subprocess");
            assert!(loader.loaded_plugins().is_empty());
        });
    }

    #[test]
    fn loader_supports_wasm_isolation_lifecycle() {
        pollster::block_on(async {
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::new(registrar);
            let wasm_bytes = wat::parse_str(
                r#"(module
                    (func (export "aman_skill_on_load") (result i32)
                      i32.const 0)
                    (func (export "aman_skill_on_unload") (result i32)
                      i32.const 0)
                    (func (export "aman_skill_execute") (result i32)
                      i32.const 11)
                )"#,
            )
            .expect("parse wat");

            let candidate = isolated_plugin_candidate(
                "wasm-loader",
                Version::new(1, 0, 0),
                PluginIsolationMode::Wasm,
                None,
                Some(wasm_bytes),
            );
            let order = loader.load_all(vec![candidate]).await.expect("load wasm");
            assert_eq!(order, vec!["wasm-loader".to_owned()]);
            assert_eq!(
                loader.state_of("wasm-loader"),
                Some(PluginLifecycleState::Running)
            );
            loader
                .unload_plugin("wasm-loader")
                .await
                .expect("unload wasm");
            assert!(loader.loaded_plugins().is_empty());
        });
    }

    #[test]
    fn plugin_installer_installs_tar_gz_archive() {
        let temp_root = std::env::temp_dir().join(format!("aman-plugin-install-{}", TraceId::new()));
        std::fs::create_dir_all(&temp_root).expect("create temp root");
        let archive_path = temp_root.join("demo-plugin.tar.gz");
        create_plugin_archive(
            &archive_path,
            "demo-plugin",
            "name: demo-plugin\nversion: 0.1.0\n",
            Some("README.md"),
            Some("hello"),
        );

        let install_root = temp_root.join("plugins");
        let installer = PluginInstaller::new(install_root.clone());
        let installed = installer
            .install_from_archive(&archive_path)
            .expect("install archive");
        assert_eq!(installed.manifest.name, "demo-plugin");
        assert!(installed.install_dir.join("plugin.yaml").exists());
        assert!(installed.install_dir.join("README.md").exists());

        let duplicate = installer
            .install_from_archive(&archive_path)
            .expect_err("duplicate install should fail");
        assert!(matches!(duplicate, Error::AlreadyExists { .. }));
    }

    #[test]
    fn plugin_installer_uninstall_calls_unload_and_removes_files() {
        pollster::block_on(async {
            let temp_root =
                std::env::temp_dir().join(format!("aman-plugin-uninstall-{}", TraceId::new()));
            let install_root = temp_root.join("plugins");
            std::fs::create_dir_all(install_root.join("demo")).expect("create plugin dir");
            std::fs::write(install_root.join("demo").join("plugin.yaml"), "name: demo\nversion: 0.1.0\n")
                .expect("write plugin file");

            let unload_log = Arc::new(Mutex::new(Vec::new()));
            let registrar = Arc::new(RecordingRegistrar::default());
            let mut loader = PluginLoader::new(registrar);
            loader
                .load_all(vec![plugin_candidate_with_options(
                    "demo",
                    Version::new(0, 1, 0),
                    vec![],
                    Some("demo-skill"),
                    0,
                    unload_log.clone(),
                    Arc::new(Mutex::new(Vec::new())),
                )])
                .await
                .expect("load plugin");

            let installer = PluginInstaller::new(install_root.clone());
            installer
                .uninstall(Some(&mut loader), "demo")
                .await
                .expect("uninstall plugin");

            assert!(loader.state_of("demo").is_none());
            assert_eq!(unload_log.lock().expect("unload log").as_slice(), &["demo"]);
            assert!(!install_root.join("demo").exists());
        });
    }

    #[tokio::test]
    async fn plugin_install_endpoint_accepts_multipart_archive() {
        let temp_root = std::env::temp_dir().join(format!("aman-plugin-http-install-{}", TraceId::new()));
        std::fs::create_dir_all(&temp_root).expect("create temp root");
        let archive_path = temp_root.join("http-plugin.tar.gz");
        create_plugin_archive(
            &archive_path,
            "http-plugin",
            "name: http-plugin\nversion: 0.3.0\n",
            None,
            None,
        );
        let archive_bytes = std::fs::read(&archive_path).expect("read archive bytes");

        let install_root = temp_root.join("plugins");
        let router = plugin_management_router(Arc::new(PluginInstaller::new(install_root.clone())));
        let boundary = format!("----aman-{}", TraceId::new());
        let body = build_multipart_archive_body(&boundary, "plugin", "http-plugin.tar.gz", &archive_bytes);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/plugin/install")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("build request"),
            )
            .await
            .expect("send request");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: Value = serde_json::from_slice(&payload).expect("parse json");
        assert_eq!(json["plugin_name"], serde_json::json!("http-plugin"));
        assert_eq!(json["version"], serde_json::json!("0.3.0"));
        assert!(install_root.join("http-plugin").join("plugin.yaml").exists());
    }

    #[tokio::test]
    async fn plugin_install_endpoint_rejects_missing_plugin_field() {
        let temp_root = std::env::temp_dir().join(format!("aman-plugin-http-bad-{}", TraceId::new()));
        let install_root = temp_root.join("plugins");
        let router = plugin_management_router(Arc::new(PluginInstaller::new(install_root)));
        let boundary = format!("----aman-{}", TraceId::new());
        let body = build_multipart_archive_body(&boundary, "not-plugin", "x.txt", b"noop");
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/plugin/install")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("build request"),
            )
            .await
            .expect("send request");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: Value = serde_json::from_slice(&payload).expect("parse json");
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("multipart must contain `plugin`")
        );
    }

    fn build_multipart_archive_body(
        boundary: &str,
        field_name: &str,
        file_name: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{file_name}\"\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: application/gzip\r\n\r\n");
        body.extend_from_slice(payload);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    fn create_plugin_archive(
        archive_path: &PathBuf,
        plugin_dir_name: &str,
        plugin_yaml: &str,
        extra_file_name: Option<&str>,
        extra_file_content: Option<&str>,
    ) {
        let tar_gz_file = std::fs::File::create(archive_path).expect("create tar.gz file");
        let encoder = flate2::write::GzEncoder::new(tar_gz_file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        let plugin_yaml_bytes = plugin_yaml.as_bytes();
        header.set_size(plugin_yaml_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{plugin_dir_name}/plugin.yaml"),
                plugin_yaml_bytes,
            )
            .expect("append plugin yaml");

        if let (Some(file_name), Some(file_content)) = (extra_file_name, extra_file_content) {
            let mut extra_header = tar::Header::new_gnu();
            let content = file_content.as_bytes();
            extra_header.set_size(content.len() as u64);
            extra_header.set_mode(0o644);
            extra_header.set_cksum();
            builder
                .append_data(&mut extra_header, format!("{plugin_dir_name}/{file_name}"), content)
                .expect("append extra file");
        }

        builder.finish().expect("finish tar builder");
        let mut encoder = builder.into_inner().expect("take encoder");
        encoder.flush().expect("flush gzip");
        encoder.finish().expect("finish gzip");
    }

    #[allow(dead_code)]
    fn _pathbuf(_: PathBuf) {}

    #[allow(dead_code)]
    fn _sample_event() -> Event {
        Event::new("test", EventType::Custom("plugin".to_owned()), json!({}))
    }
}
