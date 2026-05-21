//! Native IFC STEP serializer + parser + geometry tessellator.
//!
//! As of Phase 9 (Tasks 15–18) this is AEC Studio's *sole* IFC
//! implementation: there is no external IfcOpenShell worker. The
//! reader, writer, and [`super::tessellator`] together cover every
//! IFC4 entity AEC Studio reads or writes on disk:
//!
//!   * Spatial structure: `IfcProject`, `IfcSite`, `IfcBuilding`,
//!     `IfcBuildingStorey`, `IfcSpace`.
//!   * Building elements: `IfcWall`, `IfcSlab`, `IfcDoor`, `IfcWindow`,
//!     `IfcFurnishingElement`, `IfcCovering`, …
//!   * Spatial relations: `IfcRelAggregates` (spatial → spatial) and
//!     `IfcRelContainedInSpatialStructure` (storey ← elements).
//!   * Property sets: `IfcPropertySet` + `IfcRelDefinesByProperties`.
//!   * Quantity sets: `IfcElementQuantity` + `IfcRelDefinesByProperties`.
//!   * Globally unique identifiers (IfcGloballyUniqueId): assigned on
//!     export when missing, **preserved verbatim** on re-export.
//!
//! The writer and reader are byte-level inverses for this subset:
//! every GUID assigned by `Project` survives a `to_step` → `from_step`
//! roundtrip, including the spatial relations and the
//! IfcRelDefinesByProperties wiring between elements and Psets/Qtos.
//!
//! Schema reach: IFC4 by default; IFC2x3 and IFC4x3 entity instance
//! files are accepted and parsed (entity kinds AEC Studio doesn't
//! model are preserved verbatim by the property-roundtrip path —
//! see [`crate::properties::PropertyStore`]). Geometry for
//! `IfcExtrudedAreaSolid` and `IfcFacetedBrep` is tessellated in
//! Rust via [`crate::tessellator`]. IFC inheritance beyond the
//! listed classes and the full material library are out of scope.

pub mod reader;
pub mod writer;

pub use reader::{
    IfcReadError, IfcReadResult, IfcReadStats, IfcReader, IfcSchema, IfcSnapshot, StepIter,
    StepRecord,
};
pub use writer::{IfcWriteError, IfcWriter};

/// Deterministically map an `EntityId` to a 22-char compressed IFC
/// GlobalId. The mapping is stable across runs and process invocations
/// — the same `EntityId` always produces the same GUID — so spatial
/// nodes and elements without an explicit `ifc_guid` still get a
/// reproducible identifier on export.
pub fn compress_entity_id_to_guid(id: &aec_core::types::EntityId) -> String {
    derive_guid_from_str(&id.to_string())
}

/// Derive a stable 22-char IFC GUID from an arbitrary seed string. Used
/// internally to give relation entities (IFCRELAGGREGATES,
/// IFCRELDEFINESBYPROPERTIES, …) deterministic GlobalIds without
/// allocating real EntityIds for them.
pub fn derive_guid_from_str(seed: &str) -> String {
    // 22-char IFC GUIDs use a 64-character alphabet:
    //   0-9, A-Z, a-z, '_', '$'
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

    // Use BLAKE3 to project the seed into 16 bytes, then base64-ish
    // encode into 22 chars over the IFC alphabet. 16 bytes encodes into
    // ceil(16 * 8 / 6) = 22 chars — exactly the IFC GUID length.
    let hash = blake3::hash(seed.as_bytes());
    let raw = &hash.as_bytes()[..16];

    let mut out = String::with_capacity(22);
    // Sliding 6-bit window over the 16 input bytes. `acc` is `u32` and
    // never holds more than 12 *significant* bits at a time, so the
    // accumulator comfortably fits and `acc << 8` never discards any
    // meaningful state.
    //
    // The invariant: at the top of the outer loop `bits ∈ {0, 2, 4}`
    // (the leftover from the inner extraction); after `acc << 8 | b`
    // the live window grows to at most 12 bits; the inner `while`
    // emits 6-bit chunks until `bits < 6` again. The exact cycle is
    // `bits: 0 → 8 → 2 → 10 → 4 → 12 → 6 → 0 → …` (entries are the
    // values BEFORE each emission), and the corresponding number of
    // chars pushed per outer iteration is 1, 1, 2 — totalling 22 over
    // 16 input bytes (matching `ceil(16 * 8 / 6) = 22`, the IFC GUID
    // length).
    //
    // Note: Rust's `<<` simply discards bits shifted out of the high
    // end (no wrap-on-overflow / no debug-mode panic for `<<` with
    // shift count < 32). Because the invariant above keeps the
    // accumulator below 13 significant bits, no information is ever
    // lost from `acc` — the bits we appear to "shift out" were
    // already zero. `& 0x3F` masks the 6 target bits in each
    // extraction.
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in raw {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            let idx = ((acc >> bits) & 0x3F) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((acc << (6 - bits)) & 0x3F) as usize;
        out.push(ALPHABET[idx] as char);
    }
    debug_assert_eq!(out.len(), 22, "IFC GUIDs are exactly 22 chars");
    out
}

/// Validate that `guid` matches the IFC4 `IfcGloballyUniqueId` grammar
/// (exactly 22 characters from the 64-char compressed alphabet).
///
/// Returns `true` if the GUID is safe to interpolate directly into a
/// STEP single-quoted string. Used by the writer as defense-in-depth
/// against user-supplied GUIDs that contain `'`, `\`, control chars,
/// or are the wrong length — any of which would corrupt the IFC file
/// or break the reader's tokenizer.
pub fn is_valid_ifc_guid(guid: &str) -> bool {
    guid.len() == 22
        && guid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

/// Return `guid` if it is valid IFC syntax, otherwise return a stable
/// fallback derived from `fallback_seed`. Writers use this to guarantee
/// the emitted STEP record is always parseable, even if upstream code
/// stored a malformed GUID.
pub fn sanitize_ifc_guid(guid: &str, fallback_seed: &str) -> String {
    if is_valid_ifc_guid(guid) {
        guid.to_owned()
    } else {
        derive_guid_from_str(fallback_seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::types::EntityId;

    #[test]
    fn compressed_guid_is_22_chars_and_deterministic() {
        let id = EntityId::new();
        let a = compress_entity_id_to_guid(&id);
        let b = compress_entity_id_to_guid(&id);
        assert_eq!(a, b);
        assert_eq!(a.len(), 22);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$'));
    }

    #[test]
    fn distinct_entity_ids_produce_distinct_guids() {
        let a = compress_entity_id_to_guid(&EntityId::new());
        let b = compress_entity_id_to_guid(&EntityId::new());
        assert_ne!(a, b);
    }

    #[test]
    fn validates_well_formed_ifc_guids() {
        // 22-char alphanumeric ± `_$` GUID is valid.
        assert!(is_valid_ifc_guid("1xS3BCk291UvhgP2a6eflL"));
        assert!(is_valid_ifc_guid("AAAAAAAAAAAAAAAAAAAAAA"));
        assert!(is_valid_ifc_guid("________$$$$$$$$$$$$$$"));
    }

    #[test]
    fn rejects_malformed_ifc_guids() {
        // ---- Wrong length (covers the len() == 22 short-circuit) ----
        assert!(!is_valid_ifc_guid(""));
        assert!(!is_valid_ifc_guid("short"));
        // 23 chars — one too long.
        assert!(!is_valid_ifc_guid("1xS3BCk291UvhgP2a6eflLZ"));
        // 21 chars — one too short.
        assert!(!is_valid_ifc_guid("1xS3BCk291UvhgP2a6efl"));

        // ---- Wrong charset at the right length ----
        //
        // Each of these strings is *exactly* 22 ASCII bytes long
        // (`len() == 22`) and replaces one valid char with a
        // STEP-hostile char. This exercises the charset branch of
        // `is_valid_ifc_guid`, not the length branch — the previous
        // version of these tests inserted extra characters which
        // made the strings 23-bytes and the length check rejected
        // them before the charset check ran.
        // Embedded quote — would break STEP tokenization.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg'2a6eflL"));
        // Embedded backslash — would break STEP escape handling.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg\\2a6eflL"));
        // Embedded comma — would break STEP arg-split.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg,2a6eflL"));
        // Embedded semicolon — STEP statement terminator.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg;2a6eflL"));
        // Embedded parenthesis — STEP arg-list delimiter.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg(2a6eflL"));
        // Embedded NUL. (Use `\x00` rather than `\0` so the next
        // char `2` doesn't get parsed as part of an octal escape.)
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg\x002a6eflL"));
        // Embedded space.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg 2a6eflL"));
        // ASCII exclamation — outside the 64-char IFC alphabet.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg!2a6eflL"));
        // Non-ASCII byte in a 22-byte string: `é` is 0xC3 0xA9 (two
        // UTF-8 bytes), so we replace TWO ASCII chars with one `é`
        // to keep the byte length at exactly 22 and force the
        // charset branch to see a non-ASCII byte. (If we instead
        // inserted `é` without removing any chars, the byte length
        // would be 23 and the length check would short-circuit
        // before the charset check ran.)
        let mut s_non_ascii = String::from("1xS3BCk291Uvh");
        s_non_ascii.push('é'); // 2 bytes
        s_non_ascii.push_str("2a6eflL"); // 7 bytes — total 13 + 2 + 7 = 22
        assert_eq!(s_non_ascii.len(), 22);
        assert!(!is_valid_ifc_guid(&s_non_ascii));
    }

    #[test]
    fn sanitize_passes_valid_guids_through() {
        let good = "1xS3BCk291UvhgP2a6eflL";
        assert_eq!(sanitize_ifc_guid(good, "ignored"), good);
    }

    #[test]
    fn sanitize_replaces_malformed_guids_with_deterministic_fallback() {
        let bad = "ev'il\\guid,";
        let a = sanitize_ifc_guid(bad, "seed-1");
        let b = sanitize_ifc_guid(bad, "seed-1");
        assert_eq!(a, b, "fallback is deterministic from seed");
        assert!(is_valid_ifc_guid(&a), "fallback is a valid IFC GUID");
        let c = sanitize_ifc_guid(bad, "seed-2");
        assert_ne!(a, c, "different seed produces different GUID");
    }
}
