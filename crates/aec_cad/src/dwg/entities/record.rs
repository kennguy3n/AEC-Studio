//! Object record framing for R2000-R2018 DWG entities.
//!
//! An "object record" is the on-disk envelope that wraps one entity in
//! the OBJECTS / data section of a DWG file. Every record has this
//! shape, regardless of entity type:
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │ MS  object_size_in_bytes   (modular short, EXCLUDING this    │
//! │                              field, INCLUDING the CRC)       │
//! ├───────────────────────────────────────────────────────────────┤
//! │ Bit stream of length object_size_in_bytes - 2 (CRC) bytes:   │
//! │                                                              │
//! │   BS  object_type                                            │
//! │   [RL bitsize        — R2010+: total bit length of the      │
//! │                       data + handle streams, used to locate │
//! │                       where the handle stream starts.]       │
//! │   H   object_handle  (the entity's own handle, code = 0)     │
//! │   [EED — extended entity data, optional, terminated by 0 BS] │
//! │                                                              │
//! │   <common entity header data>     (from `header_codec`)      │
//! │   <per-type payload>              (from `line`, `arc`, …)    │
//! │                                                              │
//! │   <handle stream — owner H, reactors H..., xdict H, layer H, │
//! │    ltype H, plotstyle H, material H, etc.>                   │
//! ├───────────────────────────────────────────────────────────────┤
//! │ RS  CRC-X25 (over the bit stream bytes)                      │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! The data stream and handle stream are physically contiguous in
//! R2000-R2007 (handles follow data); R2010+ splits them logically by
//! the explicit `bitsize` field, allowing the handle stream to be
//! decoded out-of-order from the data stream. We model both shapes
//! through a single record type and pick the right framing at encode
//! time based on the target version.
//!
//! Reference: OpenDesign Specification § 19 "Objects map" and
//! LibreDWG's `decode.c::decode_R2004_section_objects`.

use crate::dwg::bits::reader::HandleRef;
use crate::dwg::bits::{crc_x25, BitReader, BitWriter};
use crate::dwg::entities::header_codec::CommonHeaderData;
use crate::dwg::entities::ObjectType;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::version::Version;

/// Whether a record carries an ENTITY (geometry: LINE, CIRCLE, TEXT, …)
/// or an OBJECT (non-geometric: LAYER, BLOCK_HEADER, DICTIONARY, …).
///
/// LibreDWG dispatches on `dwg_class[type_idx].is_entity` to pick which
/// codec runs `dwg_encode_entity` vs `dwg_encode_object`. The two share
/// the outer envelope (MS size, BS type, [RL bitsize for R2000-R2007],
/// H handle, EED) but differ on the common header in the data stream:
///
/// - **Entity** (`dwg_encode_entity`, `common_entity_data.spec`):
///   preview_exists, entmode, num_reactors, isbylayerlt/xdict_missing,
///   nolinks/has_ds_data, color, ltscale, ltype_flags, plotstyle_flags,
///   material_flags, shadow_flags, visualstyle flags, invisible, linewt.
///
/// - **Object** (`dwg_encode_object`, `encode.c:6540`):
///   num_reactors (BL), is_xdic_missing (B, R2004+), has_ds_data
///   (B, R2013+). No entity-specific flags.
///
/// The handle stream also differs: entities carry layer/ltype/prev/next
/// /material/shadow/plotstyle/visualstyle; objects carry only the
/// generic owner+reactors+xdict, plus per-type extras (e.g. LAYER's
/// xref+plotstyle+material+ltype+visualstyle, BLOCK_HEADER's
/// block_entity+first/last/owned/endblk/inserts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectSupertype {
    /// Geometric entity (LINE, CIRCLE, TEXT, INSERT, …). Default for
    /// back-compat: existing callers that constructed `ObjectRecord`
    /// without specifying a supertype get the entity codec.
    #[default]
    Entity,
    /// Non-geometric object (LAYER, LAYER_CONTROL, BLOCK_HEADER,
    /// BLOCK_CONTROL, DICTIONARY, …).
    Object,
}

/// One object record, decoded into typed parts (still carrying the
/// per-type payload as opaque bits so this module stays type-agnostic).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRecord {
    pub object_type: ObjectType,
    pub handle: HandleRef,
    /// Whether this record uses the entity or object common-header
    /// layout. See [`ObjectSupertype`].
    pub supertype: ObjectSupertype,
    pub common: CommonHeaderData,
    /// Object-supertype common header. Only used when `supertype ==
    /// Object`; ignored for entities (their flags live in `common`).
    pub object_common: ObjectCommonData,
    /// Type-specific payload bits, starting immediately after the
    /// common entity header data and ending immediately before the
    /// handle stream.
    pub payload_bits: BitBuf,
    /// R2007+ wide-string region (`T` fields) for OBJECT-supertype
    /// records. LibreDWG `obj_string_stream` reads `has_strings` at
    /// bit `bitsize - 1` and, when set, parses `data_size` (16 bits)
    /// at `bitsize - 17` followed by the string content immediately
    /// before that. We model that layout by appending these bits to
    /// the body just before the handle stream, followed by the
    /// `data_size` RS and `has_strings` B markers.
    ///
    /// Empty (`bit_len == 0`) means "no `T` fields", and the encoder
    /// writes `has_strings = 0` for R2007+ OBJECT records so the bit
    /// at `bitsize - 1` is deterministic. For pre-R2007 versions the
    /// string region is ignored — `T` fields encode inline as TV
    /// inside `payload_bits` instead.
    ///
    /// Currently consumed only by OBJECT-supertype emitters (LAYER,
    /// BLOCK_HEADER, etc.). ENTITY-supertype records still emit T
    /// fields inline in `payload_bits` regardless of version — the
    /// R2007+ string-stream layout for entities is a separate fix
    /// (tracked alongside the TEXT round-trip work).
    pub string_payload_bits: BitBuf,
    pub handles: ObjectHandles,
}

/// Object-supertype common header data, written immediately after the
/// EED terminator and the (R13/R14-only) inline RL bitsize, BEFORE the
/// per-type payload.
///
/// Source: LibreDWG `encode.c::dwg_encode_object` lines 6540-6550:
/// ```c
/// FIELD_BL (num_reactors, 0);
/// SINCE (R_2004a) { FIELD_B (is_xdic_missing, 0); }
/// SINCE (R_2013b) { FIELD_B (has_ds_data, 0); }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectCommonData {
    /// Number of reactor handles attached to this object (max 0x1000
    /// per LibreDWG `encode.c:1153`). For tables like LAYER, this is
    /// typically 0.
    pub num_reactors: u32,
    /// R2004+: when `true`, the xdict handle is NOT emitted in the
    /// handle stream. For empty/default tables we set this `true` to
    /// avoid emitting a dangling NULL xdict reference.
    pub is_xdic_missing: bool,
    /// R2013+: when `true`, an AcDs DATA chunk follows. We do not emit
    /// AcDs data, so this is always `false`.
    pub has_ds_data: bool,
}

/// The handle stream attached to every modern entity.
///
/// Field order matches LibreDWG `common_entity_handle_data.spec`. The
/// presence-bits for each optional handle are gated by the
/// corresponding flag in `CommonHeaderData`:
///
/// | Handle             | Gating flag                                  |
/// |--------------------|----------------------------------------------|
/// | `owner`            | `entity_mode == BlockHeader` (entmode==0)    |
/// | `reactors`         | count = `reactor_count`                      |
/// | `x_dictionary`     | pre-R2004: always; R2004+: `!xdict_missing`  |
/// | `layer`            | always                                       |
/// | `linetype`         | R14: `!isbylayerlt`; R2000+: `ltype_flag==3` |
/// | `prev_entity` /    | R14/R2000 only, when `!nolinks`              |
/// |   `next_entity`    |                                              |
/// | `material`         | R2007+, `material_flag==3`                   |
/// | `shadow`           | R2007+, `shadow_flags==3` (currently unused) |
/// | `plot_style`       | R2000+, `plot_style_flag==3`                 |
/// | `full_visualstyle` | R2010+, `has_full_visualstyle`               |
/// | `face_visualstyle` | R2010+, `has_face_visualstyle`               |
/// | `edge_visualstyle` | R2010+, `has_edge_visualstyle`               |
/// | `type_extras`      | per-entity-type, after the common stream     |
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectHandles {
    /// Owner handle (emitted only when `entity_mode == BlockHeader`,
    /// i.e. the LibreDWG `entmode == 0` case where the owner is
    /// stored explicitly rather than implied by entity mode).
    pub owner: Option<HandleRef>,
    /// Reactors (count must match `CommonHeaderData::reactor_count`).
    pub reactors: Vec<HandleRef>,
    /// Extension dictionary handle. For R13-R2002 the handle is
    /// ALWAYS present (no data-stream flag gates it); for R2004+ it
    /// is gated by `CommonHeaderData::xdict_missing`. When `None` on a
    /// pre-R2004 emit path the encoder writes a NULL handle
    /// (`{ code: 3, value: 0 }`) so the wire layout still has the slot
    /// — LibreDWG's decoder unconditionally reads it.
    pub x_dictionary: Option<HandleRef>,
    /// Layer handle (always present, code = 0x05 = "soft pointer").
    pub layer: HandleRef,
    /// Linetype handle. R14: emitted when `!isbylayerlt`; R2000+:
    /// emitted when `linetype_flag == Handle` (0b11).
    pub linetype: Option<HandleRef>,
    /// R14 / R2000 only: previous-entity link in the model-space
    /// chain. Emitted when `!nolinks`.
    pub prev_entity: Option<HandleRef>,
    /// R14 / R2000 only: next-entity link in the model-space chain.
    /// Emitted when `!nolinks`.
    pub next_entity: Option<HandleRef>,
    /// Material handle (R2007+; only when `material_flag == 0b11`).
    pub material: Option<HandleRef>,
    /// Shadow handle (R2007+; only when `shadow_flags == 3`).
    pub shadow: Option<HandleRef>,
    /// Plot-style handle (R2000+; only when `plot_style_flag == 0b11`).
    pub plot_style: Option<HandleRef>,
    /// Full visual-style handle (R2010+; only when `has_full_visualstyle`).
    pub full_visualstyle: Option<HandleRef>,
    /// Face visual-style handle (R2010+; only when `has_face_visualstyle`).
    pub face_visualstyle: Option<HandleRef>,
    /// Edge visual-style handle (R2010+; only when `has_edge_visualstyle`).
    pub edge_visualstyle: Option<HandleRef>,
    /// Per-entity-type extra handles, appended AFTER the common
    /// handle stream. The count and meaning are entity-type-specific:
    /// - TEXT / MTEXT / ATTRIB: `[style]` (soft pointer to AcDbStyle)
    /// - INSERT: `[block_header]` (and optionally seqend / attribs)
    /// - Most other entity types: empty
    ///
    /// The decoder uses
    /// [`type_extra_handle_count`] to know how many to read.
    pub type_extras: Vec<HandleRef>,
}

/// Opaque container for a slice of bit-stream payload, used to defer
/// per-type encoding/decoding to the entity codecs while keeping the
/// record framing type-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitBuf {
    pub bytes: Vec<u8>,
    /// Number of valid bits in the buffer (may be a non-multiple of 8;
    /// the trailing partial byte's low bits are zero-padded).
    pub bit_len: u64,
}

impl BitBuf {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit_len: 0,
        }
    }

    pub fn from_writer(w: BitWriter) -> Self {
        let bit_len = w.bit_position();
        let bytes = w.into_bytes();
        Self { bytes, bit_len }
    }

    pub fn into_reader(&self) -> BitReader<'_> {
        BitReader::new(&self.bytes)
    }
}

impl Default for BitBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectRecord {
    /// Encode this record to the wire format for `version`.
    ///
    /// Bit layout depends on the major version family:
    ///
    /// **R2000 .. R2007** (inline `bitsize`, no separate handle stream):
    ///
    /// ```text
    /// [address (byte-aligned):]
    /// MS  obj->size                (body byte count, NOT including MS or CRC)
    /// [obj->address (byte-aligned):]
    ///   BS  object_type            (2-18 bits)
    ///   RL  bitsize                (back-patched: body-bit position of handle stream)
    ///   H   handle
    ///   BS  EED-len = 0
    ///   common entity header
    ///   per-type payload
    ///   handle stream              (starts at body-bit position `bitsize`)
    ///   [pad B(0) to next byte]
    /// [end_address = obj->address + obj->size:]
    /// RS  crc                      (covers MS + body bytes; see encode.c:5867)
    /// ```
    ///
    /// **R2010+** (split data/handle streams, `handlestream_size` UMC
    /// emitted BEFORE the body so it is NOT counted in `obj->size`):
    ///
    /// ```text
    /// [address (byte-aligned):]
    /// MS  obj->size                (body byte count, NOT including MS, UMC, or CRC)
    /// UMC handlestream_size        (number of HANDLE-STREAM bits within the body)
    /// [obj->address (byte-aligned):]
    ///   BOT object_type            (2-18 bits)
    ///   common entity header / per-type payload / handle stream
    ///   [pad B(0) to next byte]
    /// [end_address = obj->address + obj->size:]
    /// RS  crc                      (covers MS + UMC + body bytes)
    /// ```
    ///
    /// The R2010+ decoder recovers the data-stream length via
    /// `bitsize = obj->size * 8 - handlestream_size` and uses
    /// `handlestream_size` to bound the handle-stream sub-reader.
    ///
    /// Source: LibreDWG `encode.c::dwg_encode_add_object` (line 5367)
    /// and `decode.c::read_objects` (line 5148). The CRC covers the
    /// MS + UMC + body bytes because LibreDWG seeds bit_check_CRC with
    /// the MS-start byte address (line 5574).
    pub fn encode(&self, version: Version) -> DwgResult<Vec<u8>> {
        // Stage 1: emit the body to a temp BitWriter so we can
        // measure its byte length (with end-of-stream byte padding
        // baked in). The body is what obj->size measures, and what
        // the handlestream_size UMC partitions.
        let mut body = BitWriter::new();
        if version >= Version::R2010 {
            body.write_bot(self.object_type as u16)?;
        } else {
            body.write_bs(self.object_type as u16 as i32)?;
        }

        // R2000..R2007 emit RL bitsize INLINE between the BS object_type
        // and the H handle (encode.c:6298). For R14 the RL bitsize is
        // embedded INSIDE common_entity_data after preview_exists; we
        // capture that slot below and back-patch it once the payload
        // ends. For R2010+ there is no inline RL: bitsize is derived
        // from `obj->size * 8 - handlestream_size` instead.
        let inline_rl_bitsize = (Version::R2000..=Version::R2007).contains(&version);
        let rl_bit_pos = body.bit_position();
        if inline_rl_bitsize {
            body.write_rl(0)?; // placeholder; back-patched below
        }

        body.write_h(self.handle)?;
        body.write_bs(0)?; // EED terminator (no EED yet)
        let r14_bitsize_slot = match self.supertype {
            ObjectSupertype::Entity => self
                .common
                .encode_for_version_with_r14_bitsize_slot(version, &mut body)?,
            ObjectSupertype::Object => {
                // For OBJECT supertype, R13/R14 still emit an inline RL
                // bitsize slot before the common-object-data block —
                // see `dwg_encode_object` at encode.c:6532:
                //   VERSIONS (R_13b1, R_14) {
                //     obj->bitsize_pos = bit_position (dat);
                //     FIELD_RL (bitsize, 0);
                //   }
                // We capture the slot position here so the caller can
                // back-patch it once `handle_stream_start_bit` is known
                // (same machinery used for entities).
                let slot = if version <= Version::R14 {
                    let pos = body.bit_position();
                    body.write_rl(0)?;
                    Some(pos)
                } else {
                    None
                };
                // BL num_reactors
                body.write_bl(i64::from(self.object_common.num_reactors))?;
                // B is_xdic_missing (R2004+)
                if version >= Version::R2004 {
                    body.write_b(self.object_common.is_xdic_missing)?;
                }
                // B has_ds_data (R2013+)
                if version >= Version::R2013 {
                    body.write_b(self.object_common.has_ds_data)?;
                }
                slot
            }
        };
        write_bitbuf(&mut body, &self.payload_bits)?;

        // R2007+ OBJECT-supertype string region. LibreDWG's
        // `obj_string_stream` (decode_r2007.c:1297) reads `has_strings`
        // B at body bit position `bitsize - 1`. When set, it reads a
        // 16-bit `data_size` RS at `bitsize - 17` and parses the
        // wide-string content forward from that position minus
        // `data_size` bits. We mirror that forward layout here by
        // appending the string content, then the data_size RS, then
        // the has_strings B — so the marker lands at the exact bit
        // `bitsize - 1` the decoder probes. ENTITY-supertype records
        // are intentionally NOT changed here: their `T` fields are
        // written inline in `payload_bits` today (the proper R2007+
        // entity string-stream wiring is tracked alongside the TEXT
        // round-trip work). Pre-R2007 versions also leave
        // `string_payload_bits` empty and write `T` fields inline.
        let emit_object_string_markers =
            version >= Version::R2007 && self.supertype == ObjectSupertype::Object;
        if emit_object_string_markers {
            let str_bits = self.string_payload_bits.bit_len;
            if str_bits > 0 {
                write_bitbuf(&mut body, &self.string_payload_bits)?;
                let data_size = u16::try_from(str_bits).map_err(|_| {
                    DwgError::InternalInvariant(format!(
                        "string region {str_bits} bits overflows the 15-bit \
                         data_size RS slot (max 32767); the >32k path with \
                         the `hi_size` RS extension is not implemented yet"
                    ))
                })?;
                if data_size & 0x8000 != 0 {
                    return Err(DwgError::InternalInvariant(format!(
                        "string region {data_size} bits has high bit set; \
                         the LibreDWG hi_size RS extension path is not \
                         implemented yet (max 32767 bits per record)"
                    )));
                }
                // `data_size` is an RS (16-bit little-endian), not a
                // big-endian raw 16-bit field. LibreDWG's `bit_read_RS`
                // pulls low byte first then high byte (see
                // `bits.c:384`), so we must emit the same ordering or
                // the decoder reads a byte-swapped value and warns
                // "Invalid string stream data_size".
                body.write_rs(data_size)?;
                body.write_b(true)?; // has_strings = 1
            } else {
                body.write_b(false)?; // has_strings = 0
            }
        }

        // Body-bit position where the handle stream begins (this is
        // `bitsize` in LibreDWG terminology). For R2000-R2007 this is
        // an absolute body-bit offset; for R14 it has the same meaning
        // (the value LibreDWG writes is `bit_position(dat) - objpos`
        // at handle-stream start, which equals our body-bit position
        // because the body buffer starts at byte 0).
        let handle_stream_start_bit = body.bit_position();
        let bitsize_value = u32::try_from(handle_stream_start_bit).map_err(|_| {
            DwgError::InternalInvariant(format!(
                "object record body exceeds 4Gib (handle_stream_start_bit={handle_stream_start_bit})"
            ))
        })?;
        if inline_rl_bitsize {
            body.patch_rl_at(rl_bit_pos, bitsize_value)?;
        }
        if let Some(slot) = r14_bitsize_slot {
            body.patch_rl_at(slot, bitsize_value)?;
        }

        encode_handle_stream(
            &mut body,
            version,
            self.object_type,
            &self.handles,
            &self.common,
            self.supertype,
            self.object_common,
        )?;

        // Pad to byte boundary so obj->size is an integer byte count.
        body.align_to_byte();
        let body_bytes = body.into_bytes();
        let obj_size = body_bytes.len();

        // Stage 2: emit MS + (UMC) + body into the final writer, then
        // pad to byte boundary, compute CRC over the entire pre-CRC
        // buffer (matching LibreDWG's `bit_write_CRC(dat, address,
        // 0xC0C1)` at encode.c:5867 where `address` is the MS start),
        // and finally write the CRC RS.
        let mut out = BitWriter::new();
        let ms_value = u32::try_from(obj_size).map_err(|_| {
            DwgError::InternalInvariant(format!(
                "object record body size ({obj_size}) exceeds u32::MAX"
            ))
        })?;
        out.write_ms(ms_value)?;
        if version >= Version::R2010 {
            // handlestream_size = obj_size_bits - bitsize where
            // `bitsize` is the body-relative bit position of the
            // handle stream start. obj_size_bits = obj_size * 8
            // (NOT including CRC; see decode.c:5155).
            let obj_size_bits = (obj_size as u64) * 8;
            let handlestream_size = obj_size_bits
                .checked_sub(handle_stream_start_bit)
                .ok_or_else(|| {
                    DwgError::InternalInvariant(format!(
                        "handle stream start ({handle_stream_start_bit}) exceeds obj_size_bits ({obj_size_bits})"
                    ))
                })?;
            out.write_umc(handlestream_size)?;
        }
        for b in &body_bytes {
            out.write_bits_u32(8, u32::from(*b))?;
        }
        // Compute CRC over MS + UMC + body bytes (everything written
        // so far); both MS and UMC are byte-aligned writes so the
        // running buffer is always byte-aligned here.
        out.align_to_byte();
        let mut buf = out.into_bytes();
        let crc = crc_x25(0xc0c1, &buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        Ok(buf)
    }

    /// Decode one record using a per-type payload decoder.
    ///
    /// This is the API the file walker uses. The `payload_decoder`
    /// callback receives the body cursor positioned immediately after
    /// the common entity header data, and must consume EXACTLY the
    /// bits the per-type payload uses (no more, no less). For R2010+
    /// the codec also independently knows the payload byte range via
    /// the `bitsize` field; the callback is still invoked, and the
    /// payload byte range it produces is cross-checked against
    /// `bitsize` — a mismatch fails with a structured error rather
    /// than silently mis-parsing handles.
    pub fn decode_with<P, F>(
        version: Version,
        bytes: &[u8],
        payload_decoder: F,
    ) -> DwgResult<(Self, P, usize)>
    where
        F: FnOnce(ObjectType, &CommonHeaderData, &mut BitReader<'_>) -> DwgResult<P>,
    {
        let (header, mut body_r, handle_stream_offset_hint, total) =
            Self::decode_header_only(version, bytes)?;
        let payload_start_bit = body_r.bit_position();
        let payload = payload_decoder(header.object_type, &header.common, &mut body_r)?;
        let payload_end_bit = body_r.bit_position();
        if let Some(handle_start_bit) = handle_stream_offset_hint {
            if payload_end_bit != handle_start_bit {
                return Err(DwgError::MalformedObject {
                    class: format!("{:?}", header.object_type),
                    offset: 0,
                    message: format!(
                        "per-type decoder consumed {} payload bits but bitsize \
                         field declared {}",
                        payload_end_bit - payload_start_bit,
                        handle_start_bit - payload_start_bit
                    ),
                });
            }
        }
        let handles =
            decode_handle_stream(&mut body_r, version, header.object_type, &header.common)?;
        Ok((
            Self {
                object_type: header.object_type,
                handle: header.handle,
                supertype: header.supertype,
                common: header.common,
                object_common: header.object_common,
                payload_bits: BitBuf::new(), // payload was consumed via callback
                string_payload_bits: BitBuf::new(),
                handles,
            },
            payload,
            total,
        ))
    }

    /// Peek at the structural header of a record without consuming
    /// the per-type payload or handle stream.
    ///
    /// This is the API the file walker uses for R2000-R2007 where the
    /// payload-to-handle-stream boundary requires per-type knowledge:
    /// the walker stores the record's raw wire bytes and the structural
    /// summary, and a later pass (with a per-type decoder) recovers
    /// the entity via [`Self::decode_with`].
    ///
    /// Returns:
    /// - `object_type`: the entity class
    /// - `handle`: the record's own handle (from H after BS type)
    /// - `common`: the decoded common entity header data
    /// - `total`: total bytes consumed by the record (MS size + body + CRC)
    pub fn peek_header(
        version: Version,
        bytes: &[u8],
    ) -> DwgResult<(ObjectType, HandleRef, CommonHeaderData, usize)> {
        let (header, _body_r, _hint, total) = Self::decode_header_only(version, bytes)?;
        Ok((header.object_type, header.handle, header.common, total))
    }

    /// Decode just the framing (MS size, [UMC handlestream_size for
    /// R2010+], BS object_type, [RL bitsize for R2000-R2007], H
    /// handle, EED terminator, common header) and return the body bit
    /// reader positioned at the start of the per-type payload, along
    /// with the body-relative bit position where the handle stream
    /// starts.
    ///
    /// The returned `handle_stream_bit_offset` is the body-relative
    /// bit position where the handle stream begins. For R2000-R2007
    /// this is the value of the inline RL `bitsize` field; for R2010+
    /// it is derived from `obj->size * 8 - handlestream_size` per
    /// LibreDWG `decode.c::read_objects` line 5155.
    fn decode_header_only(
        version: Version,
        bytes: &[u8],
    ) -> DwgResult<(HeaderOnly, BitReader<'_>, Option<u64>, usize)> {
        let mut r = BitReader::new(bytes);
        let size = r.read_ms()? as usize;
        if size == 0 {
            return Err(DwgError::MalformedObject {
                class: "ObjectRecord".to_string(),
                offset: 0,
                message: "record declares size 0".to_string(),
            });
        }
        // R2010+: the UMC handlestream_size sits between MS and the
        // body. It is NOT counted in `size` (see decode.c:5150-5157),
        // but it advances the reader cursor before the body begins.
        let modern_handlestream_size = if version >= Version::R2010 {
            Some(r.read_umc()?)
        } else {
            None
        };
        // After MS (and UMC) writes, the bit cursor is byte-aligned
        // because both write multiples of 8 bits per chunk; encoder
        // upholds this invariant. The body starts at the current
        // byte and runs `size` bytes; the CRC follows the body.
        let body_start = (r.bit_position() / 8) as usize;
        let crc_end = body_start + size + 2;
        if bytes.len() < crc_end {
            return Err(DwgError::UnexpectedEof {
                byte: crc_end,
                bit: 0,
            });
        }
        let body = &bytes[body_start..body_start + size];
        let stored_crc =
            u16::from_le_bytes([bytes[body_start + size], bytes[body_start + size + 1]]);
        // CRC covers everything from byte 0 (MS start) through the
        // last body byte; matches `bit_check_CRC(dat, address,
        // 0xC0C1)` at decode.c:5574 where `address` is the MS start.
        let computed_crc = crc_x25(0xc0c1, &bytes[0..body_start + size]);
        if stored_crc != computed_crc {
            return Err(DwgError::SectionCrcMismatch {
                section: "object_record",
                computed: u32::from(computed_crc),
                stored: u32::from(stored_crc),
            });
        }
        let mut body_r = BitReader::new(body);
        let object_type_raw = if version >= Version::R2010 {
            body_r.read_bot()?
        } else {
            body_r.read_bs()? as u16
        };
        let object_type =
            ObjectType::from_u16(object_type_raw).ok_or(DwgError::UnknownObjectType {
                value: object_type_raw,
            })?;
        let handle_stream_offset_hint = if let Some(handlestream_size) = modern_handlestream_size {
            // R2010+: bitsize (body-relative bit position of handle
            // stream start) = obj_size * 8 - handlestream_size, per
            // decode.c:5155. `size` here is the MS value (= obj_size,
            // body bytes only, NO CRC included).
            let obj_size_bits = (size as u64) * 8;
            let bitsize = obj_size_bits
                .checked_sub(handlestream_size)
                .ok_or_else(|| {
                    DwgError::MalformedObject {
                        class: "ObjectRecord".to_string(),
                        offset: 0,
                        message: format!(
                            "handlestream_size ({handlestream_size}) exceeds obj_size_bits ({obj_size_bits})"
                        ),
                    }
                })?;
            Some(bitsize)
        } else if (Version::R2000..=Version::R2007).contains(&version) {
            // R2000-R2007: inline RL bitsize between BS object_type
            // and H handle (decode.c:4136-4138).
            Some(u64::from(body_r.read_rl()?))
        } else {
            None
        };
        let handle = body_r.read_h()?;
        let eed_len = body_r.read_bs()?;
        if eed_len != 0 {
            return Err(DwgError::Unsupported(
                "extended entity data (EED) on object records is not yet decoded".into(),
            ));
        }
        // Dispatch the common-header block by supertype. For OBJECT
        // records (LAYER, BLOCK_HEADER, *_CONTROL, …) the entity
        // common header layout doesn't apply — we read the much
        // smaller object common-data block instead: optional R13/R14
        // inline `bitsize` RL, then `num_reactors` BL, then
        // `is_xdic_missing` B (R2004+), then `has_ds_data` B
        // (R2013+). Mirrors `dwg_decode_object` in LibreDWG
        // `decode.c:6532-6580` and the writer block at
        // `encode_with` lines 326-359 of this file.
        let (common, object_common, supertype, r14_bitsize) = if object_type.is_entity() {
            let (c, b) =
                CommonHeaderData::decode_for_version_capturing_r14_bitsize(version, &mut body_r)?;
            (c, ObjectCommonData::default(), ObjectSupertype::Entity, b)
        } else {
            let r14_bitsize_obj = if version <= Version::R14 {
                Some(body_r.read_rl()?)
            } else {
                None
            };
            let num_reactors = body_r.read_bl()? as u32;
            let is_xdic_missing = if version >= Version::R2004 {
                body_r.read_b()?
            } else {
                false
            };
            let has_ds_data = if version >= Version::R2013 {
                body_r.read_b()?
            } else {
                false
            };
            (
                CommonHeaderData::default(),
                ObjectCommonData {
                    num_reactors,
                    is_xdic_missing,
                    has_ds_data,
                },
                ObjectSupertype::Object,
                r14_bitsize_obj,
            )
        };
        // For R14 the bitsize lives inside the common header; promote
        // it into the handle-stream offset hint so the R14 decoder is
        // bit-perfect with the R2000-R2007 path.
        let handle_stream_offset_hint = match (handle_stream_offset_hint, r14_bitsize) {
            (Some(h), _) => Some(h),
            (None, Some(b)) => Some(u64::from(b)),
            (None, None) => None,
        };
        Ok((
            HeaderOnly {
                object_type,
                handle,
                supertype,
                common,
                object_common,
            },
            body_r,
            handle_stream_offset_hint,
            crc_end,
        ))
    }

    /// Decode one record from `bytes` starting at offset 0. Returns
    /// the record and the number of bytes consumed.
    ///
    /// **R2010+ only.** Only modern files carry the explicit `bitsize`
    /// field that lets us recover the per-type payload bits without
    /// invoking a type-specific decoder. For R2000-R2007 we must know
    /// where the per-type payload ends to find the handle stream, and
    /// that boundary requires per-type knowledge — callers MUST use
    /// [`Self::decode_with`] for those versions and pass a payload
    /// decoder that consumes exactly the right number of bits. To
    /// prevent silently mis-aligned handle streams the entry point
    /// hard-errors for any version below R2010.
    pub fn decode(version: Version, bytes: &[u8]) -> DwgResult<(Self, usize)> {
        if version < Version::R2010 {
            return Err(DwgError::MalformedObject {
                class: "ObjectRecord".to_string(),
                offset: 0,
                message: format!(
                    "ObjectRecord::decode is R2010+ only (got {version:?}); \
                     R2000-R2007 callers must use ObjectRecord::decode_with \
                     with a per-type payload decoder to locate the handle stream"
                ),
            });
        }
        let (header, mut body_r, hint, total) = Self::decode_header_only(version, bytes)?;
        let handle_start = hint.ok_or_else(|| {
            DwgError::InternalInvariant(
                "R2010+ branch reached decode() without a bitsize hint; \
             decode_header_only must always return Some(_) for version >= R2010"
                    .to_string(),
            )
        })?;
        let cur = body_r.bit_position();
        if handle_start < cur {
            return Err(DwgError::MalformedObject {
                class: format!("{:?}", header.object_type),
                offset: 0,
                message: format!(
                    "bitsize indicates handle stream starts at bit {handle_start} \
                     which is before current decoder position {cur}"
                ),
            });
        }
        let payload_bits = read_bitbuf_bits(&mut body_r, handle_start - cur)?;
        body_r.set_bit_position(handle_start)?;
        let handles =
            decode_handle_stream(&mut body_r, version, header.object_type, &header.common)?;
        Ok((
            Self {
                object_type: header.object_type,
                handle: header.handle,
                supertype: header.supertype,
                common: header.common,
                object_common: header.object_common,
                payload_bits,
                string_payload_bits: BitBuf::new(),
                handles,
            },
            total,
        ))
    }
}

/// Internal: parts of the header that we recover before invoking the
/// per-type payload decoder.
struct HeaderOnly {
    object_type: ObjectType,
    handle: HandleRef,
    supertype: ObjectSupertype,
    common: CommonHeaderData,
    object_common: ObjectCommonData,
}

fn write_bitbuf(w: &mut BitWriter, buf: &BitBuf) -> DwgResult<()> {
    let full_bytes = (buf.bit_len / 8) as usize;
    let trailing = (buf.bit_len % 8) as u8;
    for &b in &buf.bytes[..full_bytes] {
        w.write_bits_u32(8, u32::from(b))?;
    }
    if trailing > 0 {
        let last = buf.bytes[full_bytes];
        // Write the high `trailing` bits of `last` as 1-bit ops.
        for i in 0..trailing {
            let bit = (last >> (7 - i)) & 1;
            w.write_b(bit == 1)?;
        }
    }
    Ok(())
}

fn read_bitbuf_bits(r: &mut BitReader<'_>, bit_len: u64) -> DwgResult<BitBuf> {
    let full_bytes = (bit_len / 8) as usize;
    let trailing = (bit_len % 8) as u8;
    let mut bytes = Vec::with_capacity(full_bytes + usize::from(trailing > 0));
    for _ in 0..full_bytes {
        bytes.push(r.read_bits_u32(8)? as u8);
    }
    if trailing > 0 {
        let mut last: u8 = 0;
        for i in 0..trailing {
            let bit = r.read_b()? as u8;
            last |= bit << (7 - i);
        }
        bytes.push(last);
    }
    Ok(BitBuf { bytes, bit_len })
}

// `!common.nolinks` mirrors LibreDWG's `if (!FIELD_VALUE (nolinks))`
// at `common_entity_handle_data.spec:85` verbatim, so we keep the
// same negative-defined branching here rather than inverting to
// please `clippy::if_not_else` — the inverted form makes the
// has-prev/next case the `else` branch and obscures the parallel
// with the spec.
#[allow(clippy::if_not_else)]
fn encode_handle_stream(
    w: &mut BitWriter,
    version: Version,
    object_type: ObjectType,
    handles: &ObjectHandles,
    common: &CommonHeaderData,
    supertype: ObjectSupertype,
    object_common: ObjectCommonData,
) -> DwgResult<()> {
    // For OBJECT supertype, the handle stream is just:
    //   owner H (code 4 = soft owner; per FIELD_HANDLE expected-code
    //   warning in encode.c:786)
    //   reactors H[num_reactors]
    //   xdict H (R13/R2000: always; R2004+: iff !is_xdic_missing)
    //   per-type extras (type_extras, in dwg.spec order)
    //
    // No layer / ltype / prev_entity / next_entity / material / shadow
    // / plotstyle / visualstyle in the OBJECT handle stream \u2014 those are
    // entity-only.
    if supertype == ObjectSupertype::Object {
        let owner = handles.owner.unwrap_or(HandleRef { code: 4, value: 0 });
        w.write_h(owner)?;
        for i in 0..object_common.num_reactors as usize {
            let r = handles
                .reactors
                .get(i)
                .copied()
                .unwrap_or(HandleRef { code: 4, value: 0 });
            w.write_h(r)?;
        }
        let xdict_present = if version <= Version::R2000 {
            true
        } else {
            !object_common.is_xdic_missing
        };
        if xdict_present {
            let xd = handles
                .x_dictionary
                .unwrap_or(HandleRef { code: 3, value: 0 });
            w.write_h(xd)?;
        } else if handles.x_dictionary.is_some() {
            return Err(DwgError::InternalInvariant(
                "ObjectHandles::x_dictionary is Some but \
                 ObjectCommonData::is_xdic_missing is true (R2004+); set \
                 is_xdic_missing = false to emit the handle."
                    .into(),
            ));
        }
        // Per-object-type extras (e.g. LAYER's xref/plotstyle/material/
        // ltype/visualstyle; BLOCK_HEADER's block_entity/first/last/
        // owned/endblk/inserts/layout).
        for extra in &handles.type_extras {
            w.write_h(*extra)?;
        }
        return Ok(());
    }
    // Owner handle: per LibreDWG `common_entity_handle_data.spec:25`,
    // emitted ONLY when `entmode == 0` (i.e. the entity stores its
    // owner explicitly rather than inferring from {model,paper}-space
    // block-record context).
    use crate::dwg::entities::header_codec::EntityMode;
    if common.entity_mode == EntityMode::BlockHeader {
        let owner = handles.owner.unwrap_or(HandleRef { code: 4, value: 0 });
        w.write_h(owner)?;
    }
    // Reactor handles: count is dictated by common.reactor_count.
    for i in 0..common.reactor_count as usize {
        let r = handles
            .reactors
            .get(i)
            .copied()
            .unwrap_or(HandleRef { code: 4, value: 0 });
        w.write_h(r)?;
    }
    // Xdict handle:
    //   pre-R2004 (R14, R2000): always emitted, no data-stream gating
    //     bit exists for those versions in LibreDWG
    //     (see dec_macros.h:1442 ENT_XDICOBJHANDLE: the `else` branch
    //      unconditionally reads xdicobjhandle for R_13b1..R_2002).
    //   R2004+: gated by `xdict_missing` in the data stream.
    let xdict_present = if version <= Version::R2000 {
        true
    } else {
        !common.xdict_missing
    };
    if xdict_present {
        let xd = handles
            .x_dictionary
            .unwrap_or(HandleRef { code: 3, value: 0 });
        w.write_h(xd)?;
    } else if handles.x_dictionary.is_some() {
        return Err(DwgError::InternalInvariant(
            "ObjectHandles::x_dictionary is Some but CommonHeaderData::xdict_missing \
             is true (R2004+); set xdict_missing = false to emit the handle."
                .into(),
        ));
    }
    use crate::dwg::entities::header_codec::LinetypeFlag;
    // Per LibreDWG `common_entity_handle_data.spec`:
    //   * R13 / R14: `layer` + `linetype` come FIRST, then the
    //     R_13b1..R_2000 block emits `prev_entity` / `next_entity`
    //     (when `!nolinks`).
    //   * R2000 (R_2000b): the R_13b1..R_2000 block still emits
    //     `prev_entity` / `next_entity` (when `!nolinks`), but the
    //     SINCE(R_2000b) `layer` / `linetype` block runs AFTER it.
    //     So the on-wire order flips: prev/next FIRST, then layer.
    //   * R2004+: only the SINCE(R_2000b) block runs; no prev/next at
    //     all (BLOCK_HEADER carries `entities[]` instead).
    if version <= Version::R14 {
        w.write_h(handles.layer)?;
        if !common.isbylayerlt {
            let lt = handles.linetype.unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(lt)?;
        }
        if !common.nolinks {
            let prev = handles
                .prev_entity
                .unwrap_or(HandleRef { code: 4, value: 0 });
            let next = handles
                .next_entity
                .unwrap_or(HandleRef { code: 4, value: 0 });
            w.write_h(prev)?;
            w.write_h(next)?;
        }
    } else if version == Version::R2000 {
        if !common.nolinks {
            let prev = handles
                .prev_entity
                .unwrap_or(HandleRef { code: 4, value: 0 });
            let next = handles
                .next_entity
                .unwrap_or(HandleRef { code: 4, value: 0 });
            w.write_h(prev)?;
            w.write_h(next)?;
        }
        w.write_h(handles.layer)?;
        if common.linetype_flag == LinetypeFlag::Handle {
            let lt = handles.linetype.unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(lt)?;
        }
    } else {
        // R2004+: no prev/next; layer + (optional) linetype only.
        w.write_h(handles.layer)?;
        if common.linetype_flag == LinetypeFlag::Handle {
            let lt = handles.linetype.unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(lt)?;
        }
    }
    // R2007+: material (if mat==3), shadow (if shadow==3).
    if version >= Version::R2007 {
        if common.material_flag == 0b11 {
            let m = handles.material.ok_or_else(|| {
                DwgError::InternalInvariant(
                    "CommonHeaderData::material_flag is 0b11 but ObjectHandles::material is None; \
                     set material_flag to 0 (BYLAYER) or provide a material handle."
                        .into(),
                )
            })?;
            w.write_h(m)?;
        } else if handles.material.is_some() {
            return Err(DwgError::InternalInvariant(
                "ObjectHandles::material is Some but CommonHeaderData::material_flag is not 0b11; \
                 set material_flag = 0b11 to emit the handle."
                    .into(),
            ));
        }
        if common.shadow_flags == 3 {
            let s = handles.shadow.unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(s)?;
        }
    }
    // R2000+: plot-style iff flag was 0b11.
    if version >= Version::R2000 && common.plot_style_flag == 0b11 {
        let ps = handles
            .plot_style
            .unwrap_or(HandleRef { code: 5, value: 0 });
        w.write_h(ps)?;
    }
    // R2010+: 3 optional visual-style handles, each gated by the
    // corresponding `has_*_visualstyle` bit in the data stream.
    if version >= Version::R2010 {
        if common.has_full_visualstyle {
            let h = handles
                .full_visualstyle
                .unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(h)?;
        }
        if common.has_face_visualstyle {
            let h = handles
                .face_visualstyle
                .unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(h)?;
        }
        if common.has_edge_visualstyle {
            let h = handles
                .edge_visualstyle
                .unwrap_or(HandleRef { code: 5, value: 0 });
            w.write_h(h)?;
        }
    }
    // Per-entity-type extras (e.g. TEXT style, INSERT block_header).
    let expected_extras = type_extra_handle_count(object_type, common) as usize;
    if handles.type_extras.len() != expected_extras {
        return Err(DwgError::InternalInvariant(format!(
            "ObjectHandles::type_extras for {object_type:?} has {} entries; \
             type_extra_handle_count says {expected_extras}",
            handles.type_extras.len(),
        )));
    }
    for extra in &handles.type_extras {
        w.write_h(*extra)?;
    }
    Ok(())
}

/// Per-entity-type handle count for the handles appended AFTER the
/// common handle stream. Mirrors the post-`COMMON_ENTITY_HANDLE_DATA`
/// `FIELD_HANDLE` lines in LibreDWG `dwg.spec` for each entity type.
pub fn type_extra_handle_count(object_type: ObjectType, _common: &CommonHeaderData) -> u32 {
    match object_type {
        // TEXT / MTEXT / ATTRIB: single `style` handle (soft pointer
        // to AcDbStyle). dwg.spec:181 (TEXT), 386 (ATTRIB), 615 (MTEXT).
        ObjectType::Text => 1,
        // INSERT carries `block_header` (always) and, when has_attribs
        // is set, a list of attrib handles + a seqend. Our encoder
        // never sets has_attribs, so this is always exactly 1.
        // dwg.spec:854 (INSERT).
        ObjectType::Insert => 1,
        // LINE, CIRCLE, ARC, ELLIPSE, LWPOLYLINE, etc. have no
        // post-common type-specific handles.
        _ => 0,
    }
}

#[allow(clippy::if_not_else)] // see `encode_handle_stream` for rationale
fn decode_handle_stream(
    r: &mut BitReader<'_>,
    version: Version,
    object_type: ObjectType,
    common: &CommonHeaderData,
) -> DwgResult<ObjectHandles> {
    use crate::dwg::entities::header_codec::{EntityMode, LinetypeFlag};
    let owner = if common.entity_mode == EntityMode::BlockHeader {
        Some(r.read_h()?)
    } else {
        None
    };
    let mut reactors = Vec::with_capacity(common.reactor_count as usize);
    for _ in 0..common.reactor_count {
        reactors.push(r.read_h()?);
    }
    // Xdict: pre-R2004 unconditional read; R2004+ gated by xdict_missing.
    let x_dictionary = if version <= Version::R2000 || !common.xdict_missing {
        Some(r.read_h()?)
    } else {
        None
    };
    // See the encoder for the matching write order; the on-wire order
    // flips between R14 and R2000 (prev/next come BEFORE layer on
    // R2000).
    let (layer, linetype, prev_entity, next_entity) = if version <= Version::R14 {
        let layer = r.read_h()?;
        let linetype = if common.isbylayerlt {
            None
        } else {
            Some(r.read_h()?)
        };
        let (prev, next) = if !common.nolinks {
            (Some(r.read_h()?), Some(r.read_h()?))
        } else {
            (None, None)
        };
        (layer, linetype, prev, next)
    } else if version == Version::R2000 {
        let (prev, next) = if !common.nolinks {
            (Some(r.read_h()?), Some(r.read_h()?))
        } else {
            (None, None)
        };
        let layer = r.read_h()?;
        let linetype = if common.linetype_flag == LinetypeFlag::Handle {
            Some(r.read_h()?)
        } else {
            None
        };
        (layer, linetype, prev, next)
    } else {
        let layer = r.read_h()?;
        let linetype = if common.linetype_flag == LinetypeFlag::Handle {
            Some(r.read_h()?)
        } else {
            None
        };
        (layer, linetype, None, None)
    };
    let (material, shadow) = if version >= Version::R2007 {
        let m = if common.material_flag == 0b11 {
            Some(r.read_h()?)
        } else {
            None
        };
        let s = if common.shadow_flags == 3 {
            Some(r.read_h()?)
        } else {
            None
        };
        (m, s)
    } else {
        (None, None)
    };
    let plot_style = if version >= Version::R2000 && common.plot_style_flag == 0b11 {
        Some(r.read_h()?)
    } else {
        None
    };
    let (full_visualstyle, face_visualstyle, edge_visualstyle) = if version >= Version::R2010 {
        let full = if common.has_full_visualstyle {
            Some(r.read_h()?)
        } else {
            None
        };
        let face = if common.has_face_visualstyle {
            Some(r.read_h()?)
        } else {
            None
        };
        let edge = if common.has_edge_visualstyle {
            Some(r.read_h()?)
        } else {
            None
        };
        (full, face, edge)
    } else {
        (None, None, None)
    };
    let extras_count = type_extra_handle_count(object_type, common) as usize;
    let mut type_extras = Vec::with_capacity(extras_count);
    for _ in 0..extras_count {
        type_extras.push(r.read_h()?);
    }
    Ok(ObjectHandles {
        owner,
        reactors,
        x_dictionary,
        layer,
        linetype,
        prev_entity,
        next_entity,
        material,
        shadow,
        plot_style,
        full_visualstyle,
        face_visualstyle,
        edge_visualstyle,
        type_extras,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::entities::line::LineEntity;

    fn build_line_record_with_payload(version: Version) -> ObjectRecord {
        let line = LineEntity {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [100.0, 50.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut payload = BitWriter::new();
        line.encode_payload(&mut payload, version).unwrap();
        ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common: CommonHeaderData::default(),
            payload_bits: BitBuf::from_writer(payload),
            string_payload_bits: BitBuf::new(),
            handles: build_handles(),
        }
    }

    fn build_handles() -> ObjectHandles {
        ObjectHandles {
            owner: Some(HandleRef {
                code: 5,
                value: 0x10,
            }),
            layer: HandleRef {
                code: 5,
                value: 0x20,
            },
            ..Default::default()
        }
    }

    /// On R2000-R2007 there is no explicit `bitsize` marker that
    /// separates the per-type payload from the handle stream. Real
    /// walking is composed by the file walker, which dispatches a
    /// per-type decoder via [`ObjectRecord::decode_with`]. The bare
    /// [`ObjectRecord::decode`] entry point is hard-locked to R2010+
    /// to close that footgun structurally rather than relying on doc
    /// comments. This test pins both halves of the contract: the bare
    /// decode errors with a structured `MalformedObject`, and
    /// `decode_with` round-trips an empty payload correctly.
    #[test]
    fn r2000_record_round_trips_via_decode_with_only() {
        let record = ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common: CommonHeaderData::default(),
            payload_bits: BitBuf::new(),
            string_payload_bits: BitBuf::new(),
            handles: build_handles(),
        };
        let bytes = record.encode(Version::R2000).unwrap();

        let err = ObjectRecord::decode(Version::R2000, &bytes).unwrap_err();
        assert!(
            matches!(
                err,
                DwgError::MalformedObject { ref class, .. } if class == "ObjectRecord"
            ),
            "bare decode() must hard-error on R2000-R2007 to prevent silently \
             mis-aligned handle streams, got: {err:?}"
        );

        let (decoded, _payload, consumed) =
            ObjectRecord::decode_with(Version::R2000, &bytes, |_ty, _common, _r| Ok(())).unwrap();
        assert_eq!(decoded.object_type, ObjectType::Line);
        assert_eq!(decoded.handle, record.handle);
        assert_eq!(decoded.handles.layer, record.handles.layer);
        assert_eq!(decoded.handles.owner, record.handles.owner);
        assert_eq!(decoded.common, record.common);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn line_record_round_trips_on_r2010_with_bitsize() {
        let record = build_line_record_with_payload(Version::R2010);
        let bytes = record.encode(Version::R2010).unwrap();
        let (decoded, consumed) = ObjectRecord::decode(Version::R2010, &bytes).unwrap();
        assert_eq!(decoded.object_type, ObjectType::Line);
        assert_eq!(decoded.handle, record.handle);
        assert_eq!(decoded.handles.layer, record.handles.layer);
        // R2010+ carries the payload bits exactly through the bitsize
        // field, so the per-type payload survives the round trip.
        assert_eq!(decoded.payload_bits.bit_len, record.payload_bits.bit_len);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn line_record_round_trips_on_r2018_with_bitsize() {
        let record = build_line_record_with_payload(Version::R2018);
        let bytes = record.encode(Version::R2018).unwrap();
        let (decoded, _) = ObjectRecord::decode(Version::R2018, &bytes).unwrap();
        assert_eq!(decoded.object_type, ObjectType::Line);
        assert_eq!(decoded.payload_bits.bit_len, record.payload_bits.bit_len);
    }

    #[test]
    fn record_round_trips_xdict_handle_on_r2010() {
        // Build a record whose common header declares xdict-present
        // (xdict_missing = false) AND supplies the matching handle in
        // the handle stream. The encoder must emit the extra `H` in
        // the handle stream and the decoder must consume it, leaving
        // the layer handle aligned. If the symmetry were broken the
        // decoder would interpret the xdict handle bytes as the layer
        // handle and the assertion below would fail.
        let xdict = HandleRef {
            code: 3,
            value: 0x77,
        };
        let layer = HandleRef {
            code: 5,
            value: 0x20,
        };
        let common = CommonHeaderData {
            xdict_missing: false,
            ..CommonHeaderData::default()
        };
        let handles = ObjectHandles {
            owner: Some(HandleRef {
                code: 5,
                value: 0x10,
            }),
            x_dictionary: Some(xdict),
            layer,
            ..Default::default()
        };
        let record = ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common,
            payload_bits: BitBuf::new(),
            string_payload_bits: BitBuf::new(),
            handles,
        };
        let bytes = record.encode(Version::R2010).unwrap();
        let (decoded, _) = ObjectRecord::decode(Version::R2010, &bytes).unwrap();
        assert_eq!(decoded.handles.x_dictionary, Some(xdict));
        assert_eq!(decoded.handles.layer, layer);
        assert!(!decoded.common.xdict_missing);
    }

    #[test]
    fn record_round_trips_material_handle_on_r2010() {
        // Symmetric counterpart to the xdict round-trip test: encoder
        // and decoder must both treat the material handle the same way,
        // gated on common.material_flag == 0b11. Historically the
        // encoder emitted the handle whenever ObjectHandles::material
        // was Some(_) but the decoder always returned None, which
        // silently corrupted the handle-stream alignment whenever a
        // caller populated the field.
        let material = HandleRef {
            code: 5,
            value: 0xab,
        };
        let common = CommonHeaderData {
            material_flag: 0b11,
            ..CommonHeaderData::default()
        };
        let handles = ObjectHandles {
            material: Some(material),
            ..build_handles()
        };
        let record = ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common,
            payload_bits: BitBuf::new(),
            string_payload_bits: BitBuf::new(),
            handles,
        };
        let bytes = record.encode(Version::R2010).unwrap();
        let (decoded, _) = ObjectRecord::decode(Version::R2010, &bytes).unwrap();
        assert_eq!(decoded.common.material_flag, 0b11);
        assert_eq!(decoded.handles.material, Some(material));
        assert_eq!(decoded.handles.layer, record.handles.layer);
    }

    #[test]
    fn record_rejects_material_handle_when_flag_disagrees() {
        // If the caller supplies a material handle but leaves
        // material_flag at 0b00 (BYLAYER), the encoder must surface the
        // inconsistency rather than silently emit bytes the decoder
        // will never read. (Mirrors the xdict invariant above.)
        let mut handles = build_handles();
        handles.material = Some(HandleRef {
            code: 5,
            value: 0xab,
        });
        let record = ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common: CommonHeaderData::default(), // material_flag = 0
            payload_bits: BitBuf::new(),
            string_payload_bits: BitBuf::new(),
            handles,
        };
        let err = record.encode(Version::R2010).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
    }

    #[test]
    fn record_rejects_xdict_handle_when_header_says_missing() {
        // If the caller passes an x_dictionary handle but leaves
        // common.xdict_missing = true, the encoder must surface the
        // inconsistency rather than silently writing bytes the decoder
        // will never read. (Silently writing them would corrupt the
        // handle stream alignment.)
        let mut handles = build_handles();
        handles.x_dictionary = Some(HandleRef {
            code: 3,
            value: 0x77,
        });
        let record = ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common: CommonHeaderData::default(), // xdict_missing = true
            payload_bits: BitBuf::new(),
            string_payload_bits: BitBuf::new(),
            handles,
        };
        let err = record.encode(Version::R2010).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
    }

    #[test]
    fn record_rejects_crc_mismatch() {
        let record = build_line_record_with_payload(Version::R2010);
        let mut bytes = record.encode(Version::R2010).unwrap();
        // Flip a bit somewhere in the body.
        let idx = bytes.len() / 2;
        bytes[idx] ^= 0xff;
        assert!(matches!(
            ObjectRecord::decode(Version::R2010, &bytes),
            Err(DwgError::SectionCrcMismatch { .. })
        ));
    }

    #[test]
    fn line_record_round_trips_on_r2000_via_decode_with() {
        // R2000 has no explicit bitsize marker, so the per-type
        // decoder must be supplied during decoding to locate the
        // handle-stream boundary correctly.
        let record = build_line_record_with_payload(Version::R2000);
        let bytes = record.encode(Version::R2000).unwrap();
        let (decoded, payload, consumed) =
            ObjectRecord::decode_with(Version::R2000, &bytes, |obj_type, _common, r| {
                assert_eq!(obj_type, ObjectType::Line);
                LineEntity::decode_payload(r, "0".to_string(), Version::R2000)
            })
            .unwrap();
        assert_eq!(decoded.object_type, ObjectType::Line);
        assert_eq!(decoded.handle, record.handle);
        assert_eq!(decoded.handles.layer, record.handles.layer);
        assert_eq!(decoded.handles.owner, record.handles.owner);
        assert_eq!(payload.start, [0.0, 0.0, 0.0]);
        assert_eq!(payload.end, [100.0, 50.0, 0.0]);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn decode_with_cross_checks_bitsize_on_r2010() {
        // If the per-type decoder consumes fewer bits than the
        // bitsize marker declares, decode_with must surface a
        // structured MalformedObject error rather than silently
        // mis-parsing the handle stream.
        let record = build_line_record_with_payload(Version::R2010);
        let bytes = record.encode(Version::R2010).unwrap();
        let err = ObjectRecord::decode_with(Version::R2010, &bytes, |_t, _c, r| {
            // Consume 1 bit (bogus); leaves the cursor before the
            // declared handle stream start.
            r.read_b()?;
            Ok(())
        })
        .unwrap_err();
        assert!(
            matches!(err, DwgError::MalformedObject { .. }),
            "expected MalformedObject from bitsize mismatch, got {err:?}"
        );
    }

    #[test]
    fn record_decode_reports_unknown_object_type() {
        // Build a minimal R2010+ "bad type" stream. R2010+ format is:
        //   MS object_size
        //   UMC handlestream_size  <-- between MS and body
        //   [body: BS unknown_type, H handle, BS eed-len=0, ...]
        //   RS crc
        let mut body = BitWriter::new();
        // R2010+ uses BOT (variable-length type field), and 0xfff
        // falls in the third (RS) shape since it's > 0x2ef.
        body.write_bot(0xfff).unwrap();
        body.write_h(HandleRef { code: 0, value: 1 }).unwrap();
        body.write_bs(0).unwrap(); // EED terminator
        body.align_to_byte();
        let body_bytes = body.into_bytes();
        let mut out = BitWriter::new();
        out.write_ms(body_bytes.len() as u32).unwrap();
        // Minimal valid UMC for R2010+. handlestream_size = 0 works
        // here because the unknown-type check fires before any
        // handle-stream parsing happens.
        out.write_umc(0).unwrap();
        for b in &body_bytes {
            out.write_bits_u32(8, u32::from(*b)).unwrap();
        }
        out.align_to_byte();
        let mut raw = out.into_bytes();
        // CRC covers MS + UMC + body (everything before the CRC).
        let crc = crc_x25(0xc0c1, &raw);
        raw.extend_from_slice(&crc.to_le_bytes());
        let err = ObjectRecord::decode(Version::R2010, &raw).unwrap_err();
        assert!(
            matches!(err, DwgError::UnknownObjectType { .. }),
            "expected UnknownObjectType, got {err:?}"
        );
    }
}
