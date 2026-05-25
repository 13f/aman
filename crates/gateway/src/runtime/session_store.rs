#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


//! SQLite-backed session index at `~/.aman/agents/{agent_key}/sessions.db`.
//!
//! Written by the gateway when sessions are closed; read by the Tauri
//! frontend to display the session list without relying on the gateway's
//! in-memory WorkflowEngine (which is lost on restart).
//!
//! Session events (conversation messages) are persisted as JSONL files
//! under `sessions/{yyyy-MM}/{yyyy-MM-dd}-{id}.jsonl` so conversation
//! history survives gateway restarts.

use kernel::AmanResult;
use std::io::Write;
use std::path::Path;

/// A single session record in the index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub state: String,
    pub message_count: i64,
    pub created_at: i64,
    pub last_active_at: i64,
    pub session_type: String,
    /// When this session was last extracted by Reflection (millis since epoch).
    /// NULL means never reflected.
    #[serde(default)]
    pub reflected_at: Option<i64>,
}

/// Wraps a `rusqlite::Connection` to `sessions.db` plus the sessions
/// directory for JSONL file cleanup.
pub struct SessionStore {
    db: std::sync::Mutex<rusqlite::Connection>,
    /// e.g. `~/.aman/agents/{agent_key}/sessions`
    sessions_dir: std::path::PathBuf,
}

impl SessionStore {
    /// Open (or create) the sessions database at `db_path`.  `sessions_dir`
    /// is the base directory that contains `{yyyy-MM}/{yyyy-MM-dd}-{id}.jsonl`
    /// files — used when deleting a session to also clean up the JSONL artefact.
    pub fn open(db_path: &Path, sessions_dir: &Path) -> AmanResult<Self> {
        let db = rusqlite::Connection::open(db_path)
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store open: {e}") })?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id            TEXT PRIMARY KEY,
                state         TEXT NOT NULL DEFAULT 'active',
                message_count INTEGER NOT NULL DEFAULT 0,
                created_at    INTEGER NOT NULL,
                last_active_at INTEGER NOT NULL,
                session_type  TEXT DEFAULT 'persistent'
            );",
        )
        .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store schema: {e}") })?;
        // Migration: add reflected_at column (ignore error if already exists)
        let _ = db.execute("ALTER TABLE sessions ADD COLUMN reflected_at INTEGER", []);
        Ok(Self { db: std::sync::Mutex::new(db), sessions_dir: sessions_dir.to_owned() })
    }

    /// Insert or update a session record.
    pub fn upsert(&self, rec: &SessionRecord) -> AmanResult<()> {
        let db = self.db.lock().expect("session store lock");
        db.execute(
            "INSERT INTO sessions (id, state, message_count, created_at, last_active_at, session_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                state         = excluded.state,
                message_count = excluded.message_count,
                last_active_at = excluded.last_active_at,
                session_type  = excluded.session_type,
                reflected_at  = NULL",
            rusqlite::params![
                rec.id,
                rec.state,
                rec.message_count,
                rec.created_at,
                rec.last_active_at,
                rec.session_type,
            ],
        )
        .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store upsert: {e}") })?;
        Ok(())
    }

    /// Remove a session record by id and delete the corresponding JSONL file(s)
    /// from the sessions directory tree. Returns the number of DB rows deleted.
    pub fn delete(&self, id: &str) -> AmanResult<usize> {
        // 1. DB record
        let db = self.db.lock().expect("session store lock");
        let deleted = db
            .execute("DELETE FROM sessions WHERE id = ?1", [id])
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store delete: {e}") })?;
        drop(db);

        // 2. JSONL file: remove the session's event log
        let jsonl = self.jsonl_path(id);
        let _ = std::fs::remove_file(&jsonl);

        Ok(deleted)
    }

    /// List all sessions ordered by `last_active_at` descending.
    /// Check whether a session exists in the store.
    pub fn has_session(&self, id: &str) -> bool {
        let db = self.db.lock().expect("session store lock");
        db.query_row(
            "SELECT 1 FROM sessions WHERE id = ?1",
            rusqlite::params![id],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn list_all(&self) -> AmanResult<Vec<SessionRecord>> {
        let db = self.db.lock().expect("session store lock");
        let mut stmt = db
            .prepare(
                "SELECT id, state, message_count, created_at, last_active_at, session_type, reflected_at
                 FROM sessions ORDER BY last_active_at DESC",
            )
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store query: {e}") })?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SessionRecord {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    message_count: row.get(2)?,
                    created_at: row.get(3)?,
                    last_active_at: row.get(4)?,
                    session_type: row.get(5)?,
                    reflected_at: row.get(6)?,
                })
            })
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store rows: {e}") })?;
        let mut records: Vec<SessionRecord> = Vec::new();
        for row in rows {
            records.push(
                row.map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store row: {e}") })?,
            );
        }
        Ok(records)
    }

    /// Return the oldest unreflected session with at least one message, if any.
    pub fn list_unreflected(&self) -> AmanResult<Option<SessionRecord>> {
        let db = self.db.lock().expect("session store lock");
        let mut stmt = db
            .prepare(
                "SELECT id, state, message_count, created_at, last_active_at, session_type, reflected_at
                 FROM sessions
                 WHERE reflected_at IS NULL AND message_count > 0
                 ORDER BY last_active_at ASC
                 LIMIT 1",
            )
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store unreflected: {e}") })?;
        let mut rows = stmt
            .query_map([], |row| {
                Ok(SessionRecord {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    message_count: row.get(2)?,
                    created_at: row.get(3)?,
                    last_active_at: row.get(4)?,
                    session_type: row.get(5)?,
                    reflected_at: row.get(6)?,
                })
            })
            .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store unreflected rows: {e}") })?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| kernel::Error::ConfigInvalid {
                message: format!("session store unreflected row: {e}"),
            })?)),
            None => Ok(None),
        }
    }

    /// Mark a session as reflected at the given timestamp (millis since epoch).
    pub fn mark_reflected(&self, id: &str, timestamp_ms: i64) -> AmanResult<()> {
        let db = self.db.lock().expect("session store lock");
        db.execute(
            "UPDATE sessions SET reflected_at = ?1 WHERE id = ?2",
            rusqlite::params![timestamp_ms, id],
        )
        .map_err(|e| kernel::Error::ConfigInvalid { message: format!("session store mark_reflected: {e}") })?;
        Ok(())
    }

    /// Update a session's message_count from the actual event count in its JSONL file.
    /// Used to finalize explore/background pipeline sessions after all events are written.
    pub fn sync_message_count(&self, session_id: &str, now_ms: i64) -> AmanResult<()> {
        let count = self.load_session_events(session_id).len() as i64;
        let db = self.db.lock().expect("session store lock");
        db.execute(
            "UPDATE sessions SET message_count = ?1, last_active_at = ?2, reflected_at = NULL WHERE id = ?3",
            rusqlite::params![count, now_ms, session_id],
        )
        .map_err(|e| kernel::Error::ConfigInvalid {
            message: format!("session store sync_message_count: {e}"),
        })?;
        Ok(())
    }

    /// JSONL file path for a session's persisted events.
    fn jsonl_path(&self, session_id: &str) -> std::path::PathBuf {
        let _ = std::fs::create_dir_all(&self.sessions_dir);
        self.sessions_dir.join(format!("{session_id}.jsonl"))
    }

    /// Append a single event to the session's JSONL file.
    ///
    /// Each line is a JSON object with the fields needed for conversation
    /// history display: `event_type`, `timestamp_ms`, `payload`, etc.
    pub fn append_session_event(&self, session_id: &str, event: &serde_json::Value) -> AmanResult<()> {
        let path = self.jsonl_path(session_id);
        let line = serde_json::to_string(event)
            .map_err(|e| kernel::Error::ConfigInvalid {
                message: format!("serialize session event: {e}"),
            })?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| kernel::Error::ConfigInvalid {
                message: format!("open JSONL {path:?}: {e}"),
            })?;
        writeln!(file, "{line}").map_err(|e| kernel::Error::ConfigInvalid {
            message: format!("write JSONL {path:?}: {e}"),
        })?;
        Ok(())
    }

    /// Load all persisted events for a session from its JSONL file.
    ///
    /// Events are returned in insertion order (oldest first).
    pub fn load_session_events(&self, session_id: &str) -> Vec<serde_json::Value> {
        let path = self.jsonl_path(session_id);
        if !path.exists() {
            return Vec::new();
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect()
    }

    /// Load at most `max_events` most-recent events from the JSONL file.
    ///
    /// Returns events in insertion order (oldest first). For large session
    /// files this avoids loading thousands of events into memory.
    pub async fn load_recent_events(&self, session_id: &str, max_events: usize) -> Vec<serde_json::Value> {
        use std::collections::VecDeque;
        let path = self.jsonl_path(session_id);
        if !path.exists() {
            return Vec::new();
        }
        let Ok(content) = tokio::fs::read_to_string(&path).await else {
            return Vec::new();
        };
        let mut recent: VecDeque<serde_json::Value> = VecDeque::with_capacity(max_events);
        for line in content.lines() {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                if recent.len() >= max_events {
                    recent.pop_front();
                }
                recent.push_back(event);
            }
        }
        recent.into()
    }
}
