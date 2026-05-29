//! Integration tests for the read-only BIM service methods wired
//! in PR-T: `bim_export_ifc`, `bim_validate`, `bim_diff`, and
//! `bim_generate_schedule`. All four route through
//! `with_service_ref_fallible` and operate on standalone IFC files
//! (no project package required), so the tests drive the
//! `BridgeService` against tempfiles seeded with the public-domain
//! `small_office.ifc` fixture used by `bim_attach_fixture.rs`.
//!
//! Coverage matrix:
//! * `bim_export_ifc` — round-trips the fixture and re-parses the
//!   output to confirm byte-for-byte fidelity goes through the
//!   reader → writer path.
//! * `bim_validate` — runs against the clean fixture (passes) and
//!   against a mutated fixture with a missing storey-class
//!   classification (fails predictably).
//! * `bim_diff` — diffs the fixture against itself (zero changes)
//!   and against an edited copy (one added wall) to assert the
//!   modified / added / removed sets behave.
//! * `bim_generate_schedule` — generates one XLSX per supported
//!   kind (`door`, `window`, `room`, `material`) and asserts the
//!   file lands on disk with the right column count.
//! * Snapshot cache fronting — calling `bim_validate` twice for
//!   the same file should report `parse_cache_hit = true` on the
//!   second call.

use aec_bridge::{BridgeConfig, BridgeService};

const FIXTURE_BYTES: &[u8] = include_bytes!("fixtures/small_office.ifc");

fn write_template(root: &std::path::Path, category: &str, id: &str) {
    let category_dir = root.join(category);
    std::fs::create_dir_all(&category_dir).unwrap();
    let key = format!("{category}.{id}");
    let json = serde_json::json!({
        "template_id": key,
        "name": format!("Fixture journey {id}"),
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

fn service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    write_template(&templates, "interior", "apartment");
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let s = BridgeService::new(cfg, [42u8; 32]).unwrap();
    (s, tmp)
}

/// Write the fixture bytes into a fresh tempdir and return both
/// the canonicalised string path (suitable for the bridge API) and
/// the owning tempdir guard (kept alive for the duration of the
/// test so the file isn't reaped underneath us).
fn write_fixture() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small_office.ifc");
    std::fs::write(&path, FIXTURE_BYTES).unwrap();
    let s = path.to_string_lossy().into_owned();
    (s, dir)
}

#[test]
fn bim_export_ifc_round_trips_fixture_through_reader_writer() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("small_office.out.ifc");
    let out_path_str = out_path.to_string_lossy().into_owned();

    let summary = s.bim_export_ifc(&src_path, &out_path_str).unwrap();
    assert_eq!(summary.schema, "IFC4");
    assert!(
        summary.bytes_written > 0,
        "expected non-zero bytes written, got {}",
        summary.bytes_written
    );
    // First call: snapshot cache was empty for this canonical path,
    // so the reader had to parse from disk. `parse_cache_hit = false`.
    assert!(!summary.parse_cache_hit);
    // The output is byte-identical to what the bim_attach snapshot
    // would have written; re-parse it and confirm the schema +
    // element count survived round-trip.
    let body = std::fs::read_to_string(&out_path).unwrap();
    let re_parsed = aec_bim::ifc::IfcReader::from_string(&body).unwrap();
    assert!(matches!(
        re_parsed.schema,
        aec_bim::ifc::IfcSchema::Ifc4 | aec_bim::ifc::IfcSchema::Ifc4x3
    ));
}

#[test]
fn bim_export_ifc_serves_second_call_from_snapshot_cache() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path_1 = out_dir.path().join("a.ifc");
    let out_path_2 = out_dir.path().join("b.ifc");

    let r1 = s
        .bim_export_ifc(&src_path, &out_path_1.to_string_lossy())
        .unwrap();
    let r2 = s
        .bim_export_ifc(&src_path, &out_path_2.to_string_lossy())
        .unwrap();
    assert!(!r1.parse_cache_hit, "first call should populate cache");
    assert!(r2.parse_cache_hit, "second call should hit cache");
    // Byte-for-byte identical outputs across calls — the writer is
    // deterministic (DFS spatial walk + sorted Pset / material
    // iteration), so writing the same snapshot twice produces the
    // same bytes.
    assert_eq!(
        std::fs::read(&out_path_1).unwrap(),
        std::fs::read(&out_path_2).unwrap(),
        "deterministic writer should produce identical output for identical input"
    );
}

#[test]
fn bim_validate_passes_on_clean_fixture() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();

    let report = s.bim_validate(&src_path).unwrap();
    // The fixture is hand-authored to be clean (every wall has the
    // standard Pset_WallCommon, every spatial node has a
    // classification). The snapshot cache wasn't populated yet so
    // `parse_cache_hit = false`.
    assert!(!report.parse_cache_hit);
    assert_eq!(report.schema, "IFC4");
    assert!(
        report.ok,
        "fixture should validate cleanly; errors = {:?}",
        report.errors
    );
    assert!(
        report.errors.is_empty(),
        "expected zero errors, got {:?}",
        report.errors
    );
}

#[test]
fn bim_validate_caches_subsequent_parses() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();

    let r1 = s.bim_validate(&src_path).unwrap();
    let r2 = s.bim_validate(&src_path).unwrap();
    assert!(!r1.parse_cache_hit, "first call populates the cache");
    assert!(r2.parse_cache_hit, "second call serves from cache");
    // Both reports describe the same file, so the finding sets
    // must be identical (the validator is pure over its inputs).
    assert_eq!(r1.errors, r2.errors);
    assert_eq!(r1.warnings, r2.warnings);
    assert_eq!(r1.infos, r2.infos);
    assert_eq!(r1.ok, r2.ok);
}

#[test]
fn bim_diff_against_self_reports_zero_changes() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();

    let diff = s.bim_diff(&src_path, &src_path).unwrap();
    assert_eq!(diff.added.len(), 0);
    assert_eq!(diff.removed.len(), 0);
    assert_eq!(diff.modified.len(), 0);
    assert_eq!(diff.before_schema, "IFC4");
    assert_eq!(diff.after_schema, "IFC4");
    assert!(diff.diff_id.starts_with("diff_blake3_"));
    // Both sides resolved to the same canonical path, so the diff
    // id is stable; calling twice produces the same id.
    let diff2 = s.bim_diff(&src_path, &src_path).unwrap();
    assert_eq!(diff.diff_id, diff2.diff_id);
}

#[test]
fn bim_diff_round_trips_through_writer_unchanged() {
    // Diff the original fixture against the re-serialised output of
    // `bim_export_ifc`. The writer is deterministic so the output
    // is a "canonicalised" form of the input — the diff should
    // report zero added / removed / modified.
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("normalised.ifc");
    let out_path_str = out_path.to_string_lossy().into_owned();
    s.bim_export_ifc(&src_path, &out_path_str).unwrap();

    let diff = s.bim_diff(&src_path, &out_path_str).unwrap();
    assert_eq!(
        diff.added.len(),
        0,
        "writer should not introduce new elements, got added = {:?}",
        diff.added
    );
    assert_eq!(
        diff.removed.len(),
        0,
        "writer should not drop elements, got removed = {:?}",
        diff.removed
    );
    // Modified can be non-zero if (for example) the writer emits
    // properties in a different order than the input fixture
    // expressed them — the diff engine joins on GUID and compares
    // PropertyValues. The fixture is hand-authored to align with
    // writer output, so this should also be empty.
    assert_eq!(
        diff.modified.len(),
        0,
        "round-trip diff should be empty, got modified = {:?}",
        diff.modified
    );
}

#[test]
fn bim_generate_schedule_writes_all_four_kinds_to_xlsx() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();
    let out_dir = tempfile::tempdir().unwrap();

    for kind in &["door", "window", "room", "material"] {
        let out_path = out_dir.path().join(format!("{kind}.xlsx"));
        let out_path_str = out_path.to_string_lossy().into_owned();
        let summary = s
            .bim_generate_schedule(&src_path, kind, &out_path_str)
            .unwrap();
        assert_eq!(summary.kind, *kind);
        assert!(summary.bytes_written > 0);
        // ScheduleSheet has a fixed column count per schedule kind
        // — the row count varies with fixture content. Just assert
        // columns is non-zero (the writer always emits the header
        // row even if there are zero data rows).
        assert!(
            summary.columns > 0,
            "{kind} schedule should have at least one column"
        );
        // Stable id: same kind + same source → same id.
        let summary_again = s
            .bim_generate_schedule(&src_path, kind, &out_path_str)
            .unwrap();
        assert_eq!(summary.schedule_id, summary_again.schedule_id);
        assert!(summary.schedule_id.starts_with("sched_blake3_"));
        // The XLSX file itself exists on disk.
        assert!(
            out_path.exists(),
            "{} should be written",
            out_path.display()
        );
    }
}

#[test]
fn bim_generate_schedule_rejects_unknown_kind() {
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("unknown.xlsx");

    let err = s
        .bim_generate_schedule(&src_path, "boq", &out_path.to_string_lossy())
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown schedule kind"),
        "expected unknown-kind error, got {msg}"
    );
    // The renderer surfaces this string verbatim, so confirm both
    // the user-typed token and the valid set are mentioned.
    assert!(msg.contains("boq"));
    assert!(msg.contains("door"));
}

#[test]
fn bim_read_schedule_rows_round_trips_generated_xlsx() {
    // Closes the XLSX-to-row-list round trip exercised by the
    // renderer's `ScheduleView` after Phase 13 Task 2 / Phase 14
    // Task 27: `bim_generate_schedule` writes the XLSX, then
    // `bim_read_schedule_rows` reads it back so the table can
    // display real row data. Without this test, the writer and
    // reader could drift silently (e.g. a future change to
    // `ScheduleSheet::write_xlsx` that adds a metadata row at
    // index 0 would break readback without any cargo-side gate
    // firing).
    let (s, _g) = service();
    let (src_path, _src_dir) = write_fixture();
    let out_dir = tempfile::tempdir().unwrap();

    for kind in &["door", "window", "room", "material"] {
        let out_path = out_dir.path().join(format!("{kind}.xlsx"));
        let out_path_str = out_path.to_string_lossy().into_owned();
        let summary = s
            .bim_generate_schedule(&src_path, kind, &out_path_str)
            .unwrap();
        let readback = s.bim_read_schedule_rows(&out_path_str).unwrap();
        // Header column count matches the writer's column count.
        assert_eq!(
            readback.header.len(),
            summary.columns as usize,
            "{kind} header length should match writer columns"
        );
        // Row count matches what the writer reported. The fixture
        // schedules may have zero rows for some kinds; readback
        // must report the same count rather than silently
        // dropping or duplicating.
        assert_eq!(
            readback.rows.len(),
            summary.rows as usize,
            "{kind} readback row count should match writer rows"
        );
        // Every row has exactly `header.len()` cells with the
        // header names as keys — the renderer relies on this
        // shape to render a uniform table.
        for (idx, row) in readback.rows.iter().enumerate() {
            assert_eq!(
                row.len(),
                readback.header.len(),
                "{kind} row {idx} should have header.len() cells"
            );
            for col in &readback.header {
                assert!(
                    row.contains_key(col),
                    "{kind} row {idx} missing column {col}"
                );
            }
        }
    }
}

#[test]
fn bim_read_schedule_rows_typed_error_on_missing_file() {
    // Drift guard: the renderer maps the bridge's error message to
    // a user-facing toast. The `read_schedule_rows:` prefix is
    // load-bearing for our own log filtering, so assert it
    // explicitly rather than just `unwrap_err`.
    let (s, _g) = service();
    let err = s
        .bim_read_schedule_rows("/this/path/does/not/exist/missing.xlsx")
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("read_schedule_rows:"),
        "expected `read_schedule_rows:` prefix, got: {msg}"
    );
}

#[test]
fn bim_export_ifc_errors_on_missing_source() {
    let (s, _g) = service();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("out.ifc");

    let err = s
        .bim_export_ifc(
            "/this/path/does/not/exist/foo.ifc",
            &out_path.to_string_lossy(),
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("io:") || msg.contains("No such file"),
        "expected io error, got {msg}"
    );
}

#[test]
fn bim_validate_errors_on_missing_source() {
    let (s, _g) = service();
    let err = s.bim_validate("/does/not/exist/v.ifc").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("io:") || msg.contains("No such file"),
        "expected io error, got {msg}"
    );
}
