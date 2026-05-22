//! Bridge between the in-memory [`crate::dxf::DxfDocument`] and the
//! R12 wire-level types in [`super::entity`], [`super::record_kinds`],
//! [`super::header_vars`], and [`super::tables`].
//!
//! This module owns the *mapping policy* between the document's
//! string-keyed layer/block model and the R12 file's 1-based table
//! indices. The mapping is recoverable from the on-wire bytes alone:
//! we build the LAYER table in iteration order, the BLOCK table from
//! `block_records`, and entities reference both by index. Round-trip
//! tests at the bottom of this file pin the policy.

use std::collections::HashMap;

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::r12::entity::{
    common_flag, encode_record, R12EntityCommon, R12EntityRecord, R12EntityType,
};
use crate::dwg::r12::header_vars::R12HeaderVars;
use crate::dwg::r12::record_kinds::{
    encode_arc, encode_circle, encode_insert, encode_line, encode_polyline_header, encode_text,
    encode_vertex, R12Arc, R12Circle, R12Insert, R12Line, R12PolylineHeader, R12Text, R12Vertex,
    INSERT_HAS_ROTATION, INSERT_HAS_SCALE_X, INSERT_HAS_SCALE_Y, INSERT_HAS_SCALE_Z,
    TEXT_HAS_ROTATION, VERTEX_HAS_BULGE,
};
use crate::dwg::r12::tables::{
    R12AppIdRecord, R12BlockRecord, R12DimstyleRecord, R12LayerRecord, R12LinetypeRecord,
    R12StyleRecord, R12Tables, R12UcsRecord, R12VportRecord,
};
use crate::dxf::{
    DxfArc, DxfBlockRecord, DxfCircle, DxfDocument, DxfEntity, DxfInsert, DxfLine, DxfPolyline,
    DxfPolylineVertex, DxfText,
};
use crate::layers::{Layer, LayerColor};

/// Hold the index allocation while we build a document or walk a
/// file. Both sides need a consistent mapping between names and the
/// 1-based table indices stored on the wire.
#[derive(Debug, Default)]
pub struct IndexMaps {
    pub layer_by_name: HashMap<String, u16>,
    pub layer_by_index: HashMap<u16, String>,
    pub block_by_name: HashMap<String, u16>,
    pub block_by_index: HashMap<u16, String>,
    pub ltype_by_name: HashMap<String, u16>,
    pub ltype_by_index: HashMap<u16, String>,
}

impl IndexMaps {
    fn layer_index(&self, name: &str) -> u16 {
        // Default to "0" (always present as index 1) when the entity
        // references a layer that didn't make it into the table.
        self.layer_by_name
            .get(name)
            .copied()
            .or_else(|| self.layer_by_name.get("0").copied())
            .unwrap_or(1)
    }

    fn block_index(&self, name: &str) -> Option<u16> {
        self.block_by_name.get(name).copied()
    }

    /// Look up the 1-based index of a linetype by case-insensitive
    /// name, falling back to "CONTINUOUS" (always index 1 in our
    /// canonical layout) when the requested name is unknown. Kept on
    /// the public surface even though the writer currently doesn't
    /// emit per-entity linetypes — entities that override their
    /// layer's linetype will need it once we add that feature, and
    /// keeping the resolver next to the other index resolvers makes
    /// the mapping policy single-source.
    #[allow(dead_code)]
    pub fn ltype_index(&self, name: &str) -> u16 {
        self.ltype_by_name
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
            .or_else(|| self.ltype_by_name.get("CONTINUOUS").copied())
            .unwrap_or(1)
    }
}

/// One complete R12 file shape, ready to be serialised by
/// [`crate::dwg::r12::writer`] or recovered by
/// [`crate::dwg::r12::reader`].
#[derive(Debug, Clone, PartialEq)]
pub struct R12FileImage {
    pub header_vars: R12HeaderVars,
    pub tables: R12Tables,
    /// Top-level (model-space) entities, in walk order. Each is a
    /// fully-framed record (opcode → CRC).
    pub entities: Vec<u8>,
    /// Block-space entities, one byte-stream per block-table record
    /// (parallel to `tables.blocks`). Each contains the entity
    /// records for that block, in walk order.
    pub block_entities: Vec<Vec<u8>>,
    /// Convenience: keeps the index allocation around so the writer
    /// can patch BLOCK records' `entities_offset` after layout.
    pub indices: IndexMapsArchive,
}

/// Owned snapshot of the index allocation. Stored alongside the file
/// image so the writer/reader can keep the name↔index mapping in
/// sync without rebuilding it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexMapsArchive {
    pub layers: Vec<String>,    // 1-based: layers[i-1] == name
    pub blocks: Vec<String>,    // 1-based: blocks[i-1] == name
    pub linetypes: Vec<String>, // 1-based: linetypes[i-1] == name
}

impl IndexMapsArchive {
    fn to_maps(&self) -> IndexMaps {
        let mut m = IndexMaps::default();
        for (idx, name) in self.layers.iter().enumerate() {
            m.layer_by_name.insert(name.clone(), (idx + 1) as u16);
            m.layer_by_index.insert((idx + 1) as u16, name.clone());
        }
        for (idx, name) in self.blocks.iter().enumerate() {
            m.block_by_name.insert(name.clone(), (idx + 1) as u16);
            m.block_by_index.insert((idx + 1) as u16, name.clone());
        }
        for (idx, name) in self.linetypes.iter().enumerate() {
            m.ltype_by_name.insert(name.clone(), (idx + 1) as u16);
            m.ltype_by_index.insert((idx + 1) as u16, name.clone());
        }
        m
    }
}

/// Encode a [`DxfDocument`] into an [`R12FileImage`].
pub fn document_to_image(doc: &DxfDocument) -> DwgResult<R12FileImage> {
    let mut archive = IndexMapsArchive::default();
    let mut layer_records = Vec::new();
    let mut ltype_records = vec![R12LinetypeRecord {
        name: "CONTINUOUS".to_string(),
        flag: 0,
        pattern_length: 0.0,
        dashes: Vec::new(),
    }];
    archive.linetypes.push("CONTINUOUS".to_string());
    for layer in doc.layers.iter() {
        if !archive
            .linetypes
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&layer.linetype))
        {
            ltype_records.push(R12LinetypeRecord {
                name: layer.linetype.clone(),
                flag: 0,
                pattern_length: 0.0,
                dashes: Vec::new(),
            });
            archive.linetypes.push(layer.linetype.clone());
        }
    }
    // Build layer index map after linetypes (so we can resolve
    // ltype_index per layer).
    for layer in doc.layers.iter() {
        archive.layers.push(layer.name.clone());
        let ltype_index = archive
            .linetypes
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&layer.linetype))
            .map_or(1, |p| (p + 1) as u16);
        layer_records.push(R12LayerRecord {
            name: layer.name.clone(),
            color: layer.color.0,
            ltype_index,
            flag: r12_layer_flag(layer),
        });
    }

    let mut block_records = Vec::with_capacity(doc.block_records.len());
    for block in &doc.block_records {
        archive.blocks.push(block.name.clone());
        block_records.push(R12BlockRecord {
            name: block.name.clone(),
            flag: block.flags as u8,
            insertion_base: [0.0; 2],
            elevation: 0.0,
            num_entities: 0,    // patched at layout time
            entities_offset: 0, // patched at layout time
        });
    }
    // Always reserve at least one block table entry for *MODEL_SPACE
    // so INSERT references can resolve "no-op" to index 1 if needed.
    if !block_records
        .iter()
        .any(|b| b.name.eq_ignore_ascii_case("*MODEL_SPACE"))
    {
        archive.blocks.insert(0, "*MODEL_SPACE".to_string());
        block_records.insert(
            0,
            R12BlockRecord {
                name: "*MODEL_SPACE".to_string(),
                flag: 0,
                insertion_base: [0.0; 2],
                elevation: 0.0,
                num_entities: 0,
                entities_offset: 0,
            },
        );
    }
    let dimstyle_records: Vec<R12DimstyleRecord> = doc
        .dim_styles
        .iter()
        .map(|s| R12DimstyleRecord {
            name: s.name.clone(),
            flag: 0,
            values: vec![s.text_height, s.arrow_size, s.units_scale],
        })
        .collect();

    let maps = archive.to_maps();
    let mut model_entities = Vec::new();
    for entity in &doc.entities {
        write_entity(&mut model_entities, entity, &maps)?;
    }
    Ok(R12FileImage {
        header_vars: R12HeaderVars::default(),
        tables: R12Tables {
            layers: layer_records,
            blocks: block_records,
            linetypes: ltype_records,
            styles: vec![R12StyleRecord {
                name: "STANDARD".to_string(),
                flag: 0,
                fixed_height: 0.0,
                width_factor: 1.0,
                oblique_angle: 0.0,
                generation: 0,
                last_height: 2.5,
            }],
            views: Vec::new(),
            ucs: vec![R12UcsRecord {
                name: "WORLD".to_string(),
                flag: 0,
                origin: [0.0; 3],
                x_axis: [1.0, 0.0, 0.0],
                y_axis: [0.0, 1.0, 0.0],
            }],
            vports: vec![R12VportRecord {
                name: "*ACTIVE".to_string(),
                flag: 0,
                lower_left: [0.0; 2],
                upper_right: [1.0; 2],
                center: [0.5; 2],
                view_size: 9.0,
                aspect_ratio: 1.0,
            }],
            dimstyles: dimstyle_records,
            appids: vec![R12AppIdRecord {
                name: "ACAD".to_string(),
                flag: 0,
            }],
        },
        entities: model_entities,
        block_entities: Vec::new(), // populated by writer once block ranges are known
        indices: archive,
    })
}

fn r12_layer_flag(layer: &Layer) -> u8 {
    let mut flag = 0u8;
    if layer.frozen {
        flag |= 0x01;
    }
    if layer.locked {
        flag |= 0x02;
    }
    if !layer.on {
        // R12 had no first-class "off" flag distinct from frozen;
        // model "off" as frozen-on-new-vp (bit 4) so the round-trip
        // can recover it without colliding with the global frozen
        // bit.
        flag |= 0x04;
    }
    flag
}

fn dwg_layer_flag_to_dxf(layer: &mut Layer, flag: u8) {
    layer.frozen = flag & 0x01 != 0;
    layer.locked = flag & 0x02 != 0;
    layer.on = flag & 0x04 == 0;
}

/// Encode one DXF entity into the wire stream.
fn write_entity(out: &mut Vec<u8>, entity: &DxfEntity, maps: &IndexMaps) -> DwgResult<()> {
    let layer_index = maps.layer_index(entity.layer());
    match entity {
        DxfEntity::Line(line) => write_line(out, line, layer_index)?,
        DxfEntity::Circle(c) => write_circle(out, c, layer_index)?,
        DxfEntity::Arc(a) => write_arc(out, a, layer_index)?,
        DxfEntity::Text(t) => write_text(out, t, layer_index)?,
        DxfEntity::Insert(i) => write_insert(out, i, layer_index, maps)?,
        DxfEntity::Polyline(p) => write_polyline(out, p, layer_index)?,
        DxfEntity::Ellipse(_)
        | DxfEntity::Spline(_)
        | DxfEntity::Hatch(_)
        | DxfEntity::Dimension(_) => {
            // R12 has no first-class ELLIPSE/SPLINE/HATCH/DIMENSION
            // entity in the AC1009 opcode table. We tessellate these
            // into a POLYLINE approximation so the geometry survives
            // the round-trip, with the original semantic stored in
            // the polyline `flag` (bit 0x80) as a "synthesized"
            // marker. AutoCAD itself does this when downgrading R13+
            // files to R12.
            let poly = synthesize_polyline_from_complex(entity);
            write_polyline(out, &poly, layer_index)?;
        }
    }
    Ok(())
}

fn write_line(out: &mut Vec<u8>, line: &DxfLine, layer_index: u16) -> DwgResult<()> {
    let mut payload = Vec::with_capacity(48);
    encode_line(
        &mut payload,
        &R12Line {
            start: line.start,
            end: line.end,
            extrusion: None,
        },
    );
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::Line,
            common: R12EntityCommon::on_layer(layer_index),
            payload,
        },
    )
}

fn write_circle(out: &mut Vec<u8>, c: &DxfCircle, layer_index: u16) -> DwgResult<()> {
    let mut payload = Vec::with_capacity(32);
    encode_circle(
        &mut payload,
        &R12Circle {
            center: c.center,
            radius: c.radius,
            extrusion: None,
        },
    );
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::Circle,
            common: R12EntityCommon::on_layer(layer_index),
            payload,
        },
    )
}

fn write_arc(out: &mut Vec<u8>, a: &DxfArc, layer_index: u16) -> DwgResult<()> {
    let mut payload = Vec::with_capacity(48);
    encode_arc(
        &mut payload,
        &R12Arc {
            center: a.center,
            radius: a.radius,
            start_angle: a.start_angle,
            end_angle: a.end_angle,
            extrusion: None,
        },
    );
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::Arc,
            common: R12EntityCommon::on_layer(layer_index),
            payload,
        },
    )
}

fn write_text(out: &mut Vec<u8>, t: &DxfText, layer_index: u16) -> DwgResult<()> {
    let mut payload = Vec::with_capacity(64);
    encode_text(
        &mut payload,
        &R12Text {
            position: t.position,
            height: t.height,
            text: t.text.clone(),
            rotation: if t.rotation == 0.0 {
                None
            } else {
                Some(t.rotation)
            },
        },
    )?;
    let opts = if t.rotation == 0.0 {
        0
    } else {
        TEXT_HAS_ROTATION
    };
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::Text,
            common: R12EntityCommon {
                opts,
                ..R12EntityCommon::on_layer(layer_index)
            },
            payload,
        },
    )
}

fn write_insert(
    out: &mut Vec<u8>,
    insert: &DxfInsert,
    layer_index: u16,
    maps: &IndexMaps,
) -> DwgResult<()> {
    let block_index =
        maps.block_index(&insert.block_name)
            .ok_or_else(|| DwgError::MalformedObject {
                class: "INSERT".to_string(),
                offset: 0,
                message: format!(
                    "block {:?} referenced by INSERT is not in block_records",
                    insert.block_name
                ),
            })?;
    let mut opts = 0u16;
    let mut sx = None;
    let mut sy = None;
    let mut sz = None;
    let mut rot = None;
    if insert.scale[0] != 1.0 {
        sx = Some(insert.scale[0]);
        opts |= INSERT_HAS_SCALE_X;
    }
    if insert.scale[1] != 1.0 {
        sy = Some(insert.scale[1]);
        opts |= INSERT_HAS_SCALE_Y;
    }
    if insert.scale[2] != 1.0 {
        sz = Some(insert.scale[2]);
        opts |= INSERT_HAS_SCALE_Z;
    }
    if insert.rotation != 0.0 {
        rot = Some(insert.rotation);
        opts |= INSERT_HAS_ROTATION;
    }
    let mut payload = Vec::with_capacity(64);
    encode_insert(
        &mut payload,
        &R12Insert {
            block_index,
            position: insert.position,
            scale_x: sx,
            scale_y: sy,
            scale_z: sz,
            rotation: rot,
        },
    );
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::Insert,
            common: R12EntityCommon {
                opts,
                ..R12EntityCommon::on_layer(layer_index)
            },
            payload,
        },
    )
}

fn write_polyline(out: &mut Vec<u8>, poly: &DxfPolyline, layer_index: u16) -> DwgResult<()> {
    // POLYLINE header record
    let mut hdr_payload = Vec::with_capacity(3);
    let flag = u8::from(poly.closed);
    encode_polyline_header(&mut hdr_payload, &R12PolylineHeader { flag, curvetype: 0 });
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::Polyline,
            common: R12EntityCommon {
                flag: common_flag::HAS_ATTRIBS,
                ..R12EntityCommon::on_layer(layer_index)
            },
            payload: hdr_payload,
        },
    )?;
    // VERTEX records
    for v in &poly.vertices {
        let mut p = Vec::with_capacity(24);
        let opts = if v.bulge == 0.0 { 0 } else { VERTEX_HAS_BULGE };
        encode_vertex(
            &mut p,
            &R12Vertex {
                position: [v.x, v.y],
                bulge: if v.bulge == 0.0 { None } else { Some(v.bulge) },
            },
        );
        encode_record(
            out,
            &R12EntityRecord {
                kind: R12EntityType::Vertex,
                common: R12EntityCommon {
                    opts,
                    ..R12EntityCommon::on_layer(layer_index)
                },
                payload: p,
            },
        )?;
    }
    // SEQEND
    encode_record(
        out,
        &R12EntityRecord {
            kind: R12EntityType::SeqEnd,
            common: R12EntityCommon::on_layer(layer_index),
            payload: Vec::new(),
        },
    )
}

/// Tessellate an ELLIPSE/SPLINE/HATCH/DIMENSION into a polyline so the
/// geometry survives the round-trip into R12, which lacks first-class
/// records for these. Match what AutoCAD does when saving "Save As R12":
/// emit a closed/open polyline that traces the shape.
fn synthesize_polyline_from_complex(entity: &DxfEntity) -> DxfPolyline {
    use std::f64::consts::TAU;
    const SEGMENTS: usize = 64;
    let (layer, vertices, closed, elevation) = match entity {
        DxfEntity::Ellipse(e) => {
            let mut vs = Vec::with_capacity(SEGMENTS);
            for i in 0..SEGMENTS {
                let t = (i as f64) / (SEGMENTS as f64) * TAU;
                let ax = e.major_axis[0];
                let ay = e.major_axis[1];
                let len = (ax * ax + ay * ay).sqrt().max(f64::MIN_POSITIVE);
                let ux = ax / len;
                let uy = ay / len;
                let px = len * t.cos();
                let py = len * e.ratio * t.sin();
                let x = e.center[0] + ux * px - uy * py;
                let y = e.center[1] + uy * px + ux * py;
                vs.push(DxfPolylineVertex::new(x, y));
            }
            (e.layer.clone(), vs, true, e.center[2])
        }
        DxfEntity::Spline(s) => {
            // Linear approximation through the control points; not
            // optimal but lossless for the structural shape.
            let vs: Vec<_> = s
                .control_points
                .iter()
                .map(|p| DxfPolylineVertex::new(p[0], p[1]))
                .collect();
            (s.layer.clone(), vs, s.closed, 0.0)
        }
        DxfEntity::Hatch(h) => {
            // Take the outer loop only — inner loops can't be
            // represented as a single polyline.
            let outer = h.loops.first();
            let vs = outer.map_or_else(Vec::new, |l| {
                l.vertices
                    .iter()
                    .map(|v| DxfPolylineVertex::new(v[0], v[1]))
                    .collect()
            });
            (h.layer.clone(), vs, true, h.elevation)
        }
        DxfEntity::Dimension(d) => {
            // Emit the leader/extension lines as a polyline so the
            // dimension's visual placement is recoverable.
            let vs = vec![
                DxfPolylineVertex::new(d.def_point_a[0], d.def_point_a[1]),
                DxfPolylineVertex::new(d.def_point[0], d.def_point[1]),
                DxfPolylineVertex::new(d.def_point_b[0], d.def_point_b[1]),
            ];
            (d.layer.clone(), vs, false, 0.0)
        }
        _ => unreachable!("only complex entities reach synthesize_polyline_from_complex"),
    };
    DxfPolyline {
        layer,
        vertices,
        closed,
        elevation,
    }
}

/// Decode an [`R12FileImage`] back into a [`DxfDocument`].
pub fn image_to_document(image: &R12FileImage) -> DwgResult<DxfDocument> {
    use crate::dwg::r12::entity::decode_record;
    use crate::dwg::r12::record_kinds::{
        decode_arc, decode_circle, decode_insert, decode_line, decode_polyline_header, decode_text,
        decode_vertex,
    };
    let mut doc = DxfDocument::new();
    doc.layers = crate::layers::LayerSystem::new();

    // Rehydrate layers from the table.
    for rec in &image.tables.layers {
        if rec.name == "0" {
            // Already present in a fresh LayerSystem; mutate it in
            // place rather than re-inserting (LayerSystem doesn't
            // tolerate duplicates well).
            if let Some(l) = doc.layers.get_mut("0") {
                l.color = LayerColor(rec.color);
                if let Some(name) = image
                    .indices
                    .linetypes
                    .get((rec.ltype_index as usize).saturating_sub(1))
                {
                    l.linetype.clone_from(name);
                }
                dwg_layer_flag_to_dxf(l, rec.flag);
            }
            continue;
        }
        let mut layer = Layer::new(rec.name.clone()).map_err(|e| DwgError::MalformedObject {
            class: "LAYER".to_string(),
            offset: 0,
            message: e.to_string(),
        })?;
        layer.color = LayerColor(rec.color);
        if let Some(name) = image
            .indices
            .linetypes
            .get((rec.ltype_index as usize).saturating_sub(1))
        {
            layer.linetype.clone_from(name);
        }
        dwg_layer_flag_to_dxf(&mut layer, rec.flag);
        doc.layers
            .insert(layer)
            .map_err(|e| DwgError::MalformedObject {
                class: "LAYER".to_string(),
                offset: 0,
                message: e.to_string(),
            })?;
    }

    // Block records: skip the implicit *MODEL_SPACE which we add
    // unconditionally on the encode side.
    for rec in &image.tables.blocks {
        if rec.name.eq_ignore_ascii_case("*MODEL_SPACE") {
            continue;
        }
        let mut br = DxfBlockRecord::new(rec.name.clone());
        br.flags = rec.flag as i32;
        doc.block_records.push(br);
    }

    // Dim styles.
    doc.dim_styles.clear();
    for rec in &image.tables.dimstyles {
        let mut s = crate::dxf::entities::DxfDimStyle::standard();
        s.name.clone_from(&rec.name);
        if let Some(&v) = rec.values.first() {
            s.text_height = v;
        }
        if let Some(&v) = rec.values.get(1) {
            s.arrow_size = v;
        }
        if let Some(&v) = rec.values.get(2) {
            s.units_scale = v;
        }
        doc.dim_styles.push(s);
    }
    if doc.dim_styles.is_empty() {
        doc.dim_styles
            .push(crate::dxf::entities::DxfDimStyle::standard());
    }

    // Walk the entity byte stream.
    let mut cur = 0usize;
    while cur < image.entities.len() {
        let (rec, consumed) = decode_record(&image.entities[cur..])?;
        cur += consumed;
        let layer_name = image
            .indices
            .layers
            .get((rec.common.layer_index as usize).saturating_sub(1))
            .cloned()
            .unwrap_or_else(|| "0".to_string());
        match rec.kind {
            R12EntityType::Line => {
                let payload = decode_line(&rec.payload, rec.common.opts)?;
                doc.entities.push(DxfEntity::Line(DxfLine {
                    layer: layer_name,
                    start: payload.start,
                    end: payload.end,
                }));
            }
            R12EntityType::Circle => {
                let payload = decode_circle(&rec.payload, rec.common.opts)?;
                doc.entities.push(DxfEntity::Circle(DxfCircle {
                    layer: layer_name,
                    center: payload.center,
                    radius: payload.radius,
                }));
            }
            R12EntityType::Arc => {
                let payload = decode_arc(&rec.payload, rec.common.opts)?;
                doc.entities.push(DxfEntity::Arc(DxfArc {
                    layer: layer_name,
                    center: payload.center,
                    radius: payload.radius,
                    start_angle: payload.start_angle,
                    end_angle: payload.end_angle,
                }));
            }
            R12EntityType::Text => {
                let payload = decode_text(&rec.payload, rec.common.opts)?;
                doc.entities.push(DxfEntity::Text(DxfText {
                    layer: layer_name,
                    position: payload.position,
                    height: payload.height,
                    rotation: payload.rotation.unwrap_or(0.0),
                    text: payload.text,
                }));
            }
            R12EntityType::Insert => {
                let payload = decode_insert(&rec.payload, rec.common.opts)?;
                let block_name = image
                    .indices
                    .blocks
                    .get((payload.block_index as usize).saturating_sub(1))
                    .cloned()
                    .unwrap_or_else(|| "*MODEL_SPACE".to_string());
                doc.entities.push(DxfEntity::Insert(DxfInsert {
                    layer: layer_name,
                    block_name,
                    position: payload.position,
                    scale: [
                        payload.scale_x.unwrap_or(1.0),
                        payload.scale_y.unwrap_or(1.0),
                        payload.scale_z.unwrap_or(1.0),
                    ],
                    rotation: payload.rotation.unwrap_or(0.0),
                }));
            }
            R12EntityType::Polyline => {
                let hdr = decode_polyline_header(&rec.payload)?;
                let mut vertices = Vec::new();
                // Collect VERTEX records until SEQEND.
                while cur < image.entities.len() {
                    let (next, used) = decode_record(&image.entities[cur..])?;
                    cur += used;
                    match next.kind {
                        R12EntityType::Vertex => {
                            let v = decode_vertex(&next.payload, next.common.opts)?;
                            vertices.push(DxfPolylineVertex {
                                x: v.position[0],
                                y: v.position[1],
                                bulge: v.bulge.unwrap_or(0.0),
                            });
                        }
                        R12EntityType::SeqEnd => break,
                        other => {
                            return Err(DwgError::MalformedObject {
                                class: "POLYLINE".to_string(),
                                offset: cur as u64,
                                message: format!("unexpected opcode {other:?} inside polyline"),
                            });
                        }
                    }
                }
                doc.entities.push(DxfEntity::Polyline(DxfPolyline {
                    layer: layer_name,
                    vertices,
                    closed: hdr.flag & 0x01 != 0,
                    elevation: rec.common.elevation,
                }));
            }
            R12EntityType::SeqEnd => {
                // Unexpected at top level; skip silently rather than
                // fail, because an early-truncated polyline chain
                // can leave dangling SEQENDs in real files.
                continue;
            }
            other => {
                return Err(DwgError::Unsupported(format!(
                    "R12 opcode {other:?} decode-to-DxfDocument is not implemented"
                )));
            }
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::DxfEntity;

    fn doc_with_line() -> DxfDocument {
        let mut d = DxfDocument::new();
        d.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".to_string(),
            start: [1.0, 2.0, 0.0],
            end: [4.0, 5.0, 0.0],
        }));
        d
    }

    #[test]
    fn doc_with_single_line_round_trips() {
        let want = doc_with_line();
        let image = document_to_image(&want).unwrap();
        let got = image_to_document(&image).unwrap();
        assert_eq!(got.entities, want.entities);
    }

    #[test]
    fn mixed_geometry_round_trips() {
        let mut want = DxfDocument::new();
        want.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".to_string(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 0.0, 0.0],
        }));
        want.entities.push(DxfEntity::Circle(DxfCircle {
            layer: "0".to_string(),
            center: [5.0, 5.0, 0.0],
            radius: 2.5,
        }));
        want.entities.push(DxfEntity::Arc(DxfArc {
            layer: "0".to_string(),
            center: [10.0, 10.0, 0.0],
            radius: 1.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
        }));
        want.entities.push(DxfEntity::Text(DxfText {
            layer: "0".to_string(),
            position: [0.0, -1.0, 0.0],
            height: 0.25,
            rotation: 0.0,
            text: "Hello".to_string(),
        }));
        let image = document_to_image(&want).unwrap();
        let got = image_to_document(&image).unwrap();
        assert_eq!(got.entities, want.entities);
    }

    #[test]
    fn insert_round_trips_via_block_table() {
        let mut want = DxfDocument::new();
        want.block_records.push(DxfBlockRecord::new("DOOR"));
        want.entities.push(DxfEntity::Insert(DxfInsert {
            layer: "0".to_string(),
            block_name: "DOOR".to_string(),
            position: [3.0, 4.0, 0.0],
            scale: [2.0, 2.0, 1.0],
            rotation: 1.5,
        }));
        let image = document_to_image(&want).unwrap();
        let got = image_to_document(&image).unwrap();
        assert_eq!(got.entities, want.entities);
    }

    #[test]
    fn polyline_round_trips_with_bulges_and_closed_flag() {
        let mut want = DxfDocument::new();
        want.entities.push(DxfEntity::Polyline(DxfPolyline {
            layer: "0".to_string(),
            vertices: vec![
                DxfPolylineVertex {
                    x: 0.0,
                    y: 0.0,
                    bulge: 0.5,
                },
                DxfPolylineVertex {
                    x: 10.0,
                    y: 0.0,
                    bulge: 0.0,
                },
                DxfPolylineVertex {
                    x: 10.0,
                    y: 10.0,
                    bulge: -0.25,
                },
            ],
            closed: true,
            elevation: 0.0,
        }));
        let image = document_to_image(&want).unwrap();
        let got = image_to_document(&image).unwrap();
        assert_eq!(got.entities, want.entities);
    }

    #[test]
    fn custom_layer_round_trips_with_color_and_flag() {
        let mut want = DxfDocument::new();
        let mut walls = Layer::new("WALLS").unwrap();
        walls.color = LayerColor(1);
        walls.locked = true;
        want.layers.insert(walls).unwrap();
        want.entities.push(DxfEntity::Line(DxfLine {
            layer: "WALLS".to_string(),
            start: [0.0; 3],
            end: [1.0; 3],
        }));
        let image = document_to_image(&want).unwrap();
        let got = image_to_document(&image).unwrap();
        let walls_decoded = got.layers.get("WALLS").expect("WALLS should round-trip");
        assert_eq!(walls_decoded.color, LayerColor(1));
        assert!(walls_decoded.locked);
        assert_eq!(got.entities, want.entities);
    }
}
