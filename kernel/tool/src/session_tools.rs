// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Session marker tools — let the agent annotate its own session's JSONL
//! with structured signals (e.g. "this session produced no useful output").
//!
//! The marker is appended as a `session:marker` event line. Downstream
//! consumers (the sleep-phase low-value cleanup, the session-list `deletable`
//! flag) read it back by scanning the JSONL for matching event types.

use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::{ExecutionModel, ToolMode};
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::{fs, time::SystemTime};

// Re-exported from the crate root (lib.rs).
use crate::tool_session_id;

// ---------------------------------------------------------------------------
// session  — write a marker to the session's JSONL
// ---------------------------------------------------------------------------

/// Append a structured marker to the current session's persisted JSONL.
///
/// Generic by design: `marker` names the signal kind (e.g. `"deletable"`),
/// `data` carries arbitrary JSON payload. Today the only supported marker is
/// `deletable` (the agent declares it produced no useful output and the
/// session may be cleaned up). Extending it is a matter of adding new
/// `marker` values + corresponding reader logic elsewhere.
///
/// The LLM never needs to pass `session_id` — it is read from the
/// surrounding tool context. `agent_home` defaults to `$HOME/.aman/agents`
/// when omitted.
pub struct SessionTool;

#[async_trait::async_trait]
impl Tool for SessionTool {
    fn name(&self) -> &str {
        "session"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn execution_model(&self) -> ExecutionModel {
        // Mutates the session JSONL file on disk.
        ExecutionModel::Stateful
    }

    fn description(&self) -> &str {
        "Write a structured marker to the current session's persisted event log (JSONL). \
         Use this to annotate the session with signals that downstream automation \
         (cleanup, archival, the UI delete button) can act on. \
         Currently supported marker: `deletable` — set `data.deletable=true` when \
         the session produced no useful output and can be safely removed. \
         The session_id is inferred from context — do not pass it."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["marker", "data"],
                "properties": {
                    "marker": {
                        "type": "string",
                        "description": "Marker kind. Currently only \"deletable\" is supported.",
                        "enum": ["deletable"]
                    },
                    "data": {
                        "type": "object",
                        "description": "Marker payload. For \"deletable\": { deletable: true, reason?: \"...\" }",
                        "properties": {
                            "deletable": {
                                "type": "boolean",
                                "description": "true to mark the session as deletable."
                            },
                            "reason": {
                                "type": "string",
                                "description": "Optional human-readable explanation."
                            }
                        }
                    },
                    "agent_home": {
                        "type": "string",
                        "description": "Optional override for the agents root directory. \
                                       Defaults to $HOME/.aman/agents."
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
                    "written": { "type": "boolean" },
                    "path": { "type": "string" }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, ctx: ToolContext) -> AmanResult<Value> {
        let session_id = tool_session_id(&ctx).ok_or_else(|| Error::ConfigInvalid {
            message: "session tool: no session_id in context".to_owned(),
        })?;

        let marker = params
            .get("marker")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "session tool: \"marker\" must be a string".to_owned(),
            })?;

        if marker != "deletable" {
            return Err(Error::ConfigInvalid {
                message: format!("session tool: unknown marker \"{marker}\" (only \"deletable\" is supported)"),
            });
        }

        let data = params.get("data").ok_or_else(|| Error::ConfigInvalid {
            message: "session tool: \"data\" is required".to_owned(),
        })?;

        // Resolve the agents root: explicit param → $HOME/.aman/agents
        let agents_root: PathBuf = match params.get("agent_home").and_then(Value::as_str) {
            Some(p) => PathBuf::from(p),
            None => {
                let home = std::env::var("HOME").map_err(|_| Error::ConfigInvalid {
                    message: "session tool: $HOME unset and no agent_home given".to_owned(),
                })?;
                PathBuf::from(home).join(".aman").join("agents")
            }
        };

        // Derive agent_id from the session's owning directory. The JSONL
        // lives flat under {agents_root}/{agent_id}/sessions/{session_id}.jsonl.
        // We locate it by scanning agent dirs (mirrors chat_session_list_db).
        let (agent_dir, jsonl_path) = find_session_jsonl(&agents_root, &session_id)
            .ok_or_else(|| Error::ConfigInvalid {
                message: format!(
                    "session tool: could not locate JSONL for session {session_id}"
                ),
            })?;

        let timestamp_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let event = serde_json::json!({
            "event_type": "session:marker",
            "event_id": format!("marker-{}", &session_id[..8.min(session_id.len())]),
            "source": "agent:tool",
            "timestamp_ms": timestamp_ms,
            "payload": {
                "marker": marker,
                "session_id": session_id,
                "agent_id": agent_dir.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "data": data,
            },
        });

        let line = serde_json::to_string(&event).map_err(|e| Error::ConfigInvalid {
            message: format!("session tool: serialize marker: {e}"),
        })?;

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)
            .map_err(|e| Error::ConfigInvalid {
                message: format!("session tool: open {}: {e}", jsonl_path.display()),
            })?;

        writeln!(file, "{line}").map_err(|e| Error::ConfigInvalid {
            message: format!("session tool: write {}: {e}", jsonl_path.display()),
        })?;

        Ok(json!({
            "written": true,
            "path": jsonl_path.display().to_string()
        }))
    }
}

/// Locate a session's JSONL file by scanning all agent directories under
/// `agents_root`. Returns the agent dir and the JSONL path, or None.
fn find_session_jsonl(
    agents_root: &std::path::Path,
    session_id: &str,
) -> Option<(PathBuf, PathBuf)> {
    let entries = std::fs::read_dir(agents_root).ok()?;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let jsonl = entry
            .path()
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        if jsonl.exists() {
            return Some((entry.path(), jsonl));
        }
    }
    None
}
