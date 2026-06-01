#![forbid(unsafe_code)]
#![doc = "SOUL model, parser, boundary checks, and hot reload for aman."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use kernel::context::{BaseContext, PipelineContext, SkillContext, ToolContext};
use kernel::event::{Event, EventType};
use kernel::{AmanResult, Error};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Soul {
    pub name: String,
    pub identity: String,
    pub core: String,
    pub expertise: Vec<String>,
    pub boundaries: Vec<String>,
    pub vibe: String,
    pub preferences: Vec<String>,
    pub raw: String,
}

impl Soul {
    pub fn from_file(path: &Path) -> AmanResult<Self> {
        let content = fs::read_to_string(path)?;
        content.parse()
    }

    /// Parse a `Soul` from raw markdown content.
    pub fn parse(content: &str) -> AmanResult<Self> {
        let parsed = SoulMarkdown::parse(content)?;
        let name = parsed
            .title
            .clone()
            .or_else(|| parsed.sections.get("name").cloned())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "aman".to_owned());
        Ok(Self {
            name,
            identity: parsed.sections.get("identity").cloned().unwrap_or_default(),
            core: parsed.sections.get("core").cloned().unwrap_or_default(),
            expertise: parsed.list("expertise"),
            boundaries: parsed.list("boundaries"),
            vibe: parsed.sections.get("vibe").cloned().unwrap_or_default(),
            preferences: parsed.list("preferences"),
            raw: content.to_owned(),
        })
    }

    #[must_use]
    pub fn to_system_prompt(&self) -> String {
        let mut lines = vec![format!("You are {}.", self.name)];
        if !self.identity.trim().is_empty() {
            lines.push(format!("Identity: {}", self.identity.trim()));
        }
        if !self.core.trim().is_empty() {
            lines.push(format!("Core: {}", self.core.trim()));
        }
        if !self.expertise.is_empty() {
            lines.push(format!("Expertise: {}", self.expertise.join(", ")));
        }
        if !self.vibe.trim().is_empty() {
            lines.push(format!("Vibe: {}", self.vibe.trim()));
        }
        if !self.preferences.is_empty() {
            lines.push(format!("Preferences: {}", self.preferences.join("; ")));
        }
        if !self.boundaries.is_empty() {
            lines.push("Boundaries:".to_owned());
            lines.extend(self.boundaries.iter().map(|item| format!("- {item}")));
        }
        lines.join("\n")
    }

    pub fn check_boundary(&self, text: &str) -> AmanResult<()> {
        let text = text.trim().to_lowercase();
        for boundary in &self.boundaries {
            let trimmed = boundary.trim();
            if trimmed.is_empty() {
                continue;
            }
            let boundary_lower = trimmed.to_lowercase();
            let derived = boundary_lower
                .strip_prefix("do not ")
                .or_else(|| boundary_lower.strip_prefix("don't "))
                .or_else(|| boundary_lower.strip_prefix("never "))
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let blocked = text.contains(&boundary_lower)
                || derived
                    .map(|value| text.contains(value))
                    .unwrap_or(false);
            if blocked {
                return Err(Error::PermissionDenied {
                    message: format!("blocked by soul boundary: {trimmed}"),
                });
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn inject_base_context(&self, mut base: BaseContext) -> BaseContext {
        base.extensions.insert(
            "soul.name".to_owned(),
            Value::String(self.name.clone()),
        );
        base.extensions.insert(
            "soul.system_prompt".to_owned(),
            Value::String(self.to_system_prompt()),
        );
        if !self.boundaries.is_empty() {
            base.extensions.insert(
                "soul.boundaries".to_owned(),
                Value::Array(
                    self.boundaries
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect::<Vec<_>>(),
                ),
            );
        }
        base
    }

    #[must_use]
    pub fn inject_skill_context(&self, mut context: SkillContext) -> SkillContext {
        context.base = self.inject_base_context(context.base);
        context.soul_name = Some(self.name.clone());
        context
    }

    #[must_use]
    pub fn inject_pipeline_context(&self, mut context: PipelineContext) -> PipelineContext {
        context.base = self.inject_base_context(context.base);
        context
    }

    #[must_use]
    pub fn inject_tool_context(&self, mut context: ToolContext) -> ToolContext {
        context.base = self.inject_base_context(context.base);
        context
    }
}

impl FromStr for Soul {
    type Err = kernel::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[derive(Default)]
struct SoulMarkdown {
    title: Option<String>,
    sections: std::collections::HashMap<String, String>,
}

impl SoulMarkdown {
    fn parse(content: &str) -> AmanResult<Self> {
        let mut title = None;
        let mut sections = std::collections::HashMap::new();
        let mut current = None::<String>;
        let mut current_lines = Vec::<String>::new();

        let flush_section = |current: &mut Option<String>,
                             lines: &mut Vec<String>,
                             sections: &mut std::collections::HashMap<String, String>| {
            if let Some(section_name) = current.take() {
                let body = lines.join("\n").trim().to_owned();
                sections.insert(section_name, body);
                lines.clear();
            }
        };

        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("# ") {
                if title.is_none() {
                    title = Some(rest.trim().to_owned());
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("## ") {
                flush_section(&mut current, &mut current_lines, &mut sections);
                current = Some(rest.trim().to_lowercase());
                continue;
            }
            if current.is_some() {
                current_lines.push(line.to_owned());
            }
        }
        flush_section(&mut current, &mut current_lines, &mut sections);

        if title.is_none() && sections.is_empty() {
            return Err(Error::ConfigInvalid {
                message: "SOUL.md content is empty or invalid".to_owned(),
            });
        }
        Ok(Self { title, sections })
    }

    fn list(&self, section: &str) -> Vec<String> {
        self.sections
            .get(section)
            .map(|content| {
                content
                    .lines()
                    .filter_map(|line| {
                        let trimmed = line.trim();
                        trimmed
                            .strip_prefix("- ")
                            .map(str::trim)
                            .or_else(|| trimmed.strip_prefix("* ").map(str::trim))
                            .map(ToOwned::to_owned)
                    })
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

pub trait SoulChangedNotifier: Send + Sync {
    fn on_soul_changed(&self, soul: Arc<Soul>) -> AmanResult<()>;
}

#[derive(Default)]
pub struct NoopSoulNotifier;

impl SoulChangedNotifier for NoopSoulNotifier {
    fn on_soul_changed(&self, _soul: Arc<Soul>) -> AmanResult<()> {
        Ok(())
    }
}

pub struct SoulHotReloadManager {
    soul_file: PathBuf,
    soul: Arc<RwLock<Arc<Soul>>>,
    debounce_ms: u64,
    watcher: Option<notify::RecommendedWatcher>,
    receiver: Option<Receiver<notify::Result<notify::Event>>>,
    last_reload_at: Option<Instant>,
    notifier: Arc<dyn SoulChangedNotifier>,
}

impl SoulHotReloadManager {
    #[must_use]
    pub fn new(soul_file: PathBuf, initial: Soul) -> Self {
        Self {
            soul_file,
            soul: Arc::new(RwLock::new(Arc::new(initial))),
            debounce_ms: 500,
            watcher: None,
            receiver: None,
            last_reload_at: None,
            notifier: Arc::new(NoopSoulNotifier),
        }
    }

    #[must_use]
    pub fn with_debounce_ms(mut self, debounce_ms: u64) -> Self {
        self.debounce_ms = debounce_ms;
        self
    }

    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn SoulChangedNotifier>) -> Self {
        self.notifier = notifier;
        self
    }

    #[must_use]
    pub fn current(&self) -> Arc<Soul> {
        self.soul.read().expect("soul lock").clone()
    }

    #[must_use]
    pub fn soul_file(&self) -> &Path {
        &self.soul_file
    }

    pub fn start_watching(&mut self) -> AmanResult<()> {
        if self.watcher.is_some() {
            return Ok(());
        }
        let parent = self.soul_file.parent().ok_or_else(|| Error::ConfigInvalid {
            message: format!("SOUL file has no parent directory: {}", self.soul_file.display()),
        })?;
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .map_err(|error| Error::Unrecoverable {
            message: format!("notify watcher init failed: {error}"),
        })?;
        watcher
            .watch(parent, notify::RecursiveMode::NonRecursive)
            .map_err(|error| Error::Unrecoverable {
                message: format!("notify watcher subscribe failed: {error}"),
            })?;
        self.receiver = Some(rx);
        self.watcher = Some(watcher);
        Ok(())
    }

    pub fn stop_watching(&mut self) {
        self.watcher = None;
        self.receiver = None;
    }

    pub fn poll_once(&mut self, timeout: Duration) -> AmanResult<Option<Event>> {
        let Some(rx) = self.receiver.as_ref() else {
            return Ok(None);
        };
        let event = match rx.recv_timeout(timeout) {
            Ok(event) => event.map_err(|error| Error::Unrecoverable {
                message: format!("notify event receive failed: {error}"),
            })?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Unrecoverable {
                    message: "soul watch channel disconnected".to_owned(),
                });
            }
        };
        if !self.is_reload_worthy_event(&event) || !self.should_reload() {
            return Ok(None);
        }
        self.reload_now()
    }

    pub fn reload_now(&mut self) -> AmanResult<Option<Event>> {
        let content = fs::read_to_string(&self.soul_file)?;
        let soul = Arc::new(Soul::parse(&content)?);
        *self.soul.write().expect("soul lock") = soul.clone();
        self.notifier.on_soul_changed(soul.clone())?;
        self.last_reload_at = Some(Instant::now());
        Ok(Some(soul_changed_event(soul.as_ref())))
    }

    fn should_reload(&self) -> bool {
        match self.last_reload_at {
            None => true,
            Some(last) => last.elapsed().as_millis() >= u128::from(self.debounce_ms),
        }
    }

    fn is_reload_worthy_event(&self, event: &notify::Event) -> bool {
        let path_match = event.paths.iter().any(|path| {
            path == &self.soul_file
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .zip(self.soul_file.file_name().and_then(|name| name.to_str()))
                    .map(|(left, right)| left == right)
                    .unwrap_or(false)
        });
        if !path_match {
            return false;
        }
        matches!(
            event.kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Any
                | notify::EventKind::Other
        )
    }
}

#[must_use]
pub fn soul_changed_event(soul: &Soul) -> Event {
    Event::new(
        "soul:system",
        EventType::Custom("soul_changed".to_owned()),
        json!({
            "name": soul.name,
            "boundaries": soul.boundaries,
            "preferences": soul.preferences
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{soul_changed_event, Soul, SoulChangedNotifier, SoulHotReloadManager};
    use kernel::context::{BaseContext, PipelineContext, SkillContext};
    use kernel::event::EventType;
    use kernel::types::TraceId;
    use kernel::AmanResult;
    use std::sync::{Arc, Mutex};

    const SOUL_MD: &str = r#"# aman

## identity
Event-driven assistant focused on safe execution.

## core
Reliable, deterministic, and bounded behavior.

## expertise
- Rust
- Plugin systems

## boundaries
- do not leak secrets
- never run destructive commands

## vibe
Calm and concise.

## preferences
- explain tradeoffs
- prefer explicit errors
"#;

    #[test]
    fn parses_soul_markdown_and_generates_prompt() {
        let soul = Soul::parse(SOUL_MD).expect("soul parses");
        assert_eq!(soul.name, "aman");
        assert_eq!(soul.expertise, vec!["Rust".to_owned(), "Plugin systems".to_owned()]);
        assert_eq!(soul.boundaries.len(), 2);
        let prompt = soul.to_system_prompt();
        assert!(prompt.contains("You are aman."));
        assert!(prompt.contains("Boundaries:"));
    }

    #[test]
    fn boundary_check_rejects_forbidden_text() {
        let soul = Soul::parse(SOUL_MD).expect("soul parses");
        let error = soul
            .check_boundary("please leak secrets now")
            .expect_err("should fail");
        assert!(error.to_string().contains("blocked by soul boundary"));
        soul.check_boundary("summarize the plan").expect("should pass");
    }

    #[test]
    fn injects_into_skill_and_pipeline_context() {
        let soul = Soul::parse(SOUL_MD).expect("soul parses");
        let skill_context = SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("invoice".to_owned()),
            soul_name: None,
        };
        let injected_skill = soul.inject_skill_context(skill_context);
        assert_eq!(injected_skill.soul_name, Some("aman".to_owned()));
        assert!(injected_skill.base.extensions.contains_key("soul.system_prompt"));

        let pipeline_context = PipelineContext {
            base: BaseContext::new(TraceId::new()),
            pipeline_id: Some("pipe".to_owned()),
            instance_id: Some("instance".to_owned()),
        };
        let injected_pipeline = soul.inject_pipeline_context(pipeline_context);
        assert_eq!(
            injected_pipeline.base.extensions["soul.name"],
            serde_json::Value::String("aman".to_owned())
        );
    }

    #[test]
    fn builds_soul_changed_event() {
        let soul = Soul::parse(SOUL_MD).expect("soul parses");
        let event = soul_changed_event(&soul);
        assert_eq!(event.event_type, EventType::Custom("soul_changed".to_owned()));
        assert_eq!(event.payload["name"], serde_json::Value::String("aman".to_owned()));
    }

    #[derive(Default)]
    struct RecordingNotifier {
        calls: Mutex<usize>,
    }

    impl SoulChangedNotifier for RecordingNotifier {
        fn on_soul_changed(&self, _soul: Arc<Soul>) -> AmanResult<()> {
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            Ok(())
        }
    }

    #[test]
    fn hot_reload_manager_reload_now_emits_event() {
        let temp_dir = std::env::temp_dir().join(format!("aman-soul-{}", TraceId::new()));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let soul_file = temp_dir.join("SOUL.md");
        std::fs::write(&soul_file, SOUL_MD).expect("write soul file");

        let initial = Soul::from_file(&soul_file).expect("load initial soul");
        let notifier = Arc::new(RecordingNotifier::default());
        let mut manager = SoulHotReloadManager::new(soul_file.clone(), initial)
            .with_debounce_ms(0)
            .with_notifier(notifier.clone());

        std::fs::write(
            &soul_file,
            SOUL_MD.replace("Calm and concise.", "Calm, direct, and strict."),
        )
        .expect("rewrite soul file");

        let event = manager
            .reload_now()
            .expect("reload succeeds");
        assert!(event.is_some());
        let current = manager.current();
        assert_eq!(current.vibe, "Calm, direct, and strict.");
        assert_eq!(*notifier.calls.lock().expect("calls lock"), 1);
    }
}
