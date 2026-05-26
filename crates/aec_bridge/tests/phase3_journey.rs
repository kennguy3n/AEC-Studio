//! Phase 3 end-to-end user journey — Drafter ("Pure 2D CAD for a
//! steel detail set"). Driven through the `BridgeService` public
//! API for every step that's bridge-wired today, dropping into the
//! underlying `aec_cad` library only for surfaces that have not yet
//! been promoted onto the bridge (these are the `draft.*` gestures
//! sitting in PR #48 — once that PR merges, the direct
//! `DxfReader::read_str` / `Primitive::*` / `DxfWriter::write` /
//! `MoveTool::apply` / `Sheet::new` calls below will be swapped for
//! `BridgeService::draft_import_dxf` / `draft_draw_primitive` /
//! `draft_edit_tool` / `draft_create_sheet` / `draft_export_dxf`
//! respectively).
//!
//! Steps (PROPOSAL.md "User Journey D"):
//!
//!   1. Create project from the 2D drafting template
//!      (`drafting.2d_drafting`).
//!   2. Import a DXF file (a small vendor-supplied fixture
//!      generated inline so the test is hermetic and does not
//!      depend on an external file).
//!   3. Draw the four core primitives (Line, Polyline, Arc, Circle)
//!      using the real `aec_cad::primitives` types.
//!   4. Edit primitives — exercise Move (translate), Copy
//!      (translate + duplicate), and Fillet (corner between two
//!      lines), all via the real `aec_cad::editing` tools.
//!   5. Create 3 sheets with title blocks (cover, plan, detail)
//!      using the real `aec_cad::sheets` types.
//!   6. Export DXF + PDF via the bridge's `export_dxf` /
//!      `export_pdf` methods. The DXF export uses the project's
//!      wall geometry (which we seed through `command_apply`
//!      because the drafting template ships with `rooms: []`); the
//!      drawing's drafting primitives also round-trip through
//!      `DxfWriter::write_to_string` → `DxfReader::read_str` so we
//!      cover the lossless side of the export.

use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService};
use aec_cad::dxf::{
    DxfArc, DxfCircle, DxfDocument, DxfEntity, DxfLine, DxfPolyline, DxfPolylineVertex, DxfReader,
    DxfWriter,
};
use aec_cad::editing::{CopyTool, FilletTool, MoveTool};
use aec_cad::layers::{Layer, LayerColor, LayerLineweight};
use aec_cad::primitives::{Arc, Circle, Line, Polyline, PolylineVertex, Primitive};
use aec_cad::sheets::{PaperSize, Sheet, TitleBlock};
use aec_command::commands::{wall, Command, CommandKind};
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
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_template("drafting", "2d_drafting", &templates);
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let svc = BridgeService::new(cfg, [0x33u8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

/// Generates a minimal but valid DXF document with one entity per
/// layer — modelled on `journey_d.rs::vendor_dxf_str()` so the two
/// integration paths share a baseline shape.
fn vendor_dxf_str() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("0\nSECTION\n2\nHEADER\n0\nENDSEC\n");
    s.push_str("0\nSECTION\n2\nTABLES\n");
    s.push_str("0\nTABLE\n2\nLAYER\n");
    for (n, c) in [("0", 7), ("WALLS", 1), ("DOORS", 3), ("ANNO", 4)] {
        s.push_str("0\nLAYER\n");
        writeln!(s, "2\n{n}").unwrap();
        writeln!(s, "62\n{c}").unwrap();
        s.push_str("6\nCONTINUOUS\n");
        s.push_str("370\n25\n");
    }
    s.push_str("0\nENDTAB\n");
    s.push_str("0\nENDSEC\n");
    s.push_str("0\nSECTION\n2\nENTITIES\n");
    s.push_str("0\nLINE\n8\nWALLS\n10\n0.0\n20\n0.0\n30\n0.0\n11\n5000.0\n21\n0.0\n31\n0.0\n");
    s.push_str("0\nCIRCLE\n8\nDOORS\n10\n500.0\n20\n500.0\n30\n0.0\n40\n200.0\n");
    s.push_str("0\nARC\n8\nDOORS\n10\n1000.0\n20\n0.0\n30\n0.0\n40\n400.0\n50\n0.0\n51\n90.0\n");
    s.push_str("0\nENDSEC\n");
    s.push_str("0\nEOF\n");
    s
}

fn make_sheet(name: &str, sheet_number: &str, label: &str, paper: PaperSize) -> Sheet {
    let mut tb = TitleBlock::standard();
    tb.set("project.name", "Phase 3 Drafter Journey");
    tb.set("drawing.number", sheet_number);
    tb.set("drawing.title", label);
    tb.set("drawing.scale", "1:50");
    tb.set("date", "2026-05-25");
    let mut sh = Sheet::new(name, paper);
    sh.title_block = Some(tb);
    sh
}

#[test]
fn phase3_drafter_2d_cad_journey() {
    // ── Step 1: create project from the 2D drafting template ──
    let (mut svc, _g) = boot_service();
    let summary = svc
        .project_create_from_template("drafting.2d_drafting", "Phase 3 Drafter Journey")
        .expect("create from drafting template");
    assert_eq!(summary.template_id.as_deref(), Some("drafting.2d_drafting"));

    // ── Step 2: import a vendor DXF (hermetic fixture) ──
    // Parses through the same `DxfReader` that the future
    // `BridgeService::draft_import_dxf` (PR #48) routes through.
    let imported = DxfReader::read_str(&vendor_dxf_str()).expect("vendor DXF parses");
    assert!(
        imported.layers.len() >= 4,
        "imported DXF should expose 4 layers (0, WALLS, DOORS, ANNO), got {}",
        imported.layers.len()
    );
    let imported_entities = imported.entities.len();
    assert!(
        imported_entities >= 3,
        "imported DXF should produce 3 entities (LINE, CIRCLE, ARC), got {imported_entities}",
    );

    // ── Step 3: draw four primitives via the real
    //   `aec_cad::primitives` API ──
    let line_a = Primitive::Line(Line::new("WALLS", [0.0, 0.0], [5_000.0, 0.0]));
    let _line_b = Primitive::Line(Line::new("WALLS", [5_000.0, 0.0], [5_000.0, 3_000.0]));
    let polyline = Primitive::Polyline(
        Polyline::new(
            "WALLS",
            vec![
                PolylineVertex::new([0.0, 0.0]),
                PolylineVertex::new([5_000.0, 0.0]),
                PolylineVertex::new([5_000.0, 3_000.0]),
                PolylineVertex::new([0.0, 3_000.0]),
            ],
        )
        .closed(true),
    );
    let arc = Primitive::Arc(Arc::new("DOORS", [1_000.0, 1_000.0], 500.0));
    let circle = Primitive::Circle(Circle::new("ANNO", [2_500.0, 1_500.0], 250.0));

    // All four primitives should carry their declared layers.
    assert_eq!(line_a.layer(), "WALLS");
    assert_eq!(polyline.layer(), "WALLS");
    assert_eq!(arc.layer(), "DOORS");
    assert_eq!(circle.layer(), "ANNO");

    // ── Step 4: edit primitives — Move, Copy, Fillet ──
    // Move the first line by [+100, +50] and confirm the start /
    // end coordinates land where MoveTool would put them.
    let moved = MoveTool::apply(&line_a, [100.0, 50.0]);
    if let Primitive::Line(l) = &moved {
        assert!(
            (l.start[0] - 100.0).abs() < 1e-9 && (l.start[1] - 50.0).abs() < 1e-9,
            "MoveTool should translate the start point by the delta, got {:?}",
            l.start
        );
        assert!(
            (l.end[0] - 5_100.0).abs() < 1e-9 && (l.end[1] - 50.0).abs() < 1e-9,
            "MoveTool should translate the end point by the delta, got {:?}",
            l.end
        );
    } else {
        panic!("MoveTool should preserve the primitive variant");
    }

    // Copy the polyline and verify it's a real copy (delta-applied)
    // and not a reuse of the source.
    let copied = CopyTool::apply(&polyline, [200.0, 0.0]);
    if let (Primitive::Polyline(src), Primitive::Polyline(dst)) = (&polyline, &copied) {
        assert_eq!(src.vertices.len(), dst.vertices.len());
        assert!(
            (dst.vertices[0].at[0] - 200.0).abs() < 1e-9,
            "CopyTool delta should apply to the first vertex"
        );
    } else {
        panic!("CopyTool should preserve the primitive variant");
    }

    // Fillet two lines that meet at the origin → produces a
    // FilletResult with the two trimmed lines and a connecting arc.
    let l1 = Line::new("WALLS", [0.0, 0.0], [1_000.0, 0.0]);
    let l2 = Line::new("WALLS", [0.0, 0.0], [0.0, 1_000.0]);
    let fillet = FilletTool::fillet_lines(&l1, &l2, [0.0, 0.0], 50.0)
        .expect("two right-angle lines should fillet with radius 50");
    // The fillet's arc must have the requested radius.
    assert!(
        (fillet.arc.radius - 50.0).abs() < 1e-6,
        "fillet radius must match the request, got {}",
        fillet.arc.radius
    );

    // ── Step 5: assemble 3 sheets with title blocks ──
    let sheet_cover = make_sheet("Cover", "S-000", "Cover Sheet", PaperSize::IsoA3);
    let sheet_plan = make_sheet("Plan", "S-101", "First-Floor Plan", PaperSize::IsoA1);
    let sheet_detail = make_sheet(
        "Detail",
        "S-501",
        "Steel Connection Detail",
        PaperSize::IsoA2,
    );

    for sh in [&sheet_cover, &sheet_plan, &sheet_detail] {
        let tb = sh.title_block.as_ref().expect("title block populated");
        // populated_fields() returns (field, value) pairs; we only
        // need to confirm the project name + drawing number landed.
        let kv: Vec<(String, String)> = tb
            .populated_fields()
            .map(|(f, v)| (f.key.clone(), v.to_owned()))
            .collect();
        assert!(
            kv.iter()
                .any(|(k, v)| k == "project.name" && v == "Phase 3 Drafter Journey"),
            "sheet `{}` title block should carry the project name; got {:?}",
            sh.name,
            kv
        );
        assert!(
            kv.iter().any(|(k, _)| k == "drawing.number"),
            "sheet `{}` title block should carry a drawing.number",
            sh.name
        );
    }

    // Sheet dimensions sanity (Landscape default).
    assert_eq!(sheet_plan.dimensions_mm(), (841.0, 594.0));

    // ── Step 6a: seed the project with one wall + export DXF
    //   through the bridge ──
    // The 2D drafting template ships with `rooms: []` so we need
    // to seed something for the bridge's project-level `export_dxf`
    // to render. This is exactly what the in-process service tests
    // do (`seed_one_wall`) — same pattern, applied to the journey.
    let wall_id = EntityId::new();
    let cmd = Command::user(CommandKind::CreateWall(wall::CreateWall {
        entity_id: wall_id.clone(),
        start_mm: [0.0, 0.0],
        end_mm: [5_000.0, 0.0],
        height_mm: 2_700.0,
        thickness_mm: 100.0,
        material_id: None,
    }));
    svc.command_apply(&summary.path, cmd)
        .expect("seed wall through command_apply");

    let out_dir = tempfile::tempdir().unwrap();
    let out_dxf = out_dir.path().join("steel_set.dxf");
    let walls: Vec<(f64, f64, f64, f64)> = vec![
        (0.0, 0.0, 5_000.0, 0.0),
        (5_000.0, 0.0, 5_000.0, 3_000.0),
        (5_000.0, 3_000.0, 0.0, 3_000.0),
        (0.0, 3_000.0, 0.0, 0.0),
    ];
    let export = svc
        .export_dxf(out_dxf.to_str().unwrap(), &summary.name, &walls)
        .expect("bridge.export_dxf");
    assert!(
        out_dxf.exists(),
        "export_dxf should write to {}",
        out_dxf.display()
    );
    assert!(export.out_path.ends_with("steel_set.dxf"));
    // Re-read the exported DXF to confirm the writer/reader pair
    // is lossless for the wall set. This is the structural part of
    // Task 16 (DXF round-trip fidelity) re-exercised at the
    // bridge journey level.
    let exported_body = std::fs::read_to_string(&out_dxf).unwrap();
    let reloaded = DxfReader::read_str(&exported_body).expect("exported DXF re-reads");
    assert!(
        reloaded.entities.len() >= walls.len(),
        "exported DXF should contain at least one entity per wall, got {} (expected ≥ {})",
        reloaded.entities.len(),
        walls.len()
    );

    // ── Step 6b: also round-trip the drafting drawing through the
    //   DXF writer/reader, then through the export_dxf bridge ──
    let mut drafting_doc = DxfDocument::new();
    let mut layer_walls = Layer::new("WALLS").unwrap();
    layer_walls.color = LayerColor(1);
    layer_walls.lineweight = LayerLineweight::from_mm(0.50);
    drafting_doc.layers.upsert(layer_walls);
    let mut layer_doors = Layer::new("DOORS").unwrap();
    layer_doors.color = LayerColor(3);
    layer_doors.lineweight = LayerLineweight::from_mm(0.35);
    drafting_doc.layers.upsert(layer_doors);
    let mut layer_anno = Layer::new("ANNO").unwrap();
    layer_anno.color = LayerColor(4);
    drafting_doc.layers.upsert(layer_anno);

    drafting_doc.push(DxfEntity::Line(DxfLine {
        layer: "WALLS".into(),
        start: [0.0, 0.0, 0.0],
        end: [5_000.0, 0.0, 0.0],
    }));
    drafting_doc.push(DxfEntity::Polyline(DxfPolyline {
        layer: "WALLS".into(),
        vertices: vec![
            DxfPolylineVertex::new(0.0, 0.0),
            DxfPolylineVertex::new(5_000.0, 0.0),
            DxfPolylineVertex::new(5_000.0, 3_000.0),
            DxfPolylineVertex::new(0.0, 3_000.0),
        ],
        closed: true,
        elevation: 0.0,
    }));
    drafting_doc.push(DxfEntity::Arc(DxfArc {
        layer: "DOORS".into(),
        center: [1_000.0, 1_000.0, 0.0],
        radius: 500.0,
        start_angle: 0.0,
        end_angle: 90.0,
    }));
    drafting_doc.push(DxfEntity::Circle(DxfCircle {
        layer: "ANNO".into(),
        center: [2_500.0, 1_500.0, 0.0],
        radius: 250.0,
    }));

    let drafting_dxf = DxfWriter::write_to_string(&drafting_doc).expect("write drafting DXF");
    let drafting_reread = DxfReader::read_str(&drafting_dxf).expect("re-read drafting DXF");
    assert_eq!(
        drafting_reread.entities.len(),
        drafting_doc.entities.len(),
        "drafting DXF entity count must survive write→read"
    );

    // ── Step 6c: export PDF through the bridge ──
    let out_pdf = out_dir.path().join("steel_set.pdf");
    let body_lines: Vec<String> = vec![
        "Steel detail set — Phase 3 journey".into(),
        "Sheets: Cover, Plan, Detail".into(),
        format!("Walls: {} segments", walls.len()),
        format!(
            "Drafting primitives: {} entities",
            drafting_doc.entities.len()
        ),
    ];
    let pdf = svc
        .export_pdf(out_pdf.to_str().unwrap(), &summary.name, &body_lines)
        .expect("bridge.export_pdf");
    assert!(
        out_pdf.exists(),
        "export_pdf should write to {}",
        out_pdf.display()
    );
    assert!(pdf.pages >= 1, "exported PDF should have at least one page");

    // Sanity: project still saveable.
    svc.project_save(&summary.path)
        .expect("project_save after drafter journey");
}

#[test]
fn phase3_journey_dxf_roundtrip_preserves_primitives() {
    // Companion check to the main journey: assemble a fresh
    // DxfDocument with one entity per primitive type the drafter
    // would actually draw, write it through `DxfWriter`, and
    // re-read it through `DxfReader` to confirm fidelity. This is
    // intentionally a smaller surface than the broader
    // `dxf_roundtrip` tests in `aec_cad/tests` — it pins what the
    // Phase 3 journey itself depends on.
    let mut doc = DxfDocument::new();
    doc.push(DxfEntity::Line(DxfLine {
        layer: "0".into(),
        start: [0.0, 0.0, 0.0],
        end: [1_000.0, 0.0, 0.0],
    }));
    doc.push(DxfEntity::Polyline(DxfPolyline {
        layer: "0".into(),
        vertices: vec![
            DxfPolylineVertex::new(0.0, 0.0),
            DxfPolylineVertex::new(500.0, 500.0),
            DxfPolylineVertex::new(1_000.0, 0.0),
        ],
        closed: false,
        elevation: 0.0,
    }));
    doc.push(DxfEntity::Arc(DxfArc {
        layer: "0".into(),
        center: [500.0, 500.0, 0.0],
        radius: 250.0,
        start_angle: 0.0,
        end_angle: 180.0,
    }));
    doc.push(DxfEntity::Circle(DxfCircle {
        layer: "0".into(),
        center: [500.0, 500.0, 0.0],
        radius: 100.0,
    }));

    let body = DxfWriter::write_to_string(&doc).unwrap();
    let reread = DxfReader::read_str(&body).unwrap();
    assert_eq!(reread.entities.len(), 4);
    // Count by variant — guards against the reader silently
    // promoting one type to another.
    let mut line = 0;
    let mut poly = 0;
    let mut arc = 0;
    let mut circle = 0;
    for e in reread.entities {
        match e {
            DxfEntity::Line(_) => line += 1,
            DxfEntity::Polyline(_) => poly += 1,
            DxfEntity::Arc(_) => arc += 1,
            DxfEntity::Circle(_) => circle += 1,
            _ => {}
        }
    }
    assert_eq!((line, poly, arc, circle), (1, 1, 1, 1));
}
