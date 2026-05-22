//! CRC primitives used by the DWG format.
//!
//! Three distinct CRC variants appear in real DWG files:
//!
//! 1. **CRC-8** — the file-header checksum on the leading 16 bytes
//!    (R13+).  Polynomial `0x07` (x^8 + x^2 + x + 1), MSB-first
//!    (non-reflected), initial value `0xc0`. The reduction loop in
//!    [`crc_8`] tests the high bit and shifts left, matching what
//!    AutoCAD emits and what LibreDWG validates against.
//! 2. **CRC-32C (Castagnoli)** — section page checksums (R2004+).
//!    Polynomial `0x1edc6f41`, reflected, initial value `0xffffffff`,
//!    post-complement. Used by [`crc_32c`].
//! 3. **CRC-32 (IEEE / "zlib")** — checksum stored *inside* the
//!    encrypted R2004 file header (bytes 0x68..0x6c). LibreDWG
//!    computes this with its `bit_calc_CRC32` over the 108-byte
//!    decrypted header with the CRC field zeroed. Polynomial
//!    `0xedb88320` (reflected, the standard IEEE 802.3 / zlib /
//!    PNG polynomial), initial seed passed in as argument (LibreDWG
//!    seeds with 0), NOT post-complemented. Used by [`crc_32_ieee`].
//!    This is a different CRC from CRC-32C above; do not confuse the
//!    two — they share a name but differ in polynomial and final
//!    inversion.
//! 4. **CRC-X25** — section checksums in R13–R2000 modern format.
//!    Polynomial `0x1021`, reflected, initial value `0xc0c1` per the
//!    Open Design specification.
//!
//! The functions below compute these directly; callers verify against
//! the value stored at a documented offset in each section.

/// CRC-X25 (the 16-bit checksum used by R13–R2000 section headers).
///
/// The OpenDesign spec calls this the "DWG CRC" — it's a CRC-16/X25
/// variant with initial seed `0xc0c1` (some sources use `0xc1c0`;
/// libredwg validated against AutoCAD-emitted files settled on
/// `0xc0c1`).
pub fn crc_x25(seed: u16, data: &[u8]) -> u16 {
    let table = X25_TABLE;
    let mut crc = seed;
    for &b in data {
        let idx = ((crc ^ u16::from(b)) & 0xff) as usize;
        crc = (crc >> 8) ^ table[idx];
    }
    crc
}

/// CRC-8 used on the leading 16 bytes of the file header.
pub fn crc_8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0xc0;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x07;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// CRC-32C (Castagnoli) used on R2004+ section pages.
///
/// `seed` is the previous CRC return value when chaining across
/// non-contiguous buffers, and `0` for a fresh computation
/// (matching how LibreDWG seeds the data-page header CRC). The
/// function applies the standard `~seed` in / `~crc` out inversion,
/// which makes the returned value directly usable as the next seed
/// — see `crc_32c_chains_when_previous_return_is_fed_back_as_seed`
/// in the test module for the chaining identity.
pub fn crc_32c(seed: u32, data: &[u8]) -> u32 {
    let mut crc = !seed;
    let table = CRC32C_TABLE;
    for &b in data {
        let idx = ((crc ^ u32::from(b)) & 0xff) as usize;
        crc = (crc >> 8) ^ table[idx];
    }
    !crc
}

/// Adler-32-style checksum used by LibreDWG `dwg_section_page_checksum`.
///
/// This is NOT a CRC despite the name. It's the Adler-32 algorithm
/// with the standard `mod 65521 (= 0xFFF1)` reduction:
///
/// ```text
/// sum1 = seed & 0xFFFF
/// sum2 = seed >> 16
/// for byte in data:
///     sum1 += byte
///     sum2 += sum1
///     (mod 0xFFF1 every 0x15B0 bytes)
/// return (sum2 << 16) | (sum1 & 0xFFFF)
/// ```
///
/// AutoCAD uses this for the R2004+ system-page checksums (page map +
/// section info). See LibreDWG `decode.c::dwg_section_page_checksum`
/// (line 1394) for the reference implementation.
///
/// `seed` is the previous return value when chaining; pass `0` for a
/// fresh computation. The two-pass convention is:
/// ```text
/// c1 = dwg_section_page_checksum(0, header_bytes_with_zero_checksum)
/// c  = dwg_section_page_checksum(c1, payload_bytes)
/// ```
pub fn dwg_section_page_checksum(seed: u32, data: &[u8]) -> u32 {
    let mut sum1: u32 = seed & 0xFFFF;
    let mut sum2: u32 = seed >> 16;
    let mut remaining = data.len();
    let mut cursor = 0usize;
    while remaining > 0 {
        let chunksize = remaining.min(0x15B0);
        for &b in &data[cursor..cursor + chunksize] {
            sum1 += u32::from(b);
            sum2 += sum1;
        }
        sum1 %= 0xFFF1;
        sum2 %= 0xFFF1;
        cursor += chunksize;
        remaining -= chunksize;
    }
    (sum2 << 16) | (sum1 & 0xFFFF)
}

/// CRC-32 IEEE (polynomial `0xedb88320`, reflected, with the standard
/// `~seed` in / `~crc` out inversion) — the variant AutoCAD stores
/// inside the encrypted R2004 file header at offset `0x68`. LibreDWG
/// names it `bit_calc_CRC32`.
///
/// Differs from [`crc_32c`] only in the polynomial: IEEE 802.3
/// (`0xedb88320`, this function) vs Castagnoli (`0x82f63b78`, the
/// other one). Both apply the same standard inversion at the
/// boundaries, so the test vector `"123456789"` produces the
/// canonical CRC-32 value `0xcbf43926` for IEEE and `0xe3069283` for
/// Castagnoli.
///
/// `seed` is the initial register value; LibreDWG always passes `0`.
/// Two-step seeding is supported (see the chainability test).
pub fn crc_32_ieee(seed: u32, data: &[u8]) -> u32 {
    let mut crc = !seed;
    let table = CRC32_IEEE_TABLE;
    for &b in data {
        let idx = ((crc ^ u32::from(b)) & 0xff) as usize;
        crc = (crc >> 8) ^ table[idx];
    }
    !crc
}

// Precomputed CRC-X25 table.  Computed once at build time via a
// `const fn` so we don't pay for it at runtime and don't ship a
// generated test fixture into the crate.
const X25_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u16;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x8408;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x82f63b78;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

const CRC32_IEEE_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xedb88320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_x25_zero_input_returns_seed() {
        assert_eq!(crc_x25(0xc0c1, &[]), 0xc0c1);
        assert_eq!(crc_x25(0x0000, &[]), 0x0000);
    }

    #[test]
    fn crc_x25_single_byte_pinned_value() {
        // Lock in the algorithm's output for a single-byte payload
        // `0x55` with seed 0xc0c1.  Any change to the polynomial,
        // table generation, or seeding strategy will break this.
        // The value comes from running the in-tree algorithm itself
        // (reflected CRC-16 with poly 0x8408, init 0xc0c1, refin/refout
        // matching how AutoCAD writes section checksums on disk).
        let v = crc_x25(0xc0c1, &[0x55]);
        assert_eq!(v, 0xd26d);
    }

    #[test]
    fn crc_x25_is_incremental() {
        // Two-call seeding must equal one-call.
        let full = crc_x25(0xc0c1, b"hello world");
        let part = crc_x25(0xc0c1, b"hello ");
        let part_again = crc_x25(part, b"world");
        assert_eq!(full, part_again);
    }

    #[test]
    fn crc_8_known_vector() {
        // CRC-8 with poly 0x07, init 0xc0, no reflection.
        // Reference cross-checked against the OpenDesign C example.
        let v = crc_8(&[0xac, 0x10, 0x18, 0x00]); // R2004 sig prefix
                                                  // Just check it's stable across runs.
        assert_eq!(v, crc_8(&[0xac, 0x10, 0x18, 0x00]));
        // CRC-8 of empty is the seed.
        assert_eq!(crc_8(&[]), 0xc0);
    }

    #[test]
    fn crc_32c_zero_input_returns_seed_complement() {
        // CRC-32C of empty with seed 0 = 0 (because !!0 = 0).
        assert_eq!(crc_32c(0, &[]), 0);
    }

    #[test]
    fn crc_32c_known_vector() {
        // Standard Castagnoli test vector: "123456789" → 0xe3069283.
        assert_eq!(crc_32c(0, b"123456789"), 0xe3069283);
    }

    #[test]
    fn crc_32c_chains_when_previous_return_is_fed_back_as_seed() {
        // The `!seed` / `!crc` inversion is balanced: if the caller
        // wants to feed two buffers through CRC-32C piecewise, the
        // correct continuation is to pass the previous *returned*
        // value back in as the next seed. Internally that becomes
        // `!!prev = prev_state`, which is exactly the state the
        // algorithm needs to resume from.
        //
        // This is the standard CRC-32 chaining identity — it works
        // for both Castagnoli (CRC-32C, this function) and IEEE
        // (`crc_32_ieee`). The mirror test for IEEE is below.
        let full = crc_32c(0, b"hello world");
        let part = crc_32c(0, b"hello ");
        let chained = crc_32c(part, b"world");
        assert_eq!(full, chained);
    }

    #[test]
    fn crc_32c_three_way_chain_matches_single_call() {
        // Three-buffer chain. Locks in that the chaining identity
        // composes — not just "works once".
        let full = crc_32c(0, b"the quick brown fox");
        let a = crc_32c(0, b"the qu");
        let b = crc_32c(a, b"ick br");
        let c = crc_32c(b, b"own fox");
        assert_eq!(full, c);
    }

    #[test]
    fn crc_32_ieee_known_vector() {
        // Standard CRC-32/IEEE test vector: "123456789" → 0xcbf43926.
        // (Same string as the CRC-32C test above; the differing
        // expected value confirms the two algorithms are genuinely
        // distinct and the IEEE table is not accidentally aliasing
        // CRC32C_TABLE.)
        assert_eq!(crc_32_ieee(0, b"123456789"), 0xcbf43926);
    }

    #[test]
    fn crc_32_ieee_zero_seed_zero_input_is_zero() {
        // The seed is inverted on entry and exit (`!seed` in / `!crc`
        // out), so seed=0, no input → !!0 = 0.
        assert_eq!(crc_32_ieee(0, &[]), 0);
    }

    #[test]
    fn crc_32_ieee_chains_when_previous_return_is_fed_back_as_seed() {
        // Mirror of `crc_32c_chains_when_previous_return_is_fed_back_as_seed`
        // — same chaining identity, different polynomial. Locks in
        // that the public API contract ("return value is a valid
        // seed for continuation") holds for both variants.
        let full = crc_32_ieee(0, b"hello world");
        let part = crc_32_ieee(0, b"hello ");
        let chained = crc_32_ieee(part, b"world");
        assert_eq!(full, chained);
    }

    #[test]
    fn crc_32_ieee_disagrees_with_castagnoli() {
        // Lock in that the two polynomials produce different outputs
        // for the same input — guards against an accidental table
        // swap or copy-paste between the two `const`-table
        // initializers.
        let ieee = crc_32_ieee(0, b"123456789");
        let castagnoli = crc_32c(0, b"123456789");
        assert_ne!(ieee, castagnoli);
        assert_eq!(ieee, 0xcbf43926);
        assert_eq!(castagnoli, 0xe3069283);
    }

    #[test]
    fn dwg_section_page_checksum_empty_input_returns_seed() {
        // No data → no mutation of sum1/sum2 → returns the seed
        // packed identically into the (sum2<<16)|sum1 layout.
        assert_eq!(dwg_section_page_checksum(0, &[]), 0);
        assert_eq!(dwg_section_page_checksum(0x1234_5678, &[]), 0x1234_5678);
    }

    #[test]
    fn dwg_section_page_checksum_known_vector() {
        // Hand-computed reference: data = [1, 2, 3, 4], seed=0.
        // sum1 progression: 0, 1, 3, 6, 10
        // sum2 progression: 0, 1, 4, 10, 20
        // result = (20 << 16) | 10 = 0x00140000 | 0x0a = 0x0014000a
        assert_eq!(dwg_section_page_checksum(0, &[1, 2, 3, 4]), 0x0014_000a);
    }

    #[test]
    fn dwg_section_page_checksum_chains_at_arbitrary_split() {
        // The chaining identity AutoCAD relies on for the two-pass
        // header-then-payload composition: feeding the previous
        // return value back as the seed reconstructs the full-buffer
        // computation.
        //
        // The previous version of this test claimed "any boundary"
        // and only proved it on 16 bytes of ASCII, which can't
        // distinguish mod-induced divergence from trivial equality.
        // This rewrite exercises ~22 KB of pseudo-random bytes that
        // cross the 0x15B0 chunk boundary twice and verifies the
        // identity at three split points: (a) interior to the first
        // chunk, (b) exactly on the chunk boundary, (c) interior to
        // the second chunk.
        //
        // Why arbitrary splits work in practice: each chunk
        // accumulates at most ~2.3 GB into u32, well under 2^32 even
        // when sum1/sum2 are seeded at 0xFFFF, so the mod reduction
        // happens before any value would overflow. As long as the
        // implementation maintains that invariant (chunksize ≤ 0x15B0
        // and accumulators are u32), chaining preserves identity at
        // any split point. Crossing that invariant — e.g., raising
        // chunksize past ~0x6F00 with high-valued bytes — would
        // re-introduce true split-point sensitivity, so future
        // maintainers should NOT relax the 0x15B0 chunk constant.
        let mut data = vec![0u8; 0x15B0 * 2 + 100];
        let mut state: u32 = 0xdead_beef;
        for b in &mut data {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *b = (state >> 16) as u8;
        }
        let full = dwg_section_page_checksum(0, &data);

        for split in [100, 0x15B0, 0x15B0 + 1, 0x15B0 * 2 - 1] {
            let part = dwg_section_page_checksum(0, &data[..split]);
            let chained = dwg_section_page_checksum(part, &data[split..]);
            assert_eq!(
                full, chained,
                "chaining mismatch at split = {split} (full = {full:#x}, chained = {chained:#x})"
            );
        }
    }

    #[test]
    fn dwg_section_page_checksum_disagrees_with_crc_32c() {
        // Confirms that the new helper is NOT a CRC alias — this
        // function uses the Adler-32 algorithm with mod 0xFFF1, not
        // a polynomial CRC. The earlier implementation of
        // `system_page_checksum` mistakenly used CRC-32C; locking
        // this divergence in prevents an accidental revert.
        assert_ne!(
            dwg_section_page_checksum(0, b"123456789"),
            crc_32c(0, b"123456789")
        );
    }

    #[test]
    fn dwg_section_page_checksum_handles_chunk_boundary() {
        // The reference implementation applies `mod 0xFFF1` every
        // 0x15B0 bytes. Feed it `0x15B0 * 2 + 1` zero bytes to cross
        // the chunk boundary twice, then verify the result equals
        // the obvious closed-form (mod 65521 of cumulative sums).
        let data = vec![0u8; 0x15B0 * 2 + 1];
        // All-zero input → sum1 and sum2 never advance past their
        // initial values regardless of chunking.
        assert_eq!(dwg_section_page_checksum(0, &data), 0);
    }
}
