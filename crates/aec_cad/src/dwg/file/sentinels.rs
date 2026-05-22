//! Section sentinel patterns used by the R13-R2000 modern DWG format.
//!
//! Every section in the legacy modern format (R13, R14, R2000) is
//! framed by two 16-byte sentinel patterns: a start sentinel
//! immediately before the section's bit-encoded payload, and an end
//! sentinel immediately after. The patterns are documented in the Open
//! Design Specification and cross-checked against LibreDWG's
//! `dwg.h`.
//!
//! The pairs come in two flavours:
//!
//! - **Forward / reverse** — each end sentinel is the byte-wise XOR of
//!   the corresponding start sentinel with `0xff`. This invariant is
//!   verified by [`is_valid_pair`].
//! - **Per-section identity** — the start sentinel is unique to each
//!   section, so a parser that has resynchronised after a checksum
//!   failure can still identify which section it just landed in.
//!
//! R2004+ (page-based) files do not use these sentinels — sections in
//! that format are framed by page headers + CRC32 instead.

/// 16-byte byte pattern delimiting a section in the R13-R2000 format.
pub type Sentinel = [u8; 16];

/// 16-byte sentinel written immediately after the locator-block CRC
/// and before the first section payload. LibreDWG's
/// `decode_R13_R2000` does a forward search for this pattern; it
/// confirms successful parse of the header + locator block.
///
/// Bytes taken from LibreDWG `common.c::dwg_sentinel`
/// (`DWG_SENTINEL_HEADER_END = 0`).
pub const HEADER_END: Sentinel = [
    0x95, 0xa0, 0x4e, 0x28, 0x99, 0x82, 0x1a, 0xe5, 0x5e, 0x41, 0xe0, 0x5f, 0x9d, 0x3a, 0x4d, 0x00,
];

/// Start sentinel for the *header variables* section.
pub const HEADER_VARS_BEGIN: Sentinel = [
    0xcf, 0x7b, 0x1f, 0x23, 0xfd, 0xde, 0x38, 0xa9, 0x5f, 0x7c, 0x68, 0xb8, 0x4e, 0x6d, 0x33, 0x5f,
];

/// End sentinel for the *header variables* section
/// (`HEADER_VARS_BEGIN ^ 0xff`).
pub const HEADER_VARS_END: Sentinel = xor_with_ff(HEADER_VARS_BEGIN);

/// Start sentinel for the *classes* section.
pub const CLASSES_BEGIN: Sentinel = [
    0x8d, 0xa1, 0xc4, 0xb8, 0xc4, 0xa9, 0xf8, 0xc5, 0xc0, 0xdc, 0xf4, 0x5f, 0xe7, 0xcf, 0xb6, 0x8a,
];

/// End sentinel for the *classes* section.
pub const CLASSES_END: Sentinel = xor_with_ff(CLASSES_BEGIN);

/// Start sentinel for the *second-header* block (R13-R2000 only).
pub const SECOND_HEADER_BEGIN: Sentinel = [
    0xd4, 0x7b, 0x21, 0xce, 0x28, 0x93, 0x9f, 0xbf, 0x53, 0x24, 0x40, 0x09, 0x12, 0x3c, 0xaa, 0x01,
];

/// End sentinel for the *second-header* block.
pub const SECOND_HEADER_END: Sentinel = xor_with_ff(SECOND_HEADER_BEGIN);

/// Validate that a `(begin, end)` pair is the byte-wise complement of
/// each other.
pub const fn is_valid_pair(begin: Sentinel, end: Sentinel) -> bool {
    let mut i = 0;
    while i < 16 {
        if begin[i] ^ end[i] != 0xff {
            return false;
        }
        i += 1;
    }
    true
}

const fn xor_with_ff(s: Sentinel) -> Sentinel {
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = s[i] ^ 0xff;
        i += 1;
    }
    out
}

/// Search `bytes` for the first occurrence of `pattern`, returning its
/// starting offset. Used by the resync recovery path when a CRC check
/// fails partway through a section.
pub fn find_sentinel(bytes: &[u8], pattern: &Sentinel) -> Option<usize> {
    bytes.windows(16).position(|w| w == pattern.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_vars_sentinels_are_complementary() {
        assert!(is_valid_pair(HEADER_VARS_BEGIN, HEADER_VARS_END));
    }

    #[test]
    fn classes_sentinels_are_complementary() {
        assert!(is_valid_pair(CLASSES_BEGIN, CLASSES_END));
    }

    #[test]
    fn second_header_sentinels_are_complementary() {
        assert!(is_valid_pair(SECOND_HEADER_BEGIN, SECOND_HEADER_END));
    }

    #[test]
    fn sentinels_have_distinct_first_byte() {
        // Quick uniqueness check — sanity guard against copy-paste mistakes
        // in the constants above.
        let begins = [
            HEADER_VARS_BEGIN[0],
            CLASSES_BEGIN[0],
            SECOND_HEADER_BEGIN[0],
        ];
        for i in 0..begins.len() {
            for j in 0..begins.len() {
                if i != j {
                    assert_ne!(begins[i], begins[j]);
                }
            }
        }
    }

    #[test]
    fn find_sentinel_finds_pattern_at_offset() {
        let mut buf = vec![0u8; 100];
        buf[42..58].copy_from_slice(&HEADER_VARS_BEGIN);
        assert_eq!(find_sentinel(&buf, &HEADER_VARS_BEGIN), Some(42));
    }

    #[test]
    fn find_sentinel_returns_none_when_absent() {
        let buf = vec![0u8; 100];
        assert_eq!(find_sentinel(&buf, &HEADER_VARS_BEGIN), None);
    }
}
