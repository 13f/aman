// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::{fs, io};

// ---------------------------------------------------------------------------
// Shared guards
// ---------------------------------------------------------------------------

/// File extensions that should never be read (binary, non-text formats).
static BINARY_EXTENSIONS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        ".o", ".so", ".dylib", ".pyc", ".pyo", ".class", ".jar",
        ".png", ".jpg", ".jpeg", ".gif", ".ico", ".bmp", ".svg",
        ".pdf", ".zip", ".tar", ".gz", ".bz2", ".xz", ".zst",
        ".a", ".lib", ".dll", ".exe", ".wasm",
        ".mp3", ".mp4", ".avi", ".mov", ".wav", ".flac", ".ogg",
        ".ttf", ".woff", ".woff2", ".eot",
        ".db", ".sqlite", ".sqlite3",
        ".lockb", // bun lock
    ]
    .iter()
    .copied()
    .collect()
});

/// Global tracker for consecutive file reads.
static READ_TRACKER: LazyLock<Mutex<HashMap<String, u32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Track a file read and return a warning if too many consecutive reads.
pub fn track_read(path: &str) -> Option<&'static str> {
    let mut tracker = READ_TRACKER.lock().expect("read tracker lock");
    let count = tracker.entry(path.to_owned()).or_insert(0);
    *count += 1;
    match *count {
        3 => Some("You've read this file 3 times consecutively. Consider whether you already have the needed information."),
        n if n >= 4 => Some("consecutive read limit reached — try a different approach"),
        _ => None,
    }
}

/// Reset consecutive read tracking — call when any non-read tool executes.
pub fn reset_read_tracker() {
    READ_TRACKER.lock().expect("read tracker lock").clear();
}

/// Check if a file path has a binary extension.
fn is_binary_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    BINARY_EXTENSIONS.iter().any(|&ext| lower.ends_with(ext))
}

/// Sanitize common API key patterns from text content.
fn redact_sensitive_text(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut result = text.to_owned();
    let mut search_start = 0;
    while let Some(pos) = lower[search_start..].find("sk-") {
        let abs_pos = search_start + pos;
        let ctx_start = abs_pos.saturating_sub(40);
        let ctx = &lower[ctx_start..abs_pos];
        if ctx.contains("apikey") || ctx.contains("api_key")
            || ctx.contains("api-key") || ctx.contains("bearer")
            || ctx.contains("authorization")
        {
            let key_start = abs_pos;
            let mut key_end = key_start;
            for ch in text[key_start..].chars() {
                if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                    key_end += ch.len_utf8();
                } else {
                    break;
                }
            }
            if key_end - key_start >= 20 {
                result.replace_range(key_start..key_end, "[REDACTED]");
                break;
            }
        }
        search_start = abs_pos + 3;
    }
    result
}

/// Normalize whitespace for fuzzy matching: collapse runs of spaces/tabs to
/// a single space, and strip leading/trailing whitespace from each line.
fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_was_space = false;
    for ch in text.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(ch);
            prev_was_space = false;
        }
    }
    // Strip leading/trailing whitespace per line
    out.lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// ReadTool
// ---------------------------------------------------------------------------

pub struct ReadTool;

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file to read"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-based line number to start reading from (default 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max lines to return (default 500, max 2000)"
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
                    "content": {"type": "string"},
                    "bytes": {"type": "integer"},
                    "total_lines": {"type": "integer"},
                    "offset": {"type": "integer"},
                    "limit": {"type": "integer"},
                    "truncated": {"type": "boolean"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "path must be a string".to_owned(),
            })?;

        // Binary file guard
        if is_binary_path(path) {
            return Err(Error::ConfigInvalid {
                message: format!("refusing to read binary file: {path}"),
            });
        }

        let offset: usize = params
            .get("offset")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(1)
            .max(1);
        let limit: usize = params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(500)
            .min(2000);

        // Consecutive read tracking
        if let Some(warning) = track_read(path) {
            return Err(Error::ConfigInvalid {
                message: warning.to_owned(),
            });
        }

        let content = fs::read_to_string(path)?;
        let total_lines = content.lines().count();
        let lines: Vec<&str> = content.lines().collect();

        let start_idx = (offset - 1).min(total_lines);
        let end_idx = (start_idx + limit).min(total_lines);
        let truncated = end_idx < total_lines;

        let selected: String = lines[start_idx..end_idx]
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .join("\n");

        // Redact sensitive patterns from content
        let selected = redact_sensitive_text(&selected);

        let mut result = json!({
            "content": selected,
            "bytes": selected.len(),
            "total_lines": total_lines,
            "offset": offset,
            "limit": end_idx - start_idx,
            "truncated": truncated,
        });

        // Large-file hint
        if total_lines > 1000 && limit >= 500 {
            result["hint"] = json!(format!(
                "File has {total_lines} lines. Use offset=N&limit=M to read in chunks."
            ));
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// WriteTool
// ---------------------------------------------------------------------------

pub struct WriteTool;

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to write to"
                    },
                    "content": {
                        "type": "string",
                        "description": "File content to write"
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
                    "written_bytes": {"type": "integer"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "path must be a string".to_owned(),
            })?;
        let content = params
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "content must be a string".to_owned(),
            })?;
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        Ok(json!({
            "written_bytes": content.len()
        }))
    }
}

// ---------------------------------------------------------------------------
// EditTool  — exact string replacement
// ---------------------------------------------------------------------------

pub struct EditTool;

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["file_path", "old_string", "new_string"],
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text to search for (must match exactly once)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text"
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
                    "replaced": {"type": "integer"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let file_path = params
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "file_path must be a string".to_owned(),
            })?;
        let old_string = params
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "old_string must be a string".to_owned(),
            })?;
        let new_string = params
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "new_string must be a string".to_owned(),
            })?;

        let content = fs::read_to_string(file_path)?;

        // Try exact match first.
        let matches: Vec<_> = content.match_indices(old_string).collect();
        let count = matches.len();

        let (new_content, method) = if count == 1 {
            (content.replacen(old_string, new_string, 1), "exact")
        } else if count == 0 {
            // Fallback: fuzzy match (whitespace-normalized).
            let normalized_content = normalize_whitespace(&content);
            let normalized_old = normalize_whitespace(old_string);
            let fuzzy_idx = normalized_content.find(&normalized_old);
            if fuzzy_idx.is_some() {
                // Map the position back to the original content by aligning
                // whitespace runs. We rebuild the replacement using the
                // original content's whitespace by matching byte-for-byte
                // through the normalized string and finding the corresponding
                // span in the original.
                let mut orig_pos = 0usize;
                let mut norm_pos = 0usize;
                let norm_bytes = normalized_old.as_bytes();
                while norm_pos < norm_bytes.len() && orig_pos < content.len() {
                    let ob = content.as_bytes()[orig_pos];
                    let nb = norm_bytes[norm_pos];
                    let is_ws_orig = ob == b' ' || ob == b'\t';
                    let is_ws_norm = nb == b' ';
                    if is_ws_orig && is_ws_norm {
                        // Skip all whitespace in original, advance one in normalized
                        while orig_pos < content.len()
                            && (content.as_bytes()[orig_pos] == b' '
                                || content.as_bytes()[orig_pos] == b'\t')
                        {
                            orig_pos += 1;
                        }
                        norm_pos += 1;
                    } else if ob == nb {
                        orig_pos += 1;
                        norm_pos += 1;
                    } else {
                        // Mismatch — fuzzy match failed
                        break;
                    }
                }
                if norm_pos == norm_bytes.len() {
                    let end_pos = orig_pos;
                    let start_pos = end_pos - old_string.len();
                    let mut new = String::with_capacity(content.len() + new_string.len() - old_string.len());
                    new.push_str(&content[..start_pos]);
                    new.push_str(new_string);
                    new.push_str(&content[end_pos..]);
                    (new, "fuzzy")
                } else {
                    return Err(Error::ConfigInvalid {
                        message: format!(
                            "`old_string` not found in `{}`. Whitespace-normalized match also failed.",
                            file_path
                        ),
                    });
                }
            } else {
                return Err(Error::ConfigInvalid {
                    message: format!(
                        "`old_string` not found in `{}`. It may have already been edited, or the exact text differs.",
                        file_path
                    ),
                });
            }
        } else {
            return Err(Error::ConfigInvalid {
                message: format!(
                    "`old_string` matches {count} times in `{}`. Please include more surrounding context to make the match unique.",
                    file_path
                ),
            });
        };

        fs::write(file_path, &new_content)?;

        // Post-write syntax check for JSON files.
        let mut result = json!({
            "ok": true,
            "replaced": old_string.len(),
            "method": method,
        });

        if file_path.ends_with(".json") {
            match serde_json::from_str::<Value>(&new_content) {
                Ok(_) => {
                    result["syntax_check"] = json!("ok");
                }
                Err(e) => {
                    result["syntax_check"] = json!("invalid");
                    result["syntax_error"] = json!(format!("JSON syntax error: {e}"));
                }
            }
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// ListTool
// ---------------------------------------------------------------------------

pub struct ListTool;

#[async_trait::async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the directory to list"
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
                    "entries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "type": {"type": "string"},
                                "size": {"type": "integer"}
                            }
                        }
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "path must be a string".to_owned(),
            })?;

        let entries = list_directory(path)?;
        Ok(json!({ "entries": entries }))
    }
}

fn list_directory(path: &str) -> AmanResult<Vec<Value>> {
    let dir = fs::read_dir(path).map_err(|e| Error::ConfigInvalid {
        message: format!("cannot read directory `{path}`: {e}"),
    })?;

    let mut entries: Vec<Value> = Vec::new();
    for entry in dir {
        let entry = entry.map_err(|e| Error::ConfigInvalid {
            message: format!("error reading entry in `{path}`: {e}"),
        })?;

        let meta = entry.metadata().map_err(|e| Error::ConfigInvalid {
            message: format!("error reading metadata for `{}`: {e}", entry.path().display()),
        })?;

        let entry_type = if meta.is_dir() {
            "dir"
        } else if meta.is_symlink() {
            "symlink"
        } else {
            "file"
        };

        entries.push(json!({
            "name": entry.file_name().to_string_lossy(),
            "type": entry_type,
            "size": meta.len(),
        }));
    }

    // Sort: dirs first, then files, alphabetically within each group.
    entries.sort_by(|a, b| {
        let a_is_dir = a["type"].as_str() == Some("dir");
        let b_is_dir = b["type"].as_str() == Some("dir");
        match (a_is_dir, b_is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")),
        }
    });

    Ok(entries)
}

// ---------------------------------------------------------------------------
// FindTool
// ---------------------------------------------------------------------------

pub struct FindTool;

#[async_trait::async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["pattern", "base"],
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Filename to search for (substring match)"
                    },
                    "base": {
                        "type": "string",
                        "description": "Base directory to search in"
                    },
                    "type": {
                        "type": "string",
                        "description": "Filter: 'file', 'dir', or omit for both",
                        "enum": ["file", "dir"]
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
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"},
                                "type": {"type": "string"},
                                "size": {"type": "integer"}
                            }
                        }
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let pattern = params
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "pattern must be a string".to_owned(),
            })?;
        let base = params
            .get("base")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "base must be a string".to_owned(),
            })?;
        let filter_type = params.get("type").and_then(Value::as_str);

        let results = find_files(base, pattern, filter_type)?;
        Ok(json!({ "results": results }))
    }
}

fn find_files(base: &str, pattern: &str, filter_type: Option<&str>) -> AmanResult<Vec<Value>> {
    let base_path = PathBuf::from(base);
    if !base_path.is_dir() {
        return Err(Error::ConfigInvalid {
            message: format!("base path is not a directory: {base}"),
        });
    }

    let mut results = Vec::new();
    let mut dirs = vec![base_path];

    while let Some(dir) = dirs.pop() {
        let read_dir = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => continue,
            Err(e) => {
                return Err(Error::ConfigInvalid {
                    message: format!("error reading directory `{}`: {e}", dir.display()),
                });
            }
        };

        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            let entry_name = entry.file_name().to_string_lossy().to_lowercase();
            let pattern_lower = pattern.to_lowercase();

            // Check if filename matches the pattern (substring match, case-insensitive).
            if entry_name.contains(&pattern_lower) {
                let meta = entry.metadata().ok();
                let entry_type = if file_type.is_dir() {
                    "dir"
                } else if file_type.is_symlink() {
                    "symlink"
                } else {
                    "file"
                };

                match filter_type {
                    Some("file") if file_type.is_dir() => {}
                    Some("dir") if !file_type.is_dir() => {}
                    _ => {
                        results.push(json!({
                            "path": entry.path().to_string_lossy(),
                            "type": entry_type,
                            "size": meta.map(|m| m.len()).unwrap_or(0),
                        }));
                    }
                }
            }

            // Recurse into subdirectories.
            if file_type.is_dir() {
                dirs.push(entry.path());
            }
        }
    }

    // Sort by path.
    results.sort_by(|a, b| a["path"].as_str().unwrap_or("").cmp(b["path"].as_str().unwrap_or("")));

    Ok(results)
}

// ---------------------------------------------------------------------------
// GrepTool  —  wraps ripgrep for content search
// ---------------------------------------------------------------------------

pub struct GrepTool;

/// Cached check that `rg` is on PATH.
static RG_AVAILABLE: LazyLock<bool> = LazyLock::new(|| {
    Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
});

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["pattern", "path"],
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex by default; use fixed_strings for literal)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Base directory to search in"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional file glob filter, e.g. \"*.rs\", \"*.{ts,js}\""
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Max matching results to return (default 100, max 500)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Number of results to skip for pagination (default 0)"
                    },
                    "fixed_strings": {
                        "type": "boolean",
                        "description": "Treat pattern as literal string, not regex"
                    },
                    "context_lines": {
                        "type": "integer",
                        "description": "Lines of context before/after each match (default 0)"
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
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"},
                                "line_number": {"type": "integer"},
                                "text": {"type": "string"}
                            }
                        }
                    },
                    "total_matches": {"type": "integer"},
                    "matching_files": {"type": "integer"},
                    "truncated": {"type": "boolean"}
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> AmanResult<Value> {
        let pattern = params
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "pattern must be a string".to_owned(),
            })?;
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::ConfigInvalid {
                message: "path must be a string".to_owned(),
            })?;
        let max_results = params
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .min(500);
        let offset = params
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(10_000) as usize;
        let fixed_strings = params
            .get("fixed_strings")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let context_lines = params
            .get("context_lines")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let glob = params.get("glob").and_then(Value::as_str);

        if !*RG_AVAILABLE {
            return Err(Error::ConfigInvalid {
                message: "ripgrep (rg) is not installed. Install with: brew install ripgrep".to_owned(),
            });
        }

        let mut cmd = Command::new("rg");
        cmd.arg("--json")
            .arg("--no-heading")
            .arg("--color")
            .arg("never")
            .arg("--max-count")
            .arg(max_results.to_string());

        if fixed_strings {
            cmd.arg("--fixed-strings");
        }
        if context_lines > 0 {
            cmd.arg("-C").arg(context_lines.to_string());
        }
        if let Some(g) = glob {
            cmd.arg("-g").arg(g);
        }

        cmd.arg(pattern).arg(path);

        let output = cmd.output().map_err(|e| Error::Unrecoverable {
            message: format!("failed to run rg: {e}"),
        })?;

        let (all_results, matching_files) = parse_rg_json_output(&output.stdout, max_results + offset as u64);

        // Apply offset — skip the first N results.
        let offset = offset.min(all_results.len());
        let results: Vec<Value> = all_results.into_iter().skip(offset).collect();

        let total_matches = results.len() as u64;
        let truncated = total_matches >= max_results;

        let mut result = json!({
            "results": results,
            "total_matches": total_matches,
            "matching_files": matching_files,
            "truncated": truncated,
        });

        // Truncation hint for pagination.
        if truncated {
            let next_offset = offset + max_results as usize;
            result["hint"] = json!(format!(
                "Results truncated. Use offset={next_offset} to see the next page."
            ));
        }

        // Attach stderr as warning if rg reported anything (e.g. binary file skips).
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            result["warning"] = json!(stderr.trim());
        }

        Ok(result)
    }
}

/// Parse ripgrep `--json` output into structured results.
fn parse_rg_json_output(output: &[u8], max_results: u64) -> (Vec<Value>, u64) {
    let mut results = Vec::new();
    let mut seen_files = std::collections::HashSet::new();

    for line in output.split(|&b| b == b'\n') {
        if line.is_empty() || results.len() as u64 >= max_results {
            continue;
        }

        let parsed: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if parsed["type"] == "match" {
            if let Some(data) = parsed.get("data") {
                let file = data["path"]["text"].as_str().unwrap_or("");
                let line_number = data["line_number"].as_u64().unwrap_or(0);
                let raw_text = data["lines"]["text"].as_str().unwrap_or("");
                let text = raw_text.trim_end_matches('\n').trim_end_matches('\r');

                seen_files.insert(file.to_owned());
                results.push(json!({
                    "path": file,
                    "line_number": line_number,
                    "text": text,
                }));
            }
        }
    }

    let matching_files = seen_files.len() as u64;
    (results, matching_files)
}
