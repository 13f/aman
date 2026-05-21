use crate::error::AmanResult;
use crate::Error;
use semver::{Version, VersionReq};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// A reusable script runtime that executes scripts with a given interpreter.
///
/// Checks availability of the runtime via `which`, optionally validates its
/// version, then runs scripts by piping JSON input via stdin and capturing
/// stdout as the result.
///
/// Reusable across hooks, plugins, and custom event sources.
pub struct ScriptRuntime {
    /// Interpreter binary name (e.g. "python3", "node", "deno").
    runtime: String,
    /// Optional minimum version requirement (e.g. ">=3.8").
    min_version: Option<VersionReq>,
}

impl ScriptRuntime {
    /// Create a new script runtime configuration.
    ///
    /// `min_version` is an optional semver range string (e.g. `">=3.8"`, `">=18.0 <19.0"`).
    pub fn new(runtime: impl Into<String>, min_version: Option<&str>) -> Self {
        let version_req = min_version.and_then(|v| VersionReq::parse(v).ok());
        Self {
            runtime: runtime.into(),
            min_version: version_req,
        }
    }

    /// The interpreter binary name.
    #[must_use]
    pub fn runtime_name(&self) -> &str {
        &self.runtime
    }

    /// The optional version requirement.
    #[must_use]
    pub fn min_version(&self) -> Option<&VersionReq> {
        self.min_version.as_ref()
    }

    /// Check whether the runtime is available on `PATH` and meets the
    /// minimum version requirement.
    pub fn check_available(&self) -> AmanResult<()> {
        // Check that the runtime exists on PATH.
        let which = if cfg!(target_os = "windows") {
            Command::new("where").arg(&self.runtime).output()
        } else {
            Command::new("which").arg(&self.runtime).output()
        };

        match which {
            Ok(output) if output.status.success() => {}
            Ok(_) => {
                return Err(Error::NotFound {
                    name: format!("runtime `{}` not found on PATH", self.runtime),
                });
            }
            Err(e) => {
                return Err(Error::Unrecoverable {
                    message: format!("failed to check for runtime `{}`: {e}", self.runtime),
                });
            }
        }

        // Check version if required.
        if let Some(req) = &self.min_version {
            let version_output = Command::new(&self.runtime)
                .arg("--version")
                .output()
                .map_err(|e| Error::Unrecoverable {
                    message: format!(
                        "failed to get version for runtime `{}`: {e}",
                        self.runtime
                    ),
                })?;

            let version_str = extract_version(&version_output);
            let version = parse_version(&version_str).ok_or_else(|| Error::Unrecoverable {
                message: format!(
                    "could not parse version from `{} --version`: {version_str:?}",
                    self.runtime
                ),
            })?;

            if !req.matches(&version) {
                return Err(Error::VersionMismatch {
                    expected: req.to_string(),
                    found: version.to_string(),
                });
            }
        }

        Ok(())
    }

    /// Execute a script with the given JSON input on stdin.
    ///
    /// Returns the script's stdout as a string. If the script exits with a
    /// non-zero status, the stderr is included in the error message.
    pub fn execute(&self, script_path: &Path, input: &Value) -> AmanResult<String> {
        let script_str = script_path.display().to_string();

        let mut child = Command::new(&self.runtime)
            .arg(&script_str)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Unrecoverable {
                message: format!(
                    "failed to spawn script `{script_str}` with runtime `{}`: {e}",
                    self.runtime
                ),
            })?;

        // Write JSON input to stdin.
        if let Some(ref mut stdin) = child.stdin {
            serde_json::to_writer(stdin, input).map_err(|e| Error::SerdeJson(e))?;
        }

        let output = child
            .wait_with_output()
            .map_err(|e| Error::Unrecoverable {
                message: format!(
                    "failed to read output from script `{script_str}`: {e}",
                ),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Unrecoverable {
                message: format!(
                    "script `{script_str}` exited with {}: {}",
                    output.status,
                    stderr.trim()
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(stdout)
    }
}

/// Extract a version string from `--version` output by taking the first
/// whitespace-separated token that looks like a semver.
fn extract_version(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stdout);
    if text.is_empty() {
        return String::from_utf8_lossy(&output.stderr).trim().to_owned();
    }
    text.trim().to_owned()
}

/// Parse the first semver-looking token from a version string.
fn parse_version(text: &str) -> Option<Version> {
    // Try the whole string first.
    if let Ok(v) = Version::parse(text) {
        return Some(v);
    }
    // Try each whitespace-separated token.
    for token in text.split_whitespace() {
        // Strip common prefixes like "v", "Python", "Node.js", etc.
        let cleaned = token
            .trim_start_matches('v')
            .trim_start_matches('V');
        if let Ok(v) = Version::parse(cleaned) {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_direct() {
        assert_eq!(parse_version("3.8.0"), Some(Version::new(3, 8, 0)));
        assert_eq!(parse_version("v18.0.0"), Some(Version::new(18, 0, 0)));
        assert_eq!(parse_version("Python 3.11.0"), Some(Version::new(3, 11, 0)));
        assert_eq!(parse_version("node v20.0.0"), Some(Version::new(20, 0, 0)));
        assert_eq!(parse_version("Deno 1.34.0"), Some(Version::new(1, 34, 0)));
    }

    #[test]
    fn test_parse_version_invalid() {
        assert!(parse_version("not a version").is_none());
        assert!(parse_version("").is_none());
    }

    #[test]
    fn test_script_runtime_new() {
        let rt = ScriptRuntime::new("python3", Some(">=3.8"));
        assert_eq!(rt.runtime_name(), "python3");
        assert!(rt.min_version().is_some());

        let rt = ScriptRuntime::new("node", None);
        assert_eq!(rt.runtime_name(), "node");
        assert!(rt.min_version().is_none());
    }

    #[test]
    fn test_runtime_check_available_python3() {
        let rt = ScriptRuntime::new("python3", None);
        // This should pass if python3 is on PATH (common in dev environments).
        let result = rt.check_available();
        // Just verify it doesn't crash — the test environment may or may not have python3.
        if let Err(e) = &result {
            // If python3 isn't available, that's OK — just verify the error type.
            assert!(
                matches!(e, Error::NotFound { .. }),
                "unexpected error: {e}"
            );
        }
    }

    #[test]
    fn test_runtime_check_fails_for_nonexistent() {
        let rt = ScriptRuntime::new("this-runtime-does-not-exist-12345", None);
        let result = rt.check_available();
        assert!(matches!(result, Err(Error::NotFound { .. })));
    }

    #[test]
    fn test_execute_simple_python_script() {
        let rt = match ScriptRuntime::new("python3", None).check_available() {
            Ok(()) => ScriptRuntime::new("python3", None),
            Err(_) => return, // skip if python3 not available
        };

        // Create a temporary Python script.
        let dir = std::env::temp_dir().join(format!("aman-script-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("test_echo.py");
        std::fs::write(
            &script,
            r#"
import sys, json
data = json.load(sys.stdin)
print(json.dumps({"echo": data}))
"#,
        )
        .expect("write test script");

        let input = serde_json::json!({"hello": "world"});
        let result = rt.execute(&script, &input).expect("execute script");
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("parse script output");
        assert_eq!(parsed["echo"]["hello"], "world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_execute_script_non_zero_exit() {
        let rt = match ScriptRuntime::new("python3", None).check_available() {
            Ok(()) => ScriptRuntime::new("python3", None),
            Err(_) => return,
        };

        let dir = std::env::temp_dir().join(format!("aman-script-fail-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("fail.py");
        std::fs::write(&script, "import sys; sys.exit(1)").expect("write fail script");

        let result = rt.execute(&script, &serde_json::json!({}));
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
