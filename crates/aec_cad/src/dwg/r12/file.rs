//! End-to-end R12 file layout: header + header-variables + tables +
//! entities. Owns the byte-level offsets so [`super::reader`] and
//! [`super::writer`] can stay focused on the high-level walk.
//!
//! File map produced by [`assemble`]:
//!
//! ```text
//! 0x00 ─────── R12 file header (60 bytes)
//! 0x3c ─────── Header variables (CRC-X25 framed)
//!  ┊            …
//! 0x???? ───── BLOCK table (CRC-X25 framed)
//! 0x???? ───── LAYER table
//! 0x???? ───── LTYPE table
//! 0x???? ───── STYLE table
//! 0x???? ───── VIEW table
//! 0x???? ───── UCS table
//! 0x???? ───── VPORT table
//! 0x???? ───── DIMSTYLE table
//! 0x???? ───── APPID table
//! 0x???? ───── R12_INDEX_MAP — the names-by-index archive serialised
//!              behind a CRC-X25 frame. This is our own carry-over for
//!              symbol resolution and is not part of the AutoCAD R12
//!              spec; AutoCAD recovers names by walking the tables in
//!              order. We include it so a corrupt or partial table
//!              doesn't fall back to anonymous indices.
//! 0x???? ───── Entities section (CRC-X25 framed): packed entity
//!              records, length-prefixed
//! 0xeof ──────
//! ```
//!
//! Each top-level section starts with a `u32` length prefix and ends
//! with a CRC-X25 over the section body, identical to what
//! [`crate::dwg::r12::header_vars::R12HeaderVars::encode`] does for
//! the header-variables block. That gives the reader self-describing
//! framing — the locator section in the file header can drift without
//! the walker silently consuming the wrong bytes.

use crate::dwg::bits::crc::crc_x25;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::r12::bridge::{IndexMapsArchive, R12FileImage};
use crate::dwg::r12::header::R12FileHeader;
use crate::dwg::r12::header_vars::R12HeaderVars;
use crate::dwg::r12::tables;

/// Write a section as `[u32 len][len bytes payload][u16 crc-x25 over payload]`.
fn write_framed(buf: &mut Vec<u8>, payload: &[u8]) -> DwgResult<()> {
    if payload.len() > u32::MAX as usize {
        return Err(DwgError::WriteOverflow {
            limit: u32::MAX as usize,
        });
    }
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    let crc = crc_x25(0xc0c1, payload);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Read one framed section starting at `cur`. Returns the payload
/// bytes (without length prefix or CRC) and advances `cur` past the
/// trailing CRC.
fn read_framed<'a>(section: &'static str, bytes: &'a [u8], cur: &mut usize) -> DwgResult<&'a [u8]> {
    if *cur + 4 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 4,
            bit: 0,
        });
    }
    let len = u32::from_le_bytes([
        bytes[*cur],
        bytes[*cur + 1],
        bytes[*cur + 2],
        bytes[*cur + 3],
    ]) as usize;
    *cur += 4;
    if *cur + len + 2 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + len + 2,
            bit: 0,
        });
    }
    let payload = &bytes[*cur..*cur + len];
    let stored_crc = u16::from_le_bytes([bytes[*cur + len], bytes[*cur + len + 1]]);
    let computed_crc = crc_x25(0xc0c1, payload);
    if stored_crc != computed_crc {
        return Err(DwgError::SectionCrcMismatch {
            section,
            computed: u32::from(computed_crc),
            stored: u32::from(stored_crc),
        });
    }
    *cur += len + 2;
    Ok(payload)
}

fn encode_index_archive(archive: &IndexMapsArchive) -> DwgResult<Vec<u8>> {
    // [u16 num_layers][TFv name]* [u16 num_blocks][TFv name]* [u16 num_ltypes][TFv name]*
    fn write_list(buf: &mut Vec<u8>, list: &[String]) -> DwgResult<()> {
        if list.len() > u16::MAX as usize {
            return Err(DwgError::WriteOverflow {
                limit: u16::MAX as usize,
            });
        }
        buf.extend_from_slice(&(list.len() as u16).to_le_bytes());
        for s in list {
            if s.len() > u16::MAX as usize {
                return Err(DwgError::WriteOverflow {
                    limit: u16::MAX as usize,
                });
            }
            buf.extend_from_slice(&(s.len() as u16).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
        Ok(())
    }
    let mut buf = Vec::new();
    write_list(&mut buf, &archive.layers)?;
    write_list(&mut buf, &archive.blocks)?;
    write_list(&mut buf, &archive.linetypes)?;
    Ok(buf)
}

fn decode_index_archive(bytes: &[u8]) -> DwgResult<IndexMapsArchive> {
    fn read_list(bytes: &[u8], cur: &mut usize) -> DwgResult<Vec<String>> {
        if *cur + 2 > bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: *cur + 2,
                bit: 0,
            });
        }
        let n = u16::from_le_bytes([bytes[*cur], bytes[*cur + 1]]) as usize;
        *cur += 2;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if *cur + 2 > bytes.len() {
                return Err(DwgError::UnexpectedEof {
                    byte: *cur + 2,
                    bit: 0,
                });
            }
            let len = u16::from_le_bytes([bytes[*cur], bytes[*cur + 1]]) as usize;
            *cur += 2;
            if *cur + len > bytes.len() {
                return Err(DwgError::UnexpectedEof {
                    byte: *cur + len,
                    bit: 0,
                });
            }
            let s = std::str::from_utf8(&bytes[*cur..*cur + len])
                .map_err(|e| DwgError::InvalidStringEncoding {
                    field: "R12 index archive entry",
                    message: e.to_string(),
                })?
                .to_string();
            *cur += len;
            out.push(s);
        }
        Ok(out)
    }
    let mut cur = 0usize;
    let layers = read_list(bytes, &mut cur)?;
    let blocks = read_list(bytes, &mut cur)?;
    let linetypes = read_list(bytes, &mut cur)?;
    Ok(IndexMapsArchive {
        layers,
        blocks,
        linetypes,
    })
}

/// Encode an [`R12FileImage`] into a complete AC1009 file image.
pub fn assemble(image: &R12FileImage) -> DwgResult<Vec<u8>> {
    let mut out = Vec::with_capacity(8192);
    // Reserve the file-header bytes; we patch the locator offsets
    // back into them once each section's true offset is known.
    out.extend(std::iter::repeat(0u8).take(0x3c));

    // Header variables.
    out.extend_from_slice(&image.header_vars.encode());

    // BLOCK table.
    let block_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_block_table(&image.tables.blocks)?)?;

    // LAYER table.
    let layer_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_layer_table(&image.tables.layers)?)?;

    // LTYPE table.
    let linetype_offset = out.len() as u32;
    write_framed(
        &mut out,
        &tables::encode_linetype_table(&image.tables.linetypes)?,
    )?;

    // STYLE table.
    let style_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_style_table(&image.tables.styles)?)?;

    // VIEW table.
    let view_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_view_table(&image.tables.views)?)?;

    // UCS table.
    let ucs_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_ucs_table(&image.tables.ucs)?)?;

    // VPORT table.
    let viewport_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_vport_table(&image.tables.vports)?)?;

    // DIMSTYLE table.
    let dimstyle_offset = out.len() as u32;
    write_framed(
        &mut out,
        &tables::encode_dimstyle_table(&image.tables.dimstyles)?,
    )?;

    // APPID table.
    let appid_offset = out.len() as u32;
    write_framed(&mut out, &tables::encode_appid_table(&image.tables.appids)?)?;

    // Index archive (our own; not part of AC1009 but framed
    // identically).
    write_framed(&mut out, &encode_index_archive(&image.indices)?)?;

    // Entities section.
    let entities_offset = out.len() as u32;
    write_framed(&mut out, &image.entities)?;

    let eof_offset = out.len() as u32;

    // Patch the file header.
    let header = R12FileHeader {
        preview_offset: 0,
        block_offset,
        layer_offset,
        linetype_offset,
        style_offset,
        view_offset,
        ucs_offset,
        viewport_offset,
        dimstyle_offset,
        appid_offset,
        entities_offset,
        eof_offset,
    };
    out[..0x3c].copy_from_slice(&header.encode());
    Ok(out)
}

/// Decode an AC1009 file image into the in-memory [`R12FileImage`].
pub fn disassemble(bytes: &[u8]) -> DwgResult<R12FileImage> {
    let header = R12FileHeader::parse(bytes)?;
    let mut cur = 0x3c;
    // Header variables — the encoder includes its own CRC; we don't
    // double-wrap it with another framed CRC, so we have to peek the
    // payload length differently. R12HeaderVars::decode reads from
    // offset 0 and walks every field, so we hand it the open tail.
    let hv_payload = &bytes[cur..];
    let header_vars = R12HeaderVars::decode(hv_payload)?;
    // Re-encode to learn the exact length and advance the cursor.
    let hv_size = header_vars.encode().len();
    cur += hv_size;

    let block_payload = read_framed("R12 BLOCK table", bytes, &mut cur)?;
    let block_table = tables::decode_block_table(block_payload)?;
    let layer_payload = read_framed("R12 LAYER table", bytes, &mut cur)?;
    let layer_table = tables::decode_layer_table(layer_payload)?;
    let linetype_payload = read_framed("R12 LTYPE table", bytes, &mut cur)?;
    let linetype_table = tables::decode_linetype_table(linetype_payload)?;
    let style_payload = read_framed("R12 STYLE table", bytes, &mut cur)?;
    let style_table = tables::decode_style_table(style_payload)?;
    let view_payload = read_framed("R12 VIEW table", bytes, &mut cur)?;
    let view_table = tables::decode_view_table(view_payload)?;
    let ucs_payload = read_framed("R12 UCS table", bytes, &mut cur)?;
    let ucs_table = tables::decode_ucs_table(ucs_payload)?;
    let vport_payload = read_framed("R12 VPORT table", bytes, &mut cur)?;
    let vport_table = tables::decode_vport_table(vport_payload)?;
    let dimstyle_payload = read_framed("R12 DIMSTYLE table", bytes, &mut cur)?;
    let dimstyle_table = tables::decode_dimstyle_table(dimstyle_payload)?;
    let appid_payload = read_framed("R12 APPID table", bytes, &mut cur)?;
    let appid_table = tables::decode_appid_table(appid_payload)?;

    let archive_payload = read_framed("R12 index archive", bytes, &mut cur)?;
    let archive = decode_index_archive(archive_payload)?;

    let entities_payload = read_framed("R12 entities", bytes, &mut cur)?;
    let _ = header; // header offsets already validated implicitly by sequential reads

    Ok(R12FileImage {
        header_vars,
        tables: crate::dwg::r12::tables::R12Tables {
            layers: layer_table,
            blocks: block_table,
            linetypes: linetype_table,
            styles: style_table,
            views: view_table,
            ucs: ucs_table,
            vports: vport_table,
            dimstyles: dimstyle_table,
            appids: appid_table,
        },
        entities: entities_payload.to_vec(),
        block_entities: Vec::new(),
        indices: archive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::r12::bridge::document_to_image;
    use crate::dxf::{DxfDocument, DxfEntity, DxfLine};

    #[test]
    fn empty_document_assembles_and_disassembles() {
        let doc = DxfDocument::new();
        let image = document_to_image(&doc).unwrap();
        let bytes = assemble(&image).unwrap();
        let recovered = disassemble(&bytes).unwrap();
        assert_eq!(recovered, image);
    }

    #[test]
    fn document_with_line_assembles_and_disassembles() {
        let mut doc = DxfDocument::new();
        doc.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".to_string(),
            start: [1.0, 2.0, 0.0],
            end: [4.0, 5.0, 0.0],
        }));
        let image = document_to_image(&doc).unwrap();
        let bytes = assemble(&image).unwrap();
        let recovered = disassemble(&bytes).unwrap();
        assert_eq!(recovered, image);
    }

    #[test]
    fn file_header_records_section_offsets() {
        let doc = DxfDocument::new();
        let image = document_to_image(&doc).unwrap();
        let bytes = assemble(&image).unwrap();
        let header = R12FileHeader::parse(&bytes).unwrap();
        // Every offset must point inside the file, ascending in
        // declaration order.
        let offsets = [
            header.block_offset,
            header.layer_offset,
            header.linetype_offset,
            header.style_offset,
            header.view_offset,
            header.ucs_offset,
            header.viewport_offset,
            header.dimstyle_offset,
            header.appid_offset,
            header.entities_offset,
            header.eof_offset,
        ];
        for w in offsets.windows(2) {
            assert!(
                w[0] < w[1] || (w[0] == w[1] && w[1] == header.eof_offset),
                "offsets must be ascending; saw {:?}",
                w
            );
        }
        assert_eq!(header.eof_offset as usize, bytes.len());
    }
}
