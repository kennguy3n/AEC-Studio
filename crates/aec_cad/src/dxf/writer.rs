//! DXF ASCII writer.

use std::collections::HashMap;
use std::io::Write;

use crate::dxf::entities::{
    DxfArc, DxfAttdef, DxfCircle, DxfDimension, DxfDimensionKind, DxfEllipse, DxfEntity, DxfHatch,
    DxfInsert, DxfLine, DxfPolyline, DxfSpline, DxfText,
};
use crate::dxf::DxfDocument;
use crate::error::CadResult;

/// Lowest hex handle minted for the first STYLE table entry; subsequent
/// entries get sequentially higher handles. Picked above the typical
/// header / document-level reserved range (0x1–0xFF, which mainstream
/// CAD apps use for `$HANDSEED`, document records, viewports, etc.) so
/// synthetic handles for our STYLE table never visually collide with a
/// document-level handle a reader might be expecting at the same
/// position. DXF handles are file-local opaque identifiers — the
/// numeric value carries no semantic weight — but starting high keeps
/// our minted range cleanly separated from low handles that other
/// writers conventionally reserve for the document header.
const STYLE_HANDLE_BASE: u32 = 0x100;

/// Mints a deterministic uppercase-hex handle for every text style in
/// the document, in declaration order. Returns a map from style name
/// to handle that the writer uses both to emit code-5 on each STYLE
/// record and to resolve the symbolic `text_style` reference on each
/// DIMSTYLE record into a hard-pointer handle (code 340) as the DXF
/// specification requires for that group code.
fn mint_text_style_handles(doc: &DxfDocument) -> HashMap<String, String> {
    doc.text_styles
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            (
                s.name.clone(),
                format!("{:X}", STYLE_HANDLE_BASE + idx as u32),
            )
        })
        .collect()
}

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
        // DXF convention: a layer that is OFF emits its colour as a
        // negative number (e.g. -7 = white but off). This is how
        // every mainstream DXF consumer (AutoCAD, BricsCAD, QCAD,
        // LibreDWG) signals the on/off state for AC1009-era files.
        let signed_color: i32 = if layer.on {
            i32::from(layer.color.0)
        } else {
            -i32::from(layer.color.0)
        };
        write_pair(w, 62, &signed_color.to_string())?;
        write_pair(w, 6, &layer.linetype)?;
        write_pair(w, 370, &layer.lineweight.0.to_string())?;
        // Plottable / not-plottable lives at DXF code 290 in the
        // 1000+ namespace (boolean). Emit it unconditionally so the
        // round-trip is symmetric.
        write_pair(w, 290, &i32::from(layer.plottable).to_string())?;
        if let Some(desc) = &layer.description {
            // Layer description is conventionally carried as XDATA on
            // AutoCAD, but for our internal round-trip we use group
            // code 4 (which is otherwise unused for LAYER) so the
            // payload is fully ASCII and any DXF parser we
            // control sees the same bytes.
            write_pair(w, 4, desc)?;
        }
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

    // STYLE (text style) table. Each entry gets a deterministic
    // hex handle minted up-front so the DIMSTYLE 340 group code can
    // emit a real hard-pointer handle (as the DXF spec requires)
    // rather than the symbolic style name. Without this, third-party
    // DXF consumers (AutoCAD, BricsCAD, QCAD, LibreDWG) see a string
    // where they expect a hex handle on 340 and silently fall back
    // to the default style for every dimension reference.
    let style_handle_by_name = mint_text_style_handles(doc);
    write_pair(w, 0, "TABLE")?;
    write_pair(w, 2, "STYLE")?;
    write_pair(w, 70, &doc.text_styles.len().to_string())?;
    for s in &doc.text_styles {
        write_pair(w, 0, "STYLE")?;
        if let Some(handle) = style_handle_by_name.get(&s.name) {
            write_pair(w, 5, handle)?;
        }
        write_pair(w, 2, &s.name)?;
        write_pair(w, 70, "0")?;
        write_pair(w, 40, &fmt_f(s.fixed_height))?;
        write_pair(w, 41, &fmt_f(s.width_factor))?;
        write_pair(w, 50, &fmt_f(s.oblique_angle))?;
        write_pair(w, 3, &s.font_filename)?;
        write_pair(w, 4, &s.bigfont_filename)?;
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
        // DIMDEC — primary-units decimal places.
        write_pair(w, 271, &dim.decimal_places.to_string())?;
        // DIMTXSTY — hard-pointer handle of the referenced STYLE
        // table entry. We resolve the symbolic `text_style` name
        // through the same handle map we used to emit code-5 on each
        // STYLE record above. When the name doesn't match any STYLE
        // (e.g. a doc constructed without a STYLE table, or a
        // back-compat fixture), we fall back to emitting the name
        // verbatim — the reader's symmetric fallback recovers it.
        match style_handle_by_name.get(&dim.text_style) {
            Some(handle) => write_pair(w, 340, handle)?,
            None => write_pair(w, 340, &dim.text_style)?,
        }
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
        // Group code 8: layer the BLOCK entity sits on. Required by
        // the DXF spec on the BLOCK entity inside the BLOCKS section.
        // Strict third-party consumers (AutoCAD, BricsCAD, LibreDWG,
        // QCAD) reject or warn on a missing code-8. Defaults to "0"
        // so that any `BYLAYER` colors on entities inside the block
        // resolve through the insert's layer at draw time rather
        // than being baked into the block definition.
        write_pair(w, 8, &br.layer)?;
        write_pair(w, 2, &br.name)?;
        write_pair(w, 70, &br.flags.to_string())?;
        write_pair(w, 10, &fmt_f(br.base_point[0]))?;
        write_pair(w, 20, &fmt_f(br.base_point[1]))?;
        write_pair(w, 30, &fmt_f(br.base_point[2]))?;
        for entity in &br.entities {
            write_entity(entity, w)?;
        }
        // ENDBLK is itself an entity per the DXF spec and carries the
        // same layer assignment (code 8) as its parent BLOCK. AutoCAD,
        // BricsCAD, LibreDWG and QCAD all emit/expect a layer code on
        // ENDBLK; omitting it triggers either a hard reject or a
        // sticky "missing layer" warning at file load.
        write_pair(w, 0, "ENDBLK")?;
        write_pair(w, 8, &br.layer)?;
    }
    write_pair(w, 0, "ENDSEC")?;
    Ok(())
}

fn write_entities<W: Write>(doc: &DxfDocument, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "SECTION")?;
    write_pair(w, 2, "ENTITIES")?;
    for entity in &doc.entities {
        write_entity(entity, w)?;
    }
    write_pair(w, 0, "ENDSEC")?;
    Ok(())
}

fn write_entity<W: Write>(entity: &DxfEntity, w: &mut W) -> CadResult<()> {
    match entity {
        DxfEntity::Line(e) => write_line(e, w),
        DxfEntity::Polyline(e) => write_polyline(e, w),
        DxfEntity::Arc(e) => write_arc(e, w),
        DxfEntity::Circle(e) => write_circle(e, w),
        DxfEntity::Ellipse(e) => write_ellipse(e, w),
        DxfEntity::Spline(e) => write_spline(e, w),
        DxfEntity::Hatch(e) => write_hatch(e, w),
        DxfEntity::Text(e) => write_text(e, w),
        DxfEntity::Insert(e) => write_insert(e, w),
        DxfEntity::Dimension(e) => write_dimension(e, w),
        DxfEntity::Attdef(e) => write_attdef(e, w),
    }
}

fn write_attdef<W: Write>(e: &DxfAttdef, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "ATTDEF")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 10, &fmt_f(e.position[0]))?;
    write_pair(w, 20, &fmt_f(e.position[1]))?;
    write_pair(w, 30, &fmt_f(e.position[2]))?;
    write_pair(w, 40, &fmt_f(e.height))?;
    write_pair(w, 50, &fmt_f(e.rotation))?;
    write_pair(w, 1, &e.default_value)?;
    write_pair(w, 2, &e.tag)?;
    write_pair(w, 3, &e.prompt)?;
    write_pair(w, 70, &e.flags.to_string())?;
    write_pair(w, 7, &e.text_style)?;
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
        write_pair(w, 10, &fmt_f(v.x))?;
        write_pair(w, 20, &fmt_f(v.y))?;
        if v.bulge != 0.0 {
            write_pair(w, 42, &fmt_f(v.bulge))?;
        }
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

fn write_ellipse<W: Write>(e: &DxfEllipse, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "ELLIPSE")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 10, &fmt_f(e.center[0]))?;
    write_pair(w, 20, &fmt_f(e.center[1]))?;
    write_pair(w, 30, &fmt_f(e.center[2]))?;
    write_pair(w, 11, &fmt_f(e.major_axis[0]))?;
    write_pair(w, 21, &fmt_f(e.major_axis[1]))?;
    write_pair(w, 31, &fmt_f(e.major_axis[2]))?;
    write_pair(w, 40, &fmt_f(e.ratio))?;
    write_pair(w, 41, &fmt_f(e.start_param))?;
    write_pair(w, 42, &fmt_f(e.end_param))?;
    Ok(())
}

fn write_spline<W: Write>(e: &DxfSpline, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "SPLINE")?;
    write_pair(w, 8, &e.layer)?;
    let flags = i32::from(e.closed);
    write_pair(w, 70, &flags.to_string())?;
    write_pair(w, 71, &e.degree.to_string())?;
    write_pair(w, 72, &e.knots.len().to_string())?;
    write_pair(w, 73, &e.control_points.len().to_string())?;
    for k in &e.knots {
        write_pair(w, 40, &fmt_f(*k))?;
    }
    for cp in &e.control_points {
        write_pair(w, 10, &fmt_f(cp[0]))?;
        write_pair(w, 20, &fmt_f(cp[1]))?;
        write_pair(w, 30, &fmt_f(cp[2]))?;
    }
    Ok(())
}

fn write_hatch<W: Write>(e: &DxfHatch, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "HATCH")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 2, &e.pattern_name)?;
    write_pair(w, 70, &i32::from(e.solid).to_string())?;
    write_pair(w, 38, &fmt_f(e.elevation))?;
    write_pair(w, 40, &fmt_f(e.scale))?;
    write_pair(w, 41, &fmt_f(e.angle))?;
    write_pair(w, 91, &e.loops.len().to_string())?;
    for lp in &e.loops {
        write_pair(w, 93, &lp.vertices.len().to_string())?;
        for v in &lp.vertices {
            write_pair(w, 10, &fmt_f(v[0]))?;
            write_pair(w, 20, &fmt_f(v[1]))?;
        }
    }
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

fn write_dimension<W: Write>(e: &DxfDimension, w: &mut W) -> CadResult<()> {
    write_pair(w, 0, "DIMENSION")?;
    write_pair(w, 8, &e.layer)?;
    write_pair(w, 3, &e.style)?;
    let kind_code = match e.kind {
        DxfDimensionKind::Linear => 0,
        DxfDimensionKind::Aligned => 1,
        DxfDimensionKind::Angular => 2,
        DxfDimensionKind::Diameter => 3,
        DxfDimensionKind::Radial => 4,
    };
    write_pair(w, 70, &kind_code.to_string())?;
    write_pair(w, 10, &fmt_f(e.def_point[0]))?;
    write_pair(w, 20, &fmt_f(e.def_point[1]))?;
    write_pair(w, 30, &fmt_f(e.def_point[2]))?;
    write_pair(w, 11, &fmt_f(e.text_position[0]))?;
    write_pair(w, 21, &fmt_f(e.text_position[1]))?;
    write_pair(w, 31, &fmt_f(e.text_position[2]))?;
    write_pair(w, 12, &fmt_f(e.def_point_a[0]))?;
    write_pair(w, 22, &fmt_f(e.def_point_a[1]))?;
    write_pair(w, 32, &fmt_f(e.def_point_a[2]))?;
    write_pair(w, 13, &fmt_f(e.def_point_b[0]))?;
    write_pair(w, 23, &fmt_f(e.def_point_b[1]))?;
    write_pair(w, 33, &fmt_f(e.def_point_b[2]))?;
    if let Some(t) = &e.override_text {
        write_pair(w, 1, t)?;
    }
    if let Some(v) = e.measured_value {
        write_pair(w, 42, &fmt_f(v))?;
    }
    Ok(())
}

fn fmt_f(v: f64) -> String {
    format!("{:.10}", v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::entities::{
        DxfDimension, DxfDimensionKind, DxfEllipse, DxfHatch, DxfHatchLoop, DxfPolylineVertex,
        DxfSpline,
    };
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
            decimal_places: 2,
            text_style: "STANDARD".into(),
        });
        doc.push(DxfEntity::Line(DxfLine {
            layer: "WALLS".into(),
            start: [0.0, 0.0, 0.0],
            end: [4000.0, 0.0, 0.0],
        }));
        doc.push(DxfEntity::Polyline(DxfPolyline {
            layer: "WALLS".into(),
            vertices: vec![
                DxfPolylineVertex::new(0.0, 0.0),
                DxfPolylineVertex {
                    x: 4000.0,
                    y: 0.0,
                    bulge: 0.5,
                },
                DxfPolylineVertex::new(4000.0, 3000.0),
                DxfPolylineVertex::new(0.0, 3000.0),
            ],
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
        doc.push(DxfEntity::Ellipse(DxfEllipse {
            layer: "WALLS".into(),
            center: [3000.0, 1500.0, 0.0],
            major_axis: [500.0, 0.0, 0.0],
            ratio: 0.5,
            start_param: 0.0,
            end_param: std::f64::consts::TAU,
        }));
        doc.push(DxfEntity::Spline(DxfSpline {
            layer: "WALLS".into(),
            degree: 3,
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            control_points: vec![
                [0.0, 0.0, 0.0],
                [100.0, 100.0, 0.0],
                [200.0, 100.0, 0.0],
                [300.0, 0.0, 0.0],
            ],
            closed: false,
        }));
        doc.push(DxfEntity::Hatch(DxfHatch {
            layer: "WALLS".into(),
            pattern_name: "SOLID".into(),
            solid: true,
            scale: 1.0,
            angle: 0.0,
            elevation: 0.0,
            loops: vec![DxfHatchLoop {
                vertices: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            }],
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
        doc.push(DxfEntity::Dimension(DxfDimension {
            layer: "DIMS".into(),
            style: "ARCH".into(),
            kind: DxfDimensionKind::Linear,
            def_point: [0.0, 0.0, 0.0],
            text_position: [2000.0, -200.0, 0.0],
            def_point_a: [0.0, 0.0, 0.0],
            def_point_b: [4000.0, 0.0, 0.0],
            override_text: None,
            measured_value: Some(4000.0),
        }));
        doc
    }

    #[test]
    fn roundtrip_all_entity_types() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        assert!(parsed.layers.get("WALLS").is_some());
        assert!(parsed.layers.get("DIMS").is_some());
        assert_eq!(parsed.block_records.len(), 1);
        assert_eq!(parsed.block_records[0].name, "DOOR_900");
        assert!(parsed.dim_styles.iter().any(|d| d.name == "ARCH"));
        assert_eq!(parsed.entities.len(), doc.entities.len());
        for (a, b) in parsed.entities.iter().zip(doc.entities.iter()) {
            assert_eq!(std::mem::discriminant(a), std::mem::discriminant(b));
        }
    }

    #[test]
    fn polyline_roundtrips_vertices_and_closed_flag_and_bulge() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Polyline(p) = &parsed.entities[1] else {
            panic!("expected polyline at index 1");
        };
        assert_eq!(p.vertices.len(), 4);
        assert!(p.closed);
        assert!((p.vertices[2].x - 4000.0).abs() < 1e-6);
        assert!((p.vertices[2].y - 3000.0).abs() < 1e-6);
        assert!((p.vertices[1].bulge - 0.5).abs() < 1e-6);
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

    #[test]
    fn ellipse_ratio_roundtrip() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Ellipse(e) = &parsed.entities[4] else {
            panic!("expected ellipse at index 4");
        };
        assert!((e.ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn spline_control_points_roundtrip() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Spline(s) = &parsed.entities[5] else {
            panic!("expected spline at index 5");
        };
        assert_eq!(s.control_points.len(), 4);
        assert_eq!(s.knots.len(), 8);
        assert_eq!(s.degree, 3);
    }

    #[test]
    fn hatch_loops_roundtrip() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Hatch(h) = &parsed.entities[6] else {
            panic!("expected hatch at index 6");
        };
        assert_eq!(h.loops.len(), 1);
        assert_eq!(h.loops[0].vertices.len(), 4);
        assert!(h.solid);
    }

    #[test]
    fn dimension_roundtrip() {
        let doc = rich_doc();
        let text = DxfWriter::write_to_string(&doc).unwrap();
        let parsed = DxfReader::read_str(&text).unwrap();
        let DxfEntity::Dimension(d) = &parsed.entities[9] else {
            panic!("expected dimension at index 9");
        };
        assert_eq!(d.kind, DxfDimensionKind::Linear);
        assert_eq!(d.style, "ARCH");
    }
}
