//! DXF roundtrip fidelity test.
//!
//! Exercises `DxfWriter::write_to_string` → `DxfReader::read_str` and
//! asserts that the in-memory DXF document survives unchanged across:
//!
//!   * 5 layers, each with a distinct color, lineweight, and linetype.
//!   * 3 block records (with description + flags preserved).
//!   * 2 dim styles (varying text height, arrow size, units scale).
//!   * 2 text styles (modelled as varying `DxfText.height` since
//!     standalone text-style tables are not yet emitted; the
//!     reader/writer preserves per-entity text height verbatim).
//!   * Mixed primitives: line, polyline, arc, circle, ellipse, spline,
//!     hatch, text, insert, dimension.
//!
//! A separate performance test inserts 10,000 line entities, runs the
//! same write/read pair, and asserts the roundtrip completes in
//! under 30 seconds (well above what we measure locally — the cap
//! exists so CI catches a regression that turns the writer O(n²)).

use std::time::Instant;

use aec_cad::dwg::{DwgReader, DwgVersion, DwgWriter};
use aec_cad::dxf::{
    DxfArc, DxfBlockRecord, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfDocument,
    DxfEllipse, DxfEntity, DxfHatch, DxfHatchLoop, DxfInsert, DxfLine, DxfPolyline,
    DxfPolylineVertex, DxfReader, DxfSpline, DxfText, DxfWriter,
};
use aec_cad::layers::{Layer, LayerColor, LayerLineweight};

fn make_layer(name: &str, color: i16, mm: f64, linetype: &str) -> Layer {
    let mut l = Layer::new(name).expect("layer name");
    l.color = LayerColor(color);
    l.lineweight = LayerLineweight::from_mm(mm);
    l.linetype = linetype.to_string();
    l
}

#[test]
fn dxf_roundtrip_preserves_layers_blocks_dim_styles_and_entities() {
    let mut doc = DxfDocument::new();

    // ---- 5 layers ----
    let layers = vec![
        make_layer("A-WALL", 1, 0.50, "Continuous"),
        make_layer("A-DOOR", 3, 0.35, "DASHED"),
        make_layer("A-ANNO", 4, 0.25, "Continuous"),
        make_layer("A-GRID", 8, 0.18, "DASHDOT"),
        make_layer("A-DIM", 2, 0.18, "Continuous"),
    ];
    for l in &layers {
        doc.layers.upsert(l.clone());
    }

    // ---- 3 block records ----
    let block_records = vec![
        {
            let mut b = DxfBlockRecord::new("DOOR_900");
            b.description = Some("900mm single swing".into());
            b.flags = 0;
            b
        },
        {
            let mut b = DxfBlockRecord::new("WINDOW_1200");
            b.description = Some("1200mm casement".into());
            b.flags = 0;
            b
        },
        {
            let mut b = DxfBlockRecord::new("FURN_DESK");
            b.description = Some("desk symbol".into());
            b.flags = 0;
            b
        },
    ];
    doc.block_records = block_records.clone();

    // ---- 2 dim styles ----
    let dim_styles = vec![
        DxfDimStyle {
            name: "ARCH-1-50".into(),
            text_height: 2.5,
            arrow_size: 2.5,
            units_scale: 1.0,
            decimal_places: 0,
            text_style: "STANDARD".into(),
        },
        DxfDimStyle {
            name: "ARCH-1-100".into(),
            text_height: 5.0,
            arrow_size: 5.0,
            units_scale: 2.0,
            decimal_places: 2,
            text_style: "TITLES".into(),
        },
    ];
    doc.dim_styles = dim_styles.clone();

    // ---- Mixed entities, one per variant. The test asserts the
    //      reader recovers identical Debug printouts so any drift in
    //      a field shows up immediately. ----
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
        center: [1000.0, 1000.0, 0.0],
        radius: 500.0,
        start_angle: 0.0,
        end_angle: 90.0,
    }));
    doc.push(DxfEntity::Circle(DxfCircle {
        layer: "A-ANNO".into(),
        center: [2500.0, 1500.0, 0.0],
        radius: 250.0,
    }));
    doc.push(DxfEntity::Ellipse(DxfEllipse {
        layer: "A-ANNO".into(),
        center: [3000.0, 1500.0, 0.0],
        major_axis: [400.0, 0.0, 0.0],
        ratio: 0.5,
        start_param: 0.0,
        end_param: std::f64::consts::TAU,
    }));
    doc.push(DxfEntity::Spline(DxfSpline {
        layer: "A-ANNO".into(),
        degree: 3,
        control_points: vec![
            [0.0, 0.0, 0.0],
            [1000.0, 500.0, 0.0],
            [2000.0, 1500.0, 0.0],
            [3000.0, 2500.0, 0.0],
        ],
        knots: vec![0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0],
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
            vertices: vec![[0.0, 0.0], [500.0, 0.0], [500.0, 500.0], [0.0, 500.0]],
        }],
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "A-ANNO".into(),
        position: [100.0, 100.0, 0.0],
        height: 2.5,
        rotation: 0.0,
        text: "STANDARD-2.5".into(),
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "A-ANNO".into(),
        position: [200.0, 200.0, 0.0],
        height: 5.0,
        rotation: 0.0,
        text: "HEADING-5.0".into(),
    }));
    doc.push(DxfEntity::Insert(DxfInsert {
        layer: "A-DOOR".into(),
        block_name: "DOOR_900".into(),
        position: [1500.0, 1000.0, 0.0],
        scale: [1.0, 1.0, 1.0],
        rotation: 0.0,
    }));
    doc.push(DxfEntity::Dimension(DxfDimension {
        layer: "A-DIM".into(),
        style: "ARCH-1-50".into(),
        kind: DxfDimensionKind::Aligned,
        def_point: [0.0, -200.0, 0.0],
        text_position: [2500.0, -400.0, 0.0],
        def_point_a: [0.0, 0.0, 0.0],
        def_point_b: [5000.0, 0.0, 0.0],
        override_text: None,
        measured_value: Some(5000.0),
    }));

    // ---- Roundtrip ----
    let written = DxfWriter::write_to_string(&doc).expect("DXF writes");
    assert!(
        written.contains("SECTION") && written.contains("HEADER") && written.contains("ENDSEC"),
        "writer emitted SECTION/HEADER/ENDSEC markers"
    );
    let reloaded = DxfReader::read_str(&written).expect("DXF re-reads");

    // ---- Layer fidelity ----
    assert_eq!(reloaded.layers.len(), doc.layers.len(), "layer count");
    for src in &layers {
        let got = reloaded
            .layers
            .get(&src.name)
            .unwrap_or_else(|| panic!("layer {} missing on roundtrip", src.name));
        assert_eq!(got.color, src.color, "layer {} color", src.name);
        assert_eq!(
            got.lineweight, src.lineweight,
            "layer {} lineweight",
            src.name
        );
        assert_eq!(got.linetype, src.linetype, "layer {} linetype", src.name);
    }

    // ---- Block-record fidelity ----
    assert_eq!(reloaded.block_records.len(), block_records.len());
    for src in &block_records {
        let got = reloaded
            .block_records
            .iter()
            .find(|b| b.name == src.name)
            .unwrap_or_else(|| panic!("block {} missing on roundtrip", src.name));
        assert_eq!(got.description, src.description);
        assert_eq!(got.flags, src.flags);
    }

    // ---- Dim-style fidelity ----
    assert_eq!(reloaded.dim_styles.len(), dim_styles.len());
    for src in &dim_styles {
        let got = reloaded
            .dim_styles
            .iter()
            .find(|d| d.name == src.name)
            .unwrap_or_else(|| panic!("dim style {} missing on roundtrip", src.name));
        assert_eq!(got.text_height, src.text_height);
        assert_eq!(got.arrow_size, src.arrow_size);
        assert_eq!(got.units_scale, src.units_scale);
        assert_eq!(
            got.decimal_places, src.decimal_places,
            "dim style {} decimal_places",
            src.name
        );
        assert_eq!(
            got.text_style, src.text_style,
            "dim style {} text_style",
            src.name
        );
    }

    // ---- "Text style" surrogate: every authored text height must
    //      survive verbatim. ----
    let original_text_heights: Vec<f64> = doc
        .entities
        .iter()
        .filter_map(|e| match e {
            DxfEntity::Text(t) => Some(t.height),
            _ => None,
        })
        .collect();
    let reloaded_text_heights: Vec<f64> = reloaded
        .entities
        .iter()
        .filter_map(|e| match e {
            DxfEntity::Text(t) => Some(t.height),
            _ => None,
        })
        .collect();
    assert_eq!(original_text_heights, reloaded_text_heights);

    // ---- Entity-level fidelity (sequence + content). ----
    assert_eq!(
        reloaded.entities.len(),
        doc.entities.len(),
        "entity count preserved"
    );
    for (src, got) in doc.entities.iter().zip(reloaded.entities.iter()) {
        assert_eq!(
            std::mem::discriminant(src),
            std::mem::discriminant(got),
            "entity discriminant preserved"
        );
        assert_eq!(src.layer(), got.layer(), "entity layer preserved");
        // Hatch-specific: verify the pattern name roundtrips (code-2
        // fix for pre-existing reader bug where code 2 was only routed
        // to block_name).
        if let (DxfEntity::Hatch(s), DxfEntity::Hatch(g)) = (src, got) {
            assert_eq!(
                g.pattern_name, s.pattern_name,
                "hatch pattern_name preserved"
            );
        }
    }
}

#[test]
fn dxf_roundtrip_handles_ten_thousand_entities_in_under_thirty_seconds() {
    let mut doc = DxfDocument::new();
    let mut wall = Layer::new("A-WALL").unwrap();
    wall.color = LayerColor(1i16);
    wall.lineweight = LayerLineweight::from_mm(0.5);
    doc.layers.upsert(wall);

    for i in 0..10_000 {
        let x = (i % 100) as f64 * 50.0;
        let y = (i / 100) as f64 * 50.0;
        doc.push(DxfEntity::Line(DxfLine {
            layer: "A-WALL".into(),
            start: [x, y, 0.0],
            end: [x + 25.0, y + 25.0, 0.0],
        }));
    }

    let t0 = Instant::now();
    let written = DxfWriter::write_to_string(&doc).expect("DXF writes");
    let reloaded = DxfReader::read_str(&written).expect("DXF re-reads");
    let elapsed = t0.elapsed();

    assert_eq!(reloaded.entities.len(), 10_000);
    assert!(
        elapsed.as_secs() < 30,
        "10k DXF roundtrip should complete in under 30s (took {:?})",
        elapsed
    );
}

/// Native-DWG companion to the 10k-entity DXF perf test. Every DWG
/// version family the codec supports gets the same 10k-LINE round-trip
/// inside the same 30s budget; a regression that turns the encoder
/// O(n²) — most likely failure mode for paged compression / object-map
/// page splitting — trips the budget on at least one version.
fn dwg_ten_thousand_lines_under_thirty_seconds(version: DwgVersion) {
    let mut doc = DxfDocument::new();
    let mut wall = Layer::new("A-WALL").unwrap();
    wall.color = LayerColor(1i16);
    wall.lineweight = LayerLineweight::from_mm(0.5);
    doc.layers.upsert(wall);

    for i in 0..10_000 {
        let x = (i % 100) as f64 * 50.0;
        let y = (i / 100) as f64 * 50.0;
        doc.push(DxfEntity::Line(DxfLine {
            layer: "A-WALL".into(),
            start: [x, y, 0.0],
            end: [x + 25.0, y + 25.0, 0.0],
        }));
    }

    let t0 = Instant::now();
    let bytes = DwgWriter::write(&doc, version)
        .unwrap_or_else(|e| panic!("DWG write failed for {version:?}: {e:?}"));
    let reader = DwgReader::new(&bytes)
        .unwrap_or_else(|e| panic!("DWG read header failed for {version:?}: {e:?}"));
    let reloaded = reader
        .into_document()
        .unwrap_or_else(|e| panic!("DWG into_document failed for {version:?}: {e:?}"));
    let elapsed = t0.elapsed();

    assert_eq!(
        reloaded.entities.len(),
        10_000,
        "10k DWG roundtrip for {version:?} lost entities"
    );
    assert!(
        elapsed.as_secs() < 30,
        "10k DWG roundtrip for {version:?} should complete in under 30s (took {:?})",
        elapsed
    );
}

#[test]
fn dwg_roundtrip_handles_ten_thousand_entities_r12() {
    dwg_ten_thousand_lines_under_thirty_seconds(DwgVersion::R12);
}

#[test]
fn dwg_roundtrip_handles_ten_thousand_entities_r2010() {
    dwg_ten_thousand_lines_under_thirty_seconds(DwgVersion::R2010);
}

#[test]
fn dwg_roundtrip_handles_ten_thousand_entities_r2018() {
    dwg_ten_thousand_lines_under_thirty_seconds(DwgVersion::R2018);
}

/// Real-world DXF fixture covering the full set of entity-attribute
/// surfaces Task 16 calls out (layer color/lineweight/linetype/
/// freeze/thaw/off/no-plot/description, blocks with body entities and
/// nested INSERT, ATTDEF attribute definitions with tag/prompt/flags/
/// text-style, multiple text styles with varying font/height/width
/// factor/oblique/bigfont, and dim styles with varying decimal places
/// and text-style references).
///
/// Test sequence: read fixture → snapshot the in-memory document →
/// write back out → re-read the written bytes → assert the second
/// in-memory document is bit-identical to the first on every attribute
/// surface (logical equality across `DxfDocument`'s Serialize JSON,
/// which is structurally equivalent to byte equality on the
/// attribute fields the round-trip is responsible for).
#[test]
fn dxf_roundtrip_full_fixture_preserves_every_table_and_block_attribute() {
    let fixture = include_str!("fixtures/roundtrip_full.dxf");
    let doc1 = DxfReader::read_str(fixture).expect("fixture parses");

    // ---- Layer-table assertions on the as-read document. ----
    let walls = doc1
        .layers
        .get("A-WALL")
        .expect("A-WALL layer present in fixture");
    assert_eq!(walls.color, LayerColor(1));
    assert_eq!(walls.lineweight, LayerLineweight(50));
    assert_eq!(walls.linetype, "Continuous");
    assert!(!walls.frozen, "A-WALL not frozen");
    assert!(!walls.locked, "A-WALL not locked");
    assert!(walls.on, "A-WALL is on");
    assert!(walls.plottable, "A-WALL plottable");
    assert_eq!(
        walls.description.as_deref(),
        Some("Exterior structural walls")
    );

    let doors = doc1
        .layers
        .get("A-DOOR")
        .expect("A-DOOR layer present in fixture");
    assert_eq!(doors.color, LayerColor(3), "A-DOOR colour absolute");
    assert_eq!(doors.lineweight, LayerLineweight(35));
    assert_eq!(doors.linetype, "DASHED");
    assert!(!doors.frozen, "A-DOOR not frozen (flag 4 = locked)");
    assert!(doors.locked, "A-DOOR locked (flag 4)");
    assert!(!doors.on, "A-DOOR off — encoded by negative ACI");
    assert!(!doors.plottable, "A-DOOR no-plot (DXF 290 = 0)");
    assert_eq!(
        doors.description.as_deref(),
        Some("Door symbols (frozen, off, no-plot)")
    );

    let notes = doc1
        .layers
        .get("A-NOTES")
        .expect("A-NOTES layer present in fixture");
    assert!(notes.frozen, "A-NOTES frozen via group 70 bit 0");

    // ---- BLOCK_RECORD + BLOCK body assertions. ----
    let door = doc1
        .block_records
        .iter()
        .find(|b| b.name == "DOOR_900")
        .expect("DOOR_900 block present");
    assert_eq!(door.description.as_deref(), Some("900mm single swing"));
    assert_eq!(
        door.entities.len(),
        4,
        "DOOR_900 carries 4 body entities (LINE, ARC, 2x ATTDEF)"
    );
    let attdefs: Vec<_> = door
        .entities
        .iter()
        .filter_map(|e| match e {
            DxfEntity::Attdef(a) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(attdefs.len(), 2, "DOOR_900 has 2 ATTDEFs");
    let tag = attdefs
        .iter()
        .find(|a| a.tag == "DOOR_TAG")
        .expect("DOOR_TAG attdef present");
    assert_eq!(tag.default_value, "D-01");
    assert_eq!(tag.prompt, "Enter door mark");
    assert_eq!(tag.flags, 0);
    assert_eq!(tag.text_style, "STANDARD");
    assert!((tag.height - 2.5).abs() < 1e-9);
    let fire = attdefs
        .iter()
        .find(|a| a.tag == "FIRE_RATING")
        .expect("FIRE_RATING attdef present");
    assert_eq!(fire.default_value, "60min");
    assert_eq!(fire.flags, 1, "FIRE_RATING invisible flag bit 1");
    assert_eq!(fire.text_style, "TITLES");

    let window = doc1
        .block_records
        .iter()
        .find(|b| b.name == "WINDOW_DBL")
        .expect("WINDOW_DBL block present");
    assert_eq!(window.entities.len(), 2, "WINDOW_DBL holds LINE + INSERT");
    let nested = window
        .entities
        .iter()
        .find_map(|e| match e {
            DxfEntity::Insert(i) => Some(i),
            _ => None,
        })
        .expect("nested INSERT lives inside WINDOW_DBL");
    assert_eq!(nested.block_name, "DOOR_900");
    assert!((nested.scale[0] - 0.5).abs() < 1e-9);
    assert!((nested.scale[1] - 0.5).abs() < 1e-9);

    // ---- STYLE-table assertions. ----
    let standard = doc1
        .text_styles
        .iter()
        .find(|s| s.name == "STANDARD")
        .expect("STANDARD text style");
    assert_eq!(standard.font_filename, "txt");
    assert!((standard.width_factor - 1.0).abs() < 1e-9);
    let titles = doc1
        .text_styles
        .iter()
        .find(|s| s.name == "TITLES")
        .expect("TITLES text style");
    assert!((titles.fixed_height - 5.0).abs() < 1e-9);
    assert!((titles.width_factor - 0.85).abs() < 1e-9);
    assert!((titles.oblique_angle - 15.0).abs() < 1e-9);
    assert_eq!(titles.font_filename, "arial.ttf");
    let heavy = doc1
        .text_styles
        .iter()
        .find(|s| s.name == "HEAVY-TITLES")
        .expect("HEAVY-TITLES text style");
    assert_eq!(heavy.bigfont_filename, "bigfont.shx");
    assert_eq!(heavy.font_filename, "arialbd.ttf");
    assert!((heavy.width_factor - 1.2).abs() < 1e-9);

    // ---- DIMSTYLE assertions. ----
    let arch50 = doc1
        .dim_styles
        .iter()
        .find(|d| d.name == "ARCH-1-50")
        .expect("ARCH-1-50 dim style");
    assert_eq!(
        arch50.decimal_places, 0,
        "1:50 architectural rounds to whole"
    );
    assert_eq!(arch50.text_style, "STANDARD");
    let arch100 = doc1
        .dim_styles
        .iter()
        .find(|d| d.name == "ARCH-1-100")
        .expect("ARCH-1-100 dim style");
    assert_eq!(arch100.decimal_places, 2, "1:100 carries 2dp");
    assert_eq!(arch100.text_style, "TITLES");

    // ---- Round-trip: write & re-read; second doc must equal first
    //      on every Serialize-equivalent attribute. ----
    let written = DxfWriter::write_to_string(&doc1).expect("DXF writes");
    let doc2 = DxfReader::read_str(&written).expect("written DXF re-reads");

    // Use the structural JSON projection as a byte-stable proxy for
    // attribute-level equality: every field that survives round-trip
    // is captured here, and any drift in a field's value or its
    // serialisation surface flips the assertion.
    let json1 = serde_json::to_string(&doc1).expect("doc1 serialises");
    let json2 = serde_json::to_string(&doc2).expect("doc2 serialises");
    assert_eq!(
        json1, json2,
        "DXF document not byte-stable across write→read round-trip"
    );
}

#[test]
fn dxf_roundtrip_preserves_hatch_loops_inside_block_bodies() {
    // Regression: `parse_blocks` previously fed an always-empty
    // `hatch_loops` slice to `build_entity`, silently dropping every
    // HATCH boundary loop nested inside a BLOCK ... ENDBLK pair.
    // `parse_one_entity` now backs both top-level and block-body
    // entities so HATCH boundary geometry survives the round-trip
    // regardless of where the HATCH lives.
    let mut doc = DxfDocument::new();
    doc.layers.upsert(Layer::new("WALLS").expect("layer name"));

    let mut block = DxfBlockRecord::new("FLOOR_TILE");
    block.base_point = [10.0, 20.0, 0.0];
    block.entities.push(DxfEntity::Hatch(DxfHatch {
        layer: "WALLS".into(),
        pattern_name: "ANSI31".into(),
        solid: false,
        scale: 1.0,
        angle: 45.0,
        elevation: 0.0,
        loops: vec![DxfHatchLoop {
            vertices: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
        }],
    }));
    doc.block_records.push(block);

    let written = DxfWriter::write_to_string(&doc).expect("write");
    let parsed = DxfReader::read_str(&written).expect("read");

    let block = parsed
        .block_records
        .iter()
        .find(|b| b.name == "FLOOR_TILE")
        .expect("FLOOR_TILE block survives round-trip");
    assert_eq!(block.base_point, [10.0, 20.0, 0.0]);
    assert_eq!(block.entities.len(), 1, "exactly one entity in block body");
    match &block.entities[0] {
        DxfEntity::Hatch(h) => {
            assert_eq!(h.pattern_name, "ANSI31");
            assert_eq!(h.angle, 45.0);
            assert_eq!(
                h.loops.len(),
                1,
                "block-body HATCH must keep its single boundary loop"
            );
            assert_eq!(
                h.loops[0].vertices,
                vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
                "block-body HATCH must keep every boundary vertex"
            );
        }
        other => panic!("expected HATCH inside block, got {:?}", other),
    }
}

#[test]
fn dxf_roundtrip_preserves_polyline_vertex_chain_inside_block_bodies() {
    // Regression: legacy DXF emits POLYLINE as a compound entity with
    // nested VERTEX records terminated by SEQEND. `parse_blocks` used
    // to break out of its inner field-reader on the first VERTEX
    // code-0, dropping every vertex. The shared `parse_one_entity`
    // path threads the VERTEX/SEQEND chain so legacy POLYLINEs inside
    // blocks survive intact.
    let groups = [
        "0", "SECTION", "2", "BLOCKS", //
        "0", "BLOCK", "2", "STAIR", "70", "0", "10", "0.0", "20", "0.0", "30", "0.0", //
        "0", "POLYLINE", "8", "0", "70", "1", //
        "0", "VERTEX", "10", "0.0", "20", "0.0", //
        "0", "VERTEX", "10", "1000.0", "20", "0.0", //
        "0", "VERTEX", "10", "1000.0", "20", "300.0", //
        "0", "VERTEX", "10", "0.0", "20", "300.0", //
        "0", "SEQEND", //
        "0", "ENDBLK", //
        "0", "ENDSEC", //
        "0", "EOF",
    ];
    let dxf = groups.join("\n");
    let parsed = DxfReader::read_str(&dxf).expect("legacy POLYLINE-in-block parses");

    let block = parsed
        .block_records
        .iter()
        .find(|b| b.name == "STAIR")
        .expect("STAIR block parsed");
    assert_eq!(block.entities.len(), 1, "the lone POLYLINE survives");
    match &block.entities[0] {
        DxfEntity::Polyline(p) => {
            assert!(p.closed, "POLYLINE flag bit 1 marks closed");
            assert_eq!(
                p.vertices.len(),
                4,
                "every VERTEX in the chain must be threaded into the polyline"
            );
            assert_eq!((p.vertices[0].x, p.vertices[0].y), (0.0, 0.0));
            assert_eq!((p.vertices[1].x, p.vertices[1].y), (1000.0, 0.0));
            assert_eq!((p.vertices[2].x, p.vertices[2].y), (1000.0, 300.0));
            assert_eq!((p.vertices[3].x, p.vertices[3].y), (0.0, 300.0));
        }
        other => panic!("expected POLYLINE inside block, got {:?}", other),
    }
}

#[test]
fn dxf_writer_emits_dimstyle_340_as_hard_pointer_handle() {
    // Regression for the DIMSTYLE-340 finding: the writer must emit
    // a real DXF hard-pointer handle (uppercase hex, the same handle
    // it emitted on the referenced STYLE record's code-5 slot), not
    // the symbolic style name. External DXF consumers (AutoCAD,
    // BricsCAD, LibreDWG) require a hex handle in 340 — emitting a
    // raw name leaves the dimension's text-style reference
    // unresolved on their side.
    let mut doc = DxfDocument::new();
    doc.text_styles.clear();
    doc.text_styles.push(aec_cad::dxf::DxfTextStyle {
        name: "TITLES".into(),
        font_filename: "arial.ttf".into(),
        bigfont_filename: String::new(),
        fixed_height: 5.0,
        width_factor: 1.0,
        oblique_angle: 0.0,
    });
    doc.dim_styles.clear();
    doc.dim_styles.push(DxfDimStyle {
        name: "ARCH-1-50".into(),
        text_height: 2.5,
        arrow_size: 2.5,
        units_scale: 1.0,
        decimal_places: 2,
        text_style: "TITLES".into(),
    });

    let written = DxfWriter::write_to_string(&doc).expect("write");

    // Find the STYLE handle.
    let style_lines: Vec<&str> = written.lines().collect();
    let style_idx = style_lines
        .iter()
        .position(|l| l.trim() == "STYLE")
        .and_then(|i| {
            // Skip the "STYLE" table-name pair and locate the first
            // record start (a second "STYLE" line after the table header).
            style_lines[i + 1..]
                .iter()
                .position(|l| l.trim() == "STYLE")
                .map(|p| i + 1 + p)
        })
        .expect("STYLE record present");
    // The line two below the record's "STYLE" keyword carries the
    // handle (code 5 emitted before any other field).
    assert_eq!(style_lines[style_idx + 1].trim(), "5");
    let handle = style_lines[style_idx + 2].trim().to_string();
    assert!(
        !handle.is_empty(),
        "STYLE record must carry a non-empty hex handle"
    );
    assert!(
        handle.chars().all(|c| c.is_ascii_hexdigit()),
        "STYLE handle must be hex, got {handle:?}"
    );

    // Find the DIMSTYLE record and inspect its 340 emission.
    let dimstyle_idx = style_lines
        .iter()
        .position(|l| l.trim() == "DIMSTYLE")
        .and_then(|i| {
            style_lines[i + 1..]
                .iter()
                .position(|l| l.trim() == "DIMSTYLE")
                .map(|p| i + 1 + p)
        })
        .expect("DIMSTYLE record present");
    let mut found_340 = false;
    for window in style_lines[dimstyle_idx..].windows(2) {
        if window[0].trim() == "340" {
            assert_eq!(
                window[1].trim(),
                handle,
                "DIMSTYLE 340 must emit the STYLE handle, not the style name"
            );
            found_340 = true;
            break;
        }
    }
    assert!(found_340, "DIMSTYLE record must emit a 340 group");

    // And re-reading the written DXF must still recover the symbolic
    // name on the in-memory struct (the reader resolves 340 handles
    // back to names so callers continue to use names as identifiers).
    let reparsed = DxfReader::read_str(&written).expect("re-read");
    let dim = reparsed
        .dim_styles
        .iter()
        .find(|d| d.name == "ARCH-1-50")
        .expect("ARCH-1-50 round-trips");
    assert_eq!(
        dim.text_style, "TITLES",
        "DIMSTYLE 340 hex handle resolves back to the style name on read"
    );
}

#[test]
fn dxf_writer_emits_code_8_layer_on_block_entities() {
    // Regression for the missing-code-8 finding: the DXF spec
    // requires a layer (group code 8) on every BLOCK entity inside
    // the BLOCKS section. Strict third-party consumers (AutoCAD,
    // BricsCAD, LibreDWG, QCAD) reject or warn on a missing code-8.
    // Our reader defaulted to "0" on missing input, so the bug was
    // invisible on internal round-trips, but external interop is
    // broken without it. New regression locks in:
    //   1. The writer emits code-8 immediately after the BLOCK
    //      keyword.
    //   2. Default layer is "0" (AutoCAD's block-definition
    //      convention so BYLAYER colors on contained entities
    //      resolve through the INSERT's layer).
    //   3. A non-default layer round-trips verbatim.
    //   4. OR-merge on the parse_blocks flags path preserves bits
    //      from a BLOCK_RECORD-table entry's flags field when the
    //      BLOCK entity's flags differ (defense-in-depth on
    //      non-conforming files).
    let mut doc = DxfDocument::new();
    let mut block_default = DxfBlockRecord::new("DEFAULT_LAYER_BLOCK");
    block_default.entities.push(DxfEntity::Line(DxfLine {
        layer: "0".into(),
        start: [0.0, 0.0, 0.0],
        end: [10.0, 0.0, 0.0],
    }));
    doc.block_records.push(block_default);

    let mut block_custom = DxfBlockRecord::new("CUSTOM_LAYER_BLOCK");
    block_custom.layer = "A-BLOCK-DEFS".into();
    block_custom.entities.push(DxfEntity::Line(DxfLine {
        layer: "A-WALL".into(),
        start: [0.0, 0.0, 0.0],
        end: [5.0, 5.0, 0.0],
    }));
    doc.block_records.push(block_custom);

    let written = DxfWriter::write_to_string(&doc).expect("DXF writes");

    // Locate the first BLOCK record and assert code 8 appears in
    // its header before any contained entity (i.e. before the next
    // code-0 that isn't ENDBLK).
    let lines: Vec<&str> = written.lines().collect();
    let mut block_starts = Vec::new();
    let mut idx = 0;
    while idx + 1 < lines.len() {
        if lines[idx].trim() == "0" && lines[idx + 1].trim() == "BLOCK" {
            block_starts.push(idx);
            idx += 2;
        } else {
            idx += 1;
        }
    }
    assert_eq!(
        block_starts.len(),
        2,
        "expected exactly 2 BLOCK records, got {}",
        block_starts.len()
    );

    fn extract_layer(lines: &[&str], block_start: usize) -> String {
        let mut j = block_start + 2;
        while j + 1 < lines.len() && lines[j].trim() != "0" {
            if lines[j].trim() == "8" {
                return lines[j + 1].trim().to_string();
            }
            j += 2;
        }
        panic!("no code-8 layer found in BLOCK header starting at line {block_start}");
    }

    assert_eq!(
        extract_layer(&lines, block_starts[0]),
        "0",
        "first BLOCK uses default layer \"0\""
    );
    assert_eq!(
        extract_layer(&lines, block_starts[1]),
        "A-BLOCK-DEFS",
        "second BLOCK uses custom layer"
    );

    // Re-read and verify the layer round-trips on the in-memory
    // struct.
    let reparsed = DxfReader::read_str(&written).expect("re-read");
    let default_block = reparsed
        .block_records
        .iter()
        .find(|b| b.name == "DEFAULT_LAYER_BLOCK")
        .expect("default-layer block re-reads");
    assert_eq!(default_block.layer, "0");
    let custom_block = reparsed
        .block_records
        .iter()
        .find(|b| b.name == "CUSTOM_LAYER_BLOCK")
        .expect("custom-layer block re-reads");
    assert_eq!(custom_block.layer, "A-BLOCK-DEFS");
}

#[test]
fn dxf_parse_blocks_or_merges_flags_from_block_record_and_block() {
    // Regression for the conditional-flags-merge finding: the prior
    // "if existing.flags == 0 { existing.flags = flags; }" branch
    // silently dropped flag bits when a BLOCK_RECORD-table entry
    // and the matching BLOCK definition disagreed (the BLOCK_RECORD
    // bits would win even though both fields are bitfields).
    // OR-merging preserves information from non-conforming files
    // and is a no-op for conforming files where the two sides set
    // the same bits.
    //
    // Synthesise a DXF with a BLOCK_RECORD-table flags=1
    // (anonymous) and a BLOCK entity flags=4 (xref overlay); the
    // merged record must end up with flags=5.
    let dxf = r"  0
SECTION
  2
HEADER
  9
$ACADVER
  1
AC1009
  0
ENDSEC
  0
SECTION
  2
TABLES
  0
TABLE
  2
BLOCK_RECORD
 70
1
  0
BLOCK_RECORD
  2
MERGED_FLAGS
 70
1
  0
ENDTAB
  0
ENDSEC
  0
SECTION
  2
BLOCKS
  0
BLOCK
  8
0
  2
MERGED_FLAGS
 70
4
 10
0.0
 20
0.0
 30
0.0
  0
ENDBLK
  0
ENDSEC
  0
EOF
";
    let doc = DxfReader::read_str(dxf).expect("parse merged-flags fixture");
    let block = doc
        .block_records
        .iter()
        .find(|b| b.name == "MERGED_FLAGS")
        .expect("MERGED_FLAGS block present");
    assert_eq!(
        block.flags, 5,
        "BLOCK_RECORD (1) | BLOCK (4) must OR-merge to 5"
    );
}
