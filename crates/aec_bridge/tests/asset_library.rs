//! End-to-end integration tests for `BridgeService::design_list_assets`
//! against a real on-disk asset DB.
//!
//! Mirrors the test surface for `bim_check_file_size` /
//! `bim_attach_ifc` — the `service.rs` `#[cfg(test)]` module already
//! covers the in-process unit-test path, but library consumers (the
//! N-API wrapper + downstream Electron) interact with the bridge as
//! an *integration target* rather than a `pub` library, so we stand
//! up a fresh `BridgeService` per test against a tempdir-scoped
//! `state_dir` and assert the end-to-end behaviour the renderer
//! actually depends on.
//!
//! These tests stand in for what `bim_readonly_ops.rs` does for the
//! BIM domain in PR-T: catch a regression where the napi layer's
//! `with_service_ref_fallible` wiring would silently mask a fault
//! that the unit tests can't surface (e.g. cross-test pollution of
//! the on-disk DB, lazy-init races on the inner `Mutex`,
//! `state_dir` directory creation, etc).

use std::path::Path;

use aec_bridge::{AssetListQuery, BridgeConfig, BridgeService};
use tempfile::TempDir;

fn write_template(root: &Path, category: &str, id: &str) {
    let category_dir = root.join(category);
    std::fs::create_dir_all(&category_dir).unwrap();
    let key = format!("{category}.{id}");
    let json = serde_json::json!({
        "template_id": key,
        "name": format!("Test {id}"),
        "description": "test fixture",
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

fn make_service() -> (BridgeService, TempDir) {
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
    };
    let s = BridgeService::new(cfg, [42u8; 32]).unwrap();
    (s, tmp)
}

#[test]
fn fresh_state_dir_seeds_4_demo_assets() {
    // PR-U baseline: a brand-new `state_dir` must end with 4 demo
    // assets visible through `design_list_assets`. Mirrors the
    // in-process TS fallback's `seedAssets()` behaviour.
    let (svc, _g) = make_service();
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("seed query");
    assert_eq!(
        assets.len(),
        4,
        "fresh state_dir must seed the 4-asset demo library so the asset browser \
         has content on first launch"
    );
}

#[test]
fn second_service_against_same_state_dir_sees_persistent_seed() {
    // Construct a service, list assets (triggers lazy init + seed),
    // drop the service, construct a fresh service against the SAME
    // `state_dir`, list again, and assert seed didn't duplicate. Pins
    // the idempotence contract on the seed path from a cross-process
    // perspective (the renderer's reload-app workflow).
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    write_template(&templates, "interior", "apartment");
    {
        let cfg = BridgeConfig {
            state_dir: state.clone(),
            projects_dir: projects.clone(),
            templates_dir: templates.clone(),
            max_recents: 10,
        };
        let s = BridgeService::new(cfg, [42u8; 32]).unwrap();
        let _ = s.design_list_assets(&AssetListQuery::default()).unwrap();
    }
    // Second open of the same state_dir — must not re-seed.
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let s2 = BridgeService::new(cfg, [42u8; 32]).unwrap();
    let assets = s2
        .design_list_assets(&AssetListQuery::default())
        .expect("second-open query");
    assert_eq!(
        assets.len(),
        4,
        "re-opening the same state_dir must not duplicate the seed (idempotence on open_and_seed)"
    );
}

#[test]
fn search_substring_is_ascii_case_insensitive_via_like() {
    // SQLite's `LIKE` is ASCII case-insensitive by default — verify
    // the bridge inherits that behaviour. "kivik" / "KIVIK" / "Kivik"
    // all match Kivik. Pins the renderer's expectation that the user
    // can type in any case.
    let (svc, _g) = make_service();
    for term in ["kivik", "KIVIK", "Kivik"] {
        let assets = svc
            .design_list_assets(&AssetListQuery {
                search: Some(term.to_string()),
                ..AssetListQuery::default()
            })
            .expect("case-insensitive match");
        assert_eq!(
            assets.len(),
            1,
            "search term `{term}` must match Kivik case-insensitively"
        );
        assert_eq!(assets[0].asset_id, "ikea.sofa_kivik_3s");
    }
}

#[test]
fn tag_filter_is_strict_exact_match() {
    // Tag filters use plain equality (`m.tags.iter().any(|t| t == tag)`),
    // not substring — so "sof" must NOT match Kivik (tagged "sofa").
    // Pins the exact-match contract so a substring-friendly UX bug
    // can't quietly creep in.
    let (svc, _g) = make_service();
    let assets = svc
        .design_list_assets(&AssetListQuery {
            tags: vec!["sof".to_string()],
            ..AssetListQuery::default()
        })
        .expect("tag-prefix query");
    assert!(
        assets.is_empty(),
        "tag filter is exact-match; `sof` must not match `sofa`"
    );
}

#[test]
fn limit_clamps_seed_library_to_2() {
    // The bridge default limit is 24 (renderer page size). A caller
    // can lower it. Pins that limit=2 produces exactly 2 results —
    // not 4 (default), not 0 (off-by-one).
    let (svc, _g) = make_service();
    let assets = svc
        .design_list_assets(&AssetListQuery {
            limit: Some(2),
            ..AssetListQuery::default()
        })
        .expect("limit query");
    assert_eq!(assets.len(), 2);
}

#[test]
fn empty_query_object_returns_full_seed() {
    // The renderer can call `designListAssets({})` (empty TS object).
    // Default-constructed `AssetListQuery` must behave the same:
    // every field `None` / empty / falsy → no filter → 4 seed assets.
    let (svc, _g) = make_service();
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("default query");
    assert_eq!(assets.len(), 4);
}

#[test]
fn vendor_field_carries_display_name_not_id() {
    // The `AssetSummary::vendor` field is the human display name
    // ("IKEA"), not the slug id ("ikea"). The asset browser card
    // shows this verbatim ("by IKEA"). Pins the projection that
    // walks `AssetMetadata::vendor.name` → `AssetSummary::vendor`.
    let (svc, _g) = make_service();
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("seed");
    let kivik = assets
        .iter()
        .find(|a| a.asset_id == "ikea.sofa_kivik_3s")
        .expect("kivik in seed");
    assert_eq!(kivik.vendor.as_deref(), Some("IKEA"));
}

#[test]
fn thumbnail_data_uri_none_for_seed_library() {
    // The seed library ships `thumbnail_data_uri = None` for all 4
    // demo assets — the list surface deliberately doesn't base64-
    // encode thumbnail blobs on every call (see the rationale on
    // `BridgeService::design_list_assets`), and the renderer falls
    // back to a procedural placeholder card. This pins the
    // contract so anyone wiring thumbnails onto the list surface
    // has to deliberately update this test rather than rely on
    // implicit `None`.
    let (svc, _g) = make_service();
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("seed");
    for asset in &assets {
        assert!(
            asset.thumbnail_data_uri.is_none(),
            "seed asset `{}` has no thumbnail blob; renderer renders placeholder card",
            asset.asset_id
        );
    }
}
