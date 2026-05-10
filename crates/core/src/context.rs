use crate::types::TraceId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type ContextLabels = BTreeMap<String, String>;
pub type ContextExtensions = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BaseContext {
    pub trace_id: TraceId,
    pub timeout_ms: Option<u64>,
    pub labels: ContextLabels,
    pub extensions: ContextExtensions,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HookContext {
    pub base: BaseContext,
    pub hook_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PluginContext {
    pub base: BaseContext,
    pub plugin_name: Option<String>,
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
}
