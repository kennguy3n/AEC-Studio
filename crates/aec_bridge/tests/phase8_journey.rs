//! Phase 13 Task 30 — Phase 8 user-journey e2e (Extension system).
//!
//! User journey (PROPOSAL.md "User Journey H" / Phase 8 section):
//!   1. Author a real `AssetPack` extension on disk (manifest +
//!      payload bytes hashed via BLAKE3 — no fixture mocking,
//!      the manifest hash is *re*computed at the same algorithm
//!      the loader uses, which the loader then re-checks).
//!   2. Install it via the bridge's
//!      [`BridgeService::extensions_install_asset_packs`].
//!   3. Verify the installed assets are visible to the renderer
//!      through `design_list_assets`.
//!   4. Verify re-running the install is a no-op (idempotence —
//!      every entry comes back on the `skipped` list).
//!   5. Author a *second* extension that mutates a payload byte
//!      after the manifest was authored. The on-disk file's
//!      BLAKE3 no longer matches what the manifest declares;
//!      the install must fail with a structured checksum error.
//!   6. Author a *third* extension that omits the
//!      `filesystem_read` permission. The install must fail
//!      with a permission-denied error before touching the
//!      filesystem.
//!
//! Driven entirely through the public `BridgeService` API — no
//! private fields touched. The extensions are constructed by
//! emitting real `manifest.json` / payload files on disk, which
//! the bridge then loads through the production `ExtensionLoader`.

use std::path::{Path, PathBuf};

use aec_bridge::{AssetListQuery, BridgeConfig, BridgeService};
use aec_core::extensions::{
    AssetEntry, AssetEntryKind, AssetPackBody, ExtensionId, ExtensionManifest, ExtensionType,
    Permission,
};

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let svc = BridgeService::new(cfg, [0x4Cu8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

/// Author a real on-disk `AssetPack` extension:
///
/// * Writes the payload files under `<root>/<id>/<source_path>`
///   so the loader's filesystem walk finds them.
/// * Computes BLAKE3 of the payload bytes and bakes the hex
///   digest into the manifest's `AssetEntry::blake3` field so
///   the loader's `BlobChecksumMismatch` check passes on the
///   first install.
/// * Marshals the manifest as `manifest.json` next to the
///   payload tree.
///
/// `permissions` is configurable so the caller can author a
/// "no `filesystem_read`" extension to exercise the
/// permission-denied path.
fn write_asset_pack(
    root: &Path,
    ext_id: &str,
    permissions: Vec<Permission>,
    entries: Vec<(String, Vec<u8>)>,
) {
    let ext_dir = root.join(ext_id);
    std::fs::create_dir_all(ext_dir.join("furniture")).unwrap();
    let mut body_entries = Vec::with_capacity(entries.len());
    for (asset_name, bytes) in entries {
        let rel = PathBuf::from("furniture").join(format!("{asset_name}.glb"));
        std::fs::write(ext_dir.join(&rel), &bytes).unwrap();
        let blake3 = hex::encode(blake3::hash(&bytes).as_bytes());
        body_entries.push(AssetEntry {
            asset_id: format!("{ext_id}.{asset_name}"),
            name: asset_name.clone(),
            kind: AssetEntryKind::Furniture,
            tags: vec!["test".into(), "journey".into()],
            source_path: rel,
            blake3,
        });
    }
    let manifest = ExtensionManifest {
        id: ExtensionId(ext_id.to_string()),
        name: ext_id.to_string(),
        version: "1.0.0".into(),
        kind: ExtensionType::AssetPack,
        permissions,
        signature: None,
        license: "AGPL-3.0".into(),
        description: "phase 8 journey fixture".into(),
        asset_pack: Some(AssetPackBody {
            vendor: "JourneyVendor".into(),
            entries: body_entries,
        }),
        template: None,
        schedule: None,
        export_target: None,
        ai_tool: None,
        importer: None,
    };
    let manifest_path = ext_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn phase8_extension_lifecycle_journey() {
    let (svc, _g) = boot_service();

    // ── Step 1: author a happy-path extension ──
    let ext_root = tempfile::tempdir().unwrap();
    write_asset_pack(
        ext_root.path(),
        "journey.living_room",
        vec![Permission::FilesystemRead, Permission::GeometryRead],
        vec![
            ("sofa_l1".into(), b"phase8-journey-sofa-payload".to_vec()),
            ("chair_l1".into(), b"phase8-journey-chair-payload".to_vec()),
        ],
    );

    // ── Step 2: install via the bridge ──
    let summary = svc
        .extensions_install_asset_packs(ext_root.path().to_str().unwrap(), false)
        .expect("install_asset_packs (happy path)");
    assert_eq!(
        summary.installed.len(),
        2,
        "two entries should land on first install; got {:?}",
        summary
    );
    assert!(
        summary.skipped.is_empty(),
        "no entry should be on the skipped list for a first install"
    );

    // ── Step 3: design_list_assets returns the new assets ──
    let listed = svc
        .design_list_assets(&AssetListQuery {
            search: None,
            tags: vec!["journey".into()],
            style_tags: Vec::new(),
            limit: Some(100),
        })
        .expect("design_list_assets after install");
    assert!(
        listed
            .iter()
            .any(|a| a.asset_id == "journey.living_room.sofa_l1"),
        "installed sofa must appear in the asset list; got {:?}",
        listed.iter().map(|a| &a.asset_id).collect::<Vec<_>>()
    );
    assert!(
        listed
            .iter()
            .any(|a| a.asset_id == "journey.living_room.chair_l1"),
        "installed chair must appear in the asset list; got {:?}",
        listed.iter().map(|a| &a.asset_id).collect::<Vec<_>>()
    );

    // ── Step 4: re-running the install is idempotent ──
    let summary2 = svc
        .extensions_install_asset_packs(ext_root.path().to_str().unwrap(), false)
        .expect("install_asset_packs (idempotent re-run)");
    assert!(
        summary2.installed.is_empty(),
        "no new entries should land on a re-run; got {:?}",
        summary2.installed
    );
    assert_eq!(
        summary2.skipped.len(),
        2,
        "both pre-existing entries must be on the skipped list; got {:?}",
        summary2.skipped
    );

    // ── Step 5: tampered payload is rejected by checksum ──
    let tampered_root = tempfile::tempdir().unwrap();
    write_asset_pack(
        tampered_root.path(),
        "journey.tampered",
        vec![Permission::FilesystemRead, Permission::GeometryRead],
        vec![("tampered_1".into(), b"original-bytes".to_vec())],
    );
    // Overwrite the file AFTER the manifest was authored. The
    // loader's BLAKE3 check must catch this — even in
    // `allow_unsigned()` mode, payload tampering is a hard error
    // because the manifest binds the hash to the asset id.
    let tampered_blob = tampered_root
        .path()
        .join("journey.tampered")
        .join("furniture")
        .join("tampered_1.glb");
    std::fs::write(&tampered_blob, b"TAMPERED-AFTER-MANIFEST").unwrap();
    let tamper_err = svc
        .extensions_install_asset_packs(tampered_root.path().to_str().unwrap(), false)
        .expect_err("tampered payload must produce a checksum error");
    let msg = format!("{tamper_err:?}");
    assert!(
        msg.to_lowercase().contains("blake3")
            || msg.to_lowercase().contains("hash")
            || msg.to_lowercase().contains("checksum"),
        "tampered-payload error message must mention the hash mismatch; got: {msg}"
    );

    // ── Step 6: extension without filesystem_read is rejected ──
    let nogeom_root = tempfile::tempdir().unwrap();
    write_asset_pack(
        nogeom_root.path(),
        "journey.no_perm",
        // Note: only `geometry_read`. Missing `filesystem_read`
        // is the prohibited combination — the host needs
        // `filesystem_read` to read the payload off disk on
        // behalf of the extension.
        vec![Permission::GeometryRead],
        vec![("blocked_1".into(), b"never-touched".to_vec())],
    );
    let perm_err = svc
        .extensions_install_asset_packs(nogeom_root.path().to_str().unwrap(), false)
        .expect_err("missing filesystem_read must produce a permission error");
    let perm_msg = format!("{perm_err:?}");
    assert!(
        perm_msg.to_lowercase().contains("permission"),
        "permission-denied error message must mention the missing permission; got: {perm_msg}"
    );
}
