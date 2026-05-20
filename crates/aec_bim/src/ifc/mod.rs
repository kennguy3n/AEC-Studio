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

pub use reader::{IfcReadError, IfcReader, IfcReadResult, IfcReadStats};
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
    const ALPHABET: &[u8] =
        b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

    // Use BLAKE3 to project the seed into 16 bytes, then base64-ish
    // encode into 22 chars over the IFC alphabet. 16 bytes encodes into
    // ceil(16 * 8 / 6) = 22 chars — exactly the IFC GUID length.
    let hash = blake3::hash(seed.as_bytes());
    let raw = &hash.as_bytes()[..16];

    let mut out = String::with_capacity(22);
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
}
