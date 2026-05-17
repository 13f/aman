#![forbid(unsafe_code)]

//! Bridge between Aman's `kernel::tool::Tool` and rig's `ToolDyn` for LLM
//! native function calling. Each registered kernel tool is wrapped in a
//! `LlmNativeTool` and passed to the rig agent builder via `builder.tools()`.

use event_bus::EventBus;
use kernel::context::ToolContext;
use kernel::event::{Event, EventType};
use rig_core::completion::ToolDefinition;
use rig_core::tool::{ToolDyn, ToolError};
use rig_core::wasm_compat::WasmBoxedFuture;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tool::auth::AuthRegistry;
use tool::security;

/// Sequential counter for unique tool call IDs within the process lifetime.
static LLM_TOOL_SEQ: AtomicU64 = AtomicU64::new(0);

/// Wraps a `kernel::tool::Tool` as a `rig_core::tool::ToolDyn` for LLM
/// function calling.
///
/// Each registered Aman tool (exec, file, http, db, web_search, etc.) gets
/// wrapped in this struct and registered with the rig agent. The wrapper:
///
/// 1. Checks hardline deny rules (unconditional block, no auth bypass).
/// 2. Emits `llm_tool_call` progress events so the UI displays live updates.
/// 3. If `require_auth` is set, registers with `AuthRegistry`, emits
///    `tool_auth_required`, and awaits user approval (60s timeout).
/// 4. Calls the underlying `kernel::tool::Tool::execute()`.
/// 5. Emits `llm_tool_result` with a preview of the output.
pub struct LlmNativeTool {
    /// The underlying Aman tool (exec, file, http, etc.).
    pub inner: Arc<dyn kernel::tool::Tool>,
    /// Event bus for publishing progress events.
    pub bus: Option<Arc<dyn EventBus>>,
    /// Session ID scoping for progress events.
    pub session_id: Option<String>,
    /// Whether this tool requires user authorization before execution.
    pub require_auth: bool,
    /// Shared auth registry for pending approval requests.
    pub auth_registry: Option<Arc<AuthRegistry>>,
}

impl ToolDyn for LlmNativeTool {
    fn name(&self) -> String {
        self.inner.name().to_owned()
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let name = self.inner.name().to_owned();
        let params = self.inner.parameters().as_value().clone();
        Box::pin(async move {
            ToolDefinition {
                name: name.clone(),
                description: tool_description(&name),
                parameters: params,
            }
        })
    }

    fn call(&self, args_str: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        // Clone the data we need (self is &self, so we can't move out).
        let inner = Arc::clone(&self.inner);
        let bus = self.bus.clone();
        let session_id = self.session_id.clone();
        let require_auth = self.require_auth;
        let auth_registry = self.auth_registry.clone();

        Box::pin(async move {
            let tool_name = inner.name().to_owned();
            let call_id = format!(
                "{}-{}",
                tool_name,
                LLM_TOOL_SEQ.fetch_add(1, Ordering::Relaxed)
            );

            // Step 1: Parse arguments.
            let params: Value = match serde_json::from_str(&args_str) {
                Ok(v) => v,
                Err(e) => {
                    emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                        "session_id": session_id,
                        "call_id": &call_id,
                        "tool_name": &tool_name,
                        "status": "failed",
                        "error": format!("invalid arguments: {e}"),
                    })).await;
                    return Err(ToolError::JsonError(e));
                }
            };

            // Step 2: Check hardline block rules (unconditional deny).
            if let Some(reason) = security::check_hardline_block(&tool_name, &params) {
                emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                    "session_id": session_id,
                    "call_id": &call_id,
                    "tool_name": &tool_name,
                    "status": "failed",
                    "error": reason,
                })).await;
                return Err(ToolError::ToolCallError(Box::new(
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, reason),
                )));
            }

            // Step 3: Emit tool_call progress event.
            emit_event(&bus, &session_id, "llm_tool_call", serde_json::json!({
                "session_id": session_id,
                "call_id": &call_id,
                "tool_name": &tool_name,
                "arguments": &args_str,
            })).await;

            // Step 4: Authorization (if required).
            if require_auth {
                if let Some(ref registry) = auth_registry {
                    let auth_id = uuid::Uuid::now_v7().to_string();
                    let rx = registry.register(auth_id.clone());

                    emit_event(&bus, &session_id, "tool_auth_required", serde_json::json!({
                        "auth_id": &auth_id,
                        "session_id": session_id,
                        "tool_name": &tool_name,
                        "arguments_summary": summarize_args(&params),
                        "call_id": &call_id,
                    })).await;

                    // Await user response with 60-second timeout.
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        rx,
                    )
                    .await
                    {
                        Ok(Ok(true)) => {
                            // Approved — proceed.
                        }
                        Ok(Ok(false)) => {
                            registry.remove(&auth_id);
                            emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                                "session_id": session_id,
                                "call_id": &call_id,
                                "tool_name": &tool_name,
                                "status": "denied",
                                "error": "Tool execution denied by user",
                            })).await;
                            return Err(ToolError::ToolCallError(Box::new(
                                std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    format!("tool '{tool_name}' execution denied by user"),
                                ),
                            )));
                        }
                        Ok(Err(_)) => {
                            registry.remove(&auth_id);
                            emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                                "session_id": session_id,
                                "call_id": &call_id,
                                "tool_name": &tool_name,
                                "status": "failed",
                                "error": "authorization channel closed",
                            })).await;
                            return Err(ToolError::ToolCallError(Box::new(
                                std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    format!("tool '{tool_name}' authorization channel closed"),
                                ),
                            )));
                        }
                        Err(_elapsed) => {
                            registry.remove(&auth_id);
                            emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                                "session_id": session_id,
                                "call_id": &call_id,
                                "tool_name": &tool_name,
                                "status": "timeout",
                                "error": "authorization timed out after 60s",
                            })).await;
                            return Err(ToolError::ToolCallError(Box::new(
                                std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    format!("tool '{tool_name}' authorization timed out"),
                                ),
                            )));
                        }
                    }
                }
            }

            // Step 5: Execute the native tool.
            let ctx = ToolContext::default();
            let result = match inner.execute(params, ctx).await {
                Ok(v) => v,
                Err(e) => {
                    let err_msg = format!("{e}");
                    emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                        "session_id": session_id,
                        "call_id": &call_id,
                        "tool_name": &tool_name,
                        "status": "failed",
                        "error": &err_msg,
                    })).await;
                    return Err(ToolError::ToolCallError(Box::new(
                        std::io::Error::new(std::io::ErrorKind::Other, err_msg),
                    )));
                }
            };

            // Step 6: Serialize and emit result with preview.
            let result_str = serde_json::to_string(&result)
                .map_err(ToolError::JsonError)?;
            let preview: String = result_str.chars().take(1000).collect();

            emit_event(&bus, &session_id, "llm_tool_result", serde_json::json!({
                "session_id": session_id,
                "call_id": &call_id,
                "tool_name": &tool_name,
                "status": "success",
                "result": &preview,
            })).await;

            Ok(result_str)
        })
    }
}

/// Emit a progress event on the event bus (if bus and session_id are available).
async fn emit_event(
    bus: &Option<Arc<dyn EventBus>>,
    session_id: &Option<String>,
    event_type: &str,
    mut payload: Value,
) {
    if let (Some(bus), Some(sid)) = (bus.as_ref(), session_id.as_ref()) {
        if payload.get("session_id").and_then(|v| v.as_str()).is_none() {
            payload["session_id"] = serde_json::json!(sid);
        }
        let _ = bus
            .publish(Event::new(
                "plugin:llm",
                EventType::Custom(event_type.to_owned()),
                payload,
            ))
            .await;
    }
}

/// Map a tool name to a human-readable description for the LLM.
fn tool_description(name: &str) -> String {
    match name {
        "web_search" => "Search the web for real-time information. Use this when you need current data, news, facts, or any information not available in your training data.".to_owned(),
        "exec" => "Execute a shell command on the local system. Use this to run programs, scripts, or CLI tools.".to_owned(),
        "file" => "Read, write, delete, or move files on the local filesystem.".to_owned(),
        "http" => "Make HTTP requests to external APIs and websites. Supports GET, POST, PUT, DELETE, and other methods.".to_owned(),
        "db" => "Query or execute SQL statements against a SQLite database.".to_owned(),
        other => format!("{other} tool").to_owned(),
    }
}

/// Produce a shortened summary of tool arguments for the auth dialog.
fn summarize_args(params: &Value) -> String {
    match params {
        Value::Object(map) => {
            let mut parts: Vec<String> = Vec::new();
            for (key, val) in map.iter() {
                let val_str = match val {
                    Value::String(s) => {
                        if s.len() > 80 {
                            format!("{}...", &s[..77])
                        } else {
                            s.clone()
                        }
                    }
                    Value::Array(arr) => format!("[{} items]", arr.len()),
                    Value::Object(_) => "{...}".to_owned(),
                    other => other.to_string(),
                };
                parts.push(format!("{key}={val_str}"));
            }
            let joined = parts.join(", ");
            if joined.len() > 400 {
                format!("{}...", &joined[..397])
            } else {
                joined
            }
        }
        other => {
            let s = serde_json::to_string(other).unwrap_or_default();
            if s.len() > 200 {
                format!("{}...", &s[..197])
            } else {
                s
            }
        }
    }
}
