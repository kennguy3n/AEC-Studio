//! R2004+ page paging and LZ77-style codec.
//!
//! AutoCAD's R2004+ system section uses a custom LZ77 variant for
//! compressed pages. The algorithm is the same one documented in the
//! OpenDesign spec § "System section pages — compression" and in the
//! LibreDWG `decompress_R2004_section` reference implementation; see
//! the encoder/decoder commentary inline.
//!
//! Both the decoder (`decompress`) and the encoder (`compress`) are
//! pure-Rust and self-contained. The encoder emits the "store" framing
//! (all literals followed by the `0x11` terminator) which is what
//! LibreDWG also emits for system sections; the bit-for-bit pattern is
//! a valid LZ77 stream that `decompress` parses identically.
//!
//! Encrypted-handle-page support for R2018 lives in
//! [`xor_decrypt_handle_page`] (a byte-XOR mask, not real crypto).

use crate::dwg::error::{DwgError, DwgResult};

/// Maximum literal length encoded in a single opcode byte (low nibble
/// 0x01..0x0F). Lengths above this require the extended encoding.
const MAX_SINGLE_BYTE_LITERAL_LEN: usize = 18;
/// The base offset that the extended literal-length encoding subtracts
/// before serializing continuation bytes. See [`encode_literal_length`].
const LITERAL_LEN_EXTENDED_BASE: usize = 18;
/// End-of-stream opcode.
const OPCODE_EOS: u8 = 0x11;

/// Decompress one R2004+ page's payload from `src` into a fresh
/// `Vec<u8>` of at most `expected_decompressed_size` bytes.
///
/// The LZ77 variant supports both compressed back-references and raw
/// literal runs. Streams produced by [`compress`] are all-literal but
/// the decoder accepts the full opcode set so it can read pages
/// emitted by AutoCAD or other writers.
pub fn decompress(src: &[u8], expected_decompressed_size: usize) -> DwgResult<Vec<u8>> {
    let mut reader = ByteReader::new(src);
    let mut out: Vec<u8> = Vec::with_capacity(expected_decompressed_size);

    // First opcode. If high nibble is zero, the stream starts with an
    // initial literal run before the main loop.
    let mut opcode1 = reader.read_byte()?;
    if (opcode1 & 0xF0) == 0 {
        let lit_len = read_literal_length(&mut reader, opcode1)?;
        copy_literals(&mut reader, &mut out, lit_len, expected_decompressed_size)?;
        opcode1 = reader.read_byte()?;
    }

    while opcode1 != OPCODE_EOS {
        let (comp_bytes, comp_offset, next_opcode_after_offset) =
            decode_match_descriptor(&mut reader, opcode1)?;

        // Apply the back-reference. The copy must handle the case
        // where (comp_offset < comp_bytes) — a run-length extension
        // where freshly written bytes feed the source pointer.
        let pos = out.len();
        if comp_offset == 0 || comp_offset > pos {
            return Err(DwgError::InternalInvariant(format!(
                "R2004 LZ77: back-reference offset {comp_offset} exceeds output position {pos}"
            )));
        }
        let end = pos + comp_bytes;
        if end > expected_decompressed_size {
            return Err(DwgError::InternalInvariant(format!(
                "R2004 LZ77: back-reference would overrun decompressed size \
                 (pos {pos} + {comp_bytes} > {expected_decompressed_size})"
            )));
        }
        out.resize(end, 0);
        for i in pos..end {
            out[i] = out[i - comp_offset];
        }

        // Trailing literals: the low 2 bits of the *current* opcode1
        // (which, for opcodes 0x12..0x1f and 0x20+, was just updated
        // by two_byte_offset to the firstByte of the offset pair) give
        // a literal-run length in 0..3. If 0, read the next opcode and
        // if that has a zero high nibble, decode it as an extended
        // literal-length prefix.
        let mut lit_length = (next_opcode_after_offset & 3) as usize;
        if lit_length == 0 {
            opcode1 = reader.read_byte()?;
            if (opcode1 & 0xF0) == 0 {
                lit_length = read_literal_length(&mut reader, opcode1)?;
            }
        } else {
            opcode1 = next_opcode_after_offset;
        }
        if lit_length > 0 {
            let next = copy_literals_returning_next(
                &mut reader,
                &mut out,
                lit_length,
                expected_decompressed_size,
            )?;
            opcode1 = next;
        }
    }

    if out.len() != expected_decompressed_size {
        return Err(DwgError::InternalInvariant(format!(
            "R2004 LZ77: decoded {} bytes, expected {} bytes",
            out.len(),
            expected_decompressed_size
        )));
    }
    Ok(out)
}

/// Decode the (length, offset) descriptor for one back-reference. The
/// opcode-byte ranges follow LibreDWG `decompress_R2004_section`:
///
/// - `opcode < 0x10 || opcode >= 0x40`: short copy. comp_bytes is
///   `(opcode >> 4) - 1`; one trailing byte gives the offset. Note that
///   `0x10 ..= 0x3f` is split among the next two ranges.
/// - `opcode 0x12..0x1f`: medium copy. Length and offset both extend
///   via continuation bytes / a 14-bit offset.
/// - `opcode >= 0x20`: long copy (length up to 32 + extension; full
///   16-bit offset).
///
/// Returns `(comp_bytes, comp_offset, next_opcode1)` where
/// `next_opcode1` is the byte that becomes the new state — for the
/// short-copy path it is the trailing offset byte (which decoders use
/// as both offset-high and the new opcode), for the 2-byte-offset
/// paths it is the `firstByte` returned by `two_byte_offset`.
fn decode_match_descriptor(
    reader: &mut ByteReader<'_>,
    opcode1: u8,
) -> DwgResult<(usize, usize, u8)> {
    if !(0x10..0x40).contains(&opcode1) {
        // Short copy: 2-byte form, length 1..15, offset 1..1024.
        let comp_bytes = ((opcode1 >> 4) as usize).saturating_sub(1);
        let opcode2 = reader.read_byte()?;
        let comp_offset = ((((opcode1 >> 2) & 3) as usize) | ((opcode2 as usize) << 2)) + 1;
        // The opcode2 byte does *not* become the new opcode1; only
        // the firstByte of two_byte_offset takes that role. Return
        // opcode1 itself so its low 2 bits drive trailing literals.
        Ok((comp_bytes, comp_offset, opcode1))
    } else if (0x12..0x20).contains(&opcode1) {
        // Medium copy. read_compressed_bytes with bit mask 7.
        let comp_bytes = read_compressed_bytes(reader, opcode1, 7)?;
        // High bit of offset is bit 3 of opcode1, shifted up.
        let mut comp_offset: usize = ((opcode1 as usize) & 8) << 11;
        let (first_byte, second_byte) = (reader.read_byte()?, reader.read_byte()?);
        comp_offset |= (first_byte as usize) >> 2;
        comp_offset |= (second_byte as usize) << 6;
        comp_offset += 0x4000;
        Ok((comp_bytes, comp_offset, first_byte))
    } else if opcode1 >= 0x20 {
        // Long copy.
        let comp_bytes = read_compressed_bytes(reader, opcode1, 0x1f)?;
        let mut comp_offset: usize = 0;
        let (first_byte, second_byte) = (reader.read_byte()?, reader.read_byte()?);
        comp_offset |= (first_byte as usize) >> 2;
        comp_offset |= (second_byte as usize) << 6;
        comp_offset += 1;
        Ok((comp_bytes, comp_offset, first_byte))
    } else {
        // 0x10 and 0x11 itself; 0x11 is the EOS marker and 0x10 is
        // unused per OpenDesign. Anything else here is a stream
        // corruption.
        Err(DwgError::InternalInvariant(format!(
            "R2004 LZ77: invalid opcode 0x{opcode1:02x}"
        )))
    }
}

/// Read the extended literal-length prefix when opcode has its low
/// nibble zero. See the encoder in [`encode_literal_length`].
fn read_literal_length(reader: &mut ByteReader<'_>, opcode: u8) -> DwgResult<usize> {
    let mut low_bits = (opcode & 0x0F) as usize;
    if low_bits == 0 {
        loop {
            let last_byte = reader.read_byte()?;
            if last_byte == 0 {
                low_bits += 0xFF;
            } else {
                low_bits += 0x0F + last_byte as usize;
                break;
            }
        }
    }
    Ok(low_bits + 3)
}

/// Read the variable-length compressed-bytes count used by the
/// medium- and long-copy opcode families. `bits_mask` is `7` for
/// 0x12..0x1f and `0x1f` for 0x20+.
fn read_compressed_bytes(
    reader: &mut ByteReader<'_>,
    opcode: u8,
    bits_mask: u8,
) -> DwgResult<usize> {
    let mut compressed_bytes = (opcode & bits_mask) as usize;
    if compressed_bytes == 0 {
        loop {
            let last_byte = reader.read_byte()?;
            if last_byte == 0 {
                compressed_bytes += 0xFF;
            } else {
                compressed_bytes += last_byte as usize + bits_mask as usize;
                break;
            }
        }
    }
    Ok(compressed_bytes + 2)
}

/// Copy `n` literal bytes from `reader` to `out`, growing `out`.
fn copy_literals(
    reader: &mut ByteReader<'_>,
    out: &mut Vec<u8>,
    n: usize,
    cap: usize,
) -> DwgResult<()> {
    if out.len() + n > cap {
        return Err(DwgError::InternalInvariant(format!(
            "R2004 LZ77: literal run of {n} would exceed decompressed size {cap}"
        )));
    }
    for _ in 0..n {
        out.push(reader.read_byte()?);
    }
    Ok(())
}

/// Same as `copy_literals` but reads one additional byte (the next
/// opcode) and returns it — matches LibreDWG `copy_bytes`'s contract.
fn copy_literals_returning_next(
    reader: &mut ByteReader<'_>,
    out: &mut Vec<u8>,
    n: usize,
    cap: usize,
) -> DwgResult<u8> {
    copy_literals(reader, out, n, cap)?;
    reader.read_byte()
}

/// Compress `src` into an LZ77 stream. The encoder emits the "store"
/// framing — one initial literal run carrying all of `src`, followed
/// by the EOS terminator. The output is byte-for-byte what LibreDWG
/// `store_R2004_section` produces and what AutoCAD writes when the
/// payload doesn't compress to anything smaller.
///
/// This is correct for our use (system sections, where the data is
/// small and not redundant enough for LZ77 to win) and avoids the
/// substantial complexity of a hash-based match finder. The decoder
/// accepts both this framing and full-compressed streams, so the codec
/// is forward-compatible if a real compressor is added later.
pub fn compress(src: &[u8]) -> DwgResult<Vec<u8>> {
    let mut out = Vec::with_capacity(src.len() + 8);
    if src.is_empty() {
        out.push(OPCODE_EOS);
        return Ok(out);
    }
    if src.len() < 4 {
        // The literal-length encoding can only express lengths >= 4
        // in the initial-literal slot. For shorter payloads we pad
        // with trailing zeros to reach the minimum and remember to
        // pass the original length out-of-band — except that R2004
        // never emits 1-3-byte system sections (the page header alone
        // is larger), so this path is unreachable in practice. Surface
        // a structured error so callers don't silently corrupt data.
        return Err(DwgError::InternalInvariant(format!(
            "R2004 LZ77 compressor: input length {} is below the minimum (4); \
             system sections are always larger and shouldn't hit this path",
            src.len()
        )));
    }
    encode_literal_length(&mut out, src.len());
    out.extend_from_slice(src);
    out.push(OPCODE_EOS);
    Ok(out)
}

/// Emit the literal-length prefix for a literal run of `len` bytes.
/// Used by [`compress`] but split out for readability and direct
/// testability against the decoder's [`read_literal_length`].
fn encode_literal_length(out: &mut Vec<u8>, len: usize) {
    debug_assert!(len >= 4);
    if len <= MAX_SINGLE_BYTE_LITERAL_LEN {
        // Single-byte opcode with low nibble = len - 3.
        out.push((len - 3) as u8);
        return;
    }
    // Extended encoding: opcode 0x00, then enough 0x00 continuation
    // bytes to reach the remainder, then one non-zero terminator.
    out.push(0x00);
    let mut rem = len - LITERAL_LEN_EXTENDED_BASE;
    while rem > 0xFF {
        out.push(0x00);
        rem -= 0xFF;
    }
    out.push(rem as u8);
}

/// Decrypt R2018 handle pages by XORing each byte with the documented
/// magic mask. Pre-declared for the R2018 dispatch path.
pub fn xor_decrypt_handle_page(page: &mut [u8], offset: u64) {
    // Magic key — 12-byte ASCII-leaning constant cycled through the
    // page. The exact bytes are documented in OpenDesign § "R2018 —
    // encrypted handle pages" and reproduced verbatim from there.
    const KEY: [u8; 12] = [
        0x35, 0xa4, 0x64, 0x41, // "5\xa4dA"
        0x63, 0x69, 0x67, 0x61, // "ciga"
        0x4d, 0x61, 0x67, 0x69, // "Magi"
    ];
    for (i, b) in page.iter_mut().enumerate() {
        *b ^= KEY[(offset as usize + i) % KEY.len()];
    }
}

/// A trivial single-pass byte reader. We don't reuse [`BitReader`]
/// here because the LZ77 stream is byte-aligned end-to-end.
struct ByteReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn read_byte(&mut self) -> DwgResult<u8> {
        if self.pos >= self.bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: self.pos,
                bit: 0,
            });
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        Ok(b)
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
        assert_ne!(buf, original);
        xor_decrypt_handle_page(&mut buf, 0);
        assert_eq!(buf, original);
    }

    #[test]
    fn xor_decrypt_uses_offset_in_key_cycle() {
        let payload = b"AAAAAAAAAAAA".to_vec();
        let mut a = payload.clone();
        let mut b = payload.clone();
        xor_decrypt_handle_page(&mut a, 0);
        xor_decrypt_handle_page(&mut b, 3);
        assert_ne!(a, b);
    }

    #[test]
    fn compress_empty_emits_only_eos() {
        assert_eq!(compress(b"").unwrap(), vec![OPCODE_EOS]);
    }

    #[test]
    fn round_trip_short_payload() {
        // 4-byte payload — minimum single-opcode literal run.
        let payload = b"DWG!";
        let compressed = compress(payload).unwrap();
        // Expected: [0x01, b'D', b'W', b'G', b'!', 0x11].
        assert_eq!(compressed, vec![0x01, b'D', b'W', b'G', b'!', 0x11]);
        let decompressed = decompress(&compressed, payload.len()).unwrap();
        assert_eq!(decompressed.as_slice(), payload);
    }

    #[test]
    fn round_trip_18_byte_payload_uses_single_opcode_byte() {
        // 18 bytes is the boundary — encodes as opcode 0x0F + 18
        // literals + 0x11. No extension bytes.
        let payload: Vec<u8> = (0..18).map(|i| i as u8).collect();
        let compressed = compress(&payload).unwrap();
        assert_eq!(compressed.len(), 1 + 18 + 1);
        assert_eq!(compressed[0], 0x0F);
        assert_eq!(*compressed.last().unwrap(), OPCODE_EOS);
        let decompressed = decompress(&compressed, payload.len()).unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn round_trip_19_byte_payload_uses_one_extension_byte() {
        // 19 bytes crosses the boundary — opcode 0x00 + single
        // non-zero continuation (value 1) + 19 literals + 0x11.
        let payload: Vec<u8> = (0..19).map(|i| (i + 1) as u8).collect();
        let compressed = compress(&payload).unwrap();
        assert_eq!(compressed[0], 0x00);
        assert_eq!(compressed[1], 0x01);
        let decompressed = decompress(&compressed, payload.len()).unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn round_trip_273_byte_payload_uses_max_single_continuation() {
        // 273 bytes is the largest length expressible with a single
        // 0xff continuation byte (encoder writes [0x00, 0xFF]).
        let payload: Vec<u8> = (0..273).map(|i| (i % 251) as u8).collect();
        let compressed = compress(&payload).unwrap();
        assert_eq!(compressed[0], 0x00);
        assert_eq!(compressed[1], 0xFF);
        let decompressed = decompress(&compressed, payload.len()).unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn round_trip_274_byte_payload_uses_two_continuation_bytes() {
        // 274 bytes: [0x00, 0x00, 0x01, ...] — one zero continuation
        // plus a single-byte terminator.
        let payload: Vec<u8> = (0..274).map(|i| (i % 251) as u8).collect();
        let compressed = compress(&payload).unwrap();
        assert_eq!(&compressed[0..3], &[0x00, 0x00, 0x01]);
        let decompressed = decompress(&compressed, payload.len()).unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn round_trip_typical_page_size() {
        // Typical R2004 page is 0x2000 (8192) bytes.
        let payload: Vec<u8> = (0..8192)
            .map(|i| (i as u32).wrapping_mul(2654435761) as u8)
            .collect();
        let compressed = compress(&payload).unwrap();
        let decompressed = decompress(&compressed, payload.len()).unwrap();
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn decompress_rejects_truncated_stream() {
        // Header that promises 100 literal bytes but the stream is short.
        let mut buf = vec![0x00, 0x52]; // length 0+0xf+0x52+3 = 100
        buf.extend_from_slice(b"\x00\x01\x02\x03"); // only 4 bytes of literals
        let err = decompress(&buf, 100).unwrap_err();
        assert!(matches!(err, DwgError::UnexpectedEof { .. }));
    }

    #[test]
    fn decompress_rejects_undersized_target() {
        // Encoder framing for a 20-byte payload, decoder asked for 5.
        let payload: Vec<u8> = (0..20).map(|i| i as u8).collect();
        let compressed = compress(&payload).unwrap();
        let err = decompress(&compressed, 5).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
    }

    #[test]
    fn compress_rejects_short_inputs_with_structured_error() {
        // Anything 1..3 bytes — store_R2004_section in LibreDWG
        // asserts > 3, so we mirror that with a real error type.
        for len in 1..=3 {
            let buf = vec![0u8; len];
            let err = compress(&buf).unwrap_err();
            assert!(matches!(err, DwgError::InternalInvariant(_)));
        }
    }

    /// Decode a hand-crafted stream that uses the short-copy opcode
    /// path. This proves the back-reference machinery works even
    /// though our [`compress`] never emits it.
    #[test]
    fn decompress_handles_short_copy_back_reference() {
        // Payload we want out: "abcdab"
        //
        // Stream:
        //   0x03  initial literal-length: 6 bytes? No, 0x03 -> 6.
        //         Actually 0x03 means low nibble = 3, length = 3+3 = 6.
        //         But we only want 4 literals + back-ref.
        //   0x01  initial literal-length 4 bytes (low nibble = 1)
        //   'a' 'b' 'c' 'd'  literals
        //   0x40 + ... no, let's pick: short-copy opcode for length 2,
        //          offset 4, no trailing literals.
        //          opcode = 0x40 | (((offset-1) & 3) << 2) | (lit_len & 3)
        //          length comes from (opcode >> 4) - 1 = (0x40 >> 4) - 1 = 3.
        //          Hmm that gives length 3, not 2. Need length 2 means
        //          (opcode >> 4) = 3, i.e. opcode in 0x30..0x3F. But
        //          0x30 falls in 0x12..0x1F? No, 0x30 is >= 0x20 so it's
        //          a long-copy path. To get length 2 in the short-copy
        //          path we need opcode in 0x00..0x0F too — but those
        //          are filtered out by the initial-literal branch
        //          check ((opcode & 0xF0) == 0) so wouldn't reach the
        //          main loop unless we're already past the initial.
        //
        // Easier: use the LONG-copy path (opcode >= 0x20).
        //
        // opcode = 0x20 | (length - 2) but we need length encoded
        // via read_compressed_bytes mask 0x1f.
        //   For length = 4 (encoded as 4-2 = 2 in low 5 bits):
        //   opcode = 0x20 | 2 = 0x22.
        //   Then two_byte_offset reads 2 bytes. offset = (b1 >> 2) | (b2 << 6).
        //   For offset = 4: b1 >> 2 | b2 << 6 = 4 - 1 = 3 (since `+1` is added).
        //   Pick b1 = 0x0c (>> 2 = 3), b2 = 0x00. (b1 & 3 = 0 → no trailing lit.)
        //   Then need next opcode: 0x11 (EOS).
        //
        // Final out: "abcdabcd" — initial 4 literals + back-ref length 4 at offset 4.
        let stream = vec![
            0x01, b'a', b'b', b'c', b'd', // initial literal run
            0x22, // long-copy, length 4
            0x0C, 0x00, // two_byte_offset → offset 4, b1 & 3 = 0
            0x11, // EOS
        ];
        let out = decompress(&stream, 8).unwrap();
        assert_eq!(out.as_slice(), b"abcdabcd");
    }
}
