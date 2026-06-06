// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Integration test: connect to a real MCP server (everything) via stdio.

use std::collections::BTreeMap;

use mcp_client::client::McpClientHandle;
use mcp_client::config::{McpServerConfig, McpServersFile};
use serde_json::json;

#[tokio::test]
#[ignore = "requires network access to download MCP server via npx"]
async fn connect_to_everything_stdio() {
    let mut env = BTreeMap::new();
    // Ensure PATH is available for npx
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }

    let handle = McpClientHandle::connect_stdio(
        "everything",
        "npx",
        &[
            "-y".to_string(),
            "@modelcontextprotocol/server-everything".to_string(),
        ],
        &env,
    )
    .await
    .expect("should connect to everything server");

    assert!(!handle.discovered_tools.is_empty(), "should have tools");
    eprintln!(
        "Discovered {} tools: {:?}",
        handle.discovered_tools.len(),
        handle
            .discovered_tools
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );

    // Call the echo tool
    let result = handle
        .call_tool("echo", json!({"message": "hello from aman"}))
        .await
        .expect("echo tool should succeed");
    eprintln!("Echo result: {result}");

    assert!(
        result
            .get("content")
            .and_then(|v| v.as_str())
            .map_or(false, |s| s.contains("hello from aman")),
        "echo should return our message"
    );

    handle.cancel().await;
}

#[test]
fn config_auto_detection() {
    // stdio: command set, url empty → auto detects stdio
    let cfg = McpServerConfig {
        name: "test".into(),
        transport: "auto".into(),
        command: Some("npx".into()),
        args: vec![],
        url: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        auto_connect: true,
    };
    assert_eq!(cfg.resolve_transport(), Some("stdio"));

    // streamable-http: url set, command empty → auto detects streamable-http
    let cfg = McpServerConfig {
        name: "test".into(),
        transport: "auto".into(),
        command: None,
        args: vec![],
        url: Some("http://localhost:8000/mcp".into()),
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        auto_connect: true,
    };
    assert_eq!(cfg.resolve_transport(), Some("streamable-http"));

    // both set → stdio wins
    let cfg = McpServerConfig {
        name: "test".into(),
        transport: "auto".into(),
        command: Some("npx".into()),
        args: vec![],
        url: Some("http://localhost:8000/mcp".into()),
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        auto_connect: true,
    };
    assert_eq!(cfg.resolve_transport(), Some("stdio"));

    // neither set → None
    let cfg = McpServerConfig {
        name: "test".into(),
        transport: "auto".into(),
        command: None,
        args: vec![],
        url: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        auto_connect: true,
    };
    assert_eq!(cfg.resolve_transport(), None);

    // explicit transport overrides auto detection
    let cfg = McpServerConfig {
        name: "test".into(),
        transport: "streamable-http".into(),
        command: Some("npx".into()),
        args: vec![],
        url: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        auto_connect: true,
    };
    assert_eq!(cfg.resolve_transport(), Some("streamable-http"));
}

#[test]
fn config_merge_per_agent_overrides_global() {
    let global = vec![
        McpServerConfig {
            name: "shared".into(),
            transport: "auto".into(),
            command: Some("npx".into()),
            args: vec!["-y".into(), "server-shared".into()],
            url: None,
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            auto_connect: true,
        },
    ];

    let agent = vec![
        McpServerConfig {
            name: "shared".into(),
            transport: "auto".into(),
            command: Some("npx".into()),
            args: vec!["-y".into(), "server-agent-specific".into()],
            url: None,
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            auto_connect: false,
        },
        McpServerConfig {
            name: "agent-only".into(),
            transport: "auto".into(),
            command: Some("uvx".into()),
            args: vec!["server-only".into()],
            url: None,
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            auto_connect: true,
        },
    ];

    let merged = McpServersFile::merge(&global, &agent);
    assert_eq!(merged.len(), 2, "should have 2 unique servers after merge");

    let shared = merged.iter().find(|s| s.name == "shared").unwrap();
    assert_eq!(shared.args, vec!["-y", "server-agent-specific"], "per-agent should override global args");
    assert!(!shared.auto_connect, "per-agent should override auto_connect");

    let agent_only = merged.iter().find(|s| s.name == "agent-only").unwrap();
    assert_eq!(agent_only.command.as_deref(), Some("uvx"));
}
