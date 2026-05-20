//! DXF ASCII reader. Implements a working subset of the spec covering the
//! header/tables/blocks/entities sections, the entities `LINE`,
//! `LWPOLYLINE`, `POLYLINE` (with VERTEX members), `ARC`, `CIRCLE`,
//! `ELLIPSE`, `SPLINE`, `HATCH`, `TEXT`, `MTEXT`, `DIMENSION`, `INSERT`,
//! and the `LAYER` + `BLOCK_RECORD` + `DIMSTYLE` tables.

use std::io::{BufRead, BufReader, Read};

use crate::dxf::entities::{
    DxfArc, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfEllipse, DxfEntity,
    DxfHatch, DxfHatchLoop, DxfInsert, DxfLine, DxfPolyline, DxfPolylineVertex, DxfSpline, DxfText,
};
use crate::dxf::tables::DxfBlockRecord;
use crate::dxf::DxfDocument;
use crate::error::{CadError, CadResult};
use crate::layers::{Layer, LayerColor, LayerLineweight, LayerSystem};

/// A single group code/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    code: i32,
    value: String,
}

pub struct DxfReader;

impl DxfReader {
    pub fn read<R: Read>(reader: R) -> CadResult<DxfDocument> {
        let buf = BufReader::new(reader);
        let mut lines = buf.lines();
        let mut groups: Vec<Group> = Vec::new();
        while let Some(code_line) = lines.next() {
            let code_line = code_line?;
            let Some(value_line) = lines.next() else {
                return Err(CadError::UnexpectedEof);
            };
            let value_line = value_line?;
            let code: i32 = code_line
                .trim()
                .parse()
                .map_err(|_| CadError::InvalidGroupCode(0))?;
            groups.push(Group {
                code,
                value: value_line.trim_end_matches('\r').to_string(),
            });
        }
        parse(&groups)
    }

    pub fn read_str(s: &str) -> CadResult<DxfDocument> {
        Self::read(s.as_bytes())
    }
}

fn parse(groups: &[Group]) -> CadResult<DxfDocument> {
    let mut doc = DxfDocument::new();
    // Clear the auto-added "0" so we can re-insert it from the file if present.
    doc.layers = LayerSystem::default();
    // The constructor seeds a default STANDARD dim style for new
    // documents. When parsing an existing DXF the dim-style table is
    // the source of truth, so drop the seed and re-populate from the
    // file. Without this the roundtrip would silently grow STANDARD on
    // every read.
    doc.dim_styles.clear();
    let mut i = 0;
    while i < groups.len() {
        if groups[i].code == 0 && groups[i].value == "SECTION" {
            i += 1; // consume SECTION token, now advance to name (code 2).
            if i >= groups.len() {
                return Err(CadError::UnexpectedEof);
            }
            let section_name = &groups[i].value;
            i += 1;
            match section_name.as_str() {
                "TABLES" => i = parse_tables(groups, i, &mut doc)?,
                "BLOCKS" => i = skip_until_endsec(groups, i),
                "ENTITIES" => i = parse_entities(groups, i, &mut doc),
                _ => i = skip_until_endsec(groups, i),
            }
        } else if groups[i].code == 0 && groups[i].value == "EOF" {
            break;
        } else {
            i += 1;
        }
    }
    // Make sure layer "0" always exists.
    if doc.layers.get("0").is_none() {
        doc.layers.upsert(Layer::new("0")?);
    }
    Ok(doc)
}

fn skip_until_endsec(groups: &[Group], mut i: usize) -> usize {
    while i < groups.len() {
        if groups[i].code == 0 && groups[i].value == "ENDSEC" {
            return i + 1;
        }
        i += 1;
    }
    i
}

fn parse_tables(groups: &[Group], mut i: usize, doc: &mut DxfDocument) -> CadResult<usize> {
    while i < groups.len() {
        let g = &groups[i];
        if g.code == 0 && g.value == "ENDSEC" {
            return Ok(i + 1);
        }
        if g.code == 0 && g.value == "TABLE" {
            i += 1;
            let table_name = if i < groups.len() && groups[i].code == 2 {
                let n = groups[i].value.clone();
                i += 1;
                n
            } else {
                continue;
            };
            i = parse_table_entries(groups, i, &table_name, doc)?;
        } else {
            i += 1;
        }
    }
    Ok(i)
}

fn parse_table_entries(
    groups: &[Group],
    mut i: usize,
    table_name: &str,
    doc: &mut DxfDocument,
) -> CadResult<usize> {
    while i < groups.len() {
        let g = &groups[i];
        if g.code == 0 && g.value == "ENDTAB" {
            return Ok(i + 1);
        }
        if g.code == 0 {
            let entry_type = g.value.clone();
            let mut fields: Vec<(i32, String)> = Vec::new();
            i += 1;
            while i < groups.len() && groups[i].code != 0 {
                fields.push((groups[i].code, groups[i].value.clone()));
                i += 1;
            }
            match (table_name, entry_type.as_str()) {
                ("LAYER", "LAYER") => {
                    let layer = parse_layer_entry(&fields)?;
                    doc.layers.upsert(layer);
                }
                ("BLOCK_RECORD", "BLOCK_RECORD") => {
                    let mut br = DxfBlockRecord::new("");
                    for (code, val) in &fields {
                        match code {
                            2 => br.name.clone_from(val),
                            4 => br.description = Some(val.clone()),
                            70 => br.flags = val.parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                    if !br.name.is_empty() {
                        doc.block_records.push(br);
                    }
                }
                ("DIMSTYLE", "DIMSTYLE") => {
                    let mut dim = DxfDimStyle::standard();
                    for (code, val) in &fields {
                        match code {
                            2 => dim.name.clone_from(val),
                            140 => dim.text_height = val.parse().unwrap_or(2.5),
                            141 => dim.arrow_size = val.parse().unwrap_or(2.5),
                            144 => dim.units_scale = val.parse().unwrap_or(1.0),
                            _ => {}
                        }
                    }
                    doc.dim_styles.push(dim);
                }
                _ => {}
            }
        } else {
            i += 1;
        }
    }
    Ok(i)
}

fn parse_layer_entry(fields: &[(i32, String)]) -> CadResult<Layer> {
    let mut name = String::new();
    let mut color = LayerColor::WHITE;
    let mut linetype = "CONTINUOUS".to_string();
    let mut lineweight = LayerLineweight::DEFAULT;
    let mut flags: i32 = 0;
    for (code, val) in fields {
        match code {
            2 => name.clone_from(val),
            6 => linetype.clone_from(val),
            62 => {
                let raw: i16 = val.parse().unwrap_or(7);
                color = LayerColor(raw.abs());
                if raw < 0 {
                    flags |= 1;
                }
            }
            70 => flags = val.parse().unwrap_or(0),
            370 => lineweight = LayerLineweight(val.parse().unwrap_or(-3)),
            _ => {}
        }
    }
    let mut layer = Layer::new(name)?;
    layer.color = color;
    layer.linetype = linetype;
    layer.lineweight = lineweight;
    layer.frozen = (flags & 1) != 0;
    layer.locked = (flags & 4) != 0;
    Ok(layer)
}

fn parse_entities(groups: &[Group], mut i: usize, doc: &mut DxfDocument) -> usize {
    while i < groups.len() {
        let g = &groups[i];
        if g.code == 0 && g.value == "ENDSEC" {
            return i + 1;
        }
        if g.code == 0 {
            let entity_type = g.value.clone();
            // HATCH has nested boundary records, which we handle below.
            let is_polyline = entity_type == "POLYLINE";
            let is_hatch = entity_type == "HATCH";
            let mut fields: Vec<(i32, String)> = Vec::new();
            // Hatch boundary loop accumulation.
            let mut hatch_loops: Vec<DxfHatchLoop> = Vec::new();
            let mut hatch_current_loop: Vec<[f64; 2]> = Vec::new();
            i += 1;
            while i < groups.len() {
                if groups[i].code == 0 {
                    if is_polyline && (groups[i].value == "VERTEX" || groups[i].value == "SEQEND") {
                        if groups[i].value == "SEQEND" {
                            i += 1;
                            while i < groups.len() && groups[i].code != 0 {
                                i += 1;
                            }
                            break;
                        }
                        i += 1;
                        let mut x = 0.0;
                        let mut y = 0.0;
                        let mut bulge = 0.0;
                        while i < groups.len() && groups[i].code != 0 {
                            match groups[i].code {
                                10 => x = groups[i].value.parse().unwrap_or(0.0),
                                20 => y = groups[i].value.parse().unwrap_or(0.0),
                                42 => bulge = groups[i].value.parse().unwrap_or(0.0),
                                _ => {}
                            }
                            i += 1;
                        }
                        fields.push((10, x.to_string()));
                        fields.push((20, y.to_string()));
                        fields.push((42, bulge.to_string()));
                        continue;
                    }
                    break;
                }
                // Hatch boundary loop vertices live under code-10/code-20 pairs
                // and a code-93 vertex count terminates the loop. We capture
                // them here.
                if is_hatch {
                    match groups[i].code {
                        93 if !hatch_current_loop.is_empty() => {
                            // New loop is starting; flush the previous one.
                            hatch_loops.push(DxfHatchLoop {
                                vertices: std::mem::take(&mut hatch_current_loop),
                            });
                        }
                        93 => {
                            // Empty loop counter, nothing to flush.
                        }
                        10 => {
                            // Pair with the following code-20 to form a vertex.
                            let x: f64 = groups[i].value.parse().unwrap_or(0.0);
                            // Look ahead for the y.
                            let mut y = 0.0;
                            if i + 1 < groups.len() && groups[i + 1].code == 20 {
                                y = groups[i + 1].value.parse().unwrap_or(0.0);
                                i += 1;
                            }
                            hatch_current_loop.push([x, y]);
                        }
                        _ => {}
                    }
                }
                fields.push((groups[i].code, groups[i].value.clone()));
                i += 1;
            }
            if is_hatch && !hatch_current_loop.is_empty() {
                hatch_loops.push(DxfHatchLoop {
                    vertices: hatch_current_loop,
                });
            }
            if let Some(entity) = build_entity(&entity_type, &fields, &hatch_loops) {
                doc.entities.push(entity);
            }
        } else {
            i += 1;
        }
    }
    i
}

fn build_entity(
    kind: &str,
    fields: &[(i32, String)],
    hatch_loops: &[DxfHatchLoop],
) -> Option<DxfEntity> {
    let mut layer = "0".to_string();
    let mut style = "STANDARD".to_string();
    let mut x1 = 0.0;
    let mut y1 = 0.0;
    let mut z1 = 0.0;
    let mut x2 = 0.0;
    let mut y2 = 0.0;
    let mut z2 = 0.0;
    let mut x3 = 0.0;
    let mut y3 = 0.0;
    let mut z3 = 0.0;
    let mut x4 = 0.0;
    let mut y4 = 0.0;
    let mut z4 = 0.0;
    let mut radius = 0.0;
    let mut start_angle = 0.0;
    let mut end_angle = 0.0;
    let mut start_param = 0.0;
    let mut end_param = std::f64::consts::TAU;
    let mut height = 0.0;
    let mut rotation = 0.0;
    let mut text = String::new();
    let mut block_name = String::new();
    let mut sx = 1.0;
    let mut sy = 1.0;
    let mut sz = 1.0;
    let mut flags = 0i32;
    let mut elevation = 0.0;
    let mut polyline_vertices: Vec<DxfPolylineVertex> = Vec::new();
    let mut current_x: Option<f64> = None;
    let mut ratio = 1.0;
    let mut degree: i32 = 3;
    let mut knots: Vec<f64> = Vec::new();
    let mut control_points: Vec<[f64; 3]> = Vec::new();
    let mut spline_x: Option<f64> = None;
    let mut hatch_solid = false;
    let mut hatch_pattern = "SOLID".to_string();
    let mut hatch_scale = 1.0;
    let mut hatch_angle = 0.0;
    let mut dim_kind_code: i32 = 0;
    let mut measured_value: Option<f64> = None;
    let mut override_text: Option<String> = None;

    for (code, val) in fields {
        match code {
            8 => layer.clone_from(val),
            3 => {
                if kind == "DIMENSION" {
                    style.clone_from(val);
                } else if kind == "HATCH" {
                    hatch_pattern.clone_from(val);
                }
            }
            10 => {
                if kind == "LWPOLYLINE" || kind == "POLYLINE" {
                    current_x = Some(val.parse().unwrap_or(0.0));
                } else if kind == "SPLINE" {
                    spline_x = Some(val.parse().unwrap_or(0.0));
                } else {
                    // ELLIPSE and other 10-code-having entities both
                    // record the X of their first defining point here.
                    x1 = val.parse().unwrap_or(0.0);
                }
            }
            20 => {
                if kind == "LWPOLYLINE" || kind == "POLYLINE" {
                    if let Some(x) = current_x.take() {
                        let y = val.parse().unwrap_or(0.0);
                        polyline_vertices.push(DxfPolylineVertex { x, y, bulge: 0.0 });
                    } else {
                        y1 = val.parse().unwrap_or(0.0);
                    }
                } else if kind == "SPLINE" {
                    if let Some(x) = spline_x.take() {
                        let y = val.parse().unwrap_or(0.0);
                        control_points.push([x, y, 0.0]);
                    }
                } else {
                    y1 = val.parse().unwrap_or(0.0);
                }
            }
            30 => {
                if kind == "SPLINE" {
                    if let Some(last) = control_points.last_mut() {
                        last[2] = val.parse().unwrap_or(0.0);
                    }
                } else {
                    z1 = val.parse().unwrap_or(0.0);
                }
            }
            11 => x2 = val.parse().unwrap_or(0.0),
            21 => y2 = val.parse().unwrap_or(0.0),
            31 => z2 = val.parse().unwrap_or(0.0),
            12 => x3 = val.parse().unwrap_or(0.0),
            22 => y3 = val.parse().unwrap_or(0.0),
            32 => z3 = val.parse().unwrap_or(0.0),
            13 => x4 = val.parse().unwrap_or(0.0),
            23 => y4 = val.parse().unwrap_or(0.0),
            33 => z4 = val.parse().unwrap_or(0.0),
            40 => {
                if kind == "TEXT" || kind == "MTEXT" {
                    height = val.parse().unwrap_or(0.0);
                } else if kind == "ELLIPSE" {
                    ratio = val.parse().unwrap_or(1.0);
                } else if kind == "SPLINE" {
                    knots.push(val.parse().unwrap_or(0.0));
                } else if kind == "HATCH" {
                    hatch_scale = val.parse().unwrap_or(1.0);
                } else {
                    radius = val.parse().unwrap_or(0.0);
                }
            }
            41 => {
                if kind == "ELLIPSE" {
                    start_param = val.parse().unwrap_or(0.0);
                } else if kind == "HATCH" {
                    hatch_angle = val.parse().unwrap_or(0.0);
                } else {
                    sx = val.parse().unwrap_or(1.0);
                }
            }
            42 => {
                if kind == "ELLIPSE" {
                    end_param = val.parse().unwrap_or(std::f64::consts::TAU);
                } else if kind == "LWPOLYLINE" || kind == "POLYLINE" {
                    if let Some(last) = polyline_vertices.last_mut() {
                        last.bulge = val.parse().unwrap_or(0.0);
                    }
                } else if kind == "DIMENSION" {
                    measured_value = val.parse().ok();
                } else {
                    sy = val.parse().unwrap_or(1.0);
                }
            }
            43 => sz = val.parse().unwrap_or(1.0),
            50 => {
                if kind == "TEXT" || kind == "MTEXT" || kind == "INSERT" {
                    rotation = val.parse().unwrap_or(0.0);
                } else {
                    start_angle = val.parse().unwrap_or(0.0);
                }
            }
            51 => end_angle = val.parse().unwrap_or(0.0),
            70 => {
                if kind == "DIMENSION" {
                    dim_kind_code = val.parse().unwrap_or(0);
                } else if kind == "HATCH" {
                    hatch_solid = val.parse().unwrap_or(0) != 0;
                } else {
                    flags = val.parse().unwrap_or(0);
                }
            }
            71 if kind == "SPLINE" => {
                degree = val.parse().unwrap_or(3);
            }
            38 => elevation = val.parse().unwrap_or(0.0),
            1 => {
                if kind == "DIMENSION" {
                    if !val.is_empty() {
                        override_text = Some(val.clone());
                    }
                } else {
                    text.clone_from(val);
                }
            }
            2 => {
                if kind == "HATCH" {
                    // DXF spec: code 2 is the hatch pattern name.
                    hatch_pattern.clone_from(val);
                } else {
                    block_name.clone_from(val);
                }
            }
            _ => {}
        }
    }

    match kind {
        "LINE" => Some(DxfEntity::Line(DxfLine {
            layer,
            start: [x1, y1, z1],
            end: [x2, y2, z2],
        })),
        "LWPOLYLINE" | "POLYLINE" => Some(DxfEntity::Polyline(DxfPolyline {
            layer,
            vertices: polyline_vertices,
            closed: (flags & 1) != 0,
            elevation,
        })),
        "ARC" => Some(DxfEntity::Arc(DxfArc {
            layer,
            center: [x1, y1, z1],
            radius,
            start_angle,
            end_angle,
        })),
        "CIRCLE" => Some(DxfEntity::Circle(DxfCircle {
            layer,
            center: [x1, y1, z1],
            radius,
        })),
        "ELLIPSE" => Some(DxfEntity::Ellipse(DxfEllipse {
            layer,
            center: [x1, y1, z1],
            major_axis: [x2, y2, z2],
            ratio,
            start_param,
            end_param,
        })),
        "SPLINE" => Some(DxfEntity::Spline(DxfSpline {
            layer,
            degree,
            knots,
            control_points,
            closed: (flags & 1) != 0,
        })),
        "HATCH" => Some(DxfEntity::Hatch(DxfHatch {
            layer,
            pattern_name: hatch_pattern,
            solid: hatch_solid,
            scale: hatch_scale,
            angle: hatch_angle,
            elevation,
            loops: hatch_loops.to_vec(),
        })),
        "TEXT" | "MTEXT" => Some(DxfEntity::Text(DxfText {
            layer,
            position: [x1, y1, z1],
            height,
            rotation,
            text,
        })),
        "INSERT" => Some(DxfEntity::Insert(DxfInsert {
            layer,
            block_name,
            position: [x1, y1, z1],
            scale: [sx, sy, sz],
            rotation,
        })),
        "DIMENSION" => {
            let dim_kind = match dim_kind_code & 0x07 {
                1 => DxfDimensionKind::Aligned,
                2 => DxfDimensionKind::Angular,
                3 => DxfDimensionKind::Diameter,
                4 => DxfDimensionKind::Radial,
                _ => DxfDimensionKind::Linear,
            };
            Some(DxfEntity::Dimension(DxfDimension {
                layer,
                style,
                kind: dim_kind,
                def_point: [x1, y1, z1],
                text_position: [x2, y2, z2],
                def_point_a: [x3, y3, z3],
                def_point_b: [x4, y4, z4],
                override_text,
                measured_value,
            }))
        }
        _ => None,
    }
}
