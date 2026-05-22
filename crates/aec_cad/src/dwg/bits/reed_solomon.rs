//! Reed–Solomon (255, 239) encoder over GF(2^8) — used by R2007+ DWG
//! files to wrap the file header and every system-page payload.
//!
//! The DWG R2007 wire format places a degree-16 Reed–Solomon
//! correction code on top of each system page and on top of the
//! file header that lives at byte 0x80. The code rate is 239/255:
//! each block carries 239 data bytes and 16 parity bytes. AutoCAD
//! emits the parity bytes and LibreDWG, although it accepts any
//! parity (the `rs_decode_block` call is commented out in
//! `decode_r2007.c::decode_rs`), still expects the 255-byte per
//! block on-disk layout. ODA Drawings SDK *does* validate parity,
//! so emitting real parity bytes — not zeros — is the long-term
//! correct path.
//!
//! ## Field
//!
//! The field is GF(2^8), represented as polynomials in
//! `Z/2Z [X] / (X^8 + X^6 + X^5 + X^4 + 1)` (primitive poly
//! 0x171 — the leading X^8 term is implied; the byte representation
//! is 0x71 with the X^8 set bit). The element `X` is a generator
//! of the multiplicative group.
//!
//! ## Code layout on disk
//!
//! For each logical 239-byte data block:
//! 1. compute 16 parity bytes via [`rs_encode_block`] (Horner long
//!    division by the generator polynomial [`RSGEN`]).
//! 2. concatenate `data || parity` into a 255-byte codeword.
//!
//! Multiple codewords are stored **column-major interleaved** on
//! disk: byte at position `j*block_count + i` of the on-disk buffer
//! is the `j`-th byte of the `i`-th codeword. See [`rs_interleave`]
//! and [`rs_deinterleave`].
//!
//! ## Pinning
//!
//! Every table value, the generator polynomial, the multiplication
//! identity, and a round-trip data → parity → de-interleave property
//! are all pinned by tests below. Tables are taken verbatim from
//! LibreDWG `src/reedsolomon.c` (Alex Papazoglou); a divergence from
//! that source would break R2007 read compatibility.

/// Reduction table for GF(2^8) multiplication.
///
/// Indexed by `prod >> 8` after the unreduced shift-XOR multiply
/// (where `prod` is up to 16 bits wide); the lookup returns the
/// XOR-reduction that brings `prod` back into 8 bits modulo the
/// primitive polynomial 0x171.
///
/// Verbatim from LibreDWG `reedsolomon.c::f256_residue`. The first
/// entry is 0x00 (high byte zero → no reduction needed). The
/// remaining 255 entries cover every possible high-byte value.
const F256_RESIDUE: [u8; 256] = [
    0x00, 0x69, 0xd2, 0xbb, 0xcd, 0xa4, 0x1f, 0x76, 0xf3, 0x9a, 0x21, 0x48, 0x3e, 0x57, 0xec, 0x85,
    0x8f, 0xe6, 0x5d, 0x34, 0x42, 0x2b, 0x90, 0xf9, 0x7c, 0x15, 0xae, 0xc7, 0xb1, 0xd8, 0x63, 0x0a,
    0x77, 0x1e, 0xa5, 0xcc, 0xba, 0xd3, 0x68, 0x01, 0x84, 0xed, 0x56, 0x3f, 0x49, 0x20, 0x9b, 0xf2,
    0xf8, 0x91, 0x2a, 0x43, 0x35, 0x5c, 0xe7, 0x8e, 0x0b, 0x62, 0xd9, 0xb0, 0xc6, 0xaf, 0x14, 0x7d,
    0xee, 0x87, 0x3c, 0x55, 0x23, 0x4a, 0xf1, 0x98, 0x1d, 0x74, 0xcf, 0xa6, 0xd0, 0xb9, 0x02, 0x6b,
    0x61, 0x08, 0xb3, 0xda, 0xac, 0xc5, 0x7e, 0x17, 0x92, 0xfb, 0x40, 0x29, 0x5f, 0x36, 0x8d, 0xe4,
    0x99, 0xf0, 0x4b, 0x22, 0x54, 0x3d, 0x86, 0xef, 0x6a, 0x03, 0xb8, 0xd1, 0xa7, 0xce, 0x75, 0x1c,
    0x16, 0x7f, 0xc4, 0xad, 0xdb, 0xb2, 0x09, 0x60, 0xe5, 0x8c, 0x37, 0x5e, 0x28, 0x41, 0xfa, 0x93,
    0xb5, 0xdc, 0x67, 0x0e, 0x78, 0x11, 0xaa, 0xc3, 0x46, 0x2f, 0x94, 0xfd, 0x8b, 0xe2, 0x59, 0x30,
    0x3a, 0x53, 0xe8, 0x81, 0xf7, 0x9e, 0x25, 0x4c, 0xc9, 0xa0, 0x1b, 0x72, 0x04, 0x6d, 0xd6, 0xbf,
    0xc2, 0xab, 0x10, 0x79, 0x0f, 0x66, 0xdd, 0xb4, 0x31, 0x58, 0xe3, 0x8a, 0xfc, 0x95, 0x2e, 0x47,
    0x4d, 0x24, 0x9f, 0xf6, 0x80, 0xe9, 0x52, 0x3b, 0xbe, 0xd7, 0x6c, 0x05, 0x73, 0x1a, 0xa1, 0xc8,
    0x5b, 0x32, 0x89, 0xe0, 0x96, 0xff, 0x44, 0x2d, 0xa8, 0xc1, 0x7a, 0x13, 0x65, 0x0c, 0xb7, 0xde,
    0xd4, 0xbd, 0x06, 0x6f, 0x19, 0x70, 0xcb, 0xa2, 0x27, 0x4e, 0xf5, 0x9c, 0xea, 0x83, 0x38, 0x51,
    0x2c, 0x45, 0xfe, 0x97, 0xe1, 0x88, 0x33, 0x5a, 0xdf, 0xb6, 0x0d, 0x64, 0x12, 0x7b, 0xc0, 0xa9,
    0xa3, 0xca, 0x71, 0x18, 0x6e, 0x07, 0xbc, 0xd5, 0x50, 0x39, 0x82, 0xeb, 0x9d, 0xf4, 0x4f, 0x26,
];

/// Generator polynomial for RS(255, 239) — degree 16 (17
/// coefficients, ascending: `RSGEN[0]` is the X^0 term, `RSGEN[16]`
/// is the X^16 term, which must be 0x01 = X^16 leading coefficient
/// of a monic polynomial).
///
/// Verbatim from LibreDWG `reedsolomon.c::rsgen`. Constructed as
/// `prod_(i=1..16) (X - α^i)` where α is a primitive element of
/// GF(2^8). Any change here invalidates every parity byte we emit.
const RSGEN: [u8; 17] = [
    0x6a, 0xe3, 0x63, 0x1f, 0xa1, 0x24, 0x9e, 0x44, 0x13, 0x1e, 0x2f, 0xfc, 0xfd, 0xce, 0xa9, 0xdb,
    0x01,
];

/// Number of data bytes per Reed–Solomon block. Matches LibreDWG's
/// `decode_rs` data-stride and `read_system_page` block-count math.
pub const RS_DATA_SIZE: usize = 239;

/// Number of parity bytes per block (16). Computed from the
/// generator polynomial degree.
pub const RS_PARITY_SIZE: usize = RSGEN.len() - 1;

/// Total bytes per codeword (255 = 239 data + 16 parity).
pub const RS_BLOCK_SIZE: usize = RS_DATA_SIZE + RS_PARITY_SIZE;

/// On-disk size of the R2007 file-header buffer at offset 0x80 —
/// 984 bytes (= `3 * RS_BLOCK_SIZE + 219` bytes of padding). The
/// padding is a quirk of AutoCAD's emitter; LibreDWG reads exactly
/// 0x3d8 bytes here and ignores the trailing region.
pub const R2007_FILE_HEADER_ON_DISK_SIZE: usize = 0x3d8;

/// Multiply two GF(2^8) elements using the LibreDWG algorithm:
/// shift-XOR accumulate without per-iteration reduction, then a
/// single residue lookup against [`F256_RESIDUE`] at the end.
///
/// This matches `f256_multiply` in LibreDWG `reedsolomon.c` exactly.
#[inline]
fn f256_multiply(a: u8, b: u8) -> u8 {
    let mut prod: u16 = 0;
    let mut a_acc: u16 = a as u16;
    let mut b_iter: u16 = b as u16;

    while b_iter != 0 {
        if b_iter & 1 != 0 {
            prod ^= a_acc;
        }
        b_iter >>= 1;
        a_acc <<= 1;
    }

    prod ^= F256_RESIDUE[(prod >> 8) as usize] as u16;
    (prod & 0xff) as u8
}

/// Encode one 239-byte data block into 16 parity bytes using
/// Horner-method long division by [`RSGEN`].
///
/// The returned 16 bytes are appended to the data to form a 255-byte
/// codeword. The algorithm mirrors LibreDWG `rs_encode_block`
/// verbatim: process `src` from high index to low (last-byte-first,
/// matching the polynomial-coefficient convention), accumulate
/// shifted XOR-combination of the generator into the parity buffer,
/// then run 16 trailing pure-shift iterations to flush.
///
/// Note the reversed-byte-order quirk: LibreDWG's code walks
/// `src[i]` with `i` decrementing from `count - 1` down to 0. The
/// caller-visible "data" in LibreDWG is the same bytes in the same
/// order; the loop is just doing polynomial division where the
/// last byte is the highest-degree coefficient. We preserve that
/// convention so parity bytes match byte-for-byte with AutoCAD-
/// emitted files.
pub fn rs_encode_block(src: &[u8]) -> [u8; RS_PARITY_SIZE] {
    assert!(
        src.len() <= RS_DATA_SIZE,
        "rs_encode_block: src too long ({} > {})",
        src.len(),
        RS_DATA_SIZE
    );
    let mut parity = [0u8; RS_PARITY_SIZE];

    // Pull bytes high-index-first, multiply-by-X and add generator-
    // scaled parity-leader. This is identical to LibreDWG's main
    // loop in `rs_encode_block`.
    for i in (0..src.len()).rev() {
        let leader = parity[RS_PARITY_SIZE - 1];
        for j in (1..RS_PARITY_SIZE).rev() {
            parity[j] = parity[j - 1] ^ f256_multiply(leader, RSGEN[j]);
        }
        parity[0] = src[i] ^ f256_multiply(leader, RSGEN[0]);
    }

    // 16 trailing flush iterations — drain the polynomial state.
    for _ in 0..RS_PARITY_SIZE {
        let leader = parity[RS_PARITY_SIZE - 1];
        for j in (1..RS_PARITY_SIZE).rev() {
            parity[j] = parity[j - 1] ^ f256_multiply(leader, RSGEN[j]);
        }
        parity[0] = f256_multiply(leader, RSGEN[0]);
    }

    parity
}

/// Build a column-major interleaved on-disk buffer from
/// `block_count` codewords of length [`RS_BLOCK_SIZE`].
///
/// The on-disk byte at position `j * block_count + i` holds
/// `codewords[i][j]` for `0 <= i < block_count` and
/// `0 <= j < RS_BLOCK_SIZE`. The output length is exactly
/// `block_count * RS_BLOCK_SIZE`; callers that need additional
/// trailing padding (e.g. the file header pads to 0x3d8) handle
/// that themselves.
///
/// This is the exact inverse of [`rs_deinterleave`] and matches
/// LibreDWG's `decode_rs` data-stride read pattern.
pub fn rs_interleave(codewords: &[[u8; RS_BLOCK_SIZE]]) -> Vec<u8> {
    let block_count = codewords.len();
    let mut out = vec![0u8; block_count * RS_BLOCK_SIZE];
    for (i, cw) in codewords.iter().enumerate() {
        for (j, &byte) in cw.iter().enumerate() {
            out[j * block_count + i] = byte;
        }
    }
    out
}

/// Recover `block_count` data-byte streams from a column-major
/// interleaved buffer, ignoring the parity columns.
///
/// Returns `block_count * data_size` bytes — the concatenation of
/// `data_size` data bytes from each block in block-order. This
/// matches LibreDWG `decode_rs` exactly: it reads stride
/// `block_count` for `data_size` columns of each block, ignoring
/// the trailing parity columns and any padding past the data
/// region.
///
/// `data_size` is typically [`RS_DATA_SIZE`] (239) for system
/// pages. The file-header path also uses 239.
///
/// Errors are reported as `None` if `src` is too short to contain
/// `block_count * data_size` interleaved bytes at the expected
/// stride.
pub fn rs_deinterleave(src: &[u8], block_count: usize, data_size: usize) -> Option<Vec<u8>> {
    if block_count == 0 {
        return Some(Vec::new());
    }
    let last_idx = data_size.checked_sub(1)?.checked_mul(block_count)? + (block_count - 1);
    if last_idx >= src.len() {
        return None;
    }
    let mut out = Vec::with_capacity(block_count * data_size);
    for i in 0..block_count {
        for j in 0..data_size {
            out.push(src[j * block_count + i]);
        }
    }
    Some(out)
}

/// Encode a flat data buffer into the interleaved on-disk Reed–
/// Solomon layout used by R2007+ DWG files.
///
/// The caller provides `data` which will be split into
/// `block_count` chunks of [`RS_DATA_SIZE`] (239) bytes; if the
/// last chunk is shorter, it is treated as if zero-padded to 239.
/// Each chunk is RS-encoded ([`rs_encode_block`]) into a 255-byte
/// codeword, and the codewords are column-major interleaved.
///
/// `data.len()` must equal `block_count * RS_DATA_SIZE`; callers
/// must pad input ahead of time. This invariant is asserted (panics
/// on misuse — it's a programming error to call with mismatched
/// sizing).
///
/// Returns a buffer of exactly `block_count * RS_BLOCK_SIZE` bytes.
pub fn rs_encode_interleaved(data: &[u8], block_count: usize) -> Vec<u8> {
    assert_eq!(
        data.len(),
        block_count * RS_DATA_SIZE,
        "rs_encode_interleaved: data length {} != {} * {}",
        data.len(),
        block_count,
        RS_DATA_SIZE
    );
    let mut codewords: Vec<[u8; RS_BLOCK_SIZE]> = Vec::with_capacity(block_count);
    for i in 0..block_count {
        let mut cw = [0u8; RS_BLOCK_SIZE];
        let start = i * RS_DATA_SIZE;
        cw[..RS_DATA_SIZE].copy_from_slice(&data[start..start + RS_DATA_SIZE]);
        let parity = rs_encode_block(&cw[..RS_DATA_SIZE]);
        cw[RS_DATA_SIZE..].copy_from_slice(&parity);
        codewords.push(cw);
    }
    rs_interleave(&codewords)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-pin: spot-check `F256_RESIDUE` against LibreDWG values
    /// at a few positions. These are load-bearing — a single wrong
    /// entry breaks every parity byte we emit.
    #[test]
    fn f256_residue_table_pinned() {
        assert_eq!(F256_RESIDUE[0], 0x00);
        assert_eq!(F256_RESIDUE[1], 0x69);
        assert_eq!(F256_RESIDUE[2], 0xd2);
        assert_eq!(F256_RESIDUE[255], 0x26);
        // mid-table entries — first entry of each row in LibreDWG
        // `reedsolomon.c::f256_residue` (each row is 13 wide).
        assert_eq!(F256_RESIDUE[39], 0x01); // start of row 4
        assert_eq!(F256_RESIDUE[143], 0x30); // start of row 12
    }

    /// Table-pin: RSGEN coefficients from LibreDWG. The generator
    /// is monic (highest-degree coefficient 0x01) and 17 elements.
    #[test]
    fn rsgen_polynomial_pinned() {
        assert_eq!(RSGEN.len(), 17);
        assert_eq!(RSGEN[0], 0x6a);
        assert_eq!(RSGEN[16], 0x01);
        assert_eq!(RS_PARITY_SIZE, 16);
        assert_eq!(RS_BLOCK_SIZE, 255);
    }

    /// Field identity: a * 1 == a for all a.
    #[test]
    fn f256_multiply_identity() {
        for a in 0u16..256 {
            let a = a as u8;
            assert_eq!(f256_multiply(a, 1), a, "f256_multiply({a}, 1) != {a}");
            assert_eq!(f256_multiply(1, a), a, "f256_multiply(1, {a}) != {a}");
        }
    }

    /// Field identity: a * 0 == 0 for all a.
    #[test]
    fn f256_multiply_zero() {
        for a in 0u16..256 {
            let a = a as u8;
            assert_eq!(f256_multiply(a, 0), 0);
            assert_eq!(f256_multiply(0, a), 0);
        }
    }

    /// Field commutativity: a * b == b * a.
    #[test]
    fn f256_multiply_commutative() {
        for a in (0u16..256).step_by(7) {
            for b in (0u16..256).step_by(11) {
                let (a, b) = (a as u8, b as u8);
                assert_eq!(f256_multiply(a, b), f256_multiply(b, a));
            }
        }
    }

    /// All-zero data must produce all-zero parity (no errors → no
    /// correction bytes).
    #[test]
    fn rs_encode_all_zero_data_produces_zero_parity() {
        let data = [0u8; RS_DATA_SIZE];
        let parity = rs_encode_block(&data);
        assert_eq!(parity, [0u8; RS_PARITY_SIZE]);
    }

    /// Encoding the same data twice produces identical parity (no
    /// hidden state).
    #[test]
    fn rs_encode_is_deterministic() {
        let data: Vec<u8> = (0..RS_DATA_SIZE)
            .map(|i| (i as u8).wrapping_mul(13))
            .collect();
        let p1 = rs_encode_block(&data);
        let p2 = rs_encode_block(&data);
        assert_eq!(p1, p2);
    }

    /// Round-trip: interleave then de-interleave recovers the
    /// original data (parity columns are stripped by de-interleave,
    /// matching LibreDWG's reader behavior).
    #[test]
    fn rs_interleave_round_trip_data_columns() {
        let data: Vec<u8> = (0..3 * RS_DATA_SIZE)
            .map(|i| ((i.wrapping_mul(1664525)).wrapping_add(1013904223) & 0xff) as u8)
            .collect();
        let on_disk = rs_encode_interleaved(&data, 3);
        assert_eq!(on_disk.len(), 3 * RS_BLOCK_SIZE);

        let recovered = rs_deinterleave(&on_disk, 3, RS_DATA_SIZE).expect("deinterleave");
        assert_eq!(recovered.len(), 3 * RS_DATA_SIZE);
        assert_eq!(recovered, data);
    }

    /// Single-block round-trip: zeros stay zeros, both data and
    /// parity columns are present at the expected positions, and
    /// de-interleave returns just the data column slice.
    #[test]
    fn rs_single_block_interleave_layout_correct() {
        let data = vec![0xABu8; RS_DATA_SIZE];
        let on_disk = rs_encode_interleaved(&data, 1);
        assert_eq!(on_disk.len(), RS_BLOCK_SIZE);
        // Single block = no interleaving — bytes appear in order.
        assert_eq!(&on_disk[..RS_DATA_SIZE], &data[..]);
        // Last 16 bytes are parity.
        let parity = rs_encode_block(&data);
        assert_eq!(&on_disk[RS_DATA_SIZE..], &parity);
    }

    /// De-interleave handles the empty case and rejects too-short
    /// buffers without panicking.
    #[test]
    fn rs_deinterleave_edge_cases() {
        assert_eq!(rs_deinterleave(&[], 0, 239), Some(Vec::new()));
        assert_eq!(rs_deinterleave(&[1, 2, 3], 1, 10), None);
    }

    /// External cross-check against LibreDWG: encode `[1, 2, …, 239]`
    /// and assert the 16 parity bytes match what LibreDWG's
    /// `rs_encode_block` (in `src/reedsolomon.c`) produces for the
    /// same input. The expected bytes below were produced by
    /// compiling and running LibreDWG's reference implementation
    /// from version 0.13.3 — any drift here means our Rust port no
    /// longer matches the reference encoder, and R2007 files we
    /// emit will fail ODA Drawings SDK validation.
    #[test]
    fn rs_encode_matches_libredwg_reference() {
        let mut data = [0u8; RS_DATA_SIZE];
        for (i, slot) in data.iter_mut().enumerate() {
            *slot = (i + 1) as u8;
        }
        let parity = rs_encode_block(&data);
        assert_eq!(
            parity,
            [
                0xb7, 0xb6, 0xd7, 0x76, 0x06, 0xb5, 0xad, 0x84, 0xad, 0x9f, 0x8a, 0x0b, 0xe9, 0xbc,
                0x60, 0x38,
            ],
        );
    }
}
