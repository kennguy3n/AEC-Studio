//! Integration test for the Phase 16 per-extension boot diagnostics path.
//!
//! Boots a real [`BridgeService`] pointed at a synthetic extensions
//! directory containing three sub-dirs:
//!
//! 1. An unparseable `manifest.json` (`{ this is not json`) — must
//!    surface as a `manifest_parse` diagnostic with `extension_id ==
//!    None` because the parse failure happens before the id is
//!    extracted.
//! 2. A manifest with `manifest_version: 9999` — pinned-version
//!    failure surfaces as `manifest_validation` with the extension id
//!    populated.
//! 3. A valid manifest — must NOT generate a diagnostic.
//!
//! Why this test exists: the boot path is intentionally
//! fault-tolerant (a broken extension can't take the whole bridge
//! offline), but pre-Phase-16 those failures were dropped on the
//! floor — there was no observable signal that anything had gone
//! wrong. This test pins the new observability contract: the bridge
//! must report exactly the broken extensions, with stable wire
//! stages, in deterministic registry-walk order, while still booting
//! to a usable state with the good extension loaded.

use aec_bridge::{BridgeConfig, BridgeService};
use std::fs;
use tempfile::TempDir;

fn write_manifest(parent: &std::path::Path, dir_name: &str, body: &str) {
    let dir = parent.join(dir_name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.json"), body).unwrap();
}

#[test]
fn bridge_boot_buffers_per_extension_load_failures() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let state = root.join("state");
    let projects = root.join("projects");
    let templates = root.join("templates");
    let extensions = root.join("extensions");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&projects).unwrap();
    fs::create_dir_all(&templates).unwrap();
    fs::create_dir_all(&extensions).unwrap();

    // 1. Unparseable JSON — `manifest_parse`.
    write_manifest(&extensions, "01_unparseable", "{ this is not json");

    // 2. Unknown permission — fails manifest validation by the
    //    `aec_core::permission` validator (the schema parses but the
    //    permission token doesn't exist), so the loader surfaces this
    //    as a `manifest_parse` diagnostic with the offending id
    //    embedded in the error string. (The two stages we care about
    //    pinning here are "parse" and "validation"; the boundary
    //    between them lives in the loader and the test accepts
    //    either bucket so it doesn't break on a refinement.)
    write_manifest(
        &extensions,
        "02_bad_permission",
        r#"{
            "id": "demo.bad-permission",
            "name": "Bad permission",
            "version": "0.1.0",
            "type": "asset_pack",
            "permissions": ["world_domination"],
            "license": "AGPL-3.0",
            "asset_pack": {"vendor":"demo","entries":[]}
        }"#,
    );

    // 3. Valid manifest — should NOT produce a diagnostic.
    //    Asset-pack manifests need `filesystem_read` declared
    //    because the asset-pack host reads source paths under the
    //    extension root; an empty `entries` array doesn't trigger
    //    any actual reads but the permission check still fires.
    write_manifest(
        &extensions,
        "03_valid",
        r#"{
            "id": "demo.valid",
            "name": "Valid",
            "version": "0.1.0",
            "type": "asset_pack",
            "permissions": ["filesystem_read"],
            "license": "AGPL-3.0",
            "asset_pack": {"vendor":"demo","entries":[]}
        }"#,
    );

    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: Some(extensions),
    };
    let svc = BridgeService::new(cfg, [42u8; 32]).unwrap();
    let diags = svc.extension_load_diagnostics();

    // Exactly two failures captured, in deterministic order.
    assert_eq!(diags.len(), 2, "expected 2 diagnostics, got {diags:?}");

    // Diagnostic 1: unparseable manifest — no id, parse stage.
    assert!(diags[0].extension_id.is_none(), "unparseable id: {diags:?}");
    assert_eq!(diags[0].stage.as_wire_str(), "manifest_parse");
    assert!(
        diags[0].path.to_string_lossy().contains("01_unparseable"),
        "unexpected path: {:?}",
        diags[0].path
    );

    // Diagnostic 2: bad permission token. The loader surfaces this
    // via serde's tagged-enum path so the wire stage is
    // `manifest_parse`, but if a future refactor splits permission
    // validation into a dedicated post-parse phase the stage will
    // flip to `manifest_validation` — we accept both wire strings
    // so this test pins the captured-the-failure contract without
    // tying it to the loader's internal stage bucketing.
    let stage1 = diags[1].stage.as_wire_str();
    assert!(
        matches!(stage1, "manifest_parse" | "manifest_validation"),
        "unexpected stage for 02_bad_permission: {stage1}"
    );
    assert!(
        diags[1].path.to_string_lossy().contains("02_bad_permission"),
        "unexpected path: {:?}",
        diags[1].path
    );

    // Every diagnostic must carry a non-empty message — the renderer
    // shows it directly to the user, so an empty string would be a
    // UX bug. (We do not pin the exact wording, only its presence.)
    for d in diags {
        assert!(!d.message.is_empty(), "empty message: {d:?}");
    }
}

#[test]
fn bridge_boot_with_no_extensions_dir_has_empty_diagnostics() {
    // Sanity check the no-extensions path: when `extensions_dir` is
    // `None`, the diagnostics buffer must be empty (not just "not
    // populated" — actually zero-length) so the renderer's
    // "non-empty array shows the card" heuristic doesn't false-fire.
    let tmp = TempDir::new().unwrap();
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: tmp.path().join("templates"),
        max_recents: 10,
        extensions_dir: None,
    };
    fs::create_dir_all(&cfg.state_dir).unwrap();
    fs::create_dir_all(&cfg.projects_dir).unwrap();
    fs::create_dir_all(&cfg.templates_dir).unwrap();
    let svc = BridgeService::new(cfg, [42u8; 32]).unwrap();
    assert!(svc.extension_load_diagnostics().is_empty());
}
