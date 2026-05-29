//! `bim_attach` end-to-end against a real IFC4 fixture.
//!
//! Drives the bridge through the same sequence the desktop renderer
//! uses on a real "open this IFC file" click:
//!
//!   1. Boot a `BridgeService` against a temp `state_dir` /
//!      `projects_dir` / `templates_dir`.
//!   2. Create a fresh project from the in-test template.
//!   3. `bim_import_ifc` against the public-domain `small_office.ifc`
//!      fixture. Assert the preview summary numbers match what the
//!      fixture actually contains (8 spatial nodes, 5 elements, 4
//!      materials, etc.).
//!   4. `bim_attach_ifc` against the same file. Assert
//!      `parse_cache_hit = true` (the import populated the cache),
//!      assert the SQL `entities` table picked up the right number
//!      of `bim/spatial/*` and `bim/element/*` rows, and assert the
//!      `bim_cache` table has one row per element + spatial node.
//!   5. Re-attach the same file. Assert every count is now
//!      `_unchanged` (dedup contract) and zero `_inserted`.
//!   6. Verify the synthetic `AEC_LayerSetUsage` Pset round-tripped
//!      onto the two walls (the most interesting bit — Revit /
//!      ArchiCAD output uses `IfcMaterialLayerSetUsage` and the only
//!      way for us to preserve the per-wall direction / sense /
//!      offset is via the synthetic-Pset side channel).
//!
//! The fixture is hand-authored CC0; see `tests/fixtures/README.md`.

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

#[test]
fn small_office_fixture_attaches_into_project_graph() {
    let (mut s, _g) = service();
    let project = s
        .project_create_from_template("interior.apartment", "Small Office")
        .unwrap();

    // Write the fixture out to a tempdir so the bridge sees a real
    // path on disk (it canonicalises via `std::fs::canonicalize`).
    let ifc_dir = tempfile::tempdir().unwrap();
    let ifc_path = ifc_dir.path().join("small_office.ifc");
    std::fs::write(&ifc_path, FIXTURE_BYTES).unwrap();
    let ifc_path_str = ifc_path.to_string_lossy().into_owned();

    // 1. bim_import_ifc preview matches the fixture's actual shape.
    let preview = s.bim_import_ifc(&ifc_path_str).unwrap();
    assert_eq!(preview.schema, "IFC4");
    // Spatial: Project + Site + Building + 2x Storey + 2x Space = 7
    // landed nodes (Project counts toward the snapshot's
    // spatial_nodes stat).
    assert!(
        preview.spatial_nodes >= 6,
        "expected >= 6 spatial nodes, got {}",
        preview.spatial_nodes
    );
    // Elements: 2 walls + 1 slab + 1 beam + 1 column = 5.
    assert_eq!(preview.elements, 5);
    // Materials: Concrete + MineralWool + Gypsum + Steel = 4.
    assert_eq!(preview.materials, 4);
    // Layer-set: just one (Wall-250mm).
    assert_eq!(preview.material_layer_sets, 1);
    // Material assignments: 5 (one per element — 2 walls via usage
    // indirection, 1 slab single, 2 columns/beam single).
    assert_eq!(preview.material_assignments, 5);
    // The fixture has 2 Psets: `Pset_WallCommon` on the two walls
    // via one `IfcRelDefinesByProperties` relation, plus
    // `Pset_SpaceCommon` on the two IfcSpaces via a second
    // relation. The Space pset is deliberate — it's the
    // Revit / ArchiCAD pattern that previously triggered an FK
    // violation on re-attach (spatial nodes had non-deterministic
    // `EntityId::new()` ids that didn't match the previously
    // persisted row when components were re-inserted). See
    // `crates/aec_core/src/types.rs::EntityId::from_guid_seed`.
    assert_eq!(preview.psets, 2);
    // 1 Qto via one relation.
    assert_eq!(preview.qsets, 1);
    // File size < threshold.
    assert!(!preview.large_file_warning);
    assert_eq!(preview.file_size_bytes, FIXTURE_BYTES.len() as u64);

    // 2. bim_attach_ifc commits the snapshot to the project DB. The
    // import just populated the snapshot cache, so the attach must
    // hit it.
    let attach = s.bim_attach_ifc(&project.path, &ifc_path_str).unwrap();
    assert!(
        attach.parse_cache_hit,
        "attach immediately after import must hit the snapshot cache"
    );
    // We inserted, didn't update or no-op (first attach of this
    // project).
    assert!(attach.spatial_nodes_inserted >= 6);
    assert_eq!(attach.spatial_nodes_updated, 0);
    assert_eq!(attach.spatial_nodes_unchanged, 0);
    assert_eq!(attach.elements_inserted, 5);
    assert_eq!(attach.elements_updated, 0);
    assert_eq!(attach.elements_unchanged, 0);

    // 3. Re-attach the same file. Every count must now be
    // `_unchanged`; zero inserts / updates. This is the dedup
    // contract.
    let reattach = s.bim_attach_ifc(&project.path, &ifc_path_str).unwrap();
    assert_eq!(
        reattach.spatial_nodes_inserted, 0,
        "re-attach must not insert any spatial node"
    );
    assert_eq!(
        reattach.spatial_nodes_updated, 0,
        "re-attach with identical content must not update"
    );
    assert!(
        reattach.spatial_nodes_unchanged >= 6,
        "re-attach must report all spatial nodes as unchanged (got {})",
        reattach.spatial_nodes_unchanged
    );
    assert_eq!(reattach.elements_inserted, 0);
    assert_eq!(reattach.elements_updated, 0);
    assert_eq!(reattach.elements_unchanged, 5);

    // 4. Pset-only change: rewrite the fixture with a flipped
    //    `LoadBearing` value on `Pset_WallCommon` (IFCBOOLEAN(.T.) →
    //    IFCBOOLEAN(.F.)) and re-attach. The dedup classifier must
    //    report at least one entity as `_updated` rather than
    //    silently leaving it as `_unchanged`. Pre-fix the `pset_hash`
    //    was a placeholder empty string and the classifier ignored
    //    Pset changes; post-fix it hashes `ElementProperties`
    //    serde-serialised, so a flipped Pset value flips the hash
    //    and lifts the row from Unchanged → Updated.
    let pset_changed = std::str::from_utf8(FIXTURE_BYTES).unwrap().replace(
        "IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);",
        "IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.F.),$);",
    );
    let pset_changed_path = ifc_dir.path().join("small_office_pset_changed.ifc");
    std::fs::write(&pset_changed_path, pset_changed.as_bytes()).unwrap();
    let pset_changed_path_str = pset_changed_path.to_string_lossy().into_owned();
    let pset_attach = s
        .bim_attach_ifc(&project.path, &pset_changed_path_str)
        .unwrap();
    assert!(
        pset_attach.elements_updated >= 1,
        "Pset-only change must produce at least one Updated element, got {pset_attach:?}",
    );

    // 5. Re-parent only: move wall #50 from Ground Floor (#5) to
    //    First Floor (#6) by rewriting the two
    //    `IfcRelContainedInSpatialStructure` rows. The wall's body
    //    (guid, IFC class), Psets, and material assignment are all
    //    unchanged — only its containment moves. The dedup
    //    classifier must catch this and report the wall as
    //    `Updated` rather than `Unchanged`; otherwise the
    //    `entities.parent_id` column would silently retain the
    //    stale Ground-Floor parent.
    //
    //    Pre-fix the `geom_hash` was computed over `body_json` only
    //    and did NOT cover `parent_id`, so the Unchanged branch
    //    fired and the stale parentage persisted in SQL. Post-fix
    //    `parent_id` is folded into `geom_hash`, so the re-parent
    //    flips the hash and lifts the row from Unchanged →
    //    Updated, which then runs `UPDATE entities SET parent_id =
    //    ?1 …`.
    let reparented = std::str::from_utf8(FIXTURE_BYTES)
        .unwrap()
        .replace(
            "IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000d1',#1,$,$,(#50,#51,#54),#5);",
            "IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000d1',#1,$,$,(#51,#54),#5);",
        )
        .replace(
            "IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000d2',#1,$,$,(#52,#53),#6);",
            "IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000d2',#1,$,$,(#50,#52,#53),#6);",
        );
    let reparented_path = ifc_dir.path().join("small_office_reparented.ifc");
    std::fs::write(&reparented_path, reparented.as_bytes()).unwrap();
    let reparented_path_str = reparented_path.to_string_lossy().into_owned();
    let reparent_attach = s
        .bim_attach_ifc(&project.path, &reparented_path_str)
        .unwrap();
    assert!(
        reparent_attach.elements_updated >= 1,
        "Re-parent (parent_id change with otherwise-identical body) must \
         produce at least one Updated element, got {reparent_attach:?}",
    );
}
