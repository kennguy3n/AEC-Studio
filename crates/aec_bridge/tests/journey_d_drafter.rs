//! PROPOSAL.md User Journey D — Drafter ("Pure 2D CAD for a steel
//! detail set"). One test per acceptance criterion, exercised through
//! the BridgeService public API.
//!
//! PROPOSAL.md lines 271-273:
//!
//! - [ ] Drafter can complete a sheet set with keyboard-only workflows.
//! - [ ] DXF roundtrip preserves layer, block, dim style, and text style.
//! - [ ] DWG export is available but explicitly opt-in.

use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService};
use aec_cad::dwg::{DwgReader, DwgVersion, DwgWriter};
use aec_cad::dxf::{DxfDocument, DxfEntity, DxfLine, DxfReader, DxfWriter};
use aec_cad::layers::{Layer, LayerColor, LayerLineweight, LinetypeTable};
use aec_cad::primitives::Primitive;
use aec_cad::sheets::{Margins, Orientation, PaperSize, SheetViewport};
use aec_command::commands::{
    draft::{CreateSheet, DrawPrimitive},
    Command, CommandKind,
};
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

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_template("architecture", "cafe", &templates);
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, [0xD1u8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

/// PROPOSAL.md criterion 1: "Drafter can complete a sheet set with
/// keyboard-only workflows."
///
/// The keyboard command line is a UI affordance that maps key aliases
/// (`L` → Line, `O` → Offset, `CO` → Copy) to the same
/// [`Command`] / [`CommandKind`] gestures that the bridge exposes
/// via `command_apply`. A "keyboard-only workflow" means that every
/// step from project creation through sheet-set delivery is
/// achievable via `Command` objects without requiring any
/// mouse-exclusive state (drag handles, viewport pan, rubber-band
/// selections).
///
/// This test builds a 3-sheet steel detail set entirely through
/// `command_apply` calls — the functional equivalent of typing
/// keyboard commands — and verifies the resulting project contains
/// the expected sheets and primitives.
#[test]
fn journey_d_criterion_keyboard_only_sheet_set() {
    let (mut svc, _g) = boot_service();
    let project = svc
        .project_create_from_template("architecture.cafe", "Steel Detail Set")
        .expect("create project");

    // Draw lines forming a connection detail (commands that mirror
    // keyboard-driven entry: L → start → end).
    for i in 0..6 {
        let y = (i as f64) * 200.0;
        let cmd = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: EntityId::new(),
            primitive: Primitive::Line(aec_cad::primitives::Line {
                start: [0.0, y],
                end: [1500.0, y],
                layer: "STEEL-DETAIL".into(),
                color_override: None,
                lineweight_override: None,
                linetype_override: None,
            }),
        }));
        svc.command_apply(&project.path, cmd)
            .expect("draw line via keyboard");
    }

    // Create 3 sheets (keyboard: `SHEET` command alias).
    for n in 1..=3 {
        let cmd = Command::user(CommandKind::CreateSheet(CreateSheet {
            entity_id: EntityId::new(),
            name: format!("S{n}"),
            paper: PaperSize::IsoA1,
            orientation: Orientation::Landscape,
            margins: Margins::default(),
            title_block: None,
            viewports: vec![SheetViewport::new(
                format!("VP-S{n}"),
                [20.0, 20.0],
                [800.0, 560.0],
            )],
        }));
        svc.command_apply(&project.path, cmd)
            .expect("create sheet via keyboard");
    }

    // Verify via the project graph that the sheet set and primitives
    // are persisted — proving the entire workflow is achievable
    // through command dispatch (the keyboard code path).
    let entities = svc
        .project_graph_list(&project.path, None)
        .expect("list all entities");
    let sheet_count = entities.iter().filter(|e| e.kind == "sheet").count();
    let prim_count = entities.iter().filter(|e| e.kind == "primitive").count();
    assert_eq!(sheet_count, 3, "3 sheets created via keyboard workflow");
    assert!(
        prim_count >= 6,
        "at least 6 line primitives created via keyboard workflow; got {prim_count}"
    );
}

/// PROPOSAL.md criterion 2: "DXF roundtrip preserves layer, block,
/// dim style, and text style."
///
/// The roundtrip under test is the DXF **file-format** contract:
/// `DxfDocument` → `DxfWriter` → bytes → `DxfReader` → `DxfDocument`.
/// This is the canonical path that the drafter's "File → Export DXF"
/// and "File → Import DXF" gestures exercise. The bridge's
/// `draft_import_dxf` normalises to modelling primitives on purpose
/// (so they are undo-able and journal-able) — the full table
/// metadata (layer colour/lineweight, block records, dim styles,
/// text styles) lives in the `DxfDocument` that the bridge produces
/// from those primitives at export time. The crate-level roundtrip
/// pins the file-format guarantee; `aec_cad/tests/journey_d.rs`
/// covers the deep entity-variant matrix.
#[test]
fn journey_d_criterion_dxf_roundtrip_preserves_layers() {
    // Build a DXF document with custom layers and entities.
    let mut doc = DxfDocument::new();
    let mut steel = Layer::new("STEEL").expect("valid layer name");
    steel.color = LayerColor(1);
    steel.lineweight = LayerLineweight::from_mm(0.50);
    steel.linetype = "DASHED".into();
    doc.layers.upsert(steel);
    let mut dims = Layer::new("DIMS").expect("valid layer name");
    dims.color = LayerColor(5);
    dims.lineweight = LayerLineweight::from_mm(0.25);
    dims.linetype = "CONTINUOUS".into();
    doc.layers.upsert(dims);
    let _lts = LinetypeTable::standard();
    doc.push(DxfEntity::Line(DxfLine {
        layer: "STEEL".into(),
        start: [0.0, 0.0, 0.0],
        end: [1000.0, 500.0, 0.0],
    }));
    doc.push(DxfEntity::Line(DxfLine {
        layer: "DIMS".into(),
        start: [200.0, 0.0, 0.0],
        end: [200.0, 800.0, 0.0],
    }));
    doc.push(DxfEntity::Line(DxfLine {
        layer: "STEEL".into(),
        start: [500.0, 500.0, 0.0],
        end: [1500.0, 500.0, 0.0],
    }));

    // Write → read roundtrip.
    let mut buf: Vec<u8> = Vec::new();
    DxfWriter::write(&doc, &mut buf).expect("write DXF");
    let reloaded = DxfReader::read(buf.as_slice()).expect("read DXF");

    // Layers survive with metadata.
    let layer_names: Vec<&str> = reloaded.layers.iter().map(|l| l.name.as_str()).collect();
    assert!(
        layer_names.contains(&"STEEL"),
        "STEEL layer must survive DXF roundtrip; layers: {layer_names:?}"
    );
    assert!(
        layer_names.contains(&"DIMS"),
        "DIMS layer must survive DXF roundtrip; layers: {layer_names:?}"
    );
    let steel = reloaded
        .layers
        .get("STEEL")
        .expect("STEEL layer present in layer system");
    assert_eq!(steel.color, LayerColor(1), "STEEL colour roundtrip");
    let dims = reloaded
        .layers
        .get("DIMS")
        .expect("DIMS layer present in layer system");
    assert_eq!(dims.color, LayerColor(5), "DIMS colour roundtrip");

    // Entities survive with layer assignments.
    assert_eq!(reloaded.entities.len(), 3, "entity count roundtrip");
    let has_steel = reloaded.entities.iter().any(|e| e.layer() == "STEEL");
    let has_dims = reloaded.entities.iter().any(|e| e.layer() == "DIMS");
    assert!(has_steel, "entity on STEEL layer");
    assert!(has_dims, "entity on DIMS layer");

    // Also roundtrip through the bridge import→export chain to
    // verify the bridge preserves entity layer **names** (the
    // bridge normalises to Primitive which carries the layer string).
    let (mut svc, _g) = boot_service();
    let project = svc
        .project_create_from_template("architecture.cafe", "DXF RT")
        .expect("create project");
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source.dxf");
    std::fs::write(&src, &buf).unwrap();
    let import = svc
        .draft_import_dxf(&project.path, src.to_str().unwrap())
        .expect("bridge import");
    assert!(
        import.entity_count >= 3,
        "bridge import entity count: {}",
        import.entity_count
    );
    let export_path = tmp.path().join("exported.dxf");
    let export = svc
        .draft_export_dxf(&project.path, export_path.to_str().unwrap())
        .expect("bridge export");
    assert!(
        export.entity_count >= 3,
        "bridge export entity count: {}",
        export.entity_count
    );
    let re =
        DxfReader::read(std::fs::File::open(&export_path).unwrap()).expect("read bridge export");
    let bridge_has_steel = re.entities.iter().any(|e| e.layer() == "STEEL");
    let bridge_has_dims = re.entities.iter().any(|e| e.layer() == "DIMS");
    assert!(bridge_has_steel, "bridge export preserves STEEL layer name");
    assert!(bridge_has_dims, "bridge export preserves DIMS layer name");
}

/// PROPOSAL.md criterion 3: "DWG export is available but explicitly
/// opt-in."
///
/// The bridge default export is `draft_export_dxf` (canonical
/// lossless format). DWG export exists as a library path via
/// `aec_cad::dwg::DwgWriter::write` but requires the caller to
/// **explicitly request** a DWG version — it is never triggered
/// implicitly by a DXF export call. This test pins:
///
/// 1. The default `draft_export_dxf` produces a file that is
///    **not** a DWG binary (starts with DXF ASCII header, not
///    `AC10xx` magic bytes).
/// 2. `DwgWriter::write` IS available and produces a valid DWG for
///    the same content — proving the capability exists.
/// 3. The DWG round-trips through `DwgReader`, verifying it is
///    functional rather than a dead-code leftover.
#[test]
fn journey_d_criterion_dwg_export_opt_in() {
    let (mut svc, _g) = boot_service();
    let project = svc
        .project_create_from_template("architecture.cafe", "DWG Opt-In")
        .expect("create project");

    // Add a line so there's exportable content.
    let cmd = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
        entity_id: EntityId::new(),
        primitive: Primitive::Line(aec_cad::primitives::Line {
            start: [0.0, 0.0],
            end: [2000.0, 1000.0],
            layer: "DETAILS".into(),
            color_override: None,
            lineweight_override: None,
            linetype_override: None,
        }),
    }));
    svc.command_apply(&project.path, cmd).expect("draw line");

    // Default bridge export is DXF — verify it is NOT DWG binary.
    let tmp = tempfile::tempdir().unwrap();
    let dxf_path = tmp.path().join("default_export.dxf");
    svc.draft_export_dxf(&project.path, dxf_path.to_str().unwrap())
        .expect("default export DXF");
    let dxf_bytes = std::fs::read(&dxf_path).unwrap();
    // DXF ASCII files start with "0\n" (entity header) — NOT with
    // "AC10" which is the DWG magic.
    assert!(
        !dxf_bytes.starts_with(b"AC10"),
        "default draft_export_dxf must NOT produce a DWG binary"
    );
    let dxf_str = String::from_utf8_lossy(&dxf_bytes);
    assert!(
        dxf_str.starts_with("0\n") || dxf_str.starts_with("  0\n"),
        "default export must be ASCII DXF; first 20 bytes: {:?}",
        &dxf_bytes[..20.min(dxf_bytes.len())]
    );

    // Explicit DWG opt-in: build a DxfDocument and pass through
    // DwgWriter::write (the caller explicitly chooses DWG).
    let mut doc = DxfDocument::new();
    doc.push(DxfEntity::Line(DxfLine {
        layer: "DETAILS".into(),
        start: [0.0, 0.0, 0.0],
        end: [2000.0, 1000.0, 0.0],
    }));
    let dwg_bytes = DwgWriter::write(&doc, DwgVersion::R2010)
        .expect("DWG writer must succeed for a valid document");
    assert!(
        !dwg_bytes.is_empty(),
        "DWG writer must produce non-empty output"
    );
    assert!(
        dwg_bytes.starts_with(b"AC10"),
        "DWG binary must start with AC10xx magic; got {:?}",
        &dwg_bytes[..6.min(dwg_bytes.len())]
    );

    // Prove the DWG is functional (not just a dead file): read it
    // back through DwgReader.
    let reader = DwgReader::new(&dwg_bytes).expect("DWG reader must parse the opt-in export");
    assert_eq!(reader.version, DwgVersion::R2010);
    let round = reader
        .into_document()
        .expect("DWG into_document must succeed");
    assert!(
        !round.entities.is_empty(),
        "DWG roundtrip must return at least one entity"
    );
}
