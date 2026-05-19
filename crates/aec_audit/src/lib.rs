//! `aec_audit` — append-only audit log with BLAKE3 hash chaining.
//!
//! Each entry stores `{ts, actor, scope, tool, payload_hash, prev_hash, hash}`.
//! The log is persisted as JSON-lines under `audit/log.jsonl` in the project
//! package; replaying the file rebuilds the head hash deterministically.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
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
    #[error("audit chain mismatch at line {line}: expected prev_hash {expected}, found {found}")]
    ChainMismatch { line: usize, expected: String, found: String },
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
                head = entry.hash.clone();
                entries.push(entry);
            }
        }
        Ok(Self { path, head, entries })
    }

    pub fn head(&self) -> &str {
        &self.head
    }

    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
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
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
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
