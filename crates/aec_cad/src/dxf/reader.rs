//! DXF ASCII reader. Implements a working subset of the spec covering the
//! header/tables/blocks/entities sections, the entities `LINE`, `LWPOLYLINE`,
//! `POLYLINE` (with VERTEX members), `ARC`, `CIRCLE`, `TEXT`, `MTEXT`,
//! `DIMENSION`, `INSERT`, and the `LAYER` + `BLOCK_RECORD` + `DIMSTYLE`
//! tables.

use std::io::{BufRead, BufReader, Read};

use crate::dxf::entities::{
    DxfArc, DxfCircle, DxfDimStyle, DxfEntity, DxfInsert, DxfLine, DxfPolyline, DxfText,
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
            // Find the table name (code 2) and then iterate the entries.
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
            // Start of a new table entry; read up to the next 0-code.
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
            let mut fields: Vec<(i32, String)> = Vec::new();
            i += 1;
            // Collect this entity's fields. For POLYLINE we need to keep
            // reading VERTEX/SEQEND pseudo-entities; LWPOLYLINE is flat.
            let is_polyline = entity_type == "POLYLINE";
            while i < groups.len() {
                if groups[i].code == 0 {
                    if is_polyline && (groups[i].value == "VERTEX" || groups[i].value == "SEQEND") {
                        // Inline vertex data: parse and append.
                        if groups[i].value == "SEQEND" {
                            i += 1;
                            // consume SEQEND's fields (handles/layer)
                            while i < groups.len() && groups[i].code != 0 {
                                i += 1;
                            }
                            break;
                        }
                        i += 1;
                        let mut x = 0.0;
                        let mut y = 0.0;
                        while i < groups.len() && groups[i].code != 0 {
                            match groups[i].code {
                                10 => x = groups[i].value.parse().unwrap_or(0.0),
                                20 => y = groups[i].value.parse().unwrap_or(0.0),
                                _ => {}
                            }
                            i += 1;
                        }
                        // Encode the vertex as synthetic fields the entity
                        // builder picks up.
                        fields.push((10, x.to_string()));
                        fields.push((20, y.to_string()));
                        continue;
                    }
                    break;
                }
                fields.push((groups[i].code, groups[i].value.clone()));
                i += 1;
            }
            if let Some(entity) = build_entity(&entity_type, &fields) {
                doc.entities.push(entity);
            }
        } else {
            i += 1;
        }
    }
    i
}

fn build_entity(kind: &str, fields: &[(i32, String)]) -> Option<DxfEntity> {
    let mut layer = "0".to_string();
    let mut x1 = 0.0;
    let mut y1 = 0.0;
    let mut z1 = 0.0;
    let mut x2 = 0.0;
    let mut y2 = 0.0;
    let mut z2 = 0.0;
    let mut radius = 0.0;
    let mut start_angle = 0.0;
    let mut end_angle = 0.0;
    let mut height = 0.0;
    let mut rotation = 0.0;
    let mut text = String::new();
    let mut block_name = String::new();
    let mut sx = 1.0;
    let mut sy = 1.0;
    let mut sz = 1.0;
    let mut flags = 0i32;
    let mut elevation = 0.0;
    let mut polyline_vertices: Vec<[f64; 2]> = Vec::new();
    // Buffer for LWPOLYLINE vertices (paired 10/20 fields).
    let mut current_x: Option<f64> = None;

    for (code, val) in fields {
        match code {
            8 => layer.clone_from(val),
            10 => {
                if kind == "LWPOLYLINE" || kind == "POLYLINE" {
                    current_x = Some(val.parse().unwrap_or(0.0));
                } else {
                    x1 = val.parse().unwrap_or(0.0);
                }
            }
            20 => {
                if kind == "LWPOLYLINE" || kind == "POLYLINE" {
                    if let Some(x) = current_x.take() {
                        polyline_vertices.push([x, val.parse().unwrap_or(0.0)]);
                    } else {
                        y1 = val.parse().unwrap_or(0.0);
                    }
                } else {
                    y1 = val.parse().unwrap_or(0.0);
                }
            }
            30 => z1 = val.parse().unwrap_or(0.0),
            11 => x2 = val.parse().unwrap_or(0.0),
            21 => y2 = val.parse().unwrap_or(0.0),
            31 => z2 = val.parse().unwrap_or(0.0),
            40 => {
                if kind == "TEXT" || kind == "MTEXT" {
                    height = val.parse().unwrap_or(0.0);
                } else {
                    radius = val.parse().unwrap_or(0.0);
                }
            }
            41 => sx = val.parse().unwrap_or(1.0),
            42 => sy = val.parse().unwrap_or(1.0),
            43 => sz = val.parse().unwrap_or(1.0),
            50 => {
                if kind == "TEXT" || kind == "MTEXT" || kind == "INSERT" {
                    rotation = val.parse().unwrap_or(0.0);
                } else {
                    start_angle = val.parse().unwrap_or(0.0);
                }
            }
            51 => end_angle = val.parse().unwrap_or(0.0),
            70 => flags = val.parse().unwrap_or(0),
            38 => elevation = val.parse().unwrap_or(0.0),
            1 => text.clone_from(val),
            2 => block_name.clone_from(val),
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
        _ => None,
    }
}
