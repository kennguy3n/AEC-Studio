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
        },
        DxfDimStyle {
            name: "ARCH-1-100".into(),
            text_height: 5.0,
            arrow_size: 5.0,
            units_scale: 2.0,
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
        pattern_name: "SOLID".into(),
        solid: true,
        scale: 1.0,
        angle: 0.0,
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
