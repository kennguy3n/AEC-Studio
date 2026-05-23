//! Journey F — Bridge end-to-end.
//!
//! Drives the bridge's read- and write-side surface in the same
//! sequence the desktop renderer does:
//!
//! 1. Boot a `BridgeService` against a temp `state_dir` / `projects_dir`
//!    / `templates_dir` (the production directories are out of scope —
//!    the contract is the public service API, not where the directories
//!    happen to live).
//! 2. `project_create_from_template` from a tiny in-test template. Open
//!    via `project_open`. Confirm the recents store picks it up.
//! 3. `project_engine_status` against the freshly-created project: the
//!    SQL audit_chain table is empty (v2 migration ran but no command
//!    has been audited yet), the JSONL log is also empty, and the
//!    schema_version matches `aec_core::manifest::SCHEMA_VERSION`. This
//!    is the "happy-path zero-state" assertion.
//! 4. Append three real `AuditEntry`s into the project's JSONL log
//!    (covering all five scopes so the per-scope counts pane is
//!    exercised). The bridge surface doesn't expose `append` directly
//!    — `aec_audit::AuditLog::append` is the API the command engine
//!    uses, and we mirror that here.
//! 5. `project_audit_sync` to push the in-memory chain into the SQL
//!    mirror. Confirm the return is `3` (three new rows).
//! 6. `project_engine_status` again. The SQL counts should now equal
//!    the JSONL counts; per-scope counts should match what we
//!    appended; the chain head should equal the last entry's `hash`.
//! 7. Append a fourth entry without re-syncing. `project_engine_status`
//!    should now report an out-of-sync state (`audit_entry_count = 4`,
//!    `audit_chain_sql_count = 3`) — the renderer can use this to show
//!    a "stale mirror" badge.
//! 8. `project_audit_sync` again. Returns `1`, and the next status
//!    report shows the system back in sync at 4 entries.
//! 9. `project_save` + `project_open` round-trip. The chain head /
//!    schema version / per-scope counts must survive a save+reopen
//!    cycle (i.e. they're really persisted, not just in-memory).
//!
//! All artefacts live in a `tempdir`. The test makes no network calls
//! and uses no environment configuration.

use aec_audit::AuditLog;
use aec_bridge::{BridgeConfig, BridgeService};
use aec_core::{
    package::ProjectPackage,
    types::{Actor, CommandId, Scope},
};

fn write_template(root: &std::path::Path, category: &str, id: &str) {
    let category_dir = root.join(category);
    std::fs::create_dir_all(&category_dir).unwrap();
    let key = format!("{category}.{id}");
    let json = serde_json::json!({
        "template_id": key,
        "name": format!("Bridge journey fixture {id}"),
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
    std::fs::write(category_dir.join(format!("{id}.json")), json.to_string()).unwrap();
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    write_template(&templates, "interior", "studio");
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let s = BridgeService::new(cfg, [7u8; 32]).unwrap();
    (s, tmp)
}

/// Append `entries` to the project's JSONL audit log. Returns the
/// final head hash. Mirrors the way `aec_command::CommandEngine`
/// appends to the chain after each successful command execution.
fn append_via_audit_log(
    project_root: &std::path::Path,
    entries: &[(Scope, &str, serde_json::Value)],
) -> String {
    let log_path = project_root.join("audit").join("log.jsonl");
    let mut log = AuditLog::open(&log_path).unwrap();
    for (scope, tool, payload) in entries {
        log.append(CommandId::new(), *scope, Actor::user(), *tool, payload)
            .unwrap();
    }
    log.head().to_string()
}

#[test]
fn journey_f_bridge_engine_status_and_audit_sync_round_trip() {
    // 1. Boot.
    let (mut svc, _guard) = boot_service();

    // 2. Create + open.
    let summary = svc
        .project_create_from_template("interior.studio", "Bridge Journey Project")
        .expect("create");
    let opened = svc.project_open(&summary.path).expect("open");
    assert_eq!(opened.project_id, summary.project_id);
    let recents = svc.project_list_recents().expect("recents");
    assert!(
        recents.iter().any(|r| r.path == summary.path),
        "newly-created project should appear in recents",
    );

    // 3. Initial engine status. Fresh project: schema is at the
    //    current SCHEMA_VERSION (the v2 migration runs on open), audit
    //    chain is empty in both JSONL and SQL.
    let status0 = svc
        .project_engine_status(&summary.path)
        .expect("engine status (initial)");
    assert_eq!(
        status0.schema_version,
        aec_core::manifest::SCHEMA_VERSION,
        "freshly-created project must be at the current SCHEMA_VERSION",
    );
    assert_eq!(status0.audit_chain_head, AuditLog::GENESIS);
    assert_eq!(status0.audit_entry_count, 0);
    assert_eq!(status0.audit_chain_sql_count, 0);
    // All five canonical scopes are present at 0 even when the log is
    // empty — the renderer's status pane wants a stable label set.
    for scope in Scope::all() {
        assert_eq!(
            status0.audit_chain_by_scope.get(scope.as_str()).copied(),
            Some(0),
            "scope {} should be present at 0 in fresh status",
            scope.as_str(),
        );
    }

    // 4. Append three real audit entries covering three scopes.
    let project_root = ProjectPackage::open(&summary.path)
        .unwrap()
        .root()
        .to_path_buf();
    let head_after_three = append_via_audit_log(
        &project_root,
        &[
            (Scope::Design, "design.create_wall", serde_json::json!({"x": 1})),
            (Scope::Design, "design.paint_material", serde_json::json!({"mat": "oak"})),
            (Scope::Render, "render.queue", serde_json::json!({"job": 7})),
        ],
    );

    // Before sync the SQL mirror is stale.
    let status_pre_sync = svc
        .project_engine_status(&summary.path)
        .expect("engine status (post-append, pre-sync)");
    assert_eq!(status_pre_sync.audit_entry_count, 3);
    assert_eq!(status_pre_sync.audit_chain_sql_count, 0);
    assert_eq!(status_pre_sync.audit_chain_head, head_after_three);

    // 5. Sync — 3 new rows expected.
    let inserted = svc
        .project_audit_sync(&summary.path)
        .expect("audit sync (first)");
    assert_eq!(inserted, 3);

    // 6. Engine status post-sync: counts match, scope counts match.
    let status1 = svc
        .project_engine_status(&summary.path)
        .expect("engine status (post-sync)");
    assert_eq!(status1.audit_entry_count, 3);
    assert_eq!(status1.audit_chain_sql_count, 3);
    assert_eq!(status1.audit_chain_head, head_after_three);
    assert_eq!(
        status1.audit_chain_by_scope.get("design").copied(),
        Some(2),
        "design scope should have 2 entries"
    );
    assert_eq!(
        status1.audit_chain_by_scope.get("render").copied(),
        Some(1),
        "render scope should have 1 entry"
    );
    for empty in ["draft", "bim", "deliver"] {
        assert_eq!(
            status1.audit_chain_by_scope.get(empty).copied(),
            Some(0),
            "scope {empty} should be at 0",
        );
    }

    // 7. Append a fourth entry without re-syncing — bridge should
    //    report an out-of-sync state.
    let head_after_four = append_via_audit_log(
        &project_root,
        &[(
            Scope::Deliver,
            "deliver.export_pack",
            serde_json::json!({"format": "zip"}),
        )],
    );
    let status_stale = svc
        .project_engine_status(&summary.path)
        .expect("engine status (stale)");
    assert_eq!(status_stale.audit_entry_count, 4);
    assert_eq!(status_stale.audit_chain_sql_count, 3);
    assert_eq!(status_stale.audit_chain_head, head_after_four);

    // 8. Re-sync — exactly 1 new row.
    let inserted_incremental = svc
        .project_audit_sync(&summary.path)
        .expect("audit sync (incremental)");
    assert_eq!(inserted_incremental, 1);
    let status2 = svc
        .project_engine_status(&summary.path)
        .expect("engine status (post-incremental-sync)");
    assert_eq!(status2.audit_entry_count, 4);
    assert_eq!(status2.audit_chain_sql_count, 4);
    assert_eq!(status2.audit_chain_head, head_after_four);
    assert_eq!(
        status2.audit_chain_by_scope.get("deliver").copied(),
        Some(1),
        "deliver scope should now report 1 entry after the incremental sync",
    );

    // 9. Save + reopen round-trip. The bridge's `project_save` only
    //    touches the manifest, but the SQLCipher DB + JSONL log are
    //    written eagerly, so engine status should be unchanged across
    //    the cycle.
    svc.project_save(&summary.path).expect("save");
    let _reopened = svc.project_open(&summary.path).expect("reopen");
    let status3 = svc
        .project_engine_status(&summary.path)
        .expect("engine status (after reopen)");
    assert_eq!(
        status3, status2,
        "engine status must survive a save+reopen cycle bit-for-bit",
    );
}
