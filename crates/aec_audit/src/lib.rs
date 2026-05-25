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

/// Canonical serialisation of the immutable fields of an entry
/// — everything except `hash` itself. The order matches the
/// struct definition so the wire bytes are stable across runs.
#[derive(Debug, Serialize)]
struct AuditEntryCanonical<'a> {
    command_id: &'a CommandId,
    ts: String,
    scope: &'a Scope,
    actor: &'a Actor,
    tool: &'a str,
    payload_hash: &'a str,
    prev_hash: &'a str,
}

impl AuditEntry {
    /// Bytes that go into the `hash` chain computation. This is
    /// `canonical_json(all fields except hash)` — `verify_chain`
    /// re-derives the entry hash from these bytes and compares it
    /// to the stored `hash`.
    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        let c = AuditEntryCanonical {
            command_id: &self.command_id,
            ts: self.ts.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            scope: &self.scope,
            actor: &self.actor,
            tool: &self.tool,
            payload_hash: &self.payload_hash,
            prev_hash: &self.prev_hash,
        };
        serde_json::to_vec(&c).unwrap_or_default()
    }

    /// Re-derive the entry's BLAKE3 hash from its content.
    pub fn recompute_hash(&self) -> String {
        format!("blake3:{}", blake3::hash(&self.canonical_bytes()).to_hex())
    }
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
    ///
    /// The chain is append-only and the SQL mirror is a strict
    /// prefix of the JSONL log (we never INSERT out-of-order), so the
    /// number of rows already in `audit_chain` tells us exactly how
    /// many entries we can skip on the next call. Reading
    /// `count(*)` once at the top of the function is O(1) (SQLite
    /// keeps the row count for tables without WHERE-filtered
    /// indexes) and avoids sending thousands of no-op INSERTs on
    /// projects with long audit histories.
    ///
    /// As a defence-in-depth we still emit the INSERTs with
    /// `ON CONFLICT(hash) DO NOTHING` so a mismatched count (e.g.
    /// from a partially-applied rollback that left rows in the SQL
    /// table after a JSONL truncate) doesn't blow up the call — it
    /// just degenerates to per-row skipping.
    ///
    /// Returns the number of newly-inserted rows. A return value of `0`
    /// means the SQL mirror is already up-to-date with the JSONL log.
    /// All inserts share a single transaction so a mid-walk failure
    /// leaves the SQL table untouched.
    pub fn mirror_to_sql(&self, conn: &mut Connection) -> AuditResult<usize> {
        // The SQL mirror is a strict prefix of `self.entries` by
        // construction (insertions are ordered, never deleted, and
        // the UNIQUE(hash) guard makes any earlier mismatch surface
        // as a 0-row INSERT below rather than corrupting state).
        // Skipping the already-mirrored prefix is a constant-factor
        // speedup for long histories.
        let already_mirrored: i64 =
            conn.query_row("SELECT count(*) FROM audit_chain", [], |r| r.get(0))?;
        let skip = (already_mirrored as usize).min(self.entries.len());

        let tx = conn.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO audit_chain (ts, actor, scope, tool, payload_hash, prev_hash, hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(hash) DO NOTHING",
            )?;
            for entry in self.entries.iter().skip(skip) {
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
    /// `BLAKE3(canonical_json(entry_without_hash_field))`, where the
    /// entry's `prev_hash` field is the current chain head. Because
    /// every immutable field is folded into the hash, tampering with
    /// any field (`ts`, `scope`, `actor`, `tool`, `payload_hash`,
    /// `prev_hash`, or `command_id`) will be detected by
    /// [`verify_chain`].
    pub fn append(
        &mut self,
        command_id: CommandId,
        scope: Scope,
        actor: Actor,
        tool: impl Into<String>,
        payload: &serde_json::Value,
    ) -> AuditResult<&AuditEntry> {
        let canonical_payload = serde_json::to_vec(payload)?;
        let payload_hash = format!("blake3:{}", blake3::hash(&canonical_payload).to_hex());
        let prev_hash = self.head.clone();
        let mut entry = AuditEntry {
            command_id,
            ts: Utc::now(),
            scope,
            actor,
            tool: tool.into(),
            payload_hash,
            prev_hash,
            hash: String::new(),
        };
        entry.hash = entry.recompute_hash();
        self.head.clone_from(&entry.hash);
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

/// Outcome of [`verify_chain`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainVerification {
    /// Status — either `Ok` or the first detected break.
    pub status: ChainStatus,
    /// Number of entries that were fully validated before the first
    /// break (or all entries, if the chain is intact).
    pub entries_checked: u64,
    /// Files inspected in order.
    pub files_checked: Vec<PathBuf>,
    /// The latest valid `hash` head seen. For a fully-intact chain
    /// this equals the last entry's `hash`. For a broken chain this
    /// is the `hash` of the last entry that DID verify.
    pub head_hash: String,
}

impl ChainVerification {
    /// True iff the chain verified end-to-end with no breaks.
    pub fn is_ok(&self) -> bool {
        matches!(self.status, ChainStatus::Ok)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChainStatus {
    Ok,
    BrokenAt {
        file: PathBuf,
        /// 1-based line number within `file`.
        line: u64,
        reason: BreakReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BreakReason {
    /// `prev_hash` on the entry did not equal the running chain head.
    PrevHashMismatch { expected: String, found: String },
    /// `hash` on the entry did not equal the BLAKE3 re-derivation
    /// of its other fields. This catches tampering with any field
    /// other than `prev_hash`.
    HashRecomputeMismatch { stored: String, recomputed: String },
    /// The JSONL line could not be parsed as an `AuditEntry`.
    MalformedEntry { message: String },
    /// I/O error while reading the file.
    Io { message: String },
}

/// Verify the integrity of every `.jsonl` file in `audit_dir`,
/// walked in lexicographic order. The chain must:
///
/// 1. Begin with an entry whose `prev_hash` equals
///    [`AuditLog::GENESIS`].
/// 2. Have every entry's `prev_hash` equal to the previous entry's
///    `hash`.
/// 3. Have every entry's `hash` equal to BLAKE3 of its canonical
///    immutable fields (`command_id`, `ts`, `scope`, `actor`,
///    `tool`, `payload_hash`, `prev_hash`).
///
/// On the first violation, returns a `ChainVerification` with
/// `status = BrokenAt`. Otherwise the status is `Ok` and
/// `head_hash` is the final entry's `hash`. Reading the directory
/// itself failing (e.g. the path doesn't exist) is treated as an
/// I/O error.
pub fn verify_chain(audit_dir: &Path) -> AuditResult<ChainVerification> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(audit_dir)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    files.sort();

    let mut head = AuditLog::GENESIS.to_string();
    let mut entries_checked: u64 = 0;

    for file in &files {
        let f = match File::open(file) {
            Ok(f) => f,
            Err(e) => {
                return Ok(ChainVerification {
                    status: ChainStatus::BrokenAt {
                        file: file.clone(),
                        line: 0,
                        reason: BreakReason::Io {
                            message: e.to_string(),
                        },
                    },
                    entries_checked,
                    files_checked: files.clone(),
                    head_hash: head,
                });
            }
        };
        let reader = BufReader::new(f);
        for (idx, line) in reader.lines().enumerate() {
            let line_num = (idx + 1) as u64;
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    return Ok(ChainVerification {
                        status: ChainStatus::BrokenAt {
                            file: file.clone(),
                            line: line_num,
                            reason: BreakReason::Io {
                                message: e.to_string(),
                            },
                        },
                        entries_checked,
                        files_checked: files.clone(),
                        head_hash: head,
                    });
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let entry: AuditEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(e) => {
                    return Ok(ChainVerification {
                        status: ChainStatus::BrokenAt {
                            file: file.clone(),
                            line: line_num,
                            reason: BreakReason::MalformedEntry {
                                message: e.to_string(),
                            },
                        },
                        entries_checked,
                        files_checked: files.clone(),
                        head_hash: head,
                    });
                }
            };
            if entry.prev_hash != head {
                return Ok(ChainVerification {
                    status: ChainStatus::BrokenAt {
                        file: file.clone(),
                        line: line_num,
                        reason: BreakReason::PrevHashMismatch {
                            expected: head.clone(),
                            found: entry.prev_hash,
                        },
                    },
                    entries_checked,
                    files_checked: files.clone(),
                    head_hash: head,
                });
            }
            let recomputed = entry.recompute_hash();
            if recomputed != entry.hash {
                return Ok(ChainVerification {
                    status: ChainStatus::BrokenAt {
                        file: file.clone(),
                        line: line_num,
                        reason: BreakReason::HashRecomputeMismatch {
                            stored: entry.hash,
                            recomputed,
                        },
                    },
                    entries_checked,
                    files_checked: files.clone(),
                    head_hash: head,
                });
            }
            head = entry.hash;
            entries_checked += 1;
        }
    }

    Ok(ChainVerification {
        status: ChainStatus::Ok,
        entries_checked,
        files_checked: files,
        head_hash: head,
    })
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
        use aec_core::crypto::{derive_project_key, generate_project_nonce};
        use aec_core::db::open_encrypted;

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let db_path = dir.path().join("project.sqlite");
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        let mut conn = open_encrypted(&db_path, &key).unwrap();

        let mut log = AuditLog::open(&log_path).unwrap();
        for (scope, tool, payload) in [
            (
                Scope::Design,
                "design.create_wall",
                serde_json::json!({"x": 1}),
            ),
            (
                Scope::Design,
                "design.paint_material",
                serde_json::json!({"mat": "oak"}),
            ),
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
