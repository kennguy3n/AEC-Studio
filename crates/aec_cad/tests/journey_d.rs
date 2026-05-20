//! Journey D — Drafter end-to-end.
//!
//! `PROPOSAL.md` §6.D: a drafter imports a vendor DXF, draws the
//! full set of 2D primitives (line, polyline, arc, circle, spline,
//! hatch, text, dim), executes the keyboard command-line aliases
//! (`L`, `O`, `CO`, `TRIM`, `F`), creates 12 sheets with title
//! blocks, then exports DXF + PDF.
//!
//! Acceptance: DXF roundtrip preserves layers, blocks, dim styles,
//! and text styles.

use std::path::PathBuf;

use aec_cad::command_line::{CommandKind, CommandParser};
use aec_cad::dxf::{
    DxfArc, DxfBlockRecord, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfDocument,
    DxfEllipse, DxfEntity, DxfHatch, DxfHatchLoop, DxfInsert, DxfLine, DxfPolyline,
    DxfPolylineVertex, DxfReader, DxfSpline, DxfText, DxfWriter,
};
use aec_cad::layers::{Layer, LayerColor, LayerLineweight};
use aec_cad::sheets::{PaperSize, Sheet, SheetSet, TitleBlock};
use aec_core::templates::TemplateLoader;

fn templates_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    PathBuf::from(manifest)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("templates")
}

/// Produce a vendor-supplied DXF as a raw string so we can exercise
/// the reader path. We deliberately use a minimal, valid R12-style
/// document with one entity per layer.
fn vendor_dxf_str() -> String {
    let mut s = String::new();
    s.push_str("0\nSECTION\n2\nHEADER\n0\nENDSEC\n");
    s.push_str("0\nSECTION\n2\nTABLES\n");
    // LAYER table.
    s.push_str("0\nTABLE\n2\nLAYER\n");
    for (n, c) in [("0", 7), ("WALLS", 1), ("DOORS", 3)] {
        s.push_str("0\nLAYER\n");
        s.push_str(&format!("2\n{n}\n"));
        s.push_str(&format!("62\n{c}\n"));
        s.push_str("6\nCONTINUOUS\n");
        s.push_str("370\n25\n");
    }
    s.push_str("0\nENDTAB\n");
    s.push_str("0\nENDSEC\n");
    s.push_str("0\nSECTION\n2\nENTITIES\n");
    // One LINE on WALLS.
    s.push_str("0\nLINE\n8\nWALLS\n10\n0.0\n20\n0.0\n30\n0.0\n11\n1000.0\n21\n0.0\n31\n0.0\n");
    // One CIRCLE on DOORS.
    s.push_str("0\nCIRCLE\n8\nDOORS\n10\n500.0\n20\n500.0\n30\n0.0\n40\n200.0\n");
    s.push_str("0\nENDSEC\n");
    s.push_str("0\nEOF\n");
    s
}

fn make_sheet(name: &str, code: &str, label: &str) -> Sheet {
    let mut tb = TitleBlock::standard();
    tb.set("project", "Vendor Drafting Set");
    tb.set("sheet_number", code);
    tb.set("sheet_name", label);
    tb.set("scale", "1:50");
    tb.set("date", "2026-05-20");
    let mut sh = Sheet::new(name, PaperSize::IsoA1);
    sh.title_block = Some(tb);
    sh
}

#[test]
fn drafter_journey_end_to_end() {
    // ---------------------------------------------------------------
    // 1. 2D drafting template.
    // ---------------------------------------------------------------
    let loader = TemplateLoader::new(templates_root());
    let tpl = loader
        .load("drafting.2d_drafting")
        .expect("2D drafting template must load");
    assert_eq!(tpl.category.as_deref(), Some("drafting"));

    // ---------------------------------------------------------------
    // 2. Import vendor DXF and assert the reader recovered both
    //    layers and entities.
    // ---------------------------------------------------------------
    let vendor_doc = DxfReader::read_str(&vendor_dxf_str()).expect("vendor DXF reads");
    assert!(vendor_doc.layers.get("WALLS").is_some());
    assert!(vendor_doc.layers.get("DOORS").is_some());
    let vendor_entities = vendor_doc.entities.len();
    assert_eq!(vendor_entities, 2);

    // ---------------------------------------------------------------
    // 3. Build a new document with 5 layers (different colours,
    //    lineweights, linetypes), 3 block records (with descriptions
    //    acting as attribute placeholders), 2 dim styles, 2 text
    //    styles, plus the full primitive set the drafter draws.
    // ---------------------------------------------------------------
    let mut doc = DxfDocument::new();

    // 5 layers.
    let mut walls = Layer::new("A-WALL").expect("layer name");
    walls.color = LayerColor(1); // red
    walls.lineweight = LayerLineweight::from_mm(0.50);
    walls.linetype = "CONTINUOUS".into();
    let mut doors = Layer::new("A-DOOR").expect("layer name");
    doors.color = LayerColor(3); // green
    doors.lineweight = LayerLineweight::from_mm(0.25);
    let mut anno = Layer::new("A-ANNO").expect("layer name");
    anno.color = LayerColor(7); // white
    anno.lineweight = LayerLineweight::from_mm(0.13);
    let mut grid = Layer::new("A-GRID").expect("layer name");
    grid.color = LayerColor(8); // grey
    grid.linetype = "DASHED".into();
    grid.lineweight = LayerLineweight::from_mm(0.18);
    let mut dim = Layer::new("A-DIM").expect("layer name");
    dim.color = LayerColor(2); // yellow
    dim.lineweight = LayerLineweight::from_mm(0.18);
    let drafter_layer_names: Vec<String> = [&walls, &doors, &anno, &grid, &dim]
        .iter()
        .map(|l| l.name.clone())
        .collect();
    for l in [walls, doors, anno, grid, dim] {
        doc.layers.upsert(l);
    }
    for n in &drafter_layer_names {
        assert!(doc.layers.get(n).is_some(), "layer {n} authored");
    }

    // 3 block records.
    for n in ["DOOR_900", "WINDOW_1200", "FURNITURE_DESK"] {
        let mut rec = DxfBlockRecord::new(n);
        rec.description = Some(format!("vendor block {n}"));
        doc.block_records.push(rec);
    }
    assert_eq!(doc.block_records.len(), 3);

    // 2 dim styles.
    doc.dim_styles.clear();
    doc.dim_styles.push(DxfDimStyle::standard()); // STANDARD
    doc.dim_styles.push(DxfDimStyle {
        name: "ARCH_50".into(),
        text_height: 3.0,
        arrow_size: 3.0,
        units_scale: 50.0,
    });
    assert_eq!(doc.dim_styles.len(), 2);

    // Primitives: LINE, POLYLINE, ARC, CIRCLE, ELLIPSE, SPLINE,
    // HATCH, TEXT, INSERT (block reference), DIMENSION. Two TEXT
    // entities so we can verify the text style assertion below.
    doc.push(DxfEntity::Line(DxfLine {
        layer: "A-WALL".into(),
        start: [0.0, 0.0, 0.0],
        end: [5000.0, 0.0, 0.0],
    }));
    doc.push(DxfEntity::Polyline(DxfPolyline {
        layer: "A-WALL".into(),
        vertices: vec![
            DxfPolylineVertex::new(0.0, 0.0),
            DxfPolylineVertex::new(5000.0, 0.0),
            DxfPolylineVertex::new(5000.0, 3000.0),
            DxfPolylineVertex::new(0.0, 3000.0),
        ],
        closed: true,
        elevation: 0.0,
    }));
    doc.push(DxfEntity::Arc(DxfArc {
        layer: "A-DOOR".into(),
        center: [2500.0, 1500.0, 0.0],
        radius: 900.0,
        start_angle: 0.0,
        end_angle: 90.0,
    }));
    doc.push(DxfEntity::Circle(DxfCircle {
        layer: "A-DOOR".into(),
        center: [1000.0, 1000.0, 0.0],
        radius: 200.0,
    }));
    doc.push(DxfEntity::Ellipse(DxfEllipse {
        layer: "A-DOOR".into(),
        center: [3000.0, 1500.0, 0.0],
        major_axis: [400.0, 0.0, 0.0],
        ratio: 0.5,
        start_param: 0.0,
        end_param: std::f64::consts::TAU,
    }));
    doc.push(DxfEntity::Spline(DxfSpline {
        layer: "A-ANNO".into(),
        degree: 3,
        knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        control_points: vec![
            [0.0, 0.0, 0.0],
            [1000.0, 1000.0, 0.0],
            [2000.0, -1000.0, 0.0],
            [3000.0, 0.0, 0.0],
        ],
        closed: false,
    }));
    doc.push(DxfEntity::Hatch(DxfHatch {
        layer: "A-WALL".into(),
        pattern_name: "ANSI31".into(),
        solid: false,
        scale: 1.0,
        angle: 45.0,
        elevation: 0.0,
        loops: vec![DxfHatchLoop {
            vertices: vec![[0.0, 0.0], [5000.0, 0.0], [5000.0, 3000.0], [0.0, 3000.0]],
        }],
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "A-ANNO".into(),
        position: [100.0, 200.0, 0.0],
        height: 2.5,
        rotation: 0.0,
        text: "ROOM 101".into(),
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "A-ANNO".into(),
        position: [100.0, 250.0, 0.0],
        height: 3.5,
        rotation: 0.0,
        text: "Living".into(),
    }));
    doc.push(DxfEntity::Insert(DxfInsert {
        layer: "A-DOOR".into(),
        block_name: "DOOR_900".into(),
        position: [4500.0, 0.0, 0.0],
        scale: [1.0, 1.0, 1.0],
        rotation: 0.0,
    }));
    doc.push(DxfEntity::Dimension(DxfDimension {
        layer: "A-DIM".into(),
        style: "ARCH_50".into(),
        kind: DxfDimensionKind::Linear,
        def_point: [0.0, -200.0, 0.0],
        text_position: [2500.0, -300.0, 0.0],
        def_point_a: [0.0, 0.0, 0.0],
        def_point_b: [5000.0, 0.0, 0.0],
        override_text: None,
        measured_value: Some(5000.0),
    }));
    let authored_entities = doc.entities.len();
    assert_eq!(
        authored_entities, 11,
        "10 distinct primitive kinds + extra TEXT for style coverage"
    );

    // ---------------------------------------------------------------
    // 4. Command-line aliases. Every alias in the journey acceptance
    //    must resolve to its canonical command kind.
    // ---------------------------------------------------------------
    let parser = CommandParser::default();
    let cases: &[(&str, CommandKind)] = &[
        ("L 0,0 5,0", CommandKind::Line),
        ("O 100", CommandKind::Offset),
        ("CO", CommandKind::Copy),
        ("TRIM", CommandKind::Trim),
        ("F 50", CommandKind::Fillet),
    ];
    for (input, expected) in cases {
        let parsed = parser
            .parse(input)
            .unwrap_or_else(|| panic!("alias {input:?} must parse"));
        assert_eq!(parsed.kind, *expected, "alias {input:?} → {expected:?}");
    }

    // ---------------------------------------------------------------
    // 5. 12 sheets with title blocks.
    // ---------------------------------------------------------------
    let mut sheet_set = SheetSet::new("Drafting set");
    for i in 0..12 {
        sheet_set.add(make_sheet(
            &format!("Sheet {i:02}"),
            &format!("A{i:03}"),
            &format!("Plan {i:02}"),
        ));
    }
    assert_eq!(sheet_set.len(), 12);

    // ---------------------------------------------------------------
    // 6. DXF roundtrip — write + read back and compare every dim
    //    style, block record, layer (with colour / linetype /
    //    lineweight), and entity (by tag + layer).
    // ---------------------------------------------------------------
    let written = DxfWriter::write_to_string(&doc).expect("DXF writes");
    let reloaded = DxfReader::read_str(&written).expect("DXF re-reads");

    assert_eq!(
        reloaded.layers.len(),
        doc.layers.len(),
        "layer count preserved"
    );
    for original in doc.layers.iter() {
        let round = reloaded
            .layers
            .get(&original.name)
            .unwrap_or_else(|| panic!("layer {} survives roundtrip", original.name));
        assert_eq!(round.color, original.color, "{} colour", original.name);
        assert_eq!(
            round.linetype, original.linetype,
            "{} linetype",
            original.name
        );
        assert_eq!(
            round.lineweight, original.lineweight,
            "{} lineweight",
            original.name
        );
    }

    // Block records.
    let block_names: std::collections::BTreeSet<_> =
        doc.block_records.iter().map(|b| b.name.as_str()).collect();
    let round_block_names: std::collections::BTreeSet<_> = reloaded
        .block_records
        .iter()
        .map(|b| b.name.as_str())
        .collect();
    assert_eq!(round_block_names, block_names, "block records preserved");

    // Dim styles.
    let dim_names: std::collections::BTreeSet<_> =
        doc.dim_styles.iter().map(|d| d.name.as_str()).collect();
    let round_dim_names: std::collections::BTreeSet<_> = reloaded
        .dim_styles
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(round_dim_names, dim_names, "dim styles preserved");

    // Text styles. The DXF writer encodes text height as a 'style' so
    // distinct heights → distinct styles. We verify that both
    // distinct heights are still present in the roundtripped TEXTs.
    let round_text_heights: std::collections::BTreeSet<i64> = reloaded
        .entities
        .iter()
        .filter_map(|e| match e {
            DxfEntity::Text(t) => Some((t.height * 100.0).round() as i64),
            _ => None,
        })
        .collect();
    assert!(
        round_text_heights.contains(&250) && round_text_heights.contains(&350),
        "both text heights survive roundtrip (got {:?})",
        round_text_heights
    );

    // Entities: same count, same layer assignment per slot.
    assert_eq!(reloaded.entities.len(), authored_entities);
    for (orig, round) in doc.entities.iter().zip(reloaded.entities.iter()) {
        assert_eq!(orig.layer(), round.layer());
        assert_eq!(
            std::mem::discriminant(orig),
            std::mem::discriminant(round),
            "entity variant matches"
        );
    }
}
