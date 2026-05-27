// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Bidirectional JSON-RPC 2.0 bridge for subprocess plugins.
//!
//! Replaces the old one-shot [`SubprocessPluginClient`] with a long-running,
//! bidirectional connection. The plugin process stays alive for its entire
//! lifecycle, communicating over stdin/stdout with newline-delimited JSON-RPC.
//!
//! ## Protocol
//!
//! **Server → Plugin** (requests):
//! - `aman.on_load` — plugin init
//! - `aman.on_unload` — plugin shutdown
//! - `aman.handle_route` — HTTP request forwarding
//! - `aman.on_event` — EventBus event notification
//!
//! **Plugin → Server** (requests):
//! - `aman.register_routes` — register HTTP route specs
//! - `aman.subscribe_events` — subscribe to EventBus events
//! - `aman.get_agents` — query agent registry
//! - `aman.push_work_item` — push to agent WorkSystem
//! - `aman.emit_event` — publish event to EventBus
//! - `aman.register_workflow` — register workflow definition

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::oneshot;

use serde::Deserialize;

use kernel::plugin::JsonRpcMethodHandler;
use kernel::AmanResult;

use crate::SubprocessPluginConfig;

// ---------------------------------------------------------------------------
// Route spec
// ---------------------------------------------------------------------------

/// An HTTP route registered by a subprocess plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    pub method: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// SubprocessPluginBridge
// ---------------------------------------------------------------------------

/// Bidirectional JSON-RPC bridge to a long-running subprocess plugin.
pub struct SubprocessPluginBridge {
    plugin_name: String,
    /// The spawned child process.
    child: Mutex<Option<Child>>,
    /// Stdin writer for sending requests to the plugin.
    stdin: Mutex<Option<ChildStdin>>,
    /// Monotonic JSON-RPC request ID counter.
    next_id: AtomicU64,
    /// Pending server→plugin requests awaiting a response.
    pending: Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>,
    /// Routes registered by the plugin via `aman.register_routes`.
    registered_routes: RwLock<Vec<RouteSpec>>,
    /// Plugin→Server JSON-RPC method handler (provided by the host runtime).
    method_handler: Arc<dyn JsonRpcMethodHandler>,
    /// Whether the bridge has been shut down.
    shutdown: AtomicBool,
}

impl SubprocessPluginBridge {
    /// Spawn the plugin process and start the reader loop.
    pub fn spawn(
        plugin_name: &str,
        config: &SubprocessPluginConfig,
        plugin_dir: Option<&PathBuf>,
        method_handler: Arc<dyn JsonRpcMethodHandler>,
    ) -> AmanResult<Arc<Self>> {
        let mut command = Command::new(&config.command);
        command.args(&config.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());

        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        } else if let Some(dir) = plugin_dir {
            command.current_dir(dir);
        }

        let mut child = command.spawn().map_err(|e| kernel::Error::Unrecoverable {
            message: format!("failed to spawn subprocess plugin `{plugin_name}`: {e}"),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| kernel::Error::Unrecoverable {
            message: format!("subprocess plugin `{plugin_name}`: stdin not available"),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| kernel::Error::Unrecoverable {
            message: format!("subprocess plugin `{plugin_name}`: stdout not available"),
        })?;

        let bridge = Arc::new(Self {
            plugin_name: plugin_name.to_owned(),
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            registered_routes: RwLock::new(Vec::new()),
            method_handler,
            shutdown: AtomicBool::new(false),
        });

        // Spawn reader thread for plugin → server messages
        let bridge_clone = Arc::clone(&bridge);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if bridge_clone.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                match line {
                    Ok(text) if text.trim().is_empty() => continue,
                    Ok(text) => {
                        bridge_clone.handle_incoming(text.trim());
                    }
                    Err(_) => break, // pipe closed
                }
            }
            bridge_clone.shutdown.store(true, Ordering::Relaxed);
        });

        Ok(bridge)
    }

    // ------------------------------------------------------------------
    // Server → Plugin requests
    // ------------------------------------------------------------------

    /// Send a JSON-RPC request to the plugin and wait for a response.
    pub fn request(&self, method: &str, params: serde_json::Value) -> AmanResult<serde_json::Value> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(kernel::Error::Unrecoverable {
                message: format!("plugin `{}` bridge is shut down", self.plugin_name),
            });
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        // Write to stdin
        {
            let mut stdin_guard = self.stdin.lock().unwrap();
            if let Some(stdin) = stdin_guard.as_mut() {
                let line = format!("{payload}\n");
                stdin.write_all(line.as_bytes()).map_err(|e| kernel::Error::Unrecoverable {
                    message: format!("plugin `{}`: write failed: {e}", self.plugin_name),
                })?;
                stdin.flush().ok();
            }
        }

        // Wait for response (bridges sync/async via pollster)
        match pollster::block_on(rx) {
            Ok(response) => Ok(response),
            Err(_) => Err(kernel::Error::Unrecoverable {
                message: format!("plugin `{}`: no response for {method}", self.plugin_name),
            }),
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn notify(&self, method: &str, params: serde_json::Value) -> AmanResult<()> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let mut stdin_guard = self.stdin.lock().unwrap();
        if let Some(stdin) = stdin_guard.as_mut() {
            let line = format!("{payload}\n");
            stdin.write_all(line.as_bytes()).map_err(|e| kernel::Error::Unrecoverable {
                message: format!("plugin `{}`: notify write failed: {e}", self.plugin_name),
            })?;
            stdin.flush().ok();
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Lifecycle helpers
    // ------------------------------------------------------------------

    pub fn on_load(&self, version: &semver::Version) -> AmanResult<serde_json::Value> {
        self.request(
            "aman.on_load",
            serde_json::json!({
                "plugin_name": self.plugin_name,
                "version": version.to_string(),
            }),
        )
    }

    pub fn on_unload(&self) -> AmanResult<()> {
        let _ = self.request("aman.on_unload", serde_json::json!({ "plugin_name": self.plugin_name }));
        Ok(())
    }

    /// Forward an HTTP request to the plugin and return the JSON response.
    pub fn handle_route(
        &self,
        method: &str,
        path: &str,
        query: Option<&str>,
        headers: &HashMap<String, String>,
        body: Option<&str>,
    ) -> AmanResult<serde_json::Value> {
        self.request(
            "aman.handle_route",
            serde_json::json!({
                "method": method,
                "path": path,
                "query": query,
                "headers": headers,
                "body": body,
            }),
        )
    }

    /// Send an event notification to the plugin.
    pub fn on_event(&self, event_type: &str, payload: &serde_json::Value) -> AmanResult<()> {
        self.notify(
            "aman.on_event",
            serde_json::json!({
                "event_type": event_type,
                "payload": payload,
            }),
        )
    }

    // ------------------------------------------------------------------
    // Route accessors
    // ------------------------------------------------------------------

    #[must_use]
    pub fn registered_routes(&self) -> Vec<RouteSpec> {
        self.registered_routes.read().unwrap().clone()
    }

    #[must_use]
    pub fn has_routes(&self) -> bool {
        !self.registered_routes.read().unwrap().is_empty()
    }

    // ------------------------------------------------------------------
    // Shutdown
    // ------------------------------------------------------------------

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Wait for child process to exit
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    // ------------------------------------------------------------------
    // Incoming message handling (Plugin → Server)
    // ------------------------------------------------------------------

    fn handle_incoming(&self, line: &str) {
        let msg: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(plugin = %self.plugin_name, error = %e, "invalid json from subprocess plugin");
                return;
            }
        };

        // JSON-RPC 2.0: requests have "method" (with optional "id"); responses
        // have "id" but no "method". Check method first so plugin→server
        // requests with both "id" and "method" are not misclassified.
        if let Some(method) = msg.get("method").and_then(|v| v.as_str()) {
            let params = msg.get("params").cloned().unwrap_or(serde_json::Value::Null);
            let req_id = msg.get("id").and_then(|v| v.as_u64());
            self.handle_plugin_request(method, params, req_id);
        } else if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
            let mut pending = self.pending.lock().unwrap();
            if let Some(tx) = pending.remove(&id) {
                let result = msg.get("result").cloned().unwrap_or(serde_json::Value::Null);
                let _ = tx.send(result);
            }
        }
    }

    fn handle_plugin_request(&self, method: &str, params: serde_json::Value, req_id: Option<u64>) {
        match method {
            "aman.register_routes" => {
                let routes: Vec<RouteSpec> = match serde_json::from_value(params) {
                    Ok(v) => v,
                    Err(e) => {
                        self.send_error(req_id, -32602, &format!("invalid params: {e}"));
                        return;
                    }
                };
                {
                    let mut guard = self.registered_routes.write().unwrap();
                    // Merge new routes into existing set, deduplicating by (method, path).
                    for route in routes {
                        let key = (route.method.clone(), route.path.clone());
                        if !guard.iter().any(|r| r.method == key.0 && r.path == key.1) {
                            guard.push(route);
                        }
                    }
                }
                tracing::info!(
                    plugin = %self.plugin_name,
                    count = self.registered_routes.read().unwrap().len(),
                    "subprocess plugin registered routes"
                );
                self.send_result(req_id, serde_json::json!({"ok": true}));
            }
            _ => {
                // Delegate to the host runtime handler
                let handler = Arc::clone(&self.method_handler);
                let plugin_name = self.plugin_name.clone();
                let method_owned = method.to_owned();
                let req_id_copy = req_id;

                // We're in a synchronous context (blocking reader thread).
                // Use pollster to bridge to async.
                match pollster::block_on(handler.handle_method(&plugin_name, &method_owned, params)) {
                    Ok(result) => self.send_result(req_id_copy, result),
                    Err(e) => self.send_error(req_id_copy, -32000, &e.to_string()),
                }
            }
        }
    }

    fn send_result(&self, id: Option<u64>, result: serde_json::Value) {
        let Some(id) = id else { return }; // notification, no response needed
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        let mut guard = self.stdin.lock().unwrap();
        if let Some(stdin) = guard.as_mut() {
            let line = format!("{response}\n");
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        }
    }

    fn send_error(&self, id: Option<u64>, code: i64, message: &str) {
        let Some(id) = id else { return };
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        });
        let mut guard = self.stdin.lock().unwrap();
        if let Some(stdin) = guard.as_mut() {
            let line = format!("{response}\n");
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// RouteSpec serde
// ---------------------------------------------------------------------------

impl<'de> serde::Deserialize<'de> for RouteSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            method: String,
            path: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            method: raw.method.to_uppercase(),
            path: raw.path,
        })
    }
}

// ---------------------------------------------------------------------------
// Route collection helpers
// ---------------------------------------------------------------------------

/// Build an axum Router<()> from the routes registered by a subprocess plugin.
/// Each route forwards HTTP requests to the plugin via JSON-RPC `handle_route`.
///
/// Additionally, a catch-all route is added for each unique top-level path prefix
/// (e.g. `/team`) so that dynamically registered routes (e.g. new projects) are
/// forwarded to the plugin without requiring an axum Router rebuild.
pub fn build_subprocess_router(bridge: Arc<SubprocessPluginBridge>) -> axum::Router<()> {
    let routes = bridge.registered_routes();
    let mut router = axum::Router::new();

    for spec in &routes {
        let bridge = Arc::clone(&bridge);
        let method = spec.method.clone();
        let path = spec.path.clone();

        // axum 0.8 requires Handler to return a Future; wrap sync call in spawn_blocking
        let handler = {
            let method = method.clone();
            let path = path.clone();
            move |req: axum::http::Request<axum::body::Body>| {
                let bridge = Arc::clone(&bridge);
                let method = method.clone();
                let path = path.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        forward_to_plugin_sync(&bridge, &method, &path, req)
                    })
                    .await
                    .unwrap_or_else(|_| {
                        let mut resp = axum::response::Response::new(
                            axum::body::Body::from("plugin handler panicked"),
                        );
                        *resp.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        resp
                    })
                }
            }
        };

        // Register the route with the appropriate method
        router = match method.as_str() {
            "GET" => router.route(&path, axum::routing::get(handler)),
            "POST" => router.route(&path, axum::routing::post(handler)),
            "PUT" => router.route(&path, axum::routing::put(handler)),
            "DELETE" => router.route(&path, axum::routing::delete(handler)),
            _ => {
                tracing::warn!(method = %method, path = %path, "unsupported HTTP method for plugin route");
                router
            }
        };
    }

    // Add catch-all routes for each unique top-level path prefix.
    // This ensures dynamically registered routes (e.g. project pages created
    // after plugin init) are forwarded to the plugin's handle_route handler.
    {
        let mut prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();
        for spec in &routes {
            // Extract "/first-segment" from paths like "/team/setup" or "/team/projects/foo"
            if let Some(rest) = spec.path.strip_prefix('/')
                && let Some(first) = rest.split('/').next()
                && !first.is_empty()
                && !first.starts_with('{')
            {
                prefixes.insert(format!("/{first}"));
            }
        }

        for prefix in prefixes {
            let bridge = Arc::clone(&bridge);
            let catch_all_path = format!("{prefix}/{{*rest}}");
            let handler = move |req: axum::http::Request<axum::body::Body>| {
                let bridge = Arc::clone(&bridge);
                async move {
                    let method = req.method().to_string();
                    let path = req.uri().path().to_string();
                    tokio::task::spawn_blocking(move || {
                        forward_to_plugin_sync(&bridge, &method, &path, req)
                    })
                    .await
                    .unwrap_or_else(|_| {
                        let mut resp = axum::response::Response::new(
                            axum::body::Body::from("plugin handler panicked"),
                        );
                        *resp.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        resp
                    })
                }
            };
            router = router.route(&catch_all_path, axum::routing::any(handler));
        }
    }

    router
}

/// Forward an HTTP request to the subprocess plugin synchronously.
fn forward_to_plugin_sync(
    bridge: &SubprocessPluginBridge,
    method: &str,
    path: &str,
    req: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    use axum::body::Body;

    let uri = req.uri().clone();
    let query = uri.query();
    let headers: HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let (_parts, body) = req.into_parts();
    // Read body bytes via blocking
    let body_bytes = pollster::block_on(async {
        axum::body::to_bytes(body, usize::MAX).await
    });
    let body_str = match body_bytes {
        Ok(ref b) if b.is_empty() => String::new(),
        Ok(b) => String::from_utf8_lossy(&b).to_string(),
        Err(_) => {
            let mut resp = axum::response::Response::new(Body::from("failed to read request body"));
            *resp.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
            return resp;
        }
    };

    let body_param = if body_str.is_empty() { None } else { Some(body_str.as_str()) };

    match bridge.handle_route(method, path, query, &headers, body_param) {
        Ok(response) => {
            let status = response
                .get("status")
                .and_then(|s| s.as_u64())
                .unwrap_or(200) as u16;
            let resp_headers = response.get("headers").and_then(|h| h.as_object());
            let body = response
                .get("body")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();

            let mut builder = axum::response::Response::builder().status(status);
            if let Some(headers) = resp_headers {
                for (k, v) in headers {
                    if let Some(val) = v.as_str() {
                        builder = builder.header(k.as_str(), val);
                    }
                }
            }
            builder.body(Body::from(body)).unwrap()
        }
        Err(e) => {
            let mut resp = axum::response::Response::new(Body::from(format!("plugin error: {e}")));
            *resp.status_mut() = axum::http::StatusCode::BAD_GATEWAY;
            resp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_spec_deserialization() {
        let json = serde_json::json!([
            {"method": "get", "path": "/team/{team_id}/tasks"},
            {"method": "POST", "path": "/team/{team_id}/tasks/create"},
        ]);
        let routes: Vec<RouteSpec> = serde_json::from_value(json).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[1].method, "POST");
    }
}
