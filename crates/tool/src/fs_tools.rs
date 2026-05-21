use kernel::context::ToolContext;
use kernel::schema::JsonSchema;
use kernel::tool::Tool;
use kernel::types::ToolMode;
use kernel::{AmanResult, Error};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::{fs, io};

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
                    "bytes": {"type": "integer"}
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
        let content = fs::read_to_string(path)?;
        Ok(json!({
            "content": content,
            "bytes": content.len()
        }))
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

        // Count matches
        let matches: Vec<_> = content.match_indices(old_string).collect();
        let count = matches.len();

        if count == 0 {
            return Err(Error::ConfigInvalid {
                message: format!(
                    "`old_string` not found in `{}`. It may have already been edited, or the exact text differs.",
                    file_path
                ),
            });
        }

        if count > 1 {
            return Err(Error::ConfigInvalid {
                message: format!(
                    "`old_string` matches {count} times in `{}`. Please include more surrounding context to make the match unique.",
                    file_path
                ),
            });
        }

        // Exactly one match — do the replacement.
        let new_content = content.replacen(old_string, new_string, 1);
        fs::write(file_path, &new_content)?;

        Ok(json!({
            "ok": true,
            "replaced": old_string.len()
        }))
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
