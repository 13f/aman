// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Core study system types: StudyState, StudyEvent, StudyItem, StudyPhase.
//!
//! Architecture ref: study-design.md v2 §2-3

use kernel::types::Timestamp;
pub use lifecycle::Priority;
use lifecycle::LifecycleState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

// Re-export shared lifecycle types.
pub use lifecycle::{IdleSignal, ItemId, StepOutput};

// ---------------------------------------------------------------------------
// StudyState — type alias
// ---------------------------------------------------------------------------

pub type StudyState = LifecycleState;

// ---------------------------------------------------------------------------
// StudyItemId
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StudyItemId(pub Uuid);

impl StudyItemId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for StudyItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for StudyItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// StudyDepth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StudyDepth {
    /// 略读：标题+摘要+结论。
    Skim,
    /// 通读：完整阅读，记要点笔记。
    #[default]
    Read,
    /// 深度学习：完整阅读 + 笔记 + 练习 + 知识图谱 + 间隔重复。
    Deep,
}

// ---------------------------------------------------------------------------
// StudyPhase — the internal step type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudyPhase {
    /// 获取/搜索材料。
    GatherMaterials,
    /// 制定学习路径。
    Plan,
    /// 学习单个模块。
    LearnModule { index: usize },
    /// 练习。
    Practice,
    /// 巩固（笔记写入、知识图谱连接、间隔复习调度）。
    Consolidate,
}

impl StudyPhase {
    #[must_use]
    pub fn max_retries(&self) -> u32 {
        match self {
            Self::GatherMaterials => 1,
            Self::Plan => 1,
            Self::LearnModule { .. } => 2,
            Self::Practice => 3,
            Self::Consolidate => 2,
        }
    }

    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::GatherMaterials => "Gather learning materials".into(),
            Self::Plan => "Create learning plan".into(),
            Self::LearnModule { index } => format!("Learn module {index}"),
            Self::Practice => "Practice exercises".into(),
            Self::Consolidate => "Consolidate knowledge".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// StudyItem
// ---------------------------------------------------------------------------

/// 推送到 Study 队列的学习工作单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyItem {
    pub id: StudyItemId,
    pub topic: String,

    /// 学习材料（可选）。
    pub materials: Option<Vec<MaterialRef>>,

    /// 学习深度。
    #[serde(default)]
    pub depth: StudyDepth,

    /// 优先级。
    #[serde(default)]
    pub priority: Priority,

    /// 执行超时。
    #[serde(default)]
    pub timeout: Option<Duration>,

    /// 附带的上下文。
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,

    /// 是否需要在完成后通知调用方。
    #[serde(default)]
    pub notify_on_complete: bool,

    /// 创建时间。
    #[serde(default = "Timestamp::now")]
    pub created_at: Timestamp,
}

// ---------------------------------------------------------------------------
// MaterialRef
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialRef {
    pub title: String,
    pub url: Option<String>,
    pub source: String,
    pub relevance: Option<f64>,
}

// ---------------------------------------------------------------------------
// StudyItemSource
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StudyItemSource {
    UserAssigned { operator: String },
    IdleExploration { curiosity_topic: String },
    MaterialSubscription { feed_url: String },
    ScheduledReview { node_id: String, review_round: u32 },
    SeekResponse { request_id: String },
    Custom {
        name: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
}

// ---------------------------------------------------------------------------
// StudyEvent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "study_event_type", rename_all = "snake_case")]
pub enum StudyEvent {
    StudyItemAssigned {
        item: StudyItem,
        source: StudyItemSource,
    },
    StudyItemCompleted {
        item_id: StudyItemId,
        outcome: StudyOutcome,
        duration: Duration,
    },
    StudyItemFailed {
        item_id: StudyItemId,
        error: StudyError,
        retryable: bool,
    },
    Interrupt {
        reason: String,
        by_system: String,
    },
}

// ---------------------------------------------------------------------------
// StudyOutcome / StudyError
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyOutcome {
    Completed { comprehension: f64 },
    Failed { retryable: bool },
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

// ---------------------------------------------------------------------------
// StudyResult
// ---------------------------------------------------------------------------

pub type StudyResult<T> = Result<T, StudyError>;

impl From<lifecycle::LifecycleError> for StudyError {
    fn from(e: lifecycle::LifecycleError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            retryable: e.retryable,
        }
    }
}

// ---------------------------------------------------------------------------
// LearningPath
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningPath {
    pub modules: Vec<LearningModule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningModule {
    pub index: usize,
    pub title: String,
    pub concepts: Vec<String>,
}

// ---------------------------------------------------------------------------
// StudyNotes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StudyNotes {
    pub key_concepts: Vec<(String, String)>,
    pub open_questions: Vec<String>,
    pub comprehension: f64,
}

impl StudyNotes {
    pub fn merge(&mut self, other: StudyNotes) {
        self.key_concepts.extend(other.key_concepts);
        self.open_questions.extend(other.open_questions);
        self.comprehension = (self.comprehension + other.comprehension) / 2.0;
    }
}

// ---------------------------------------------------------------------------
// StudyContext
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StudyContext {
    pub(crate) inner: lifecycle::LifecycleContext<StudyItem, StudyPhase>,
    pub learning_path: Option<LearningPath>,
    pub accumulated_notes: StudyNotes,
}

impl StudyContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: lifecycle::LifecycleContext::new(),
            learning_path: None,
            accumulated_notes: StudyNotes::default(),
        }
    }

    #[must_use]
    pub fn state(&self) -> StudyState {
        self.inner.state
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.inner.queue_len()
    }

    pub fn enqueue(&mut self, item: StudyItem) {
        self.inner.enqueue(item);
    }

    pub fn dequeue(&mut self) -> Option<StudyItem> {
        self.inner.dequeue()
    }

    #[must_use]
    pub fn current(&self) -> Option<&StudyItem> {
        self.inner.current.as_ref()
    }

    #[must_use]
    pub fn phases(&self) -> &[StudyPhase] {
        &self.inner.steps
    }

    #[must_use]
    pub fn phase_index(&self) -> usize {
        self.inner.step_index
    }

    #[must_use]
    pub fn step_outputs(&self) -> &[StepOutput] {
        &self.inner.step_outputs
    }

    pub fn reset_to_idle(&mut self) {
        self.inner.reset_to_idle();
        self.learning_path = None;
        self.accumulated_notes = StudyNotes::default();
    }
}

impl Default for StudyContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Event source constants
// ---------------------------------------------------------------------------

pub const STUDY_SOURCE: &str = "study.system";
pub const STUDY_STEP_KIND: &str = "study.phase.execute";

impl StudyEvent {
    #[must_use]
    pub fn source(&self) -> &'static str {
        STUDY_SOURCE
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::StudyItemAssigned { .. } => "study.item.assigned",
            Self::StudyItemCompleted { .. } => "study.item.completed",
            Self::StudyItemFailed { .. } => "study.item.failed",
            Self::Interrupt { .. } => "study.interrupt",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn study_state_is_lifecycle_state() {
        let ss: StudyState = LifecycleState::Idle;
        assert_eq!(ss, StudyState::Idle);
    }

    #[test]
    fn study_depth_default_is_read() {
        assert_eq!(StudyDepth::default(), StudyDepth::Read);
    }

    #[test]
    fn study_context_new_is_idle() {
        let ctx = StudyContext::new();
        assert!(ctx.is_idle());
        assert!(ctx.current().is_none());
        assert_eq!(ctx.queue_len(), 0);
        assert!(ctx.phases().is_empty());
    }

    #[test]
    fn study_context_enqueue_dequeue_fifo() {
        let mut ctx = StudyContext::new();
        let item_a = StudyItem {
            id: StudyItemId::new(),
            topic: "A".into(),
            materials: None,
            depth: StudyDepth::Read,
            priority: Priority::default(),
            timeout: None,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        };
        let item_b = StudyItem {
            id: StudyItemId::new(),
            topic: "B".into(),
            ..item_a.clone()
        };
        ctx.enqueue(item_a.clone());
        ctx.enqueue(item_b);
        assert_eq!(ctx.queue_len(), 2);
        assert_eq!(ctx.dequeue().unwrap().topic, "A");
        assert_eq!(ctx.dequeue().unwrap().topic, "B");
    }

    #[test]
    fn study_event_serde_tagged() {
        let event = StudyEvent::StudyItemAssigned {
            item: StudyItem {
                id: StudyItemId::new(),
                topic: "test".into(),
                materials: None,
                depth: StudyDepth::Read,
                priority: Priority::default(),
                timeout: None,
                context: HashMap::new(),
                notify_on_complete: false,
                created_at: Timestamp::now(),
            },
            source: StudyItemSource::UserAssigned {
                operator: "user".into(),
            },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("study_item_assigned"), "{json}");
        let deser: StudyEvent = serde_json::from_str(&json).expect("deserialize");
        match deser {
            StudyEvent::StudyItemAssigned { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn study_phase_max_retries() {
        assert_eq!(StudyPhase::GatherMaterials.max_retries(), 1);
        assert_eq!(StudyPhase::LearnModule { index: 0 }.max_retries(), 2);
        assert_eq!(StudyPhase::Practice.max_retries(), 3);
    }

    #[test]
    fn study_notes_merge() {
        let mut a = StudyNotes {
            key_concepts: vec![("k1".into(), "v1".into())],
            open_questions: vec!["q1".into()],
            comprehension: 0.8,
        };
        let b = StudyNotes {
            key_concepts: vec![("k2".into(), "v2".into())],
            open_questions: vec!["q2".into()],
            comprehension: 0.6,
        };
        a.merge(b);
        assert_eq!(a.key_concepts.len(), 2);
        assert_eq!(a.open_questions.len(), 2);
        assert!((a.comprehension - 0.7).abs() < 0.01);
    }
}
