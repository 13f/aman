// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde_json::Value;
use std::time::Duration;

/// HTTP client for communicating with the aman Gateway daemon.
#[derive(Debug, Clone)]
pub struct GatewayClient {
    pub base_url: String,
    client: reqwest::Client,
}

impl GatewayClient {
    pub fn new(base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .expect("reqwest Client::builder");
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client,
        }
    }

    pub fn new_with_token(base_url: &str, api_token: &str) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_token}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .expect("reqwest Client::builder");
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    // ── Health ──────────────────────────────────────────────────────────

    pub async fn health(&self) -> Result<(), String> {
        let resp = self
            .client
            .get(self.url("/health/live"))
            .send()
            .await
            .map_err(|e| format!("Gateway connection failed: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("Gateway health check failed: {}", resp.status()))
        }
    }

    // ── IM Channels ──────────────────────────────────────────────────────

    pub async fn im_channel_reload(&self, platform: &str, instance: &str) -> Result<(), String> {
        let path = format!("/im-channel/{}/{}/reload", platform, instance);
        let resp = self
            .client
            .post(self.url(&path))
            .send()
            .await
            .map_err(|e| format!("im_channel_reload: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("Reload failed ({status}): {body}"))
        }
    }

    // ── Runtime ─────────────────────────────────────────────────────────

    pub async fn runtime_status(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/runtime/status"))
            .send()
            .await
            .map_err(|e| format!("runtime_status: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("runtime_status decode: {e}"))
    }

    pub async fn runtime_config(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/runtime/config"))
            .send()
            .await
            .map_err(|e| format!("runtime_config: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("runtime_config decode: {e}"))
    }

    // ── Debug Metrics ───────────────────────────────────────────────────

    pub async fn debug_metrics(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/debug/metrics"))
            .send()
            .await
            .map_err(|e| format!("debug_metrics: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("debug_metrics decode: {e}"))
    }

    // ── Skills ──────────────────────────────────────────────────────────

    pub async fn list_skills(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/skills"))
            .send()
            .await
            .map_err(|e| format!("list_skills: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("list_skills decode: {e}"))
    }

    pub async fn list_llm_skills(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/llm-skills"))
            .send()
            .await
            .map_err(|e| format!("list_llm_skills: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("list_llm_skills decode: {e}"))
    }

    pub async fn reload_skills(&self) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url("/skills/reload"))
            .send()
            .await
            .map_err(|e| format!("reload_skills: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("reload_skills", resp.status()).await)
        }
    }

    pub async fn reload_agent(&self, agent_id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/agent/{agent_id}/reload")))
            .send()
            .await
            .map_err(|e| format!("reload_agent: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("reload_agent", resp.status()).await)
        }
    }

    pub async fn enable_skill(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/skill/{name}/enable")))
            .send()
            .await
            .map_err(|e| format!("enable_skill: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("enable_skill", resp.status()).await)
        }
    }

    pub async fn disable_skill(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/skill/{name}/disable")))
            .send()
            .await
            .map_err(|e| format!("disable_skill: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("disable_skill", resp.status()).await)
        }
    }

    pub async fn search_skills(&self, query: &str, limit: usize) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/skills/search"))
            .query(&[("q", query), ("limit", &limit.to_string())])
            .send()
            .await
            .map_err(|e| format!("search_skills: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("search_skills decode: {e}"))
    }

    pub async fn read_skill_content(&self, name: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/skill/{name}/content")))
            .send()
            .await
            .map_err(|e| format!("read_skill_content: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("read_skill_content decode: {e}"))
        } else {
            Err(status_error("read_skill_content", resp.status()).await)
        }
    }

    // ── Events ──────────────────────────────────────────────────────────

    pub async fn recent_events(&self, limit: usize) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/events/recent?limit={limit}")))
            .send()
            .await
            .map_err(|e| format!("recent_events: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("recent_events decode: {e}"))
    }

    pub async fn event_trace(&self, trace_id: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/events/trace/{trace_id}")))
            .send()
            .await
            .map_err(|e| format!("event_trace: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("event_trace decode: {e}"))
        } else {
            Err(status_error("event_trace", resp.status()).await)
        }
    }

    pub async fn inject_event(&self, source: &str, event_type: &str, payload: Value) -> Result<String, String> {
        let body = serde_json::json!({
            "source": source,
            "event_type": event_type,
            "payload": payload,
        });
        let resp = self
            .client
            .post(self.url("/inject-event"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("inject_event: {e}"))?;
        if resp.status().is_success() {
            let v: Value = resp.json().await.map_err(|e| format!("inject_event decode: {e}"))?;
            Ok(v["id"].as_str().unwrap_or("").to_owned())
        } else {
            Err(status_error("inject_event", resp.status()).await)
        }
    }

    // ── Workflows ───────────────────────────────────────────────────────

    pub async fn workflow_instances(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/workflow-instances"))
            .send()
            .await
            .map_err(|e| format!("workflow_instances: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("workflow_instances decode: {e}"))
    }

    pub async fn workflow_def(&self, name: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/workflow/{name}")))
            .send()
            .await
            .map_err(|e| format!("workflow_def: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("workflow_def decode: {e}"))
        } else {
            Err(status_error("workflow_def", resp.status()).await)
        }
    }

    pub async fn retry_workflow(&self, id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/workflow-instance/{id}/retry")))
            .send()
            .await
            .map_err(|e| format!("retry_workflow: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("retry_workflow", resp.status()).await)
        }
    }

    pub async fn cancel_workflow(&self, id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/workflow-instance/{id}/cancel")))
            .send()
            .await
            .map_err(|e| format!("cancel_workflow: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("cancel_workflow", resp.status()).await)
        }
    }

    // ── Soul ───────────────────────────────────────────────────────────

    pub async fn soul_info(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/soul/info"))
            .send()
            .await
            .map_err(|e| format!("soul_info: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("soul_info decode: {e}"))
        } else if resp.status().as_u16() == 404 {
            Ok(serde_json::json!({}))
        } else {
            Err(status_error("soul_info", resp.status()).await)
        }
    }

    pub async fn soul_raw(&self) -> Result<String, String> {
        let resp = self
            .client
            .get(self.url("/soul/raw"))
            .send()
            .await
            .map_err(|e| format!("soul_raw: {e}"))?;
        if resp.status().is_success() {
            let v: Value = resp.json().await.map_err(|e| format!("soul_raw decode: {e}"))?;
            Ok(v["raw"].as_str().unwrap_or("").to_owned())
        } else if resp.status().as_u16() == 404 {
            Err("No SOUL configured".to_owned())
        } else {
            Err(status_error("soul_raw", resp.status()).await)
        }
    }

    pub async fn soul_system_prompt(&self) -> Result<String, String> {
        let resp = self
            .client
            .get(self.url("/soul/system-prompt"))
            .send()
            .await
            .map_err(|e| format!("soul_system_prompt: {e}"))?;
        if resp.status().is_success() {
            let v: Value = resp.json().await.map_err(|e| format!("soul_system_prompt decode: {e}"))?;
            Ok(v["system_prompt"].as_str().unwrap_or("").to_owned())
        } else if resp.status().as_u16() == 404 {
            Err("No SOUL configured".to_owned())
        } else {
            Err(status_error("soul_system_prompt", resp.status()).await)
        }
    }

    pub async fn update_soul(&self, content: &str) -> Result<(), String> {
        let body = serde_json::json!({ "content": content });
        let resp = self
            .client
            .post(self.url("/soul/update"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("update_soul: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("update_soul", resp.status()).await)
        }
    }

    // ── Plugins ─────────────────────────────────────────────────────────

    pub async fn list_plugins(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/plugins"))
            .send()
            .await
            .map_err(|e| format!("list_plugins: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("list_plugins decode: {e}"))
    }

    pub async fn enable_plugin(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/plugin/{name}/enable")))
            .send()
            .await
            .map_err(|e| format!("enable_plugin: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("enable_plugin", resp.status()).await)
        }
    }

    pub async fn disable_plugin(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/plugin/{name}/disable")))
            .send()
            .await
            .map_err(|e| format!("disable_plugin: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("disable_plugin", resp.status()).await)
        }
    }

    // ── Plugin UI pages ──────────────────────────────────────────────────

    pub async fn plugin_pages(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/ui/pages"))
            .send()
            .await
            .map_err(|e| format!("plugin_pages: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("plugin_pages decode: {e}"))
    }

    // ── Capabilities ────────────────────────────────────────────────────

    pub async fn capabilities(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/capabilities"))
            .send()
            .await
            .map_err(|e| format!("capabilities: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("capabilities decode: {e}"))
    }

    // ── DLQ ─────────────────────────────────────────────────────────────

    pub async fn list_dlq(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/dlq"))
            .send()
            .await
            .map_err(|e| format!("list_dlq: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("list_dlq decode: {e}"))
    }

    pub async fn retry_dlq(&self, id: &str) -> Result<(), String> {
        let body = serde_json::json!({ "reason": "manual retry from dashboard" });
        let resp = self
            .client
            .post(self.url(&format!("/dlq/{id}/retry")))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("retry_dlq: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("retry_dlq", resp.status()).await)
        }
    }

    pub async fn discard_dlq(&self, id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/dlq/{id}/discard")))
            .send()
            .await
            .map_err(|e| format!("discard_dlq: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("discard_dlq", resp.status()).await)
        }
    }

    // ── Notifications ─────────────────────────────────────────────────

    pub async fn notifications(&self, active_only: bool, severity: Option<&str>, limit: usize) -> Result<Value, String> {
        let mut path = format!("/notifications?active_only={active_only}&limit={limit}");
        if let Some(sev) = severity {
            path.push_str(&format!("&severity={sev}"));
        }
        let resp = self
            .client
            .get(self.url(&path))
            .send()
            .await
            .map_err(|e| format!("notifications: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("notifications decode: {e}"))
    }

    pub async fn notifications_unread_count(&self) -> Result<i64, String> {
        let resp = self
            .client
            .get(self.url("/notifications/unread-count"))
            .send()
            .await
            .map_err(|e| format!("notifications_unread_count: {e}"))?;
        let v: Value = resp.json().await.map_err(|e| format!("notifications_unread_count decode: {e}"))?;
        Ok(v["count"].as_i64().unwrap_or(0))
    }

    pub async fn notification_dismiss(&self, id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/notifications/{id}/dismiss")))
            .send()
            .await
            .map_err(|e| format!("notification_dismiss: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("notification_dismiss", resp.status()).await)
        }
    }

    pub async fn notification_ack(&self, id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/notifications/{id}/ack")))
            .send()
            .await
            .map_err(|e| format!("notification_ack: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("notification_ack", resp.status()).await)
        }
    }

    pub async fn notification_dismiss_all(&self) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url("/notifications/dismiss-all"))
            .send()
            .await
            .map_err(|e| format!("notification_dismiss_all: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("notification_dismiss_all", resp.status()).await)
        }
    }

    // ── Chat ────────────────────────────────────────────────────────────

    pub async fn chat_sessions(&self, agent_id: Option<&str>) -> Result<Value, String> {
        let url = match agent_id {
            Some(aid) => format!("{}?agent_id={aid}", self.url("/chat/sessions")),
            None => self.url("/chat/sessions"),
        };
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("chat_sessions: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("chat_sessions decode: {e}"))
    }

    pub async fn chat_session_create(&self, agent_key: Option<&str>, session_type: Option<&str>) -> Result<String, String> {
        let body = serde_json::json!({
            "agent_id": agent_key.unwrap_or("aman"),
            "session_type": session_type.unwrap_or("persistent"),
        });
        let resp = self
            .client
            .post(self.url("/chat/session/create"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat_session_create: {e}"))?;
        if resp.status().is_success() {
            let v: Value = resp.json().await.map_err(|e| format!("chat_session_create decode: {e}"))?;
            Ok(v["id"].as_str().unwrap_or("").to_owned())
        } else {
            Err(status_error("chat_session_create", resp.status()).await)
        }
    }

    pub async fn chat_session_create_branch(
        &self,
        parent_session_id: &str,
        branch_message_id: &str,
        agent_key: Option<&str>,
        session_type: Option<&str>,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "agent_id": agent_key.unwrap_or("aman"),
            "session_type": session_type.unwrap_or("branch"),
            "parent_session_id": parent_session_id,
            "branch_message_id": branch_message_id,
        });
        let resp = self
            .client
            .post(self.url("/chat/session/create"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat_session_create_branch: {e}"))?;
        if resp.status().is_success() {
            let v: Value = resp.json().await.map_err(|e| format!("chat_session_create_branch decode: {e}"))?;
            Ok(v["id"].as_str().unwrap_or("").to_owned())
        } else {
            Err(status_error("chat_session_create_branch", resp.status()).await)
        }
    }

    pub async fn chat_session_state(&self, session_id: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/chat/session/{session_id}/state")))
            .send()
            .await
            .map_err(|e| format!("chat_session_state: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("chat_session_state decode: {e}"))
        } else if resp.status().as_u16() == 404 {
            Err(format!("Session not found: {session_id}"))
        } else {
            Err(status_error("chat_session_state", resp.status()).await)
        }
    }

    pub async fn chat_session_history(&self, session_id: &str, _limit: Option<usize>) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/chat/session/{session_id}/history")))
            .send()
            .await
            .map_err(|e| format!("chat_session_history: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("chat_session_history decode: {e}"))
        } else {
            Err(status_error("chat_session_history", resp.status()).await)
        }
    }

    pub async fn chat_send_message(
        &self,
        session_id: &str,
        text: &str,
        expected_version: Option<u64>,
    ) -> Result<String, String> {
        let mut body = serde_json::json!({
            "text": text,
        });
        if let Some(ver) = expected_version {
            body["expected_version"] = serde_json::json!(ver);
        }
        let resp = self
            .client
            .post(self.url(&format!("/chat/session/{session_id}/send")))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat_send_message: {e}"))?;
        if resp.status().is_success() {
            let v: Value = resp.json().await.map_err(|e| format!("chat_send_message decode: {e}"))?;
            Ok(v["event_id"].as_str().unwrap_or("").to_owned())
        } else {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            Err(format!("chat_send_message ({}): {}", status, body_text))
        }
    }

    pub async fn chat_close_session(&self, session_id: &str) -> Result<(), String> {
        let body = serde_json::json!({ "reason": null });
        let resp = self
            .client
            .post(self.url(&format!("/chat/session/{session_id}/close")))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat_close_session: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("chat_close_session", resp.status()).await)
        }
    }

    pub async fn chat_delete_session(&self, session_id: &str) -> Result<(), String> {
        let resp = self
            .client
            .delete(self.url(&format!("/chat/session/{session_id}")))
            .send()
            .await
            .map_err(|e| format!("chat_delete_session: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else if resp.status().as_u16() == 404 {
            Err(format!("Session not found: {session_id}"))
        } else {
            Err(status_error("chat_delete_session", resp.status()).await)
        }
    }

    pub async fn chat_stop_generation(&self, session_id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/chat/session/{session_id}/stop")))
            .send()
            .await
            .map_err(|e| format!("chat_stop_generation: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("chat_stop_generation", resp.status()).await)
        }
    }

    pub async fn chat_retry(&self, session_id: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(self.url(&format!("/chat/session/{session_id}/retry")))
            .send()
            .await
            .map_err(|e| format!("chat_retry: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("chat_retry", resp.status()).await)
        }
    }

    pub async fn chat_edit_message(
        &self,
        session_id: &str,
        message_event_id: &str,
        new_text: &str,
        expected_version: Option<u64>,
    ) -> Result<(), String> {
        let mut body = serde_json::json!({
            "message_event_id": message_event_id,
            "new_text": new_text,
        });
        if let Some(ver) = expected_version {
            body["expected_version"] = serde_json::json!(ver);
        }
        let resp = self
            .client
            .post(self.url(&format!("/chat/session/{session_id}/edit")))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat_edit_message: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("chat_edit_message", resp.status()).await)
        }
    }

    // ── Agent Management ──────────────────────────────────────────────

    pub async fn list_agents(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/agents"))
            .send()
            .await
            .map_err(|e| format!("list_agents: {e}"))?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("list_agents decode: {e}"))
    }

    pub async fn get_agent(&self, agent_id: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url(&format!("/agent/{agent_id}")))
            .send()
            .await
            .map_err(|e| format!("get_agent: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("get_agent decode: {e}"))
        } else {
            Err(status_error("get_agent", resp.status()).await)
        }
    }

    pub async fn set_agent_status(&self, agent_id: &str, status: &str) -> Result<(), String> {
        let body = serde_json::json!({ "status": status });
        let resp = self
            .client
            .post(self.url(&format!("/agent/{agent_id}/status")))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("set_agent_status: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error("set_agent_status", resp.status()).await)
        }
    }

    pub async fn explore_start(&self, agent_key: Option<&str>) -> Result<Value, String> {
        let mut body = serde_json::json!({});
        if let Some(k) = agent_key {
            body["agent_key"] = serde_json::json!(k);
        }
        let resp = self
            .client
            .post(self.url("/explore/start"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("explore_start: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("explore_start decode: {e}"))
        } else {
            Err(status_error("explore_start", resp.status()).await)
        }
    }
    pub async fn idle_run(&self, tag: &str, agent_key: Option<&str>, background: bool) -> Result<Value, String> {
        let mut body = serde_json::json!({ "tag": tag, "background": background });
        if let Some(k) = agent_key {
            body["agent_key"] = serde_json::json!(k);
        }
        let resp = self
            .client
            .post(self.url("/idle-run"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("idle_run: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("idle_run decode: {e}"))
        } else {
            // Try to extract the server error message from the response body
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            if let Ok(val) = serde_json::from_str::<Value>(&body_text) {
                if let Some(msg) = val.get("error").and_then(|v| v.as_str()) {
                    return Err(msg.to_owned());
                }
            }
            Err(format!("idle_run failed: {status}"))
        }
    }

    /// Fetch per-agent work/study/fun idle-run button availability.
    pub async fn list_idle_availability(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(self.url("/agents/idle-availability"))
            .send()
            .await
            .map_err(|e| format!("list_idle_availability: {e}"))?;
        if resp.status().is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| format!("list_idle_availability decode: {e}"))
        } else {
            Err(status_error("list_idle_availability", resp.status()).await)
        }
    }
}

async fn status_error(context: &str, status: reqwest::StatusCode) -> String {
    format!("{context} failed: {status}")
}
