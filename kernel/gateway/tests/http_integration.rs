//! HTTP integration tests for the gateway.
//!
//! These tests build a minimal AgentRuntime and start the HTTP server on a
//! random port, then make real HTTP requests against it.
//!
//! NOTE: These tests are slow (~6s each) because they go through the full
//! AgentRuntime::build() pipeline. They are characterization tests that
//! document the gateway's external HTTP behavior.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// A test harness that owns a tokio runtime, AgentRuntime, and temp dir.
///
/// Fields are ordered so that drop happens in the correct sequence:
/// server handle → runtime → temp dir → tokio runtime.
struct GatewayTestHarness {
    server_handle: Option<gateway::runtime::HttpServerHandle>,
    runtime: Arc<gateway::runtime::AgentRuntime>,
    _tmp: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
}

impl GatewayTestHarness {
    fn new() -> Self {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _enter = rt.enter();
        let _tmp = tempfile::TempDir::new().expect("temp dir");
        let runtime = gateway::runtime::AgentRuntimeBuilder::new(
            config::AgentConfig::default(),
        )
        .with_runtime_dir(_tmp.path().to_path_buf())
        .with_predefined_dir("predefined")
        .with_bind_addr("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_runtime_handle(rt.handle().clone())
        .build()
        .expect("AgentRuntime::build() should succeed with default config");
        drop(_enter);
        Self {
            server_handle: None,
            runtime,
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

    fn base_url(&mut self) -> String {
        let addr = self.start_server().local_addr();
        format!("http://{addr}")
    }

    /// Make an HTTP GET request and return (status, body_text).
    fn http_get(&mut self, path: &str) -> (u16, String) {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let addr = self.start_server().local_addr();
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
    let (code, _body) = harness.http_get("/health/live");
    assert!(
        (200..300).contains(&code),
        "expected 2xx for /health/live, got {code}"
    );
}

#[test]
fn test_health_ready_endpoint() {
    let mut harness = GatewayTestHarness::new();
    let (code, _body) = harness.http_get("/health/ready");
    // /health/ready may return 503 if the runtime is not ready yet.
    assert!(
        code == 200 || code == 503,
        "expected 200 or 503 for /health/ready, got {code}"
    );
}

#[test]
fn test_agents_endpoint_returns_json() {
    let mut harness = GatewayTestHarness::new();
    let (code, body) = harness.http_get("/agents");
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
