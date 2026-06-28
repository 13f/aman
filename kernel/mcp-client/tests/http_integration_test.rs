// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Integration test: connect to an HTTP MCP server via streamable-http.

use std::collections::BTreeMap;

use mcp_client::client::McpClientHandle;
use serde_json::json;

/// Requires a local FastMCP server running on http://127.0.0.1:9020/mcp.
/// Start it with:
///   python3 /tmp/test_mcp_http_server.py
#[tokio::test]
#[ignore = "requires local HTTP MCP server on http://127.0.0.1:9020/mcp"]
#[allow(clippy::print_stderr)] // diagnostic output for manual integration test runs
async fn connect_to_http_mcp_server() {
    let url = "http://127.0.0.1:9020/mcp";

    let handle = McpClientHandle::connect_http(
        "test-http-server",
        url,
        &BTreeMap::new(),
    )
    .await
    .expect("should connect to HTTP MCP server");

    assert!(
        !handle.discovered_tools.is_empty(),
        "should have discovered tools"
    );
    eprintln!(
        "HTTP server discovered {} tools: {:?}",
        handle.discovered_tools.len(),
        handle
            .discovered_tools
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );

    // Test ping tool
    let result = handle
        .call_tool("ping", json!({}))
        .await
        .expect("ping should succeed");
    eprintln!("Ping result: {result:?}");
    assert!(
        result
            .get("content")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("pong")),
        "ping should return pong"
    );

    // Test add tool
    let result = handle
        .call_tool("add", json!({"a": 3, "b": 4}))
        .await
        .expect("add should succeed");
    eprintln!("Add result: {result:?}");
    assert!(
        result
            .get("content")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("7")),
        "3 + 4 should equal 7"
    );

    // Test greet tool
    let result = handle
        .call_tool("greet", json!({"name": "aman"}))
        .await
        .expect("greet should succeed");
    eprintln!("Greet result: {result:?}");
    assert!(
        result
            .get("content")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("aman")),
        "greet should mention aman"
    );

    handle.cancel().await;
}
