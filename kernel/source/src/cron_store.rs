// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Per-agent cron job persistence.
//!
//! [`CronStore`] manages the `jobs.json` file under
//! `~/.aman/agents/{agent_key}/cron/`.  Every mutation goes through a
//! read-modify-write cycle so concurrent writers (same agent, different
//! gateway session) are not expected — the file is owned by the gateway
//! process for the lifetime of the cron job.

use crate::cron::{CronJobConfig, CronJobsFile};
use chrono::Utc;
use kernel::fs::atomic_write;
use kernel::{AmanResult, Error};
use std::path::PathBuf;

/// Manages the `jobs.json` file for one agent's cron jobs.
pub struct CronStore {
    dir: PathBuf,
}

impl CronStore {
    /// Create a store rooted at `dir` (the `cron/` subdirectory for a
    /// single agent).  The directory is created on first write; no I/O
    /// happens in the constructor.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Path to `{dir}/jobs.json`.
    #[must_use]
    pub fn jobs_path(&self) -> PathBuf {
        self.dir.join("jobs.json")
    }

    // ── Read ──────────────────────────────────────────────────────

    /// Load all jobs from disk.
    ///
    /// Returns an empty `Vec` when the file does not exist (not an
    /// error — a fresh agent simply has no cron jobs yet).
    pub async fn load(&self) -> AmanResult<Vec<CronJobConfig>> {
        let path = self.jobs_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&path).map_err(Error::Io)?;
        let file: CronJobsFile =
            serde_json::from_str(&raw).map_err(|error| Error::ConfigInvalid {
                message: format!("invalid cron jobs file {}: {error}", path.display()),
            })?;
        Ok(file.jobs)
    }

    // ── Write (full) ──────────────────────────────────────────────

    /// Atomically write the full job list to disk.
    pub async fn save(&self, jobs: &[CronJobConfig]) -> AmanResult<()> {
        let file = CronJobsFile {
            jobs: jobs.to_vec(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|error| {
            Error::Unrecoverable {
                message: format!("failed to serialize cron jobs: {error}"),
            }
        })?;
        let path = self.jobs_path();
        // Ensure the parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        atomic_write(&path, &json).map_err(Error::Io)?;
        Ok(())
    }

    // ── Convenience mutations (read-modify-write) ─────────────────

    /// Add a job, persisting immediately.
    pub async fn add(&self, job: &CronJobConfig) -> AmanResult<()> {
        let mut jobs = self.load().await?;
        // Replace if already exists (idempotent add).
        jobs.retain(|j| j.id != job.id);
        jobs.push(job.clone());
        self.save(&jobs).await
    }

    /// Update the expression and timezone of an existing job, then
    /// persist.  Returns `NotFound` if the job does not exist.
    pub async fn update(
        &self,
        id: &str,
        expression: &str,
        timezone: &str,
    ) -> AmanResult<()> {
        let mut jobs = self.load().await?;
        let job = jobs.iter_mut().find(|j| j.id == id).ok_or_else(|| {
            Error::NotFound {
                name: id.to_owned(),
            }
        })?;
        job.expression = expression.to_owned();
        job.timezone = timezone.to_owned();
        job.updated_at = Some(Utc::now().to_rfc3339());
        self.save(&jobs).await
    }

    /// Remove a job by id, persisting immediately.  Does nothing if
    /// the job does not exist.
    pub async fn remove(&self, id: &str) -> AmanResult<()> {
        let mut jobs = self.load().await?;
        jobs.retain(|j| j.id != id);
        self.save(&jobs).await
    }

    /// Record the outcome of the most recent run for a job.
    ///
    /// `status` should be `"ok"` or `"error"`.  `error` is an optional
    /// error message (only meaningful when status is `"error"`).
    pub async fn set_last_run(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> AmanResult<()> {
        let mut jobs = self.load().await?;
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.last_run_at = Some(Utc::now().to_rfc3339());
            job.last_status = Some(status.to_owned());
            job.last_error = error.map(|s| s.to_owned());
            self.save(&jobs).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir() -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!("aman_cron_store_test_{nonce}"))
    }

    fn sample_job(id: &str) -> CronJobConfig {
        CronJobConfig {
            id: id.to_owned(),
            name: None,
            expression: "*/5 * * * *".to_owned(),
            timezone: "UTC".to_owned(),
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            updated_at: None,
            last_run_at: None,
            last_status: None,
            last_error: None,
        }
    }

    #[tokio::test]
    async fn load_returns_empty_for_missing_file() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        let jobs = store.load().await.unwrap();
        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn add_and_load_roundtrip() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        let job = sample_job("test-add");
        store.add(&job).await.unwrap();

        let jobs = store.load().await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "test-add");
        assert_eq!(jobs[0].expression, "*/5 * * * *");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn add_idempotent_replaces() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());

        store.add(&sample_job("dup")).await.unwrap();
        let mut updated = sample_job("dup");
        updated.expression = "0 0 * * 0".to_owned();
        store.add(&updated).await.unwrap();

        let jobs = store.load().await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].expression, "0 0 * * 0");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn update_modifies_expression_and_timezone() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        store.add(&sample_job("update-me")).await.unwrap();

        store
            .update("update-me", "*/10 * * * *", "Asia/Shanghai")
            .await
            .unwrap();

        let jobs = store.load().await.unwrap();
        assert_eq!(jobs[0].expression, "*/10 * * * *");
        assert_eq!(jobs[0].timezone, "Asia/Shanghai");
        assert!(jobs[0].updated_at.is_some());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn update_nonexistent_returns_not_found() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        let err = store
            .update("nope", "* * * * *", "UTC")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }));
    }

    #[tokio::test]
    async fn remove_deletes_job() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        store.add(&sample_job("remove-me")).await.unwrap();
        store.add(&sample_job("keep-me")).await.unwrap();

        store.remove("remove-me").await.unwrap();

        let jobs = store.load().await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "keep-me");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn remove_nonexistent_is_noop() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        store.remove("nope").await.unwrap(); // no error
        let jobs = store.load().await.unwrap();
        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn set_last_run_updates_timestamps() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        store.add(&sample_job("run-me")).await.unwrap();

        store
            .set_last_run("run-me", "ok", None)
            .await
            .unwrap();

        let jobs = store.load().await.unwrap();
        assert_eq!(jobs[0].last_status.as_deref(), Some("ok"));
        assert!(jobs[0].last_run_at.is_some());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn set_last_run_with_error() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        store.add(&sample_job("err-job")).await.unwrap();

        store
            .set_last_run("err-job", "error", Some("timeout"))
            .await
            .unwrap();

        let jobs = store.load().await.unwrap();
        assert_eq!(jobs[0].last_status.as_deref(), Some("error"));
        assert_eq!(jobs[0].last_error.as_deref(), Some("timeout"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn set_last_run_nonexistent_is_noop() {
        let dir = temp_dir();
        let store = CronStore::new(dir.clone());
        store
            .set_last_run("nope", "ok", None)
            .await
            .unwrap(); // no error
    }
}
