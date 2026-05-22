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

/// One object record, decoded into typed parts (still carrying the
/// per-type payload as opaque bits so this module stays type-agnostic).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRecord {
    pub object_type: ObjectType,
    pub handle: HandleRef,
    pub common: CommonHeaderData,
    /// Type-specific payload bits, starting immediately after the
    /// common entity header data and ending immediately before the
    /// handle stream.
    pub payload_bits: BitBuf,
    pub handles: ObjectHandles,
}

/// The handle stream attached to every modern entity.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectHandles {
    /// Owner handle (block-header for entity-mode 0b00 / 0b01).
    pub owner: Option<HandleRef>,
    /// Reactors (count must match `CommonHeaderData::reactor_count`).
    pub reactors: Vec<HandleRef>,
    /// Extension dictionary handle (R2000+; only present when the
    /// xdict-missing flag in the data stream is false).
    pub x_dictionary: Option<HandleRef>,
    /// Layer handle (always present, code = 0x05 = "soft pointer").
    pub layer: HandleRef,
    /// Linetype handle (only present when the linetype flag in the
    /// data stream is 0b11 = "handle follows").
    pub linetype: Option<HandleRef>,
    /// Plot-style handle (only present when the plot-style flag is
    /// 0b11; defaults to BYLAYER when omitted).
    pub plot_style: Option<HandleRef>,
    /// Material handle (R2007+, only when material flag is 0b11).
    pub material: Option<HandleRef>,
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
    /// Bit layout (R2010+):
    ///
    /// ```text
    /// [0]            BS  object_type     (variable: 2-18 bits)
    /// [bs_end]       RL  bitsize         (32 bits, back-patched)
    /// [bs_end+32]    H   handle
    ///                BS  EED-len = 0     (no EED)
    ///                common entity header
    ///                per-type payload
    /// [bitsize]      handle stream
    /// ```
    ///
    /// `bitsize` is measured from absolute bit 0 of the body (the
    /// position right after the MS size prefix), so it == the bit
    /// position where the handle stream starts. R2000-R2007 omit the
    /// RL field and the handle stream simply follows the data stream
    /// contiguously.
    pub fn encode(&self, version: Version) -> DwgResult<Vec<u8>> {
        let mut body = BitWriter::new();

        body.write_bs(self.object_type as u16 as i32)?;

        let has_rl = version >= Version::R2010;
        let rl_bit_pos = body.bit_position();
        if has_rl {
            body.write_rl(0)?; // placeholder; back-patched below
        }

        body.write_h(self.handle)?;
        body.write_bs(0)?; // EED terminator (no EED yet)
        self.common.encode_for_version(version, &mut body)?;
        write_bitbuf(&mut body, &self.payload_bits)?;

        // The current position is the absolute body bit position
        // where the handle stream starts — which is exactly the
        // `bitsize` value the reader will use as
        // `hdlpos = bitsize_pos + bitsize` (bitsize_pos = 0 here).
        let handle_stream_start_bit = body.bit_position();
        if has_rl {
            let bitsize_value = u32::try_from(handle_stream_start_bit).map_err(|_| {
                DwgError::InternalInvariant(format!(
                    "object record body exceeds 4Gib (handle_stream_start_bit={handle_stream_start_bit})"
                ))
            })?;
            body.patch_rl_at(rl_bit_pos, bitsize_value)?;
        }

        encode_handle_stream(&mut body, version, &self.handles, &self.common)?;

        let body_bytes = body.into_bytes();
        let crc = crc_x25(0xc0c1, &body_bytes);

        let mut out = BitWriter::new();
        // MS counts bytes including the trailing CRC but NOT itself.
        out.write_ms((body_bytes.len() + 2) as u32)?;
        for b in &body_bytes {
            out.write_bits_u32(8, u32::from(*b))?;
        }
        out.write_rs(crc)?;
        Ok(out.into_bytes())
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
        let handles = decode_handle_stream(&mut body_r, version, &header.common)?;
        Ok((
            Self {
                object_type: header.object_type,
                handle: header.handle,
                common: header.common,
                payload_bits: BitBuf::new(), // payload was consumed via callback
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

    /// Decode just the header (BS object_type, [RL bitsize], H handle,
    /// EED terminator, common header) and return the body bit reader
    /// positioned at the start of the payload, along with the handle
    /// stream offset hint (Some for R2010+, None for R2000-R2007).
    fn decode_header_only(
        version: Version,
        bytes: &[u8],
    ) -> DwgResult<(HeaderOnly, BitReader<'_>, Option<u64>, usize)> {
        let mut r = BitReader::new(bytes);
        let size = r.read_ms()? as usize;
        if size < 2 {
            return Err(DwgError::MalformedObject {
                class: "ObjectRecord".to_string(),
                offset: 0,
                message: format!("record declares size {size} < 2"),
            });
        }
        let body_start = (r.bit_position() / 8) as usize;
        let body_end = body_start + size - 2;
        let crc_end = body_start + size;
        if bytes.len() < crc_end {
            return Err(DwgError::UnexpectedEof {
                byte: crc_end,
                bit: 0,
            });
        }
        let body = &bytes[body_start..body_end];
        let stored_crc = u16::from_le_bytes([bytes[body_end], bytes[body_end + 1]]);
        let computed_crc = crc_x25(0xc0c1, body);
        if stored_crc != computed_crc {
            return Err(DwgError::SectionCrcMismatch {
                section: "object_record",
                computed: u32::from(computed_crc),
                stored: u32::from(stored_crc),
            });
        }
        let mut body_r = BitReader::new(body);
        let object_type_raw = body_r.read_bs()?;
        let object_type =
            ObjectType::from_u16(object_type_raw as u16).ok_or(DwgError::UnknownObjectType {
                value: object_type_raw as u16,
            })?;
        let handle_stream_offset_hint = if version >= Version::R2010 {
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
        let common = CommonHeaderData::decode_for_version(version, &mut body_r)?;
        Ok((
            HeaderOnly {
                object_type,
                handle,
                common,
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
        let handles = decode_handle_stream(&mut body_r, version, &header.common)?;
        Ok((
            Self {
                object_type: header.object_type,
                handle: header.handle,
                common: header.common,
                payload_bits,
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
    common: CommonHeaderData,
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

fn encode_handle_stream(
    w: &mut BitWriter,
    _version: Version,
    handles: &ObjectHandles,
    common: &CommonHeaderData,
) -> DwgResult<()> {
    // Owner handle: emitted only when entity_mode encodes it.
    use crate::dwg::entities::header_codec::EntityMode;
    if matches!(
        common.entity_mode,
        EntityMode::BlockHeader | EntityMode::DistinctBlockHeader
    ) {
        if let Some(owner) = handles.owner {
            w.write_h(owner)?;
        } else {
            // Spec says we MUST emit one; use 0x0500000000 (soft
            // pointer to nothing) as a defensive default.
            w.write_h(HandleRef { code: 5, value: 0 })?;
        }
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
    // Xdict handle: present only when the xdict-missing flag in the
    // common header is false. We assert the encoder is internally
    // consistent — if the caller has an xdict handle to emit, the
    // common header must say so; if not, we must NOT write any extra
    // bytes here (writing them would desynchronise the handle stream
    // from the bit cursor and corrupt every following handle).
    if !common.xdict_missing {
        let xd = handles
            .x_dictionary
            .unwrap_or(HandleRef { code: 3, value: 0 });
        w.write_h(xd)?;
    } else if handles.x_dictionary.is_some() {
        return Err(DwgError::InternalInvariant(
            "ObjectHandles::x_dictionary is Some but CommonHeaderData::xdict_missing \
             is true; set xdict_missing = false to emit the handle."
                .into(),
        ));
    }
    // Layer is mandatory.
    w.write_h(handles.layer)?;
    // Linetype handle iff flag was Handle.
    use crate::dwg::entities::header_codec::LinetypeFlag;
    if common.linetype_flag == LinetypeFlag::Handle {
        let lt = handles.linetype.unwrap_or(HandleRef { code: 5, value: 0 });
        w.write_h(lt)?;
    }
    // Plot-style handle iff flag was 0b11.
    if common.plot_style_flag == 0b11 {
        let ps = handles
            .plot_style
            .unwrap_or(HandleRef { code: 5, value: 0 });
        w.write_h(ps)?;
    }
    // Material handle: not yet emitted (we always encode the material
    // flag as BYLAYER in CommonHeaderData::encode_for_version, so
    // there's nothing to do here until structured material support
    // lands).
    if let Some(m) = handles.material {
        w.write_h(m)?;
    }
    Ok(())
}

fn decode_handle_stream(
    r: &mut BitReader<'_>,
    _version: Version,
    common: &CommonHeaderData,
) -> DwgResult<ObjectHandles> {
    use crate::dwg::entities::header_codec::{EntityMode, LinetypeFlag};
    let owner = match common.entity_mode {
        EntityMode::BlockHeader | EntityMode::DistinctBlockHeader => Some(r.read_h()?),
        _ => None,
    };
    let mut reactors = Vec::with_capacity(common.reactor_count as usize);
    for _ in 0..common.reactor_count {
        reactors.push(r.read_h()?);
    }
    let x_dictionary = if common.xdict_missing {
        None
    } else {
        Some(r.read_h()?)
    };
    let layer = r.read_h()?;
    let linetype = if common.linetype_flag == LinetypeFlag::Handle {
        Some(r.read_h()?)
    } else {
        None
    };
    let plot_style = if common.plot_style_flag == 0b11 {
        Some(r.read_h()?)
    } else {
        None
    };
    let material = None;
    Ok(ObjectHandles {
        owner,
        reactors,
        x_dictionary,
        layer,
        linetype,
        plot_style,
        material,
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
        line.encode_payload(&mut payload).unwrap();
        let _ = version;
        ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            common: CommonHeaderData::default(),
            payload_bits: BitBuf::from_writer(payload),
            handles: build_handles(),
        }
    }

    fn build_handles() -> ObjectHandles {
        ObjectHandles {
            owner: Some(HandleRef {
                code: 5,
                value: 0x10,
            }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef {
                code: 5,
                value: 0x20,
            },
            linetype: None,
            plot_style: None,
            material: None,
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
            common: CommonHeaderData::default(),
            payload_bits: BitBuf::new(),
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
            reactors: Vec::new(),
            x_dictionary: Some(xdict),
            layer,
            linetype: None,
            plot_style: None,
            material: None,
        };
        let record = ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef { code: 0, value: 1 },
            common,
            payload_bits: BitBuf::new(),
            handles,
        };
        let bytes = record.encode(Version::R2010).unwrap();
        let (decoded, _) = ObjectRecord::decode(Version::R2010, &bytes).unwrap();
        assert_eq!(decoded.handles.x_dictionary, Some(xdict));
        assert_eq!(decoded.handles.layer, layer);
        assert!(!decoded.common.xdict_missing);
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
            common: CommonHeaderData::default(), // xdict_missing = true
            payload_bits: BitBuf::new(),
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
                LineEntity::decode_payload(r, "0".to_string())
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
        // Build a minimal "bad type" stream:
        let mut body = BitWriter::new();
        body.write_bs(0xfff).unwrap(); // unknown type
        body.write_rl(0).unwrap();
        body.write_h(HandleRef { code: 0, value: 1 }).unwrap();
        body.write_bs(0).unwrap(); // EED terminator
        let body_bytes = body.into_bytes();
        let crc = crc_x25(0xc0c1, &body_bytes);
        let mut out = BitWriter::new();
        out.write_ms((body_bytes.len() + 2) as u32).unwrap();
        for b in &body_bytes {
            out.write_bits_u32(8, u32::from(*b)).unwrap();
        }
        out.write_rs(crc).unwrap();
        let raw = out.into_bytes();
        let err = ObjectRecord::decode(Version::R2010, &raw).unwrap_err();
        assert!(
            matches!(err, DwgError::UnknownObjectType { .. }),
            "expected UnknownObjectType, got {err:?}"
        );
    }
}
