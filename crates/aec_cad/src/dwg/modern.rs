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
use crate::dwg::file::header_vars_body::HeaderVars;
use crate::dwg::file::r2000_layout::{assemble_r2000, parse_r2000, R2000FileParts, R2000Object};
use crate::dwg::file::r2004_layout::{assemble_r2004, parse_r2004, R2004FileParts};
use crate::dwg::file::r2007_layout::{assemble_r2007, parse_r2007, R2007FileParts};
use crate::dwg::tables::object_emit::{self, handles as object_handles, ModelSpaceOwnership};
use crate::dwg::version::Version;
use crate::dxf::{DxfDocument, DxfEntity};

/// First user-entity handle. The handles below `0x21` are reserved
/// for the implicit table-record objects we emit alongside every
/// modern document (see
/// [`crate::dwg::tables::object_emit::handles`]). User entities
/// are numbered sequentially from this base.
const FIRST_ENTITY_HANDLE: u64 = object_handles::FIRST_USER_ENTITY;

/// Handle of the always-present LAYER "0" record. Every entity's
/// `layer` handle resolves to this object; if the object doesn't
/// exist, LibreDWG's `dwg_resolve_handle` logs `Object handle not
/// found` and `dxf_tables_write` bails before emitting LAYER rows.
const LAYER_ZERO_HANDLE: u64 = object_handles::LAYER_ZERO;

/// Handle of the `*Model_Space` `BLOCK_HEADER` record. Each entity
/// owner-handle resolves to this object; without it,
/// `dwg_model_space_object` returns `NULL` and `dxf_entities_write`
/// truncates the DXF after the TABLES section header.
const MODEL_SPACE_HANDLE: u64 = object_handles::MODEL_SPACE_BLOCK_HEADER;

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

    // Build the user-entity records first (they need to be wired
    // into the BLOCK_HEADER's ownership chain, which means we have
    // to know their handle list before we can serialise the table
    // objects).
    let mut user_entities = Vec::with_capacity(doc.entities.len());
    for (idx, entity) in doc.entities.iter().enumerate() {
        let handle = FIRST_ENTITY_HANDLE + idx as u64;
        let record = entity_to_record(entity, version, handle)?;
        user_entities.push(record);
    }

    let records = if version == Version::R2007 {
        // R2007 ships an empty document today; the table-object
        // wrapper is unused, so keep `records` empty.
        Vec::new()
    } else {
        build_record_set(version, user_entities)?
    };

    if version == Version::R2007 {
        // R2007 uses LibreDWG's `decode_R2007` codepath: RS-encoded
        // file header at byte 0x80, RS-wrapped system pages, and
        // hashcode-keyed sections-map. R2010/R2013/R2018 share the
        // `decode_R2004` codepath instead (encrypted header), so
        // they're handled by `assemble_r2004` like R2004 itself.
        //
        // The R2007 entity-bearing data pages are NOT yet wired into
        // `assemble_r2007` — the first cut emits a zero-section file
        // to pin the file-header layer against the LibreDWG oracle.
        // Until that work lands, callers must NOT pass entities in
        // the doc when targeting R2007 — silently dropping them is
        // worse than failing loudly, since the data loss is otherwise
        // invisible to any caller that doesn't watch stderr.
        if !doc.entities.is_empty() {
            return Err(DwgError::UnsupportedInVersion {
                version,
                what: format!(
                    "writing {n} entity record(s) to a R2007 document is \
                     not yet supported: `assemble_r2007` does not emit \
                     entity-bearing data pages (the R2007 system pages \
                     ship the file header, classes, and handle map only). \
                     Either target R2004, R2010, R2013, or R2018 -- all of \
                     which fully round-trip entities through the \
                     decode_R2004 codepath -- or pre-filter the document \
                     down to zero entities before writing R2007. See the \
                     R2007 conformance roadmap (PR-C / phase 6) for the \
                     follow-up work that lifts this restriction.",
                    // Report the count from the original user document.
                    // `records` was reset to `Vec::new()` above (R2007's
                    // table-object wrapper is empty), so its length is
                    // always 0 here.
                    n = doc.entities.len()
                ),
            });
        }
        // `assemble_r2007` now derives every section's content from
        // `version` internally — it builds its own `AuxHeaderSection`,
        // `ClassesSection`, and `ObjectMap` at the layout step (see
        // `r2007_layout::assemble_r2007`'s step 3). When R2007 entity
        // round-trip lands, `R2007FileParts` will grow `objects`,
        // `header_vars`, `classes` fields mirroring `R2004FileParts`
        // and this branch will start threading them in.
        let parts = R2007FileParts { version };
        assemble_r2007(parts)
    } else if version.has_paged_system_sections() {
        let parts = R2004FileParts {
            version,
            header_vars: HeaderVarsSection::with_vars(version, &header_vars_for_records(&records)),
            classes: ClassesSection::empty(version),
            objects: records,
        };
        assemble_r2004(parts)
    } else {
        let parts = R2000FileParts {
            version,
            header_vars: HeaderVarsSection::with_vars(version, &header_vars_for_records(&records)),
            classes: ClassesSection::empty(version),
            objects: records,
        };
        assemble_r2000(parts)
    }
}

/// Assemble the full record set for a modern (R14+) DWG. The order
/// is significant for LibreDWG's DXF emitter: control objects come
/// first so that `dwg_get_first_object(BLOCK_CONTROL)` /
/// `LAYER_CONTROL` return non-NULL, then their children, then the
/// model-space BLOCK_HEADER and the entities it frames.
fn build_record_set(
    version: Version,
    mut user_entities: Vec<ObjectRecord>,
) -> DwgResult<Vec<ObjectRecord>> {
    // R14/R2000 chain entities via prev_entity/next_entity in the
    // handle stream (gated by `!common.nolinks`). The default
    // `CommonHeaderData::nolinks = true` suppresses those handles,
    // so flip it for R14/R2000 BEFORE constructing the chain.
    if version <= Version::R2000 {
        for record in &mut user_entities {
            record.common.nolinks = false;
        }
    }

    let user_handles: Vec<u64> = user_entities.iter().map(|r| r.handle.value).collect();
    let ownership = ModelSpaceOwnership {
        block_entity: object_handles::MODEL_SPACE_BLOCK,
        endblk_entity: object_handles::MODEL_SPACE_ENDBLK,
        user_entities: user_handles,
    };

    // Frame entities (BLOCK / ENDBLK) bracketing the user entities.
    let block_entity = object_emit::emit_model_space_block_entity(
        version,
        object_handles::MODEL_SPACE_BLOCK,
        MODEL_SPACE_HANDLE,
        LAYER_ZERO_HANDLE,
    )?;
    let endblk_entity = object_emit::emit_model_space_endblk_entity(
        object_handles::MODEL_SPACE_ENDBLK,
        MODEL_SPACE_HANDLE,
        LAYER_ZERO_HANDLE,
    )?;

    // [BLOCK, user…, ENDBLK]. wire_block_chain only mutates prev/next
    // for R14/R2000; on R2004+ it is a no-op (entities[] vector
    // inside BLOCK_HEADER carries ownership instead of a chain).
    let mut chain = Vec::with_capacity(user_entities.len() + 2);
    chain.push(block_entity);
    chain.append(&mut user_entities);
    chain.push(endblk_entity);
    object_emit::wire_block_chain(version, &mut chain);

    // Table-record objects: LAYER "0", LAYER_CONTROL, BLOCK_HEADER,
    // BLOCK_CONTROL. The CONTROL objects own the table records via
    // `entries[]` handles; the BLOCK_HEADER owns its frame entities
    // and (R2004+) the entities[] vector.
    let layer_zero =
        object_emit::emit_layer_zero(version, LAYER_ZERO_HANDLE, object_handles::LAYER_CONTROL)?;
    let layer_control = object_emit::emit_layer_control(
        version,
        object_handles::LAYER_CONTROL,
        &[LAYER_ZERO_HANDLE],
    )?;
    let block_header = object_emit::emit_model_space_block_header(
        version,
        MODEL_SPACE_HANDLE,
        object_handles::BLOCK_CONTROL,
        &ownership,
    )?;
    let block_control = object_emit::emit_block_control(
        version,
        object_handles::BLOCK_CONTROL,
        &[MODEL_SPACE_HANDLE],
        MODEL_SPACE_HANDLE,
        0, // *Paper_Space — not emitted
    )?;

    // Final record ordering. LibreDWG iterates `dwg->object[i]` in
    // index order to build the object_map, so any ordering is valid
    // as long as every referenced handle resolves. We emit the
    // control objects first so the file is easy to inspect with
    // `dwgread -v9`.
    let mut records =
        Vec::with_capacity(4 /* table objects */ + 1 /* block_header */ + chain.len());
    records.push(block_control);
    records.push(layer_control);
    records.push(layer_zero);
    records.push(block_header);
    records.extend(chain);
    Ok(records)
}

/// Build a `HeaderVars` whose handle fields point at the table
/// objects we emit alongside every modern DWG.
///
/// `HANDSEED` is computed from the actual record set as
/// `max(handle.value) + 1`, matching the spec definition ("next
/// handle to allocate"). Falls back to `FIRST_USER_ENTITY` if
/// `records` is empty so an empty-document fixture still gets a
/// sane next-unused value.
fn header_vars_for_records(records: &[ObjectRecord]) -> HeaderVars {
    // **HANDSEED = max(handle) + 1.** Spec-correct: HANDSEED is the
    // "next handle to allocate", so it points one past the highest
    // handle currently in use. LibreDWG's post-decode
    // `dwg_resolve_handle` loop (see `dwg.c:896-911`) iterates every
    // `object_ref` in the model — including the HANDSEED slot in
    // header_vars — and warns
    // `Warning: Object handle not found <abs>/<abs_hex>` for any
    // value not in the `object_map`. Since HANDSEED by definition
    // points at an unused handle, it ALWAYS triggers this warning;
    // LibreDWG's own `example_2000.dwg` fires it for its
    // `HANDSEED = 0xBE7`.
    //
    // We accept that warning at the oracle gate, but only the
    // **exact** warning produced by our HANDSEED — see the
    // `ALLOW_HANDSEED` pattern in
    // `.github/workflows/ci.yml::libredwg_oracle`. The allow-list
    // is scoped to the precise decimal/hex pair our writer emits
    // (one past the highest user-entity handle), so any OTHER
    // dangling-handle regression — an entity pointing at a missing
    // BLOCK_HEADER, a corrupted owner pointer, a typo in a table
    // record — still fails the gate because it produces a different
    // numeric value (or the long-form warning text when the dangling
    // handle is below HANDSEED).
    //
    // Edge case: if `records` is empty (no entities, no table
    // objects), we have no handles to look at. Fall back to
    // `FIRST_USER_ENTITY` (0x21), which is what the next allocation
    // would use anyway — consistent with the spec definition of
    // "next handle to allocate".
    let max_handle = records
        .iter()
        .map(|r| r.handle.value)
        .max()
        .unwrap_or(object_handles::FIRST_USER_ENTITY - 1);
    let handseed_value = max_handle + 1;
    HeaderVars {
        handseed: HandleRef {
            code: 0,
            value: handseed_value,
        },
        clayer: HandleRef {
            code: 5,
            value: LAYER_ZERO_HANDLE,
        },
        block_control_object: HandleRef {
            code: 3,
            value: object_handles::BLOCK_CONTROL,
        },
        layer_control_object: HandleRef {
            code: 3,
            value: object_handles::LAYER_CONTROL,
        },
        block_record_mspace: HandleRef {
            code: 5,
            value: MODEL_SPACE_HANDLE,
        },
        ..HeaderVars::default()
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
    // Per-entity-type extra handles, appended AFTER the common
    // handle stream. Mirrors the LibreDWG `dwg.spec` lines that emit
    // `FIELD_HANDLE (...)` AFTER `COMMON_ENTITY_HANDLE_DATA` for each
    // entity type. Counts must match `type_extra_handle_count`.
    let type_extras = match object_type {
        // TEXT.style: soft pointer to AcDbStyle. We don't yet emit a
        // text-styles section, so we use a NULL soft pointer; LibreDWG
        // treats `H { code: 5, value: 0 }` as the STANDARD style.
        ObjectType::Text => vec![HandleRef { code: 5, value: 0 }],
        // INSERT.block_header: soft pointer to AcDbBlockTableRecord.
        // Same NULL-pointer convention; LibreDWG logs "BLOCK_HEADER:
        // NULL" but does not error.
        ObjectType::Insert => vec![HandleRef { code: 5, value: 0 }],
        _ => Vec::new(),
    };
    Ok(ObjectRecord {
        object_type,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: crate::dwg::entities::record::ObjectSupertype::Entity,
        common: CommonHeaderData::default(),
        object_common: crate::dwg::entities::record::ObjectCommonData::default(),
        payload_bits: BitBuf::from_writer(payload),
        string_payload_bits: BitBuf::new(),
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
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras,
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
    // OBJECT-supertype records (LAYER, BLOCK_HEADER, *_CONTROL,
    // DICTIONARY, …) use a different common-header layout than
    // entities and never bridge to a `DxfEntity`. Skip them on read.
    // `decode_with` would otherwise blow up trying to parse the
    // entity `preview_exists` bit out of object common-data bytes.
    if !object.object_type.is_entity() {
        return Ok(None);
    }
    // BLOCK / ENDBLK are *framing* entities that delimit a block's
    // entity sequence (see LibreDWG `objects.spec` BLOCK_HEADER's
    // first_entity / last_entity chain). They are entities by
    // supertype but do not surface as user-visible `DxfEntity`s on
    // the DXF side — skip them so round-trip preserves only the
    // entities the user authored.
    if matches!(object.object_type, ObjectType::Block | ObjectType::EndBlk) {
        return Ok(None);
    }
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

    /// Pin the HANDSEED value the LibreDWG oracle's `ALLOW_HANDSEED`
    /// pattern (`.github/workflows/ci.yml`) expects, so a change to
    /// the fixture entity count fails this test with a clear,
    /// actionable error rather than a cryptic CI "unexpected
    /// Warning" surprise.
    ///
    /// **Pins the coupling end-to-end** by importing
    /// `crate::dwg::test_fixtures::oracle_fixture_doc` — the same
    /// function `crates/aec_cad/examples/dwg_oracle_fixture.rs` uses
    /// to build the per-version .dwg files CI feeds into `dwgread`.
    /// Any change to that function's entity count fails both
    /// assertions below with explicit, actionable messages before it
    /// can land in CI.
    ///
    /// Keep all three in lockstep:
    ///   1. `crate::dwg::test_fixtures::oracle_fixture_doc` (geometry);
    ///   2. `EXPECTED_ORACLE_FIXTURE_ENTITY_COUNT` and
    ///      `EXPECTED_ORACLE_FIXTURE_HANDSEED` below (the pinned
    ///      counts the CI gate expects);
    ///   3. `ALLOW_HANDSEED` in `.github/workflows/ci.yml` (the
    ///      `<decimal>/0x<hex>` pair CI's regex matches against).
    #[test]
    fn oracle_fixture_handseed_matches_ci_allow_list() {
        use crate::dwg::test_fixtures::oracle_fixture_doc;

        // CI's `ALLOW_HANDSEED` regex hardcodes `36/0x24` (see
        // `.github/workflows/ci.yml::libredwg_oracle`). 0x24 == 36.
        const EXPECTED_ORACLE_FIXTURE_ENTITY_COUNT: usize = 3;
        const EXPECTED_ORACLE_FIXTURE_HANDSEED: u64 = 0x24;

        // Use the SAME function the example binary uses, not a
        // duplicate. This is what gives the test its end-to-end
        // pinning power — drift in `oracle_fixture_doc` shows up
        // immediately in the entity-count assert below.
        let doc = oracle_fixture_doc();

        assert_eq!(
            doc.entities.len(),
            EXPECTED_ORACLE_FIXTURE_ENTITY_COUNT,
            "Oracle-fixture entity-count drift: \
             `crate::dwg::test_fixtures::oracle_fixture_doc` now \
             returns {actual} entities, but CI's `ALLOW_HANDSEED` \
             regex in `.github/workflows/ci.yml` is hardcoded to \
             `36/0x24` (which assumes exactly 3 entities at handles \
             0x21 / 0x22 / 0x23 → HANDSEED = 0x24). If the new \
             entity count is intentional, update \
             `EXPECTED_ORACLE_FIXTURE_ENTITY_COUNT` and \
             `EXPECTED_ORACLE_FIXTURE_HANDSEED` in this test, and \
             update `ALLOW_HANDSEED` in `.github/workflows/ci.yml` \
             to `<decimal>/0x<hex>` matching the new HANDSEED value.",
            actual = doc.entities.len(),
        );

        // Build the user-entity records the same way `write_modern`
        // does (handles assigned sequentially from FIRST_USER_ENTITY).
        let version = Version::R2000;
        let mut user_entities = Vec::with_capacity(doc.entities.len());
        for (idx, entity) in doc.entities.iter().enumerate() {
            let handle = FIRST_ENTITY_HANDLE + idx as u64;
            let record = entity_to_record(entity, version, handle).unwrap();
            user_entities.push(record);
        }
        let records = build_record_set(version, user_entities).unwrap();
        let hvars = header_vars_for_records(&records);

        assert_eq!(
            hvars.handseed.value,
            EXPECTED_ORACLE_FIXTURE_HANDSEED,
            "Oracle-fixture HANDSEED drift: computed {actual:#x} but \
             CI's `ALLOW_HANDSEED` pattern in \
             `.github/workflows/ci.yml` is hardcoded to `36/0x24`. \
             This usually means the fixture entity count in \
             `crate::dwg::test_fixtures::oracle_fixture_doc` changed \
             but `ALLOW_HANDSEED` / \
             `EXPECTED_ORACLE_FIXTURE_HANDSEED` weren't updated. \
             Update both to `<decimal>/0x<hex>` matching the new \
             HANDSEED value.",
            actual = hvars.handseed.value,
        );
    }
}
