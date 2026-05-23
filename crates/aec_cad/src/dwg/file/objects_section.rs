//! Shared helpers for assembling and recovering the OBJECTS +
//! HANDLES sections used by R14/R2000, R2004, and R2007 file
//! layouts.
//!
//! The three modern layouts (`r2000_layout`, `r2004_layout`,
//! `r2007_layout`) share the same in-section byte format for
//! objects and the same MC-delta encoding for the object map —
//! only the page packaging around them differs:
//!
//! - **R14 / R2000** lays OBJECTS at a known file offset and the
//!   object map records *file-absolute* offsets that the parser
//!   uses to seek directly.
//! - **R2004 / R2007** wraps OBJECTS inside a paged section, so
//!   the object map records *section-relative* offsets and the
//!   parser walks the decompressed payload sequentially.
//!
//! Both cases reduce to:
//!
//! 1. **Encode**: serialize each `ObjectRecord` to wire bytes,
//!    concatenate, remember per-record offsets, then sort by
//!    handle and emit an `ObjectMap`.
//! 2. **Decode**: peek each record's structural header, slice
//!    off `total` bytes, append a `R2000Object` per slice.
//!
//! This module factors those two halves into version-agnostic
//! helpers so the three assemblers/parsers can share a single
//! implementation. The previous parallel codepaths drifted twice
//! during the conformance work (PR-H1 round 2 fixed a dead
//! `find/map_or` lookup that only existed in R2004 + R2007; the
//! shared helpers prevent that class of asymmetry mechanically).

use crate::dwg::entities::record::ObjectRecord;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::object_map::{ObjectMap, ObjectMapEntry};
use crate::dwg::file::r2000_layout::R2000Object;
use crate::dwg::version::Version;

/// Encode every record's wire bytes into a single payload and
/// return the per-record section-relative offsets in input order.
///
/// `payload[offsets[i]..offsets[i] + record_size_i]` is the wire
/// bytes of `records[i]`. Callers that need the object map must
/// pair the result with [`build_handle_object_map`].
pub(crate) fn encode_objects_payload(
    records: &[ObjectRecord],
    version: Version,
) -> DwgResult<(Vec<u8>, Vec<u64>)> {
    let mut payload: Vec<u8> = Vec::new();
    let mut offsets: Vec<u64> = Vec::with_capacity(records.len());
    for record in records {
        offsets.push(payload.len() as u64);
        let wire = record.encode(version)?;
        payload.extend_from_slice(&wire);
    }
    Ok((payload, offsets))
}

/// Build the object map for a previously-encoded objects payload.
///
/// Entries are emitted in handle order (canonical layout used by
/// LibreDWG and AutoCAD); each entry's `file_offset` is the input
/// `offset_base` plus the record's section-relative offset.
///
/// - For R14/R2000 callers, `offset_base` is the *file-absolute*
///   offset of the start of the OBJECTS stream (the object map's
///   offsets are then file-absolute, matching what the R14/R2000
///   parser dereferences with [`recover_objects_via_map`]).
/// - For R2004/R2007 callers, `offset_base` is `0` (the offsets
///   are section-relative to the decompressed OBJECTS payload).
pub(crate) fn build_handle_object_map(
    records: &[ObjectRecord],
    record_offsets: &[u64],
    offset_base: u64,
) -> DwgResult<ObjectMap> {
    if records.len() != record_offsets.len() {
        return Err(DwgError::InternalInvariant(format!(
            "build_handle_object_map: records.len()={} but \
             record_offsets.len()={}; the two slices must be \
             produced together by `encode_objects_payload`",
            records.len(),
            record_offsets.len(),
        )));
    }
    let mut indices: Vec<usize> = (0..records.len()).collect();
    indices.sort_by_key(|&i| records[i].handle.value);
    let mut map = ObjectMap::new();
    for &i in &indices {
        map.entries.push(ObjectMapEntry {
            handle: records[i].handle.value,
            file_offset: offset_base + record_offsets[i],
        });
    }
    Ok(map)
}

/// Recover one [`R2000Object`] per record in `payload`, walking it
/// sequentially via [`ObjectRecord::peek_header`].
///
/// Used by the R2004/R2007 parsers, where the OBJECTS section is
/// stored as a single decompressed payload buffer (the object
/// map's offsets are section-relative to that buffer, and we walk
/// it from index 0).
pub(crate) fn recover_objects_sequential(
    payload: &[u8],
    version: Version,
) -> DwgResult<Vec<R2000Object>> {
    let mut objects: Vec<R2000Object> = Vec::new();
    let mut cursor = 0usize;
    while cursor < payload.len() {
        let (object_type, record_handle, common, total) =
            ObjectRecord::peek_header(version, &payload[cursor..])?;
        if cursor + total > payload.len() {
            return Err(DwgError::UnexpectedEof {
                byte: cursor + total,
                bit: 0,
            });
        }
        let raw_bytes = payload[cursor..cursor + total].to_vec();
        // The object map's role is to validate handle presence
        // (see `ObjectMap::validate_against_records`) and to
        // recover per-record offsets within the section — not to
        // relabel handles. The record's own `record_handle` is
        // the canonical identifier; the previous R2004/R2007
        // codepaths each had a redundant `.find(|e| e.handle ==
        // record_handle.value).map_or(record_handle.value, |e|
        // e.handle)` lookup that returned `record_handle.value`
        // on both branches. PR-H1 round 2 dropped that lookup;
        // this helper keeps the corrected semantics.
        let map_handle = record_handle.value;
        objects.push(R2000Object {
            map_handle,
            object_type,
            record_handle,
            common,
            raw_bytes,
        });
        cursor += total;
    }
    Ok(objects)
}

/// Recover one [`R2000Object`] per entry in `object_map`, seeking
/// into `bytes` via each entry's `file_offset`.
///
/// Used by the R14/R2000 parser, where the OBJECTS section lives
/// at a known file offset and the object map carries
/// file-absolute offsets — there's no separate decompressed
/// buffer to walk sequentially.
pub(crate) fn recover_objects_via_map(
    bytes: &[u8],
    object_map: &ObjectMap,
    version: Version,
) -> DwgResult<Vec<R2000Object>> {
    let mut objects = Vec::with_capacity(object_map.entries.len());
    for entry in &object_map.entries {
        let off = entry.file_offset as usize;
        if off >= bytes.len() {
            return Err(DwgError::DanglingHandle {
                handle: entry.handle,
                offset: entry.file_offset,
                file_size: bytes.len(),
            });
        }
        let (object_type, record_handle, common, total) =
            ObjectRecord::peek_header(version, &bytes[off..])?;
        if off + total > bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: off + total,
                bit: 0,
            });
        }
        let raw_bytes = bytes[off..off + total].to_vec();
        objects.push(R2000Object {
            map_handle: entry.handle,
            object_type,
            record_handle,
            common,
            raw_bytes,
        });
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::bits::reader::HandleRef;
    use crate::dwg::entities::header_codec::CommonHeaderData;
    use crate::dwg::entities::record::{BitBuf, ObjectCommonData, ObjectHandles, ObjectSupertype};
    use crate::dwg::entities::ObjectType;

    /// Minimal handle-only ObjectRecord for shuffler tests. The
    /// helpers operate on the (handle, wire-size) projection of a
    /// record; per-type payload is irrelevant here.
    fn line_record(handle_value: u64) -> ObjectRecord {
        ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef {
                code: 0,
                value: handle_value,
            },
            supertype: ObjectSupertype::Entity,
            common: CommonHeaderData::default(),
            object_common: ObjectCommonData::default(),
            payload_bits: BitBuf::new(),
            string_payload_bits: BitBuf::new(),
            handles: ObjectHandles {
                owner: Some(HandleRef {
                    code: 5,
                    value: 0x01,
                }),
                layer: HandleRef {
                    code: 5,
                    value: 0x02,
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn encode_then_recover_preserves_handle_order() {
        let records = vec![line_record(0x42), line_record(0x10), line_record(0x99)];
        let (payload, offsets) = encode_objects_payload(&records, Version::R2000).unwrap();
        assert_eq!(offsets.len(), 3);
        let recovered = recover_objects_sequential(&payload, Version::R2000).unwrap();
        assert_eq!(recovered.len(), 3);
        // recover_objects_sequential walks the payload in input
        // order; the per-record handle is preserved.
        assert_eq!(recovered[0].record_handle.value, 0x42);
        assert_eq!(recovered[1].record_handle.value, 0x10);
        assert_eq!(recovered[2].record_handle.value, 0x99);
    }

    #[test]
    fn build_handle_object_map_sorts_by_handle() {
        let records = vec![line_record(0x42), line_record(0x10), line_record(0x99)];
        let (_payload, offsets) = encode_objects_payload(&records, Version::R2000).unwrap();
        let map = build_handle_object_map(&records, &offsets, 0).unwrap();
        let handles: Vec<u64> = map.entries.iter().map(|e| e.handle).collect();
        assert_eq!(handles, vec![0x10, 0x42, 0x99]);
        // Offsets are section-relative when offset_base = 0.
        // The smallest-handle record (0x10) was recorded second
        // in input order, so its map offset matches `offsets[1]`.
        assert_eq!(map.entries[0].file_offset, offsets[1]);
        assert_eq!(map.entries[1].file_offset, offsets[0]);
        assert_eq!(map.entries[2].file_offset, offsets[2]);
    }

    #[test]
    fn build_handle_object_map_applies_offset_base() {
        let records = vec![line_record(0x10)];
        let (_payload, offsets) = encode_objects_payload(&records, Version::R2000).unwrap();
        let map = build_handle_object_map(&records, &offsets, 0x1000).unwrap();
        assert_eq!(map.entries[0].file_offset, 0x1000 + offsets[0]);
    }

    #[test]
    fn build_handle_object_map_rejects_mismatched_slices() {
        let records = vec![line_record(0x10)];
        let offsets = vec![0u64, 1u64]; // length mismatch
        let err = build_handle_object_map(&records, &offsets, 0).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
    }
}
