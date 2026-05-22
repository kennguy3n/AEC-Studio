//! Per-entity payload codecs for R12 (AC1009).
//!
//! Each codec takes (or returns) the entity-specific payload bytes
//! that follow the common appendix in an [`R12EntityRecord`]. The
//! codecs here intentionally do NOT touch the framing, the common
//! header, the appendix block, or the CRC — those live in
//! [`super::entity`] — so that we can change framing later (e.g. add
//! a fixture-derived variant) without rewriting every entity.
//!
//! The payload layouts match LibreDWG's `dwg.spec` PRE(R_13b1) branches:
//!
//! - **LINE** (`opcode=1`): `RD start.x, RD start.y, RD start.z, RD end.x,
//!   RD end.y, RD end.z` (48 bytes). If `opts & 1 != 0` an extra
//!   `RD extrusion.{x,y,z}` follows.
//! - **POINT** (`opcode=2`): `RD x, RD y, RD z` (24 bytes). If
//!   `opts & 1 != 0` then `RD extrusion.{x,y,z}` follows; if
//!   `opts & 2 != 0` then `RD x_angle` follows.
//! - **CIRCLE** (`opcode=3`): `RD center.x, RD center.y, RD center.z,
//!   RD radius` (32 bytes). If `opts & 1 != 0` then extrusion.
//! - **ARC** (`opcode=8`): `RD center.{x,y,z}, RD radius, RD
//!   start_angle, RD end_angle` (48 bytes). If `opts & 1 != 0` then
//!   extrusion.
//! - **TEXT** (`opcode=7`): `RD pos.{x,y,z}, RD height, TFv text`
//!   (variable). `opts` controls optional rotation, width factor,
//!   oblique, style index, generation, h-align, alignment point,
//!   extrusion, v-align.
//! - **INSERT** (`opcode=14`): `RS block_index, RD pos.{x,y,z}`
//!   (26 bytes minimum). `opts` controls optional scale x/y/z,
//!   rotation, columns, rows, col-spacing, row-spacing, extrusion.
//! - **BLOCK** (`opcode=12`): `RC flag, TFv name` (variable). Marks
//!   the start of a block-table entity sub-section.
//! - **ENDBLK** (`opcode=13`): empty payload.
//! - **POLYLINE** (`opcode=19`): `RC pline_flag, RS curvetype` (3
//!   bytes minimum). Vertices follow as separate VERTEX records.
//! - **VERTEX** (`opcode=20`): `RD pos.x, RD pos.y` (16 bytes minimum).
//!   `opts` controls bulge, start-width, end-width, flag, tangent.
//! - **SEQEND** (`opcode=17`): empty payload. Terminates a POLYLINE
//!   or INSERT/ATTRIB chain.
//!
//! All RD fields are 8-byte little-endian IEEE-754 doubles. All RS
//! fields are 2-byte little-endian unsigned shorts. All RC fields are
//! single bytes. TFv is `RS length + length bytes UTF-8` (the legacy
//! AutoCAD code-page; we always emit/accept the ASCII subset).

use crate::dwg::error::{DwgError, DwgResult};

// ─── INSERT opts_r11 bits (per LibreDWG `common_entity_data.spec`) ─────────
pub const INSERT_HAS_SCALE_X: u16 = 0x0001;
pub const INSERT_HAS_SCALE_Y: u16 = 0x0002;
pub const INSERT_HAS_ROTATION: u16 = 0x0004;
pub const INSERT_HAS_SCALE_Z: u16 = 0x0008;
#[allow(dead_code)]
pub const INSERT_HAS_NUM_COLS: u16 = 0x0010;
#[allow(dead_code)]
pub const INSERT_HAS_NUM_ROWS: u16 = 0x0020;
#[allow(dead_code)]
pub const INSERT_HAS_COL_SPACING: u16 = 0x0040;
#[allow(dead_code)]
pub const INSERT_HAS_ROW_SPACING: u16 = 0x0080;
#[allow(dead_code)]
pub const INSERT_HAS_EXTRUSION: u16 = 0x0100;

// ─── TEXT opts_r11 bits ────────────────────────────────────────────────────
pub const TEXT_HAS_ROTATION: u16 = 0x0001;
#[allow(dead_code)]
pub const TEXT_HAS_WIDTH_FACTOR: u16 = 0x0002;
#[allow(dead_code)]
pub const TEXT_HAS_OBLIQUE: u16 = 0x0004;
#[allow(dead_code)]
pub const TEXT_HAS_STYLE: u16 = 0x0008;
#[allow(dead_code)]
pub const TEXT_HAS_GENERATION: u16 = 0x0010;
#[allow(dead_code)]
pub const TEXT_HAS_HORIZ_ALIGN: u16 = 0x0020;
#[allow(dead_code)]
pub const TEXT_HAS_ALIGNMENT_POINT: u16 = 0x0040;
#[allow(dead_code)]
pub const TEXT_HAS_EXTRUSION: u16 = 0x0080;
#[allow(dead_code)]
pub const TEXT_ALIGNED_VERT_TO: u16 = 0x0100;

// ─── VERTEX opts_r11 bits ──────────────────────────────────────────────────
#[allow(dead_code)]
pub const VERTEX_HAS_START_WIDTH: u16 = 0x0001;
#[allow(dead_code)]
pub const VERTEX_HAS_END_WIDTH: u16 = 0x0002;
pub const VERTEX_HAS_BULGE: u16 = 0x0004;
#[allow(dead_code)]
pub const VERTEX_HAS_FLAG: u16 = 0x0008;

/// Decoded LINE entity payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12Line {
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub extrusion: Option<[f64; 3]>,
}

/// Decoded POINT entity payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12Point {
    pub position: [f64; 3],
    pub extrusion: Option<[f64; 3]>,
    pub x_angle: Option<f64>,
}

/// Decoded CIRCLE entity payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12Circle {
    pub center: [f64; 3],
    pub radius: f64,
    pub extrusion: Option<[f64; 3]>,
}

/// Decoded ARC entity payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12Arc {
    pub center: [f64; 3],
    pub radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
    pub extrusion: Option<[f64; 3]>,
}

/// Decoded TEXT entity payload.
#[derive(Debug, Clone, PartialEq)]
pub struct R12Text {
    pub position: [f64; 3],
    pub height: f64,
    pub text: String,
    pub rotation: Option<f64>,
}

/// Decoded INSERT entity payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12Insert {
    pub block_index: u16,
    pub position: [f64; 3],
    pub scale_x: Option<f64>,
    pub scale_y: Option<f64>,
    pub scale_z: Option<f64>,
    pub rotation: Option<f64>,
}

/// Decoded BLOCK header entity payload (starts a block table entry).
#[derive(Debug, Clone, PartialEq)]
pub struct R12Block {
    pub flag: u8,
    pub name: String,
}

/// Decoded POLYLINE entity payload (the chain header — vertices
/// follow as separate VERTEX records, terminated by SEQEND).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12PolylineHeader {
    pub flag: u8,
    pub curvetype: u16,
}

/// Decoded VERTEX entity payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R12Vertex {
    pub position: [f64; 2],
    pub bulge: Option<f64>,
}

// ───────────────────────────── helpers ─────────────────────────────

fn write_rd(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_3rd(buf: &mut Vec<u8>, p: [f64; 3]) {
    write_rd(buf, p[0]);
    write_rd(buf, p[1]);
    write_rd(buf, p[2]);
}

fn write_2rd(buf: &mut Vec<u8>, p: [f64; 2]) {
    write_rd(buf, p[0]);
    write_rd(buf, p[1]);
}

fn write_tfv(buf: &mut Vec<u8>, s: &str) -> DwgResult<()> {
    let bytes = s.as_bytes();
    if bytes.len() > u16::MAX as usize {
        return Err(DwgError::WriteOverflow {
            limit: u16::MAX as usize,
        });
    }
    buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(bytes);
    Ok(())
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

fn read_3rd(bytes: &[u8], cur: &mut usize) -> DwgResult<[f64; 3]> {
    Ok([
        read_rd(bytes, cur)?,
        read_rd(bytes, cur)?,
        read_rd(bytes, cur)?,
    ])
}

fn read_2rd(bytes: &[u8], cur: &mut usize) -> DwgResult<[f64; 2]> {
    Ok([read_rd(bytes, cur)?, read_rd(bytes, cur)?])
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

fn read_tfv(bytes: &[u8], cur: &mut usize) -> DwgResult<String> {
    let len = read_rs(bytes, cur)? as usize;
    if *cur + len > bytes.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + len,
            bit: 0,
        });
    }
    let slice = &bytes[*cur..*cur + len];
    *cur += len;
    String::from_utf8(slice.to_vec()).map_err(|e| DwgError::InvalidStringEncoding {
        field: "TFv body",
        message: e.to_string(),
    })
}

// ───────────────────────────── LINE ─────────────────────────────

pub fn encode_line(buf: &mut Vec<u8>, line: &R12Line) {
    write_3rd(buf, line.start);
    write_3rd(buf, line.end);
    if let Some(ext) = line.extrusion {
        write_3rd(buf, ext);
    }
}

pub fn decode_line(payload: &[u8], opts: u16) -> DwgResult<R12Line> {
    let mut cur = 0usize;
    let start = read_3rd(payload, &mut cur)?;
    let end = read_3rd(payload, &mut cur)?;
    let extrusion = if opts & 1 != 0 {
        Some(read_3rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "LINE".to_string(),
            offset: cur as u64,
            message: format!("trailing {} bytes after LINE payload", payload.len() - cur),
        });
    }
    Ok(R12Line {
        start,
        end,
        extrusion,
    })
}

// ───────────────────────────── POINT ─────────────────────────────

pub fn encode_point(buf: &mut Vec<u8>, point: &R12Point) {
    write_3rd(buf, point.position);
    if let Some(ext) = point.extrusion {
        write_3rd(buf, ext);
    }
    if let Some(ang) = point.x_angle {
        write_rd(buf, ang);
    }
}

pub fn decode_point(payload: &[u8], opts: u16) -> DwgResult<R12Point> {
    let mut cur = 0usize;
    let position = read_3rd(payload, &mut cur)?;
    let extrusion = if opts & 1 != 0 {
        Some(read_3rd(payload, &mut cur)?)
    } else {
        None
    };
    let x_angle = if opts & 2 != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "POINT".to_string(),
            offset: cur as u64,
            message: format!("trailing {} bytes after POINT payload", payload.len() - cur),
        });
    }
    Ok(R12Point {
        position,
        extrusion,
        x_angle,
    })
}

// ───────────────────────────── CIRCLE ─────────────────────────────

pub fn encode_circle(buf: &mut Vec<u8>, circle: &R12Circle) {
    write_3rd(buf, circle.center);
    write_rd(buf, circle.radius);
    if let Some(ext) = circle.extrusion {
        write_3rd(buf, ext);
    }
}

pub fn decode_circle(payload: &[u8], opts: u16) -> DwgResult<R12Circle> {
    let mut cur = 0usize;
    let center = read_3rd(payload, &mut cur)?;
    let radius = read_rd(payload, &mut cur)?;
    let extrusion = if opts & 1 != 0 {
        Some(read_3rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "CIRCLE".to_string(),
            offset: cur as u64,
            message: format!(
                "trailing {} bytes after CIRCLE payload",
                payload.len() - cur
            ),
        });
    }
    Ok(R12Circle {
        center,
        radius,
        extrusion,
    })
}

// ───────────────────────────── ARC ─────────────────────────────

pub fn encode_arc(buf: &mut Vec<u8>, arc: &R12Arc) {
    write_3rd(buf, arc.center);
    write_rd(buf, arc.radius);
    write_rd(buf, arc.start_angle);
    write_rd(buf, arc.end_angle);
    if let Some(ext) = arc.extrusion {
        write_3rd(buf, ext);
    }
}

pub fn decode_arc(payload: &[u8], opts: u16) -> DwgResult<R12Arc> {
    let mut cur = 0usize;
    let center = read_3rd(payload, &mut cur)?;
    let radius = read_rd(payload, &mut cur)?;
    let start_angle = read_rd(payload, &mut cur)?;
    let end_angle = read_rd(payload, &mut cur)?;
    let extrusion = if opts & 1 != 0 {
        Some(read_3rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "ARC".to_string(),
            offset: cur as u64,
            message: format!("trailing {} bytes after ARC payload", payload.len() - cur),
        });
    }
    Ok(R12Arc {
        center,
        radius,
        start_angle,
        end_angle,
        extrusion,
    })
}

// ───────────────────────────── TEXT ─────────────────────────────

pub fn encode_text(buf: &mut Vec<u8>, text: &R12Text) -> DwgResult<()> {
    write_3rd(buf, text.position);
    write_rd(buf, text.height);
    write_tfv(buf, &text.text)?;
    if let Some(rot) = text.rotation {
        write_rd(buf, rot);
    }
    Ok(())
}

pub fn decode_text(payload: &[u8], opts: u16) -> DwgResult<R12Text> {
    let mut cur = 0usize;
    let position = read_3rd(payload, &mut cur)?;
    let height = read_rd(payload, &mut cur)?;
    let text = read_tfv(payload, &mut cur)?;
    let rotation = if opts & TEXT_HAS_ROTATION != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "TEXT".to_string(),
            offset: cur as u64,
            message: format!("trailing {} bytes after TEXT payload", payload.len() - cur),
        });
    }
    Ok(R12Text {
        position,
        height,
        text,
        rotation,
    })
}

// ───────────────────────────── INSERT ─────────────────────────────

pub fn encode_insert(buf: &mut Vec<u8>, insert: &R12Insert) {
    buf.extend_from_slice(&insert.block_index.to_le_bytes());
    write_3rd(buf, insert.position);
    if let Some(sx) = insert.scale_x {
        write_rd(buf, sx);
    }
    if let Some(sy) = insert.scale_y {
        write_rd(buf, sy);
    }
    if let Some(rot) = insert.rotation {
        write_rd(buf, rot);
    }
    if let Some(sz) = insert.scale_z {
        write_rd(buf, sz);
    }
}

pub fn decode_insert(payload: &[u8], opts: u16) -> DwgResult<R12Insert> {
    let mut cur = 0usize;
    let block_index = read_rs(payload, &mut cur)?;
    let position = read_3rd(payload, &mut cur)?;
    let scale_x = if opts & INSERT_HAS_SCALE_X != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    let scale_y = if opts & INSERT_HAS_SCALE_Y != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    let rotation = if opts & INSERT_HAS_ROTATION != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    let scale_z = if opts & INSERT_HAS_SCALE_Z != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "INSERT".to_string(),
            offset: cur as u64,
            message: format!(
                "trailing {} bytes after INSERT payload",
                payload.len() - cur
            ),
        });
    }
    Ok(R12Insert {
        block_index,
        position,
        scale_x,
        scale_y,
        scale_z,
        rotation,
    })
}

// ───────────────────────────── BLOCK / ENDBLK ─────────────────────────────

pub fn encode_block(buf: &mut Vec<u8>, block: &R12Block) -> DwgResult<()> {
    buf.push(block.flag);
    write_tfv(buf, &block.name)
}

pub fn decode_block(payload: &[u8]) -> DwgResult<R12Block> {
    let mut cur = 0usize;
    let flag = read_rc(payload, &mut cur)?;
    let name = read_tfv(payload, &mut cur)?;
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "BLOCK".to_string(),
            offset: cur as u64,
            message: format!("trailing {} bytes after BLOCK payload", payload.len() - cur),
        });
    }
    Ok(R12Block { flag, name })
}

// ───────────────────────────── POLYLINE + VERTEX ─────────────────────────────

pub fn encode_polyline_header(buf: &mut Vec<u8>, header: &R12PolylineHeader) {
    buf.push(header.flag);
    buf.extend_from_slice(&header.curvetype.to_le_bytes());
}

pub fn decode_polyline_header(payload: &[u8]) -> DwgResult<R12PolylineHeader> {
    let mut cur = 0usize;
    let flag = read_rc(payload, &mut cur)?;
    let curvetype = read_rs(payload, &mut cur)?;
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "POLYLINE".to_string(),
            offset: cur as u64,
            message: format!(
                "trailing {} bytes after POLYLINE payload",
                payload.len() - cur
            ),
        });
    }
    Ok(R12PolylineHeader { flag, curvetype })
}

pub fn encode_vertex(buf: &mut Vec<u8>, vertex: &R12Vertex) {
    write_2rd(buf, vertex.position);
    if let Some(bulge) = vertex.bulge {
        write_rd(buf, bulge);
    }
}

pub fn decode_vertex(payload: &[u8], opts: u16) -> DwgResult<R12Vertex> {
    let mut cur = 0usize;
    let position = read_2rd(payload, &mut cur)?;
    let bulge = if opts & VERTEX_HAS_BULGE != 0 {
        Some(read_rd(payload, &mut cur)?)
    } else {
        None
    };
    if cur != payload.len() {
        return Err(DwgError::MalformedObject {
            class: "VERTEX".to_string(),
            offset: cur as u64,
            message: format!(
                "trailing {} bytes after VERTEX payload",
                payload.len() - cur
            ),
        });
    }
    Ok(R12Vertex { position, bulge })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_round_trips_with_and_without_extrusion() {
        for ext in [None, Some([0.0, 0.0, -1.0])] {
            let want = R12Line {
                start: [1.0, 2.0, 3.0],
                end: [4.0, 5.0, 6.0],
                extrusion: ext,
            };
            let mut buf = Vec::new();
            encode_line(&mut buf, &want);
            let opts = u16::from(ext.is_some());
            let got = decode_line(&buf, opts).unwrap();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn point_round_trips_with_optional_extrusion_and_x_angle() {
        let want = R12Point {
            position: [10.5, -2.5, 0.0],
            extrusion: Some([0.0, 0.0, 1.0]),
            x_angle: Some(std::f64::consts::FRAC_PI_4),
        };
        let mut buf = Vec::new();
        encode_point(&mut buf, &want);
        let got = decode_point(&buf, 0b11).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn circle_round_trips() {
        let want = R12Circle {
            center: [0.0, 0.0, 0.0],
            radius: 7.5,
            extrusion: None,
        };
        let mut buf = Vec::new();
        encode_circle(&mut buf, &want);
        let got = decode_circle(&buf, 0).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn arc_round_trips_with_extrusion() {
        let want = R12Arc {
            center: [1.0, 2.0, 0.0],
            radius: 5.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
            extrusion: Some([0.0, 0.0, 1.0]),
        };
        let mut buf = Vec::new();
        encode_arc(&mut buf, &want);
        let got = decode_arc(&buf, 1).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn text_round_trips_with_rotation() {
        let want = R12Text {
            position: [10.0, 20.0, 0.0],
            height: 2.5,
            text: "Hello R12".to_string(),
            rotation: Some(std::f64::consts::FRAC_PI_2),
        };
        let mut buf = Vec::new();
        encode_text(&mut buf, &want).unwrap();
        let got = decode_text(&buf, TEXT_HAS_ROTATION).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn insert_round_trips_with_all_optional_fields() {
        let want = R12Insert {
            block_index: 3,
            position: [100.0, 200.0, 0.0],
            scale_x: Some(2.0),
            scale_y: Some(3.0),
            scale_z: Some(4.0),
            rotation: Some(1.5),
        };
        let mut buf = Vec::new();
        encode_insert(&mut buf, &want);
        let opts =
            INSERT_HAS_SCALE_X | INSERT_HAS_SCALE_Y | INSERT_HAS_SCALE_Z | INSERT_HAS_ROTATION;
        let got = decode_insert(&buf, opts).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn block_round_trips() {
        let want = R12Block {
            flag: 0,
            name: "DOOR_03".to_string(),
        };
        let mut buf = Vec::new();
        encode_block(&mut buf, &want).unwrap();
        let got = decode_block(&buf).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn polyline_header_round_trips() {
        let want = R12PolylineHeader {
            flag: 0x01,
            curvetype: 5,
        };
        let mut buf = Vec::new();
        encode_polyline_header(&mut buf, &want);
        let got = decode_polyline_header(&buf).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn vertex_round_trips_with_bulge() {
        let want = R12Vertex {
            position: [5.0, 10.0],
            bulge: Some(0.5),
        };
        let mut buf = Vec::new();
        encode_vertex(&mut buf, &want);
        let got = decode_vertex(&buf, VERTEX_HAS_BULGE).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn decoders_reject_trailing_bytes() {
        // Append a stray byte after a valid LINE payload and confirm
        // the decoder rejects the misalignment.
        let line = R12Line {
            start: [0.0; 3],
            end: [1.0; 3],
            extrusion: None,
        };
        let mut buf = Vec::new();
        encode_line(&mut buf, &line);
        buf.push(0xff);
        let err = decode_line(&buf, 0).unwrap_err();
        assert!(
            matches!(err, DwgError::MalformedObject { .. }),
            "got {err:?}"
        );
    }
}
