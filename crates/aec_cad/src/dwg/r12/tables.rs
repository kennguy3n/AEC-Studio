//! R12 (AC1009) table records.
//!
//! AutoCAD R12 keeps named tables (LAYER, BLOCK, LTYPE, STYLE, VIEW,
//! UCS, VPORT, DIMSTYLE, APPID) at fixed offsets recorded in the file
//! header (see [`super::header::R12FileHeader`]). Each table is laid
//! out as:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ RS  num_records                                             │
//! │ RS  record_size  — bytes per record (fixed per table kind)  │
//! │ <num_records × record_size> bytes                           │
//! │ RS  crc16        — CRC-X25 over the table preamble + data   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! `record_size` is encoded in the preamble so future versions can
//! extend table records without breaking forward-compat. For R12 each
//! table kind has the size LibreDWG documents in
//! `dwg.spec :: DWG_OBJECT_TABLE_HDR`.
//!
//! The string fields in every record are 32-byte zero-padded ASCII
//! (TF32). Names longer than 31 bytes are rejected; names shorter
//! than 32 bytes are zero-padded to fill the slot.

use crate::dwg::bits::crc::crc_x25;
use crate::dwg::error::{DwgError, DwgResult};

/// Fixed name field length used by every R12 table record.
pub const R12_NAME_LEN: usize = 32;

/// Layer table record (51 bytes per AC1009 spec).
///
/// Layout: 32-byte name, RC color, RS ltype_index, RC flag.
#[derive(Debug, Clone, PartialEq)]
pub struct R12LayerRecord {
    pub name: String,
    /// AutoCAD color index (signed: -1=invisible, 0=ByBlock, 1..=255).
    pub color: i16,
    /// Linetype table index (1-based).
    pub ltype_index: u16,
    /// Layer flags: 1=frozen, 2=locked, 4=frozen-on-new-vp, 64=in-use.
    pub flag: u8,
}

/// Block table record (61 bytes per AC1009 spec).
///
/// Layout: 32-byte name, RC flag, 2RD insertion_base, RD elevation,
/// RS num_entities, RL entities_section_offset.
#[derive(Debug, Clone, PartialEq)]
pub struct R12BlockRecord {
    pub name: String,
    pub flag: u8,
    pub insertion_base: [f64; 2],
    pub elevation: f64,
    pub num_entities: u16,
    pub entities_offset: u32,
}

/// Linetype table record (43 bytes minimum + dash array).
#[derive(Debug, Clone, PartialEq)]
pub struct R12LinetypeRecord {
    pub name: String,
    pub flag: u8,
    /// Total pattern length (sum of |dash| values).
    pub pattern_length: f64,
    /// Dash array (positive = dash, negative = gap, zero = dot).
    pub dashes: Vec<f64>,
}

/// Style (text-style) table record (50 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct R12StyleRecord {
    pub name: String,
    pub flag: u8,
    pub fixed_height: f64,
    pub width_factor: f64,
    pub oblique_angle: f64,
    pub generation: u8,
    pub last_height: f64,
}

/// View table record (88 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct R12ViewRecord {
    pub name: String,
    pub flag: u8,
    pub size: [f64; 2],
    pub center: [f64; 2],
    pub direction: [f64; 3],
    pub target: [f64; 3],
}

/// UCS table record (54 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct R12UcsRecord {
    pub name: String,
    pub flag: u8,
    pub origin: [f64; 3],
    pub x_axis: [f64; 3],
    pub y_axis: [f64; 3],
}

/// Viewport table record (72 bytes minimum).
#[derive(Debug, Clone, PartialEq)]
pub struct R12VportRecord {
    pub name: String,
    pub flag: u8,
    pub lower_left: [f64; 2],
    pub upper_right: [f64; 2],
    pub center: [f64; 2],
    pub view_size: f64,
    pub aspect_ratio: f64,
}

/// Dimension-style record (variable; we encode the subset our writer
/// needs end-to-end). Layout: 32-byte name + RC flag + RS num_doubles
/// + (num_doubles × RD).
#[derive(Debug, Clone, PartialEq)]
pub struct R12DimstyleRecord {
    pub name: String,
    pub flag: u8,
    /// Numeric DIM-style values in declaration order. The R12 dim
    /// style has 49 doubles; we don't tie the codec to a fixed count
    /// because real files have R12-vs-R13 quirks here.
    pub values: Vec<f64>,
}

/// AppID table record (35 bytes per AC1009 spec).
#[derive(Debug, Clone, PartialEq)]
pub struct R12AppIdRecord {
    pub name: String,
    pub flag: u8,
}

/// Logical table contents, version-agnostic.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct R12Tables {
    pub layers: Vec<R12LayerRecord>,
    pub blocks: Vec<R12BlockRecord>,
    pub linetypes: Vec<R12LinetypeRecord>,
    pub styles: Vec<R12StyleRecord>,
    pub views: Vec<R12ViewRecord>,
    pub ucs: Vec<R12UcsRecord>,
    pub vports: Vec<R12VportRecord>,
    pub dimstyles: Vec<R12DimstyleRecord>,
    pub appids: Vec<R12AppIdRecord>,
}

// ───────────────────────────── shared helpers ─────────────────────────────

fn write_name(buf: &mut Vec<u8>, name: &str) -> DwgResult<()> {
    let bytes = name.as_bytes();
    if bytes.len() >= R12_NAME_LEN {
        return Err(DwgError::InternalInvariant(format!(
            "R12 table name {name:?} is {} bytes (max {})",
            bytes.len(),
            R12_NAME_LEN - 1
        )));
    }
    buf.extend_from_slice(bytes);
    buf.extend(std::iter::repeat(0u8).take(R12_NAME_LEN - bytes.len()));
    Ok(())
}

fn read_name(bytes: &[u8], cur: &mut usize) -> DwgResult<String> {
    if *cur + R12_NAME_LEN > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + R12_NAME_LEN,
            bit: 0,
        });
    }
    let slice = &bytes[*cur..*cur + R12_NAME_LEN];
    *cur += R12_NAME_LEN;
    let end = slice.iter().position(|&b| b == 0).unwrap_or(R12_NAME_LEN);
    String::from_utf8(slice[..end].to_vec()).map_err(|e| DwgError::InvalidStringEncoding {
        field: "R12 table name (TF32)",
        message: e.to_string(),
    })
}

fn write_rd(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn read_rd(bytes: &[u8], cur: &mut usize) -> DwgResult<f64> {
    if *cur + 8 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 8,
            bit: 0,
        });
    }
    let mut z = [0u8; 8];
    z.copy_from_slice(&bytes[*cur..*cur + 8]);
    *cur += 8;
    Ok(f64::from_le_bytes(z))
}

fn write_2rd(buf: &mut Vec<u8>, p: [f64; 2]) {
    write_rd(buf, p[0]);
    write_rd(buf, p[1]);
}

fn read_2rd(bytes: &[u8], cur: &mut usize) -> DwgResult<[f64; 2]> {
    Ok([read_rd(bytes, cur)?, read_rd(bytes, cur)?])
}

fn write_3rd(buf: &mut Vec<u8>, p: [f64; 3]) {
    write_rd(buf, p[0]);
    write_rd(buf, p[1]);
    write_rd(buf, p[2]);
}

fn read_3rd(bytes: &[u8], cur: &mut usize) -> DwgResult<[f64; 3]> {
    Ok([
        read_rd(bytes, cur)?,
        read_rd(bytes, cur)?,
        read_rd(bytes, cur)?,
    ])
}

fn write_rs(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn read_rs(bytes: &[u8], cur: &mut usize) -> DwgResult<u16> {
    if *cur + 2 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 2,
            bit: 0,
        });
    }
    let v = u16::from_le_bytes([bytes[*cur], bytes[*cur + 1]]);
    *cur += 2;
    Ok(v)
}

fn read_rl(bytes: &[u8], cur: &mut usize) -> DwgResult<u32> {
    if *cur + 4 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 4,
            bit: 0,
        });
    }
    let v = u32::from_le_bytes([
        bytes[*cur],
        bytes[*cur + 1],
        bytes[*cur + 2],
        bytes[*cur + 3],
    ]);
    *cur += 4;
    Ok(v)
}

fn read_rc(bytes: &[u8], cur: &mut usize) -> DwgResult<u8> {
    if *cur >= bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 1,
            bit: 0,
        });
    }
    let v = bytes[*cur];
    *cur += 1;
    Ok(v)
}

// ───────────────────────────── per-record encoders ─────────────────────────────

// LAYER: name(TF32, 32) + color(RS signed, 2) + ltype_index(RS, 2) + flag(RC, 1) = 37
const LAYER_REC_SIZE: u16 = R12_NAME_LEN as u16 + 2 + 2 + 1;
const BLOCK_REC_SIZE: u16 = R12_NAME_LEN as u16 + 1 + 16 + 8 + 2 + 4; // 63
const STYLE_REC_SIZE: u16 = R12_NAME_LEN as u16 + 1 + 8 + 8 + 8 + 1 + 8; // 66
const VIEW_REC_SIZE: u16 = R12_NAME_LEN as u16 + 1 + 16 + 16 + 24 + 24; // 113
const UCS_REC_SIZE: u16 = R12_NAME_LEN as u16 + 1 + 24 + 24 + 24; // 105
const VPORT_REC_SIZE: u16 = R12_NAME_LEN as u16 + 1 + 16 + 16 + 16 + 8 + 8; // 97
const APPID_REC_SIZE: u16 = R12_NAME_LEN as u16 + 1; // 33

fn write_layer(buf: &mut Vec<u8>, rec: &R12LayerRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.extend_from_slice(&rec.color.to_le_bytes()); // RS color (signed)
    write_rs(buf, rec.ltype_index);
    buf.push(rec.flag);
    Ok(())
}

fn read_layer(bytes: &[u8], cur: &mut usize) -> DwgResult<R12LayerRecord> {
    let name = read_name(bytes, cur)?;
    let color = i16::from_le_bytes([read_rc(bytes, cur)?, read_rc(bytes, cur)?]);
    let ltype_index = read_rs(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    Ok(R12LayerRecord {
        name,
        color,
        ltype_index,
        flag,
    })
}

fn write_block(buf: &mut Vec<u8>, rec: &R12BlockRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    write_2rd(buf, rec.insertion_base);
    write_rd(buf, rec.elevation);
    write_rs(buf, rec.num_entities);
    buf.extend_from_slice(&rec.entities_offset.to_le_bytes());
    Ok(())
}

fn read_block(bytes: &[u8], cur: &mut usize) -> DwgResult<R12BlockRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let insertion_base = read_2rd(bytes, cur)?;
    let elevation = read_rd(bytes, cur)?;
    let num_entities = read_rs(bytes, cur)?;
    let entities_offset = read_rl(bytes, cur)?;
    Ok(R12BlockRecord {
        name,
        flag,
        insertion_base,
        elevation,
        num_entities,
        entities_offset,
    })
}

fn write_style(buf: &mut Vec<u8>, rec: &R12StyleRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    write_rd(buf, rec.fixed_height);
    write_rd(buf, rec.width_factor);
    write_rd(buf, rec.oblique_angle);
    buf.push(rec.generation);
    write_rd(buf, rec.last_height);
    Ok(())
}

fn read_style(bytes: &[u8], cur: &mut usize) -> DwgResult<R12StyleRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let fixed_height = read_rd(bytes, cur)?;
    let width_factor = read_rd(bytes, cur)?;
    let oblique_angle = read_rd(bytes, cur)?;
    let generation = read_rc(bytes, cur)?;
    let last_height = read_rd(bytes, cur)?;
    Ok(R12StyleRecord {
        name,
        flag,
        fixed_height,
        width_factor,
        oblique_angle,
        generation,
        last_height,
    })
}

fn write_view(buf: &mut Vec<u8>, rec: &R12ViewRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    write_2rd(buf, rec.size);
    write_2rd(buf, rec.center);
    write_3rd(buf, rec.direction);
    write_3rd(buf, rec.target);
    Ok(())
}

fn read_view(bytes: &[u8], cur: &mut usize) -> DwgResult<R12ViewRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let size = read_2rd(bytes, cur)?;
    let center = read_2rd(bytes, cur)?;
    let direction = read_3rd(bytes, cur)?;
    let target = read_3rd(bytes, cur)?;
    Ok(R12ViewRecord {
        name,
        flag,
        size,
        center,
        direction,
        target,
    })
}

fn write_ucs(buf: &mut Vec<u8>, rec: &R12UcsRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    write_3rd(buf, rec.origin);
    write_3rd(buf, rec.x_axis);
    write_3rd(buf, rec.y_axis);
    Ok(())
}

fn read_ucs(bytes: &[u8], cur: &mut usize) -> DwgResult<R12UcsRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let origin = read_3rd(bytes, cur)?;
    let x_axis = read_3rd(bytes, cur)?;
    let y_axis = read_3rd(bytes, cur)?;
    Ok(R12UcsRecord {
        name,
        flag,
        origin,
        x_axis,
        y_axis,
    })
}

fn write_vport(buf: &mut Vec<u8>, rec: &R12VportRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    write_2rd(buf, rec.lower_left);
    write_2rd(buf, rec.upper_right);
    write_2rd(buf, rec.center);
    write_rd(buf, rec.view_size);
    write_rd(buf, rec.aspect_ratio);
    Ok(())
}

fn read_vport(bytes: &[u8], cur: &mut usize) -> DwgResult<R12VportRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let lower_left = read_2rd(bytes, cur)?;
    let upper_right = read_2rd(bytes, cur)?;
    let center = read_2rd(bytes, cur)?;
    let view_size = read_rd(bytes, cur)?;
    let aspect_ratio = read_rd(bytes, cur)?;
    Ok(R12VportRecord {
        name,
        flag,
        lower_left,
        upper_right,
        center,
        view_size,
        aspect_ratio,
    })
}

fn write_appid(buf: &mut Vec<u8>, rec: &R12AppIdRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    Ok(())
}

fn read_appid(bytes: &[u8], cur: &mut usize) -> DwgResult<R12AppIdRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    Ok(R12AppIdRecord { name, flag })
}

// ─── LTYPE and DIMSTYLE are variable-length; they use per-record size prefixes. ───

fn write_linetype(buf: &mut Vec<u8>, rec: &R12LinetypeRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    write_rd(buf, rec.pattern_length);
    if rec.dashes.len() > u16::MAX as usize {
        return Err(DwgError::WriteOverflow {
            limit: u16::MAX as usize,
        });
    }
    write_rs(buf, rec.dashes.len() as u16);
    for d in &rec.dashes {
        write_rd(buf, *d);
    }
    Ok(())
}

fn read_linetype(bytes: &[u8], cur: &mut usize) -> DwgResult<R12LinetypeRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let pattern_length = read_rd(bytes, cur)?;
    let num_dashes = read_rs(bytes, cur)? as usize;
    let mut dashes = Vec::with_capacity(num_dashes);
    for _ in 0..num_dashes {
        dashes.push(read_rd(bytes, cur)?);
    }
    Ok(R12LinetypeRecord {
        name,
        flag,
        pattern_length,
        dashes,
    })
}

fn write_dimstyle(buf: &mut Vec<u8>, rec: &R12DimstyleRecord) -> DwgResult<()> {
    write_name(buf, &rec.name)?;
    buf.push(rec.flag);
    if rec.values.len() > u16::MAX as usize {
        return Err(DwgError::WriteOverflow {
            limit: u16::MAX as usize,
        });
    }
    write_rs(buf, rec.values.len() as u16);
    for v in &rec.values {
        write_rd(buf, *v);
    }
    Ok(())
}

fn read_dimstyle(bytes: &[u8], cur: &mut usize) -> DwgResult<R12DimstyleRecord> {
    let name = read_name(bytes, cur)?;
    let flag = read_rc(bytes, cur)?;
    let num_values = read_rs(bytes, cur)? as usize;
    let mut values = Vec::with_capacity(num_values);
    for _ in 0..num_values {
        values.push(read_rd(bytes, cur)?);
    }
    Ok(R12DimstyleRecord { name, flag, values })
}

// ───────────────────────────── table-level codecs ─────────────────────────────

/// Encode an entire fixed-size table (LAYER/BLOCK/STYLE/VIEW/UCS/VPORT/APPID).
fn encode_table_fixed<T, F>(
    section: &'static str,
    records: &[T],
    record_size: u16,
    mut write_one: F,
) -> DwgResult<Vec<u8>>
where
    F: FnMut(&mut Vec<u8>, &T) -> DwgResult<()>,
{
    if records.len() > u16::MAX as usize {
        return Err(DwgError::WriteOverflow {
            limit: u16::MAX as usize,
        });
    }
    let mut payload = Vec::with_capacity(4 + records.len() * record_size as usize + 2);
    write_rs(&mut payload, records.len() as u16);
    write_rs(&mut payload, record_size);
    for rec in records {
        let pos = payload.len();
        write_one(&mut payload, rec)?;
        let written = payload.len() - pos;
        if written != record_size as usize {
            return Err(DwgError::InternalInvariant(format!(
                "{section}: encoder wrote {written} bytes but record_size is {record_size}"
            )));
        }
    }
    let crc = crc_x25(0xc0c1, &payload);
    payload.extend_from_slice(&crc.to_le_bytes());
    Ok(payload)
}

/// Decode a fixed-size-record table (records that always read the
/// same number of bytes). The decoder cross-checks the declared
/// `record_size` against the expected wire size.
fn decode_table_fixed<T, F>(
    section: &'static str,
    bytes: &[u8],
    expected_record_size: u16,
    mut read_one: F,
) -> DwgResult<Vec<T>>
where
    F: FnMut(&[u8], &mut usize) -> DwgResult<T>,
{
    if bytes.len() < 6 {
        return Err(DwgError::UnexpectedEof { byte: 6, bit: 0 });
    }
    let mut cur = 0usize;
    let num = read_rs(bytes, &mut cur)? as usize;
    let record_size = read_rs(bytes, &mut cur)?;
    if record_size != expected_record_size {
        return Err(DwgError::MalformedObject {
            class: section.to_string(),
            offset: 2,
            message: format!(
                "declared record_size {record_size} != expected {expected_record_size}"
            ),
        });
    }
    let data_end = cur + num * record_size as usize;
    if data_end + 2 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: data_end + 2,
            bit: 0,
        });
    }
    let mut out = Vec::with_capacity(num);
    for _ in 0..num {
        let record_start = cur;
        let rec = read_one(bytes, &mut cur)?;
        let consumed = cur - record_start;
        if consumed != record_size as usize {
            return Err(DwgError::MalformedObject {
                class: section.to_string(),
                offset: record_start as u64,
                message: format!(
                    "per-record decoder consumed {consumed} bytes but record_size is {record_size}"
                ),
            });
        }
        out.push(rec);
    }
    let stored_crc = u16::from_le_bytes([bytes[data_end], bytes[data_end + 1]]);
    let computed_crc = crc_x25(0xc0c1, &bytes[..data_end]);
    if stored_crc != computed_crc {
        return Err(DwgError::SectionCrcMismatch {
            section: "r12_table",
            computed: u32::from(computed_crc),
            stored: u32::from(stored_crc),
        });
    }
    Ok(out)
}

/// Encode an entire variable-record table (LTYPE/DIMSTYLE). The
/// `record_size` field is set to 0 to flag variable-length records;
/// each record carries its own size internally.
fn encode_table_variable<T, F>(records: &[T], mut write_one: F) -> DwgResult<Vec<u8>>
where
    F: FnMut(&mut Vec<u8>, &T) -> DwgResult<()>,
{
    if records.len() > u16::MAX as usize {
        return Err(DwgError::WriteOverflow {
            limit: u16::MAX as usize,
        });
    }
    let mut payload = Vec::new();
    write_rs(&mut payload, records.len() as u16);
    write_rs(&mut payload, 0); // sentinel: variable-length
    for rec in records {
        // Record framing: RS size + body
        let size_pos = payload.len();
        payload.extend_from_slice(&[0u8, 0]);
        let body_start = payload.len();
        write_one(&mut payload, rec)?;
        let body_len = payload.len() - body_start;
        if body_len > u16::MAX as usize {
            return Err(DwgError::WriteOverflow {
                limit: u16::MAX as usize,
            });
        }
        let size_bytes = (body_len as u16).to_le_bytes();
        payload[size_pos] = size_bytes[0];
        payload[size_pos + 1] = size_bytes[1];
    }
    let crc = crc_x25(0xc0c1, &payload);
    payload.extend_from_slice(&crc.to_le_bytes());
    Ok(payload)
}

fn decode_table_variable<T, F>(
    section: &'static str,
    bytes: &[u8],
    mut read_one: F,
) -> DwgResult<Vec<T>>
where
    F: FnMut(&[u8], &mut usize) -> DwgResult<T>,
{
    if bytes.len() < 6 {
        return Err(DwgError::UnexpectedEof { byte: 6, bit: 0 });
    }
    let mut cur = 0usize;
    let num = read_rs(bytes, &mut cur)? as usize;
    let record_size = read_rs(bytes, &mut cur)?;
    if record_size != 0 {
        return Err(DwgError::MalformedObject {
            class: section.to_string(),
            offset: 2,
            message: format!(
                "expected variable-length sentinel (0) but got fixed-size {record_size}"
            ),
        });
    }
    let mut out = Vec::with_capacity(num);
    for _ in 0..num {
        let body_size = read_rs(bytes, &mut cur)? as usize;
        let body_end = cur + body_size;
        if body_end > bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: body_end,
                bit: 0,
            });
        }
        let pre_decode = cur;
        let rec = read_one(bytes, &mut cur)?;
        if cur != body_end {
            return Err(DwgError::MalformedObject {
                class: section.to_string(),
                offset: pre_decode as u64,
                message: format!(
                    "per-record decoder consumed {} bytes but body_size is {body_size}",
                    cur - pre_decode
                ),
            });
        }
        out.push(rec);
    }
    let crc_pos = cur;
    if cur + 2 > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: cur + 2,
            bit: 0,
        });
    }
    let stored_crc = u16::from_le_bytes([bytes[cur], bytes[cur + 1]]);
    let computed_crc = crc_x25(0xc0c1, &bytes[..crc_pos]);
    if stored_crc != computed_crc {
        return Err(DwgError::SectionCrcMismatch {
            section: "r12_table_var",
            computed: u32::from(computed_crc),
            stored: u32::from(stored_crc),
        });
    }
    Ok(out)
}

// ───────────────────────────── public table API ─────────────────────────────

pub fn encode_layer_table(records: &[R12LayerRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("LAYER", records, LAYER_REC_SIZE, write_layer)
}
pub fn decode_layer_table(bytes: &[u8]) -> DwgResult<Vec<R12LayerRecord>> {
    decode_table_fixed("LAYER", bytes, LAYER_REC_SIZE, read_layer)
}
pub fn encode_block_table(records: &[R12BlockRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("BLOCK", records, BLOCK_REC_SIZE, write_block)
}
pub fn decode_block_table(bytes: &[u8]) -> DwgResult<Vec<R12BlockRecord>> {
    decode_table_fixed("BLOCK", bytes, BLOCK_REC_SIZE, read_block)
}
pub fn encode_style_table(records: &[R12StyleRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("STYLE", records, STYLE_REC_SIZE, write_style)
}
pub fn decode_style_table(bytes: &[u8]) -> DwgResult<Vec<R12StyleRecord>> {
    decode_table_fixed("STYLE", bytes, STYLE_REC_SIZE, read_style)
}
pub fn encode_view_table(records: &[R12ViewRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("VIEW", records, VIEW_REC_SIZE, write_view)
}
pub fn decode_view_table(bytes: &[u8]) -> DwgResult<Vec<R12ViewRecord>> {
    decode_table_fixed("VIEW", bytes, VIEW_REC_SIZE, read_view)
}
pub fn encode_ucs_table(records: &[R12UcsRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("UCS", records, UCS_REC_SIZE, write_ucs)
}
pub fn decode_ucs_table(bytes: &[u8]) -> DwgResult<Vec<R12UcsRecord>> {
    decode_table_fixed("UCS", bytes, UCS_REC_SIZE, read_ucs)
}
pub fn encode_vport_table(records: &[R12VportRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("VPORT", records, VPORT_REC_SIZE, write_vport)
}
pub fn decode_vport_table(bytes: &[u8]) -> DwgResult<Vec<R12VportRecord>> {
    decode_table_fixed("VPORT", bytes, VPORT_REC_SIZE, read_vport)
}
pub fn encode_appid_table(records: &[R12AppIdRecord]) -> DwgResult<Vec<u8>> {
    encode_table_fixed("APPID", records, APPID_REC_SIZE, write_appid)
}
pub fn decode_appid_table(bytes: &[u8]) -> DwgResult<Vec<R12AppIdRecord>> {
    decode_table_fixed("APPID", bytes, APPID_REC_SIZE, read_appid)
}
pub fn encode_linetype_table(records: &[R12LinetypeRecord]) -> DwgResult<Vec<u8>> {
    encode_table_variable(records, write_linetype)
}
pub fn decode_linetype_table(bytes: &[u8]) -> DwgResult<Vec<R12LinetypeRecord>> {
    decode_table_variable("LTYPE", bytes, read_linetype)
}
pub fn encode_dimstyle_table(records: &[R12DimstyleRecord]) -> DwgResult<Vec<u8>> {
    encode_table_variable(records, write_dimstyle)
}
pub fn decode_dimstyle_table(bytes: &[u8]) -> DwgResult<Vec<R12DimstyleRecord>> {
    decode_table_variable("DIMSTYLE", bytes, read_dimstyle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_table_round_trips() {
        let recs = vec![
            R12LayerRecord {
                name: "0".to_string(),
                color: 7,
                ltype_index: 1,
                flag: 0,
            },
            R12LayerRecord {
                name: "WALLS".to_string(),
                color: 1,
                ltype_index: 2,
                flag: 0,
            },
            R12LayerRecord {
                name: "DIMS".to_string(),
                color: 3,
                ltype_index: 1,
                flag: 64, // in-use
            },
        ];
        let bytes = encode_layer_table(&recs).unwrap();
        let got = decode_layer_table(&bytes).unwrap();
        assert_eq!(got, recs);
    }

    #[test]
    fn block_table_round_trips() {
        let recs = vec![
            R12BlockRecord {
                name: "*MODEL_SPACE".to_string(),
                flag: 0,
                insertion_base: [0.0, 0.0],
                elevation: 0.0,
                num_entities: 0,
                entities_offset: 0,
            },
            R12BlockRecord {
                name: "DOOR".to_string(),
                flag: 0,
                insertion_base: [0.0, 0.0],
                elevation: 0.0,
                num_entities: 12,
                entities_offset: 0xdeadbeef,
            },
        ];
        let bytes = encode_block_table(&recs).unwrap();
        let got = decode_block_table(&bytes).unwrap();
        assert_eq!(got, recs);
    }

    #[test]
    fn style_table_round_trips() {
        let recs = vec![R12StyleRecord {
            name: "STANDARD".to_string(),
            flag: 0,
            fixed_height: 0.0,
            width_factor: 1.0,
            oblique_angle: 0.0,
            generation: 0,
            last_height: 2.5,
        }];
        let bytes = encode_style_table(&recs).unwrap();
        let got = decode_style_table(&bytes).unwrap();
        assert_eq!(got, recs);
    }

    #[test]
    fn linetype_table_round_trips_variable_length() {
        let recs = vec![
            R12LinetypeRecord {
                name: "CONTINUOUS".to_string(),
                flag: 0,
                pattern_length: 0.0,
                dashes: Vec::new(),
            },
            R12LinetypeRecord {
                name: "DASHED".to_string(),
                flag: 0,
                pattern_length: 12.0,
                dashes: vec![6.0, -6.0],
            },
            R12LinetypeRecord {
                name: "CENTER".to_string(),
                flag: 0,
                pattern_length: 22.0,
                dashes: vec![12.0, -3.0, 5.0, -2.0],
            },
        ];
        let bytes = encode_linetype_table(&recs).unwrap();
        let got = decode_linetype_table(&bytes).unwrap();
        assert_eq!(got, recs);
    }

    #[test]
    fn dimstyle_table_round_trips() {
        let recs = vec![R12DimstyleRecord {
            name: "STANDARD".to_string(),
            flag: 0,
            values: vec![1.0, 0.18, 0.0625, 0.18, 0.18, 0.0],
        }];
        let bytes = encode_dimstyle_table(&recs).unwrap();
        let got = decode_dimstyle_table(&bytes).unwrap();
        assert_eq!(got, recs);
    }

    #[test]
    fn view_ucs_vport_appid_tables_round_trip() {
        let view = vec![R12ViewRecord {
            name: "TOP".to_string(),
            flag: 0,
            size: [100.0, 80.0],
            center: [50.0, 40.0],
            direction: [0.0, 0.0, 1.0],
            target: [0.0, 0.0, 0.0],
        }];
        let bytes = encode_view_table(&view).unwrap();
        assert_eq!(decode_view_table(&bytes).unwrap(), view);

        let ucs = vec![R12UcsRecord {
            name: "WORLD".to_string(),
            flag: 0,
            origin: [0.0, 0.0, 0.0],
            x_axis: [1.0, 0.0, 0.0],
            y_axis: [0.0, 1.0, 0.0],
        }];
        let bytes = encode_ucs_table(&ucs).unwrap();
        assert_eq!(decode_ucs_table(&bytes).unwrap(), ucs);

        let vport = vec![R12VportRecord {
            name: "*ACTIVE".to_string(),
            flag: 0,
            lower_left: [0.0, 0.0],
            upper_right: [1.0, 1.0],
            center: [0.5, 0.5],
            view_size: 100.0,
            aspect_ratio: 1.0,
        }];
        let bytes = encode_vport_table(&vport).unwrap();
        assert_eq!(decode_vport_table(&bytes).unwrap(), vport);

        let appid = vec![R12AppIdRecord {
            name: "ACAD".to_string(),
            flag: 0,
        }];
        let bytes = encode_appid_table(&appid).unwrap();
        assert_eq!(decode_appid_table(&bytes).unwrap(), appid);
    }

    #[test]
    fn layer_table_rejects_crc_corruption() {
        let recs = vec![R12LayerRecord {
            name: "0".to_string(),
            color: 7,
            ltype_index: 1,
            flag: 0,
        }];
        let mut bytes = encode_layer_table(&recs).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        let err = decode_layer_table(&bytes).unwrap_err();
        assert!(
            matches!(err, DwgError::SectionCrcMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn layer_name_too_long_rejected() {
        let recs = vec![R12LayerRecord {
            name: "a".repeat(R12_NAME_LEN), // 32 == max name body, leaves no room for terminator
            color: 7,
            ltype_index: 1,
            flag: 0,
        }];
        let err = encode_layer_table(&recs).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)), "got {err:?}");
    }
}
