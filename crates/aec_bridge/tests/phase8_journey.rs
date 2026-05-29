//! Phase 8 end-to-end user journey — Extension system.
//!
//! Exercises the full extension-lifecycle contract from on-disk
//! manifest to bridge-surface visibility:
//!
//!   1. Plant a *signed* asset-pack extension on disk. The manifest
//!      ships an Ed25519 signature over the canonical manifest payload
//!      (sorted keys, no whitespace, signature field elided per
//!      [`aec_core::canonical_payload_bytes`]).
//!   2. Boot the bridge with [`BridgeConfig::extensions_dir`] pointed
//!      at the extensions root. The boot path loads every manifest
//!      via [`aec_core::ExtensionLoader`], derives a
//!      [`aec_core::PermissionEnforcer`] from the registry, and
//!      invokes [`aec_assets::install_asset_packs`] so every
//!      asset-pack extension's entries land in the asset library DB
//!      at `<state_dir>/asset_library/assets.sqlite`.
//!   3. Assert `BridgeService::design_list_assets` surfaces the
//!      extension's asset rows (named `phase8.assets.*`) alongside the
//!      seed library — the `designListAssets` equivalent listed in the
//!      task spec.
//!   4. Plant a template extension carrying a `TemplateDefinition`
//!      JSON next to its manifest. Assert
//!      `BridgeService::list_templates` reports the extension key and
//!      `BridgeService::project_create_from_template(key, name)`
//!      materialises a real `.aecstudio` package.
//!   5. Plant an AI tool extension that declares
//!      [`aec_core::Permission::AiTools`]. Assert
//!      `BridgeService::ai_list_tools` returns a descriptor for the
//!      extension's `tool_id` with the manifest-declared scopes and
//!      `max_entities_modified` cap.
//!   6. Plant a *second* AI tool extension that **omits** the
//!      `AiTools` permission. Assert `ai_list_tools` does NOT include
//!      it — the [`aec_ai::resolve_extension_ai_tool`] permission gate
//!      is the safe default for the planner's tool picker.
//!   7. Assert the bridge's permission enforcer denies
//!      [`aec_core::Operation::WriteGeometry`] for an extension that
//!      did not declare [`aec_core::Permission::GeometryWrite`] —
//!      this is the "extension without `write_project` cannot modify
//!      entities" check from the task spec.
//!
//! Signatures: each manifest carries a real Ed25519 signature minted
//! with the test-only [`aec_core::keygen_test_only`] helper. The
//! bridge boots with [`aec_core::LoadOptions::allow_unsigned`] (which
//! also accepts signed-but-untrusted manifests — production builds
//! tighten this by supplying a populated `TrustStore` once the
//! signing-key UX lands), so the signatures are verified against the
//! embedded public key at sign time via
//! [`aec_core::verify_signature_self_consistent`] rather than against
//! a trust store. That keeps this test focused on the
//! lifecycle/visibility contract while still exercising the canonical
//! signing path end-to-end.

use std::fs;
use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService};
use aec_core::extension_permissions::{keygen_test_only, verify_signature_self_consistent};
use aec_core::{
    canonical_payload_bytes, AssetEntry, AssetEntryKind, AssetPackBody, ExtensionId,
    ExtensionManifest, ExtensionRegistry, ExtensionSignature, ExtensionType, Operation, Permission,
    PermissionEnforcer, TemplateBody, TrustStore,
};
use ed25519_dalek::Signer;

fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

fn copy_shipped_apartment(dest: &Path) {
    let src = workspace_templates_dir()
        .join("interior")
        .join("apartment.json");
    let cat = dest.join("interior");
    fs::create_dir_all(&cat).unwrap();
    fs::copy(&src, cat.join("apartment.json")).unwrap();
}

/// Sign a manifest with the supplied key. The signature carries the
/// hex-encoded verifying key and the hex-encoded Ed25519 signature
/// over [`canonical_payload_bytes`].
///
/// Mutates the manifest in place so the on-disk JSON the loader reads
/// matches the bytes the signature was minted over — the canonical
/// payload elides the signature field by construction.
fn sign_manifest(m: &mut ExtensionManifest, sk: &ed25519_dalek::SigningKey) {
    let payload = canonical_payload_bytes(m);
    let sig = sk.sign(&payload);
    m.signature = Some(ExtensionSignature {
        algorithm: "ed25519".into(),
        public_key_hex: hex::encode(sk.verifying_key().to_bytes()),
        signature_hex: hex::encode(sig.to_bytes()),
    });
    // Eager self-consistency check: catches a manifest mutation
    // between sign time and file write time (the canonical-payload
    // function is deterministic, but a future refactor that
    // accidentally adds a field would surface here rather than as a
    // mysterious load failure downstream).
    let sig_ref = m.signature.as_ref().expect("signature just set");
    verify_signature_self_consistent(m, sig_ref).expect("self-consistent signature");
}

fn write_manifest(ext_dir: &Path, m: &ExtensionManifest) {
    fs::create_dir_all(ext_dir).unwrap();
    let json = serde_json::to_vec_pretty(m).unwrap();
    fs::write(ext_dir.join("manifest.json"), json).unwrap();
}

/// Plant an asset-pack extension carrying one furniture entry. The
/// extension's source asset is a small `.glb`-named byte buffer; the
/// host re-hashes it on install to detect tampering between sign
/// time and load time.
fn plant_asset_pack_extension(
    extensions_root: &Path,
    sk: &ed25519_dalek::SigningKey,
) -> (ExtensionId, String) {
    let ext_id = ExtensionId("phase8.assets".into());
    let ext_dir = extensions_root.join(&ext_id.0);
    fs::create_dir_all(ext_dir.join("furniture")).unwrap();

    // Real bytes on disk so the asset-pack host's blake3 verification
    // path is exercised end-to-end. The actual contents don't need
    // to be a valid glTF — `install_asset_packs` only reads + hashes;
    // mesh interpretation happens lazily via the asset query layer.
    let asset_bytes: Vec<u8> = b"PHASE8_TEST_ASSET_PAYLOAD_PADDING_TO_EXERCISE_TRIANGLE_ESTIMATE_AT_LEAST_ONE_HUNDRED_BYTES____xxx"
        .repeat(2);
    let rel = PathBuf::from("furniture/chair.glb");
    fs::write(ext_dir.join(&rel), &asset_bytes).unwrap();
    let blake = hex::encode(blake3::hash(&asset_bytes).as_bytes());

    let asset_id = format!("{}.chair", ext_id.0);
    let entry = AssetEntry {
        asset_id: asset_id.clone(),
        name: "Phase 8 chair".into(),
        kind: AssetEntryKind::Furniture,
        tags: vec!["phase8".into(), "demo".into()],
        source_path: rel,
        blake3: blake,
    };

    let mut manifest = ExtensionManifest {
        id: ext_id.clone(),
        name: "Phase 8 assets".into(),
        version: "1.0.0".into(),
        kind: ExtensionType::AssetPack,
        // FilesystemRead is required by the asset-pack host to read
        // the source file. GeometryRead is unused here but kept so
        // the same manifest can be referenced by the permission
        // assertion below ("does NOT have GeometryWrite").
        permissions: vec![Permission::FilesystemRead, Permission::GeometryRead],
        signature: None,
        license: "AGPL-3.0".into(),
        description: "Phase 8 asset pack journey fixture".into(),
        asset_pack: Some(AssetPackBody {
            vendor: "Phase 8 Studio".into(),
            entries: vec![entry],
        }),
        template: None,
        schedule: None,
        export_target: None,
        ai_tool: None,
        importer: None,
    };
    sign_manifest(&mut manifest, sk);
    write_manifest(&ext_dir, &manifest);
    (ext_id, asset_id)
}

fn plant_template_extension(
    extensions_root: &Path,
    sk: &ed25519_dalek::SigningKey,
) -> (ExtensionId, String) {
    let ext_id = ExtensionId("phase8.template".into());
    let template_key = "interior.phase8_loft".to_string();
    let ext_dir = extensions_root.join(&ext_id.0);
    fs::create_dir_all(ext_dir.join("templates")).unwrap();
    let rel = PathBuf::from("templates/loft.json");
    // Minimal-but-real TemplateDefinition JSON; the
    // template_apply::template_to_commands path needs `rooms`,
    // `default_walls`, `region_defaults`, and `units` to instantiate
    // entities into the project graph.
    let template_json = serde_json::json!({
        "template_id": template_key,
        "category": "interior",
        "name": "Phase 8 loft",
        "description": "Loft template shipped via extension for the Phase 8 journey test",
        "region_defaults": {
            "EU": { "units": "mm", "standards": ["EN ISO 5457"] }
        },
        "units": "mm",
        "rooms": [
            { "name": "Loft", "width_mm": 7_000, "depth_mm": 5_000, "height_mm": 3_200 }
        ],
        "default_walls": { "exterior_thickness_mm": 250, "interior_thickness_mm": 100 },
        "lighting_preset": "daylight",
        "asset_shelf": [],
        "camera_presets": []
    });
    fs::write(
        ext_dir.join(&rel),
        serde_json::to_vec_pretty(&template_json).unwrap(),
    )
    .unwrap();

    let mut manifest = ExtensionManifest {
        id: ext_id.clone(),
        name: "Phase 8 template".into(),
        version: "1.0.0".into(),
        kind: ExtensionType::Template,
        permissions: vec![Permission::FilesystemRead, Permission::GeometryRead],
        signature: None,
        license: "AGPL-3.0".into(),
        description: "Phase 8 template journey fixture".into(),
        asset_pack: None,
        template: Some(TemplateBody {
            key: template_key.clone(),
            definition_path: rel,
        }),
        schedule: None,
        export_target: None,
        ai_tool: None,
        importer: None,
    };
    sign_manifest(&mut manifest, sk);
    write_manifest(&ext_dir, &manifest);
    (ext_id, template_key)
}

/// Plant an AI tool extension. `grant_ai_tools = true` makes the
/// manifest declare [`Permission::AiTools`]; `false` omits it so the
/// permission gate in
/// [`aec_ai::resolve_extension_ai_tool`] denies it. The pair is what
/// drives the negative case in step 6 of the journey.
fn plant_ai_tool_extension(
    extensions_root: &Path,
    sk: &ed25519_dalek::SigningKey,
    ext_id_str: &str,
    tool_id: &str,
    grant_ai_tools: bool,
) -> ExtensionId {
    let ext_id = ExtensionId(ext_id_str.into());
    let ext_dir = extensions_root.join(&ext_id.0);
    fs::create_dir_all(&ext_dir).unwrap();
    let mut permissions = vec![Permission::GeometryRead];
    if grant_ai_tools {
        permissions.push(Permission::AiTools);
    }
    let mut manifest = ExtensionManifest {
        id: ext_id.clone(),
        name: format!("Phase 8 AI tool ({})", ext_id_str),
        version: "1.0.0".into(),
        kind: ExtensionType::AiTool,
        permissions,
        signature: None,
        license: "AGPL-3.0".into(),
        description: "Phase 8 AI tool journey fixture".into(),
        asset_pack: None,
        template: None,
        schedule: None,
        export_target: None,
        ai_tool: Some(aec_core::AiToolBody {
            tool_id: tool_id.into(),
            display_name: format!("{} (extension)", tool_id),
            description: "Extension-supplied tool for the phase 8 journey".into(),
            allowed_scopes: vec!["design".into(), "deliver".into()],
            // Cap matches the host `layout_suggestion` schema cap of 16,
            // so the advertised cap and the dispatch-side clamp both
            // resolve to 16 without lossy narrowing — the test pins the
            // happy-path "declared cap == host cap" branch of
            // `ai_list_tools`'s cap-clamp.
            max_entities_modified: 16,
            // Use a real built-in grammar_key (`layout_suggestion`)
            // rather than a synthetic one — `ai_list_tools` filters
            // extension AI tools whose grammar_key does not resolve to
            // a known host schema, matching the dispatch-side guard in
            // `resolve_ai_tool_alias`. Picking a fixture grammar that
            // doesn't exist in `ai_tools.json` would (correctly) hide
            // the tool from the picker and break the journey.
            grammar_key: "layout_suggestion".into(),
        }),
        importer: None,
    };
    sign_manifest(&mut manifest, sk);
    write_manifest(&ext_dir, &manifest);
    ext_id
}

/// Construct a [`TrustStore`] containing the verifying key the
/// fixture signed all four manifests with. Returned so the test can
/// assert the canonical-payload signatures actually pass a
/// production-shape trust-store check (the bridge boot path itself
/// uses `allow_unsigned`, but the signing/verification round-trip is
/// part of the contract the journey is documenting).
fn trust_store_for(sk: &ed25519_dalek::SigningKey) -> TrustStore {
    let pk_hex = hex::encode(sk.verifying_key().to_bytes());
    TrustStore::single_from_hex(&pk_hex).expect("valid hex key")
}

#[test]
fn phase8_extension_lifecycle_journey_through_bridge_service() {
    // ---- Step 0: scaffold a state dir + extensions dir + shipped
    //              template tree. The shipped apartment template is
    //              copied so `list_templates` shows BOTH the shipped
    //              key and the extension key (and the merge order
    //              comes out clean).
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    let extensions = tmp.path().join("extensions");
    fs::create_dir_all(&templates).unwrap();
    fs::create_dir_all(&extensions).unwrap();
    copy_shipped_apartment(&templates);

    // Deterministic per-test keypair via the helper documented for
    // exactly this use case (`getrandom` seeded; not for production).
    let (signing_key, _pk_hex) = keygen_test_only();

    // ---- Step 1+2: plant all extensions on disk.
    let (asset_ext_id, asset_id) = plant_asset_pack_extension(&extensions, &signing_key);
    let (template_ext_id, template_key) = plant_template_extension(&extensions, &signing_key);
    let ai_ok_ext_id = plant_ai_tool_extension(
        &extensions,
        &signing_key,
        "phase8.ai.ok",
        "phase8.ok",
        true, /* grant_ai_tools */
    );
    let ai_denied_ext_id = plant_ai_tool_extension(
        &extensions,
        &signing_key,
        "phase8.ai.denied",
        "phase8.denied",
        false, /* grant_ai_tools */
    );

    // ---- Signature round-trip parity: the four manifests verify
    //      against a real `TrustStore`. This isn't the boot-path
    //      check (the bridge uses `allow_unsigned`), but it asserts
    //      the canonical-payload signing pipeline our fixture uses
    //      matches what a production trust-store deployment would
    //      accept.
    let trust = trust_store_for(&signing_key);
    for ext_dir in [
        &asset_ext_id,
        &template_ext_id,
        &ai_ok_ext_id,
        &ai_denied_ext_id,
    ] {
        let path = extensions.join(&ext_dir.0).join("manifest.json");
        let manifest: ExtensionManifest =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let sig = manifest
            .signature
            .as_ref()
            .expect("fixture manifest must be signed");
        aec_core::verify_signature_against(&manifest, sig, &trust)
            .expect("trust-store verification round-trip");
    }

    // ---- Step 3: boot the bridge with the extensions dir wired in.
    let cfg = BridgeConfig {
        state_dir: state.clone(),
        projects_dir: projects.clone(),
        templates_dir: templates.clone(),
        max_recents: 10,
        extensions_dir: Some(extensions.clone()),
    };
    let mut svc = BridgeService::new(cfg, [0x8Au8; 32]).expect("boot BridgeService");

    // ---- Step 4: design_list_assets surfaces the extension's
    //              asset rows. We query by the entry's tag so the
    //              4-asset demo seed library doesn't drown the
    //              row we're checking.
    let asset_query = aec_bridge::AssetListQuery {
        search: None,
        tags: vec!["phase8".into()],
        style_tags: Vec::new(),
        limit: Some(16),
    };
    let assets = svc
        .design_list_assets(&asset_query)
        .expect("design_list_assets");
    assert!(
        assets.iter().any(|a| a.asset_id == asset_id),
        "extension asset {} not surfaced through design_list_assets; got {:?}",
        asset_id,
        assets.iter().map(|a| &a.asset_id).collect::<Vec<_>>()
    );

    // ---- Step 5: list_templates includes the extension's
    //              template key alongside the shipped apartment
    //              template. project_create_from_template against
    //              the extension key materialises a real project
    //              package on disk.
    let templates_listed = svc.list_templates().expect("list_templates");
    let listed_keys: Vec<&str> = templates_listed.iter().map(|t| t.key.as_str()).collect();
    assert!(
        listed_keys.contains(&"interior.apartment"),
        "shipped apartment template lost; saw {:?}",
        listed_keys
    );
    assert!(
        listed_keys.contains(&template_key.as_str()),
        "extension template `{}` not in list_templates; saw {:?}",
        template_key,
        listed_keys
    );

    let project_summary = svc
        .project_create_from_template(&template_key, "Phase 8 Loft")
        .expect("project_create_from_template (extension key)");
    assert!(
        Path::new(&project_summary.path).is_dir(),
        "extension-template-created project dir missing: {}",
        project_summary.path
    );
    assert!(
        Path::new(&project_summary.path)
            .join("manifest.json")
            .is_file(),
        "extension-template-created project missing manifest.json at {}",
        project_summary.path
    );

    // The project graph carries the wall entities the template
    // instantiator emitted from the loft room.
    let walls = svc
        .project_graph_list(&project_summary.path, Some("wall"))
        .expect("project_graph_list(wall)");
    assert!(
        !walls.is_empty(),
        "extension template instantiated zero walls; expected ≥1 from the loft room"
    );

    // ---- Step 6: ai_list_tools includes the permitted extension
    //              tool with the manifest-declared scopes and cap.
    let tools = svc.ai_list_tools().expect("ai_list_tools");
    let ok_tool = tools
        .iter()
        .find(|t| t.name == "phase8.ok")
        .expect("permitted extension AI tool not in ai_list_tools");
    assert_eq!(ok_tool.max_entities_modified, 16);
    assert_eq!(ok_tool.grammar_key, "layout_suggestion");
    let mut scopes = ok_tool.allowed_scopes.clone();
    scopes.sort();
    assert_eq!(
        scopes,
        vec!["deliver".to_string(), "design".to_string()],
        "extension AI tool scopes mismatch"
    );

    // ---- Step 7: ai_list_tools does NOT include the unprivileged
    //              extension tool. The permission gate in
    //              `resolve_extension_ai_tool` is the safe default —
    //              an extension without `AiTools` permission is
    //              omitted from the planner's tool picker.
    assert!(
        tools.iter().all(|t| t.name != "phase8.denied"),
        "extension AI tool without AiTools permission must NOT appear in ai_list_tools; got {:?}",
        tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // ---- Step 8: write-permission enforcement.
    //
    // The bridge's enforcer (built from the same registry at boot)
    // denies `WriteGeometry` for an extension that did not declare
    // `GeometryWrite`. We reconstruct the registry/enforcer the same
    // way the bridge does and verify the denial directly. This is
    // the "extension without write_project permission cannot modify
    // entities" check from the task spec.
    let registry = aec_core::ExtensionLoader::new(&extensions)
        .load(&aec_core::LoadOptions::allow_unsigned())
        .expect("registry reload");
    let enforcer = PermissionEnforcer::from_registry(&registry);

    // Asset pack ext declared FilesystemRead + GeometryRead → no
    // GeometryWrite → WriteGeometry is denied.
    let denied = enforcer.check_permission(&asset_ext_id, &Operation::WriteGeometry);
    assert!(
        !denied.allowed(),
        "asset_pack extension declared no GeometryWrite, but enforcer allowed WriteGeometry"
    );

    // Sanity: the same extension *does* hold the perms it declared —
    // ReadGeometry is allowed because GeometryRead is in its
    // manifest. This pins the enforcer's positive case so the
    // denial above isn't a vacuous "everything is denied"
    // assertion.
    let allowed = enforcer.check_permission(&asset_ext_id, &Operation::ReadGeometry);
    assert!(
        allowed.allowed(),
        "asset_pack extension declared GeometryRead, but enforcer denied ReadGeometry"
    );

    // And: an unknown extension id surfaces as Denied for every
    // operation. The enforcer is the bridge's safe-default fence.
    let unknown = ExtensionId("never.declared".into());
    let check = enforcer.check_permission(&unknown, &Operation::ReadGeometry);
    assert!(
        !check.allowed(),
        "enforcer must deny unknown extension id; got allow for {}",
        unknown
    );

    // Hold registry alive to the test's end so its `LoadedExtension`
    // root paths (which the loader records as relative-to-extensions
    // root) point at the still-existing tempdir.
    let _ = ai_denied_ext_id;
    drop(registry);
    drop(svc);
    drop(tmp);
}

#[test]
fn phase8_bridge_default_extensions_dir_yields_empty_registry() {
    // Sanity case: when `extensions_dir = None` (the pre-Phase-14
    // default), the bridge boots cleanly, every surface still works,
    // and the extension-aware lists are empty / shipped-only.
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    fs::create_dir_all(&templates).unwrap();
    copy_shipped_apartment(&templates);

    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, [0x8Bu8; 32]).expect("boot BridgeService");

    let tpls = svc.list_templates().expect("list_templates");
    assert!(
        tpls.iter().any(|t| t.key == "interior.apartment"),
        "shipped apartment missing in no-extensions boot"
    );

    // ai_list_tools returns only built-in tools — no extension
    // descriptors creep into the no-extensions boot path.
    let tools = svc.ai_list_tools().expect("ai_list_tools");
    assert!(
        tools.iter().all(|t| !t.name.starts_with("phase8.")),
        "no-extensions boot leaked extension tools: {:?}",
        tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // The reconstructed registry (mirroring what the bridge holds)
    // is empty — and the enforcer denies every operation.
    let registry = ExtensionRegistry::default();
    let enforcer = PermissionEnforcer::from_registry(&registry);
    let check = enforcer.check_permission(&ExtensionId("any".into()), &Operation::ReadGeometry);
    assert!(!check.allowed());
}
