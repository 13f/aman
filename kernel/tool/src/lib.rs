#![forbid(unsafe_code)]
#![doc = "Tool registry, runner, and builtin tools for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


pub mod auth;
pub mod code_agent;
pub mod fs_tools;
pub mod planner;
pub mod security;
pub mod web_fetch;
pub mod web_search;

pub use auth::AuthRegistry;
pub use auth::PluginApprovalRegistry;
pub use planner::PlannerTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use kernel::hook::EventPublisher;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::{ExecutionModel, ToolMode};
use kernel::{AmanResult, Error};
use rusqlite::types::{Value as SqlValue, ValueRef as SqlValueRef};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, RwLock};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.list_tools())
            .finish()
    }
}

impl ToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, tool: Arc<dyn Tool>) -> AmanResult<()> {
        let name = tool.name().to_owned();
        let mut tools = self.tools.write().expect("tool registry write lock");
        if tools.contains_key(&name) {
            return Err(Error::AlreadyExists {
                name: format!("tool:{name}"),
            });
        }
        tools.insert(name, tool);
        Ok(())
    }

    pub fn unregister(&self, name: &str) -> AmanResult<()> {
        let mut tools = self.tools.write().expect("tool registry write lock");
        if tools.remove(name).is_none() {
            return Err(Error::NotFound {
                name: format!("tool:{name}"),
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .expect("tool registry read lock")
            .get(name)
            .cloned()
    }

    /// Return the names of all registered tools.
    #[must_use]
    pub fn list_tools(&self) -> Vec<String> {
        self.tools
            .read()
            .expect("tool registry read lock")
            .keys()
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolSecurityConfig {
    pub allowed_paths: Vec<PathBuf>,
    pub network_allowed: bool,
    pub command_allowlist: Vec<String>,
    /// When false, all security checks pass (backward-compatible default).
    /// Set to true to enforce path/network/command allowlisting.
    pub allowlist_enabled: bool,
}

impl ToolSecurityConfig {
    /// A permissive config that allows everything (backward-compatible default).
    pub fn permissive() -> Self {
        Self {
            allowed_paths: vec![],
            network_allowed: true,
            command_allowlist: vec![],
            allowlist_enabled: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub allowed_paths: Vec<PathBuf>,
    pub network_allowed: bool,
    pub max_memory_bytes: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allowed_paths: Vec::new(),
            network_allowed: false,
            max_memory_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolResourceConfig {
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
}

impl Default for ToolResourceConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: 5_000,
            max_timeout_ms: 60_000,
        }
    }
}

pub struct ToolRunner {
    registry: Arc<ToolRegistry>,
    security: ToolSecurityConfig,
    resources: ToolResourceConfig,
}

impl ToolRunner {
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            security: ToolSecurityConfig::default(),
            resources: ToolResourceConfig::default(),
        }
    }

    #[must_use]
    pub fn with_security(mut self, security: ToolSecurityConfig) -> Self {
        self.security = security;
        self
    }

    #[must_use]
    pub fn with_resources(mut self, resources: ToolResourceConfig) -> Self {
        self.resources = resources;
        self
    }

    pub async fn execute(
        &self,
        tool_name: &str,
        params: Value,
        mut ctx: ToolContext,
    ) -> AmanResult<ToolExecutionResult> {
        let tool = self.registry.get(tool_name).ok_or_else(|| Error::NotFound {
            name: format!("tool:{tool_name}"),
        })?;

        self.validate_params(tool.parameters().as_value(), &params)?;

        if let Err(e) = self.security_check(&params) {
            tracing::warn!(tool_name, error = %e, "tool security check denied");
            return Err(e);
        }
        if let Err(e) = self.hardline_check(tool_name, &params) {
            tracing::warn!(tool_name, error = %e, "tool hardline check denied");
            return Err(e);
        }

        // Reset consecutive read tracking when a non-read tool runs.
        if tool_name != "read" {
            crate::fs_tools::reset_read_tracker();
        }

        let timeout_ms = self.allocate_resources(&mut ctx)?;
        let temp_dir = runner_temp_dir(&ctx)?;
        let _cleanup = TempDirCleanup {
            path: temp_dir.clone(),
        };

        let started = Instant::now();
        let raw_output = tool.execute(params, ctx).await?;
        if started.elapsed().as_millis() > u128::from(timeout_ms) {
            return Err(Error::Timeout);
        }
        let output = normalize_output(raw_output);

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(ToolExecutionResult {
            tool_name: tool_name.to_owned(),
            mode: tool.mode(),
            duration_ms,
            output,
        })
    }

    fn validate_params(&self, schema: &Value, params: &Value) -> AmanResult<()> {
        validate_against_schema(schema, params)
    }

    fn security_check(&self, params: &Value) -> AmanResult<()> {
        check_tool_security(&self.security, params)
    }

    fn hardline_check(&self, tool_name: &str, params: &Value) -> AmanResult<()> {
        if let Some(reason) = security::check_hardline_block(tool_name, params) {
            return Err(Error::PermissionDenied {
                message: reason.to_owned(),
            });
        }
        Ok(())
    }

    fn allocate_resources(&self, ctx: &mut ToolContext) -> AmanResult<u64> {
        let timeout_ms = ctx
            .base
            .timeout_ms
            .unwrap_or(self.resources.default_timeout_ms)
            .min(self.resources.max_timeout_ms);
        let temp_dir = std::env::temp_dir().join(format!("aman-tool-{}", Uuid::now_v7()));
        fs::create_dir_all(&temp_dir)?;
        let temp_dir_text = temp_dir.display().to_string();
        ctx.base.extensions.insert(
            "runner_temp_dir".to_owned(),
            Value::String(temp_dir_text.clone()),
        );
        if ctx.working_directory.is_none() {
            ctx.working_directory = Some(temp_dir_text);
        }
        Ok(timeout_ms)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub tool_name: String,
    pub mode: ToolMode,
    pub duration_ms: u64,
    pub output: Value,
}

struct TempDirCleanup {
    path: PathBuf,
}

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn runner_temp_dir(ctx: &ToolContext) -> AmanResult<PathBuf> {
    let Some(path) = ctx
        .base
        .extensions
        .get("runner_temp_dir")
        .and_then(Value::as_str)
    else {
        return Err(Error::Unrecoverable {
            message: "runner_temp_dir is missing".to_owned(),
        });
    };
    Ok(PathBuf::from(path))
}

fn normalize_output(output: Value) -> Value {
    match output {
        Value::Object(_) => output,
        value => json!({ "result": value }),
    }
}

fn validate_against_schema(schema: &Value, params: &Value) -> AmanResult<()> {
    let expected_type = schema.get("type").and_then(Value::as_str);
    if let Some(expected_type) = expected_type {
        let type_matches = match expected_type {
            "object" => params.is_object(),
            "array" => params.is_array(),
            "string" => params.is_string(),
            "boolean" => params.is_boolean(),
            "integer" => params.as_i64().is_some(),
            "number" => params.as_f64().is_some(),
            _ => true,
        };
        if !type_matches {
            return Err(Error::ConfigInvalid {
                message: format!("params type mismatch, expected {expected_type}"),
            });
        }
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if params.get(field).is_none() {
                return Err(Error::ConfigInvalid {
                    message: format!("missing required param: {field}"),
                });
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (field, field_schema) in properties {
            if let Some(field_value) = params.get(field) {
                validate_against_schema(field_schema, field_value)?;
            }
        }
    }

    Ok(())
}

pub fn check_allowed_path(allowed_paths: &[PathBuf], candidate: Option<&Value>) -> AmanResult<()> {
    if allowed_paths.is_empty() {
        return Ok(());
    }
    let Some(candidate) = candidate.and_then(Value::as_str) else {
        return Ok(());
    };
    let candidate_path = PathBuf::from(candidate);
    let is_allowed = allowed_paths
        .iter()
        .any(|base| path_within(&candidate_path, base));
    if is_allowed {
        Ok(())
    } else {
        Err(Error::PermissionDenied {
            message: format!("path not allowed: {candidate}"),
        })
    }
}

pub fn path_within(candidate: &Path, base: &Path) -> bool {
    let Ok(base) = base.canonicalize() else {
        return false;
    };

    if let Ok(candidate) = candidate.canonicalize() {
        return candidate.starts_with(&base);
    }

    if let Some(parent) = candidate.parent()
        && let Ok(parent) = parent.canonicalize() {
            return parent.starts_with(&base);
        }

    false
}

/// Check tool call arguments against a [`ToolSecurityConfig`].
///
/// Returns `Ok(())` if the call is allowed, `Err(PermissionDenied)` otherwise.
/// Checks path allowlist for file-related params, network access for HTTP,
/// and command allowlist for exec.
pub fn check_tool_security(config: &ToolSecurityConfig, params: &Value) -> AmanResult<()> {
    // Backward-compatible: when allowlisting is disabled, all checks pass.
    // Hardline blocks (rm -rf /, fork bombs, etc.) always run regardless.
    if !config.allowlist_enabled {
        return Ok(());
    }

    check_allowed_path(&config.allowed_paths, params.get("path"))?;
    check_allowed_path(&config.allowed_paths, params.get("file_path"))?;
    check_allowed_path(&config.allowed_paths, params.get("from"))?;
    check_allowed_path(&config.allowed_paths, params.get("to"))?;
    check_allowed_path(&config.allowed_paths, params.get("cwd"))?;
    check_allowed_path(&config.allowed_paths, params.get("base"))?;
    check_allowed_path(&config.allowed_paths, params.get("db_path"))?;

    if let Some(url) = params.get("url").and_then(Value::as_str)
        && !config.network_allowed {
            return Err(Error::PermissionDenied {
                message: format!("network access is disabled: {url}"),
            });
        }

    if let Some(command) = params.get("command").and_then(Value::as_str) {
        let executable = command.split_whitespace().next().unwrap_or_default();
        if config.command_allowlist.is_empty()
            || !config.command_allowlist.iter().any(|allowed| allowed == executable)
        {
            return Err(Error::PermissionDenied {
                message: format!("command not allowed: {executable}"),
            });
        }
    }

    Ok(())
}

pub fn install_builtin_tools(registry: &ToolRegistry) -> AmanResult<()> {
    registry.register(Arc::new(fs_tools::ReadTool))?;
    registry.register(Arc::new(fs_tools::WriteTool))?;
    registry.register(Arc::new(fs_tools::EditTool))?;
    registry.register(Arc::new(fs_tools::ListTool))?;
    registry.register(Arc::new(fs_tools::FindTool))?;
    registry.register(Arc::new(fs_tools::GrepTool))?;
    registry.register(Arc::new(HttpTool))?;
    registry.register(Arc::new(ExecTool))?;
    registry.register(Arc::new(DbTool))?;
    registry.register(Arc::new(WebSearchTool))?;
    registry.register(Arc::new(WebFetchTool))?;
    registry.register(Arc::new(PlannerTool))
}

/// Register all code agent CLI tools that are available on PATH.
///
/// Scans `predefined/agents/code-agents.json` and registers a tool for each
/// CLI coding agent whose command is found on the system.
pub fn install_code_agent_tools(registry: &ToolRegistry) {
    for config in code_agent::available_code_agent_configs() {
        let tool = Arc::new(code_agent::CodeAgentTool::new(&config));
        // Best-effort: skip duplicates (user may have overridden)
        let _ = registry.register(tool);
    }
}

struct HttpTool;

#[async_trait::async_trait]
impl Tool for HttpTool {
    fn name(&self) -> &str {
        "http"
    }

    fn description(&self) -> &str {
        "Make an HTTP request to a URL. For Aman gateway API endpoints, use \
         the gateway base URL from the system prompt (http://127.0.0.1:9999 by default). \
         Common endpoints include /api/v1/team/projects for the Team kanban."
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {"type": "string"},
                    "method": {"type": "string"},
                    "headers": {"type": "object"},
                    "body": {}
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "ok": {"type": "boolean"},
                    "status": {"type": "integer"},
                    "headers": {"type": "object"},
                    "body": {"type": "string"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, ctx: ToolContext) -> AmanResult<Value> {
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "url must be a string".to_owned(),
            })?;
        let method = params
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_uppercase();
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|error| {
            Error::ConfigInvalid {
                message: format!("invalid http method: {error}"),
            }
        })?;

        let timeout_ms = ctx.base.timeout_ms.unwrap_or(5_000);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .no_proxy()
            .build()
            .map_err(|error| Error::Unrecoverable {
                message: format!("failed to build http client: {error}"),
            })?;

        let mut request = client.request(method, url);
        if let Some(headers) = params.get("headers").and_then(Value::as_object) {
            for (name, value) in headers {
                if let Some(text) = value.as_str() {
                    request = request.header(name, text);
                }
            }
        }
        if let Some(body) = params.get("body") {
            let body_text = match body {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            request = request.body(body_text);
        }

        let response = request.send().await.map_err(|error| Error::Unrecoverable {
            message: format!("http request failed: {error}"),
        })?;
        let status = response.status().as_u16();
        let mut response_headers = serde_json::Map::new();
        for (name, value) in response.headers() {
            response_headers.insert(
                name.to_string(),
                Value::String(value.to_str().unwrap_or_default().to_owned()),
            );
        }
        let body = response.text().await.map_err(|error| Error::Unrecoverable {
            message: format!("failed to read http response body: {error}"),
        })?;

        let body_json = serde_json::from_str::<Value>(&body).ok();
        Ok(json!({
            "ok": true,
            "status": status,
            "headers": response_headers,
            "body": body,
            "json": body_json
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxExecutionResult {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct SubprocessSandbox {
    config: SandboxConfig,
}

impl SubprocessSandbox {
    #[must_use]
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    pub fn execute_command(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        timeout_ms: u64,
    ) -> AmanResult<SandboxExecutionResult> {
        let mut process = Command::new(command);
        process
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
            if !self.config.allowed_paths.is_empty()
                && !self.config.allowed_paths.iter().any(|base| path_within(cwd, base))
            {
                return Err(Error::PermissionDenied {
                    message: format!("cwd not allowed by sandbox: {}", cwd.display()),
                });
            }
        }
        let mut child = process.spawn().map_err(|error| Error::Unrecoverable {
            message: format!("failed to spawn command: {error}"),
        })?;

        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().map_err(|error| Error::Unrecoverable {
                message: format!("failed to poll command status: {error}"),
            })? {
                let output = child.wait_with_output().map_err(|error| Error::Unrecoverable {
                    message: format!("failed to read command output: {error}"),
                })?;
                return Ok(SandboxExecutionResult {
                    status: status.code(),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    timed_out: false,
                });
            }
            if started.elapsed().as_millis() > u128::from(timeout_ms) {
                let _ = child.kill();
                let output = child.wait_with_output().map_err(|error| Error::Unrecoverable {
                    message: format!("failed to collect timed out command output: {error}"),
                })?;
                return Ok(SandboxExecutionResult {
                    status: None,
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    timed_out: true,
                });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub trait ContainerToolAdapter: Send + Sync {
    fn execute_in_container(
        &self,
        image: &str,
        command: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> AmanResult<SandboxExecutionResult>;
}

pub trait WasmToolAdapter: Send + Sync {
    fn execute_in_wasm(
        &self,
        module: &Path,
        function: &str,
        params: Value,
        timeout_ms: u64,
    ) -> AmanResult<Value>;
}

/// When LLMs mistakenly pass the entire command line as `command` (e.g.
/// `"/usr/bin/env python3 -c '...'"`), split it into the real executable
/// and arguments. Also strips the `/usr/bin/env` prefix when present.
fn normalize_exec_command(raw_command: &str, args: &[String]) -> (String, Vec<String>) {
    // Only auto-split when the LLM clearly passed a compound command and no
    // explicit args were provided — never override an explicit args list.
    if !args.is_empty() || !raw_command.contains(char::is_whitespace) {
        return (raw_command.to_owned(), args.to_vec());
    }

    let words = shell_split(raw_command);
    let mut words_iter = words.into_iter();

    let first = match words_iter.next() {
        Some(w) => w,
        None => return (raw_command.to_owned(), args.to_vec()),
    };

    // Strip `/usr/bin/env` (or bare `env`) — the LLM often prepends it out
    // of habit, but `execvp` doesn't need an explicit env resolver.
    let cmd = if first == "/usr/bin/env" || first == "env" {
        match words_iter.next() {
            Some(real) => real,
            None => return (first, Vec::new()),
        }
    } else {
        first
    };

    let extra_args: Vec<String> = words_iter.collect();
    (cmd, extra_args)
}

/// Minimal shell-like word splitter. Handles single quotes, double quotes,
/// and backslash escapes within double quotes. This covers the most common
/// patterns LLMs produce when they pass a full command line as `command`.
fn shell_split(input: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\\' if in_double => {
                // Backslash only escapes special chars inside double quotes.
                if let Some(&next) = chars.peek()
                    && matches!(next, '"' | '\\' | '$' | '`')
                {
                    current.push(chars.next().unwrap());
                    continue;
                }
                current.push(ch);
            }
            ch if ch.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

struct ExecTool;

/// Background monitor for a detached child process.
///
/// Reads stdout/stderr line-by-line, publishes `tool:progress` events, then
/// publishes a final `tool:completed` or `tool:failed` event when the process
/// exits. The calling thread blocks until the child exits — call this from a
/// dedicated `std::thread` so the tool response can return immediately.
fn monitor_detached_process(
    mut child: std::process::Child,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    bus: Arc<dyn EventPublisher>,
    pid: u32,
    tool_name: String,
    runtime: tokio::runtime::Handle,
) {
    // ── Reader threads ───────────────────────────────────────────────
    let (line_tx, line_rx) = mpsc::channel::<DetachedLine>();

    // stdout reader
    if let Some(out) = stdout {
        let tx = line_tx.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(out);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        if tx.send(DetachedLine::Stdout(text)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // stderr reader
    if let Some(err) = stderr {
        let tx = line_tx.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(err);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        if tx.send(DetachedLine::Stderr(text)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // Drop the original sender so the channel closes when all readers finish.
    drop(line_tx);

    // ── Accumulate output, publish progress, wait for exit ──────────
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    loop {
        // Check if child exited
        if let Ok(Some(status)) = child.try_wait() {
            // Drain remaining lines
            for line in line_rx.try_iter() {
                match line {
                    DetachedLine::Stdout(t) => {
                        stdout_buf.push_str(&t);
                        stdout_buf.push('\n');
                    }
                    DetachedLine::Stderr(t) => {
                        stderr_buf.push_str(&t);
                        stderr_buf.push('\n');
                    }
                }
            }

            let exit_code = status.code().unwrap_or(-1);
            let success = status.success();

            if let Err(e) = runtime.block_on(bus.publish(Event::new(
                "tool:detached",
                EventType::Custom("tool:completed".to_owned()),
                json!({
                    "tool_name": tool_name,
                    "pid": pid,
                    "success": success,
                    "exit_code": exit_code,
                    "stdout": stdout_buf,
                    "stderr": stderr_buf,
                }),
            ))) {
                tracing::warn!(error = %e, "event bus publish failed; event dropped");
            }
            return;
        }

        // Publish progress lines
        for line in line_rx.try_iter() {
            let (stream, text) = match line {
                DetachedLine::Stdout(t) => {
                    stdout_buf.push_str(&t);
                    stdout_buf.push('\n');
                    ("stdout", t)
                }
                DetachedLine::Stderr(t) => {
                    stderr_buf.push_str(&t);
                    stderr_buf.push('\n');
                    ("stderr", t)
                }
            };

            if let Err(e) = runtime.block_on(bus.publish(Event::new(
                "tool:detached",
                EventType::Custom("tool:progress".to_owned()),
                json!({
                    "tool_name": tool_name,
                    "pid": pid,
                    "stream": stream,
                    "line": text,
                }),
            ))) {
                tracing::warn!(error = %e, "event bus publish failed; event dropped");
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

enum DetachedLine {
    Stdout(String),
    Stderr(String),
}

#[async_trait::async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        concat!(
            "Execute a command in a sandboxed subprocess. ",
            "Use `detach: true` for long-running scripts (minutes+) — returns a PID immediately ",
            "and publishes progress/completion events instead of blocking. ",
            "Always split `command` (executable name only) and `args` (arguments array).",
        )
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Sandbox
    }

    fn execution_model(&self) -> ExecutionModel {
        ExecutionModel::SideEffect
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The executable name or path (e.g. 'python3', '/usr/bin/git'). Do NOT include arguments — use the `args` array instead."
                    },
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Arguments to pass to the command. Each arg is a separate string element."
                    },
                    "cwd": {"type": "string", "description": "Working directory for the command."},
                    "detach": {
                        "type": "boolean",
                        "description": "When true, spawn the process and return immediately with a PID. Progress and completion are published as events on the agent's event bus. When false (default), block until the command finishes or times out."
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "ok": {"type": "boolean"},
                    "status": {"type": "integer"},
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"},
                    "timed_out": {"type": "boolean"},
                    "pid": {"type": "integer", "description": "Process ID (only when detach=true)"},
                    "detached": {"type": "boolean", "description": "True when the process was spawned in background"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, ctx: ToolContext) -> AmanResult<Value> {
        let raw_command = params
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!(
                    "exec requires a \"command\" field (non-empty string). \
                     Received params: {} — pass e.g. {{\"command\": \"ls\", \"args\": [\"-la\"]}}",
                    params
                ),
            })?;
        let explicit_args = params
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let detach = params
            .get("detach")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // Auto-normalize compound command strings (LLMs often pass the full
        // command line as `command` instead of splitting command / args).
        let (command, args) = normalize_exec_command(raw_command, &explicit_args);

        let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        let timeout_ms = ctx.base.timeout_ms.unwrap_or(5_000);

        // ── Detach path: spawn and return ────────────────────────────
        if detach {
            let mut process = Command::new(&command);
            process
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(ref cwd) = cwd {
                process.current_dir(cwd);
            }

            let mut child = process.spawn().map_err(|error| Error::Unrecoverable {
                message: format!("failed to spawn command: {error}"),
            })?;
            let pid = child.id();

            // If an event bus is available, spawn a background monitor so the
            // agent receives progress and completion events. Otherwise the
            // child runs fully detached (fire-and-forget).
            if let Some(bus) = ctx.base.event_bus.clone() {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let tool_name = self.name().to_owned();

                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    std::thread::spawn(move || {
                        monitor_detached_process(
                            child, stdout, stderr, bus, pid, tool_name, handle,
                        );
                    });
                } else {
                    // No tokio runtime on this thread — process runs detached
                    // without progress reporting (still returns PID).
                    drop(child);
                }
            } else {
                // Fully detached: no event bus, no monitoring.
                drop(child);
            }

            return Ok(json!({
                "ok": true,
                "pid": pid,
                "detached": true
            }));
        }

        // ── Synchronous path ─────────────────────────────────────────
        let sandbox = SubprocessSandbox::new(SandboxConfig {
            allowed_paths: Vec::new(),
            network_allowed: false,
            max_memory_bytes: 256 * 1024 * 1024,
        });
        let outcome = sandbox.execute_command(&command, &args, cwd.as_deref(), timeout_ms)?;
        if outcome.timed_out {
            return Err(Error::Timeout);
        }

        Ok(json!({
            "ok": outcome.status == Some(0),
            "status": outcome.status,
            "stdout": outcome.stdout,
            "stderr": outcome.stderr,
            "timed_out": outcome.timed_out
        }))
    }
}

struct DbTool;

#[async_trait::async_trait]
impl Tool for DbTool {
    fn name(&self) -> &str {
        "db"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn execution_model(&self) -> ExecutionModel {
        ExecutionModel::Stateful
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["db_path", "sql"],
                "properties": {
                    "db_path": {"type": "string"},
                    "sql": {"type": "string"},
                    "params": {"type": "array"},
                    "operation": {"type": "string"}
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "ok": {"type": "boolean"},
                    "rows_affected": {"type": "integer"},
                    "rows": {"type": "array"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let db_path = params
            .get("db_path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "db_path must be a string".to_owned(),
            })?;
        let sql = params
            .get("sql")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "sql must be a string".to_owned(),
            })?;
        let operation = params
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("query");
        let sql_params = params
            .get("params")
            .and_then(Value::as_array)
            .map(|values| values.iter().map(json_to_sql_value).collect::<Vec<_>>())
            .unwrap_or_default();

        let connection = rusqlite::Connection::open(db_path).map_err(|error| Error::Unrecoverable {
            message: format!("failed to open database: {error}"),
        })?;

        let mut statement = connection.prepare(sql).map_err(|error| Error::ConfigInvalid {
            message: format!("invalid sql statement: {error}"),
        })?;
        let params_iter = rusqlite::params_from_iter(sql_params.iter());

        match operation {
            "execute" => {
                let rows_affected = statement.execute(params_iter).map_err(|error| {
                    Error::ConfigInvalid {
                        message: format!("sql execute failed: {error}"),
                    }
                })?;
                Ok(json!({
                    "ok": true,
                    "rows_affected": rows_affected
                }))
            }
            "query" => {
                let column_names = statement
                    .column_names()
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                let mut rows = statement.query(params_iter).map_err(|error| Error::ConfigInvalid {
                    message: format!("sql query failed: {error}"),
                })?;
                let mut output = Vec::new();
                while let Some(row) = rows.next().map_err(|error| Error::ConfigInvalid {
                    message: format!("sql row read failed: {error}"),
                })? {
                    let mut object = serde_json::Map::new();
                    for (index, name) in column_names.iter().enumerate() {
                        let value = row.get_ref(index).map_err(|error| Error::ConfigInvalid {
                            message: format!("sql value decode failed: {error}"),
                        })?;
                        object.insert(name.clone(), sql_value_ref_to_json(value));
                    }
                    output.push(Value::Object(object));
                }
                Ok(json!({
                    "ok": true,
                    "rows": output
                }))
            }
            _ => Err(Error::ConfigInvalid {
                message: format!("unsupported db operation: {operation}"),
            }),
        }
    }
}

fn json_to_sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(flag) => SqlValue::Integer(i64::from(*flag)),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                SqlValue::Integer(value)
            } else if let Some(value) = number.as_f64() {
                SqlValue::Real(value)
            } else {
                SqlValue::Null
            }
        }
        Value::String(text) => SqlValue::Text(text.clone()),
        Value::Array(_) | Value::Object(_) => SqlValue::Text(value.to_string()),
    }
}

fn sql_value_ref_to_json(value: SqlValueRef<'_>) -> Value {
    match value {
        SqlValueRef::Null => Value::Null,
        SqlValueRef::Integer(value) => json!(value),
        SqlValueRef::Real(value) => json!(value),
        SqlValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).to_string()),
        SqlValueRef::Blob(value) => Value::String(format!("blob:{}bytes", value.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        install_builtin_tools, ToolExecutionResult, ToolRegistry, ToolResourceConfig, ToolRunner,
        ToolSecurityConfig,
    };
    use kernel::context::{BaseContext, ToolContext};
    use kernel::schema::JsonSchema;
    use kernel::tool::Tool;
    use kernel::types::{ToolMode, TraceId};
    use kernel::{AmanResult, Error};
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    struct SchemaTool;

    #[async_trait::async_trait]
    impl Tool for SchemaTool {
        fn name(&self) -> &str {
            "schema-tool"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> = std::sync::LazyLock::new(|| {
                JsonSchema::from(json!({
                    "type": "object",
                    "required": ["value"],
                    "properties": {
                        "value": {"type": "string"}
                    }
                }))
            });
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &RETURNS
        }

        async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            Ok(json!({ "echo": params }))
        }
    }

    struct SlowTool;

    #[async_trait::async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &str {
            "slow-tool"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &RETURNS
        }

        async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            std::thread::sleep(std::time::Duration::from_millis(10));
            Ok(json!({"ok": true}))
        }
    }

    fn base_context() -> ToolContext {
        ToolContext {
            base: BaseContext::new(TraceId::new()),
            tool_name: None,
            working_directory: None,
        }
    }

    #[test]
    fn runner_rejects_invalid_params() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            registry
                .register(Arc::new(SchemaTool))
                .expect("tool register succeeds");
            let runner = ToolRunner::new(registry);

            let error = runner
                .execute("schema-tool", json!({}), base_context())
                .await
                .expect_err("should reject missing required param");

            assert!(matches!(error, Error::ConfigInvalid { .. }));
        });
    }

    #[test]
    fn runner_enforces_allowed_paths() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin");

            let sandbox = std::env::temp_dir().join(format!("aman-sandbox-{}", TraceId::new()));
            std::fs::create_dir_all(&sandbox).expect("create sandbox");

            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: vec![sandbox],
                network_allowed: false,
                command_allowlist: Vec::new(),
                allowlist_enabled: true,
            });

            let forbidden_path = std::env::temp_dir()
                .join("aman-forbidden.txt")
                .display()
                .to_string();
            let error = runner
                .execute(
                    "write",
                    json!({
                        "path": forbidden_path,
                        "content": "hello"
                    }),
                    base_context(),
                )
                .await
                .expect_err("forbidden path should be blocked");
            assert!(matches!(error, Error::PermissionDenied { .. }));
        });
    }

    #[test]
    fn runner_reports_timeout() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            registry
                .register(Arc::new(SlowTool))
                .expect("register slow tool");
            let runner = ToolRunner::new(registry).with_resources(ToolResourceConfig {
                default_timeout_ms: 1,
                max_timeout_ms: 1,
            });

            let mut ctx = base_context();
            ctx.base.timeout_ms = Some(1);
            let error = runner
                .execute("slow-tool", json!({}), ctx)
                .await
                .expect_err("slow execution should timeout");
            assert!(matches!(error, Error::Timeout));
        });
    }

    #[test]
    fn builtin_read_write_tool_roundtrip() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin");

            let sandbox = std::env::temp_dir().join(format!("aman-rw-{}", TraceId::new()));
            std::fs::create_dir_all(&sandbox).expect("create sandbox");
            let file_path = sandbox.join("note.txt");

            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: vec![sandbox.clone()],
                network_allowed: false,
                command_allowlist: Vec::new(),
                allowlist_enabled: true,
            });

            let write_result = runner
                .execute(
                    "write",
                    json!({
                        "path": file_path.display().to_string(),
                        "content": "aman"
                    }),
                    base_context(),
                )
                .await
                .expect("write should succeed");
            assert_eq!(write_result.output["written_bytes"], json!(4));

            let read_result = runner
                .execute(
                    "read",
                    json!({
                        "path": file_path.display().to_string()
                    }),
                    base_context(),
                )
                .await
                .expect("read should succeed");
            assert_eq!(read_result.output["content"], json!("aman"));
        });
    }

    #[tokio::test]
    async fn builtin_http_tool_get_roundtrip() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            let address = listener.local_addr().expect("local addr");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept request");
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer);
                let body = "{\"ok\":true}";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            });

            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: Vec::new(),
                network_allowed: true,
                command_allowlist: Vec::new(),
                allowlist_enabled: true,
            });

            let result = runner
                .execute(
                    "http",
                    json!({
                        "url": format!("http://{}", address),
                        "method": "GET"
                    }),
                    base_context(),
                )
                .await
                .expect("http request should succeed");
            assert_eq!(result.output["status"], json!(200));
            assert_eq!(result.output["json"]["ok"], json!(true));
            server.join().expect("server thread should join");
    }

    #[test]
    fn http_tool_is_blocked_when_network_disabled() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: Vec::new(),
                network_allowed: false,
                command_allowlist: Vec::new(),
                allowlist_enabled: true,
            });

            let error = runner
                .execute(
                    "http",
                    json!({
                        "url": "https://example.com",
                        "method": "GET"
                    }),
                    base_context(),
                )
                .await
                .expect_err("http should be blocked");
            assert!(matches!(error, Error::PermissionDenied { .. }));
        });
    }

    #[test]
    fn builtin_exec_tool_runs_allowlisted_command() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: Vec::new(),
                network_allowed: false,
                command_allowlist: vec!["echo".to_owned()],
                allowlist_enabled: true,
            });

            let result = runner
                .execute(
                    "exec",
                    json!({
                        "command": "echo",
                        "args": ["aman"]
                    }),
                    base_context(),
                )
                .await
                .expect("exec should succeed");
            assert_eq!(result.output["ok"], json!(true));
            assert_eq!(result.output["status"], json!(0));
            assert!(result.output["stdout"].as_str().unwrap_or("").contains("aman"));
        });
    }

    #[test]
    fn exec_tool_is_blocked_when_command_not_allowlisted() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: Vec::new(),
                network_allowed: false,
                command_allowlist: vec!["echo".to_owned()],
                allowlist_enabled: true,
            });

            let error = runner
                .execute(
                    "exec",
                    json!({
                        "command": "uname",
                        "args": ["-a"]
                    }),
                    base_context(),
                )
                .await
                .expect_err("command should be blocked");
            assert!(matches!(error, Error::PermissionDenied { .. }));
        });
    }

    #[test]
    fn exec_tool_times_out_long_running_command() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: Vec::new(),
                network_allowed: false,
                command_allowlist: vec!["sleep".to_owned()],
                allowlist_enabled: true,
            });

            let mut ctx = base_context();
            ctx.base.timeout_ms = Some(10);
            let error = runner
                .execute(
                    "exec",
                    json!({
                        "command": "sleep",
                        "args": ["1"]
                    }),
                    ctx,
                )
                .await
                .expect_err("sleep should timeout");
            assert!(matches!(error, Error::Timeout));
        });
    }

    #[test]
    fn builtin_db_tool_execute_and_query_with_params() {
        pollster::block_on(async {
            let sandbox = std::env::temp_dir().join(format!("aman-db-{}", TraceId::new()));
            std::fs::create_dir_all(&sandbox).expect("create db sandbox");
            let db_path = sandbox.join("test.sqlite3");

            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: vec![sandbox.clone()],
                network_allowed: false,
                command_allowlist: vec!["echo".to_owned()],
                allowlist_enabled: true,
            });

            runner
                .execute(
                    "db",
                    json!({
                        "db_path": db_path.display().to_string(),
                        "operation": "execute",
                        "sql": "CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
                    }),
                    base_context(),
                )
                .await
                .expect("create table");

            let insert = runner
                .execute(
                    "db",
                    json!({
                        "db_path": db_path.display().to_string(),
                        "operation": "execute",
                        "sql": "INSERT INTO users(name, age) VALUES(?, ?)",
                        "params": ["Alice", 18]
                    }),
                    base_context(),
                )
                .await
                .expect("insert row");
            assert_eq!(insert.output["rows_affected"], json!(1));

            let query = runner
                .execute(
                    "db",
                    json!({
                        "db_path": db_path.display().to_string(),
                        "operation": "query",
                        "sql": "SELECT name, age FROM users WHERE age >= ?",
                        "params": [18]
                    }),
                    base_context(),
                )
                .await
                .expect("query rows");
            assert_eq!(query.output["rows"][0]["name"], json!("Alice"));
            assert_eq!(query.output["rows"][0]["age"], json!(18));
        });
    }

    #[test]
    fn db_tool_is_blocked_by_allowed_paths() {
        pollster::block_on(async {
            let allowed = std::env::temp_dir().join(format!("aman-db-allow-{}", TraceId::new()));
            std::fs::create_dir_all(&allowed).expect("create allow dir");
            let forbidden = std::env::temp_dir().join(format!("aman-db-deny-{}", TraceId::new()));
            std::fs::create_dir_all(&forbidden).expect("create deny dir");

            let registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&registry).expect("install builtin tools");
            let runner = ToolRunner::new(registry).with_security(ToolSecurityConfig {
                allowed_paths: vec![allowed],
                network_allowed: false,
                command_allowlist: Vec::new(),
                allowlist_enabled: true,
            });

            let error = runner
                .execute(
                    "db",
                    json!({
                        "db_path": forbidden.join("x.sqlite3").display().to_string(),
                        "operation": "query",
                        "sql": "SELECT 1"
                    }),
                    base_context(),
                )
                .await
                .expect_err("db path should be blocked");
            assert!(matches!(error, Error::PermissionDenied { .. }));
        });
    }

    #[test]
    fn runner_normalizes_non_object_output() {
        pollster::block_on(async {
            let registry = Arc::new(ToolRegistry::new());
            registry
                .register(Arc::new(ScalarTool))
                .expect("register scalar tool");
            let runner = ToolRunner::new(registry);
            let result = runner
                .execute("scalar-tool", json!({}), base_context())
                .await
                .expect("execution succeeds");
            assert_eq!(result.output, json!({"result": "ok"}));
        });
    }

    struct ScalarTool;

    #[async_trait::async_trait]
    impl Tool for ScalarTool {
        fn name(&self) -> &str {
            "scalar-tool"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "string"})));
            &RETURNS
        }

        async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            Ok(json!("ok"))
        }
    }

    #[allow(dead_code)]
    fn _assert_result_shape(result: &ToolExecutionResult) {
        let _ = &result.tool_name;
    }

    // ── shell_split / normalize_exec_command tests ─────────────────

    #[test]
    fn shell_split_plain_words() {
        let words = super::shell_split("python3 -c 'print(1)'");
        assert_eq!(words, vec!["python3", "-c", "print(1)"]);
    }

    #[test]
    fn shell_split_double_quotes() {
        let words = super::shell_split("echo \"hello world\"");
        assert_eq!(words, vec!["echo", "hello world"]);
    }

    #[test]
    fn shell_split_env_prefix() {
        let words = super::shell_split("/usr/bin/env python3 -c 'import sys'");
        assert_eq!(words, vec!["/usr/bin/env", "python3", "-c", "import sys"]);
    }

    #[test]
    fn shell_split_no_quotes() {
        let words = super::shell_split("ls -la /tmp");
        assert_eq!(words, vec!["ls", "-la", "/tmp"]);
    }

    #[test]
    fn normalize_strips_env_prefix() {
        let (cmd, args) = super::normalize_exec_command(
            "/usr/bin/env python3 -c 'print(1)'",
            &[],
        );
        assert_eq!(cmd, "python3");
        assert_eq!(args, vec!["-c", "print(1)"]);
    }

    #[test]
    fn normalize_leaves_clean_command_alone() {
        let (cmd, args) = super::normalize_exec_command("python3", &["-c".into(), "print(1)".into()]);
        assert_eq!(cmd, "python3");
        assert_eq!(args, vec!["-c", "print(1)"]);
    }

    #[test]
    fn normalize_no_args_clean_command() {
        let (cmd, args) = super::normalize_exec_command("ls", &[]);
        assert_eq!(cmd, "ls");
        assert!(args.is_empty());
    }

    #[test]
    fn normalize_respects_explicit_args() {
        // When explicit args are provided, don't override them.
        let (cmd, args) = super::normalize_exec_command(
            "python3 -c 'bad'",
            &["-c".into(), "good".into()],
        );
        assert_eq!(cmd, "python3 -c 'bad'");  // left as-is
        assert_eq!(args, vec!["-c", "good"]);   // explicit args preserved
    }

    #[test]
    fn normalize_bare_env_prefix() {
        let (cmd, args) = super::normalize_exec_command(
            "env python3 -c 'hi'",
            &[],
        );
        assert_eq!(cmd, "python3");
        assert_eq!(args, vec!["-c", "hi"]);
    }
}
