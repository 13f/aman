#![forbid(unsafe_code)]

//! Hardline deny patterns for tool execution (inspired by Hermes agent).
//!
//! These patterns are checked **before** the user authorization dialog and
//! **cannot** be approved. They protect against catastrophic operations.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Exec-tool hardline patterns
// ---------------------------------------------------------------------------

/// Patterns that target the root filesystem recursively.
/// Matches `rm -rf /` and `rm /` but NOT `rm /tmp/file.txt`.
static RM_RF_ROOT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\brm\s+(-[^\s]+\s+)*/(\s|$)").unwrap()
});

/// Fork bomb pattern.
static FORK_BOMB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r":\(\)\s*\{\s*:.*\|.*:.*\s*\}\s*;").unwrap()
});

/// `mkfs` — filesystem format.
static MKFS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bmkfs(\.[a-z0-9]+)?\b").unwrap()
});

/// `dd` with raw block device output.
static DD_BLOCK_DEVICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bdd\b[^\n]*\bof=/dev/(sd|nvme|hd|loop|dm-|rdisk)").unwrap()
});

/// `kill -1` or `kill -9 -1` — kill all processes.
static KILL_ALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bkill\s+(-[^\s]+\s+)*-1\b").unwrap()
});

/// Shutdown / reboot / halt / poweroff at the start of a command.
static SHUTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(sudo\s+)?(shutdown|reboot|halt|poweroff)\b").unwrap()
});

/// `chmod` on the root filesystem.
static CHMOD_ROOT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bchmod\s+(-R\s+)?\d+\s+/(\s|$)").unwrap()
});

// ---------------------------------------------------------------------------
// File-tool hardline patterns
// ---------------------------------------------------------------------------

/// Paths that should never be written to.
static DENIED_WRITE_PATHS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        // SSH
        ".ssh/authorized_keys",
        ".ssh/id_rsa",
        ".ssh/id_ed25519",
        ".ssh/config",
        // Shell config
        ".bashrc",
        ".zshrc",
        ".profile",
        ".bash_profile",
        // Credentials
        ".env",
        ".netrc",
        ".pgpass",
        ".npmrc",
        ".pypirc",
        // System
        "/etc/sudoers",
        "/etc/passwd",
        "/etc/shadow",
    ]
});

// ---------------------------------------------------------------------------
// Db-tool hardline patterns
// ---------------------------------------------------------------------------

/// SQL statements that could cause data loss.
/// Note: the `regex` crate does not support look-around, so we check
/// for DROP/TRUNCATE unconditionally and handle DELETE+WHERE in Rust.
static DESTRUCTIVE_SQL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(DROP\s+(TABLE|DATABASE|INDEX)|TRUNCATE)\b").unwrap()
});

/// Match DELETE FROM (with optional table name), then we verify WHERE
/// absence in Rust since the regex crate lacks look-ahead.
static DELETE_FROM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bDELETE\s+FROM\b").unwrap()
});

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check a tool call against all hardline block rules.
///
/// Returns `Some(reason)` if the call matches a hardline rule and must be
/// blocked unconditionally. Returns `None` if the call is allowed to proceed
/// to the user authorization dialog.
///
/// # Arguments
///
/// * `tool_name` — the tool name (e.g. `"exec"`, `"file"`, `"db"`, `"http"`).
/// * `args` — the parsed JSON arguments for the tool call.
#[must_use]
pub fn check_hardline_block(tool_name: &str, args: &Value) -> Option<&'static str> {
    match tool_name {
        "exec" => check_exec_hardline(args),
        "write" | "edit" => check_write_hardline(args),
        "db" => check_db_hardline(args),
        _ => None,
    }
}

fn check_exec_hardline(args: &Value) -> Option<&'static str> {
    let command = args.get("command").and_then(Value::as_str)?;

    // Build the full command string with args for regex matching.
    let full_command = if let Some(arg_array) = args.get("args").and_then(Value::as_array) {
        let arg_str: String = arg_array
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        format!("{command} {arg_str}")
    } else {
        command.to_owned()
    };

    if RM_RF_ROOT.is_match(&full_command) {
        return Some("recursive root filesystem deletion is blocked");
    }
    if FORK_BOMB.is_match(&full_command) {
        return Some("fork bomb is blocked");
    }
    if MKFS.is_match(&full_command) {
        return Some("filesystem format (mkfs) is blocked");
    }
    if DD_BLOCK_DEVICE.is_match(&full_command) {
        return Some("raw block device write (dd) is blocked");
    }
    if KILL_ALL.is_match(&full_command) {
        return Some("kill all processes is blocked");
    }
    if SHUTDOWN.is_match(&full_command) {
        return Some("system shutdown/reboot is blocked");
    }
    if CHMOD_ROOT.is_match(&full_command) {
        return Some("chmod on root filesystem is blocked");
    }

    None
}

fn check_write_hardline(args: &Value) -> Option<&'static str> {
    let path = args
        .get("path")
        .or_else(|| args.get("file_path"))
        .and_then(Value::as_str)?;

    for denied in DENIED_WRITE_PATHS.iter() {
        if path.contains(denied) {
            return Some("write to sensitive path is blocked");
        }
    }

    None
}

fn check_db_hardline(args: &Value) -> Option<&'static str> {
    let sql = args.get("sql").and_then(Value::as_str)?;
    if DESTRUCTIVE_SQL.is_match(sql) {
        return Some("destructive SQL (DROP/TRUNCATE) is blocked");
    }
    if DELETE_FROM.is_match(sql) && !sql.to_uppercase().contains("WHERE") {
        return Some("DELETE without WHERE clause is blocked");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn block_rm_rf_root() {
        let result = check_hardline_block("exec", &json!({"command": "rm", "args": ["-rf", "/"]}));
        assert!(result.is_some(), "rm -rf / should be blocked");
    }

    #[test]
    fn block_rm_rf_root_without_args_array() {
        let result = check_hardline_block("exec", &json!({"command": "rm -rf /"}));
        assert!(result.is_some());
    }

    #[test]
    fn allow_safe_rm() {
        let result =
            check_hardline_block("exec", &json!({"command": "rm", "args": ["/tmp/file.txt"]}));
        assert!(result.is_none(), "rm of a file should be allowed");
    }

    #[test]
    fn block_mkfs() {
        let result = check_hardline_block("exec", &json!({"command": "mkfs.ext4", "args": ["/dev/sda1"]}));
        assert!(result.is_some(), "mkfs should be blocked");
    }

    #[test]
    fn block_dd_block_device() {
        let result = check_hardline_block("exec", &json!({"command": "dd if=/dev/zero of=/dev/sda bs=1M count=1"}));
        assert!(result.is_some(), "dd to block device should be blocked");
    }

    #[test]
    fn block_shutdown() {
        let result = check_hardline_block("exec", &json!({"command": "shutdown", "args": ["-h", "now"]}));
        assert!(result.is_some(), "shutdown should be blocked");
    }

    #[test]
    fn block_sudo_shutdown() {
        let result = check_hardline_block("exec", &json!({"command": "sudo shutdown -h now"}));
        assert!(result.is_some(), "sudo shutdown should be blocked");
    }

    #[test]
    fn allow_ls() {
        let result = check_hardline_block("exec", &json!({"command": "ls", "args": ["-la", "/tmp"]}));
        assert!(result.is_none(), "ls should be allowed");
    }

    #[test]
    fn block_write_to_ssh() {
        let result = check_hardline_block(
            "write",
            &json!({
                "path": "/Users/test/.ssh/authorized_keys"
            }),
        );
        assert!(result.is_some(), "write to .ssh/authorized_keys should be blocked");
    }

    #[test]
    fn allow_write_to_temp() {
        let result = check_hardline_block(
            "write",
            &json!({
                "path": "/tmp/test.txt"
            }),
        );
        assert!(result.is_none(), "write to /tmp should be allowed");
    }

    #[test]
    fn block_destructive_sql() {
        let result = check_hardline_block(
            "db",
            &json!({
                "sql": "DROP TABLE users",
                "db_path": "/tmp/test.db"
            }),
        );
        assert!(result.is_some(), "DROP TABLE should be blocked");
    }

    #[test]
    fn allow_safe_sql() {
        let result = check_hardline_block(
            "db",
            &json!({
                "sql": "SELECT * FROM users",
                "db_path": "/tmp/test.db"
            }),
        );
        assert!(result.is_none(), "SELECT should be allowed");
    }

    #[test]
    fn no_hardline_for_unknown_tool() {
        let result = check_hardline_block("web_search", &json!({"query": "hello"}));
        assert!(result.is_none());
    }

    #[test]
    fn block_chmod_root() {
        let result = check_hardline_block("exec", &json!({"command": "chmod -R 777 /"}));
        assert!(result.is_some(), "chmod -R 777 / should be blocked");
    }

    #[test]
    fn block_fork_bomb() {
        let result = check_hardline_block("exec", &json!({"command": ":(){ :|:& };:"}));
        assert!(result.is_some(), "fork bomb should be blocked");
    }

    #[test]
    fn block_kill_minus_one() {
        let result = check_hardline_block("exec", &json!({"command": "kill -9 -1"}));
        assert!(result.is_some(), "kill -9 -1 should be blocked");
    }
}
