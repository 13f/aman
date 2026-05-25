// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! JSONL-backed [`TraceStore`] implementation.
//!
//! Each trace is persisted as a single JSON file under
//! `{data_dir}/traces/{trace_id}.json`. The directory listing serves as the
//! index — `load_recent` sorts by mtime so the newest traces are returned
//! first. This is intentionally simple; a SQLite index can be added later if
//! query patterns become more complex.

use async_trait::async_trait;
use kernel::trace::{
    DecisionPoint, TraceError, TraceOutcome, TraceRecord, TraceStatsSummary, TraceStore,
    ToolCallRecord,
};
use kernel::AmanResult;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub struct JsonlTraceStore {
    /// Per-agent directory containing trace JSON files.
    data_dir: PathBuf,
}

impl JsonlTraceStore {
    /// Open (or create) the trace store rooted at `data_dir`.
    /// Traces are stored under `data_dir/{agent_id}.traces/`.
    pub fn open(data_dir: &Path) -> AmanResult<Self> {
        fs::create_dir_all(data_dir)?;
        Ok(Self {
            data_dir: data_dir.to_owned(),
        })
    }

    // -- internal helpers --------------------------------------------------

    fn traces_dir(&self, agent_id: &str) -> PathBuf {
        self.data_dir.join(format!("{agent_id}.traces"))
    }

    fn ensure_traces_dir(&self, agent_id: &str) -> AmanResult<PathBuf> {
        let dir = self.traces_dir(agent_id);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Read a single trace file. Returns `None` if the file doesn't exist
    /// or is unparseable.
    fn read_trace_file(path: &Path) -> Option<TraceRecord> {
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// List all `.json` trace files in the agent's trace directory, sorted
    /// by modification time (newest first).
    fn list_trace_files(&self, agent_id: &str) -> AmanResult<Vec<PathBuf>> {
        let dir = self.traces_dir(agent_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<(u64, PathBuf)> = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                let mtime = entry
                    .metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                entries.push((mtime, path));
            }
        }
        // Sort descending by mtime
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(entries.into_iter().map(|(_, p)| p).collect())
    }

    /// Load all traces for an agent (newest first).
    fn load_all_traces(&self, agent_id: &str) -> AmanResult<Vec<TraceRecord>> {
        let files = self.list_trace_files(agent_id)?;
        let mut traces = Vec::with_capacity(files.len());
        for path in files {
            if let Some(t) = Self::read_trace_file(&path) {
                traces.push(t);
            }
        }
        Ok(traces)
    }

    /// Read-modify-write a single trace file atomically.
    ///
    /// The closure receives the current [`TraceRecord`] and should return the
    /// modified version (or `None` to abort the update). The trace is written
    /// atomically via temp → fsync → rename.
    fn update_trace<F>(&self, agent_id: &str, trace_id: &str, f: F) -> AmanResult<()>
    where
        F: FnOnce(&mut TraceRecord),
    {
        let dir = self.traces_dir(agent_id);
        let path = dir.join(format!("{trace_id}.json"));
        let mut trace = Self::read_trace_file(&path)
            .ok_or_else(|| kernel::Error::Unrecoverable {
                message: format!("TraceStore: trace not found: {trace_id}"),
            })?;
        f(&mut trace);

        let json = serde_json::to_string_pretty(&trace)?;
        let tmp = dir.join(format!(".tmp.{trace_id}.json"));
        {
            let mut fh = fs::File::create(&tmp)?;
            fh.write_all(json.as_bytes())?;
            fh.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[async_trait]
impl TraceStore for JsonlTraceStore {
    fn name(&self) -> &str {
        "jsonl"
    }

    // ── Phase A: basic CRUD ──────────────────────────────────────────────

    async fn save_trace(&self, trace: &TraceRecord) -> AmanResult<()> {
        let dir = self.ensure_traces_dir(&trace.agent_id)?;
        let path = dir.join(format!("{}.json", trace.trace_id));
        let json = serde_json::to_string_pretty(trace)?;

        let tmp = dir.join(format!(".tmp.{}.json", trace.trace_id));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;

        tracing::trace!(
            agent_id = %trace.agent_id,
            trace_id = %trace.trace_id,
            path = %path.display(),
            "TraceStore: saved trace",
        );
        Ok(())
    }

    async fn begin_trace(
        &self,
        agent_id: &str,
        session_id: Option<&str>,
        task_type: &str,
        description: &str,
        input: &str,
    ) -> AmanResult<String> {
        let dir = self.ensure_traces_dir(agent_id)?;
        let trace_id = uuid::Uuid::now_v7().to_string();
        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let trace = TraceRecord {
            trace_id: trace_id.clone(),
            agent_id: agent_id.to_owned(),
            session_id: session_id.map(|s| s.to_owned()),
            task_type: task_type.to_owned(),
            description: description.to_owned(),
            input: input.to_owned(),
            outcome: TraceOutcome::Partial,
            duration_ms: 0,
            decision_points: Vec::new(),
            tool_calls: Vec::new(),
            errors: Vec::new(),
            entities: Vec::new(),
            started_at_ms: now_ms,
            ended_at_ms: None,
        };

        let json = serde_json::to_string_pretty(&trace)?;
        let tmp = dir.join(format!(".tmp.{trace_id}.json"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &dir.join(format!("{trace_id}.json")))?;

        tracing::trace!(
            agent_id,
            trace_id = %trace_id,
            task_type,
            "TraceStore: begin trace",
        );
        Ok(trace_id)
    }

    async fn end_trace(
        &self,
        agent_id: &str,
        trace_id: &str,
        outcome: TraceOutcome,
        entities: &[String],
    ) -> AmanResult<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        self.update_trace(agent_id, trace_id, |t| {
            t.outcome = outcome;
            t.ended_at_ms = Some(now_ms);
            t.duration_ms = (now_ms - t.started_at_ms).max(0) as u64;
            t.entities = entities.to_vec();
        })?;

        tracing::trace!(
            agent_id,
            trace_id,
            ?outcome,
            duration_ms = now_ms,
            "TraceStore: end trace",
        );
        Ok(())
    }

    async fn load_recent(&self, agent_id: &str, count: usize) -> AmanResult<Vec<TraceRecord>> {
        let files = self.list_trace_files(agent_id)?;
        let limit = count.min(files.len());
        let mut traces = Vec::with_capacity(limit);
        for path in files.into_iter().take(limit) {
            if let Some(t) = Self::read_trace_file(&path) {
                traces.push(t);
            }
        }
        Ok(traces)
    }

    async fn load_by_session(
        &self,
        agent_id: &str,
        session_id: &str,
    ) -> AmanResult<Vec<TraceRecord>> {
        let all = self.load_all_traces(agent_id)?;
        Ok(all
            .into_iter()
            .filter(|t| t.session_id.as_deref() == Some(session_id))
            .collect())
    }

    async fn is_empty(&self, agent_id: &str) -> AmanResult<bool> {
        let dir = self.traces_dir(agent_id);
        if !dir.exists() {
            return Ok(true);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .map_or(false, |e| e == "json")
                && !entry
                    .file_name()
                    .to_str()
                    .map_or(false, |n| n.starts_with(".tmp."))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // ── Phase B: decision points + errors ────────────────────────────────

    async fn append_decision_point(
        &self,
        agent_id: &str,
        trace_id: &str,
        dp: &DecisionPoint,
    ) -> AmanResult<()> {
        let dp = dp.clone();
        self.update_trace(agent_id, trace_id, |t| {
            t.decision_points.push(dp);
        })
    }

    async fn append_error(
        &self,
        agent_id: &str,
        trace_id: &str,
        error: &TraceError,
    ) -> AmanResult<()> {
        let error = error.clone();
        self.update_trace(agent_id, trace_id, |t| {
            t.errors.push(error);
        })
    }

    async fn load_recent_errors(
        &self,
        agent_id: &str,
        count: usize,
    ) -> AmanResult<Vec<TraceRecord>> {
        let all = self.load_all_traces(agent_id)?;
        let mut error_traces: Vec<TraceRecord> = all
            .into_iter()
            .filter(|t| !t.errors.is_empty())
            .collect();
        error_traces.truncate(count);
        Ok(error_traces)
    }

    // ── Phase C: chain detection + tools ─────────────────────────────────

    async fn append_tool_call(
        &self,
        agent_id: &str,
        trace_id: &str,
        tc: &ToolCallRecord,
    ) -> AmanResult<()> {
        let tc = tc.clone();
        self.update_trace(agent_id, trace_id, |t| {
            t.tool_calls.push(tc);
        })
    }

    async fn find_incomplete(&self, agent_id: &str) -> AmanResult<Vec<TraceRecord>> {
        let all = self.load_all_traces(agent_id)?;
        Ok(all
            .into_iter()
            .filter(|t| t.outcome == TraceOutcome::Partial && t.ended_at_ms.is_none())
            .collect())
    }

    async fn detect_chains(
        &self,
        agent_id: &str,
    ) -> AmanResult<Vec<Vec<TraceRecord>>> {
        let incomplete = self.find_incomplete(agent_id).await?;
        if incomplete.is_empty() {
            return Ok(Vec::new());
        }

        // Group by session_id — traces sharing a session form a potential chain.
        // Traces without a session_id are treated as standalone.
        let mut session_groups: std::collections::HashMap<String, Vec<TraceRecord>> =
            std::collections::HashMap::new();
        let mut standalones: Vec<Vec<TraceRecord>> = Vec::new();

        for trace in incomplete {
            if let Some(ref sid) = trace.session_id {
                session_groups
                    .entry(sid.clone())
                    .or_default()
                    .push(trace);
            } else {
                standalones.push(vec![trace]);
            }
        }

        let mut chains: Vec<Vec<TraceRecord>> = session_groups.into_values().collect();
        chains.sort_by_key(|g| {
            g.iter()
                .map(|t| t.started_at_ms)
                .min()
                .unwrap_or(i64::MAX)
        });
        chains.extend(standalones);
        Ok(chains)
    }

    // ── Phase D: management ──────────────────────────────────────────────

    async fn count(&self, agent_id: &str) -> AmanResult<u64> {
        let files = self.list_trace_files(agent_id)?;
        Ok(files.len() as u64)
    }

    async fn list_all(&self, agent_id: &str) -> AmanResult<Vec<String>> {
        let files = self.list_trace_files(agent_id)?;
        Ok(files
            .into_iter()
            .filter_map(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_owned())
            })
            .collect())
    }

    async fn delete_trace(&self, agent_id: &str, trace_id: &str) -> AmanResult<bool> {
        let path = self.traces_dir(agent_id).join(format!("{trace_id}.json"));
        if path.exists() {
            fs::remove_file(&path)?;
            tracing::trace!(agent_id, trace_id, "TraceStore: deleted trace");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn stats_summary(&self, agent_id: &str) -> AmanResult<TraceStatsSummary> {
        let all = self.load_all_traces(agent_id)?;
        let mut summary = TraceStatsSummary {
            total_traces: all.len() as u64,
            total_agents: if all.is_empty() { 0 } else { 1 },
            ..Default::default()
        };
        for t in &all {
            match t.outcome {
                TraceOutcome::Success => summary.success_count += 1,
                TraceOutcome::Failure => summary.failure_count += 1,
                TraceOutcome::Partial => summary.partial_count += 1,
                TraceOutcome::Cancelled => summary.cancelled_count += 1,
            }
            summary.total_errors += t.errors.len() as u64;
            summary.total_tool_calls += t.tool_calls.len() as u64;
        }
        Ok(summary)
    }

    async fn prune(&self, agent_id: &str, older_than_secs: u64) -> AmanResult<u64> {
        let dir = self.traces_dir(agent_id);
        if !dir.exists() {
            return Ok(0);
        }
        let cutoff = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().saturating_sub(older_than_secs))
            .unwrap_or(0);
        let mut pruned = 0u64;

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json")
                && !entry
                    .file_name()
                    .to_str()
                    .map_or(false, |n| n.starts_with(".tmp."))
            {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if let Ok(secs) = mtime.duration_since(UNIX_EPOCH) {
                            if secs.as_secs() < cutoff {
                                let _ = fs::remove_file(&path);
                                pruned += 1;
                            }
                        }
                    }
                }
            }
        }

        if pruned > 0 {
            tracing::info!(
                agent_id,
                pruned,
                older_than_secs,
                "TraceStore: pruned old traces",
            );
        }
        Ok(pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::trace::{DecisionPoint, TraceError, TraceOutcome, TraceRecord};

    fn make_trace(agent_id: &str, trace_id: &str, outcome: TraceOutcome) -> TraceRecord {
        TraceRecord {
            trace_id: trace_id.to_owned(),
            agent_id: agent_id.to_owned(),
            session_id: None,
            task_type: "test_task".to_owned(),
            description: "test description".to_owned(),
            input: String::new(),
            outcome,
            duration_ms: 100,
            decision_points: vec![DecisionPoint {
                branch: "pick strategy".into(),
                taken: "fast path".into(),
                alternatives: vec!["slow path".into()],
                timestamp_ms: 1000,
            }],
            tool_calls: Vec::new(),
            errors: Vec::new(),
            entities: vec!["entity_a".into()],
            started_at_ms: 1000,
            ended_at_ms: Some(1100),
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aman_trace_test_{}",
            uuid::Uuid::now_v7()
        ));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn save_and_load_recent() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        let t1 = make_trace("agent1", "t1", TraceOutcome::Success);
        let t2 = make_trace("agent1", "t2", TraceOutcome::Failure);
        let t3 = make_trace("agent2", "t3", TraceOutcome::Success);

        pollster::block_on(store.save_trace(&t1)).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        pollster::block_on(store.save_trace(&t2)).unwrap();
        pollster::block_on(store.save_trace(&t3)).unwrap();

        let recent = pollster::block_on(store.load_recent("agent1", 10)).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].trace_id, "t2");
        assert_eq!(recent[1].trace_id, "t1");

        assert_eq!(
            pollster::block_on(store.load_recent("agent2", 10))
                .unwrap()
                .len(),
            1
        );

        assert!(pollster::block_on(store.load_recent("nobody", 10))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn begin_and_end_trace() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        // begin creates a Partial trace
        let trace_id = pollster::block_on(store.begin_trace(
            "agent1",
            Some("session-1"),
            "skill_run",
            "Run the build skill",
            "cargo build",
        ))
        .unwrap();
        assert!(!trace_id.is_empty());

        // trace should be findable as incomplete
        let incomplete = pollster::block_on(store.find_incomplete("agent1")).unwrap();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].trace_id, trace_id);
        assert_eq!(incomplete[0].outcome, TraceOutcome::Partial);
        assert_eq!(incomplete[0].session_id.as_deref(), Some("session-1"));

        // append decisions and errors
        let dp = DecisionPoint {
            branch: "build tool".into(),
            taken: "cargo".into(),
            alternatives: vec!["make".into()],
            timestamp_ms: 2000,
        };
        pollster::block_on(store.append_decision_point("agent1", &trace_id, &dp)).unwrap();

        let err = TraceError {
            error_type: "BuildError".into(),
            error_message: "missing dep".into(),
            recovery_action: Some("cargo update".into()),
            recovered: true,
        };
        pollster::block_on(store.append_error("agent1", &trace_id, &err)).unwrap();

        // end the trace
        pollster::block_on(store.end_trace(
            "agent1",
            &trace_id,
            TraceOutcome::Success,
            &["rust".into(), "cargo".into()],
        ))
        .unwrap();

        // load_recent should show the completed trace
        let recent = pollster::block_on(store.load_recent("agent1", 1)).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].trace_id, trace_id);
        assert_eq!(recent[0].outcome, TraceOutcome::Success);
        assert_eq!(recent[0].decision_points.len(), 1);
        assert_eq!(recent[0].errors.len(), 1);
        assert_eq!(recent[0].entities, vec!["rust", "cargo"]);
        assert!(recent[0].ended_at_ms.is_some());
        assert!(recent[0].duration_ms > 0);

        // no longer incomplete
        let incomplete = pollster::block_on(store.find_incomplete("agent1")).unwrap();
        assert!(incomplete.is_empty());
    }

    #[test]
    fn is_empty_and_count() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        assert!(pollster::block_on(store.is_empty("agent1")).unwrap());
        assert_eq!(pollster::block_on(store.count("agent1")).unwrap(), 0);

        pollster::block_on(store.save_trace(&make_trace("agent1", "t1", TraceOutcome::Success)))
            .unwrap();

        assert!(!pollster::block_on(store.is_empty("agent1")).unwrap());
        assert_eq!(pollster::block_on(store.count("agent1")).unwrap(), 1);
    }

    #[test]
    fn find_incomplete() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        let mut partial = make_trace("agent1", "p1", TraceOutcome::Partial);
        partial.ended_at_ms = None;
        let complete = make_trace("agent1", "c1", TraceOutcome::Success);

        pollster::block_on(store.save_trace(&partial)).unwrap();
        pollster::block_on(store.save_trace(&complete)).unwrap();

        let incomplete = pollster::block_on(store.find_incomplete("agent1")).unwrap();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].trace_id, "p1");
    }

    #[test]
    fn load_recent_errors() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        // Trace with error
        let mut err_trace = make_trace("agent1", "e1", TraceOutcome::Failure);
        err_trace.errors = vec![TraceError {
            error_type: "TimeoutError".into(),
            error_message: "request timed out".into(),
            recovery_action: None,
            recovered: false,
        }];
        pollster::block_on(store.save_trace(&err_trace)).unwrap();

        // Trace without error
        pollster::block_on(store.save_trace(&make_trace("agent1", "ok1", TraceOutcome::Success)))
            .unwrap();

        let errors = pollster::block_on(store.load_recent_errors("agent1", 10)).unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].trace_id, "e1");
        assert_eq!(errors[0].errors[0].error_type, "TimeoutError");
    }

    #[test]
    fn detect_chains() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        // Two incomplete traces in same session
        let mut t1 = make_trace("agent1", "c1", TraceOutcome::Partial);
        t1.session_id = Some("chain-session".into());
        t1.ended_at_ms = None;
        let mut t2 = make_trace("agent1", "c2", TraceOutcome::Partial);
        t2.session_id = Some("chain-session".into());
        t2.ended_at_ms = None;
        // Standalone incomplete
        let mut t3 = make_trace("agent1", "c3", TraceOutcome::Partial);
        t3.session_id = None;
        t3.ended_at_ms = None;

        pollster::block_on(store.save_trace(&t1)).unwrap();
        pollster::block_on(store.save_trace(&t2)).unwrap();
        pollster::block_on(store.save_trace(&t3)).unwrap();

        let chains = pollster::block_on(store.detect_chains("agent1")).unwrap();
        // Two chains: one with 2 traces in same session, one standalone
        assert_eq!(chains.len(), 2);
        let (paired, solo): (Vec<_>, Vec<_>) =
            chains.into_iter().partition(|c| c.len() > 1);
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].len(), 2);
        assert_eq!(solo.len(), 1);
        assert_eq!(solo[0][0].trace_id, "c3");
    }

    #[test]
    fn list_all_and_delete() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        pollster::block_on(store.save_trace(&make_trace("agent1", "a1", TraceOutcome::Success)))
            .unwrap();
        pollster::block_on(store.save_trace(&make_trace("agent1", "b2", TraceOutcome::Failure)))
            .unwrap();

        let ids = pollster::block_on(store.list_all("agent1")).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a1".to_owned()));
        assert!(ids.contains(&"b2".to_owned()));

        // Delete one
        assert!(pollster::block_on(store.delete_trace("agent1", "a1")).unwrap());
        assert!(!pollster::block_on(store.delete_trace("agent1", "a1")).unwrap()); // gone

        let ids = pollster::block_on(store.list_all("agent1")).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "b2");
    }

    #[test]
    fn stats_summary() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        pollster::block_on(store.save_trace(&make_trace("agent1", "s1", TraceOutcome::Success)))
            .unwrap();
        let mut fail = make_trace("agent1", "f1", TraceOutcome::Failure);
        fail.errors = vec![TraceError {
            error_type: "E1".into(),
            error_message: "m1".into(),
            recovery_action: None,
            recovered: false,
        }];
        pollster::block_on(store.save_trace(&fail)).unwrap();

        let summary = pollster::block_on(store.stats_summary("agent1")).unwrap();
        assert_eq!(summary.total_traces, 2);
        assert_eq!(summary.total_agents, 1);
        assert_eq!(summary.success_count, 1);
        assert_eq!(summary.failure_count, 1);
        assert_eq!(summary.partial_count, 0);
        assert_eq!(summary.total_errors, 1);
    }

    #[test]
    fn append_tool_call() {
        let tmp = temp_dir();
        let store = JsonlTraceStore::open(&tmp).unwrap();

        pollster::block_on(store.save_trace(&make_trace("agent1", "tc1", TraceOutcome::Partial)))
            .unwrap();

        let tc = ToolCallRecord {
            tool_name: "read_file".into(),
            input_summary: "path: src/main.rs".into(),
            output_summary: "200 lines".into(),
            duration_ms: 45,
            success: true,
        };
        pollster::block_on(store.append_tool_call("agent1", "tc1", &tc)).unwrap();

        let traces = pollster::block_on(store.load_recent("agent1", 1)).unwrap();
        assert_eq!(traces[0].tool_calls.len(), 1);
        assert_eq!(traces[0].tool_calls[0].tool_name, "read_file");
        assert!(traces[0].tool_calls[0].success);
    }
}
