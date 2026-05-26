//! AI audit logger.
//!
//! Two artifacts per project, both under `<project>/audit/`:
//!
//! 1. **`ai_audit.jsonl`** — the tamper-evident hash chain. One
//!    `AuditEntry` per AI lifecycle event (plan / accept / reject).
//!    Carries only the payload *hash* (not the payload itself), so a
//!    chain verifier can detect tampering without reading the full
//!    record. This is the file [`aec_audit::AuditLog`] manages.
//!
//! 2. **`ai_audit_records.jsonl`** — the forensic companion. One
//!    full [`AiAuditRecord`] per event, including the rejection
//!    reason the renderer supplied. The companion path is derived
//!    from the chain path by appending `_records` to the file
//!    stem (see [`AiAuditLogger::open`]), so `ai_audit.jsonl`
//!    pairs with `ai_audit_records.jsonl`. This file is NOT
//!    hash-chained — the parallel `ai_audit.jsonl` entry's
//!    `payload_hash` IS the canonical hash of the same record,
//!    so any post-hoc tampering of `ai_audit_records.jsonl` is
//!    detectable by recomputing the BLAKE3 of the line and
//!    comparing against the chained entry.
//!
//! Splitting the two lets the chain stay small and constant-size
//! per entry (every line is the same shape: hashes + envelope)
//! while still preserving the full "why did the user reject this?"
//! detail a security reviewer needs.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use aec_audit::{AuditEntry, AuditError, AuditLog};
use aec_core::types::{Actor, CommandId, Scope};

use crate::diff_engine::{Diff, DiffStatus};
use crate::tool_schema::ToolName;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAuditRecord {
    pub tool: ToolName,
    pub scope: Scope,
    pub status: DiffStatus,
    pub diff_id: String,
    pub payload_hash: String,
    pub ts: DateTime<Utc>,
    /// Free-form reason supplied when a diff was rejected (or other
    /// renderer-supplied annotation). Empty string for acceptances.
    /// Skipped from serialisation when empty so legacy accept-only
    /// records on disk stay byte-identical.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Stable BLAKE3-hex hash of a [`Diff`]'s operations. Used as the
/// `payload_hash` field of every [`AiAuditRecord`] so a verifier
/// can detect tampering of the AI proposal after the fact (e.g. a
/// post-hoc edit of the diff before re-execution).
///
/// Returns just the hex digest (no `blake3:` prefix) so the field
/// matches the convention used by other hash sinks in the codebase.
pub fn diff_payload_hash(diff: &Diff) -> String {
    // Serialise the operations only — the `id`, `status`, and
    // `tool` are envelope metadata that move independently of the
    // proposal's content. Two diffs with the same operations should
    // hash to the same value regardless of `DiffId` (which is
    // generated per `DiffEngine::build` call).
    //
    // Devin Review `ANALYSIS_0006` (round 6): the previous
    // implementation used `unwrap_or_default()`, which would
    // silently substitute an empty byte vector if serialisation
    // ever failed. That collapses every "broken" diff to the same
    // hash, which is a denial-of-tampering-detection in the audit
    // chain (two distinct unserialisable diffs would carry the
    // same `payload_hash`, masking corruption). `DiffOperation`
    // derives `Serialize` and its fields are all JSON-safe types
    // (strings, numbers, vectors of same), so the failure path
    // is genuinely impossible — but we make the impossible failure
    // loud via `.expect` rather than silent via `unwrap_or_default`.
    let payload = serde_json::to_vec(&diff.operations)
        .expect("DiffOperation is always JSON-serialisable; this is an audit-chain invariant");
    blake3::hash(&payload).to_hex().to_string()
}

pub struct AiAuditLogger {
    log: AuditLog,
    records_path: PathBuf,
}

impl AiAuditLogger {
    /// Open (or create) the AI audit log at `chain_path`. The
    /// forensic companion file is derived by appending `_records`
    /// to the chain file's stem and keeping the same extension:
    /// e.g. `audit/ai_audit.jsonl` pairs with
    /// `audit/ai_audit_records.jsonl`. Both files are created
    /// lazily on first append.
    ///
    /// The derivation is deterministic and total: every path with
    /// a valid UTF-8 filename produces exactly one companion path
    /// (`{stem}_records.{ext}`, defaulting the extension to
    /// `jsonl` if the caller passed an extension-less path). Paths
    /// without a filename (e.g. `..`, root, or non-UTF-8 file
    /// stems) are rejected with `ErrorKind::InvalidInput` so a
    /// misconfigured call site surfaces loudly rather than
    /// silently producing a surprising companion name.
    ///
    /// Devin Review `ANALYSIS_0001` (PR #51): the previous shape
    /// had a fallback branch `chain_path.with_extension("records.jsonl")`
    /// that only ran when `file_stem` returned `None`, but it
    /// would have produced `ai_audit.records.jsonl` (note the
    /// dot) rather than the documented `ai_audit_records.jsonl`
    /// (note the underscore) — a silent inconsistency between
    /// the documented contract and the fallback behaviour. The
    /// new shape collapses both branches into a single canonical
    /// derivation and treats the no-filename case as an error
    /// rather than papering over it.
    pub fn open(chain_path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let chain_path = chain_path.as_ref();
        let stem = chain_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "AiAuditLogger::open: chain path {} has no UTF-8 file stem; expected something like `<dir>/ai_audit.jsonl`",
                        chain_path.display()
                    ),
                )
            })?;
        let ext = chain_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("jsonl");
        let mut records_path = chain_path.to_path_buf();
        records_path.set_file_name(format!("{stem}_records.{ext}"));
        Ok(Self {
            log: AuditLog::open(chain_path)?,
            records_path,
        })
    }

    /// Path of the forensic companion JSONL. Exposed for tests and
    /// for the chain-verification tool that needs to walk both
    /// files in lockstep.
    pub fn records_path(&self) -> &Path {
        &self.records_path
    }

    /// Append a new AI audit record. The underlying [`AuditLog`]
    /// hash-chains the payload (storing only the hash); this wrapper
    /// ALSO writes the full record to the forensic companion file
    /// so a reviewer can read the rejection reason / payload hash
    /// without needing the renderer running.
    ///
    /// Both writes happen inside the same call: the chain entry
    /// goes through `AuditLog::append` (which fsyncs to the chain
    /// file), and the records entry is appended to the records
    /// file. If the records-file write fails, the chain entry is
    /// already on disk and the next replay would detect the missing
    /// companion. We surface the records-file error so the renderer
    /// can re-issue the append if needed.
    pub fn append(&mut self, record: AiAuditRecord) -> Result<AuditEntry, AuditError> {
        let payload = serde_json::to_value(&record)?;
        let tool_label = format!("ai.{}", record.tool.as_str());
        let entry = self
            .log
            .append(
                CommandId::new(),
                record.scope,
                Actor::ai(record.tool.as_str()),
                tool_label,
                &payload,
            )?
            .clone();
        if let Some(parent) = self.records_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.records_path)?;
        let line = serde_json::to_string(&record)?;
        writeln!(file, "{}", line)?;
        Ok(entry)
    }

    /// Convenience: append an `Accepted` record for `diff` with the
    /// canonical `payload_hash` derived from the diff's operations.
    /// The caller controls the `scope` because not every AI tool
    /// targets Design (a future render-doctor tool would target
    /// Render scope).
    pub fn log_acceptance(&mut self, diff: &Diff, scope: Scope) -> Result<AuditEntry, AuditError> {
        self.append(AiAuditRecord {
            tool: diff.tool,
            scope,
            status: DiffStatus::Accepted,
            diff_id: diff.id.to_string(),
            payload_hash: diff_payload_hash(diff),
            ts: Utc::now(),
            reason: String::new(),
        })
    }

    /// Append a `Rejected` record for `diff` with the supplied
    /// `reason` (free-form, may be empty). Phase 11 task 11 — the
    /// AI provenance log captures both accepted and rejected
    /// proposals so auditors can compute "acceptance rate" without
    /// scraping every command's actor field.
    pub fn log_rejection(
        &mut self,
        diff: &Diff,
        scope: Scope,
        reason: &str,
    ) -> Result<AuditEntry, AuditError> {
        self.append(AiAuditRecord {
            tool: diff.tool,
            scope,
            status: DiffStatus::Rejected,
            diff_id: diff.id.to_string(),
            payload_hash: diff_payload_hash(diff),
            ts: Utc::now(),
            reason: reason.to_owned(),
        })
    }

    pub fn head(&self) -> &str {
        self.log.head()
    }

    pub fn entry_count(&self) -> usize {
        self.log.entries().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_changes_head_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai_audit.jsonl");
        let mut logger = AiAuditLogger::open(&path).unwrap();
        let head_before = logger.head().to_string();
        let record = AiAuditRecord {
            tool: ToolName::StyleAssistant,
            scope: Scope::Design,
            status: DiffStatus::Accepted,
            diff_id: "diff_abc".into(),
            payload_hash: "blake3:cafe".into(),
            ts: Utc::now(),
            reason: String::new(),
        };
        logger.append(record).unwrap();
        assert_ne!(logger.head(), head_before);
        assert_eq!(logger.entry_count(), 1);
    }

    #[test]
    fn log_rejection_records_reason_and_hash_changes() {
        use crate::{DiffEngine, PlanResponse};
        let plan = PlanResponse {
            tool: ToolName::StyleAssistant,
            raw_payload: "{}".into(),
            parsed: serde_json::json!({
                "furniture_ids": ["a"],
                "material_ids": [],
                "lighting_preset_id": "warm_evening",
            }),
            entities_modified: 2,
        };
        let diff = DiffEngine::build(&plan);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai_audit.jsonl");
        let mut logger = AiAuditLogger::open(&path).unwrap();
        let head0 = logger.head().to_string();
        let _entry = logger
            .log_rejection(&diff, Scope::Design, "wrong room")
            .unwrap();
        assert_ne!(logger.head(), head0);
        // The chained `ai_audit.jsonl` only carries the payload
        // *hash* (it is tamper-evident, not content-addressable);
        // the forensic reason lives in the companion
        // `ai_audit_records.jsonl`. Read both back and confirm the
        // reason is recoverable.
        let records = std::fs::read_to_string(logger.records_path()).unwrap();
        assert!(
            records.contains("wrong room"),
            "expected `wrong room` in ai_audit_records.jsonl, got: {records}"
        );
        // The chain file should NOT contain the reason text —
        // tamper-evidence is via the hash chain, not the text.
        let chain = std::fs::read_to_string(&path).unwrap();
        assert!(
            !chain.contains("wrong room"),
            "reason should NOT leak into the chained log, got: {chain}"
        );
    }

    #[test]
    fn log_acceptance_omits_reason_field_on_disk() {
        use crate::{DiffEngine, PlanResponse};
        let plan = PlanResponse {
            tool: ToolName::StyleAssistant,
            raw_payload: "{}".into(),
            parsed: serde_json::json!({
                "furniture_ids": ["a"],
                "material_ids": [],
                "lighting_preset_id": "warm_evening",
            }),
            entities_modified: 2,
        };
        let diff = DiffEngine::build(&plan);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai_audit.jsonl");
        let mut logger = AiAuditLogger::open(&path).unwrap();
        let _entry = logger.log_acceptance(&diff, Scope::Design).unwrap();
        // `reason` defaults to empty string and is skipped on
        // serialise so legacy accept-only records on disk stay
        // byte-identical to pre-task-11 behaviour.
        let records = std::fs::read_to_string(logger.records_path()).unwrap();
        assert!(
            !records.contains("\"reason\""),
            "empty reason must be skipped in ai_audit_records.jsonl, got: {records}"
        );
        let _chain = std::fs::read_to_string(&path).unwrap();
    }
}
