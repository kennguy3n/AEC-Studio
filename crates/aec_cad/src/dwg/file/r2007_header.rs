//! R2007 file header — the 288-byte structure that lives at offset
//! 0x80 of every R2007+ DWG file, wrapped in a 32-byte metadata
//! block and Reed–Solomon encoded into 984 on-disk bytes.
//!
//! ## On-disk layout (0x80 .. 0x458)
//!
//! ```text
//! 0x080 ┌──────────────────────────────────────────────────────────┐
//!       │ Reed–Solomon interleaved buffer (984 bytes, 0x3d8)        │
//!       │                                                          │
//!       │ Logical contents after RS de-interleave (717 bytes):     │
//!       │   ┌──────────────────────────────────────────────────┐  │
//!       │   │ seqence_crc64 (8 bytes LE)                        │  │
//!       │   │ seqence_key64 (8 bytes LE)                        │  │
//!       │   │ compr_crc64   (8 bytes LE)                        │  │
//!       │   │ compr_len     (4 bytes LE, 0 = stored mode)       │  │
//!       │   │ len2          (4 bytes LE, decompressed length)   │  │
//!       │   ├──────────────────────────────────────────────────┤  │
//!       │   │ Dwg_R2007_Header (288 bytes, 36 × int64 LE)       │  │
//!       │   ├──────────────────────────────────────────────────┤  │
//!       │   │ zero padding to 717 bytes (= 3 * RS_DATA_SIZE)    │  │
//!       │   └──────────────────────────────────────────────────┘  │
//!       │ + 48 bytes of RS parity (3 blocks × 16 bytes)            │
//!       │ + 219 bytes of trailing zero pad (to 0x3d8)              │
//! 0x458 └──────────────────────────────────────────────────────────┘
//! ```
//!
//! Reference: LibreDWG `decode_r2007.c::read_file_header` (line 1184),
//! `Dwg_R2007_Header` struct in `include/dwg.h` line 9407.
//!
//! ## Stored vs compressed mode
//!
//! LibreDWG (line 1216-1221) chooses between two paths based on
//! `compr_len`:
//! - `compr_len > 0`: the bytes following the 32-byte metadata are
//!   LZ77-compressed (R2007 variant) and `decompress_r2007` is
//!   invoked.
//! - `compr_len == 0`: a plain `memcpy` of the next 288 bytes copies
//!   the header verbatim. **We use this stored-mode path** to avoid
//!   coupling the file-header writer to the R2007 LZ77 variant. The
//!   resulting file is bit-for-bit valid; only the on-disk file size
//!   is a few bytes larger than an AutoCAD-emitted file would be.
//!
//! ## CRC fields
//!
//! `seqence_crc64`, `seqence_key64`, `compr_crc64` are random-looking
//! 64-bit values that AutoCAD writes for integrity but LibreDWG
//! never validates (the only consumers are `LOG_TRACE` calls). We
//! emit zeros for forward compatibility — if a future LibreDWG
//! release starts validating them, we update the writer in one
//! place.

use crate::dwg::bits::reed_solomon::{
    rs_deinterleave, rs_encode_interleaved, R2007_FILE_HEADER_ON_DISK_SIZE, RS_DATA_SIZE,
};
use crate::dwg::error::{DwgError, DwgResult};

/// File offset where the R2007 file header begins (immediately after
/// the 0x80-byte legacy file header). Same offset as R2004.
pub const R2007_HEADER_OFFSET: usize = 0x80;

/// Logical size of `Dwg_R2007_Header` on disk (after LZ77 decompress,
/// before RS encoding). 34 fields × 8 bytes each = 272 bytes — the
/// exact value `sizeof(Dwg_R2007_Header)` evaluates to in LibreDWG
/// (`include/dwg.h:9407-9443`, under `#pragma pack(1)`), which is
/// what `read_file_header` passes to `memcpy(file_header, ...,
/// sizeof(Dwg_R2007_Header))` at decode_r2007.c:1221.
///
/// **Do not** add speculative "padding" trailers here. An earlier
/// version of this code carried two extra `padding1`/`padding2`
/// fields that pushed the logical size to 288 bytes; LibreDWG read
/// our header correctly anyway (its memcpy only consumes the first
/// 272 bytes, so the extras silently shifted into trailing pad of
/// the 717-byte pedata buffer), but ODA SDK and any decoder that
/// reads the on-disk format literally would mis-parse the next
/// section. Pinning the constant to LibreDWG's actual struct size
/// prevents that footgun.
pub const R2007_HEADER_LOGICAL_SIZE: usize = 34 * 8;

/// Size of the metadata wrapper prepended to the header before LZ77
/// + RS encoding. Layout: `seqence_crc(8) || seqence_key(8) ||
/// compr_crc(8) || compr_len(4) || len2(4)`.
pub const R2007_METADATA_SIZE: usize = 32;

/// Number of RS blocks used by the file-header buffer. Fixed at 3
/// per AutoCAD spec (LibreDWG `decode_rs(data, 3, 239, 0x3d8)`).
pub const R2007_HEADER_BLOCK_COUNT: usize = 3;

/// The 34-field `Dwg_R2007_Header` struct, mirroring LibreDWG's
/// `include/dwg.h:9407-9443` definition line-for-line under
/// `#pragma pack(1)`. All fields are 64-bit little-endian on disk.
///
/// Field-by-field documentation tracks LibreDWG's comments. Where a
/// field's purpose is "unknown" to LibreDWG itself, we propagate
/// that uncertainty in the doc comment rather than inventing
/// semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct R2007FileHeader {
    /// Always 0x70 in AutoCAD-emitted files.
    pub header_size: i64,
    /// Total file size. AutoCAD sets this to the actual size; LibreDWG
    /// only uses it for bounds checking (LOG_ERROR if > dat->size).
    pub file_size: i64,
    /// CRC of the compressed pages_map (LibreDWG-internal — never
    /// validated). We emit 0.
    pub pages_map_crc_compressed: i64,
    /// "Correction" value passed to `read_pages_map` — used by the
    /// reader's `repeat_count` calculation. AutoCAD typically writes
    /// 1 here; values > 1 indicate the page-map content is repeated.
    pub pages_map_correction: i64,
    /// Optional secondary seed for the pages-map checksum (unused by
    /// LibreDWG).
    pub pages_map_crc_seed: i64,
    /// Address of a fallback pages_map (zero if not used).
    pub pages_map2_offset: i64,
    /// ID of the secondary pages_map (zero if not used).
    pub pages_map2_id: i64,
    /// Starting address of the Page Map section, RELATIVE to byte
    /// (0x80 + 0x3d8 + 0x28) = 0x480. The reader does
    /// `dat->byte = 0x80 + 0x3d8 + 0x28 + pages_map_offset`.
    pub pages_map_offset: i64,
    /// ID of the pages_map page (AutoCAD writes 0).
    pub pages_map_id: i64,
    /// Address of the "second header" / aux file header. Zero if
    /// not emitted.
    pub header2_offset: i64,
    /// Compressed size of the pages_map system page (used by
    /// `read_system_page` for sizing).
    pub pages_map_size_comp: i64,
    /// Uncompressed (logical) size of the pages_map (used to bound
    /// the read).
    pub pages_map_size_uncomp: i64,
    /// Number of pages in the file (highest page id + 1).
    pub pages_amount: i64,
    /// Maximum page id in use (typically equal to `pages_amount`).
    pub pages_maxid: i64,
    /// Reserved field; LibreDWG comment says "0x20".
    pub unknown1: i64,
    /// Reserved field; LibreDWG comment says "0x40".
    pub unknown2: i64,
    /// CRC of the uncompressed pages_map (LibreDWG never validates).
    pub pages_map_crc_uncomp: i64,
    /// Reserved field; LibreDWG comment says "0xf800".
    pub unknown3: i64,
    /// Reserved field; LibreDWG comment says "4".
    pub unknown4: i64,
    /// Reserved field; LibreDWG comment says "1".
    pub unknown5: i64,
    /// Number of logical sections (AcDb:Header, AcDb:Classes, …).
    pub num_sections: i64,
    /// CRC of the uncompressed sections_map (unused by LibreDWG).
    pub sections_map_crc_uncomp: i64,
    /// Compressed size of the sections_map.
    pub sections_map_size_comp: i64,
    /// Fallback ID for the sections_map page (zero if not used).
    pub sections_map2_id: i64,
    /// ID of the sections_map page (the reader does
    /// `get_page(pages_map, sections_map_id)` to find its offset).
    pub sections_map_id: i64,
    /// Uncompressed size of the sections_map.
    pub sections_map_size_uncomp: i64,
    /// CRC of the compressed sections_map.
    pub sections_map_crc_comp: i64,
    /// "Correction" repeat count for the sections_map system page.
    pub sections_map_correction: i64,
    /// CRC seed for the sections_map (unused).
    pub sections_map_crc_seed: i64,
    /// Stream version constant. LibreDWG comment says `0x60100`.
    pub stream_version: i64,
    /// Random CRC seed (unused by LibreDWG).
    pub crc_seed: i64,
    /// Encoded form of `crc_seed` (unused).
    pub crc_seed_encoded: i64,
    /// Random seed (unused).
    pub random_seed: i64,
    /// CRC of the file header itself (unused — LibreDWG only logs).
    /// **This is the last `int64_t` in `Dwg_R2007_Header`** — the
    /// struct ends at `dwg.h:9442`, the closing brace is on 9443.
    /// Any field added after this must also exist in LibreDWG's
    /// struct (currently it does not) or it will be silently ignored
    /// by LibreDWG's `memcpy(...sizeof(Dwg_R2007_Header))` while
    /// breaking ODA SDK readers.
    pub header_crc: i64,
}

impl Default for R2007FileHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl R2007FileHeader {
    /// Construct a file header with AutoCAD-typical default values
    /// for the constant fields. Address/size fields default to zero
    /// and must be filled in by the file assembler once those
    /// values are known.
    pub const fn new() -> Self {
        Self {
            header_size: 0x70,
            file_size: 0,
            pages_map_crc_compressed: 0,
            pages_map_correction: 1,
            pages_map_crc_seed: 0,
            pages_map2_offset: 0,
            pages_map2_id: 0,
            pages_map_offset: 0,
            pages_map_id: 0,
            header2_offset: 0,
            pages_map_size_comp: 0,
            pages_map_size_uncomp: 0,
            pages_amount: 0,
            pages_maxid: 0,
            unknown1: 0x20,
            unknown2: 0x40,
            pages_map_crc_uncomp: 0,
            unknown3: 0xf800,
            unknown4: 4,
            unknown5: 1,
            num_sections: 0,
            sections_map_crc_uncomp: 0,
            sections_map_size_comp: 0,
            sections_map2_id: 0,
            sections_map_id: 0,
            sections_map_size_uncomp: 0,
            sections_map_crc_comp: 0,
            sections_map_correction: 1,
            sections_map_crc_seed: 0,
            stream_version: 0x60100,
            crc_seed: 0,
            crc_seed_encoded: 0,
            random_seed: 0,
            header_crc: 0,
        }
    }

    /// Serialize the 36-field struct as 288 bytes of little-endian
    /// `int64_t` values, in the exact order LibreDWG's
    /// `Dwg_R2007_Header` declares them.
    pub fn encode(&self) -> [u8; R2007_HEADER_LOGICAL_SIZE] {
        let fields = self.as_array();
        let mut out = [0u8; R2007_HEADER_LOGICAL_SIZE];
        for (i, &field) in fields.iter().enumerate() {
            out[i * 8..(i + 1) * 8].copy_from_slice(&field.to_le_bytes());
        }
        out
    }

    /// Inverse of [`encode`](Self::encode) — parse 288 bytes into a
    /// `R2007FileHeader`. Errors if the slice is too short.
    pub fn parse(bytes: &[u8]) -> DwgResult<Self> {
        if bytes.len() < R2007_HEADER_LOGICAL_SIZE {
            return Err(DwgError::InternalInvariant(format!(
                "R2007FileHeader::parse: need {} bytes, got {}",
                R2007_HEADER_LOGICAL_SIZE,
                bytes.len()
            )));
        }
        let mut fields = [0i64; 34];
        for (i, slot) in fields.iter_mut().enumerate() {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&bytes[i * 8..(i + 1) * 8]);
            *slot = i64::from_le_bytes(buf);
        }
        Ok(Self::from_array(&fields))
    }

    fn as_array(&self) -> [i64; 34] {
        [
            self.header_size,
            self.file_size,
            self.pages_map_crc_compressed,
            self.pages_map_correction,
            self.pages_map_crc_seed,
            self.pages_map2_offset,
            self.pages_map2_id,
            self.pages_map_offset,
            self.pages_map_id,
            self.header2_offset,
            self.pages_map_size_comp,
            self.pages_map_size_uncomp,
            self.pages_amount,
            self.pages_maxid,
            self.unknown1,
            self.unknown2,
            self.pages_map_crc_uncomp,
            self.unknown3,
            self.unknown4,
            self.unknown5,
            self.num_sections,
            self.sections_map_crc_uncomp,
            self.sections_map_size_comp,
            self.sections_map2_id,
            self.sections_map_id,
            self.sections_map_size_uncomp,
            self.sections_map_crc_comp,
            self.sections_map_correction,
            self.sections_map_crc_seed,
            self.stream_version,
            self.crc_seed,
            self.crc_seed_encoded,
            self.random_seed,
            self.header_crc,
        ]
    }

    fn from_array(fields: &[i64; 34]) -> Self {
        Self {
            header_size: fields[0],
            file_size: fields[1],
            pages_map_crc_compressed: fields[2],
            pages_map_correction: fields[3],
            pages_map_crc_seed: fields[4],
            pages_map2_offset: fields[5],
            pages_map2_id: fields[6],
            pages_map_offset: fields[7],
            pages_map_id: fields[8],
            header2_offset: fields[9],
            pages_map_size_comp: fields[10],
            pages_map_size_uncomp: fields[11],
            pages_amount: fields[12],
            pages_maxid: fields[13],
            unknown1: fields[14],
            unknown2: fields[15],
            pages_map_crc_uncomp: fields[16],
            unknown3: fields[17],
            unknown4: fields[18],
            unknown5: fields[19],
            num_sections: fields[20],
            sections_map_crc_uncomp: fields[21],
            sections_map_size_comp: fields[22],
            sections_map2_id: fields[23],
            sections_map_id: fields[24],
            sections_map_size_uncomp: fields[25],
            sections_map_crc_comp: fields[26],
            sections_map_correction: fields[27],
            sections_map_crc_seed: fields[28],
            stream_version: fields[29],
            crc_seed: fields[30],
            crc_seed_encoded: fields[31],
            random_seed: fields[32],
            header_crc: fields[33],
        }
    }
}

/// Wrap a 288-byte serialized `R2007FileHeader` with the 32-byte
/// metadata block, pad to 717 bytes (`3 * RS_DATA_SIZE`), Reed–Solomon
/// encode into 3 interleaved 255-byte codewords, and zero-pad the
/// final output to exactly [`R2007_FILE_HEADER_ON_DISK_SIZE`] bytes.
///
/// The returned buffer is what writes at byte 0x80 of the DWG file.
///
/// We use **stored-mode** (`compr_len = 0`, `len2 = 288`) — LibreDWG's
/// `read_file_header` then takes the `else` branch on line 1221 and
/// does a direct `memcpy` instead of invoking `decompress_r2007`.
/// See the module docstring for the rationale.
pub fn encode_file_header_on_disk(
    header: &R2007FileHeader,
) -> [u8; R2007_FILE_HEADER_ON_DISK_SIZE] {
    let header_bytes = header.encode();

    // 32-byte metadata block.
    let mut pedata = vec![0u8; R2007_HEADER_BLOCK_COUNT * RS_DATA_SIZE];
    // bytes 0..8: seqence_crc — emit 0 (LibreDWG never validates).
    // bytes 8..16: seqence_key — emit 0.
    // bytes 16..24: compr_crc — emit 0.
    // bytes 24..28: compr_len = 0 → stored mode.
    // bytes 28..32: len2 = 272 = decompressed size (sizeof(Dwg_R2007_Header)).
    pedata[28..32].copy_from_slice(&(R2007_HEADER_LOGICAL_SIZE as u32).to_le_bytes());

    // Header bytes follow the metadata.
    pedata[R2007_METADATA_SIZE..R2007_METADATA_SIZE + R2007_HEADER_LOGICAL_SIZE]
        .copy_from_slice(&header_bytes);

    // Remaining bytes of pedata are already zero (vec init).

    // RS encode into 3 × 255 = 765 bytes.
    let rs_buf = rs_encode_interleaved(&pedata, R2007_HEADER_BLOCK_COUNT);
    assert_eq!(rs_buf.len(), R2007_HEADER_BLOCK_COUNT * 255);

    // Pad to 984 bytes total.
    let mut on_disk = [0u8; R2007_FILE_HEADER_ON_DISK_SIZE];
    on_disk[..rs_buf.len()].copy_from_slice(&rs_buf);
    on_disk
}

/// Inverse of [`encode_file_header_on_disk`] — read 984 bytes, RS
/// de-interleave (data columns only, parity ignored — matches
/// LibreDWG), unwrap the 32-byte metadata, and parse the 288-byte
/// header struct.
///
/// Errors if the RS de-interleave fails or the metadata indicates a
/// compressed-mode header (which we don't write but should report
/// rather than silently mis-decode).
pub fn decode_file_header_on_disk(on_disk: &[u8]) -> DwgResult<R2007FileHeader> {
    if on_disk.len() < R2007_FILE_HEADER_ON_DISK_SIZE {
        return Err(DwgError::InternalInvariant(format!(
            "decode_file_header_on_disk: need {} bytes, got {}",
            R2007_FILE_HEADER_ON_DISK_SIZE,
            on_disk.len()
        )));
    }
    // RS de-interleave reads exactly `block_count * 255` bytes from
    // the start and returns the first `block_count * data_size`
    // bytes in block order (data columns, parity ignored).
    let pedata = rs_deinterleave(
        &on_disk[..R2007_HEADER_BLOCK_COUNT * 255],
        R2007_HEADER_BLOCK_COUNT,
        RS_DATA_SIZE,
    )
    .ok_or_else(|| {
        DwgError::InternalInvariant("rs_deinterleave failed on R2007 file header".into())
    })?;

    let mut compr_len_bytes = [0u8; 4];
    compr_len_bytes.copy_from_slice(&pedata[24..28]);
    let compr_len = u32::from_le_bytes(compr_len_bytes);
    if compr_len != 0 {
        return Err(DwgError::InternalInvariant(format!(
            "decode_file_header_on_disk: compressed mode not supported here (compr_len={compr_len})"
        )));
    }

    R2007FileHeader::parse(
        &pedata[R2007_METADATA_SIZE..R2007_METADATA_SIZE + R2007_HEADER_LOGICAL_SIZE],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constants_match_libredwg() {
        let h = R2007FileHeader::new();
        // These three constants come directly from LibreDWG's
        // `include/dwg.h` comments on the struct.
        assert_eq!(h.header_size, 0x70);
        assert_eq!(h.unknown1, 0x20);
        assert_eq!(h.unknown2, 0x40);
        assert_eq!(h.unknown3, 0xf800);
        assert_eq!(h.unknown4, 4);
        assert_eq!(h.unknown5, 1);
        assert_eq!(h.stream_version, 0x60100);
    }

    #[test]
    fn encoded_size_matches_libredwg_struct_size() {
        // `sizeof(Dwg_R2007_Header)` in LibreDWG's `include/dwg.h`
        // (lines 9407–9443, 34 packed `int64_t` fields) is 272.
        // `read_file_header` at decode_r2007.c:1221 calls
        // `memcpy(file_header, &pedata[32], sizeof(Dwg_R2007_Header))`
        // — anything we emit beyond byte 272 is silently dropped on
        // the LibreDWG side but would mis-shift on ODA SDK / any
        // decoder that walks the on-disk format literally. Pin both
        // values here so a future struct edit can't drift them apart.
        let bytes = R2007FileHeader::new().encode();
        assert_eq!(bytes.len(), R2007_HEADER_LOGICAL_SIZE);
        assert_eq!(R2007_HEADER_LOGICAL_SIZE, 272);
        assert_eq!(R2007_HEADER_LOGICAL_SIZE, 34 * std::mem::size_of::<i64>());
    }

    #[test]
    fn header_round_trips_through_encode_parse() {
        let mut h = R2007FileHeader::new();
        h.file_size = 0x1234_5678_9abc;
        h.pages_map_offset = 0x4000;
        h.pages_map_id = 42;
        h.num_sections = 27;
        h.sections_map_id = 99;
        let bytes = h.encode();
        let parsed = R2007FileHeader::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn header_parse_rejects_short_input() {
        let result = R2007FileHeader::parse(&[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn header_field_layout_matches_libredwg_order() {
        // Pin the first 8 bytes (header_size) and bytes at field index
        // 7 (pages_map_offset) so the on-disk byte order can't drift.
        let mut h = R2007FileHeader::new();
        h.pages_map_offset = 0x1122_3344;
        let bytes = h.encode();
        // header_size = 0x70 at offset 0
        assert_eq!(&bytes[0..8], &0x70_i64.to_le_bytes());
        // pages_map_offset = 0x11223344 at field index 7 (offset 56)
        assert_eq!(&bytes[56..64], &0x1122_3344_i64.to_le_bytes());
    }

    #[test]
    fn on_disk_buffer_is_exactly_984_bytes() {
        let h = R2007FileHeader::new();
        let on_disk = encode_file_header_on_disk(&h);
        assert_eq!(on_disk.len(), R2007_FILE_HEADER_ON_DISK_SIZE);
        assert_eq!(on_disk.len(), 0x3d8);
    }

    #[test]
    fn on_disk_header_round_trips() {
        let mut h = R2007FileHeader::new();
        h.file_size = 0xdead_beef_cafe;
        h.pages_map_offset = 0x1000;
        h.pages_map_id = 1;
        h.num_sections = 5;
        h.sections_map_id = 2;
        h.pages_amount = 3;
        h.pages_maxid = 3;
        let on_disk = encode_file_header_on_disk(&h);
        let recovered = decode_file_header_on_disk(&on_disk).unwrap();
        assert_eq!(recovered, h);
    }

    #[test]
    fn on_disk_trailing_padding_is_zero() {
        let h = R2007FileHeader::new();
        let on_disk = encode_file_header_on_disk(&h);
        // Bytes after the RS-encoded region (765..984) must be zero
        // padding — matches AutoCAD's output and is what LibreDWG
        // expects (though it ignores them).
        for (i, &b) in on_disk[3 * 255..].iter().enumerate() {
            assert_eq!(b, 0, "trailing pad byte at +{} = {:02x}", i, b);
        }
    }

    #[test]
    fn on_disk_compr_len_is_zero_for_stored_mode() {
        let h = R2007FileHeader::new();
        let on_disk = encode_file_header_on_disk(&h);
        // RS de-interleave to recover pedata.
        let pedata = rs_deinterleave(
            &on_disk[..R2007_HEADER_BLOCK_COUNT * 255],
            R2007_HEADER_BLOCK_COUNT,
            RS_DATA_SIZE,
        )
        .unwrap();
        // bytes 24..28 = compr_len, must be 0 (stored mode).
        assert_eq!(&pedata[24..28], &[0u8; 4]);
        // bytes 28..32 = len2, must be 272 (decompressed size,
        // `sizeof(Dwg_R2007_Header)` per dwg.h:9407-9443).
        assert_eq!(&pedata[28..32], &272u32.to_le_bytes());
    }

    /// Pin every field in `R2007FileHeader` against its byte offset
    /// in LibreDWG's `Dwg_R2007_Header` struct (include/dwg.h lines
    /// 9407–9443). A mismatch in even a single position swaps two
    /// header values silently and only surfaces when parsing real
    /// AutoCAD-produced files — too late. Every field is set to a
    /// distinguishable sentinel here and the encoded bytes are read
    /// back at the exact offset where LibreDWG's reader expects them.
    #[test]
    fn every_field_lands_at_libredwg_byte_offset() {
        let h = R2007FileHeader {
            header_size: 0x70,
            file_size: 0x01,
            pages_map_crc_compressed: 0x02,
            pages_map_correction: 0x03,
            pages_map_crc_seed: 0x04,
            pages_map2_offset: 0x05,
            pages_map2_id: 0x06,
            pages_map_offset: 0x07,
            pages_map_id: 0x08,
            header2_offset: 0x09,
            pages_map_size_comp: 0x0a,
            pages_map_size_uncomp: 0x0b,
            pages_amount: 0x0c,
            pages_maxid: 0x0d,
            unknown1: 0x0e,
            unknown2: 0x0f,
            pages_map_crc_uncomp: 0x10,
            unknown3: 0x11,
            unknown4: 0x12,
            unknown5: 0x13,
            num_sections: 0x14,
            sections_map_crc_uncomp: 0x15,
            sections_map_size_comp: 0x16,
            sections_map2_id: 0x17,
            sections_map_id: 0x18,
            sections_map_size_uncomp: 0x19,
            sections_map_crc_comp: 0x1a,
            sections_map_correction: 0x1b,
            sections_map_crc_seed: 0x1c,
            stream_version: 0x1d,
            crc_seed: 0x1e,
            crc_seed_encoded: 0x1f,
            random_seed: 0x20,
            header_crc: 0x21,
        };
        let bytes = h.encode();
        // Indices match LibreDWG's declaration order at dwg.h:9409.
        let expected: [(usize, i64, &str); 34] = [
            (0, 0x70, "header_size"),
            (1, 0x01, "file_size"),
            (2, 0x02, "pages_map_crc_compressed"),
            (3, 0x03, "pages_map_correction"),
            (4, 0x04, "pages_map_crc_seed"),
            (5, 0x05, "pages_map2_offset"),
            (6, 0x06, "pages_map2_id"),
            (7, 0x07, "pages_map_offset"),
            (8, 0x08, "pages_map_id"),
            (9, 0x09, "header2_offset"),
            (10, 0x0a, "pages_map_size_comp"),
            (11, 0x0b, "pages_map_size_uncomp"),
            (12, 0x0c, "pages_amount"),
            (13, 0x0d, "pages_maxid"),
            (14, 0x0e, "unknown1"),
            (15, 0x0f, "unknown2"),
            (16, 0x10, "pages_map_crc_uncomp"),
            (17, 0x11, "unknown3"),
            (18, 0x12, "unknown4"),
            (19, 0x13, "unknown5"),
            (20, 0x14, "num_sections"),
            (21, 0x15, "sections_map_crc_uncomp"),
            (22, 0x16, "sections_map_size_comp"),
            (23, 0x17, "sections_map2_id"),
            (24, 0x18, "sections_map_id"),
            (25, 0x19, "sections_map_size_uncomp"),
            (26, 0x1a, "sections_map_crc_comp"),
            (27, 0x1b, "sections_map_correction"),
            (28, 0x1c, "sections_map_crc_seed"),
            (29, 0x1d, "stream_version"),
            (30, 0x1e, "crc_seed"),
            (31, 0x1f, "crc_seed_encoded"),
            (32, 0x20, "random_seed"),
            (33, 0x21, "header_crc"),
        ];
        for (idx, value, name) in expected {
            let off = idx * 8;
            assert_eq!(
                &bytes[off..off + 8],
                &value.to_le_bytes(),
                "field {name} at offset {off:#x} did not match LibreDWG's dwg.h ordering"
            );
        }
    }
}
