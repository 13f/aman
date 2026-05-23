#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use serde::{Deserialize, Serialize};

/// Severity level — determines UI treatment (modal vs toast, color, dismiss behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Red overlay, must acknowledge (POST /ack), cannot be dismissed.
    Critical,
    /// Yellow toast / banner, auto-dismiss or click to dismiss.
    Warning,
}

/// Category for grouping and filtering notifications in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Backpressure,
    Security,
    Workflow,
    Llm,
    Plugin,
    Skill,
    Dlq,
    Gateway,
    Secret,
}

impl Category {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backpressure => "backpressure",
            Self::Security => "security",
            Self::Workflow => "workflow",
            Self::Llm => "llm",
            Self::Plugin => "plugin",
            Self::Skill => "skill",
            Self::Dlq => "dlq",
            Self::Gateway => "gateway",
            Self::Secret => "secret",
        }
    }
}

/// A single notification destined for the user's attention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub severity: Severity,
    pub category: Category,
    pub created_at: i64,
    pub title: String,
    pub message: String,

    /// Whether the notification has been dismissed by the user.
    pub dismissed: bool,
    /// Critical notifications cannot be dismissed — they must be acknowledged.
    pub dismissible: bool,

    /// Optional action button label (e.g. "View DLQ").
    pub action_label: Option<String>,
    /// Optional frontend route to navigate to on action click (e.g. "/dlq").
    pub action_route: Option<String>,

    /// Correlated event id, if any.
    pub event_id: Option<String>,
    /// Event source string, if any.
    pub source: Option<String>,
}

impl Notification {
    /// Build a new warning notification that is dismissible and has no action.
    #[must_use]
    pub fn warning(
        category: Category,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            severity: Severity::Warning,
            category,
            created_at: kernel::types::Timestamp::now().as_millis(),
            title: title.into(),
            message: message.into(),
            dismissed: false,
            dismissible: true,
            action_label: None,
            action_route: None,
            event_id: None,
            source: None,
        }
    }

    /// Build a new critical notification that must be acknowledged.
    #[must_use]
    pub fn critical(
        category: Category,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            severity: Severity::Critical,
            category,
            created_at: kernel::types::Timestamp::now().as_millis(),
            title: title.into(),
            message: message.into(),
            dismissed: false,
            dismissible: false,
            action_label: None,
            action_route: None,
            event_id: None,
            source: None,
        }
    }

    /// Attach an action button.
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>, route: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self.action_route = Some(route.into());
        self
    }

    /// Attach event correlation.
    #[must_use]
    pub fn with_event(mut self, event_id: impl Into<String>, source: impl Into<String>) -> Self {
        self.event_id = Some(event_id.into());
        self.source = Some(source.into());
        self
    }

    /// Whether this notification is still live (undismissed and unacknowledged).
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.dismissed
    }
}
