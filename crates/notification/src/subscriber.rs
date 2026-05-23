#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


//! EventBus subscriber that translates system events into user-facing notifications.
//!
//! Rules are stacked: the subscriber receives every event and applies a match
//! chain. Matched events produce a `Notification` pushed into `NotificationStore`.

use crate::model::{Category, Notification};
use crate::store::NotificationStore;
use kernel::event::{Event, EventType};
use std::sync::Arc;

/// Subscribes to all events on the EventBus and creates notifications for
/// matching rules.
pub struct NotificationSubscriber {
    store: Arc<NotificationStore>,
}

impl NotificationSubscriber {
    #[must_use]
    pub fn new(store: Arc<NotificationStore>) -> Self {
        Self { store }
    }

    fn maybe_notify(&self, event: &Event) {
        match &event.event_type {
            // ── LLM errors ──────────────────────────────────────────
            EventType::Custom(s) if s == "llm_error" => {
                let msg = event
                    .payload
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                self.store.push(
                    Notification::warning(Category::Llm, "LLM 调用失败", msg)
                        .with_action("查看会话", "/chat"),
                );
            }

            // ── Output blocked ──────────────────────────────────────
            EventType::Custom(s) if s == "output_blocked" => {
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知原因");
                self.store.push(
                    Notification::warning(
                        Category::Security,
                        "输出被安全策略拦截",
                        format!("原因: {reason}"),
                    )
                    .with_action("查看详情", "/events"),
                );
            }

            // ── Message dropped ─────────────────────────────────────
            EventType::Custom(s) if s == "message_dropped" => {
                let session_id = event
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                self.store.push(
                    Notification::warning(
                        Category::Llm,
                        "消息被丢弃",
                        format!("会话 {session_id} 的消息因队列已满被丢弃"),
                    )
                    .with_action("查看会话", "/chat"),
                );
            }

            // ── Injection detected (reserved) ───────────────────────
            EventType::InjectionDetected => {
                let source = &event.source;
                self.store.push(
                    Notification::critical(Category::Security, "检测到提示注入攻击", format!("来源: {source}"))
                        .with_action("查看安全日志", "/events"),
                );
            }

            // ── Workflow state changes ────────────────────────────
            EventType::WorkflowStateChanged => {
                let to_state = event
                    .payload
                    .get("to_state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let workflow_name = event
                    .payload
                    .get("workflow_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let instance_id = event
                    .payload
                    .get("instance_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match to_state {
                    "ERROR" => {
                        let reason = event
                            .payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        self.store.push(
                            Notification::warning(
                                Category::Workflow,
                                "工作流进入错误状态",
                                format!("工作流 {workflow_name}({instance_id}) 错误: {reason}"),
                            )
                            .with_action("查看工作流", "/workflows"),
                        );
                    }
                    "CLOSED" => {
                        let reason = event
                            .payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if reason.contains("timeout") || reason.contains("Timeout") {
                            self.store.push(
                                Notification::warning(
                                    Category::Workflow,
                                    "会话因超时已关闭",
                                    format!("工作流 {workflow_name}({instance_id}) 超时关闭"),
                                )
                                .with_action("查看工作流", "/workflows"),
                            );
                        }
                    }
                    _ => {}
                }
            }

            // ── Secret rotation failure (reserved) ──────────────────
            EventType::SecretRotated => {
                self.store.push(
                    Notification::warning(Category::Secret, "密钥轮换事件", "密钥轮换已触发")
                        .with_event(event.id.to_string(), event.source.to_string()),
                );
            }

            // ── Skill reload changes ────────────────────────────────
            EventType::SkillReloaded => {
                let removed = event
                    .payload
                    .get("removed")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if removed >= 3 {
                    self.store.push(
                        Notification::warning(
                            Category::Skill,
                            "多个技能文件变更",
                            format!("{removed} 个技能被移除"),
                        )
                        .with_action("查看技能", "/skills"),
                    );
                }
            }

            // ── System level via backpressure events ────────────────
            EventType::Custom(s) if s == "system.queue_drained" => {
                // Queues draining is normal, not actionable
            }

            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl event_bus::EventHandler for NotificationSubscriber {
    async fn handle(&self, event: Event) -> kernel::AmanResult<()> {
        self.maybe_notify(&event);
        Ok(())
    }
}
