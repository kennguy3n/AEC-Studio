//! R2007 system-page wrapping (Reed–Solomon encoded).
//!
//! In R2007+ files, the pages-map and sections-map system pages
//! aren't the R2004-style 20-byte envelope + LZ77 + Adler-32 layout.
//! They're bare payload bytes (stored or LZ77-compressed), padded to
//! 8 bytes, then Reed–Solomon encoded with `block_count = ⌈pesize /
//! RS_DATA_SIZE⌉` blocks of 255 bytes each. The result is written
//! verbatim to the file at the offset the file header (or pages
//! map) specifies.
//!
//! Encoder API: [`encode_system_page`] takes the raw payload bytes
//! and emits the on-disk RS-encoded buffer plus the
//! `(size_comp, size_uncomp, repeat_count)` triple the file header
//! needs to refer to this page.
//!
//! Decoder API: [`decode_system_page`] is the inverse — reads
//! `page_size` bytes, RS de-interleaves, and returns the payload.
//!
//! Reference: LibreDWG `decode_r2007.c::read_system_page`
//! (line 597).

use crate::dwg::bits::reed_solomon::{
    rs_deinterleave, rs_encode_interleaved, RS_BLOCK_SIZE, RS_DATA_SIZE,
};
use crate::dwg::error::{DwgError, DwgResult};

/// Round `n` up to the next multiple of 8. R2007's RS-encoded
/// pages always align on 8-byte boundaries (per LibreDWG
/// `(size_comp + 7) & ~7`).
const fn round_up_8(n: usize) -> usize {
    (n + 7) & !7
}

/// Wire-format report from [`encode_system_page`].
///
/// `on_disk` is the byte stream that goes directly to the file at
/// the page's offset. `size_comp` and `size_uncomp` are the fields
/// the file header (or parent page map) stores so the reader can
/// reconstruct the same bounds. `repeat_count` is 1 in our writer
/// (AutoCAD also uses 1 in practice; values > 1 are an unused
/// AutoCAD feature for replicated pages).
#[derive(Debug, Clone)]
pub struct R2007SystemPageOnDisk {
    /// The exact byte stream to write at the page's file offset.
    /// Length is `block_count * RS_BLOCK_SIZE` rounded up to 8.
    pub on_disk: Vec<u8>,
    /// Compressed size — stored mode uses `size_comp == size_uncomp`.
    pub size_comp: i64,
    /// Uncompressed (logical) payload size.
    pub size_uncomp: i64,
    /// `repeat_count` — always 1 for our writer.
    pub repeat_count: i64,
}

/// RS-encode a system-page payload using stored mode (no LZ77
/// compression). Returns the on-disk bytes plus the size fields the
/// caller needs for the parent metadata.
///
/// "Stored mode" means `size_comp == size_uncomp` — when LibreDWG's
/// `read_system_page` sees that on line 669, it takes the `memcpy`
/// path instead of `decompress_r2007`. This skips the R2007 LZ77
/// variant entirely.
///
/// The on-disk layout:
/// 1. Pad `payload` to `pesize = round_up_8(payload.len())` with zeros.
/// 2. Pad `pesize` up to `block_count * RS_DATA_SIZE` with more zeros
///    (so we have an exact multiple of 239 bytes for RS).
/// 3. RS-encode → `block_count * RS_BLOCK_SIZE` bytes (765 for 3
///    blocks).
/// 4. The on-disk length must be a multiple of 8; the formula
///    `round_up_8(block_count * RS_BLOCK_SIZE)` is what LibreDWG
///    expects in `page_size` (line 633).
pub fn encode_system_page(payload: &[u8]) -> R2007SystemPageOnDisk {
    let size_uncomp = payload.len();
    let size_comp = size_uncomp; // stored mode
    let pesize_unrounded = round_up_8(size_comp);
    // Number of full RS data-blocks required to hold pesize bytes.
    let block_count = pesize_unrounded.div_ceil(RS_DATA_SIZE).max(1);
    let rs_input_len = block_count * RS_DATA_SIZE;

    let mut rs_input = vec![0u8; rs_input_len];
    rs_input[..payload.len()].copy_from_slice(payload);

    let rs_bytes = rs_encode_interleaved(&rs_input, block_count);
    assert_eq!(rs_bytes.len(), block_count * RS_BLOCK_SIZE);

    // page_size on disk is rs_bytes.len() rounded up to 8.
    let page_size = round_up_8(rs_bytes.len());
    let mut on_disk = vec![0u8; page_size];
    on_disk[..rs_bytes.len()].copy_from_slice(&rs_bytes);

    R2007SystemPageOnDisk {
        on_disk,
        size_comp: size_comp as i64,
        size_uncomp: size_uncomp as i64,
        repeat_count: 1,
    }
}

/// Inverse of [`encode_system_page`] — reads exactly `page_size`
/// bytes from the start of `src`, RS de-interleaves, and returns
/// `size_uncomp` bytes of payload.
///
/// `size_uncomp` and `size_comp` are the values the parent header
/// stored (and `size_comp` is normally equal to `size_uncomp` in
/// stored mode; if they differ, the caller must additionally
/// LZ77-decompress, which is not yet supported here).
pub fn decode_system_page(
    src: &[u8],
    size_comp: i64,
    size_uncomp: i64,
    repeat_count: i64,
) -> DwgResult<Vec<u8>> {
    if repeat_count < 1 {
        return Err(DwgError::InternalInvariant(format!(
            "decode_system_page: invalid repeat_count {repeat_count}"
        )));
    }
    if size_comp != size_uncomp {
        return Err(DwgError::InternalInvariant(
            "decode_system_page: compressed mode not supported (size_comp != size_uncomp)".into(),
        ));
    }
    let size_comp = size_comp as usize;
    let size_uncomp = size_uncomp as usize;
    let pesize_unrounded = round_up_8(size_comp) * repeat_count as usize;
    let block_count = pesize_unrounded.div_ceil(RS_DATA_SIZE).max(1);
    let page_size = round_up_8(block_count * RS_BLOCK_SIZE);
    if src.len() < page_size {
        return Err(DwgError::InternalInvariant(format!(
            "decode_system_page: src too short (need {page_size}, got {})",
            src.len()
        )));
    }
    let pedata = rs_deinterleave(
        &src[..block_count * RS_BLOCK_SIZE],
        block_count,
        RS_DATA_SIZE,
    )
    .ok_or_else(|| {
        DwgError::InternalInvariant("rs_deinterleave failed in decode_system_page".into())
    })?;
    Ok(pedata[..size_uncomp].to_vec())
}

/// Compute the on-disk page size for a given payload length — useful
/// when the caller needs to lay out file offsets before producing
/// the actual bytes.
pub fn system_page_on_disk_size(payload_len: usize) -> usize {
    let pesize_unrounded = round_up_8(payload_len);
    let block_count = pesize_unrounded.div_ceil(RS_DATA_SIZE).max(1);
    round_up_8(block_count * RS_BLOCK_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_payload_yields_one_block_page() {
        let result = encode_system_page(&[]);
        assert_eq!(result.size_comp, 0);
        assert_eq!(result.size_uncomp, 0);
        assert_eq!(result.repeat_count, 1);
        // Even empty payloads occupy one RS block (255 bytes,
        // rounded up to 256).
        assert_eq!(result.on_disk.len(), round_up_8(RS_BLOCK_SIZE));
        // The first 255 bytes are an RS codeword of all-zero data
        // (parity is also zero).
        assert_eq!(result.on_disk[..RS_DATA_SIZE], [0u8; RS_DATA_SIZE]);
    }

    #[test]
    fn payload_under_239_bytes_uses_one_block() {
        let payload = b"hello R2007";
        let result = encode_system_page(payload);
        assert_eq!(result.size_comp, payload.len() as i64);
        assert_eq!(result.size_uncomp, payload.len() as i64);
        assert_eq!(
            result.on_disk.len(),
            system_page_on_disk_size(payload.len())
        );
        // Round-trip the payload.
        let recovered = decode_system_page(
            &result.on_disk,
            result.size_comp,
            result.size_uncomp,
            result.repeat_count,
        )
        .unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn payload_exactly_239_uses_two_blocks_due_to_8_byte_round_up() {
        // 239 → round_up_8(239) = 240, then ceil(240/239) = 2 blocks.
        // The 8-byte alignment quirk pushes us into a second block
        // even though the payload itself fits in one. This matches
        // LibreDWG's `pesize = ((size_comp + 7) & ~7)` formula.
        let payload = vec![0x42u8; RS_DATA_SIZE];
        let result = encode_system_page(&payload);
        assert_eq!(result.on_disk.len(), round_up_8(2 * RS_BLOCK_SIZE));
        let recovered = decode_system_page(
            &result.on_disk,
            result.size_comp,
            result.size_uncomp,
            result.repeat_count,
        )
        .unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn payload_240_to_478_uses_two_blocks() {
        let payload = vec![0x37u8; RS_DATA_SIZE + 1];
        let result = encode_system_page(&payload);
        assert_eq!(result.on_disk.len(), round_up_8(2 * RS_BLOCK_SIZE));
        let recovered = decode_system_page(
            &result.on_disk,
            result.size_comp,
            result.size_uncomp,
            result.repeat_count,
        )
        .unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn pseudo_random_round_trip_three_blocks() {
        let payload: Vec<u8> = (0..3 * RS_DATA_SIZE - 7)
            .map(|i| ((i.wrapping_mul(2654435761)) & 0xff) as u8)
            .collect();
        let result = encode_system_page(&payload);
        assert_eq!(result.on_disk.len(), round_up_8(3 * RS_BLOCK_SIZE));
        let recovered = decode_system_page(
            &result.on_disk,
            result.size_comp,
            result.size_uncomp,
            result.repeat_count,
        )
        .unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn decode_rejects_compressed_mode() {
        let result = encode_system_page(&[0u8; 100]);
        // Simulate caller passing mismatched size_comp != size_uncomp.
        let err = decode_system_page(
            &result.on_disk,
            50, // size_comp != size_uncomp
            100,
            1,
        );
        assert!(err.is_err());
    }

    #[test]
    fn decode_rejects_truncated_input() {
        let result = encode_system_page(&[0u8; 100]);
        let short = &result.on_disk[..result.on_disk.len() - 10];
        let err = decode_system_page(short, result.size_comp, result.size_uncomp, 1);
        assert!(err.is_err());
    }

    #[test]
    fn on_disk_size_helper_matches_encode() {
        for n in [0usize, 1, 16, 239, 240, 256, 717, 718, 4096] {
            let result = encode_system_page(&vec![0u8; n]);
            assert_eq!(result.on_disk.len(), system_page_on_disk_size(n), "n={n}");
        }
    }
}
