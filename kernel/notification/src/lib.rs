#![forbid(unsafe_code)]
#![doc = "Notification center — severity-classed user-facing alerts for the aman agent runtime."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


pub mod model;
pub mod store;
pub mod subscriber;

pub use model::{Category, Notification, Severity};
pub use store::NotificationStore;
pub use subscriber::NotificationSubscriber;

#[cfg(test)]
mod tests {
    use super::*;
    use event_bus::EventHandler;
    use kernel::event::{Event, EventType};
    use std::sync::Arc;

    #[test]
    fn store_push_list_roundtrip() {
        let store = NotificationStore::new(100);
        assert_eq!(store.unread_count(), 0);

        let n1 = Notification::info(Category::Llm, "test title", "test message");
        let n2 = Notification::warning(Category::Security, "warning title", "warning message");
        let n3 = Notification::critical(Category::Workflow, "critical title", "critical message");

        store.push(n1);
        store.push(n2);
        store.push(n3);

        assert_eq!(store.unread_count(), 3);

        // List all active with no filter
        let all = store.list(true, None, 10, 0);
        assert_eq!(all.len(), 3);

        // List without active filter returns all
        let all_no_filter = store.list(false, None, 10, 0);
        assert_eq!(all_no_filter.len(), 3);

        // Filter by severity
        let criticals = store.list(true, Some(Severity::Critical), 10, 0);
        assert_eq!(criticals.len(), 1);
        assert_eq!(criticals[0].severity, Severity::Critical);

        // Pagination: limit
        let limited = store.list(true, None, 2, 0);
        assert_eq!(limited.len(), 2);

        // Pagination: offset
        let offset = store.list(true, None, 10, 2);
        assert_eq!(offset.len(), 1);
    }

    #[test]
    fn store_dismiss_and_acknowledge() {
        let store = NotificationStore::new(100);

        // Dismissible warning
        let n1 = Notification::warning(Category::Llm, "dismiss me", "msg");
        let n1_id = n1.id.clone();
        store.push(n1);

        // Non-dismissible critical
        let n2 = Notification::critical(Category::Security, "ack me", "msg");
        let n2_id = n2.id.clone();
        store.push(n2);

        // dismiss() on dismissible notification succeeds
        assert!(store.dismiss(&n1_id));
        assert_eq!(store.unread_count(), 1); // only n2 still active

        // dismiss() on non-dismissible (critical) fails
        assert!(!store.dismiss(&n2_id));

        // acknowledge() on critical works
        assert!(store.acknowledge(&n2_id));
        assert_eq!(store.unread_count(), 0);

        // dismiss on nonexistent ID returns false
        assert!(!store.dismiss("nonexistent"));
    }

    #[test]
    fn store_dismiss_all() {
        let store = NotificationStore::new(100);
        store.push(Notification::info(Category::Skill, "i1", ""));
        store.push(Notification::warning(Category::Backpressure, "w1", ""));
        store.push(Notification::critical(Category::Idle, "c1", ""));

        assert_eq!(store.unread_count(), 3);

        store.dismiss_all();

        // Critical notifications are not dismissible, so they remain active
        assert_eq!(store.unread_count(), 1);
        let remaining = store.list(true, None, 10, 0);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].severity, Severity::Critical);
    }

    #[test]
    fn store_eviction_at_capacity() {
        let store = NotificationStore::new(3);
        store.push(Notification::info(Category::Llm, "a", ""));
        store.push(Notification::info(Category::Llm, "b", ""));
        store.push(Notification::info(Category::Llm, "c", ""));
        assert_eq!(store.list(false, None, 10, 0).len(), 3);

        // Pushing a 4th evicts oldest ("a")
        store.push(Notification::info(Category::Llm, "d", ""));
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 3);
        // "d" should be newest (first in reversed list)
        assert_eq!(all[0].title, "d");
    }

    #[test]
    fn store_min_capacity_is_one() {
        let store = NotificationStore::new(0); // should be clamped to 1
        store.push(Notification::info(Category::Llm, "only", ""));
        assert_eq!(store.list(false, None, 10, 0).len(), 1);
        store.push(Notification::info(Category::Llm, "second", ""));
        assert_eq!(store.list(false, None, 10, 0).len(), 1);
    }

    // ── NotificationSubscriber characterization tests ──────────

    #[test]
    fn subscriber_llm_error_creates_warning() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "test",
            EventType::Custom("llm_error".into()),
            serde_json::json!({"error": "rate limit exceeded"}),
        );
        // Call maybe_notify (private) via the EventHandler impl
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].severity, Severity::Warning);
        assert_eq!(all[0].category, Category::Llm);
        assert!(all[0].title.contains("LLM"));
    }

    #[test]
    fn subscriber_injection_detected_creates_critical() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "attacker",
            EventType::InjectionDetected,
            serde_json::json!({}),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].severity, Severity::Critical);
        assert_eq!(all[0].category, Category::Security);
    }

    #[test]
    fn subscriber_workflow_error_creates_warning() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "wf",
            EventType::WorkflowStateChanged,
            serde_json::json!({
                "to_state": "ERROR",
                "workflow_name": "approval",
                "instance_id": "inst-1",
                "reason": "timeout"
            }),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].category, Category::Workflow);
        assert!(all[0].title.contains("错误"));
    }

    #[test]
    fn subscriber_skill_reloaded_with_three_removed_creates_warning() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "skills",
            EventType::SkillReloaded,
            serde_json::json!({"removed": ["a", "b", "c"]}),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].category, Category::Skill);
    }

    #[test]
    fn subscriber_skill_reloaded_with_one_removed_no_notification() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "skills",
            EventType::SkillReloaded,
            serde_json::json!({"removed": ["a"]}),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        assert_eq!(store.unread_count(), 0);
    }

    #[test]
    fn subscriber_output_blocked_creates_security_warning() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "validator",
            EventType::Custom("output_blocked".into()),
            serde_json::json!({"reason": "secret leak detected"}),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].category, Category::Security);
        assert!(all[0].message.contains("secret leak"));
    }

    #[test]
    fn subscriber_normal_event_no_notification() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "src",
            EventType::Custom("some_random_event".into()),
            serde_json::json!({}),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        assert_eq!(store.unread_count(), 0);
    }

    #[test]
    fn subscriber_llm_backend_down_creates_critical() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "llm_health",
            EventType::Custom("llm_backend_down".into()),
            serde_json::json!({
                "base_url": "https://api.openai.com/v1",
                "from": "Degraded",
                "to": "Down",
                "consecutive_failures": 6,
                "last_error": "timeout"
            }),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].severity, Severity::Critical);
        assert_eq!(all[0].category, Category::Llm);
        assert!(all[0].title.contains("不可用"));
    }

    #[test]
    fn subscriber_llm_backend_recovered_creates_info() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "llm_health",
            EventType::Custom("llm_backend_recovered".into()),
            serde_json::json!({
                "base_url": "https://api.openai.com/v1",
                "from": "Down",
                "to": "Ok",
                "consecutive_failures": 0,
                "last_error": ""
            }),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].severity, Severity::Info);
        assert_eq!(all[0].category, Category::Gateway);
        assert!(all[0].title.contains("恢复"));
    }

    #[test]
    fn subscriber_cognitive_state_catatonic_creates_critical() {
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "cognitive_health",
            EventType::Custom("cognitive_state_changed".into()),
            serde_json::json!({
                "agent_id": "coder",
                "state": "Catatonic",
                "force_sleep": true
            }),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        let all = store.list(false, None, 10, 0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].severity, Severity::Critical);
        assert!(all[0].title.contains("木僵"));
    }

    #[test]
    fn subscriber_cognitive_state_groggy_no_notification() {
        // Groggy 不需要通知（只有 Catatonic/Coma 才通知）
        let store = Arc::new(NotificationStore::new(100));
        let subscriber = NotificationSubscriber::new(Arc::clone(&store));
        let event = Event::new(
            "cognitive_health",
            EventType::Custom("cognitive_state_changed".into()),
            serde_json::json!({
                "agent_id": "coder",
                "state": "Groggy",
                "force_sleep": true
            }),
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async { subscriber.handle(event).await.unwrap() });
        assert_eq!(store.unread_count(), 0);
    }

    #[test]
    fn notification_model_constructors() {
        let info = Notification::info(Category::Llm, "title", "msg");
        assert_eq!(info.severity, Severity::Info);
        assert!(info.dismissible);
        assert!(!info.dismissed);
        assert!(info.action_label.is_none());

        let warning = Notification::warning(Category::Security, "warn", "msg");
        assert_eq!(warning.severity, Severity::Warning);
        assert!(warning.dismissible);

        let critical = Notification::critical(Category::Workflow, "crit", "msg");
        assert_eq!(critical.severity, Severity::Critical);
        assert!(!critical.dismissible);

        // with_action() builder
        let with_action = Notification::info(Category::Llm, "t", "m")
            .with_action("View", "/view");
        assert_eq!(with_action.action_label.unwrap(), "View");

        // with_event() builder
        let with_event = Notification::info(Category::Llm, "t", "m")
            .with_event("evt-1", "source-a");
        assert_eq!(with_event.event_id.unwrap(), "evt-1");
    }
}
