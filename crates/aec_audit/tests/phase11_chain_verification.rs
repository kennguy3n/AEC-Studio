//! Phase 11 Task 26 — BLAKE3 chain verification.
//!
//! Spec:
//! > Implement `verify_chain(audit_dir: &Path) -> Result<ChainVerification>`
//! > that:
//! >   - Reads all `.jsonl` files in chronological order
//! >   - Verifies each entry's `hash` matches `blake3(previous_hash || entry_bytes)`
//! >   - Reports the first broken link if any
//! >
//! > Test: create 10 audit entries → verify chain passes → tamper with
//! > entry 5 → verify chain reports break at entry 5.

use std::fs;
use std::path::Path;

use aec_audit::{
    verify_chain, verify_chain_with, AuditLog, BreakReason, ChainStatus, ChainVerification,
    VerifyOptions, HASH_VERSION_CURRENT, HASH_VERSION_LEGACY,
};
use aec_core::types::{Actor, CommandId, Scope};

fn make_ten_entries(dir: &Path) -> Vec<String> {
    let log_path = dir.join("log.jsonl");
    let mut log = AuditLog::open(&log_path).unwrap();
    let mut hashes = Vec::new();
    for i in 0..10 {
        let h = log
            .append(
                CommandId::new(),
                Scope::Design,
                Actor::user(),
                format!("design.op_{i}"),
                &serde_json::json!({"i": i}),
            )
            .unwrap()
            .hash
            .clone();
        hashes.push(h);
    }
    hashes
}

#[test]
fn verify_passes_on_intact_ten_entry_chain() {
    let dir = tempfile::tempdir().unwrap();
    let hashes = make_ten_entries(dir.path());

    let v: ChainVerification = verify_chain(dir.path()).unwrap();
    assert!(v.is_ok(), "expected Ok, got {:?}", v.status);
    assert_eq!(v.entries_checked, 10);
    assert_eq!(v.head_hash, hashes[9]);
    assert_eq!(v.files_checked.len(), 1);
}

#[test]
fn verify_reports_break_when_entry_five_tool_field_tampered() {
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 10);

    // Tamper entry 5 — change the `tool` field's value (entry remains
    // syntactically valid JSON but its content no longer matches the
    // stored `hash`).
    let mut entry: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
    entry["tool"] = serde_json::Value::String("design.op_INJECTED".into());
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[4] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    assert!(!v.is_ok());
    match v.status {
        ChainStatus::BrokenAt {
            line,
            reason: BreakReason::HashRecomputeMismatch { .. },
            ..
        } => {
            assert_eq!(line, 5, "break should be at line 5, got {line}");
        }
        other => panic!("expected HashRecomputeMismatch at line 5, got {other:?}"),
    }
    // Lines 1..=4 verified before the break.
    assert_eq!(v.entries_checked, 4);
}

#[test]
fn verify_reports_break_when_entry_five_hash_field_tampered() {
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();

    // Mutate the `hash` field of line 5 directly.
    let mut entry: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
    entry["hash"] = serde_json::Value::String("blake3:deadbeef".into());
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[4] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    match v.status {
        ChainStatus::BrokenAt {
            line,
            reason: BreakReason::HashRecomputeMismatch { .. },
            ..
        } => {
            assert_eq!(line, 5);
        }
        other => panic!("expected HashRecomputeMismatch at line 5, got {other:?}"),
    }
}

#[test]
fn verify_reports_break_when_entry_five_prev_hash_tampered() {
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();

    // Mutate the `prev_hash` field of line 5 to break linkage.
    let mut entry: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
    entry["prev_hash"] = serde_json::Value::String("blake3:0000".into());
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[4] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    // The prev_hash mismatch fires first (before HashRecompute).
    match v.status {
        ChainStatus::BrokenAt {
            line,
            reason: BreakReason::PrevHashMismatch { .. },
            ..
        } => {
            assert_eq!(line, 5);
        }
        other => panic!("expected PrevHashMismatch at line 5, got {other:?}"),
    }
}

#[test]
fn verify_walks_multiple_jsonl_files_in_lex_order() {
    // Two segment files; entries in the first must precede those in
    // the second. We simulate a log rotation by writing two files.
    let dir = tempfile::tempdir().unwrap();

    // First create a single log, then split its contents into two
    // sequential files to simulate rotation.
    let combined = dir.path().join("combined.jsonl");
    let mut log = AuditLog::open(&combined).unwrap();
    for i in 0..6 {
        log.append(
            CommandId::new(),
            Scope::Design,
            Actor::user(),
            format!("op_{i}"),
            &serde_json::json!({"i": i}),
        )
        .unwrap();
    }
    let raw = fs::read_to_string(&combined).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    fs::remove_file(&combined).unwrap();
    // verify_chain ignores non-`.jsonl` files, so the combined file
    // wouldn't have been picked up anyway after this rename.
    fs::write(dir.path().join("0001.jsonl"), lines[..3].join("\n") + "\n").unwrap();
    fs::write(dir.path().join("0002.jsonl"), lines[3..].join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    assert!(v.is_ok(), "got {:?}", v.status);
    assert_eq!(v.entries_checked, 6);
    assert_eq!(v.files_checked.len(), 2);
}

#[test]
fn verify_ignores_non_jsonl_files() {
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());
    // Add a noise file; verify_chain should ignore it.
    fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    fs::write(dir.path().join("backup.json"), "{}").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    assert!(v.is_ok());
    assert_eq!(v.entries_checked, 10);
}

#[test]
fn verify_returns_error_for_nonexistent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist");
    let r = verify_chain(&missing);
    assert!(r.is_err(), "expected I/O error, got {r:?}");
}

#[test]
fn verify_handles_blank_lines_between_entries_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    // Insert blank lines between every entry; verify_chain skips them.
    let with_blanks = raw.replace('\n', "\n\n");
    fs::write(&log_path, with_blanks).unwrap();

    let v = verify_chain(dir.path()).unwrap();
    assert!(v.is_ok());
    assert_eq!(v.entries_checked, 10);
}

#[test]
fn verify_accepts_legacy_v1_entries_with_linkage_only_check() {
    // A v1 entry is one whose `hash_version` field equals
    // HASH_VERSION_LEGACY (1). The stored `hash` was computed with the
    // pre-canonical-JSON algorithm whose input includes the original
    // payload bytes; those bytes aren't persisted on the entry, so
    // verify_chain cannot recompute the hash. It MUST therefore fall
    // back to linkage-only verification (i.e. checks `prev_hash`
    // against the running head but does NOT compare the stored
    // `hash`) — and report that fact via `entries_legacy_linkage_only`
    // rather than silently treating the chain as fully verified.
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    // Downgrade entry 3's `hash_version` to v1. The entry's stored
    // `hash` is still the v2 BLAKE3 (we don't touch it), but
    // verify_chain must not attempt to recompute the v1 hash from
    // canonical JSON — it must just verify linkage and tally it as a
    // linkage-only entry.
    let mut entry: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    entry["hash_version"] = serde_json::json!(HASH_VERSION_LEGACY);
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[2] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    assert!(v.is_ok(), "got {:?}", v.status);
    assert_eq!(v.entries_checked, 10);
    assert_eq!(
        v.entries_legacy_linkage_only, 1,
        "exactly one entry was tagged v1; got {} legacy entries",
        v.entries_legacy_linkage_only
    );
}

#[test]
fn verify_reports_unsupported_hash_version_on_unknown_version() {
    // Future-proofing: an entry with a `hash_version` neither v1 nor
    // v2 (e.g. a v99 written by a newer build) must NOT be silently
    // accepted as legacy — verify_chain doesn't know how to validate
    // it, so it surfaces UnsupportedHashVersion at the offending
    // entry. This prevents an attacker from bumping `hash_version`
    // past the validator's known set to bypass integrity checks.
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    let mut entry: serde_json::Value = serde_json::from_str(lines[5]).unwrap();
    entry["hash_version"] = serde_json::json!(99u8);
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[5] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    match v.status {
        ChainStatus::BrokenAt {
            line,
            reason:
                BreakReason::UnsupportedHashVersion {
                    version, supported, ..
                },
            ..
        } => {
            assert_eq!(line, 6, "break should be at line 6 (entry index 5)");
            assert_eq!(version, 99);
            assert!(supported.contains(&HASH_VERSION_LEGACY));
            assert!(supported.contains(&HASH_VERSION_CURRENT));
        }
        other => panic!("expected UnsupportedHashVersion at line 6, got {other:?}"),
    }
    // 5 entries verified cleanly before the bad one (line 6 / index 5).
    assert_eq!(v.entries_checked, 5);
}

#[test]
fn verify_files_checked_includes_only_inspected_files_on_early_break() {
    // Three rotated log segments; tamper a hash in the SECOND file.
    // The break must short-circuit verification of the third file —
    // and `files_checked` must reflect only the two files actually
    // opened (not all three in the directory).
    let dir = tempfile::tempdir().unwrap();

    let combined = dir.path().join("combined.jsonl");
    let mut log = AuditLog::open(&combined).unwrap();
    for i in 0..9 {
        log.append(
            CommandId::new(),
            Scope::Design,
            Actor::user(),
            format!("op_{i}"),
            &serde_json::json!({"i": i}),
        )
        .unwrap();
    }
    let raw = fs::read_to_string(&combined).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    fs::remove_file(&combined).unwrap();

    fs::write(dir.path().join("0001.jsonl"), lines[..3].join("\n") + "\n").unwrap();
    // Tamper line 1 of the second segment (entries 3..6 inclusive).
    let mut seg2_lines: Vec<String> = lines[3..6].iter().map(|&s| s.to_owned()).collect();
    let mut entry: serde_json::Value = serde_json::from_str(&seg2_lines[0]).unwrap();
    entry["tool"] = serde_json::json!("op_INJECTED");
    seg2_lines[0] = serde_json::to_string(&entry).unwrap();
    fs::write(dir.path().join("0002.jsonl"), seg2_lines.join("\n") + "\n").unwrap();
    fs::write(dir.path().join("0003.jsonl"), lines[6..].join("\n") + "\n").unwrap();

    let v = verify_chain(dir.path()).unwrap();
    assert!(!v.is_ok());
    match &v.status {
        ChainStatus::BrokenAt {
            file,
            reason: BreakReason::HashRecomputeMismatch { .. },
            ..
        } => {
            assert!(file.ends_with("0002.jsonl"), "break should be in segment 2");
        }
        other => panic!("expected HashRecomputeMismatch in 0002.jsonl, got {other:?}"),
    }
    assert_eq!(
        v.files_checked.len(),
        2,
        "files_checked must include 0001.jsonl and 0002.jsonl (the broken one), \
         not 0003.jsonl which was never opened — got {:?}",
        v.files_checked
    );
    assert!(v.files_checked[0].ends_with("0001.jsonl"));
    assert!(v.files_checked[1].ends_with("0002.jsonl"));
}

#[test]
fn strict_v2_only_rejects_v1_entry_as_downgrade_attack() {
    // Threat model: an attacker with write access to the JSONL file
    // can forge a v1 entry — `hash_version=1` with arbitrary content
    // — because the v1 hash algorithm needs the original payload
    // bytes (not persisted on the entry), so `verify_chain` cannot
    // recompute it and falls back to linkage-only verification.
    //
    // For projects that have only ever been written by
    // `AuditLog::append` in this codebase (every entry is
    // HASH_VERSION_CURRENT by construction), encountering any v1
    // entry necessarily indicates tampering. `verify_chain_with`
    // with `VerifyOptions::strict_v2_only()` must surface this as a
    // chain break at the offending line rather than silently
    // accepting it with `entries_legacy_linkage_only += 1`.
    let dir = tempfile::tempdir().unwrap();
    make_ten_entries(dir.path());

    let log_path = dir.path().join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    // Downgrade entry 3 to v1. (The lenient `verify_chain` already
    // accepts this — see `verify_accepts_legacy_v1_entries_*` —
    // but strict mode must reject it.)
    let mut entry: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    entry["hash_version"] = serde_json::json!(HASH_VERSION_LEGACY);
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[2] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    // Sanity-check that the lenient default still accepts this so we
    // know the test exercises strict mode specifically (and not just
    // the underlying chain).
    let lenient = verify_chain(dir.path()).unwrap();
    assert!(
        lenient.is_ok(),
        "lenient mode should accept the v1 entry; got {:?}",
        lenient.status
    );
    assert_eq!(lenient.entries_legacy_linkage_only, 1);

    let strict = verify_chain_with(dir.path(), VerifyOptions::strict_v2_only()).unwrap();
    match strict.status {
        ChainStatus::BrokenAt {
            line,
            reason:
                BreakReason::LegacyHashVersionRejected {
                    version,
                    required_min,
                },
            ..
        } => {
            assert_eq!(line, 3, "break should be at line 3 (entry index 2)");
            assert_eq!(version, HASH_VERSION_LEGACY);
            assert_eq!(required_min, HASH_VERSION_CURRENT);
        }
        other => panic!("expected LegacyHashVersionRejected at line 3, got {other:?}"),
    }
    // 2 entries verified cleanly before the rejected one (lines 1-2).
    assert_eq!(strict.entries_checked, 2);
    // No v1 entry was *accepted* by the strict pass — the one v1
    // entry was rejected, not counted.
    assert_eq!(strict.entries_legacy_linkage_only, 0);
}

#[test]
fn strict_v2_only_accepts_intact_v2_chain() {
    // Strict mode must not regress lenient mode on the happy path:
    // a chain whose every entry is HASH_VERSION_CURRENT verifies
    // identically under both policies.
    let dir = tempfile::tempdir().unwrap();
    let hashes = make_ten_entries(dir.path());

    let v = verify_chain_with(dir.path(), VerifyOptions::strict_v2_only()).unwrap();
    assert!(v.is_ok(), "expected Ok, got {:?}", v.status);
    assert_eq!(v.entries_checked, 10);
    assert_eq!(v.entries_legacy_linkage_only, 0);
    assert_eq!(v.head_hash, *hashes.last().unwrap());
}

#[test]
fn append_failure_leaves_in_memory_state_consistent_with_disk() {
    // If `AuditLog::append` cannot persist the entry to disk (e.g.
    // the audit directory is read-only at the OS layer, or storage
    // is full), the in-memory `head` and `entries` vector must NOT
    // advance — otherwise a subsequent `open` of the same path
    // would replay only the entries that did make it to disk and
    // rebuild a *different* head than the live process holds,
    // silently breaking chain continuity across a process restart.
    //
    // We simulate an unwritable target by pointing the log at a
    // path whose *parent component* already exists as a regular
    // file: `create_dir_all` then fails because it cannot create a
    // directory inside a file, and the in-memory state must be
    // untouched.
    let dir = tempfile::tempdir().unwrap();
    // Put a regular file where a parent directory is required to
    // live, so `create_dir_all(parent)` inside `append` fails.
    let blocking_file = dir.path().join("audit");
    fs::write(&blocking_file, b"not-a-directory").unwrap();
    let log_path = blocking_file.join("inner").join("log.jsonl");

    let mut log = AuditLog::open(&log_path).unwrap();
    let head_before = log.head().to_string();
    let entries_before = log.entries().len();

    let result = log.append(
        CommandId::new(),
        Scope::Design,
        Actor::user(),
        "design.create_wall",
        &serde_json::json!({"x": 1}),
    );
    assert!(
        result.is_err(),
        "append should fail when the parent dir is blocked by a regular file"
    );

    // After the failure the in-memory chain must be byte-for-byte
    // identical to its pre-call state. If `self.head` had been
    // advanced before the file write (the pre-fix order), this
    // assertion would fail.
    assert_eq!(log.head(), head_before, "head must not advance on I/O fail");
    assert_eq!(
        log.entries().len(),
        entries_before,
        "entries must not be pushed on I/O fail"
    );
}
