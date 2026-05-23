#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use kernel::context::{BaseContext, PipelineContext, SkillContext, ToolContext};
use kernel::event::Event;
use kernel::AmanResult;
use soul::{Soul, SoulChangedNotifier, SoulHotReloadManager};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Clone)]
pub struct SoulRuntime {
    soul: Arc<RwLock<Arc<Soul>>>,
    last_soul_changed_event: Arc<RwLock<Option<Event>>>,
}

impl SoulRuntime {
    #[must_use]
    pub fn new(initial: Soul) -> Self {
        Self {
            soul: Arc::new(RwLock::new(Arc::new(initial))),
            last_soul_changed_event: Arc::new(RwLock::new(None)),
        }
    }

    #[must_use]
    pub fn current_soul(&self) -> Arc<Soul> {
        self.soul.read().expect("runtime soul lock").clone()
    }

    #[must_use]
    pub fn inject_skill_context(&self, context: SkillContext) -> SkillContext {
        self.current_soul().inject_skill_context(context)
    }

    #[must_use]
    pub fn inject_pipeline_context(&self, context: PipelineContext) -> PipelineContext {
        self.current_soul().inject_pipeline_context(context)
    }

    #[must_use]
    pub fn inject_tool_context(&self, context: ToolContext) -> ToolContext {
        self.current_soul().inject_tool_context(context)
    }

    #[must_use]
    pub fn inject_base_context(&self, base: BaseContext) -> BaseContext {
        self.current_soul().inject_base_context(base)
    }

    #[must_use]
    pub fn last_soul_changed_event(&self) -> Option<Event> {
        self.last_soul_changed_event
            .read()
            .expect("runtime soul event lock")
            .clone()
    }

    pub fn build_hot_reload_manager(&self, soul_file: PathBuf) -> AmanResult<SoulHotReloadManager> {
        let initial = Soul::from_file(&soul_file)?;
        let notifier: Arc<dyn SoulChangedNotifier> = Arc::new(self.clone());
        Ok(SoulHotReloadManager::new(soul_file, initial).with_notifier(notifier))
    }

    pub fn reload_now(
        &self,
        manager: &mut SoulHotReloadManager,
    ) -> AmanResult<Option<Event>> {
        let event = manager.reload_now()?;
        if let Some(ref changed) = event {
            *self
                .last_soul_changed_event
                .write()
                .expect("runtime soul event lock") = Some(changed.clone());
        }
        Ok(event)
    }

    pub fn poll_once(
        &self,
        manager: &mut SoulHotReloadManager,
        timeout: Duration,
    ) -> AmanResult<Option<Event>> {
        let event = manager.poll_once(timeout)?;
        if let Some(ref changed) = event {
            *self
                .last_soul_changed_event
                .write()
                .expect("runtime soul event lock") = Some(changed.clone());
        }
        Ok(event)
    }
}

impl SoulChangedNotifier for SoulRuntime {
    fn on_soul_changed(&self, soul: Arc<Soul>) -> AmanResult<()> {
        *self.soul.write().expect("runtime soul lock") = soul;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SoulRuntime;
    use kernel::context::{BaseContext, SkillContext};
    use kernel::event::EventType;
    use kernel::types::TraceId;
    use serde_json::Value;
    use soul::Soul;

    const SOUL_MD: &str = r#"# aman
## boundaries
- never leak secrets
"#;

    #[test]
    fn runtime_refreshes_soul_and_injects_latest_context() {
        let temp_dir = std::env::temp_dir().join(format!("aman-runtime-soul-{}", TraceId::new()));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let soul_file = temp_dir.join("SOUL.md");
        std::fs::write(&soul_file, SOUL_MD).expect("write initial soul");

        let runtime = SoulRuntime::new(Soul::from_file(&soul_file).expect("initial soul"));
        let mut manager = runtime
            .build_hot_reload_manager(soul_file.clone())
            .expect("build manager");

        std::fs::write(
            &soul_file,
            "# aman\n## boundaries\n- never leak secrets\n- do not execute destructive commands\n",
        )
        .expect("rewrite soul");

        let event = runtime.reload_now(&mut manager).expect("reload now");
        assert!(event.is_some());
        assert_eq!(
            event
                .as_ref()
                .expect("event exists")
                .event_type,
            EventType::Custom("soul_changed".to_owned())
        );
        assert!(runtime.last_soul_changed_event().is_some());

        let injected = runtime.inject_skill_context(SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("test-skill".to_owned()),
            soul_name: None,
        });
        assert_eq!(injected.soul_name, Some("aman".to_owned()));
        let boundaries = injected
            .base
            .extensions
            .get("soul.boundaries")
            .and_then(Value::as_array)
            .expect("soul boundaries array");
        assert_eq!(boundaries.len(), 2);
    }
}
