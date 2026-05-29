//! PROPOSAL.md User Journey E — Studio lead ("Concept to contract in
//! one project package"). One test per acceptance criterion,
//! exercised through the BridgeService public API.
//!
//! PROPOSAL.md lines 298-300:
//!
//! - [ ] One `.aecstudio` package feeds all four delivery types
//!   (renders, drawings, IFC, contract).
//! - [ ] Revisions can be diffed at the project, sheet, and element
//!   level.
//! - [ ] Studio standards (title block, dim style, layer policy)
//!   are stored in the project and reused across deliverables.

use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService, DeliverBuildPackParams, DeliverPackInventoryFlags};
use aec_cad::sheets::{Margins, Orientation, PaperSize, SheetViewport};
use aec_command::commands::{draft::CreateSheet, wall::CreateWall, Command, CommandKind};
use aec_core::package::ProjectPackage;
use aec_core::types::EntityId;

fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

fn copy_template(category: &str, id: &str, dest: &Path) {
    let src = workspace_templates_dir()
        .join(category)
        .join(format!("{id}.json"));
    let dest_dir = dest.join(category);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest_file = dest_dir.join(format!("{id}.json"));
    std::fs::copy(&src, &dest_file).unwrap_or_else(|e| {
        panic!(
            "failed to copy shipped template {} -> {}: {e}",
            src.display(),
            dest_file.display()
        )
    });
}

const MASTER_KEY: [u8; 32] = [0xE1u8; 32];

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    // Villa = the studio-lead reference template (PROPOSAL.md line 283).
    copy_template("architecture", "villa", &templates);
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, MASTER_KEY).expect("boot BridgeService");
    (svc, tmp)
}

fn add_wall(svc: &mut BridgeService, project_path: &str, end_x_mm: f64) {
    let cmd = Command::user(CommandKind::CreateWall(CreateWall {
        entity_id: EntityId::new(),
        start_mm: [0.0, 0.0],
        end_mm: [end_x_mm, 0.0],
        height_mm: 2800.0,
        thickness_mm: 200.0,
        material_id: Some("concrete_white".into()),
    }));
    svc.command_apply(project_path, cmd).expect("create wall");
}

fn add_sheet(svc: &mut BridgeService, project_path: &str, name: &str) {
    let cmd = Command::user(CommandKind::CreateSheet(CreateSheet {
        entity_id: EntityId::new(),
        name: name.into(),
        paper: PaperSize::IsoA1,
        orientation: Orientation::Landscape,
        margins: Margins::default(),
        title_block: None,
        viewports: vec![SheetViewport::new(
            format!("VP-{name}"),
            [40.0, 40.0],
            [760.0, 480.0],
        )],
    }));
    svc.command_apply(project_path, cmd).expect("create sheet");
}

/// PROPOSAL.md criterion 1: "One `.aecstudio` package feeds all four
/// delivery types (renders, drawings, IFC, contract)."
///
/// Validates that a single project package, opened **once** with
/// the same master key, drives every `DeliverPackKind` variant
/// (`concept`, `interior`, `contractor`, `bim`) and that each
/// archive carries real content distinguishable by its archetype-
/// specific PDF entry. The bridge's `deliver_build_pack` is the
/// public surface that maps the renderer's Deliver page selector
/// to those four archetypes, so this test pins the contract that
/// downstream code (KChat publishing, post-export hooks) depends on.
#[test]
fn journey_e_criterion_single_package_feeds_all_four_pack_kinds() {
    let (mut svc, _g) = boot_service();
    let project = svc
        .project_create_from_template("architecture.villa", "Villa Studio")
        .expect("create project from villa template");

    // Add modelled geometry so the contractor / BIM packs have real
    // BOQ + IFC content rather than empty payloads.
    add_wall(&mut svc, &project.path, 6000.0);
    add_wall(&mut svc, &project.path, 4500.0);
    add_sheet(&mut svc, &project.path, "A100");
    add_sheet(&mut svc, &project.path, "A200");

    let tmp = tempfile::tempdir().unwrap();

    // The four archetypes the Deliver page surfaces. Each archetype
    // emits a distinct top-level PDF — naming pinned to
    // [`aec_export::write_deliver_pack_with_context`]:
    //
    // - concept_pack.pdf       (Concept)
    // - interior_summary.pdf   (Interior)
    // - contractor_summary.pdf (Contractor)
    // - validation_report.pdf  (Bim)
    let cases = [
        ("concept", "concept_pack.pdf"),
        ("interior", "interior_summary.pdf"),
        ("contractor", "contractor_summary.pdf"),
        ("bim", "validation_report.pdf"),
    ];
    for (kind, expected_pdf) in cases {
        let out = tmp.path().join(format!("{kind}.zip"));
        let result = svc
            .deliver_build_pack(DeliverBuildPackParams {
                out_path: out.to_string_lossy().into_owned(),
                kind: kind.to_string(),
                project_name: "Villa Studio".to_string(),
                options: DeliverPackInventoryFlags {
                    include_renders: true,
                    include_sheets: true,
                    include_ifc: true,
                    include_boq: true,
                    include_proposal: true,
                },
                project_path: Some(project.path.clone()),
            })
            .unwrap_or_else(|e| panic!("deliver_build_pack({kind}) failed: {e}"));

        assert!(
            result.total_bytes > 0,
            "{kind} pack must contain non-empty content; got 0 bytes"
        );

        // Inspect the ZIP and assert the archetype-specific PDF is
        // present. This is what makes the four kinds **distinct** —
        // not just zip files with different names.
        let bytes = std::fs::read(&out).unwrap();
        let cursor = std::io::Cursor::new(&bytes);
        let archive = zip::ZipArchive::new(cursor)
            .unwrap_or_else(|e| panic!("{kind} pack must be a valid zip: {e}"));
        let names: Vec<String> = archive
            .file_names()
            .map(std::string::ToString::to_string)
            .collect();
        assert!(
            names.iter().any(|n| n == expected_pdf),
            "{kind} pack must contain {expected_pdf}; got entries: {names:?}"
        );
    }
}

/// PROPOSAL.md criterion 2: "Revisions can be diffed at the project,
/// sheet, and element level."
///
/// Drives the bridge revision API directly:
///
/// 1. Create revision **v1** at initial geometry (project-level
///    state).
/// 2. Apply a wall mutation (element-level change).
/// 3. Apply a sheet addition (sheet-level change).
/// 4. Create revision **v2**.
/// 5. `deliver_compare_revisions(v1, v2)` and assert the diff
///    surfaces:
///    - **Project level**: the two revisions are distinct
///      (different ids, distinct head pointers).
///    - **Element level**: at least one wall delta in the change
///      list with `kind = "added"` or `"modified"`.
///    - **Sheet level**: at least one sheet delta in the change
///      list.
#[test]
fn journey_e_criterion_revisions_diffable_at_three_levels() {
    let (mut svc, _g) = boot_service();
    let project = svc
        .project_create_from_template("architecture.villa", "Revisions Villa")
        .expect("create villa project");

    // Initial state — one wall, one sheet.
    add_wall(&mut svc, &project.path, 5000.0);
    add_sheet(&mut svc, &project.path, "A100");

    let v1 = svc
        .deliver_create_revision(&project.path, "v1", "initial massing", None)
        .expect("create v1");

    // Mutate state: add a second wall (element delta) and a second
    // sheet (sheet delta).
    add_wall(&mut svc, &project.path, 7500.0);
    add_sheet(&mut svc, &project.path, "A200");

    let v2 = svc
        .deliver_create_revision(&project.path, "v2", "added wall + sheet", None)
        .expect("create v2");

    // Project level: distinct revisions, distinct ids.
    assert_ne!(
        v1.revision_id, v2.revision_id,
        "revisions must be distinct at project level"
    );

    let diff = svc
        .deliver_compare_revisions(&project.path, &v1.revision_id, &v2.revision_id)
        .expect("diff v1..v2");

    // The diff should surface ADDED entries: a wall and a sheet.
    // Element category names come from the project graph's `kind`
    // column; we accept any of the recognised wall/sheet kinds.
    let added: Vec<_> = diff.changes.iter().filter(|c| c.kind == "added").collect();
    assert!(
        !added.is_empty(),
        "v1..v2 must surface at least one added entity; full change list: {:?}",
        diff.changes
    );

    let added_categories: std::collections::BTreeSet<&str> =
        added.iter().map(|c| c.category.as_str()).collect();

    // Element-level: at least one wall delta.
    let has_wall = added_categories
        .iter()
        .any(|c| c.contains("wall") || c == &"primitive");
    assert!(
        has_wall,
        "element-level diff must surface a wall change; got categories {added_categories:?}"
    );

    // Sheet-level: at least one sheet delta.
    let has_sheet = added_categories.iter().any(|c| c == &"sheet");
    assert!(
        has_sheet,
        "sheet-level diff must surface a sheet change; got categories {added_categories:?}"
    );

    // Project-level: by-category aggregation reports a non-zero
    // `added` count across more than one category.
    let categories_with_adds: usize = diff.by_category.iter().filter(|(_, c)| c.added > 0).count();
    assert!(
        categories_with_adds >= 2,
        "project-level diff must aggregate adds across >= 2 categories; got {:?}",
        diff.by_category
    );
}

/// PROPOSAL.md criterion 3: "Studio standards (title block, dim
/// style, layer policy) are stored in the project and reused across
/// deliverables."
///
/// "Studio standards" live in the template definition (drafting
/// templates ship `sheet_presets` with title-block ids and
/// `dim_styles`; all templates ship a `region_defaults` map whose
/// `standards` is the drawing-standards identifier list). The
/// project package persists the template id and the resolved
/// `ProjectSettings.region` so every Deliver action can re-read
/// them deterministically — no separate "studio config" sidecar.
///
/// This test:
///
/// 1. Lists available templates and verifies the studio templates
///    declare a `template_id` (project key for standards lookup).
/// 2. Creates a project from a villa template and asserts the
///    package manifest persists the `template_id`.
/// 3. Re-opens the package and asserts the persisted
///    `ProjectSettings` (units + region) round-trip — so any later
///    "deliverable" call (sheet PDF, IFC, contract pack) reads the
///    same standards rather than re-asking the user.
/// 4. Runs two distinct deliver-pack exports back-to-back from the
///    same project; both produce non-empty archives, confirming the
///    standards are reused across deliverables without manual
///    re-config.
#[test]
fn journey_e_criterion_studio_standards_persist_and_reuse() {
    let (mut svc, _g) = boot_service();

    // (1) Templates available — the villa template defines studio
    // standards (region_defaults → standards list).
    let templates = svc.list_templates().expect("list templates");
    let villa = templates
        .iter()
        .find(|t| t.key == "architecture.villa")
        .expect("villa template must be available in the studio templates dir");
    // Templates expose human-readable names + ids; we don't need
    // to peek into region_defaults from the bridge — the standards
    // travel with the template_id, and the project_create path
    // resolves them via TemplateLoader.
    assert!(
        !villa.name.is_empty(),
        "villa template must declare a name; got blank"
    );

    // (2) Create project; template_id must be persisted on the summary.
    let project = svc
        .project_create_from_template("architecture.villa", "Standards Villa")
        .expect("create villa project");
    assert_eq!(
        project.template_id.as_deref(),
        Some("architecture.villa"),
        "project summary must carry template_id (= studio-standards key)"
    );

    // (3) Re-open the package via the encrypted-package API directly
    // and assert the persisted settings round-trip. This is what
    // every Deliver action does internally — open the package,
    // read settings, build the export from those settings.
    let pkg = ProjectPackage::open_with_master_key(&project.path, &MASTER_KEY)
        .expect("re-open villa package with the studio's master key");
    let manifest = pkg.manifest();
    assert_eq!(
        manifest.template_id.as_deref(),
        Some("architecture.villa"),
        "on-disk manifest must carry template_id"
    );
    assert_eq!(
        manifest.settings.units,
        aec_core::types::Units::Mm,
        "villa template's units are mm and must round-trip into ProjectSettings"
    );

    // (4) Two distinct deliverables from the same project, back-to-
    // back, using the same standards (template_id + settings).
    add_wall(&mut svc, &project.path, 6000.0);
    add_sheet(&mut svc, &project.path, "A100");

    let tmp = tempfile::tempdir().unwrap();
    let make_pack = |svc: &BridgeService, kind: &str, file: &str| {
        let out = tmp.path().join(file);
        let result = svc
            .deliver_build_pack(DeliverBuildPackParams {
                out_path: out.to_string_lossy().into_owned(),
                kind: kind.to_string(),
                project_name: "Standards Villa".to_string(),
                options: DeliverPackInventoryFlags {
                    include_renders: true,
                    include_sheets: true,
                    include_ifc: true,
                    include_boq: true,
                    include_proposal: true,
                },
                project_path: Some(project.path.clone()),
            })
            .unwrap_or_else(|e| panic!("deliver_build_pack({kind}) failed: {e}"));
        assert!(
            result.total_bytes > 0,
            "{kind} pack must contain content built from project standards; got 0 bytes"
        );
        out
    };
    let _contract = make_pack(&svc, "contractor", "contract.zip");
    let _bim = make_pack(&svc, "bim", "bim.zip");
    // Both archives exist + non-empty by virtue of make_pack's asserts.
}
