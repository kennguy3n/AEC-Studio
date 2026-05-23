//! `aec_audit` — append-only audit log with BLAKE3 hash chaining.
//!
//! Each entry stores `{ts, actor, scope, tool, payload_hash, prev_hash, hash}`.
//! The log is persisted as JSON-lines under `audit/log.jsonl` in the project
//! package; replaying the file rebuilds the head hash deterministically.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_core::types::{Actor, CommandId, Scope};

pub type AuditResult<T> = std::result::Result<T, AuditError>;

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("audit chain mismatch at line {line}: expected prev_hash {expected}, found {found}")]
    ChainMismatch {
        line: usize,
        expected: String,
        found: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub command_id: CommandId,
    pub ts: DateTime<Utc>,
    pub scope: Scope,
    pub actor: Actor,
    pub tool: String,
    pub payload_hash: String,
    pub prev_hash: String,
    pub hash: String,
}

/// In-memory audit log with file-backed persistence.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    head: String,
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    pub const GENESIS: &'static str = "blake3:genesis";

    /// Open (or create) an audit log file. Existing entries are replayed to
    /// rebuild the head hash and verify the chain.
    pub fn open(path: impl AsRef<Path>) -> AuditResult<Self> {
        let path = path.as_ref().to_path_buf();
        let mut head = Self::GENESIS.to_string();
        let mut entries: Vec<AuditEntry> = Vec::new();
        if path.exists() {
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            for (idx, line) in reader.lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let entry: AuditEntry = serde_json::from_str(&line)?;
                if entry.prev_hash != head {
                    return Err(AuditError::ChainMismatch {
                        line: idx + 1,
                        expected: head,
                        found: entry.prev_hash,
                    });
                }
                head.clone_from(&entry.hash);
                entries.push(entry);
            }
        }
        Ok(Self {
            path,
            head,
            entries,
        })
    }

    pub fn head(&self) -> &str {
        &self.head
    }

    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Mirror the in-memory chain into the v2 `audit_chain` SQL table.
    /// Existing rows (matched by the `UNIQUE(hash)` column) are skipped
    /// via `ON CONFLICT(hash) DO NOTHING`, so callers can run this on
    /// every `append` without worrying about double-insert errors.
    ///
    /// Returns the number of newly-inserted rows. A return value of `0`
    /// means the SQL mirror is already up-to-date with the JSONL log.
    /// All inserts share a single transaction so a mid-walk failure
    /// leaves the SQL table untouched.
    pub fn mirror_to_sql(&self, conn: &mut Connection) -> AuditResult<usize> {
        let tx = conn.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO audit_chain (ts, actor, scope, tool, payload_hash, prev_hash, hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(hash) DO NOTHING",
            )?;
            for entry in &self.entries {
                let actor_json = serde_json::to_string(&entry.actor)?;
                let n = stmt.execute(params![
                    entry.ts.to_rfc3339(),
                    actor_json,
                    entry.scope.as_str(),
                    entry.tool,
                    entry.payload_hash,
                    entry.prev_hash,
                    entry.hash,
                ])?;
                inserted += n;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Append a new entry. The hash is computed as
    /// `BLAKE3(prev_hash || command_id || canonical_json(payload))`.
    pub fn append(
        &mut self,
        command_id: CommandId,
        scope: Scope,
        actor: Actor,
        tool: impl Into<String>,
        payload: &serde_json::Value,
    ) -> AuditResult<&AuditEntry> {
        let canonical = serde_json::to_vec(payload)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.head.as_bytes());
        hasher.update(command_id.as_str().as_bytes());
        hasher.update(&canonical);
        let payload_hash = format!("blake3:{}", blake3::hash(&canonical).to_hex());
        let hash = format!("blake3:{}", hasher.finalize().to_hex());
        let entry = AuditEntry {
            command_id,
            ts: Utc::now(),
            scope,
            actor,
            tool: tool.into(),
            payload_hash,
            prev_hash: std::mem::replace(&mut self.head, hash.clone()),
            hash,
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(&entry)?;
        writeln!(file, "{}", line)?;
        self.entries.push(entry);
        Ok(self.entries.last().expect("just pushed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::types::Actor;

    #[test]
    fn append_then_reopen_replays_chain() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let mut log = AuditLog::open(&log_path).unwrap();
        assert_eq!(log.head(), AuditLog::GENESIS);

        let h1 = log
            .append(
                CommandId::new(),
                Scope::Design,
                Actor::user(),
                "design.create_wall",
                &serde_json::json!({"x": 1}),
            )
            .unwrap()
            .hash
            .clone();
        let _ = log
            .append(
                CommandId::new(),
                Scope::Design,
                Actor::ai("style_assistant"),
                "design.paint_material",
                &serde_json::json!({"mat": "oak"}),
            )
            .unwrap();
        let head_before = log.head().to_string();

        drop(log);

        let reopened = AuditLog::open(&log_path).unwrap();
        assert_eq!(reopened.head(), head_before);
        assert_eq!(reopened.entries().len(), 2);
        assert_eq!(reopened.entries()[0].hash, h1);
        assert_eq!(reopened.entries()[1].prev_hash, h1);
    }

    #[test]
    fn mirror_to_sql_inserts_each_entry_once_and_is_idempotent() {
        use aec_core::db::open_encrypted;
        use aec_core::crypto::{derive_project_key, generate_project_nonce};

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let db_path = dir.path().join("project.sqlite");
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        let mut conn = open_encrypted(&db_path, &key).unwrap();

        let mut log = AuditLog::open(&log_path).unwrap();
        for (scope, tool, payload) in [
            (Scope::Design, "design.create_wall", serde_json::json!({"x": 1})),
            (Scope::Design, "design.paint_material", serde_json::json!({"mat": "oak"})),
            (Scope::Render, "render.queue", serde_json::json!({"job": 7})),
        ] {
            log.append(CommandId::new(), scope, Actor::user(), tool, &payload)
                .unwrap();
        }

        // First mirror: all 3 entries land.
        let n = log.mirror_to_sql(&mut conn).unwrap();
        assert_eq!(n, 3);
        // Second mirror with no new entries: zero inserts.
        let n2 = log.mirror_to_sql(&mut conn).unwrap();
        assert_eq!(n2, 0);

        // Counts match by scope.
        let render_n: i64 = conn
            .query_row(
                "SELECT count(*) FROM audit_chain WHERE scope = 'render'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(render_n, 1);
        let design_n: i64 = conn
            .query_row(
                "SELECT count(*) FROM audit_chain WHERE scope = 'design'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(design_n, 2);
        // Head hash is the last-inserted row's `hash`.
        let head_in_sql: String = conn
            .query_row(
                "SELECT hash FROM audit_chain ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(head_in_sql, log.head());

        // Append one more entry; subsequent mirror inserts only the new row.
        log.append(
            CommandId::new(),
            Scope::Deliver,
            Actor::user(),
            "deliver.export_pack",
            &serde_json::json!({"format": "zip"}),
        )
        .unwrap();
        let n3 = log.mirror_to_sql(&mut conn).unwrap();
        assert_eq!(n3, 1);
        let total: i64 = conn
            .query_row("SELECT count(*) FROM audit_chain", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 4);
    }

    #[test]
    fn tampered_line_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let mut log = AuditLog::open(&log_path).unwrap();
        log.append(
            CommandId::new(),
            Scope::Design,
            Actor::user(),
            "design.create_wall",
            &serde_json::json!({"x": 1}),
        )
        .unwrap();
        log.append(
            CommandId::new(),
            Scope::Design,
            Actor::user(),
            "design.delete_wall",
            &serde_json::json!({"x": 2}),
        )
        .unwrap();
        drop(log);

        // Tamper: change a hash in the middle of the file.
        let raw = std::fs::read_to_string(&log_path).unwrap();
        let tampered = raw.replace("\"hash\":\"blake3:", "\"hash\":\"blake3:dead");
        std::fs::write(&log_path, tampered).unwrap();

        let err = AuditLog::open(&log_path).unwrap_err();
        match err {
            AuditError::ChainMismatch { .. } => {}
            other => panic!("expected ChainMismatch, got {other:?}"),
        }
    }
}
