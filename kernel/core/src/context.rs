// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::hook::EventPublisher;
use crate::types::TraceId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub type ContextLabels = BTreeMap<String, String>;
pub type ContextExtensions = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BaseContext {
    pub trace_id: TraceId,
    pub timeout_ms: Option<u64>,
    pub labels: ContextLabels,
    pub extensions: ContextExtensions,
    /// Event bus available for publishing progress/completion events during
    /// long-running operations (e.g. exec in detach mode). Skipped in serde /
    /// equality — set by the runtime before dispatching.
    #[serde(skip, default)]
    pub event_bus: Option<Arc<dyn EventPublisher>>,
}

// Manual PartialEq: skip event_bus (dyn trait pointers can't be compared).
impl PartialEq for BaseContext {
    fn eq(&self, other: &Self) -> bool {
        self.trace_id == other.trace_id
            && self.timeout_ms == other.timeout_ms
            && self.labels == other.labels
            && self.extensions == other.extensions
    }
}

impl BaseContext {
    #[must_use]
    pub fn new(trace_id: TraceId) -> Self {
        Self {
            trace_id,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SkillContext {
    pub base: BaseContext,
    pub skill_name: Option<String>,
    pub soul_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PipelineContext {
    pub base: BaseContext,
    pub pipeline_id: Option<String>,
    pub instance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolContext {
    pub base: BaseContext,
    pub tool_name: Option<String>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookContext {
    pub base: BaseContext,
    pub hook_name: Option<String>,
    /// Event bus available to hooks for publishing events (e.g. progress
    /// notifications). Skipped in serde/compare — set by the runtime before
    /// dispatching hooks.
    #[serde(skip, default)]
    pub event_bus: Option<Arc<dyn EventPublisher>>,
}

// Manual PartialEq: skip event_bus (dyn trait pointers can't be compared).
impl PartialEq for HookContext {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base && self.hook_name == other.hook_name
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginTrackedResources {
    pub fds: Vec<u64>,
    pub dbs: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PluginResourceTracker {
    pub resources: PluginTrackedResources,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginContext {
    pub base: BaseContext,
    pub plugin_name: Option<String>,
    #[serde(skip, default = "default_plugin_resource_tracker")]
    pub resource_tracker: Arc<Mutex<PluginResourceTracker>>,
}

impl PartialEq for PluginContext {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base && self.plugin_name == other.plugin_name
    }
}

fn default_plugin_resource_tracker() -> Arc<Mutex<PluginResourceTracker>> {
    Arc::new(Mutex::new(PluginResourceTracker::default()))
}

impl PluginContext {
    pub fn track_fd(&self, fd: u64) {
        self.resource_tracker
            .lock()
            .expect("plugin resource tracker lock")
            .resources
            .fds
            .push(fd);
    }

    pub fn track_db(&self, db: impl Into<String>) {
        self.resource_tracker
            .lock()
            .expect("plugin resource tracker lock")
            .resources
            .dbs
            .push(db.into());
    }

    pub fn track_path(&self, path: impl Into<String>) {
        self.resource_tracker
            .lock()
            .expect("plugin resource tracker lock")
            .resources
            .paths
            .push(path.into());
    }

    #[must_use]
    pub fn tracked_resources(&self) -> PluginTrackedResources {
        self.resource_tracker
            .lock()
            .expect("plugin resource tracker lock")
            .resources
            .clone()
    }

    #[must_use]
    pub fn clear_tracked_resources(&self) -> PluginTrackedResources {
        let mut tracker = self
            .resource_tracker
            .lock()
            .expect("plugin resource tracker lock");
        std::mem::take(&mut tracker.resources)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SourceContext {
    pub base: BaseContext,
    pub source_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        BaseContext, HookContext, PipelineContext, PluginContext, SkillContext, SourceContext,
        ToolContext,
    };
    use crate::types::TraceId;

    #[test]
    fn base_context_new_sets_trace_id() {
        let trace_id = TraceId::new();
        let context = BaseContext::new(trace_id);

        assert_eq!(context.trace_id, trace_id);
        assert!(context.timeout_ms.is_none());
        assert!(context.labels.is_empty());
        assert!(context.extensions.is_empty());
    }

    #[test]
    fn specialized_contexts_default_to_empty_state() {
        let skill = SkillContext::default();
        let pipeline = PipelineContext::default();
        let tool = ToolContext::default();
        let hook = HookContext::default();
        let plugin = PluginContext::default();
        let source = SourceContext::default();

        assert!(skill.skill_name.is_none());
        assert!(pipeline.pipeline_id.is_none());
        assert!(tool.tool_name.is_none());
        assert!(hook.hook_name.is_none());
        assert!(plugin.plugin_name.is_none());
        assert!(source.source_name.is_none());
    }

    #[test]
    fn plugin_context_tracks_and_clears_resources() {
        let context = PluginContext::default();
        context.track_fd(7);
        context.track_db("sqlite://test.db");
        context.track_path("/tmp/aman/plugin.sock");

        let tracked = context.tracked_resources();
        assert_eq!(tracked.fds, vec![7]);
        assert_eq!(tracked.dbs, vec!["sqlite://test.db".to_owned()]);
        assert_eq!(tracked.paths, vec!["/tmp/aman/plugin.sock".to_owned()]);

        let cleared = context.clear_tracked_resources();
        assert_eq!(cleared, tracked);
        let empty = context.tracked_resources();
        assert!(empty.fds.is_empty());
        assert!(empty.dbs.is_empty());
        assert!(empty.paths.is_empty());
    }
}
