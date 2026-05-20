//! In-process IFC4 STEP serializer + parser.
//!
//! AEC Studio's primary IFC pipeline is the out-of-process IfcOpenShell
//! worker (`workers/ifc/`). However, for end-to-end tests and for the
//! "BIM Lite" export pack we ship a small, self-contained STEP writer
//! and reader that supports the subset of the IFC4 schema this codebase
//! actually needs:
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
//! Limitations (intentional): no geometry, no IFC inheritance beyond
//! the listed classes, no material library. Geometry handoff goes
//! through the worker; the Lite roundtrip is intended for schedule /
//! property / classification fidelity.

pub mod reader;
pub mod writer;

pub use reader::{IfcReadError, IfcReadResult, IfcReadStats, IfcReader};
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
    // Sliding 6-bit window over the 16 input bytes. `acc` is intentionally
    // `u32` (not `u64`) — the left-shift by 8 below relies on Rust's
    // defined wrap-on-overflow for `<<` so the top bits drop off
    // cleanly after each byte is fully consumed. The invariant we
    // maintain is `bits <= 14` at the top of the loop (it cycles
    // 0→8→2→10→4→12→6→0 across bytes), so the `acc >> bits` extraction
    // never reads beyond the live window: the bits we'd "lose" to
    // wrap-around have already been emitted into `out` on a prior
    // iteration. `& 0x3F` masks the 6 target bits regardless.
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
        // Wrong length.
        assert!(!is_valid_ifc_guid(""));
        assert!(!is_valid_ifc_guid("short"));
        assert!(!is_valid_ifc_guid("1xS3BCk291UvhgP2a6eflLZ"));
        // Embedded quote — would break STEP tokenization.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg'P2a6eflL"));
        // Embedded backslash — would break STEP escape handling.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg\\P2a6eflL"));
        // Embedded comma — would break STEP arg-split.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg,P2a6eflL"));
        // Embedded NUL.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhg\0P2a6eflL"));
        // Unicode.
        assert!(!is_valid_ifc_guid("1xS3BCk291Uvhgéa6eflLZZ"));
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
