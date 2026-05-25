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

use aec_audit::{verify_chain, AuditLog, BreakReason, ChainStatus, ChainVerification};
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
