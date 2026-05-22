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

/// Overflow-safe variant of [`round_up_8`] — returns `None` if
/// `n + 7` would wrap. Used on the hot path where `n` may come from
/// an untrusted file-header field that has already been validated as
/// non-negative but is otherwise unbounded.
const fn round_up_8_checked(n: usize) -> Option<usize> {
    match n.checked_add(7) {
        Some(v) => Some(v & !7),
        None => None,
    }
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
                                 // `pesize` is LibreDWG's name for the 8-byte-aligned post-compression
                                 // payload size that gets fed into the RS encoder. With repeat_count = 1
                                 // (always, for our writer) the pre-multiplication and post-multiplication
                                 // values are the same, so we only need one variable here. The
                                 // [`compute_page_size`] helper used by the decoder/sizing path keeps
                                 // both names because it has to handle repeat_count > 1.
    let pesize = round_up_8(size_comp);
    // Number of full RS data-blocks required to hold pesize bytes.
    let block_count = pesize.div_ceil(RS_DATA_SIZE).max(1);
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
    // Defense in depth: the public signature accepts `i64` because
    // those fields come straight from the wire format. A negative
    // value cast-to-usize wraps to a huge positive number that
    // would slip past the `src.len() < page_size` check below. The
    // existing production caller (`parse_r2007`) already validates
    // these are non-negative, but the function's own contract must
    // hold for any future caller — guard here so a third caller
    // can't accidentally bypass it.
    if size_comp < 0 || size_uncomp < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "decode_system_page: negative sizes (comp={size_comp}, uncomp={size_uncomp})"
        )));
    }
    if size_comp != size_uncomp {
        return Err(DwgError::InternalInvariant(
            "decode_system_page: compressed mode not supported (size_comp != size_uncomp)".into(),
        ));
    }
    let size_comp = size_comp as usize;
    let size_uncomp = size_uncomp as usize;
    let page_size = compute_page_size(size_comp, repeat_count)?;
    let block_count = page_size_to_block_count(page_size);
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

/// Compute the on-disk page size for a given payload length and
/// `repeat_count` — useful when the caller needs to lay out file
/// offsets before producing the actual bytes.
///
/// The `repeat_count` argument MUST match what the caller will pass
/// to [`decode_system_page`]; otherwise the bounds check the caller
/// builds on this value will under-allocate. `repeat_count` is
/// `header.pages_map_correction` in the R2007 file header context;
/// AutoCAD-emitted files always use 1, and so does our encoder.
///
/// Returns an error if `repeat_count < 1` or if any of the internal
/// arithmetic would overflow `usize` (e.g. an adversarial file with
/// `payload_len` near `usize::MAX / 2` and `repeat_count > 1`).
/// Returning a typed error rather than panicking lets the caller
/// propagate the rejection up to its parse-error path without
/// breaking the host process.
pub fn system_page_on_disk_size(payload_len: usize, repeat_count: i64) -> DwgResult<usize> {
    if repeat_count < 1 {
        return Err(DwgError::InternalInvariant(format!(
            "system_page_on_disk_size: repeat_count must be >= 1 (got {repeat_count})"
        )));
    }
    compute_page_size(payload_len, repeat_count)
}

/// Shared `compute pesize → block_count → page_size` pipeline used
/// by both [`system_page_on_disk_size`] and [`decode_system_page`].
/// Centralizing the math here guarantees the encoder and decoder
/// can never disagree on the on-disk size for a given
/// `(payload_len, repeat_count)` pair — that consistency is what
/// `parse_r2007`'s bounds check relies on.
fn compute_page_size(payload_len: usize, repeat_count: i64) -> DwgResult<usize> {
    // repeat_count is validated by the callers (>= 1) before we get
    // here, so the `as usize` cast is numerically safe. We still
    // checked_mul through every step to defeat adversarial
    // `payload_len` near `usize::MAX / 2`.
    let repeat = repeat_count as usize;
    // Matches LibreDWG `read_system_page` line 619:
    //     pesize = ((size_comp + 7) & ~7) * repeat_count;
    // We split it in two so each overflow gets its own typed error,
    // but the names track the LibreDWG variable: `pesize_aligned`
    // is one block's 8-byte-aligned payload size, `pesize` is the
    // total after repeat-count multiplication.
    let pesize_aligned = round_up_8_checked(payload_len).ok_or_else(|| {
        DwgError::InternalInvariant(format!(
            "system_page math: round_up_8({payload_len}) overflowed usize"
        ))
    })?;
    let pesize = pesize_aligned.checked_mul(repeat).ok_or_else(|| {
        DwgError::InternalInvariant(format!(
            "system_page math: pesize_aligned * repeat_count overflowed usize ({pesize_aligned} * {repeat})"
        ))
    })?;
    let block_count = pesize.div_ceil(RS_DATA_SIZE).max(1);
    let codeword_bytes = block_count.checked_mul(RS_BLOCK_SIZE).ok_or_else(|| {
        DwgError::InternalInvariant(format!(
            "system_page math: block_count * RS_BLOCK_SIZE overflowed usize ({block_count} * {RS_BLOCK_SIZE})"
        ))
    })?;
    round_up_8_checked(codeword_bytes).ok_or_else(|| {
        DwgError::InternalInvariant(format!(
            "system_page math: round_up_8({codeword_bytes}) overflowed usize"
        ))
    })
}

/// Inverse of [`compute_page_size`] on the codeword side — given the
/// final 8-byte-aligned page size, return how many 255-byte RS
/// codewords it contains.
///
/// ## Why the simple division works
///
/// [`compute_page_size`] produces
/// `page_size = round_up_8(block_count * RS_BLOCK_SIZE)`, which adds
/// at most 7 bytes of padding. Since `RS_BLOCK_SIZE = 255 > 7`, the
/// truncating integer division `page_size / 255` always recovers
/// the original `block_count` exactly — the padding can never push
/// the quotient up to the next integer. `compute_page_size` also
/// applies `.max(1)` to `block_count`, so `page_size >= 256`
/// (= 255 + 1) and the result here is always `>= 1`.
///
/// Pinned by [`page_size_to_block_count_is_inverse_of_compute_page_size`].
fn page_size_to_block_count(page_size: usize) -> usize {
    page_size / RS_BLOCK_SIZE
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
            system_page_on_disk_size(payload.len(), 1).unwrap()
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
            assert_eq!(
                result.on_disk.len(),
                system_page_on_disk_size(n, 1).unwrap(),
                "n={n}"
            );
        }
    }

    #[test]
    fn on_disk_size_rejects_zero_repeat_count() {
        let err = system_page_on_disk_size(100, 0);
        assert!(err.is_err(), "repeat_count = 0 must be rejected");
    }

    #[test]
    fn on_disk_size_rejects_negative_repeat_count() {
        let err = system_page_on_disk_size(100, -1);
        assert!(err.is_err(), "negative repeat_count must be rejected");
    }

    #[test]
    fn on_disk_size_rejects_overflow_inputs() {
        // Adversarial: payload_len near usize::MAX would overflow on
        // `round_up_8(payload_len)`. Result must be a clean Err,
        // never a panic or wrap.
        let err = system_page_on_disk_size(usize::MAX, 1);
        assert!(err.is_err());
        // Same shape via the repeat_count * pesize path. pick a
        // pesize that fits and a repeat_count that would overflow on
        // the multiplication.
        let err = system_page_on_disk_size(usize::MAX / 4, i64::MAX);
        assert!(err.is_err());
    }

    #[test]
    fn decode_system_page_rejects_negative_sizes() {
        let result = encode_system_page(&[0u8; 100]);
        let err = decode_system_page(&result.on_disk, -1, -1, 1);
        assert!(err.is_err(), "negative sizes must be rejected");
        // Also the mismatched-sign case.
        let err = decode_system_page(&result.on_disk, -100, 100, 1);
        assert!(err.is_err(), "negative size_comp must be rejected");
        let err = decode_system_page(&result.on_disk, 100, -100, 1);
        assert!(err.is_err(), "negative size_uncomp must be rejected");
    }

    #[test]
    fn page_size_to_block_count_is_inverse_of_compute_page_size() {
        // Pins the property that page_size / RS_BLOCK_SIZE always
        // recovers the block_count that compute_page_size produced.
        // Verified across the practical range plus the empty case:
        //   - payload_len = 0 → block_count = 1 (the .max(1) floor)
        //   - payload_len = 1, 8, 9, … crosses round_up_8 boundaries
        //   - payload_len at the 239-byte block boundary (and ±1, ±8)
        //   - payload_len up to RS_DATA_SIZE * 32 = 7648 bytes
        //
        // If anyone ever changes RS_BLOCK_SIZE or the padding scheme
        // such that page_size grows by >= RS_BLOCK_SIZE bytes beyond
        // block_count * RS_BLOCK_SIZE, this test fires before that
        // change can ship a decoder that under-reads.
        let mut payload_lens: Vec<usize> = Vec::new();
        payload_lens.extend([0, 1, 7, 8, 9, 15, 16, 17]);
        for n in 1..=32 {
            let boundary = n * RS_DATA_SIZE;
            payload_lens.extend(
                [
                    boundary.saturating_sub(8),
                    boundary.saturating_sub(1),
                    boundary,
                ]
                .iter()
                .copied(),
            );
            payload_lens.push(boundary + 1);
            payload_lens.push(boundary + 8);
        }
        for &payload_len in &payload_lens {
            for repeat_count in [1i64, 2, 3, 4, 8] {
                let expected_pesize_aligned = (payload_len + 7) & !7;
                let expected_pesize = expected_pesize_aligned * (repeat_count as usize);
                let expected_block_count = (expected_pesize.div_ceil(RS_DATA_SIZE)).max(1);
                let page_size =
                    compute_page_size(payload_len, repeat_count).expect("size should compute");
                let recovered = page_size_to_block_count(page_size);
                assert_eq!(
                    recovered, expected_block_count,
                    "block_count round-trip failed for payload_len={payload_len}, repeat_count={repeat_count} \
                     (page_size={page_size}, expected_block_count={expected_block_count}, recovered={recovered})"
                );
                assert!(
                    recovered >= 1,
                    "block_count must be >= 1 (got {recovered} for payload_len={payload_len})"
                );
            }
        }
    }
}
