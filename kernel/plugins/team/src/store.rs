// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Team data store — SQLite-backed safety_log and context tables.
//!
//! Architecture ref: docs/team-architect.md §7

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// Persistent store for team-specific data.
///
/// Work items and stage history are managed by WorkflowEngine + StateStore,
/// so this only holds team-only tables: safety_log and context.
///
/// Uses `Arc` internally so it can be shared cheaply via `Clone`.
pub struct TeamStore {
    conn: std::sync::Arc<Mutex<Connection>>,
}

impl Clone for TeamStore {
    fn clone(&self) -> Self {
        Self {
            conn: std::sync::Arc::clone(&self.conn),
        }
    }
}

// ---------------------------------------------------------------------------
// SafetyLogEntry
// ---------------------------------------------------------------------------

/// A row in the safety_log table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyLogEntry {
    pub id: i64,
    pub work_item_id: String,
    pub agent_id: String,
    pub action: String,
    pub reason: SafetyGateReason,
    pub human_decision: Option<HumanDecision>,
    pub decided_by: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyGateReason {
    DangerousAction,
    LowConfidence,
    PermissionDenied,
}

impl SafetyGateReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DangerousAction => "dangerous_action",
            Self::LowConfidence => "low_confidence",
            Self::PermissionDenied => "permission_denied",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "dangerous_action" => Self::DangerousAction,
            "low_confidence" => Self::LowConfidence,
            _ => Self::PermissionDenied,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanDecision {
    Approved,
    Denied,
}

// ---------------------------------------------------------------------------
// ContextEntry
// ---------------------------------------------------------------------------

/// A row in the context table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub id: i64,
    pub title: String,
    pub file_path: String,
    pub content: String,
    pub category: String,
    pub updated_at: Option<String>,
    pub indexed_at: String,
}

// ---------------------------------------------------------------------------
// TeamStore
// ---------------------------------------------------------------------------

impl TeamStore {
    /// Open (or create) the team database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let conn =
            Connection::open(db_path).map_err(|e| format!("open team.db: {e}"))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS safety_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                action TEXT NOT NULL DEFAULT '',
                reason TEXT NOT NULL,
                human_decision TEXT,
                decided_by TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                resolved_at TEXT
            );

            CREATE TABLE IF NOT EXISTS context (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                file_path TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL DEFAULT '',
                category TEXT NOT NULL DEFAULT 'general',
                updated_at TEXT,
                indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_safety_log_work_item
                ON safety_log(work_item_id);
            CREATE INDEX IF NOT EXISTS idx_safety_log_pending
                ON safety_log(human_decision) WHERE human_decision IS NULL;
            CREATE INDEX IF NOT EXISTS idx_context_category
                ON context(category);",
        )
        .map_err(|e| format!("migrate team.db: {e}"))?;

        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
        })
    }

    // ------------------------------------------------------------------
    // Safety log
    // ------------------------------------------------------------------

    /// Insert a safety gate trigger event.
    pub fn insert_safety_log(
        &self,
        work_item_id: &str,
        agent_id: &str,
        action: &str,
        reason: SafetyGateReason,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        conn.execute(
            "INSERT INTO safety_log (work_item_id, agent_id, action, reason)
             VALUES (?1, ?2, ?3, ?4)",
            params![work_item_id, agent_id, action, reason.as_str()],
        )
        .map_err(|e| format!("insert safety_log: {e}"))?;
        Ok(conn.last_insert_rowid())
    }

    /// Resolve a pending safety gate.
    pub fn resolve_safety_log(
        &self,
        id: i64,
        decision: HumanDecision,
        decided_by: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let rows = conn
            .execute(
                "UPDATE safety_log
                 SET human_decision = ?1, decided_by = ?2, resolved_at = datetime('now')
                 WHERE id = ?3 AND human_decision IS NULL",
                params![
                    serde_json::to_string(&decision).unwrap_or_default(),
                    decided_by,
                    id
                ],
            )
            .map_err(|e| format!("resolve safety_log: {e}"))?;
        if rows == 0 {
            return Err(format!("safety_log {id} not found or already resolved"));
        }
        Ok(())
    }

    /// List pending (unresolved) safety gates.
    pub fn pending_safety_logs(&self) -> Result<Vec<SafetyLogEntry>, String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, work_item_id, agent_id, action, reason,
                        human_decision, decided_by, created_at, resolved_at
                 FROM safety_log
                 WHERE human_decision IS NULL
                 ORDER BY created_at DESC",
            )
            .map_err(|e| format!("query safety_log: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SafetyLogEntry {
                    id: row.get(0)?,
                    work_item_id: row.get(1)?,
                    agent_id: row.get(2)?,
                    action: row.get(3)?,
                    reason: SafetyGateReason::parse(&row.get::<_, String>(4)?),
                    human_decision: row
                        .get::<_, Option<String>>(5)?
                        .map(|s| serde_json::from_str(&s).unwrap()),
                    decided_by: row.get(6)?,
                    created_at: row.get(7)?,
                    resolved_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("map safety_log: {e}"))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("row: {e}"))?);
        }
        Ok(entries)
    }

    // ------------------------------------------------------------------
    // Context
    // ------------------------------------------------------------------

    /// Upsert a context entry (by file_path).
    pub fn upsert_context(
        &self,
        title: &str,
        file_path: &str,
        content: &str,
        category: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        conn.execute(
            "INSERT INTO context (title, file_path, content, category, updated_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))
             ON CONFLICT(file_path) DO UPDATE SET
                title = excluded.title,
                content = excluded.content,
                category = excluded.category,
                updated_at = datetime('now'),
                indexed_at = datetime('now')",
            params![title, file_path, content, category],
        )
        .map_err(|e| format!("upsert context: {e}"))?;
        Ok(())
    }

    /// List all context entries, optionally filtered by category.
    pub fn list_context(&self, category: Option<&str>) -> Result<Vec<ContextEntry>, String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let sql = if category.is_some() {
            "SELECT id, title, file_path, content, category, updated_at, indexed_at
             FROM context WHERE category = ?1 ORDER BY title"
        } else {
            "SELECT id, title, file_path, content, category, updated_at, indexed_at
             FROM context ORDER BY category, title"
        };

        let mut stmt = conn.prepare(sql).map_err(|e| format!("query context: {e}"))?;
        let rows = if let Some(cat) = category {
            stmt.query_map(params![cat], map_context_row)
        } else {
            stmt.query_map([], map_context_row)
        }
        .map_err(|e| format!("map context: {e}"))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("row: {e}"))?);
        }
        Ok(entries)
    }

    /// Get a single context entry by id.
    pub fn get_context(&self, id: i64) -> Result<Option<ContextEntry>, String> {
        let conn = self.conn.lock().map_err(|e| format!("lock: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, file_path, content, category, updated_at, indexed_at
                 FROM context WHERE id = ?1",
            )
            .map_err(|e| format!("query context: {e}"))?;

        let mut rows = stmt
            .query_map(params![id], map_context_row)
            .map_err(|e| format!("map context: {e}"))?;

        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| format!("row: {e}"))?)),
            None => Ok(None),
        }
    }
}

fn map_context_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ContextEntry> {
    Ok(ContextEntry {
        id: row.get(0)?,
        title: row.get(1)?,
        file_path: row.get(2)?,
        content: row.get(3)?,
        category: row.get(4)?,
        updated_at: row.get(5)?,
        indexed_at: row.get(6)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_and_insert_safety_log() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = TeamStore::open(&db_path).unwrap();

        let id = store
            .insert_safety_log("task-1", "coder", "rm -rf /tmp/build", SafetyGateReason::DangerousAction)
            .unwrap();
        assert!(id > 0);

        let pending = store.pending_safety_logs().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].work_item_id, "task-1");
        assert_eq!(pending[0].agent_id, "coder");
    }

    #[test]
    fn resolve_safety_log() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = TeamStore::open(&db_path).unwrap();

        let id = store
            .insert_safety_log("task-2", "reviewer", "deploy to prod", SafetyGateReason::DangerousAction)
            .unwrap();

        store.resolve_safety_log(id, HumanDecision::Approved, "jerin").unwrap();

        let pending = store.pending_safety_logs().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn upsert_and_list_context() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = TeamStore::open(&db_path).unwrap();

        store
            .upsert_context(
                "Architecture Overview",
                "docs/architecture.md",
                "# Architecture\n\nThis is the architecture doc.",
                "architecture",
            )
            .unwrap();

        let entries = store.list_context(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Architecture Overview");
        assert_eq!(entries[0].category, "architecture");

        // Upsert with same file_path should update
        store
            .upsert_context(
                "Architecture Overview v2",
                "docs/architecture.md",
                "# Architecture v2\n\nUpdated.",
                "architecture",
            )
            .unwrap();

        let entries = store.list_context(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Architecture Overview v2");
    }

    #[test]
    fn list_context_by_category() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = TeamStore::open(&db_path).unwrap();

        store.upsert_context("A", "a.md", "content a", "architecture").unwrap();
        store.upsert_context("B", "b.md", "content b", "standard").unwrap();

        let arch = store.list_context(Some("architecture")).unwrap();
        assert_eq!(arch.len(), 1);
        assert_eq!(arch[0].title, "A");
    }
}
