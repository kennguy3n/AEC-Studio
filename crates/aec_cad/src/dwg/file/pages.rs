//! R2004+ page paging and LZ77-style decompression.
//!
//! AutoCAD's R2004+ system section uses a custom LZ77 variant for
//! compressed pages. The algorithm is documented in the OpenDesign
//! spec § "System section pages — compression" and reverse-engineered
//! in LibreDWG's `decode_R2004_compress.c`.

use crate::dwg::error::{DwgError, DwgResult};

/// Decompress one R2004+ page's payload from `src` into a fresh
/// `Vec<u8>` of size `expected_decompressed_size`.
///
/// This is a placeholder for the full LZ77 dispatcher. The complete
/// state machine handles a literal-prefix marker (0x11–0x20) plus a
/// 14-state compressed-token machine. It is gated behind R2004+ entity
/// codec work — see [`super::system_section::parse_descriptors`] for
/// the current dispatch policy.
pub fn decompress(_src: &[u8], expected_decompressed_size: usize) -> DwgResult<Vec<u8>> {
    Err(DwgError::InternalInvariant(format!(
        "R2004 LZ77 decompressor not yet wired (target size {expected_decompressed_size})"
    )))
}

/// Compress a payload using the inverse algorithm. The encoder is
/// allowed to emit an all-literal stream (no back-references); a
/// minimal but spec-compliant implementation lands alongside the
/// decoder. This signature pre-declares the API.
pub fn compress(src: &[u8]) -> DwgResult<Vec<u8>> {
    if src.is_empty() {
        return Ok(Vec::new());
    }
    Err(DwgError::InternalInvariant(
        "R2004 LZ77 compressor not yet wired".to_string(),
    ))
}

/// Decrypt R2018 handle pages by XORing each byte with the documented
/// magic mask. Pre-declared for the R2018 dispatch path.
pub fn xor_decrypt_handle_page(page: &mut [u8], offset: u64) {
    // Magic key is the constant `0x4164_4135 0x4d61_676963` cycled
    // through the page. The byte-order quirk is documented in
    // OpenDesign § "R2018 — encrypted handle pages".
    const KEY: [u8; 12] = [
        0x35, 0xa4, 0x64, 0x41, // "5\xa4dA"
        0x63, 0x69, 0x67, 0x61, // "ciga"
        0x4d, 0x61, 0x67, 0x69, // "Magi"
    ];
    for (i, b) in page.iter_mut().enumerate() {
        *b ^= KEY[(offset as usize + i) % KEY.len()];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_decrypt_is_involutive() {
        let original = b"hello, R2018 handle page".to_vec();
        let mut buf = original.clone();
        xor_decrypt_handle_page(&mut buf, 0);
        assert_ne!(buf, original); // it actually changed something
        xor_decrypt_handle_page(&mut buf, 0);
        assert_eq!(buf, original);
    }

    #[test]
    fn xor_decrypt_uses_offset_in_key_cycle() {
        // Same payload at different offsets must produce different
        // ciphertexts (offset modulates the key cycle position).
        let a_payload = b"AAAAAAAAAAAA".to_vec();
        let mut a = a_payload.clone();
        let mut b = a_payload.clone();
        xor_decrypt_handle_page(&mut a, 0);
        xor_decrypt_handle_page(&mut b, 3);
        assert_ne!(a, b);
    }

    #[test]
    fn compress_empty_is_empty() {
        assert!(compress(b"").unwrap().is_empty());
    }
}
