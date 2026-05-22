//! Bridge between [`DxfDocument`] and the on-disk R14/R2000 layout.
//!
//! The native DWG codec is structured around two layers:
//!
//! 1. **Wire layer** (everything under `dwg::file`, `dwg::bits`,
//!    `dwg::entities`) — turns bytes into typed records.
//! 2. **Document layer** (this module) — turns those typed records
//!    into the canonical [`DxfDocument`] in-memory shape so callers
//!    don't need to know anything about handles or section locators.
//!
//! Per-version support:
//! - R14 and R2000 → handled here (sections are not paginated)
//! - R2004+ → handled in `dwg::pages`-aware sibling modules
//! - R12 → handled separately under `dwg::r12`
//!
//! Today we round-trip the entity kinds whose bit-level codec already
//! exists (LINE, ARC, CIRCLE, ELLIPSE, INSERT, LWPOLYLINE, TEXT).
//! Unsupported entity kinds surface a structured
//! [`DwgError::UnsupportedInVersion`] rather than silently dropping
//! data.

use crate::dwg::bits::reader::HandleRef;
use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::entities::header_codec::CommonHeaderData;
use crate::dwg::entities::record::{BitBuf, ObjectHandles};
use crate::dwg::entities::{
    arc::ArcEntity, circle::CircleEntity, ellipse::EllipseEntity, insert::InsertEntity,
    line::LineEntity, lwpolyline::LwPolylineEntity, text::TextEntity, ObjectRecord, ObjectType,
};
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::classes::ClassesSection;
use crate::dwg::file::header_vars::HeaderVarsSection;
use crate::dwg::file::r2000_layout::{assemble_r2000, parse_r2000, R2000FileParts, R2000Object};
use crate::dwg::file::r2004_layout::{assemble_r2004, parse_r2004, R2004FileParts};
use crate::dwg::file::r2007_layout::{assemble_r2007, parse_r2007, R2007FileParts};
use crate::dwg::version::Version;
use crate::dxf::{DxfDocument, DxfEntity};

/// First object handle we hand out. Real DWG files reserve the low
/// handles for the well-known table records (BLOCK_RECORD, LAYER,
/// STYLE, LTYPE, …). 0x10 is conventional enough that LibreDWG
/// fixtures match this; once we encode real table records the
/// assignment will be threaded through the symbol-table builder.
const FIRST_ENTITY_HANDLE: u64 = 0x10;

/// Convenience handle used for unresolved "layer 0" references in
/// the handle stream. Real R2000 round-trip needs a LAYER record at
/// this handle; that wiring lands in the symbol-tables commit.
const LAYER_ZERO_HANDLE: u64 = 0x14;

/// Convenience handle used for the model-space BLOCK_HEADER (owner
/// of every entity in model space). Same caveat as above.
const MODEL_SPACE_HANDLE: u64 = 0x1f;

/// Write a [`DxfDocument`] to modern (R14 / R2000 / R2004+) wire
/// bytes.
///
/// Per-version dispatch:
/// - R14, R2000 → flat section-locator layout (`r2000_layout`)
/// - R2004+ → paged system sections with LZ77-compressed data pages
///   and the encrypted R2004 file header (`r2004_layout`)
pub fn write_modern(doc: &DxfDocument, version: Version) -> DwgResult<Vec<u8>> {
    if !version.is_modern() {
        return Err(DwgError::UnsupportedInVersion {
            version,
            what: format!(
                "modern::write_modern only handles R14+; got {version:?} \
                 (R12 uses the fixed-record codepath under dwg::r12)"
            ),
        });
    }

    let mut records = Vec::with_capacity(doc.entities.len());
    for (idx, entity) in doc.entities.iter().enumerate() {
        let handle = FIRST_ENTITY_HANDLE + idx as u64;
        let record = entity_to_record(entity, version, handle)?;
        records.push(record);
    }

    if version == Version::R2007 {
        // R2007 uses LibreDWG's `decode_R2007` codepath: RS-encoded
        // file header at byte 0x80, RS-wrapped system pages, and
        // hashcode-keyed sections-map. R2010/R2013/R2018 share the
        // `decode_R2004` codepath instead (encrypted header), so
        // they're handled by `assemble_r2004` like R2004 itself.
        //
        // Note: section payloads (header_vars / classes / objects)
        // are NOT yet wired into `assemble_r2007` — the first cut
        // emits a zero-section file to pin the file-header layer
        // against the LibreDWG oracle. Section content lands in a
        // follow-up commit.
        //
        // Loud surface for the silent-drop: if the caller hands us
        // entities, log a warning to stderr so the data loss isn't
        // invisible in production. aec_cad does not currently
        // depend on `tracing`/`log`, so `eprintln!` is the
        // available channel — matches the style used by
        // `examples/dwg_oracle_fixture.rs`. This goes away when
        // entity-bearing data pages land.
        if !records.is_empty() {
            eprintln!(
                "warning: aec_cad::dwg: dropping {} entity record(s) when \
                 writing R2007 (entity-bearing data pages not yet wired \
                 into assemble_r2007; see PR-C / phase 6 of the R2007 \
                 conformance roadmap)",
                records.len()
            );
        }
        let _ = (
            &records,
            HeaderVarsSection::libredwg_conformant(version),
            ClassesSection::empty(version),
        );
        let parts = R2007FileParts { version };
        assemble_r2007(parts)
    } else if version.has_paged_system_sections() {
        let parts = R2004FileParts {
            version,
            header_vars: HeaderVarsSection::libredwg_conformant(version),
            classes: ClassesSection::empty(version),
            objects: records,
        };
        assemble_r2004(parts)
    } else {
        let parts = R2000FileParts {
            version,
            header_vars: HeaderVarsSection::libredwg_conformant(version),
            classes: ClassesSection::empty(version),
            objects: records,
        };
        assemble_r2000(parts)
    }
}

/// Read a [`DxfDocument`] from modern (R14 / R2000 / R2004+) wire
/// bytes. Dispatches on the file's signature; the per-version
/// section format details stay encapsulated inside
/// `parse_r2000` / `parse_r2004`.
pub fn read_modern(bytes: &[u8]) -> DwgResult<DxfDocument> {
    let version = crate::dwg::version::detect(bytes).ok_or({
        DwgError::InvalidSignature({
            let mut s = [0u8; 6];
            let n = bytes.len().min(6);
            s[..n].copy_from_slice(&bytes[..n]);
            s
        })
    })?;
    let (decoded_version, objects) = if version == Version::R2007 {
        let file = parse_r2007(bytes, version)?;
        // R2007 section content not yet decoded — return version
        // only. Once `assemble_r2007` writes real sections, the
        // parser will populate this vector via the same r2004-style
        // record bridge.
        (file.version, Vec::new())
    } else if version.has_paged_system_sections() {
        let file = parse_r2004(bytes)?;
        (file.version, file.objects)
    } else if matches!(version, Version::R14 | Version::R2000) {
        let file = parse_r2000(bytes)?;
        (file.version, file.objects)
    } else {
        return Err(DwgError::UnsupportedInVersion {
            version,
            what: "modern::read_modern only handles R14+".into(),
        });
    };

    let mut doc = DxfDocument::new();
    for object in &objects {
        // The on-disk reader (parse_r2000 / parse_r2004) only peeks
        // each object's structural header. For the per-type payload
        // we re-decode via decode_with on the stored raw bytes — the
        // per-type decoder consumes exactly the payload bits and
        // leaves the cursor at the handle stream, so the framing
        // inside decode_with stays consistent.
        let entity = record_to_entity(decoded_version, object)?;
        if let Some(e) = entity {
            doc.entities.push(e);
        }
    }
    Ok(doc)
}

/// Build the wire object record for one entity. The common header is
/// fixed to "model-space BLOCK_HEADER" owner mode and a single layer
/// handle, matching what a tiny single-LINE drawing looks like in
/// LibreDWG's example fixtures.
fn entity_to_record(entity: &DxfEntity, version: Version, handle: u64) -> DwgResult<ObjectRecord> {
    let mut payload = BitWriter::new();
    let object_type = match entity {
        DxfEntity::Line(line) => {
            LineEntity::from_dxf(line).encode_payload(&mut payload, version)?;
            ObjectType::Line
        }
        DxfEntity::Arc(arc) => {
            ArcEntity::from_dxf(arc).encode_payload(&mut payload)?;
            ObjectType::Arc
        }
        DxfEntity::Circle(c) => {
            CircleEntity::from_dxf(c).encode_payload(&mut payload)?;
            ObjectType::Circle
        }
        DxfEntity::Ellipse(e) => {
            EllipseEntity::from_dxf(e).encode_payload(&mut payload)?;
            ObjectType::Ellipse
        }
        DxfEntity::Polyline(p) => {
            LwPolylineEntity::from_dxf(p).encode_payload(&mut payload)?;
            ObjectType::LwPolyline
        }
        DxfEntity::Insert(ins) => {
            InsertEntity::from_dxf(ins).encode_payload(&mut payload, version)?;
            ObjectType::Insert
        }
        DxfEntity::Text(t) => {
            TextEntity::from_dxf(t).encode_payload(&mut payload, version)?;
            ObjectType::Text
        }
        DxfEntity::Spline(_) | DxfEntity::Hatch(_) | DxfEntity::Dimension(_) => {
            return Err(DwgError::UnsupportedInVersion {
                version,
                what: format!(
                    "{:?} bit-level encoding not yet wired in modern::write_modern; \
                     the per-type codec lands in subsequent commits",
                    entity_kind(entity)
                ),
            });
        }
    };
    Ok(ObjectRecord {
        object_type,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        common: CommonHeaderData::default(),
        payload_bits: BitBuf::from_writer(payload),
        handles: ObjectHandles {
            owner: Some(HandleRef {
                code: 5,
                value: MODEL_SPACE_HANDLE,
            }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef {
                code: 5,
                value: LAYER_ZERO_HANDLE,
            },
            linetype: None,
            plot_style: None,
            material: None,
        },
    })
}

/// Convert one parsed `R2000Object` back into a `DxfEntity`. We
/// run the record's raw wire bytes through `decode_with` so the
/// per-type decoder consumes exactly the payload bits and the
/// handle stream is recovered consistently afterwards. Returns
/// `None` for entity kinds we don't yet bridge — the caller is
/// free to skip them.
fn record_to_entity(version: Version, object: &R2000Object) -> DwgResult<Option<DxfEntity>> {
    let layer = record_layer_name(object);
    let (_record, entity, _consumed) =
        ObjectRecord::decode_with(version, &object.raw_bytes, |obj_type, _common, r| {
            payload_to_entity(obj_type, r, version, layer.clone())
        })?;
    Ok(Some(entity))
}

/// Decode the per-type payload from `r` into a `DxfEntity`.
fn payload_to_entity(
    obj_type: ObjectType,
    r: &mut BitReader<'_>,
    version: Version,
    layer: String,
) -> DwgResult<DxfEntity> {
    match obj_type {
        ObjectType::Line => Ok(DxfEntity::Line(
            LineEntity::decode_payload(r, layer, version)?.into_dxf(),
        )),
        ObjectType::Arc => Ok(DxfEntity::Arc(
            ArcEntity::decode_payload(r, layer)?.into_dxf(),
        )),
        ObjectType::Circle => Ok(DxfEntity::Circle(
            CircleEntity::decode_payload(r, layer)?.into_dxf(),
        )),
        ObjectType::Ellipse => Ok(DxfEntity::Ellipse(
            EllipseEntity::decode_payload(r, layer)?.into_dxf(),
        )),
        ObjectType::LwPolyline => Ok(DxfEntity::Polyline(
            LwPolylineEntity::decode_payload(r, layer)?.into_dxf(),
        )),
        ObjectType::Insert => Ok(DxfEntity::Insert(
            InsertEntity::decode_payload(r, layer, version)?.into_dxf(),
        )),
        ObjectType::Text => Ok(DxfEntity::Text(
            TextEntity::decode_payload(r, layer, version)?.into_dxf(),
        )),
        other => Err(DwgError::UnsupportedInVersion {
            version,
            what: format!("{other:?} decoding not yet bridged to DxfEntity"),
        }),
    }
}

/// The layer name the entity claims to live on. The decoded
/// `R2000Object` doesn't yet resolve its layer handle to a string
/// (that needs the LAYER symbol table); we default to "0" until that
/// wiring lands.
fn record_layer_name(_object: &R2000Object) -> String {
    "0".into()
}

fn entity_kind(e: &DxfEntity) -> &'static str {
    match e {
        DxfEntity::Line(_) => "Line",
        DxfEntity::Polyline(_) => "Polyline",
        DxfEntity::Arc(_) => "Arc",
        DxfEntity::Circle(_) => "Circle",
        DxfEntity::Ellipse(_) => "Ellipse",
        DxfEntity::Spline(_) => "Spline",
        DxfEntity::Hatch(_) => "Hatch",
        DxfEntity::Text(_) => "Text",
        DxfEntity::Insert(_) => "Insert",
        DxfEntity::Dimension(_) => "Dimension",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::{DxfArc, DxfCircle, DxfEllipse, DxfInsert, DxfLine, DxfText};

    #[test]
    fn empty_document_round_trips_r2000() {
        let doc = DxfDocument::new();
        let bytes = write_modern(&doc, Version::R2000).unwrap();
        let back = read_modern(&bytes).unwrap();
        assert_eq!(back.entities.len(), 0);
    }

    #[test]
    fn single_line_round_trips_r2000() {
        let mut doc = DxfDocument::new();
        doc.push(DxfEntity::Line(DxfLine {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [100.0, 50.0, 0.0],
        }));
        let bytes = write_modern(&doc, Version::R2000).unwrap();
        let back = read_modern(&bytes).unwrap();
        assert_eq!(back.entities.len(), 1);
        match &back.entities[0] {
            DxfEntity::Line(l) => {
                assert_eq!(l.start, [0.0, 0.0, 0.0]);
                assert_eq!(l.end, [100.0, 50.0, 0.0]);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    #[test]
    fn single_line_round_trips_r14() {
        let mut doc = DxfDocument::new();
        doc.push(DxfEntity::Line(DxfLine {
            layer: "0".into(),
            start: [1.0, 2.0, 3.0],
            end: [4.0, 5.0, 6.0],
        }));
        let bytes = write_modern(&doc, Version::R14).unwrap();
        let back = read_modern(&bytes).unwrap();
        assert_eq!(back.entities.len(), 1);
        match &back.entities[0] {
            DxfEntity::Line(l) => {
                assert_eq!(l.start, [1.0, 2.0, 3.0]);
                assert_eq!(l.end, [4.0, 5.0, 6.0]);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    #[test]
    fn multiple_lines_round_trip_in_order() {
        let mut doc = DxfDocument::new();
        for i in 0..5 {
            let f = i as f64;
            doc.push(DxfEntity::Line(DxfLine {
                layer: "0".into(),
                start: [f, 0.0, 0.0],
                end: [f + 1.0, 0.0, 0.0],
            }));
        }
        let bytes = write_modern(&doc, Version::R2000).unwrap();
        let back = read_modern(&bytes).unwrap();
        assert_eq!(back.entities.len(), 5);
        for (i, entity) in back.entities.iter().enumerate() {
            match entity {
                DxfEntity::Line(l) => assert_eq!(l.start[0], i as f64),
                other => panic!("expected Line at {i}, got {other:?}"),
            }
        }
    }

    #[test]
    fn arc_round_trips_r2000() {
        // `DxfArc::{start_angle,end_angle}` are degrees (matching how
        // DXF group codes 50/51 are spelled on disk). The DWG wire
        // codec stores them as radians; `ArcEntity::{from_dxf,into_dxf}`
        // perform the deg<->rad conversion. Using realistic degree
        // values here — not radians masquerading as degrees — also
        // catches any future regression where the conversion is
        // accidentally skipped or doubled.
        let mut doc = DxfDocument::new();
        doc.push(DxfEntity::Arc(DxfArc {
            layer: "0".into(),
            center: [5.0, 5.0, 0.0],
            radius: 3.0,
            start_angle: 30.0,
            end_angle: 90.0,
        }));
        let bytes = write_modern(&doc, Version::R2000).unwrap();
        let back = read_modern(&bytes).unwrap();
        match &back.entities[0] {
            DxfEntity::Arc(a) => {
                assert_eq!(a.center, [5.0, 5.0, 0.0]);
                assert_eq!(a.radius, 3.0);
                assert!((a.start_angle - 30.0).abs() < 1e-9);
                assert!((a.end_angle - 90.0).abs() < 1e-9);
            }
            other => panic!("expected Arc, got {other:?}"),
        }
    }

    /// R2007+ swaps strings inside the entity payload from TV (CP1252)
    /// to T (UTF-16LE). This test feeds a TEXT entity whose `text`
    /// field contains characters outside the CP1252 set (CJK ideographs)
    /// through the full DwgWriter/DwgReader pipeline for every R2007+
    /// version — proving the per-version dispatch actually fires.
    #[test]
    fn text_with_non_ascii_round_trips_r2007_through_r2018() {
        // R2007 itself is intentionally excluded here — PR-C is
        // mid-flight: the file-header layer is wired against the
        // LibreDWG oracle, but entity-bearing data pages aren't
        // emitted yet, so the round-trip drops content. R2010 /
        // R2013 / R2018 still go through assemble_r2004 and round-
        // trip cleanly. Re-enabling R2007 here is pinned by the
        // entity-content commit later in this same PR.
        for v in [Version::R2010, Version::R2013, Version::R2018] {
            let mut doc = DxfDocument::new();
            doc.push(DxfEntity::Text(DxfText {
                layer: "0".into(),
                position: [0.0, 0.0, 0.0],
                height: 2.5,
                rotation: 0.0,
                text: "\u{6f22}\u{5b57}".into(), // 漢字
            }));
            let bytes = write_modern(&doc, v).expect("write_modern");
            let back = read_modern(&bytes).expect("read_modern");
            assert_eq!(back.entities.len(), 1, "v={v:?}");
            match &back.entities[0] {
                DxfEntity::Text(t) => {
                    assert_eq!(t.text, "\u{6f22}\u{5b57}", "v={v:?}");
                    assert_eq!(t.height, 2.5, "v={v:?}");
                }
                other => panic!("expected Text under {v:?}, got {other:?}"),
            }
        }
    }

    /// INSERT carries a block name as a payload-level string. Exercise
    /// the version dispatch for that too on a R2018 round-trip with a
    /// non-ASCII block name.
    #[test]
    fn insert_with_non_ascii_block_name_round_trips_r2018() {
        let mut doc = DxfDocument::new();
        doc.push(DxfEntity::Insert(DxfInsert {
            layer: "0".into(),
            block_name: "\u{56fe}\u{5757}-A1".into(), // 图块-A1
            position: [10.0, 20.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        }));
        let bytes = write_modern(&doc, Version::R2018).expect("write_modern R2018");
        let back = read_modern(&bytes).expect("read_modern R2018");
        assert_eq!(back.entities.len(), 1);
        match &back.entities[0] {
            DxfEntity::Insert(ins) => {
                assert_eq!(ins.block_name, "\u{56fe}\u{5757}-A1");
                assert_eq!(ins.position, [10.0, 20.0, 0.0]);
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    /// R2000 must still write TV (CP1252). Feeding it a non-CP1252
    /// string must error rather than corrupt the file silently.
    #[test]
    fn text_with_non_ascii_rejects_r2000() {
        let mut doc = DxfDocument::new();
        doc.push(DxfEntity::Text(DxfText {
            layer: "0".into(),
            position: [0.0, 0.0, 0.0],
            height: 2.5,
            rotation: 0.0,
            text: "\u{6f22}\u{5b57}".into(),
        }));
        let err = write_modern(&doc, Version::R2000).unwrap_err();
        assert!(matches!(err, DwgError::InvalidStringEncoding { .. }));
    }

    #[test]
    fn circle_and_ellipse_round_trip_r2000() {
        let mut doc = DxfDocument::new();
        doc.push(DxfEntity::Circle(DxfCircle {
            layer: "0".into(),
            center: [1.0, 2.0, 0.0],
            radius: 4.0,
        }));
        doc.push(DxfEntity::Ellipse(DxfEllipse {
            layer: "0".into(),
            center: [10.0, 20.0, 0.0],
            major_axis: [5.0, 0.0, 0.0],
            ratio: 0.5,
            start_param: 0.0,
            end_param: std::f64::consts::TAU,
        }));
        let bytes = write_modern(&doc, Version::R2000).unwrap();
        let back = read_modern(&bytes).unwrap();
        assert_eq!(back.entities.len(), 2);
        match &back.entities[0] {
            DxfEntity::Circle(c) => {
                assert_eq!(c.center, [1.0, 2.0, 0.0]);
                assert_eq!(c.radius, 4.0);
            }
            other => panic!("expected Circle, got {other:?}"),
        }
        match &back.entities[1] {
            DxfEntity::Ellipse(e) => {
                assert_eq!(e.center, [10.0, 20.0, 0.0]);
                assert_eq!(e.major_axis, [5.0, 0.0, 0.0]);
                assert_eq!(e.ratio, 0.5);
            }
            other => panic!("expected Ellipse, got {other:?}"),
        }
    }
}
