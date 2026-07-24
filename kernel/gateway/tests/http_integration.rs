//! HTTP integration tests for the gateway.
//!
//! These tests build a minimal AgentRuntime and start the HTTP server on a
//! random port, then make real HTTP requests against it.
//!
//! NOTE: These tests are slow (~6s each) because they go through the full
//! AgentRuntime::build() pipeline. They are characterization tests that
//! document the gateway's external HTTP behavior.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Serializes the env-var + plugin-discovery tests. They point the process-wide
// `AMAN_DATA_DIR` at a per-test temp directory and build a real AgentRuntime
// that reads it — running those steps concurrently would have threads race on
// the shared env var and observe each other's plugins. The lock makes each test
// set up, build, assert and tear down atomically.
static LOCAL_PLUGIN_TESTS_LOCK: Mutex<()> = Mutex::new(());

/// A test harness that owns a tokio runtime, AgentRuntime, and temp dir.
///
/// Fields are ordered so that drop happens in the correct sequence:
/// server handle → runtime → agenverse → temp dir → tokio runtime.
struct GatewayTestHarness {
    server_handle: Option<gateway::runtime::HttpServerHandle>,
    runtime: Arc<gateway::runtime::AgentRuntime>,
    _agenverse: Arc<gateway::runtime::Agenverse>,
    _tmp: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
}

impl GatewayTestHarness {
    fn new() -> Self {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _enter = rt.enter();
        let _tmp = tempfile::TempDir::new().expect("temp dir");
        let agenverse = Arc::new(gateway::runtime::Agenverse::new(Duration::from_millis(0), Duration::from_secs(720)));
        let runtime = gateway::runtime::AgentRuntimeBuilder::new(
            config::AgentConfig::default(),
        )
        .with_runtime_dir(_tmp.path().to_path_buf())
        .with_predefined_dir("predefined")
        .with_bind_addr("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_runtime_handle(rt.handle().clone())
        .build(Arc::clone(&agenverse))
        .expect("AgentRuntime::build() should succeed with default config");
        agenverse.set_runtime(Arc::clone(&runtime));
        drop(_enter);
        Self {
            server_handle: None,
            runtime,
            _agenverse: agenverse,
            _tmp,
            rt,
        }
    }

    fn start_server(&mut self) -> &gateway::runtime::HttpServerHandle {
        if self.server_handle.is_none() {
            let config = gateway::runtime::HttpServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
            };
            let handle = self.rt.block_on(async {
                gateway::runtime::serve(Arc::clone(&self.runtime), config)
                    .await
                    .expect("serve should start")
            });
            self.server_handle = Some(handle);
        }
        self.server_handle.as_ref().unwrap()
    }
}

/// Isolate a test's data dir from the real ~/.aman by pointing
/// `AMAN_DATA_DIR` at a temp directory. Returns the temp dir (keep it alive
/// for the test's duration) plus the local .aman path so callers can
/// pre-stage plugin manifests and/or approvals before building.
fn isolate_aman_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let local_aman = tmp.path().join(".aman");
    std::fs::create_dir_all(&local_aman).expect("create local .aman");
    // SAFETY: tests run single-threaded in our suite and this test owns the
    // env var for its whole duration; no concurrent reader observes torn state.
    unsafe { std::env::set_var("AMAN_DATA_DIR", &local_aman); }
    (tmp, local_aman)
}

/// Make an HTTP GET request against the harness's gateway and return
/// (status_code, raw_response_text).
fn http_get(harness: &mut GatewayTestHarness, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = harness.start_server().local_addr();
    let mut stream = TcpStream::connect(addr).expect("connect to gateway");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");

    let status_line = response.lines().next().expect("status line");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status code");
    (code, response)
}

impl Drop for GatewayTestHarness {
    fn drop(&mut self) {
        // Shutdown server first, then drop runtime, then temp dir, then runtime.
        if let Some(handle) = self.server_handle.take() {
            handle.shutdown();
        }
    }
}

#[test]
fn test_health_live_endpoint() {
    let mut harness = GatewayTestHarness::new();
    let (code, _body) = http_get(&mut harness, "/health/live");
    assert!(
        (200..300).contains(&code),
        "expected 2xx for /health/live, got {code}"
    );
}

#[test]
fn test_health_ready_endpoint() {
    let mut harness = GatewayTestHarness::new();
    let (code, _body) = http_get(&mut harness, "/health/ready");
    // /health/ready may return 503 if the runtime is not ready yet.
    assert!(
        code == 200 || code == 503,
        "expected 200 or 503 for /health/ready, got {code}"
    );
}

#[test]
fn test_agents_endpoint_returns_json() {
    let mut harness = GatewayTestHarness::new();
    let (code, body) = http_get(&mut harness, "/agents");
    assert!(
        (200..300).contains(&code),
        "expected 2xx for GET /agents, got {code}"
    );
    // Verify the body is valid JSON.
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(
        body.split("\r\n\r\n").nth(1).unwrap_or(""),
    );
    assert!(
        parsed.is_ok(),
        "response body should be valid JSON, got: {body}"
    );
}

// ---------------------------------------------------------------------------
// Local-subprocess-plugin discovery → approval → load regression test
//
// Verifies that a plugin dropped into `$AMAN_DATA_DIR/plugins/<name>/plugin.yaml`
// is (a) discovered, (b) passes the capability-approval gate when a matching
// approval file exists, and (c) reaches the Running state after startup.
// This is the path team/startup/info-hub rely on, so if it regresses the
// plugins silently disappear from the runtime.
// ---------------------------------------------------------------------------

/// Minimal "echo" subprocess plugin: reads JSON-RPC requests from stdin and
/// replies `{jsonrpc:2.0, id, result:{status:"ok"}}` — enough to pass the
/// `aman.on_load` handshake and reach Running.
fn write_test_echo_plugin(plugin_dir: &std::path::Path) {
    std::fs::create_dir_all(plugin_dir).expect("create plugin dir");
    #[cfg(target_os = "windows")]
    let py = "python";
    #[cfg(not(target_os = "windows"))]
    let py = "python3";
    std::fs::write(plugin_dir.join("plugin.yaml"), format!(
        r#"name: "test-echo"
version: "0.1.0"
description: "Test echo plugin for discovery+load verification"
isolation: subprocess
runtime: {py}
min_version: ">=3.10"
entrypoint: "echo_bridge.py"
lifecycle:
  auto_start: true
security:
  requested_capabilities:
    publish_events: false
    subscribe_events: false
    read_paths: []
    write_paths: []
"#,
    )).expect("write plugin.yaml");
    std::fs::write(plugin_dir.join("echo_bridge.py"),
        r#"#!/usr/bin/env python3
import json, sys
def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            continue
        rid = req.get("id")
        if rid is None:
            continue
        print(json.dumps({"jsonrpc":"2.0","id":rid,"result":{"status":"ok"}}), flush=True)
if __name__ == "__main__":
    main()
"#,
    ).expect("write echo_bridge.py");
}

/// Persist a capability approval for `test-echo` with a valid BLAKE3 signature.
/// Mirrors the production approval flow: the runtime computes the signature
/// from `~/.aman/.security-key`, so the file must be produced by
/// `ApprovalCache::save` — a hand-written signature fails the tamper check and
/// the plugin is deferred (see test_subprocess_plugin_not_loaded_without_approval).
///
/// The approved capability set is read straight from the plugin's own manifest
/// so it always matches what `check_approval` sees (`approved.contains(requested)`
/// holds). Approving anything short of the manifest's request would itself be a
/// mismatched-capability regression.
fn write_test_approval(aman_root: &std::path::Path) {
    let manifest_path = aman_root
        .join("plugins")
        .join("test-echo")
        .join("plugin.yaml");
    let manifest = plugin::PluginManifest::from_file(&manifest_path)
        .expect("parse test-echo manifest");
    let requested = match manifest.security {
        Some(sec) => sec.requested_capabilities,
        // No security manifest → empty request; approve a permissive default
        // so the `contains` check still passes.
        None => kernel::security::CapabilitySet::default(),
    };
    let mut approved = kernel::security::ApprovedCapabilities {
        plugin_version: manifest.version.to_string(),
        capabilities: requested,
        approved_at_ms: 1_000_000_000_000,
        approved_by: "test".to_owned(),
        signature: String::new(),
    };
    let cache = kernel::security::ApprovalCache::new(aman_root.to_path_buf())
        .expect("build approval cache");
    cache
        .save("test-echo", &mut approved)
        .expect("save approval with valid signature");
}

#[test]
fn test_subprocess_plugin_discovered_and_loaded_when_approved() {
    let _guard = LOCAL_PLUGIN_TESTS_LOCK.lock().unwrap();
    let (_tmp, local_aman) = isolate_aman_dir();
    write_test_echo_plugin(&local_aman.join("plugins").join("test-echo"));
    write_test_approval(&local_aman);

    let harness = GatewayTestHarness::new();

    // The plugin should be discoverable + loaded into the Running state.
    let state = harness.rt.block_on(async {
        let loader = harness.runtime.plugin_loader().await;
        loader.state_of("test-echo")
    });
    assert!(
        matches!(state, Some(s) if matches!(s, plugin::PluginLifecycleState::Running)),
        "approved subprocess plugin should be discovered and reach Running, got {state:?}"
    );
}

#[test]
fn test_subprocess_plugin_not_loaded_without_approval() {
    let _guard = LOCAL_PLUGIN_TESTS_LOCK.lock().unwrap();
    let (_tmp, local_aman) = isolate_aman_dir();
    write_test_echo_plugin(&local_aman.join("plugins").join("test-echo"));
    // No approval file → the plugin must be deferred, NOT loaded.

    let harness = GatewayTestHarness::new();

    let state = harness.rt.block_on(async {
        let loader = harness.runtime.plugin_loader().await;
        loader.state_of("test-echo")
    });
    assert_eq!(
        state,
        None,
        "unapproved subprocess plugin should not be loaded (deferred for approval)"
    );
}
