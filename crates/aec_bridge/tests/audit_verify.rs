//! Phase 11 Task 26 — Audit chain verification through the bridge.
//!
//! Stands up a real `BridgeService`, creates a project, appends
//! audit entries via the real `AuditLog::append` API (same code
//! path that `aec_command::Engine` uses on every command apply),
//! then runs `project_audit_verify`. Three scenarios:
//!
//!   1. Intact chain  -> verification returns `Ok`.
//!   2. Single tampered entry -> verification surfaces a
//!      `HashRecomputeMismatch` at the exact line.
//!   3. Project with no `audit/` dir -> verification returns
//!      `Ok` with zero entries (no audit log was created yet).

use std::fs;
use std::path::Path;

use aec_audit::{AuditLog, BreakReason, ChainStatus};
use aec_bridge::{BridgeConfig, BridgeService};
use aec_core::types::{Actor, CommandId, Scope};
use tempfile::TempDir;

fn write_template(root: &Path, category: &str, id: &str) {
    let category_dir = root.join(category);
    fs::create_dir_all(&category_dir).unwrap();
    let key = format!("{category}.{id}");
    let json = serde_json::json!({
        "template_id": key,
        "name": format!("Audit verify fixture {id}"),
        "description": "in-test fixture",
        "units": "mm",
        "region_defaults": {
            "EU": {"units": "mm", "standards": ["IFC4"]}
        },
        "rooms": [],
        "default_walls": {
            "exterior_thickness_mm": 250,
            "interior_thickness_mm": 100,
            "material": "wall_white"
        },
        "lighting_preset": "daylight",
        "asset_shelf": [],
        "camera_presets": []
    });
    fs::write(
        category_dir.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
}

fn make_service() -> (BridgeService, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    fs::create_dir_all(&templates).unwrap();
    write_template(&templates, "interior", "apartment");
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let s = BridgeService::new(cfg, [42u8; 32]).unwrap();
    (s, tmp)
}

fn append_entries(project_root: &Path, n: usize) {
    let log_path = project_root.join("audit").join("log.jsonl");
    let mut log = AuditLog::open(&log_path).unwrap();
    for i in 0..n {
        log.append(
            CommandId::new(),
            Scope::Design,
            Actor::user(),
            format!("design.op_{i}"),
            &serde_json::json!({"i": i}),
        )
        .unwrap();
    }
}

#[test]
fn audit_verify_returns_ok_on_intact_chain() {
    let (mut s, _tmp) = make_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Verify")
        .unwrap();
    append_entries(Path::new(&summary.path), 10);

    let v = s.project_audit_verify(&summary.path).unwrap();
    assert!(v.is_ok(), "expected Ok, got {:?}", v.status);
    assert!(
        v.entries_checked >= 10,
        "expected at least 10 entries verified, got {}",
        v.entries_checked
    );
    assert_eq!(v.files_checked.len(), 1);
}

#[test]
fn audit_verify_reports_break_at_tampered_line_through_bridge() {
    let (mut s, _tmp) = make_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Verify Tamper")
        .unwrap();
    append_entries(Path::new(&summary.path), 10);

    let log_path = Path::new(&summary.path).join("audit").join("log.jsonl");
    let raw = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert!(lines.len() >= 10);

    // Tamper a line near the middle of the log.
    let tamper_idx = lines.len() / 2;
    let mut entry: serde_json::Value = serde_json::from_str(lines[tamper_idx]).unwrap();
    entry["tool"] = serde_json::Value::String("design.op_TAMPERED".into());
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_owned()).collect();
    new_lines[tamper_idx] = serde_json::to_string(&entry).unwrap();
    fs::write(&log_path, new_lines.join("\n") + "\n").unwrap();

    let v = s.project_audit_verify(&summary.path).unwrap();
    match v.status {
        ChainStatus::BrokenAt {
            line,
            reason: BreakReason::HashRecomputeMismatch { .. },
            ..
        } => {
            assert_eq!(
                line,
                (tamper_idx + 1) as u64,
                "expected break at tampered line"
            );
        }
        other => panic!("expected HashRecomputeMismatch, got {other:?}"),
    }
    assert_eq!(v.entries_checked, tamper_idx as u64);
}

#[test]
fn audit_verify_returns_ok_on_freshly_created_project_with_no_entries_yet() {
    // Just-created project: the `audit/` directory exists (package
    // invariant) but has no `.jsonl` files yet — verification is
    // trivially `Ok` with zero entries.
    let (mut s, _tmp) = make_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Empty Audit")
        .unwrap();
    // Remove any `.jsonl` files that might have been written by the
    // template instantiation so we have a known empty state.
    let audit_dir = Path::new(&summary.path).join("audit");
    if audit_dir.exists() {
        for entry in fs::read_dir(&audit_dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                fs::remove_file(p).unwrap();
            }
        }
    }

    let v = s.project_audit_verify(&summary.path).unwrap();
    assert!(v.is_ok());
    assert_eq!(v.entries_checked, 0);
    assert!(v.files_checked.is_empty());
}
