//! DXF ASCII writer.

use std::io::Write;

use crate::dxf::entities::{
    DxfArc, DxfCircle, DxfEntity, DxfInsert, DxfLine, DxfPolyline, DxfText,
};
use crate::dxf::DxfDocument;
use crate::error::CadResult;

pub struct DxfWriter;

impl DxfWriter {
    pub fn write<W: Write>(doc: &DxfDocument, w: &mut W) -> CadResult<()> {
        write_header(w)?;
        write_tables(doc, w)?;
        write_blocks(doc, w)?;
        write_entities(doc, w)?;
        write_pair(w, 0, "EOF")?;
        Ok(())
    }

    pub fn write_to_string(doc: &DxfDocument) -> CadResult<String> {
        let mut buf: Vec<u8> = Vec::new();
        Self::write(doc, &mut buf)?;
        Ok(String::from_utf8(buf).expect("DXF writer emits ASCII only"))
    }
}

fn write_pair<W: Write>(w: &mut W, code: i32, value: &str) -> CadResult<()> {
    // DXF wants `code` right-aligned in width 3, but most parsers (including
    // ours) accept any whitespace-stripped integer.
    writeln!(w, "{:>3}", code)?;
    writeln!(w, "{}", value)?;
    Ok(())
}

fn write_header<W: Write>(w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "SECTION")?;
    write_pair(w, 2, "HEADER")?;
    write_pair(w, 9, "$ACADVER")?;
    write_pair(w, 1, "AC1009")?;
    write_pair(w, 9, "$INSUNITS")?;
    write_pair(w, 70, "4")?; // 4 = mm
    write_pair(w, 0, "ENDSEC")?;
    Ok(())
}

fn write_tables<W: Write>(doc: &DxfDocument, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "SECTION")?;
    write_pair(w, 2, "TABLES")?;

    // LAYER table.
    write_pair(w, 0, "TABLE")?;
    write_pair(w, 2, "LAYER")?;
    write_pair(w, 70, &doc.layers.len().to_string())?;
    for layer in doc.layers.iter() {
        write_pair(w, 0, "LAYER")?;
        write_pair(w, 2, &layer.name)?;
        let mut flags = 0i32;
        if layer.frozen {
            flags |= 1;
        }
        if layer.locked {
            flags |= 4;
        }
        write_pair(w, 70, &flags.to_string())?;
        write_pair(w, 62, &layer.color.0.to_string())?;
        write_pair(w, 6, &layer.linetype)?;
        write_pair(w, 370, &layer.lineweight.0.to_string())?;
    }
    write_pair(w, 0, "ENDTAB")?;

    // BLOCK_RECORD table.
    write_pair(w, 0, "TABLE")?;
    write_pair(w, 2, "BLOCK_RECORD")?;
    write_pair(w, 70, &doc.block_records.len().to_string())?;
    for br in &doc.block_records {
        write_pair(w, 0, "BLOCK_RECORD")?;
        write_pair(w, 2, &br.name)?;
        write_pair(w, 70, &br.flags.to_string())?;
        if let Some(desc) = &br.description {
            write_pair(w, 4, desc)?;
        }
    }
    write_pair(w, 0, "ENDTAB")?;

    // DIMSTYLE table.
    write_pair(w, 0, "TABLE")?;
    write_pair(w, 2, "DIMSTYLE")?;
    write_pair(w, 70, &doc.dim_styles.len().to_string())?;
    for dim in &doc.dim_styles {
        write_pair(w, 0, "DIMSTYLE")?;
        write_pair(w, 2, &dim.name)?;
        write_pair(w, 70, "0")?;
        write_pair(w, 140, &fmt_f(dim.text_height))?;
        write_pair(w, 141, &fmt_f(dim.arrow_size))?;
        write_pair(w, 144, &fmt_f(dim.units_scale))?;
    }
    write_pair(w, 0, "ENDTAB")?;

    write_pair(w, 0, "ENDSEC")?;
    Ok(())
}

fn write_blocks<W: Write>(doc: &DxfDocument, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "SECTION")?;
    write_pair(w, 2, "BLOCKS")?;
    for br in &doc.block_records {
        write_pair(w, 0, "BLOCK")?;
        write_pair(w, 2, &br.name)?;
        write_pair(w, 70, &br.flags.to_string())?;
        write_pair(w, 10, "0.0")?;
        write_pair(w, 20, "0.0")?;
        write_pair(w, 30, "0.0")?;
        write_pair(w, 0, "ENDBLK")?;
    }
    write_pair(w, 0, "ENDSEC")?;
    Ok(())
}

fn write_entities<W: Write>(doc: &DxfDocument, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "SECTION")?;
    write_pair(w, 2, "ENTITIES")?;
    for entity in &doc.entities {
        match entity {
            DxfEntity::Line(e) => write_line(e, w)?,
            DxfEntity::Polyline(e) => write_polyline(e, w)?,
            DxfEntity::Arc(e) => write_arc(e, w)?,
            DxfEntity::Circle(e) => write_circle(e, w)?,
            DxfEntity::Text(e) => write_text(e, w)?,
            DxfEntity::Insert(e) => write_insert(e, w)?,
        }
    }
    write_pair(w, 0, "ENDSEC")?;
    Ok(())
}

fn write_line<W: Write>(e: &DxfLine, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "LINE")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 10, &fmt_f(e.start[0]))?;
    write_pair(w, 20, &fmt_f(e.start[1]))?;
    write_pair(w, 30, &fmt_f(e.start[2]))?;
    write_pair(w, 11, &fmt_f(e.end[0]))?;
    write_pair(w, 21, &fmt_f(e.end[1]))?;
    write_pair(w, 31, &fmt_f(e.end[2]))?;
    Ok(())
}

fn write_polyline<W: Write>(e: &DxfPolyline, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "LWPOLYLINE")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 90, &e.vertices.len().to_string())?;
    write_pair(w, 70, &i32::from(e.closed).to_string())?;
    write_pair(w, 38, &fmt_f(e.elevation))?;
    for v in &e.vertices {
        write_pair(w, 10, &fmt_f(v[0]))?;
        write_pair(w, 20, &fmt_f(v[1]))?;
    }
    Ok(())
}

fn write_arc<W: Write>(e: &DxfArc, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "ARC")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 10, &fmt_f(e.center[0]))?;
    write_pair(w, 20, &fmt_f(e.center[1]))?;
    write_pair(w, 30, &fmt_f(e.center[2]))?;
    write_pair(w, 40, &fmt_f(e.radius))?;
    write_pair(w, 50, &fmt_f(e.start_angle))?;
    write_pair(w, 51, &fmt_f(e.end_angle))?;
    Ok(())
}

fn write_circle<W: Write>(e: &DxfCircle, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "CIRCLE")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 10, &fmt_f(e.center[0]))?;
    write_pair(w, 20, &fmt_f(e.center[1]))?;
    write_pair(w, 30, &fmt_f(e.center[2]))?;
    write_pair(w, 40, &fmt_f(e.radius))?;
    Ok(())
}

fn write_text<W: Write>(e: &DxfText, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "TEXT")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 10, &fmt_f(e.position[0]))?;
    write_pair(w, 20, &fmt_f(e.position[1]))?;
    write_pair(w, 30, &fmt_f(e.position[2]))?;
    write_pair(w, 40, &fmt_f(e.height))?;
    write_pair(w, 50, &fmt_f(e.rotation))?;
    write_pair(w, 1, &e.text)?;
    Ok(())
}

fn write_insert<W: Write>(e: &DxfInsert, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "INSERT")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 2, &e.block_name)?;
    write_pair(w, 10, &fmt_f(e.position[0]))?;
    write_pair(w, 20, &fmt_f(e.position[1]))?;
    write_pair(w, 30, &fmt_f(e.position[2]))?;
    write_pair(w, 41, &fmt_f(e.scale[0]))?;
    write_pair(w, 42, &fmt_f(e.scale[1]))?;
    write_pair(w, 43, &fmt_f(e.scale[2]))?;
    write_pair(w, 50, &fmt_f(e.rotation))?;
    Ok(())
}

fn fmt_f(v: f64) -> String {
    // DXF uses dot decimal separator regardless of locale.
    format!("{:.10}", v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::DxfReader;
    use crate::layers::Layer;

    fn rich_doc() -> DxfDocument {
        let mut doc = DxfDocument::new();
        doc.layers.upsert(Layer::new("WALLS").unwrap());
        doc.layers.upsert(Layer::new("DIMS").unwrap());
        doc.block_records
            .push(crate::dxf::tables::DxfBlockRecord::new("DOOR_900"));
        doc.dim_styles.push(crate::dxf::DxfDimStyle {
            name: "ARCH".into(),
            text_height: 3.5,
            arrow_size: 3.5,
            units_scale: 1000.0,
        });
        doc.push(DxfEntity::Line(DxfLine {
            layer: "WALLS".into(),
            start: [0.0, 0.0, 0.0],
            end: [4000.0, 0.0, 0.0],
        }));
        doc.push(DxfEntity::Polyline(DxfPolyline {
            layer: "WALLS".into(),
            vertices: vec![[0.0, 0.0], [4000.0, 0.0], [4000.0, 3000.0], [0.0, 3000.0]],
            closed: true,
            elevation: 0.0,
        }));
        doc.push(DxfEntity::Arc(DxfArc {
            layer: "WALLS".into(),
            center: [1000.0, 1000.0, 0.0],
            radius: 250.0,
            start_angle: 0.0,
            end_angle: 90.0,
        }));
        doc.push(DxfEntity::Circle(DxfCircle {
            layer: "WALLS".into(),
            center: [2000.0, 1500.0, 0.0],
            radius: 100.0,
        }));
        doc.push(DxfEntity::Text(DxfText {
            layer: "DIMS".into(),
            position: [500.0, 500.0, 0.0],
            height: 2.5,
            rotation: 0.0,
            text: "LIVING ROOM".into(),
        }));
        doc.push(DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "DOOR_900".into(),
            position: [1500.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        }));
        doc
    }

    #[test]
    fn roundtrip_all_entity_types() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        // Layer table preserved (3: "0" auto, plus 2 user-added).
        assert!(parsed.layers.get("WALLS").is_some());
        assert!(parsed.layers.get("DIMS").is_some());
        // Block records preserved.
        assert_eq!(parsed.block_records.len(), 1);
        assert_eq!(parsed.block_records[0].name, "DOOR_900");
        // DimStyles preserved (STANDARD + ARCH).
        assert!(parsed.dim_styles.iter().any(|d| d.name == "ARCH"));
        // Entities preserved (order).
        assert_eq!(parsed.entities.len(), doc.entities.len());
        for (a, b) in parsed.entities.iter().zip(doc.entities.iter()) {
            assert_eq!(std::mem::discriminant(a), std::mem::discriminant(b));
        }
    }

    #[test]
    fn polyline_roundtrips_vertices_and_closed_flag() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Polyline(p) = &parsed.entities[1] else {
            panic!("expected polyline at index 1");
        };
        assert_eq!(p.vertices.len(), 4);
        assert!(p.closed);
        assert!((p.vertices[2][0] - 4000.0).abs() < 1e-6);
        assert!((p.vertices[2][1] - 3000.0).abs() < 1e-6);
    }

    #[test]
    fn arc_angles_roundtrip() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Arc(a) = &parsed.entities[2] else {
            panic!("expected arc at index 2");
        };
        assert!((a.start_angle - 0.0).abs() < 1e-6);
        assert!((a.end_angle - 90.0).abs() < 1e-6);
        assert!((a.radius - 250.0).abs() < 1e-6);
    }
}
