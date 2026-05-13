use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{HealthStatus, SourceType, Timestamp};
use kernel::{AmanResult, Error};
use notify::{Config, Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckOpenFilesMode {
    Auto,
    True,
    #[default]
    False,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ForcePublishOnTimeout {
    #[default]
    MarkIncomplete,
    PublishAnyway,
    None,
}

#[derive(Debug, Clone)]
struct PendingFile {
    event_type: EventType,
    first_seen: Instant,
    stable_since: Instant,
    last_size: Option<u64>,
}

#[derive(Debug, Clone)]
struct FileSnapshot {
    modified_at: Option<SystemTime>,
    size: Option<u64>,
}

pub struct FileWatchSource {
    id: String,
    watch_paths: Vec<PathBuf>,
    debounce_ms: u64,
    max_stable_wait_ms: u64,
    check_open_files: CheckOpenFilesMode,
    force_publish_on_timeout: ForcePublishOnTimeout,
    initialized: bool,
    watcher: Option<RecommendedWatcher>,
    watcher_rx: Mutex<Option<Receiver<notify::Result<NotifyEvent>>>>,
    known_files: HashMap<PathBuf, FileSnapshot>,
    pending: HashMap<PathBuf, PendingFile>,
}

impl FileWatchSource {
    #[must_use]
    pub fn new(id: impl Into<String>, watch_paths: Vec<PathBuf>) -> Self {
        Self {
            id: id.into(),
            watch_paths,
            debounce_ms: 500,
            max_stable_wait_ms: 30_000,
            check_open_files: CheckOpenFilesMode::False,
            force_publish_on_timeout: ForcePublishOnTimeout::MarkIncomplete,
            initialized: false,
            watcher: None,
            watcher_rx: Mutex::new(None),
            known_files: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    fn map_notify_kind(kind: &EventKind) -> Option<EventType> {
        match kind {
            EventKind::Create(_) => Some(EventType::FileCreated),
            EventKind::Modify(_) => Some(EventType::FileChanged),
            EventKind::Remove(_) => Some(EventType::FileDeleted),
            _ => None,
        }
    }

    fn queue_notify_event(&mut self, event: NotifyEvent) {
        let Some(event_type) = Self::map_notify_kind(&event.kind) else {
            return;
        };
        for path in event.paths {
            if !self.should_track_path(path.as_path(), &event_type) {
                continue;
            }
            self.queue_path_event(path, event_type.clone());
        }
    }

    fn should_track_path(&self, path: &Path, event_type: &EventType) -> bool {
        match event_type {
            EventType::FileCreated | EventType::FileChanged => {
                std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
            }
            EventType::FileDeleted => self.known_files.contains_key(path),
            _ => true,
        }
    }

    fn snapshot_for(path: &Path) -> FileSnapshot {
        let metadata = std::fs::metadata(path).ok();
        FileSnapshot {
            modified_at: metadata.as_ref().and_then(|meta| meta.modified().ok()),
            size: metadata.as_ref().map(|meta| meta.len()),
        }
    }

    fn current_file_size(path: &Path) -> Option<u64> {
        std::fs::metadata(path).ok().map(|meta| meta.len())
    }

    fn scan_files(paths: &[PathBuf]) -> HashMap<PathBuf, FileSnapshot> {
        let mut files = HashMap::new();
        for root in paths {
            Self::walk_path(root, &mut files);
        }
        files
    }

    fn walk_path(path: &Path, out: &mut HashMap<PathBuf, FileSnapshot>) {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.is_file() {
                out.insert(path.to_path_buf(), Self::snapshot_for(path));
                return;
            }
            if meta.is_dir()
                && let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        Self::walk_path(entry.path().as_path(), out);
                    }
                }
        }
    }

    fn queue_path_event(&mut self, path: PathBuf, event_type: EventType) {
        let now = Instant::now();
        if let Some(pending) = self.pending.get_mut(&path) {
            pending.event_type = event_type;
            let latest_size = Self::current_file_size(path.as_path());
            if pending.last_size != latest_size {
                pending.last_size = latest_size;
                pending.stable_since = now;
            }
            return;
        }
        self.pending.insert(
            path.clone(),
            PendingFile {
                event_type,
                first_seen: now,
                stable_since: now,
                last_size: Self::current_file_size(path.as_path()),
            },
        );
    }

    fn should_check_open_files(&self) -> bool {
        match self.check_open_files {
            CheckOpenFilesMode::False => false,
            CheckOpenFilesMode::True => true,
            CheckOpenFilesMode::Auto => !self.is_remote_filesystem(),
        }
    }

    fn is_remote_filesystem(&self) -> bool {
        self.watch_paths
            .iter()
            .any(|path| Self::path_looks_remote(path))
    }

    fn path_looks_remote(path: &Path) -> bool {
        let text = path.to_string_lossy().to_lowercase();
        text.starts_with("//")
            || text.starts_with("\\\\")
            || text.starts_with("smb://")
            || text.starts_with("nfs://")
            || text.starts_with("/net/")
            || text.starts_with("/afs/")
            || text.starts_with("/volumes/")
            || text.starts_with("/run/user/")
            && text.contains("gvfs")
    }

    fn looks_open_or_locked(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .is_err()
    }

    fn build_event(&self, path: &Path, pending: &PendingFile, incomplete: bool) -> Event {
        let mut event = Event::new(
            self.id.clone(),
            pending.event_type.clone(),
            serde_json::json!({
                "path": path.to_string_lossy().to_string(),
                "incomplete": incomplete,
                "check_open_files": self.should_check_open_files(),
                "detected_at_ms": Timestamp::now().as_millis()
            }),
        );
        event.source = kernel::types::SourceId::new(self.id.clone());
        event
    }

    #[cfg(test)]
    fn inject_for_test(&mut self, path: PathBuf, event_type: EventType) {
        let now = Instant::now();
        self.pending.insert(
            path.clone(),
            PendingFile {
                event_type,
                first_seen: now,
                stable_since: now,
                last_size: Self::current_file_size(path.as_path()),
            },
        );
    }
}

#[async_trait::async_trait]
impl EventSource for FileWatchSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::File
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }
        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                let _ = tx.send(event);
            },
            Config::default(),
        )
        .map_err(|error| Error::Unrecoverable {
            message: format!("failed to initialize notify watcher: {error}"),
        })?;
        for path in &self.watch_paths {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|error| Error::Unrecoverable {
                    message: format!("failed to watch `{}`: {error}", path.display()),
                })?;
        }
        self.watcher = Some(watcher);
        *self
            .watcher_rx
            .lock()
            .expect("watcher receiver mutex should not be poisoned") = Some(rx);
        self.known_files = Self::scan_files(&self.watch_paths);
        self.initialized = true;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        self.watcher = None;
        *self
            .watcher_rx
            .lock()
            .expect("watcher receiver mutex should not be poisoned") = None;
        self.known_files.clear();
        self.pending.clear();
        self.initialized = false;
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized {
            return Ok(Vec::new());
        }

        let drained_notify_events = {
            let guard = self
                .watcher_rx
                .lock()
                .expect("watcher receiver mutex should not be poisoned");
            let mut drained = Vec::new();
            if let Some(rx) = guard.as_ref() {
                while let Ok(next) = rx.try_recv() {
                    drained.push(next);
                }
            }
            drained
        };
        for event in drained_notify_events.into_iter().flatten() {
            self.queue_notify_event(event);
        }

        let current_files = Self::scan_files(&self.watch_paths);
        let current_paths = current_files.keys().cloned().collect::<HashSet<_>>();
        let known_paths = self.known_files.keys().cloned().collect::<HashSet<_>>();

        for path in current_paths.difference(&known_paths) {
            self.queue_path_event(path.clone(), EventType::FileCreated);
        }
        for path in known_paths.difference(&current_paths) {
            self.queue_path_event(path.clone(), EventType::FileDeleted);
        }
        for path in current_paths.intersection(&known_paths) {
            let current = current_files.get(path).expect("path exists");
            let previous = self.known_files.get(path).expect("path exists");
            if current.modified_at != previous.modified_at || current.size != previous.size {
                self.queue_path_event(path.clone(), EventType::FileChanged);
            }
        }
        self.known_files = current_files;

        let mut ready_paths = Vec::new();
        let mut timeout_paths = Vec::new();
        let now = Instant::now();
        let debounce = Duration::from_millis(self.debounce_ms);
        let max_wait = Duration::from_millis(self.max_stable_wait_ms);

        for (path, pending) in &self.pending {
            let current_size = Self::current_file_size(path.as_path());
            if current_size != pending.last_size {
                continue;
            }
            if now.duration_since(pending.first_seen) >= max_wait {
                timeout_paths.push(path.clone());
                continue;
            }
            let stable_enough = now.duration_since(pending.stable_since) >= debounce;
            let blocked_by_lock_check =
                self.should_check_open_files() && Self::looks_open_or_locked(path.as_path());
            if stable_enough && !blocked_by_lock_check {
                ready_paths.push(path.clone());
            }
        }

        let mut events = Vec::new();
        for path in ready_paths {
            if let Some(pending) = self.pending.remove(&path) {
                events.push(self.build_event(path.as_path(), &pending, false));
            }
        }

        for path in timeout_paths {
            if let Some(pending) = self.pending.remove(&path) {
                match self.force_publish_on_timeout {
                    ForcePublishOnTimeout::MarkIncomplete => {
                        events.push(self.build_event(path.as_path(), &pending, true));
                    }
                    ForcePublishOnTimeout::PublishAnyway => {
                        events.push(self.build_event(path.as_path(), &pending, false));
                    }
                    ForcePublishOnTimeout::None => {}
                }
            }
        }

        Ok(events)
    }

    fn health(&self) -> HealthStatus {
        if self.initialized {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        }
    }

    async fn reconfigure(&mut self, config: Value) -> AmanResult<()> {
        if let Some(debounce_ms) = config.get("debounce_ms").and_then(Value::as_u64) {
            self.debounce_ms = debounce_ms;
        }
        if let Some(max_stable_wait_ms) = config.get("max_stable_wait_ms").and_then(Value::as_u64)
        {
            self.max_stable_wait_ms = max_stable_wait_ms;
        }
        if let Some(mode) = config.get("check_open_files").and_then(Value::as_str) {
            self.check_open_files = match mode {
                "auto" => CheckOpenFilesMode::Auto,
                "true" => CheckOpenFilesMode::True,
                _ => CheckOpenFilesMode::False,
            };
        }
        if let Some(mode) = config
            .get("force_publish_on_timeout")
            .and_then(Value::as_str)
        {
            self.force_publish_on_timeout = match mode {
                "publish_anyway" => ForcePublishOnTimeout::PublishAnyway,
                "none" => ForcePublishOnTimeout::None,
                _ => ForcePublishOnTimeout::MarkIncomplete,
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckOpenFilesMode, FileWatchSource, ForcePublishOnTimeout};
    use kernel::context::{BaseContext, SourceContext};
    use kernel::event::EventType;
    use kernel::source::EventSource;
    use kernel::types::TraceId;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::time::Duration;

    fn context() -> SourceContext {
        SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("watch:test".to_owned()),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aman-source-{name}-{nonce}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[tokio::test]
    async fn debounce_coalesces_rapid_writes() {
        let dir = temp_dir("debounce");
        let file = dir.join("note.txt");
        let mut source = FileWatchSource::new("watch:debounce", vec![dir.clone()]);
        source
            .reconfigure(serde_json::json!({
                "debounce_ms": 120,
                "max_stable_wait_ms": 2_000
            }))
            .await
            .expect("reconfigure");
        source.init(context()).await.expect("init");

        fs::write(&file, "a").expect("write a");
        fs::write(&file, "ab").expect("write ab");
        fs::write(&file, "abc").expect("write abc");

        let mut captured = Vec::new();
        for _ in 0..40 {
            let events = source.poll(&context()).await.expect("poll");
            if !events.is_empty() {
                captured.extend(events);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let target_events = captured
            .iter()
            .filter(|event| {
                event.payload.get("path")
                    == Some(&serde_json::Value::String(file.to_string_lossy().to_string()))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            target_events.len(),
            1,
            "debounce should emit a single event for target file"
        );
        assert!(
            matches!(
                target_events[0].event_type,
                EventType::FileCreated | EventType::FileChanged
            ),
            "first emission can be create or modify depending on platform ordering"
        );
    }

    #[tokio::test]
    async fn timeout_marks_incomplete_when_configured() {
        let dir = temp_dir("incomplete");
        let missing = dir.join("still-open.txt");
        let mut source = FileWatchSource::new("watch:incomplete", vec![dir]);
        source
            .reconfigure(serde_json::json!({
                "debounce_ms": 100,
                "max_stable_wait_ms": 5,
                "force_publish_on_timeout": "mark_incomplete"
            }))
            .await
            .expect("reconfigure");
        source.init(context()).await.expect("init");
        source.inject_for_test(missing.clone(), EventType::FileChanged);

        tokio::time::sleep(Duration::from_millis(10)).await;
        let events = source.poll(&context()).await.expect("poll");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].payload.get("incomplete"),
            Some(&serde_json::Value::Bool(true))
        );

        source
            .reconfigure(serde_json::json!({
                "force_publish_on_timeout": "none"
            }))
            .await
            .expect("reconfigure none");
        source.inject_for_test(missing, EventType::FileChanged);
        tokio::time::sleep(Duration::from_millis(10)).await;
        let events = source.poll(&context()).await.expect("poll");
        assert!(events.is_empty());

        // keeps variant used to avoid dead-code lint in test module.
        let _ = ForcePublishOnTimeout::PublishAnyway;
    }

    #[test]
    fn auto_mode_skips_open_file_check_for_remote_paths() {
        let mut source = FileWatchSource::new(
            "watch:auto-remote",
            vec![PathBuf::from("/Volumes/remote-share")],
        );
        source.check_open_files = CheckOpenFilesMode::Auto;
        assert!(!source.should_check_open_files());
    }

    #[test]
    fn auto_mode_keeps_open_file_check_for_local_paths() {
        let mut source = FileWatchSource::new("watch:auto-local", vec![PathBuf::from("/tmp")]);
        source.check_open_files = CheckOpenFilesMode::Auto;
        assert!(source.should_check_open_files());
    }
}
