//! End-to-end R12 (AC1009) file layout.
//!
//! This module is a thin adapter between the bridge-layer
//! [`R12FileImage`] (which is what [`crate::dwg::r12::writer`] and
//! [`crate::dwg::r12::reader`] hand around) and the wire-level
//! assembler in [`crate::dwg::r12::spec::assemble`].
//!
//! The wire format produced is AC1009 — the same byte stream
//! AutoCAD R12 and LibreDWG's `PRE(R_13b1)` codepath produce. See
//! [`crate::dwg::r12::spec::assemble`] for the per-region layout
//! diagram and the source-of-truth references into LibreDWG.
//!
//! PR-F2 scope: the assembler emits a *valid* AC1009 file with all
//! ten section tables empty. Block records, layer records, etc.
//! present in `image.tables.*` are not yet written to the wire; the
//! disassembler returns them empty on the read path. Entity
//! round-trip for the model-space layer "0" still works because the
//! [`bridge::image_to_document`] read path falls back to layer "0"
//! when the LAYER index map is empty (see
//! [`crate::dwg::r12::bridge::image_to_document`]).

use crate::dwg::error::DwgResult;
use crate::dwg::r12::bridge::{IndexMapsArchive, R12FileImage};
use crate::dwg::r12::spec::assemble::{
    assemble as spec_assemble, disassemble as spec_disassemble, R12Assembly,
};
use crate::dwg::r12::tables::R12Tables;

/// Encode an [`R12FileImage`] into a complete AC1009 file image.
///
/// The per-section tables on `image.tables.*` are intentionally not
/// yet emitted to the wire (PR-F2 scope). Entity bytes from
/// `image.entities` are emitted inside the sentinel-framed
/// `ENTITIES` region; the assembler computes the section locator,
/// the CRC at `entities_start - 18`, and the auxiliary header
/// trailing block.
pub fn assemble(image: &R12FileImage) -> DwgResult<Vec<u8>> {
    let assembly = R12Assembly {
        header_vars: image.header_vars.clone(),
        entities: image.entities.clone(),
        block_entities: Vec::new(),
        extras: Vec::new(),
    };
    spec_assemble(&assembly)
}

/// Decode an AC1009 file image into the in-memory [`R12FileImage`].
///
/// The read path is the mirror of [`assemble`]: it pulls the
/// sentinel-framed entity stream out of the file and hands the
/// bytes back wholesale. Per-section tables are returned empty
/// because the AC1009 wire format we currently emit leaves every
/// descriptor at `number = 0`; the LAYER/BLOCK/LTYPE name maps the
/// bridge needs on the read side fall back to defaults
/// (`"0"`/`"*MODEL_SPACE"`/`"CONTINUOUS"`).
pub fn disassemble(bytes: &[u8]) -> DwgResult<R12FileImage> {
    let dis = spec_disassemble(bytes)?;
    Ok(R12FileImage {
        header_vars: dis.header_vars,
        tables: R12Tables::default(),
        entities: dis.entities,
        block_entities: Vec::new(),
        indices: IndexMapsArchive::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::r12::bridge::document_to_image;
    use crate::dwg::r12::spec::file_header::R12FileHeader;
    use crate::dxf::{DxfDocument, DxfEntity, DxfLine};

    #[test]
    fn empty_document_round_trips_through_assembler() {
        let doc = DxfDocument::new();
        let image = document_to_image(&doc).unwrap();
        let bytes = assemble(&image).unwrap();
        let recovered = disassemble(&bytes).unwrap();
        // header_vars are reconstructed from the wire image; the
        // bridge fills them with defaults and they survive the
        // round trip.
        assert_eq!(recovered.header_vars, image.header_vars);
        assert_eq!(recovered.entities, image.entities);
    }

    #[test]
    fn document_with_line_round_trips_through_assembler() {
        let mut doc = DxfDocument::new();
        doc.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".to_string(),
            start: [1.0, 2.0, 0.0],
            end: [4.0, 5.0, 0.0],
        }));
        let image = document_to_image(&doc).unwrap();
        let bytes = assemble(&image).unwrap();
        let recovered = disassemble(&bytes).unwrap();
        // The entity byte stream is opaque to the assembler; what
        // goes in comes out byte-for-byte.
        assert_eq!(recovered.entities, image.entities);
    }

    #[test]
    fn file_header_locator_records_section_offsets() {
        let doc = DxfDocument::new();
        let image = document_to_image(&doc).unwrap();
        let bytes = assemble(&image).unwrap();
        let header = R12FileHeader::parse(&bytes).unwrap();
        // Locator offsets must be strictly ascending and inside the
        // file — empty entities region collapses to
        // entities_start == entities_end, but blocks/extras start
        // strictly after.
        assert!(header.locator.entities_start <= header.locator.entities_end);
        assert!(header.locator.entities_end < header.locator.blocks_start);
        assert!(header.locator.blocks_start < header.locator.extras_start);
        assert!((header.locator.extras_start as usize) < bytes.len());
        // blocks_size and extras_size both carry their version
        // flags (0x40000000 and 0x80000000 respectively).
        assert_eq!(header.locator.blocks_size & 0x4000_0000, 0x4000_0000);
        assert_eq!(header.locator.extras_size & 0x8000_0000, 0x8000_0000);
    }
}
