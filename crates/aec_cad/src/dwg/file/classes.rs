//! R13 - R2018 classes section.
//!
//! Each non-fixed-class object in a DWG file carries a class code that
//! is dereferenced through this section. The wire format diverges
//! between the legacy R13-R2000 layout and the R2004+ layout used
//! inside paged sections.
//!
//! ## R13-R2000 (legacy)
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ Start sentinel (16 bytes — CLASSES_BEGIN)            │
//! ├──────────────────────────────────────────────────────┤
//! │ RL  size_in_bytes                                    │
//! ├──────────────────────────────────────────────────────┤
//! │ N × ClassRecord (strings inline as TV)               │
//! ├──────────────────────────────────────────────────────┤
//! │ RS unknown (always 0 in R14, padding bits in R2000)  │
//! ├──────────────────────────────────────────────────────┤
//! │ CRC-X25                                              │
//! ├──────────────────────────────────────────────────────┤
//! │ End sentinel (16 bytes — CLASSES_END)                │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! ## R2004+ (paged + version-gated headers)
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ Start sentinel (16 bytes — CLASSES_BEGIN)                │
//! ├──────────────────────────────────────────────────────────┤
//! │ RL  size_in_bytes                                        │
//! ├──────────────────────────────────────────────────────────┤
//! │ RL  hsize          (R2010+ && maint > 3, OR R2018+)      │
//! ├──────────────────────────────────────────────────────────┤
//! │ RL  bitsize        (R2007+ only)                         │
//! ├──────────────────────────────────────────────────────────┤
//! │ BS  max_num                                              │
//! │ RC  0                                                    │
//! │ RC  0                                                    │
//! │ B   1                                                    │
//! ├──────────────────────────────────────────────────────────┤
//! │ N × ClassRecord — numeric fields only                    │
//! │  (R2004 : strings ARE inline as TV; R2007+ : strings     │
//! │   are SPLIT into the dedicated sub-stream below)         │
//! ├──────────────────────────────────────────────────────────┤
//! │ R2007+ string sub-stream (UTF-16LE T entries)           │
//! │ RS data_size       (number of bits in the string stream) │
//! │ B  endbit = 1                                            │
//! ├──────────────────────────────────────────────────────────┤
//! │ CRC-X25                                                  │
//! ├──────────────────────────────────────────────────────────┤
//! │ End sentinel (16 bytes — CLASSES_END)                    │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! The `bitsize` field is recovered by `decode_r2007.c:1398` as
//! `endbit_position = bitsize + 159` (no hsize) or `bitsize + 191`
//! (with hsize) — counted from the section start INCLUDING the
//! 16-byte sentinel. See [`open_string_substream`] for the matching
//! decoder.
//!
//! ## Per-record layout
//!
//! ```text
//! BS class_number   (≥ 500 for custom classes; < 500 reserved)
//! BS version        (proxy/object version flags)
//! TV app_name       ("ObjectDBX Classes")        [inline pre-R2007]
//! TV cpp_class_name ("AcDbHatch")                [inline pre-R2007]
//! TV dxf_record_name ("HATCH")                   [inline pre-R2007]
//! B  was_zombie     (R14+: did the class load as zombie?)
//! BS item_class_id  (0x1F2 for entity, 0x1F3 for object)
//! ```
//!
//! For R2007+ the three string fields move into the section's
//! dedicated string sub-stream; the numeric fields stay in the main
//! stream in the same relative order.
//!
//! ## Empty-section conformance
//!
//! LibreDWG's R2004+ decoder requires `max_num >= 500` and computes
//! `num_classes = max_num - 499` (`decode.c:2192`). The smallest
//! representable section therefore has exactly ONE record; an empty
//! upstream document still emits a synthetic `AcDbPlaceHolder` (see
//! [`placeholder_class`]). The decoder identifies that exact byte
//! pattern via [`is_placeholder_class`] and strips it back out so
//! round-trips stay empty. AutoCAD-emitted DWGs follow the same
//! convention — `AcDbPlaceHolder` is one of the first custom classes
//! in every real-world file.

use crate::dwg::bits::{crc_x25, BitReader, BitWriter};
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::sentinels::{CLASSES_BEGIN, CLASSES_END};
use crate::dwg::version::Version;

/// One class record. The DXF subset we round-trip uses only fixed
/// classes (LINE, CIRCLE, …), so this is needed only when a file
/// references custom AcDbProxyEntity-derived classes — which we
/// preserve verbatim from input to output but do not interpret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRecord {
    pub class_number: i32,
    pub version: i32,
    pub app_name: String,
    pub cpp_class_name: String,
    pub dxf_record_name: String,
    pub was_zombie: bool,
    pub item_class_id: i32,
}

/// Classes section, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassesSection {
    pub version: Version,
    pub classes: Vec<ClassRecord>,
}

impl ClassesSection {
    /// Build an empty section. Sufficient for any DWG that uses only
    /// the fixed classes.
    pub fn empty(version: Version) -> Self {
        Self {
            version,
            classes: Vec::new(),
        }
    }

    /// Encode the section into `out` (start sentinel + body + end sentinel).
    ///
    /// The wire format diverges between pre-R2004 (legacy) and R2004+
    /// (LibreDWG-conformant). The pre-R2004 layout is just:
    ///
    /// ```text
    /// sentinel | RL size | <records> | RS 0 | CRC | sentinel
    /// ```
    ///
    /// The R2004+ layout (per `encode.c:2700-2792` and
    /// `decode.c:2154-2208` in LibreDWG) prepends version-gated
    /// headers and a strictly required `max_num` field:
    ///
    /// ```text
    /// sentinel | RL size
    ///          | [RL hsize]   (R2010+ maint>3 OR R2018+)
    ///          | [RL bitsize] (R2007+)
    ///          | BS max_num = num_classes + 500
    ///          | RC 0 | RC 0 | B 1
    ///          | <records>
    ///          | [string sub-stream + RS data_size + endbit]  (R2007+)
    ///          | CRC | sentinel
    /// ```
    pub fn encode(&self, out: &mut Vec<u8>) -> DwgResult<()> {
        self.encode_with_maint(out, 0)
    }

    /// Same as [`Self::encode`] but accepts an explicit maintenance
    /// version so the R2010+/R2018+ `hsize` placeholder is gated
    /// against the actual `maint_version` byte in the file header.
    ///
    /// The decoder reads `hsize` only when `from_version >= R_2010 &&
    /// maint_version > 3` OR `from_version >= R_2018` (see
    /// `decode.c:2169`). Callers that emit R2010 with maint=0 must
    /// NOT include `hsize` or the decoder will misalign by 32 bits.
    pub fn encode_with_maint(&self, out: &mut Vec<u8>, maint_version: u8) -> DwgResult<()> {
        if self.version >= Version::R2004 {
            return self.encode_r2004_plus(out, maint_version);
        }
        // Pre-R2004 (R13/R14/R2000) wire format — short, single stream.
        let mut body = BitWriter::new();
        body.write_rl(0)?; // size placeholder
        for c in &self.classes {
            body.write_bs(c.class_number)?;
            body.write_bs(c.version)?;
            body.write_tv(&c.app_name)?;
            body.write_tv(&c.cpp_class_name)?;
            body.write_tv(&c.dxf_record_name)?;
            body.write_b(c.was_zombie)?;
            body.write_bs(c.item_class_id)?;
        }
        body.write_rs(0)?; // unknown padding RS (mirrors LibreDWG)
        let mut framed = body.into_bytes();
        let body_size = (framed.len() - 4) as u32;
        framed[0..4].copy_from_slice(&body_size.to_le_bytes());
        let crc = crc_x25(0xc0c1, &framed);
        framed.extend_from_slice(&crc.to_le_bytes());

        out.extend_from_slice(&CLASSES_BEGIN);
        out.extend_from_slice(&framed);
        out.extend_from_slice(&CLASSES_END);
        Ok(())
    }

    fn encode_r2004_plus(&self, out: &mut Vec<u8>, maint_version: u8) -> DwgResult<()> {
        let r2007_plus = self.version >= Version::R2007;
        let bitsize_hi_emit =
            (self.version >= Version::R2010 && maint_version > 3) || self.version >= Version::R2018;

        // LibreDWG's decoder rejects `max_num < 500` (decode.c:2192)
        // and computes `num_classes = max_num - 499`. So the smallest
        // representable section has exactly ONE class record with
        // max_num = 500.  When the upstream document carries zero
        // custom classes we still emit a single neutral placeholder
        // (AcDbPlaceHolder) so the wire format stays conformant —
        // AutoCAD-emitted DWGs follow the same convention.
        let placeholder;
        let records: &[ClassRecord] = if self.classes.is_empty() {
            placeholder = [placeholder_class()];
            &placeholder
        } else {
            &self.classes
        };

        // Build the per-record main + string sub-streams independently
        // (matching the same shape as the AcDb:Header encoder). For
        // R2007+ class strings travel in the dedicated string stream
        // appended after the numeric region; for R2004 they go inline
        // in the main stream.
        let mut main_w = BitWriter::new();
        let mut str_w = BitWriter::new();
        for c in records {
            main_w.write_bs(c.class_number)?;
            main_w.write_bs(c.version)?;
            if r2007_plus {
                str_w.write_t(&c.app_name)?;
                str_w.write_t(&c.cpp_class_name)?;
                str_w.write_t(&c.dxf_record_name)?;
            } else {
                main_w.write_tv(&c.app_name)?;
                main_w.write_tv(&c.cpp_class_name)?;
                main_w.write_tv(&c.dxf_record_name)?;
            }
            main_w.write_b(c.was_zombie)?;
            main_w.write_bs(c.item_class_id)?;
        }
        let main_bits = main_w.bit_position();
        let str_bits = str_w.bit_position();
        let main_bytes = main_w.into_bytes();
        let str_bytes = str_w.into_bytes();

        // Assemble the body.
        let mut body = BitWriter::new();
        body.write_rl(0)?; // size_in_bytes placeholder
        if bitsize_hi_emit {
            body.write_rl(0)?; // hsize (zero for our writer)
        }
        let bitsize_field_pos = body.bit_position();
        if r2007_plus {
            body.write_rl(0)?; // bitsize placeholder
        }
        // max_num + the two RC unknowns + B(1) live at the start of
        // the numeric region, BEFORE the per-record content. This is
        // the order the decoder reads them in (`decode.c:2180-2188`).
        //
        // The decoder computes `num_classes = max_num - 499`
        // (decode.c:2191) and rejects `max_num < 500` (decode.c:2192),
        // so the on-the-wire convention is `max_num = num_records +
        // 499` — NOT the off-by-one `num + 500` that LibreDWG's own
        // encoder writes (encode.c:2723). We follow the decoder side
        // since that's what reads our files.
        //
        // The `records.len()` here is the post-placeholder count
        // (always ≥ 1), guaranteeing `max_num ≥ 500`.
        let num_records: u16 = records.len().try_into().map_err(|_| {
            DwgError::InternalInvariant(format!(
                "classes section has {} entries; > u16::MAX is unsupported",
                records.len()
            ))
        })?;
        let max_num = num_records.checked_add(499).ok_or_else(|| {
            DwgError::InternalInvariant(format!(
                "classes section: num_records ({num_records}) + 499 overflows u16"
            ))
        })?;
        body.write_bs(i32::from(max_num))?;
        body.write_rc(0)?;
        body.write_rc(0)?;
        body.write_b(true)?;

        // Append the per-record main stream.
        body.append_bits_from(&main_bytes, main_bits)?;

        // R2007+ string sub-stream: same shape as the AcDb:Header
        // string stream. Emit the strings, then the 16-bit RS
        // data_size field, then a 1-bit endbit=1. Decoder walks
        // backward from `bitsize + 159|191` to find this.
        if r2007_plus {
            // The data_size field is a single RS (16 bits) in our
            // single-record-per-section world. LibreDWG's
            // `section_string_stream` defines an extended-size shape
            // that uses a second RS for the high bits when
            // data_size > 0x7fff; we surface an error rather than
            // silently truncating via `as u16` if the strings ever
            // exceed that threshold. In practice this is unreachable
            // because the placeholder class's strings are short.
            if str_bits > 0x7fff {
                return Err(DwgError::InternalInvariant(format!(
                    "classes string stream exceeded 0x7fff bits ({str_bits}); \
                     the extended-size encoding (high RS half) is not yet \
                     implemented -- shorten the per-class strings or add \
                     the hi_size emit/decode path"
                )));
            }
            body.append_bits_from(&str_bytes, str_bits)?;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the runtime guard above pins str_bits to fit u16"
            )]
            body.write_rs(str_bits as u16)?;
            body.write_b(true)?; // endbit
        }
        // Patch bitsize — same formula as the AcDb:Header path:
        // bitsize = (post-endbit position) - (position of the
        // bitsize RL field itself).
        if r2007_plus {
            let region_end = body.bit_position();
            let bitsize = region_end - bitsize_field_pos;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bitsize is naturally bounded by classes-section size, well below u32::MAX"
            )]
            body.patch_rl_at(bitsize_field_pos, bitsize as u32)?;
        }
        body.align_to_byte();

        let mut framed = body.into_bytes();
        // Patch size_in_bytes (excludes the 4-byte size field itself
        // and excludes the trailing CRC).
        #[allow(
            clippy::cast_possible_truncation,
            reason = "classes section size is naturally bounded by num_classes; well below u32::MAX"
        )]
        let body_size = (framed.len() - 4) as u32;
        framed[0..4].copy_from_slice(&body_size.to_le_bytes());

        let crc = crc_x25(0xc0c1, &framed);
        framed.extend_from_slice(&crc.to_le_bytes());

        out.extend_from_slice(&CLASSES_BEGIN);
        out.extend_from_slice(&framed);
        out.extend_from_slice(&CLASSES_END);
        Ok(())
    }

    /// Parse the section starting at `offset`.
    pub fn parse(version: Version, bytes: &[u8], offset: usize) -> DwgResult<Self> {
        Self::parse_with_maint(version, bytes, offset, 0)
    }

    /// Same as [`Self::parse`] but the caller passes the file-level
    /// `maint_version` byte. R2010+ files with `maint > 3` and all
    /// R2018+ files prepend a 32-bit `hsize` placeholder between the
    /// `size_in_bytes` RL and the `bitsize` RL — skipping it here is
    /// what keeps the per-record decode aligned with the encoder.
    pub fn parse_with_maint(
        version: Version,
        bytes: &[u8],
        offset: usize,
        maint_version: u8,
    ) -> DwgResult<Self> {
        if bytes.len() < offset + 16 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let begin = &bytes[offset..offset + 16];
        if begin != CLASSES_BEGIN.as_slice() {
            let mut got = [0u8; 16];
            got.copy_from_slice(begin);
            return Err(DwgError::InvalidSentinel {
                section: "classes",
                expected: CLASSES_BEGIN,
                got,
            });
        }
        let after_begin = offset + 16;
        if bytes.len() < after_begin + 4 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let size_in_bytes = u32::from_le_bytes([
            bytes[after_begin],
            bytes[after_begin + 1],
            bytes[after_begin + 2],
            bytes[after_begin + 3],
        ]) as usize;
        // Body region (bit-encoded): 4 (size prefix) .. 4 + size_in_bytes.
        // CRC immediately follows: 2 bytes.
        // End sentinel: 16 bytes.
        let body_end = after_begin + 4 + size_in_bytes;
        if bytes.len() < body_end + 2 + 16 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let stored_crc = u16::from_le_bytes([bytes[body_end], bytes[body_end + 1]]);
        let computed_crc = crc_x25(0xc0c1, &bytes[after_begin..body_end]);
        if stored_crc != computed_crc {
            return Err(DwgError::SectionCrcMismatch {
                section: "classes",
                computed: u32::from(computed_crc),
                stored: u32::from(stored_crc),
            });
        }
        let end_off = body_end + 2;
        let end = &bytes[end_off..end_off + 16];
        if end != CLASSES_END.as_slice() {
            let mut got = [0u8; 16];
            got.copy_from_slice(end);
            return Err(DwgError::InvalidSentinel {
                section: "classes",
                expected: CLASSES_END,
                got,
            });
        }
        // Decode the body. The wire format diverges by version:
        // pre-R2004 is a tight bit-stream of records; R2004+ has a
        // version-gated header (hsize/bitsize/max_num/RC/RC/B) then
        // records, then optionally a string sub-stream.
        let mut reader = BitReader::new(&bytes[after_begin + 4..body_end]);

        let r2004_plus = version >= Version::R2004;
        let r2007_plus = version >= Version::R2007;
        let bitsize_hi_emit =
            (version >= Version::R2010 && maint_version > 3) || version >= Version::R2018;

        if r2004_plus {
            if bitsize_hi_emit {
                let _hsize = reader.read_rl()?;
            }
            let mut bitsize_value: u32 = 0;
            if r2007_plus {
                bitsize_value = reader.read_rl()?;
            }
            let max_num = reader.read_bs()?;
            if !(500..=u16::MAX as i32).contains(&max_num) {
                return Err(DwgError::InternalInvariant(format!(
                    "classes section: max_num={max_num} out of [500, 65535]"
                )));
            }
            // Symmetric to the encoder above: decoder treats
            // `max_num` as `highest_class_number_assigned + 1`, so
            // `num_records = max_num - 499` (decode.c:2191). When we
            // see exactly one record we treat it as the writer's
            // placeholder and surface zero classes back to callers so
            // round-trips through an empty Document stay empty.
            let num_records = (max_num - 499) as usize;
            let _rc1 = reader.read_rc()?;
            let _rc2 = reader.read_rc()?;
            let _btrue = reader.read_b()?;

            // For R2007+ the per-record strings live in a separate
            // sub-stream that begins somewhere below `endbit_pos =
            // bitsize_value + 159|191`. We rebuild that sub-stream
            // here before decoding the records so each record can pull
            // numeric fields from the main stream and string fields
            // from the string stream, in lockstep.
            let mut string_reader = if r2007_plus {
                Some(open_string_substream(
                    &bytes[after_begin..body_end],
                    bitsize_value,
                    bitsize_hi_emit,
                )?)
            } else {
                None
            };

            let mut records = Vec::with_capacity(num_records);
            for _ in 0..num_records {
                records.push(decode_class_record(
                    &mut reader,
                    string_reader.as_mut(),
                    version,
                )?);
            }
            // Strip the writer-side placeholder (see the matching
            // comment in `encode_r2004_plus`). The placeholder is
            // identified structurally — class_number == 500 with the
            // exact placeholder strings — so user-supplied classes
            // that happen to be at index 500 are NOT mistaken for it.
            let classes = if records.len() == 1 && is_placeholder_class(&records[0]) {
                Vec::new()
            } else {
                records
            };
            return Ok(Self { version, classes });
        }

        // Pre-R2004 records-only path. Walk until the trailing RS +
        // padding is reached.
        let mut classes = Vec::new();
        while reader.remaining_bits() >= 16 {
            let snapshot = reader.bit_position();
            if let Ok(c) = decode_class_record(&mut reader, None, version) {
                classes.push(c);
            } else {
                reader.set_bit_position(snapshot)?;
                break;
            }
        }
        Ok(Self { version, classes })
    }
}

/// Marker class number used by [`placeholder_class`] / [`is_placeholder_class`].
/// First custom class number per AutoCAD convention (0..=499 are
/// reserved for fixed types).
const PLACEHOLDER_CLASS_NUMBER: i32 = 500;
/// `app_name` for the writer-side placeholder. AutoCAD-emitted DWGs
/// always tag custom classes with this exact application name.
const PLACEHOLDER_APP_NAME: &str = "ObjectDBX Classes";
/// `cpp_class_name` for the writer-side placeholder. AutoCAD always
/// emits `AcDbPlaceHolder` as one of the first custom classes; we
/// reuse it to keep the wire format indistinguishable from a real
/// minimal AutoCAD file.
const PLACEHOLDER_CPP_CLASS_NAME: &str = "AcDbPlaceHolder";
const PLACEHOLDER_DXF_RECORD_NAME: &str = "ACDBPLACEHOLDER";

/// Synthetic placeholder class emitted when the upstream document
/// carries zero custom classes. LibreDWG's decoder rejects `max_num
/// < 500` (`decode.c:2192`), so the minimum representable Classes
/// section has exactly one record with `class_number == 500`.
fn placeholder_class() -> ClassRecord {
    ClassRecord {
        class_number: PLACEHOLDER_CLASS_NUMBER,
        version: 0,
        app_name: PLACEHOLDER_APP_NAME.to_string(),
        cpp_class_name: PLACEHOLDER_CPP_CLASS_NAME.to_string(),
        dxf_record_name: PLACEHOLDER_DXF_RECORD_NAME.to_string(),
        was_zombie: false,
        item_class_id: 0x1F3,
    }
}

/// Structural check used by the decoder to recognise the writer's
/// own placeholder and strip it so an empty `ClassesSection`
/// round-trips as empty rather than appearing to carry a phantom
/// AcDbPlaceHolder. User-supplied classes that happen to share
/// class_number 500 but differ on any other field will NOT match.
fn is_placeholder_class(c: &ClassRecord) -> bool {
    c.class_number == PLACEHOLDER_CLASS_NUMBER
        && c.version == 0
        && c.app_name == PLACEHOLDER_APP_NAME
        && c.cpp_class_name == PLACEHOLDER_CPP_CLASS_NAME
        && c.dxf_record_name == PLACEHOLDER_DXF_RECORD_NAME
        && !c.was_zombie
        && c.item_class_id == 0x1F3
}

/// Open the per-section string sub-stream described in
/// `decode_r2007.c:1398-1445`. The buffer passed in must begin at
/// the byte AFTER the section's 16-byte CLASSES_BEGIN sentinel —
/// i.e. starting at the size RL.
///
/// The decoder formula for the endbit position is:
/// * `bitsize + 159` (8*20 - 1) when no hsize, OR
/// * `bitsize + 191` (8*24 - 1) when hsize is present.
///
/// Those constants are absolute positions in the FULL section bytes
/// (which include the 128-bit sentinel). Stripping the sentinel —
/// which is what every caller of this helper does — drops both
/// constants by 128 bits, yielding 31 / 63.
fn open_string_substream(
    body_after_sentinel: &[u8],
    bitsize: u32,
    has_hsize: bool,
) -> DwgResult<BitReader<'_>> {
    let endbit_pos: u32 = if has_hsize {
        bitsize.checked_add(63).ok_or_else(|| {
            DwgError::InternalInvariant(format!(
                "classes string-stream: bitsize {bitsize} + 63 overflows u32"
            ))
        })?
    } else {
        bitsize.checked_add(31).ok_or_else(|| {
            DwgError::InternalInvariant(format!(
                "classes string-stream: bitsize {bitsize} + 31 overflows u32"
            ))
        })?
    };
    let mut probe = BitReader::new(body_after_sentinel);
    probe.set_bit_position(u64::from(endbit_pos))?;
    let endbit = probe.read_b()?;
    if !endbit {
        // No strings present: hand back an empty reader so callers
        // that try to pull from it fail with a clear UnexpectedEof.
        return Ok(BitReader::new(&[]));
    }
    let data_size_pos = endbit_pos.checked_sub(16).ok_or_else(|| {
        DwgError::InternalInvariant(
            "classes string-stream: endbit_pos < 16 (not enough room for the data_size RS)".into(),
        )
    })?;
    probe.set_bit_position(u64::from(data_size_pos))?;
    let mut data_size = u32::from(probe.read_rs()?);
    let mut substream_start = data_size_pos;
    if data_size & 0x8000 != 0 {
        // Extended length: a second RS at `data_size_pos - 16` holds
        // the high 15 bits.
        let hi_pos = data_size_pos.checked_sub(16).ok_or_else(|| {
            DwgError::InternalInvariant(
                "classes string-stream: extended-size form requires data_size_pos >= 16".into(),
            )
        })?;
        probe.set_bit_position(u64::from(hi_pos))?;
        let hi = u32::from(probe.read_rs()?);
        data_size = (data_size & 0x7FFF) | (hi << 15);
        substream_start = hi_pos;
    }
    let start = substream_start
        .checked_sub(data_size)
        .ok_or_else(|| DwgError::InternalInvariant(format!(
            "classes string-stream: data_size {data_size} exceeds available room before {substream_start}"
        )))?;
    let mut str_reader = BitReader::new(body_after_sentinel);
    str_reader.set_bit_position(u64::from(start))?;
    Ok(str_reader)
}

fn decode_class_record(
    r: &mut BitReader<'_>,
    str_r: Option<&mut BitReader<'_>>,
    version: Version,
) -> DwgResult<ClassRecord> {
    let class_number = r.read_bs()?;
    let record_version = r.read_bs()?;
    let (app_name, cpp_class_name, dxf_record_name) = if let Some(s) = str_r {
        // R2007+: numeric fields stay in `r`; strings come from the
        // section's dedicated string sub-stream.
        (s.read_t()?, s.read_t()?, s.read_t()?)
    } else if version.uses_utf16_strings() {
        (r.read_t()?, r.read_t()?, r.read_t()?)
    } else {
        (r.read_tv()?, r.read_tv()?, r.read_tv()?)
    };
    Ok(ClassRecord {
        class_number,
        version: record_version,
        app_name,
        cpp_class_name,
        dxf_record_name,
        was_zombie: r.read_b()?,
        item_class_id: r.read_bs()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_section_round_trips() {
        let section = ClassesSection::empty(Version::R2000);
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        let parsed = ClassesSection::parse(Version::R2000, &buf, 0).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn non_empty_section_round_trips_r2000_tv() {
        let section = ClassesSection {
            version: Version::R2000,
            classes: vec![ClassRecord {
                class_number: 500,
                version: 0,
                app_name: "ObjectDBX Classes".into(),
                cpp_class_name: "AcDbWipeout".into(),
                dxf_record_name: "WIPEOUT".into(),
                was_zombie: false,
                item_class_id: 0x1f2,
            }],
        };
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        let parsed = ClassesSection::parse(Version::R2000, &buf, 0).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn non_empty_section_round_trips_r2007_utf16() {
        let section = ClassesSection {
            version: Version::R2007,
            classes: vec![ClassRecord {
                class_number: 500,
                version: 0,
                app_name: "ObjectDBX Classes".into(),
                cpp_class_name: "AcDbWipeout".into(),
                dxf_record_name: "WIPEOUT".into(),
                was_zombie: false,
                item_class_id: 0x1f2,
            }],
        };
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        let parsed = ClassesSection::parse(Version::R2007, &buf, 0).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn class_record_with_non_ascii_round_trips_r2010_utf16() {
        // CJK characters require UTF-16 — the CP1252 path would error.
        let section = ClassesSection {
            version: Version::R2010,
            classes: vec![ClassRecord {
                class_number: 600,
                version: 1,
                app_name: "\u{6f22}\u{5b57}App".into(), // 漢字App
                cpp_class_name: "AcDb\u{571f}Class".into(), // AcDb土Class
                dxf_record_name: "NONASCII".into(),
                was_zombie: true,
                item_class_id: 0x1f3,
            }],
        };
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        let parsed = ClassesSection::parse(Version::R2010, &buf, 0).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn parse_rejects_wrong_sentinel() {
        let buf = vec![0u8; 64];
        assert!(matches!(
            ClassesSection::parse(Version::R2000, &buf, 0),
            Err(DwgError::InvalidSentinel { .. })
        ));
    }

    #[test]
    fn parse_rejects_crc_mismatch() {
        let section = ClassesSection::empty(Version::R2000);
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        // Corrupt the body (between the size prefix and CRC).
        buf[16 + 5] ^= 0xff;
        let err = ClassesSection::parse(Version::R2000, &buf, 0).unwrap_err();
        assert!(matches!(err, DwgError::SectionCrcMismatch { .. }));
    }

    #[test]
    fn empty_section_round_trips_through_r2004_placeholder() {
        // R2004+ on-wire format requires at least one record
        // (max_num >= 500). The encoder injects a synthetic
        // AcDbPlaceHolder; the decoder strips it back out so an empty
        // input round-trips to an empty output.
        for version in [Version::R2004, Version::R2007, Version::R2010] {
            let section = ClassesSection::empty(version);
            let mut buf = Vec::new();
            section.encode(&mut buf).unwrap();
            let parsed = ClassesSection::parse(version, &buf, 0).unwrap();
            assert_eq!(parsed, section, "round-trip failed for {version:?}");
        }
    }

    #[test]
    fn empty_r2010_with_real_maint_emits_hsize_and_round_trips() {
        // R2010 ships with maint_version=4 in the file header (see
        // version.rs:119), which triggers the hsize RL emit path.
        // Verify the encoder + decoder agree on that gated header.
        let section = ClassesSection::empty(Version::R2010);
        let mut buf = Vec::new();
        section.encode_with_maint(&mut buf, 4).unwrap();
        let parsed = ClassesSection::parse_with_maint(Version::R2010, &buf, 0, 4).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn empty_r2018_emits_hsize_and_round_trips() {
        // R2018+ unconditionally emits hsize (regardless of maint).
        let section = ClassesSection::empty(Version::R2018);
        let mut buf = Vec::new();
        section.encode_with_maint(&mut buf, 1).unwrap();
        let parsed = ClassesSection::parse_with_maint(Version::R2018, &buf, 0, 1).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn r2010_maint4_multi_record_round_trips() {
        // Multiple records with the hsize path on, exercising both
        // the main + string sub-streams end-to-end.
        let section = ClassesSection {
            version: Version::R2010,
            classes: vec![
                ClassRecord {
                    class_number: 500,
                    version: 0,
                    app_name: "ObjectDBX Classes".into(),
                    cpp_class_name: "AcDbDictionaryWithDefault".into(),
                    dxf_record_name: "ACDBDICTIONARYWDFLT".into(),
                    was_zombie: false,
                    item_class_id: 0x1f3,
                },
                ClassRecord {
                    class_number: 501,
                    version: 0,
                    app_name: "ObjectDBX Classes".into(),
                    cpp_class_name: "AcDbPlaceHolder".into(),
                    dxf_record_name: "ACDBPLACEHOLDER".into(),
                    was_zombie: false,
                    item_class_id: 0x1f3,
                },
                ClassRecord {
                    class_number: 502,
                    version: 1,
                    app_name: "\u{6f22}\u{5b57}App".into(), // CJK
                    cpp_class_name: "AcDb\u{571f}Class".into(),
                    dxf_record_name: "NONASCII".into(),
                    was_zombie: true,
                    item_class_id: 0x1f2,
                },
            ],
        };
        let mut buf = Vec::new();
        section.encode_with_maint(&mut buf, 4).unwrap();
        let parsed = ClassesSection::parse_with_maint(Version::R2010, &buf, 0, 4).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn placeholder_class_is_stable_and_recognised() {
        // The structural is_placeholder_class check must accept the
        // exact bytes the encoder emits and reject user-supplied
        // class records that share class_number 500.
        let p = placeholder_class();
        assert!(is_placeholder_class(&p));
        let almost = ClassRecord {
            cpp_class_name: "NotPlaceHolder".into(),
            ..p.clone()
        };
        assert!(!is_placeholder_class(&almost));
    }

    #[test]
    fn user_class_at_index_500_is_not_stripped() {
        // A user-supplied class with class_number == 500 but
        // different strings must NOT be misidentified as the
        // placeholder.
        let section = ClassesSection {
            version: Version::R2010,
            classes: vec![ClassRecord {
                class_number: 500,
                version: 0,
                app_name: "ObjectDBX Classes".into(),
                cpp_class_name: "AcDbWipeout".into(),
                dxf_record_name: "WIPEOUT".into(),
                was_zombie: false,
                item_class_id: 0x1f2,
            }],
        };
        let mut buf = Vec::new();
        section.encode_with_maint(&mut buf, 4).unwrap();
        let parsed = ClassesSection::parse_with_maint(Version::R2010, &buf, 0, 4).unwrap();
        assert_eq!(parsed, section, "user-supplied class wrongly stripped");
    }
}
