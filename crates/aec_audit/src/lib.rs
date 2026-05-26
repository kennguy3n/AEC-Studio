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

/// Hash format used by [`AuditEntry`]. The field is encoded in
/// [`AuditEntryCanonical`] so any tampering with the version itself
/// (e.g. attempting to downgrade a v2 entry to v1 to skip the
/// recompute check) is detected by the v2 hash.
///
/// **Version 1** (legacy): the entry's `hash` was computed as
/// `BLAKE3(prev_hash_bytes || command_id_bytes || payload_bytes)`.
/// This formula needs the original *payload* bytes to verify, which
/// the entry does NOT persist (only `payload_hash` is stored). As a
/// result, [`verify_chain`] cannot recompute v1 hashes from the
/// stored entry alone — it falls back to linkage-only verification
/// (i.e. checks that `prev_hash` matches the previous entry's `hash`,
/// but cannot detect tampering of any other field).
///
/// **Version 2** (current): the entry's `hash` is
/// `BLAKE3(canonical_json(all_fields_except_hash))`. The canonical
/// serialisation includes `hash_version` itself, every immutable
/// field, and the running `prev_hash` head. [`verify_chain`]
/// recomputes this from the stored fields and detects tampering of
/// any of them.
pub const HASH_VERSION_LEGACY: u8 = 1;
pub const HASH_VERSION_CURRENT: u8 = 2;

fn default_hash_version_legacy() -> u8 {
    HASH_VERSION_LEGACY
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
    /// Hash format version. Entries written before the canonical-JSON
    /// hash format was introduced do not have this field on disk, so
    /// `serde` populates it with [`HASH_VERSION_LEGACY`] (1) via
    /// `#[serde(default)]`. New entries written through
    /// [`AuditLog::append`] always set this to [`HASH_VERSION_CURRENT`].
    #[serde(default = "default_hash_version_legacy")]
    pub hash_version: u8,
}

/// Canonical serialisation of the immutable fields of an entry
/// — everything except `hash` itself. The order matches the
/// struct definition so the wire bytes are stable across runs.
///
/// `hash_version` is included so that flipping the version field
/// on an *otherwise-untouched* v2 entry (e.g. an attempt to mark a
/// v2 entry as v1 in order to skip the content recompute) is caught
/// by the v2 BLAKE3 hash check on that same entry: the stored
/// `hash` was computed over `hash_version: 2`, so reading the entry
/// as v2 with the version flipped to 1 first hits the `Current` arm
/// of [`AuditEntry::recompute_hash`] and surfaces a
/// [`BreakReason::HashRecomputeMismatch`].
///
/// **Important — this is NOT a defence against arbitrary v1
/// forgery.** If an attacker controls a JSONL file, they can write a
/// fresh entry whose `hash_version` is [`HASH_VERSION_LEGACY`] with
/// arbitrary `tool` / `actor` / `scope` / `payload_hash` content,
/// then set `prev_hash` to chain correctly to the surrounding
/// entries. [`verify_chain`] cannot recompute the v1 hash (the v1
/// algorithm needs the original payload bytes, which are not
/// persisted on the entry), so the entry is accepted with
/// linkage-only verification and only counted via
/// `entries_legacy_linkage_only` in the report. For projects that
/// have never used the v1 hash algorithm (i.e. every entry was
/// written by [`AuditLog::append`] in this codebase, which always
/// sets [`HASH_VERSION_CURRENT`]), callers should use
/// [`verify_chain_with`] with [`VerifyOptions::strict_v2_only`] to
/// reject any v1 entry as tampering.
#[derive(Debug, Serialize)]
struct AuditEntryCanonical<'a> {
    command_id: &'a CommandId,
    ts: String,
    scope: &'a Scope,
    actor: &'a Actor,
    tool: &'a str,
    payload_hash: &'a str,
    prev_hash: &'a str,
    hash_version: u8,
}

impl AuditEntry {
    /// Bytes that go into the `hash` chain computation for v2+
    /// entries. This is `canonical_json(all fields except hash)`.
    /// [`verify_chain`] re-derives the entry hash from these bytes
    /// and compares it to the stored `hash`.
    ///
    /// `to_rfc3339_opts(SecondsFormat::Nanos, true)` is used (always
    /// 9 fractional digits) so the wire bytes are stable regardless
    /// of the `DateTime`'s sub-second precision. This is safe because
    /// `canonical_bytes` always operates on the in-memory `DateTime`
    /// value, not a JSONL-decoded string — chrono's serde round-trip
    /// preserves the `DateTime` losslessly.
    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        let c = AuditEntryCanonical {
            command_id: &self.command_id,
            ts: self.ts.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            scope: &self.scope,
            actor: &self.actor,
            tool: &self.tool,
            payload_hash: &self.payload_hash,
            prev_hash: &self.prev_hash,
            hash_version: self.hash_version,
        };
        // All fields of `AuditEntryCanonical` are owned or referenced
        // primitives / strings with infallible `Serialize` impls; the
        // only way `to_vec` can fail is `Serializer::serialize_*`
        // returning an error from a fallible custom impl, which none
        // of these types have. `.expect` rather than
        // `.unwrap_or_default()` so any future field that breaks this
        // invariant fails loudly during testing instead of silently
        // producing `blake3(empty)` — which would be a critical
        // integrity failure (every entry would hash identically).
        serde_json::to_vec(&c).expect("AuditEntryCanonical serialises infallibly")
    }

    /// Re-derive the entry's BLAKE3 hash from its content for v2+
    /// entries. Returns:
    ///
    /// * [`HashRecompute::LegacyLinkageOnly`] for v1 entries: the v1
    ///   algorithm hashes the *raw* payload (not the payload hash),
    ///   and the payload itself isn't persisted on the entry, so the
    ///   v1 hash cannot be reconstructed from a stored entry alone.
    ///   [`verify_chain`] falls back to linkage-only verification.
    /// * [`HashRecompute::Current`] holding the recomputed hash for
    ///   v2 entries. [`verify_chain`] compares this to the stored
    ///   `hash` and surfaces a mismatch as
    ///   [`BreakReason::HashRecomputeMismatch`].
    /// * [`HashRecompute::Unsupported`] for any version this build
    ///   does not understand (e.g. a future v3 written by a newer
    ///   build). [`verify_chain`] surfaces this as
    ///   [`BreakReason::UnsupportedHashVersion`] rather than silently
    ///   treating it as legacy — if we can't validate the entry, the
    ///   audit log integrity is unknown, not OK.
    pub fn recompute_hash(&self) -> HashRecompute {
        match self.hash_version {
            HASH_VERSION_LEGACY => HashRecompute::LegacyLinkageOnly,
            HASH_VERSION_CURRENT => HashRecompute::Current(format!(
                "blake3:{}",
                blake3::hash(&self.canonical_bytes()).to_hex()
            )),
            _ => HashRecompute::Unsupported,
        }
    }
}

/// Outcome of [`AuditEntry::recompute_hash`]. The three arms
/// correspond directly to the three possible cases
/// [`verify_chain`] must handle per [`HASH_VERSION_LEGACY`] /
/// [`HASH_VERSION_CURRENT`] / unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashRecompute {
    /// v1 entry — hash cannot be recomputed without the original
    /// payload bytes, so only the `prev_hash` linkage is checked.
    LegacyLinkageOnly,
    /// v2 entry — BLAKE3 over canonical JSON of every immutable
    /// field. Compare this value to `entry.hash`.
    Current(String),
    /// Unknown version — [`verify_chain`] cannot validate this entry
    /// and surfaces [`BreakReason::UnsupportedHashVersion`].
    Unsupported,
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
    /// `BLAKE3(canonical_json(entry_without_hash_field))` per
    /// [`HASH_VERSION_CURRENT`], where the entry's `prev_hash` field
    /// is the current chain head. Because every immutable field
    /// (including `hash_version`) is folded into the hash, tampering
    /// with any field will be detected by [`verify_chain`].
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
            hash_version: HASH_VERSION_CURRENT,
        };
        entry.hash = match entry.recompute_hash() {
            HashRecompute::Current(h) => h,
            // `hash_version: HASH_VERSION_CURRENT` is set immediately
            // above; reaching anything else here would mean the
            // version constants drifted from `recompute_hash`'s match
            // arms. Fail loud rather than write an entry whose hash
            // we can't verify later.
            HashRecompute::LegacyLinkageOnly => {
                unreachable!("append writes HASH_VERSION_CURRENT; recompute_hash returned legacy")
            }
            HashRecompute::Unsupported => {
                unreachable!(
                    "append writes HASH_VERSION_CURRENT; recompute_hash returned unsupported"
                )
            }
        };
        // Write the entry to disk BEFORE mutating in-memory state.
        // If any I/O step fails (parent dir create, open, writeln, or
        // the implicit flush on drop), `self.head` and `self.entries`
        // are left untouched — on retry the next `append` will
        // compute the same `prev_hash` and the same `hash`, producing
        // a bit-identical line. Updating `self.head` first would
        // leave the in-memory chain ahead of the on-disk chain on
        // failure: a re-`open` would replay only the entries that
        // made it to disk and rebuild a *different* head than the
        // one this process holds, silently breaking the chain across
        // a process restart.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(&entry)?;
        writeln!(file, "{}", line)?;
        // `writeln!` on `File` flushes to the OS write() syscall
        // (no userspace BufWriter), so a successful return here
        // means a subsequent `File::open` + read in this process or
        // any other will see the entry. Durability across a system
        // crash is *not* guaranteed without an `fsync`, which we
        // skip deliberately — audit append is on the hot path of
        // every command apply, and an `fsync` per command would cap
        // throughput at storage commit latency. A power-loss
        // scenario can therefore lose the last few entries; that's
        // acceptable for an append-only audit log (the chain still
        // verifies against the prefix that did flush).
        self.head.clone_from(&entry.hash);
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
    /// Subset of `entries_checked` that were verified with
    /// linkage-only checks because their `hash_version` is
    /// [`HASH_VERSION_LEGACY`] — i.e. the v1 hash algorithm needs the
    /// original payload bytes (which are not persisted in the entry),
    /// so the entry's `hash` cannot be recomputed and only the
    /// `prev_hash` linkage is verified. The chain still reports `Ok`
    /// in this case; operators who want stronger guarantees should
    /// regenerate the audit log on the current `HASH_VERSION_CURRENT`
    /// algorithm or use this counter to gate downstream trust.
    pub entries_legacy_linkage_only: u64,
    /// Files that were actually opened and inspected, in the order
    /// they were walked. On a successful verification this includes
    /// every `.jsonl` under `audit_dir`; on an early break this only
    /// includes the files up to and including the one in which the
    /// break occurred. Files discovered during `read_dir` but not yet
    /// opened at the time of the break are NOT included.
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
    /// The entry's `hash_version` is a value this build does not
    /// understand (i.e. neither [`HASH_VERSION_LEGACY`] nor
    /// [`HASH_VERSION_CURRENT`]). Most likely the log was written by
    /// a newer build that this build cannot validate. Surfaces
    /// loudly rather than silently treating the entry as legacy.
    UnsupportedHashVersion { version: u8, supported: Vec<u8> },
    /// The entry's `hash_version` is below the caller's required
    /// minimum (see [`VerifyOptions::min_hash_version`]). For
    /// projects that have only ever been written by
    /// [`AuditLog::append`] in this codebase, every entry is
    /// [`HASH_VERSION_CURRENT`] by construction, so encountering a
    /// v1 entry necessarily means tampering or a downgrade attack.
    /// Surfaced only when the caller uses
    /// [`verify_chain_with`] with
    /// [`VerifyOptions::strict_v2_only`]; the default lenient mode
    /// accepts v1 entries with linkage-only verification (see
    /// `entries_legacy_linkage_only`).
    LegacyHashVersionRejected { version: u8, required_min: u8 },
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
///
/// This call uses [`VerifyOptions::default`], which is **lenient**
/// about [`HASH_VERSION_LEGACY`] entries (verifies linkage only and
/// counts them in `entries_legacy_linkage_only`). Callers whose
/// projects have only ever used [`HASH_VERSION_CURRENT`] should use
/// [`verify_chain_with`] with [`VerifyOptions::strict_v2_only`] so
/// any v1 entry is treated as tampering (see the doc comment on
/// [`AuditEntryCanonical`] for the threat model).
pub fn verify_chain(audit_dir: &Path) -> AuditResult<ChainVerification> {
    verify_chain_with(audit_dir, VerifyOptions::default())
}

/// Verification policy for [`verify_chain_with`]. Default is the
/// lenient policy: accept v1 entries with linkage-only verification
/// and report them in `entries_legacy_linkage_only`. Strict mode
/// (see [`VerifyOptions::strict_v2_only`]) rejects any v1 entry as
/// a chain break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyOptions {
    /// Minimum acceptable [`AuditEntry::hash_version`]. Any entry
    /// with `hash_version < min_hash_version` is treated as a chain
    /// break with [`BreakReason::LegacyHashVersionRejected`].
    ///
    /// Defaults to [`HASH_VERSION_LEGACY`] (1), which means "accept
    /// every known version". Setting this to [`HASH_VERSION_CURRENT`]
    /// rejects any v1 entry — appropriate for projects that have
    /// only ever been written by this codebase (every
    /// [`AuditLog::append`] call writes v2), where the presence of a
    /// v1 entry necessarily indicates either tampering or a
    /// downgrade attack (see the doc comment on
    /// [`AuditEntryCanonical`]).
    pub min_hash_version: u8,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            min_hash_version: HASH_VERSION_LEGACY,
        }
    }
}

impl VerifyOptions {
    /// Strict policy: only [`HASH_VERSION_CURRENT`] (or later, once
    /// added) entries are accepted. Any v1 entry causes
    /// [`verify_chain_with`] to surface
    /// [`BreakReason::LegacyHashVersionRejected`] at the offending
    /// line.
    ///
    /// Use this for projects that have only ever been written by
    /// this codebase. The lenient default exists for backward
    /// compatibility with audit logs that genuinely contain v1
    /// entries from an older release.
    pub fn strict_v2_only() -> Self {
        Self {
            min_hash_version: HASH_VERSION_CURRENT,
        }
    }
}

/// Variant of [`verify_chain`] that takes a [`VerifyOptions`] for
/// policy control (e.g. strict v2-only mode that rejects legacy v1
/// entries as tampering). See [`VerifyOptions`] for details.
pub fn verify_chain_with(audit_dir: &Path, opts: VerifyOptions) -> AuditResult<ChainVerification> {
    // Enumerate `.jsonl` files. Surface per-entry `DirEntry` errors
    // (e.g. EACCES on a stat) explicitly rather than silently skipping
    // them with `filter_map(Result::ok)` — a skipped audit file is
    // exactly the integrity gap this function is meant to detect.
    let entries_iter = std::fs::read_dir(audit_dir)?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries_iter {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                return Ok(ChainVerification {
                    status: ChainStatus::BrokenAt {
                        file: audit_dir.to_path_buf(),
                        line: 0,
                        reason: BreakReason::Io {
                            message: format!(
                                "failed to read directory entry under {}: {}",
                                audit_dir.display(),
                                e
                            ),
                        },
                    },
                    entries_checked: 0,
                    entries_legacy_linkage_only: 0,
                    files_checked: Vec::new(),
                    head_hash: AuditLog::GENESIS.to_string(),
                });
            }
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files.sort();

    let mut head = AuditLog::GENESIS.to_string();
    let mut entries_checked: u64 = 0;
    let mut entries_legacy_linkage_only: u64 = 0;
    // Track files that were actually opened (i.e. inspected) so the
    // returned `files_checked` reflects coverage, not directory
    // contents. On an early break this contains the files up to and
    // including the one in which the break occurred; files discovered
    // during `read_dir` but not yet opened are NOT included.
    let mut files_checked: Vec<PathBuf> = Vec::with_capacity(files.len());

    for file in &files {
        let f = match File::open(file) {
            Ok(f) => {
                files_checked.push(file.clone());
                f
            }
            Err(e) => {
                // The file is the one whose open failed — record it
                // as "inspected" because we made an attempt, then
                // return immediately.
                files_checked.push(file.clone());
                return Ok(ChainVerification {
                    status: ChainStatus::BrokenAt {
                        file: file.clone(),
                        line: 0,
                        reason: BreakReason::Io {
                            message: e.to_string(),
                        },
                    },
                    entries_checked,
                    entries_legacy_linkage_only,
                    files_checked,
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
                        entries_legacy_linkage_only,
                        files_checked,
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
                        entries_legacy_linkage_only,
                        files_checked,
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
                    entries_legacy_linkage_only,
                    files_checked,
                    head_hash: head,
                });
            }
            match entry.recompute_hash() {
                HashRecompute::Current(recomputed) => {
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
                            entries_legacy_linkage_only,
                            files_checked,
                            head_hash: head,
                        });
                    }
                }
                HashRecompute::LegacyLinkageOnly => {
                    // v1 path. If the caller asked for strict
                    // v2-only verification (their project has only
                    // ever been written by this codebase), reject
                    // here — a v1 entry in a v2-only project is
                    // either tampering or a downgrade attack (see
                    // `AuditEntryCanonical` doc comment for the
                    // threat model). Otherwise fall back to
                    // linkage-only verification (already done above)
                    // and tally so callers can surface the legacy
                    // fraction to operators.
                    if entry.hash_version < opts.min_hash_version {
                        return Ok(ChainVerification {
                            status: ChainStatus::BrokenAt {
                                file: file.clone(),
                                line: line_num,
                                reason: BreakReason::LegacyHashVersionRejected {
                                    version: entry.hash_version,
                                    required_min: opts.min_hash_version,
                                },
                            },
                            entries_checked,
                            entries_legacy_linkage_only,
                            files_checked,
                            head_hash: head,
                        });
                    }
                    entries_legacy_linkage_only += 1;
                }
                HashRecompute::Unsupported => {
                    return Ok(ChainVerification {
                        status: ChainStatus::BrokenAt {
                            file: file.clone(),
                            line: line_num,
                            reason: BreakReason::UnsupportedHashVersion {
                                version: entry.hash_version,
                                supported: vec![HASH_VERSION_LEGACY, HASH_VERSION_CURRENT],
                            },
                        },
                        entries_checked,
                        entries_legacy_linkage_only,
                        files_checked,
                        head_hash: head,
                    });
                }
            }
            head = entry.hash;
            entries_checked += 1;
        }
    }

    Ok(ChainVerification {
        status: ChainStatus::Ok,
        entries_checked,
        entries_legacy_linkage_only,
        files_checked,
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
