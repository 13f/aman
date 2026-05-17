#![forbid(unsafe_code)]

//! SQLite-backed session index at `~/.aman/agents/{agent_key}/sessions.db`.
//!
//! Written by the gateway when sessions are closed; read by the Tauri
//! frontend to display the session list without relying on the gateway's
//! in-memory WorkflowEngine (which is lost on restart).

use kernel::AmanResult;
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
                session_type  = excluded.session_type",
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

        // 2. JSONL file(s): scan `{sessions_dir}/{yyyy-MM}/` for files
        //    whose name contains the session id.
        if self.sessions_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.sessions_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Ok(files) = std::fs::read_dir(&path) {
                            for file in files.flatten() {
                                let fp = file.path();
                                if fp.is_file()
                                    && fp.extension().map_or(false, |e| e == "jsonl")
                                {
                                    if let Some(name) = fp.file_stem().and_then(|n| n.to_str())
                                    {
                                        if name.ends_with(id) {
                                            let _ = std::fs::remove_file(&fp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(deleted)
    }

    /// List all sessions ordered by `last_active_at` descending.
    pub fn list_all(&self) -> AmanResult<Vec<SessionRecord>> {
        let db = self.db.lock().expect("session store lock");
        let mut stmt = db
            .prepare(
                "SELECT id, state, message_count, created_at, last_active_at, session_type
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
}
