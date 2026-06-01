// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{HealthStatus, SourceType};
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const DEFAULT_RATE_LIMIT_PER_SEC: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatchUpMode {
    Skip,
    Latest,
    All,
}

impl CatchUpMode {
    fn from_str(value: &str) -> Self {
        match value {
            "latest" => Self::Latest,
            "all" => Self::All,
            _ => Self::Skip,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Latest => "latest",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaylightSavingPolicy {
    Skip,
    RepeatOnce,
    WallClock,
}

impl DaylightSavingPolicy {
    fn from_str(value: &str) -> Self {
        match value {
            "repeat_once" => Self::RepeatOnce,
            "wall_clock" => Self::WallClock,
            _ => Self::Skip,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::RepeatOnce => "repeat_once",
            Self::WallClock => "wall_clock",
        }
    }
}

pub struct CronSource {
    id: String,
    expression: String,
    schedule: Schedule,
    timezone: Tz,
    catch_up: CatchUpMode,
    daylight_saving: DaylightSavingPolicy,
    rate_limit_per_sec: u32,
    rate_limit_overflow_delay: bool,
    leader_election: bool,
    is_leader: bool,
    next_run_at: Option<DateTime<Tz>>,
    pending_due: Vec<DateTime<Tz>>,
    initialized: bool,
    paused: bool,
}

impl CronSource {
    pub fn new(id: impl Into<String>, expression: impl Into<String>) -> AmanResult<Self> {
        let expression = expression.into();
        let normalized = normalize_expression(&expression)?;
        let schedule = Schedule::from_str(&normalized).map_err(|error| Error::ConfigInvalid {
            message: format!("invalid cron expression `{expression}`: {error}"),
        })?;
        let timezone = chrono_tz::UTC;
        Ok(Self {
            id: id.into(),
            expression,
            schedule,
            timezone,
            catch_up: CatchUpMode::Skip,
            daylight_saving: DaylightSavingPolicy::Skip,
            rate_limit_per_sec: DEFAULT_RATE_LIMIT_PER_SEC,
            rate_limit_overflow_delay: true,
            leader_election: false,
            is_leader: true,
            next_run_at: None,
            pending_due: Vec::new(),
            initialized: false,
            paused: false,
        })
    }

    fn next_after(&self, from: DateTime<Tz>) -> Option<DateTime<Tz>> {
        self.schedule.after(&from).next().map(|next| next.with_timezone(&self.timezone))
    }

    fn build_event(&self, scheduled_at: DateTime<Tz>) -> Event {
        Event::new(
            self.id.clone(),
            EventType::CronTick,
            serde_json::json!({
                "cron_expression": self.expression,
                "scheduled_at": scheduled_at.to_rfc3339(),
                "timezone": self.timezone.name(),
                "catch_up": self.catch_up.as_str(),
                "daylight_saving": self.daylight_saving.as_str(),
            }),
        )
    }

    fn collect_due(&mut self, now: DateTime<Tz>) -> Vec<DateTime<Tz>> {
        let mut due = Vec::new();
        while let Some(next) = self.next_run_at {
            if next > now {
                break;
            }
            due.push(next);
            self.next_run_at = self.next_after(next);
        }
        due
    }

    fn next_run_rfc3339(&self) -> Option<String> {
        self.next_run_at.map(|next| next.to_rfc3339())
    }

    fn apply_daylight_saving_policy(&self, due: Vec<DateTime<Tz>>) -> Vec<DateTime<Tz>> {
        if matches!(self.daylight_saving, DaylightSavingPolicy::RepeatOnce) {
            return due;
        }

        let mut selected_by_wall_time = HashMap::<String, DateTime<Tz>>::new();
        for scheduled_at in due {
            let key = scheduled_at.format("%Y-%m-%dT%H:%M:%S").to_string();
            match self.daylight_saving {
                DaylightSavingPolicy::Skip => {
                    selected_by_wall_time.entry(key).or_insert(scheduled_at);
                }
                DaylightSavingPolicy::WallClock => {
                    selected_by_wall_time.insert(key, scheduled_at);
                }
                DaylightSavingPolicy::RepeatOnce => {}
            }
        }

        let mut out = selected_by_wall_time.into_values().collect::<Vec<_>>();
        out.sort_unstable();
        out
    }

    fn override_spec(&self) -> CronOverrideSpec {
        CronOverrideSpec {
            id: self.id.clone(),
            expression: Some(self.expression.clone()),
            timezone: Some(self.timezone.name().to_owned()),
            catch_up: Some(self.catch_up.as_str().to_owned()),
            daylight_saving: Some(self.daylight_saving.as_str().to_owned()),
            rate_limit_per_sec: Some(self.rate_limit_per_sec),
            rate_limit_overflow: Some(if self.rate_limit_overflow_delay {
                "delay".to_owned()
            } else {
                "drop".to_owned()
            }),
            leader_election: Some(self.leader_election),
            removed: false,
        }
    }
}

fn normalize_expression(raw: &str) -> AmanResult<String> {
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    match fields.len() {
        5 => Ok(format!("0 {raw}")),
        6 => Ok(raw.to_owned()),
        _ => Err(Error::ConfigInvalid {
            message: format!(
                "cron expression must have 5 or 6 fields, got {}",
                fields.len()
            ),
        }),
    }
}

#[async_trait::async_trait]
impl EventSource for CronSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Timer
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }
        let now = Utc::now().with_timezone(&self.timezone);
        self.next_run_at = self.next_after(now);
        self.initialized = true;
        self.paused = false;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        self.initialized = false;
        self.paused = true;
        self.next_run_at = None;
        self.pending_due.clear();
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized || self.paused {
            return Ok(Vec::new());
        }
        if self.leader_election && !self.is_leader {
            return Ok(Vec::new());
        }
        if self.next_run_at.is_none() {
            self.next_run_at = self.next_after(Utc::now().with_timezone(&self.timezone));
        }

        let now = Utc::now().with_timezone(&self.timezone);
        let due = self.collect_due(now);
        let due = self.apply_daylight_saving_policy(due);
        let selected_due = match self.catch_up {
            CatchUpMode::All => due,
            CatchUpMode::Skip | CatchUpMode::Latest => due.last().copied().into_iter().collect(),
        };

        let mut queue = Vec::new();
        queue.append(&mut self.pending_due);
        queue.extend(selected_due);

        if matches!(self.catch_up, CatchUpMode::Skip | CatchUpMode::Latest) {
            queue = queue.last().copied().into_iter().collect();
        }

        if queue.is_empty() {
            return Ok(Vec::new());
        }

        let limit = self.rate_limit_per_sec.clamp(1, 100) as usize;
        let (to_emit, overflow) = if queue.len() > limit && self.rate_limit_overflow_delay {
            (queue[..limit].to_vec(), queue[limit..].to_vec())
        } else {
            (queue, Vec::new())
        };
        self.pending_due = overflow;

        Ok(to_emit
            .into_iter()
            .map(|scheduled_at| self.build_event(scheduled_at))
            .collect())
    }

    fn health(&self) -> HealthStatus {
        if self.initialized {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        }
    }

    async fn pause(&mut self) -> AmanResult<()> {
        self.paused = true;
        Ok(())
    }

    async fn resume(&mut self) -> AmanResult<()> {
        if self.initialized {
            self.paused = false;
        }
        Ok(())
    }

    async fn reconfigure(&mut self, config: Value) -> AmanResult<()> {
        if let Some(expression) = config.get("expression").and_then(Value::as_str) {
            let normalized = normalize_expression(expression)?;
            self.schedule = Schedule::from_str(&normalized).map_err(|error| Error::ConfigInvalid {
                message: format!("invalid cron expression `{expression}`: {error}"),
            })?;
            self.expression = expression.to_owned();
            self.next_run_at = self.next_after(Utc::now().with_timezone(&self.timezone));
        }
        if let Some(timezone) = config.get("timezone").and_then(Value::as_str) {
            self.timezone = timezone.parse::<Tz>().map_err(|error| Error::ConfigInvalid {
                message: format!("invalid timezone `{timezone}`: {error}"),
            })?;
            self.next_run_at = self.next_after(Utc::now().with_timezone(&self.timezone));
        }
        if let Some(catch_up) = config.get("catch_up").and_then(Value::as_str) {
            self.catch_up = CatchUpMode::from_str(catch_up);
        }
        if let Some(daylight_saving) = config.get("daylight_saving").and_then(Value::as_str) {
            self.daylight_saving = DaylightSavingPolicy::from_str(daylight_saving);
        }
        if let Some(rate_limit) = config.get("rate_limit_per_sec").and_then(Value::as_u64) {
            self.rate_limit_per_sec = rate_limit.clamp(1, 100) as u32;
        }
        if let Some(mode) = config
            .get("rate_limit_overflow")
            .and_then(Value::as_str)
        {
            self.rate_limit_overflow_delay = mode == "delay";
        }
        if let Some(enabled) = config.get("leader_election").and_then(Value::as_bool) {
            self.leader_election = enabled;
        }
        if let Some(is_leader) = config.get("is_leader").and_then(Value::as_bool) {
            self.is_leader = is_leader;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronJobInfo {
    pub id: String,
    pub expression: String,
    pub timezone: String,
    pub paused: bool,
    pub catch_up: String,
    pub daylight_saving: String,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CronAuditLog {
    pub old_interval: Option<String>,
    pub new_interval: Option<String>,
    pub caller: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CronOverrideSpec {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expression: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    catch_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daylight_saving: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit_per_sec: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit_overflow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    leader_election: Option<bool>,
    #[serde(default)]
    removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CronOverrideFile {
    jobs: Vec<CronOverrideSpec>,
}

pub struct CronManager {
    jobs: HashMap<String, CronSource>,
    runtime_dir: Option<PathBuf>,
    audit_logs: Vec<CronAuditLog>,
}

impl CronManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            runtime_dir: None,
            audit_logs: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_runtime_dir(runtime_dir: impl Into<PathBuf>) -> Self {
        Self {
            jobs: HashMap::new(),
            runtime_dir: Some(runtime_dir.into()),
            audit_logs: Vec::new(),
        }
    }

    fn override_path(&self) -> Option<PathBuf> {
        self.runtime_dir
            .as_ref()
            .map(|runtime_dir| runtime_dir.join("cron_override.yaml"))
    }

    fn audit_path(&self) -> Option<PathBuf> {
        self.runtime_dir
            .as_ref()
            .map(|runtime_dir| runtime_dir.join("cron_audit.yaml"))
    }

    fn load_override_specs(&self) -> AmanResult<HashMap<String, CronOverrideSpec>> {
        let Some(path) = self.override_path() else {
            return Ok(HashMap::new());
        };
        if !path.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(&path)?;
        let data: CronOverrideFile =
            serde_yaml::from_str(&content).map_err(|error| Error::ConfigInvalid {
                message: format!("invalid cron override file `{}`: {error}", path.display()),
            })?;

        Ok(data.jobs.into_iter().map(|job| (job.id.clone(), job)).collect())
    }

    fn persist_override_specs(&self, specs: &HashMap<String, CronOverrideSpec>) -> AmanResult<()> {
        let Some(path) = self.override_path() else {
            return Ok(());
        };
        let mut jobs = specs.values().cloned().collect::<Vec<_>>();
        jobs.sort_by(|left, right| left.id.cmp(&right.id));
        let content = serde_yaml::to_string(&CronOverrideFile { jobs }).map_err(|error| {
            Error::ConfigInvalid {
                message: format!("failed to serialize cron overrides: {error}"),
            }
        })?;
        write_atomically_with_fsync(&path, content.as_bytes())
    }

    fn persist_audit_logs(&self) -> AmanResult<()> {
        let Some(path) = self.audit_path() else {
            return Ok(());
        };
        let content = serde_yaml::to_string(&self.audit_logs).map_err(|error| Error::ConfigInvalid {
            message: format!("failed to serialize cron audit logs: {error}"),
        })?;
        write_atomically_with_fsync(&path, content.as_bytes())
    }

    fn append_audit_log(
        &mut self,
        old_interval: Option<String>,
        new_interval: Option<String>,
        caller: &str,
    ) -> AmanResult<()> {
        self.audit_logs.push(CronAuditLog {
            old_interval,
            new_interval,
            caller: caller.to_owned(),
            timestamp: Utc::now().to_rfc3339(),
        });
        self.persist_audit_logs()
    }

    pub fn remove_with_caller(&mut self, id: &str, caller: &str) -> AmanResult<()> {
        let old_interval = self.jobs.get(id).map(|source| source.expression.clone());
        self.jobs
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| Error::NotFound {
                name: id.to_owned(),
            })?;

        let mut specs = self.load_override_specs()?;
        specs.insert(
            id.to_owned(),
            CronOverrideSpec {
                id: id.to_owned(),
                expression: None,
                timezone: None,
                catch_up: None,
                daylight_saving: None,
                rate_limit_per_sec: None,
                rate_limit_overflow: None,
                leader_election: None,
                removed: true,
            },
        );
        self.persist_override_specs(&specs)?;
        self.append_audit_log(old_interval, None, caller)?;
        Ok(())
    }

    pub async fn add(&mut self, source: CronSource, ctx: SourceContext) -> AmanResult<()> {
        self.add_with_caller(source, ctx, "system").await
    }

    pub async fn add_with_caller(
        &mut self,
        mut source: CronSource,
        ctx: SourceContext,
        caller: &str,
    ) -> AmanResult<()> {
        let id = source.id.clone();
        if self.jobs.contains_key(&id) {
            return Err(Error::AlreadyExists { name: id });
        }
        source.init(ctx).await?;
        let spec = source.override_spec();
        self.jobs.insert(source.id.clone(), source);
        let mut specs = self.load_override_specs()?;
        specs.insert(id.clone(), spec);
        self.persist_override_specs(&specs)?;
        let new_interval = self.jobs.get(&id).map(|item| item.expression.clone());
        self.append_audit_log(None, new_interval, caller)?;
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> AmanResult<()> {
        self.remove_with_caller(id, "system")
    }

    pub async fn update(&mut self, id: &str, config: Value) -> AmanResult<()> {
        self.update_with_caller(id, config, "system").await
    }

    pub async fn update_with_caller(
        &mut self,
        id: &str,
        config: Value,
        caller: &str,
    ) -> AmanResult<()> {
        let (spec, old_interval, new_interval) = {
            let source = self.jobs.get_mut(id).ok_or_else(|| Error::NotFound {
                name: id.to_owned(),
            })?;
            let old_interval = Some(source.expression.clone());
            source.reconfigure(config).await?;
            let new_interval = Some(source.expression.clone());
            (source.override_spec(), old_interval, new_interval)
        };

        let mut specs = self.load_override_specs()?;
        specs.insert(id.to_owned(), spec);
        self.persist_override_specs(&specs)?;
        self.append_audit_log(old_interval, new_interval, caller)
    }

    pub async fn pause(&mut self, id: &str) -> AmanResult<()> {
        let source = self.jobs.get_mut(id).ok_or_else(|| Error::NotFound {
            name: id.to_owned(),
        })?;
        source.pause().await
    }

    pub async fn resume(&mut self, id: &str) -> AmanResult<()> {
        let source = self.jobs.get_mut(id).ok_or_else(|| Error::NotFound {
            name: id.to_owned(),
        })?;
        source.resume().await
    }

    #[must_use]
    pub fn list(&self) -> Vec<CronJobInfo> {
        self.jobs
            .values()
            .map(|source| CronJobInfo {
                id: source.id.clone(),
                expression: source.expression.clone(),
                timezone: source.timezone.name().to_owned(),
                paused: source.paused,
                catch_up: source.catch_up.as_str().to_owned(),
                daylight_saving: source.daylight_saving.as_str().to_owned(),
                next_run_at: source.next_run_rfc3339(),
            })
            .collect()
    }

    #[must_use]
    pub fn get_next_run(&self, id: &str) -> Option<String> {
        self.jobs.get(id).and_then(|source| source.next_run_rfc3339())
    }

    #[must_use]
    pub fn audit_logs(&self) -> &[CronAuditLog] {
        &self.audit_logs
    }
}

impl Default for CronManager {
    fn default() -> Self {
        Self::new()
    }
}

fn write_atomically_with_fsync(path: &Path, bytes: &[u8]) -> AmanResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        let dir = File::open(parent)?;
        dir.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
impl CronSource {
    fn force_due_count_for_test(&mut self, count: usize) {
        let now = Utc::now().with_timezone(&self.timezone);
        self.pending_due = std::iter::repeat_n(now, count).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::{CronManager, CronSource};
    use chrono::{LocalResult, NaiveDate, TimeZone};
    use kernel::context::{BaseContext, SourceContext};
    use kernel::event::EventType;
    use kernel::source::EventSource;
    use kernel::types::TraceId;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn context() -> SourceContext {
        SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("cron:test".to_owned()),
        }
    }

    fn temp_runtime_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aman-source-{label}-{nonce}"))
    }

    #[tokio::test]
    async fn accepts_five_field_expression() {
        let mut source = CronSource::new("cron:five", "*/5 * * * *").expect("create source");
        source.init(context()).await.expect("init");
    }

    #[tokio::test]
    async fn emits_tick_with_timezone_context() {
        let mut source = CronSource::new("cron:tz", "*/1 * * * * *").expect("create source");
        source
            .reconfigure(serde_json::json!({
                "timezone": "Asia/Shanghai",
                "catch_up": "latest"
            }))
            .await
            .expect("reconfigure");
        source.init(context()).await.expect("init");
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        let events = source.poll(&context()).await.expect("poll");
        assert!(!events.is_empty());
        assert_eq!(events[0].event_type, EventType::CronTick);
        assert_eq!(
            events[0].payload.get("timezone"),
            Some(&serde_json::Value::String("Asia/Shanghai".to_owned()))
        );
    }

    #[tokio::test]
    async fn rate_limit_delay_keeps_overflow_for_next_poll() {
        let mut source = CronSource::new("cron:limit", "*/1 * * * * *").expect("create source");
        source.init(context()).await.expect("init");
        source
            .reconfigure(serde_json::json!({
                "rate_limit_per_sec": 2,
                "rate_limit_overflow": "delay",
                "catch_up": "all"
            }))
            .await
            .expect("reconfigure");
        source.force_due_count_for_test(5);

        let first = source.poll(&context()).await.expect("first poll");
        assert_eq!(first.len(), 2);
        let second = source.poll(&context()).await.expect("second poll");
        assert_eq!(second.len(), 2);
        let third = source.poll(&context()).await.expect("third poll");
        assert_eq!(third.len(), 1);
    }

    #[tokio::test]
    async fn catch_up_latest_emits_single_event_when_backlogged() {
        let mut source = CronSource::new("cron:latest", "*/1 * * * * *").expect("create source");
        source.init(context()).await.expect("init");
        source
            .reconfigure(serde_json::json!({"catch_up": "latest"}))
            .await
            .expect("reconfigure");
        source.force_due_count_for_test(5);

        let events = source.poll(&context()).await.expect("poll");
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn leader_election_follower_does_not_emit() {
        let mut source = CronSource::new("cron:leader", "*/1 * * * * *").expect("create source");
        source.init(context()).await.expect("init");
        source
            .reconfigure(serde_json::json!({
                "leader_election": true,
                "is_leader": false
            }))
            .await
            .expect("reconfigure");
        source.force_due_count_for_test(3);
        let events = source.poll(&context()).await.expect("poll");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn cron_manager_add_update_pause_resume_and_next_run() {
        let mut manager = CronManager::new();
        manager
            .add(
                CronSource::new("cron:manager", "*/5 * * * *").expect("create source"),
                context(),
            )
            .await
            .expect("add");

        assert!(
            manager.get_next_run("cron:manager").is_some(),
            "next run should exist after init"
        );
        manager
            .update(
                "cron:manager",
                serde_json::json!({
                    "timezone": "Asia/Shanghai",
                    "catch_up": "all"
                }),
            )
            .await
            .expect("update");
        manager.pause("cron:manager").await.expect("pause");
        manager.resume("cron:manager").await.expect("resume");

        let list = manager.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].timezone, "Asia/Shanghai");
        assert_eq!(list[0].catch_up, "all");
    }

    #[tokio::test]
    async fn cron_manager_persists_override_file() {
        let runtime_dir = temp_runtime_dir("override");
        let mut manager = CronManager::with_runtime_dir(&runtime_dir);
        manager
            .add_with_caller(
                CronSource::new("cron:persist", "*/5 * * * *").expect("create source"),
                context(),
                "tester",
            )
            .await
            .expect("add");
        manager
            .update_with_caller(
                "cron:persist",
                serde_json::json!({
                    "expression": "*/10 * * * *",
                    "timezone": "UTC"
                }),
                "tester",
            )
            .await
            .expect("update");
        manager
            .remove_with_caller("cron:persist", "tester")
            .expect("remove");

        let override_file = runtime_dir.join("cron_override.yaml");
        let content = fs::read_to_string(&override_file).expect("read override file");
        assert!(content.contains("id: cron:persist"));
        assert!(content.contains("removed: true"));

        if runtime_dir.exists() {
            fs::remove_dir_all(&runtime_dir).expect("cleanup runtime dir");
        }
    }

    #[tokio::test]
    async fn cron_manager_records_audit_log_for_update() {
        let runtime_dir = temp_runtime_dir("audit");
        let mut manager = CronManager::with_runtime_dir(&runtime_dir);
        manager
            .add_with_caller(
                CronSource::new("cron:audit", "*/5 * * * *").expect("create source"),
                context(),
                "alice",
            )
            .await
            .expect("add");
        manager
            .update_with_caller(
                "cron:audit",
                serde_json::json!({"expression": "*/15 * * * *"}),
                "alice",
            )
            .await
            .expect("update");

        let audit = manager.audit_logs();
        assert!(
            audit.len() >= 2,
            "add + update should produce at least two audit entries"
        );
        let update_entry = audit.last().expect("latest audit entry");
        assert_eq!(update_entry.old_interval.as_deref(), Some("*/5 * * * *"));
        assert_eq!(update_entry.new_interval.as_deref(), Some("*/15 * * * *"));
        assert_eq!(update_entry.caller, "alice");
        assert!(
            !update_entry.timestamp.is_empty(),
            "timestamp should not be empty"
        );

        let audit_file = runtime_dir.join("cron_audit.yaml");
        let content = fs::read_to_string(&audit_file).expect("read audit file");
        assert!(content.contains("caller: alice"));

        if runtime_dir.exists() {
            fs::remove_dir_all(&runtime_dir).expect("cleanup runtime dir");
        }
    }

    fn ny_fall_back_ambiguous_pair() -> (chrono::DateTime<chrono_tz::Tz>, chrono::DateTime<chrono_tz::Tz>) {
        let ny = chrono_tz::America::New_York;
        let naive = NaiveDate::from_ymd_opt(2025, 11, 2)
            .expect("valid date")
            .and_hms_opt(1, 30, 0)
            .expect("valid time");
        match ny.from_local_datetime(&naive) {
            LocalResult::Ambiguous(first, second) => {
                if first < second {
                    (first, second)
                } else {
                    (second, first)
                }
            }
            _ => panic!("expected ambiguous wall-clock time"),
        }
    }

    #[tokio::test]
    async fn daylight_saving_skip_keeps_single_occurrence_on_fall_back() {
        let mut source = CronSource::new("cron:dst-skip", "*/1 * * * * *").expect("create source");
        source
            .reconfigure(serde_json::json!({
                "timezone": "America/New_York",
                "daylight_saving": "skip"
            }))
            .await
            .expect("reconfigure");
        let (first, second) = ny_fall_back_ambiguous_pair();
        let filtered = source.apply_daylight_saving_policy(vec![first, second]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], first);
    }

    #[tokio::test]
    async fn daylight_saving_repeat_once_keeps_both_occurrences_on_fall_back() {
        let mut source = CronSource::new("cron:dst-repeat", "*/1 * * * * *").expect("create source");
        source
            .reconfigure(serde_json::json!({
                "timezone": "America/New_York",
                "daylight_saving": "repeat_once"
            }))
            .await
            .expect("reconfigure");
        let (first, second) = ny_fall_back_ambiguous_pair();
        let filtered = source.apply_daylight_saving_policy(vec![first, second]);
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn daylight_saving_wall_clock_prefers_late_occurrence_on_fall_back() {
        let mut source = CronSource::new("cron:dst-wall", "*/1 * * * * *").expect("create source");
        source
            .reconfigure(serde_json::json!({
                "timezone": "America/New_York",
                "daylight_saving": "wall_clock"
            }))
            .await
            .expect("reconfigure");
        let (first, second) = ny_fall_back_ambiguous_pair();
        let filtered = source.apply_daylight_saving_policy(vec![first, second]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], second);
    }
}
