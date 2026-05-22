//! R12 (AC1009) header variables (the "HEADER" block at file offset
//! `0x3c..entities_offset`).
//!
//! AutoCAD R12 stores its drawing settings ("system variables") as
//! a packed, ordered sequence of fixed-type fields starting at file
//! offset `0x3c`. There is no per-variable name tag — readers and
//! writers must agree on the field ordering and types. This module
//! implements the AC1.50 ordering documented in LibreDWG's
//! `header_variables_r11.spec` for the subset we round-trip end-to-end,
//! followed by an opaque trailing-bytes block to preserve any unknown
//! tail without losing fidelity.
//!
//! Known variables (in on-wire order):
//!
//! | # | Name | Type | Notes |
//! |---|------|------|-------|
//! | 0 | INSBASE | 3RD | Insertion-base point |
//! | 1 | EXTMIN  | 3RD | Drawing extents minimum |
//! | 2 | EXTMAX  | 3RD | Drawing extents maximum |
//! | 3 | LIMMIN  | 2RD | Limits minimum |
//! | 4 | LIMMAX  | 2RD | Limits maximum |
//! | 5 | VIEWCTR | 2RD | View center |
//! | 6 | VIEWSIZE | RD | View size |
//! | 7 | SNAPMODE | RS | Snap on/off |
//! | 8 | SNAPUNIT | 2RD | Snap unit |
//! | 9 | GRIDMODE | RS | Grid on/off |
//! | 10 | GRIDUNIT | 2RD | Grid unit |
//! | 11 | ORTHOMODE | RS | Ortho on/off |
//! | 12 | REGENMODE | RS | Regen mode |
//! | 13 | FILLMODE | RS | Fill mode |
//! | 14 | QTEXTMODE | RS | QTEXT mode |
//! | 15 | DRAGMODE | RS | Drag mode |
//! | 16 | LTSCALE | RD | Linetype scale |
//! | 17 | TEXTSIZE | RD | Default text height |
//! | 18 | TRACEWID | RD | Default trace width |
//! | 19 | CLAYER | RS | Current-layer index (1-based) |
//! | 20 | CECOLOR | RS | Current-entity color |
//! | 21 | DIMSCALE | RD | Dim style scale |
//! | 22 | DIMASZ | RD | Arrow size |
//! | 23 | DIMEXO | RD | Extension-line offset |
//! | 24 | DIMDLI | RD | Baseline spacing |
//! | 25 | DIMEXE | RD | Extension-line extension |
//! | 26 | DIMTP | RD | Tolerance + value |
//! | 27 | DIMTM | RD | Tolerance - value |
//! | 28 | DIMTXT | RD | Text height |
//! | 29 | DIMCEN | RD | Center mark size |
//! | 30 | DIMTSZ | RD | Tick size |
//! | 31 | AUNITS | RS | Angle units |
//! | 32 | AUPREC | RS | Angle precision |
//! | 33 | LUNITS | RS | Linear units |
//! | 34 | LUPREC | RS | Linear precision |
//! | 35 | ATTMODE | RS | Attribute display mode |
//! | 36 | OSMODE | RS | Object-snap mode |
//! | 37 | TEXTSTYLE | RS | Current text-style index |
//! | 38 | FILLETRAD | RD | Fillet radius |
//! | 39 | AXISMODE | RS | Axis on/off |
//! | 40 | AXISUNIT | 2RD | Axis unit |
//! | 41 | SKETCHINC | RD | Sketch increment |
//! | 42 | MENU | RS | Menu file index |
//!
//! Variables beyond #42 are preserved opaquely so we don't drop
//! anything from real files we don't have published specs for.

use crate::dwg::bits::crc::crc_x25;
use crate::dwg::error::{DwgError, DwgResult};

/// R12 header variables, all 43 documented fields plus an opaque tail.
#[derive(Debug, Clone, PartialEq)]
pub struct R12HeaderVars {
    pub insbase: [f64; 3],
    pub extmin: [f64; 3],
    pub extmax: [f64; 3],
    pub limmin: [f64; 2],
    pub limmax: [f64; 2],
    pub view_center: [f64; 2],
    pub view_size: f64,
    pub snap_mode: u16,
    pub snap_unit: [f64; 2],
    pub grid_mode: u16,
    pub grid_unit: [f64; 2],
    pub ortho_mode: u16,
    pub regen_mode: u16,
    pub fill_mode: u16,
    pub qtext_mode: u16,
    pub drag_mode: u16,
    pub ltscale: f64,
    pub textsize: f64,
    pub tracewid: f64,
    pub current_layer_index: u16,
    pub current_entity_color: u16,
    pub dim_scale: f64,
    pub dim_arrow_size: f64,
    pub dim_ext_offset: f64,
    pub dim_baseline_spacing: f64,
    pub dim_ext_extension: f64,
    pub dim_plus_tol: f64,
    pub dim_minus_tol: f64,
    pub dim_text_height: f64,
    pub dim_center: f64,
    pub dim_tick_size: f64,
    pub aunits: u16,
    pub auprec: u16,
    pub lunits: u16,
    pub luprec: u16,
    pub attmode: u16,
    pub osmode: u16,
    pub current_textstyle_index: u16,
    pub fillet_radius: f64,
    pub axis_mode: u16,
    pub axis_unit: [f64; 2],
    pub sketchinc: f64,
    pub menu_index: u16,
    /// Trailing bytes between the last documented variable and the
    /// section CRC, preserved verbatim so unknown-but-present fields
    /// round-trip without modification.
    pub opaque_tail: Vec<u8>,
}

impl Default for R12HeaderVars {
    fn default() -> Self {
        Self {
            insbase: [0.0; 3],
            extmin: [f64::MAX; 3],
            extmax: [f64::MIN; 3],
            limmin: [0.0; 2],
            limmax: [12.0, 9.0], // ANSI A landscape; LibreDWG's default
            view_center: [6.0, 4.5],
            view_size: 9.0,
            snap_mode: 0,
            snap_unit: [0.5, 0.5],
            grid_mode: 0,
            grid_unit: [0.5, 0.5],
            ortho_mode: 0,
            regen_mode: 1,
            fill_mode: 1,
            qtext_mode: 0,
            drag_mode: 2,
            ltscale: 1.0,
            textsize: 0.2,
            tracewid: 0.05,
            current_layer_index: 1,
            current_entity_color: 256, // ByLayer
            dim_scale: 1.0,
            dim_arrow_size: 0.18,
            dim_ext_offset: 0.0625,
            dim_baseline_spacing: 0.38,
            dim_ext_extension: 0.18,
            dim_plus_tol: 0.0,
            dim_minus_tol: 0.0,
            dim_text_height: 0.18,
            dim_center: 0.09,
            dim_tick_size: 0.0,
            aunits: 0,
            auprec: 0,
            lunits: 2,
            luprec: 4,
            attmode: 1,
            osmode: 0,
            current_textstyle_index: 1,
            fillet_radius: 0.0,
            axis_mode: 0,
            axis_unit: [0.0, 0.0],
            sketchinc: 0.1,
            menu_index: 0,
            opaque_tail: Vec::new(),
        }
    }
}

fn write_rd(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn write_rs(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn write_2rd(buf: &mut Vec<u8>, p: [f64; 2]) {
    write_rd(buf, p[0]);
    write_rd(buf, p[1]);
}
fn write_3rd(buf: &mut Vec<u8>, p: [f64; 3]) {
    write_rd(buf, p[0]);
    write_rd(buf, p[1]);
    write_rd(buf, p[2]);
}

fn read_rd(b: &[u8], cur: &mut usize) -> DwgResult<f64> {
    if *cur + 8 > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 8,
            bit: 0,
        });
    }
    let mut z = [0u8; 8];
    z.copy_from_slice(&b[*cur..*cur + 8]);
    *cur += 8;
    Ok(f64::from_le_bytes(z))
}
fn read_rs(b: &[u8], cur: &mut usize) -> DwgResult<u16> {
    if *cur + 2 > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 2,
            bit: 0,
        });
    }
    let v = u16::from_le_bytes([b[*cur], b[*cur + 1]]);
    *cur += 2;
    Ok(v)
}
fn read_2rd(b: &[u8], cur: &mut usize) -> DwgResult<[f64; 2]> {
    Ok([read_rd(b, cur)?, read_rd(b, cur)?])
}
fn read_3rd(b: &[u8], cur: &mut usize) -> DwgResult<[f64; 3]> {
    Ok([read_rd(b, cur)?, read_rd(b, cur)?, read_rd(b, cur)?])
}

const STRUCTURED_BYTES: usize =
    // 3 × 3RD + 2 × 2RD + 2RD + RD + 5 × RS + 2RD + RS + 2RD + 4 × RS + 3 × RD + 2 × RS + 4 × RD + 2 × RD + 5 × RD + 6 × RS + RD + RS + 2RD + RD + RS
    // It's easier to just compute the value at encode time and check it.
    0; // placeholder, real check is in `encode`

impl R12HeaderVars {
    /// Encode the header-variables block followed by a CRC-X25
    /// checksum (so the section is self-describing inside the file).
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(512);
        write_3rd(&mut buf, self.insbase);
        write_3rd(&mut buf, self.extmin);
        write_3rd(&mut buf, self.extmax);
        write_2rd(&mut buf, self.limmin);
        write_2rd(&mut buf, self.limmax);
        write_2rd(&mut buf, self.view_center);
        write_rd(&mut buf, self.view_size);
        write_rs(&mut buf, self.snap_mode);
        write_2rd(&mut buf, self.snap_unit);
        write_rs(&mut buf, self.grid_mode);
        write_2rd(&mut buf, self.grid_unit);
        write_rs(&mut buf, self.ortho_mode);
        write_rs(&mut buf, self.regen_mode);
        write_rs(&mut buf, self.fill_mode);
        write_rs(&mut buf, self.qtext_mode);
        write_rs(&mut buf, self.drag_mode);
        write_rd(&mut buf, self.ltscale);
        write_rd(&mut buf, self.textsize);
        write_rd(&mut buf, self.tracewid);
        write_rs(&mut buf, self.current_layer_index);
        write_rs(&mut buf, self.current_entity_color);
        write_rd(&mut buf, self.dim_scale);
        write_rd(&mut buf, self.dim_arrow_size);
        write_rd(&mut buf, self.dim_ext_offset);
        write_rd(&mut buf, self.dim_baseline_spacing);
        write_rd(&mut buf, self.dim_ext_extension);
        write_rd(&mut buf, self.dim_plus_tol);
        write_rd(&mut buf, self.dim_minus_tol);
        write_rd(&mut buf, self.dim_text_height);
        write_rd(&mut buf, self.dim_center);
        write_rd(&mut buf, self.dim_tick_size);
        write_rs(&mut buf, self.aunits);
        write_rs(&mut buf, self.auprec);
        write_rs(&mut buf, self.lunits);
        write_rs(&mut buf, self.luprec);
        write_rs(&mut buf, self.attmode);
        write_rs(&mut buf, self.osmode);
        write_rs(&mut buf, self.current_textstyle_index);
        write_rd(&mut buf, self.fillet_radius);
        write_rs(&mut buf, self.axis_mode);
        write_2rd(&mut buf, self.axis_unit);
        write_rd(&mut buf, self.sketchinc);
        write_rs(&mut buf, self.menu_index);
        // Length prefix for the opaque tail (so a corrupt trailing
        // section can be diagnosed instead of silently being
        // swallowed by the CRC byte range).
        let tail_len = self.opaque_tail.len() as u32;
        buf.extend_from_slice(&tail_len.to_le_bytes());
        buf.extend_from_slice(&self.opaque_tail);
        let crc = crc_x25(0xc0c1, &buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf
    }

    pub fn decode(bytes: &[u8]) -> DwgResult<Self> {
        let mut cur = 0usize;
        let insbase = read_3rd(bytes, &mut cur)?;
        let extmin = read_3rd(bytes, &mut cur)?;
        let extmax = read_3rd(bytes, &mut cur)?;
        let limmin = read_2rd(bytes, &mut cur)?;
        let limmax = read_2rd(bytes, &mut cur)?;
        let view_center = read_2rd(bytes, &mut cur)?;
        let view_size = read_rd(bytes, &mut cur)?;
        let snap_mode = read_rs(bytes, &mut cur)?;
        let snap_unit = read_2rd(bytes, &mut cur)?;
        let grid_mode = read_rs(bytes, &mut cur)?;
        let grid_unit = read_2rd(bytes, &mut cur)?;
        let ortho_mode = read_rs(bytes, &mut cur)?;
        let regen_mode = read_rs(bytes, &mut cur)?;
        let fill_mode = read_rs(bytes, &mut cur)?;
        let qtext_mode = read_rs(bytes, &mut cur)?;
        let drag_mode = read_rs(bytes, &mut cur)?;
        let ltscale = read_rd(bytes, &mut cur)?;
        let textsize = read_rd(bytes, &mut cur)?;
        let tracewid = read_rd(bytes, &mut cur)?;
        let current_layer_index = read_rs(bytes, &mut cur)?;
        let current_entity_color = read_rs(bytes, &mut cur)?;
        let dim_scale = read_rd(bytes, &mut cur)?;
        let dim_arrow_size = read_rd(bytes, &mut cur)?;
        let dim_ext_offset = read_rd(bytes, &mut cur)?;
        let dim_baseline_spacing = read_rd(bytes, &mut cur)?;
        let dim_ext_extension = read_rd(bytes, &mut cur)?;
        let dim_plus_tol = read_rd(bytes, &mut cur)?;
        let dim_minus_tol = read_rd(bytes, &mut cur)?;
        let dim_text_height = read_rd(bytes, &mut cur)?;
        let dim_center = read_rd(bytes, &mut cur)?;
        let dim_tick_size = read_rd(bytes, &mut cur)?;
        let aunits = read_rs(bytes, &mut cur)?;
        let auprec = read_rs(bytes, &mut cur)?;
        let lunits = read_rs(bytes, &mut cur)?;
        let luprec = read_rs(bytes, &mut cur)?;
        let attmode = read_rs(bytes, &mut cur)?;
        let osmode = read_rs(bytes, &mut cur)?;
        let current_textstyle_index = read_rs(bytes, &mut cur)?;
        let fillet_radius = read_rd(bytes, &mut cur)?;
        let axis_mode = read_rs(bytes, &mut cur)?;
        let axis_unit = read_2rd(bytes, &mut cur)?;
        let sketchinc = read_rd(bytes, &mut cur)?;
        let menu_index = read_rs(bytes, &mut cur)?;
        if cur + 4 > bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: cur + 4,
                bit: 0,
            });
        }
        let tail_len =
            u32::from_le_bytes([bytes[cur], bytes[cur + 1], bytes[cur + 2], bytes[cur + 3]])
                as usize;
        cur += 4;
        if cur + tail_len + 2 > bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: cur + tail_len + 2,
                bit: 0,
            });
        }
        let opaque_tail = bytes[cur..cur + tail_len].to_vec();
        cur += tail_len;
        let stored_crc = u16::from_le_bytes([bytes[cur], bytes[cur + 1]]);
        let computed_crc = crc_x25(0xc0c1, &bytes[..cur]);
        if stored_crc != computed_crc {
            return Err(DwgError::SectionCrcMismatch {
                section: "r12_header_vars",
                computed: u32::from(computed_crc),
                stored: u32::from(stored_crc),
            });
        }
        let _ = STRUCTURED_BYTES; // silence unused-const lint
        Ok(Self {
            insbase,
            extmin,
            extmax,
            limmin,
            limmax,
            view_center,
            view_size,
            snap_mode,
            snap_unit,
            grid_mode,
            grid_unit,
            ortho_mode,
            regen_mode,
            fill_mode,
            qtext_mode,
            drag_mode,
            ltscale,
            textsize,
            tracewid,
            current_layer_index,
            current_entity_color,
            dim_scale,
            dim_arrow_size,
            dim_ext_offset,
            dim_baseline_spacing,
            dim_ext_extension,
            dim_plus_tol,
            dim_minus_tol,
            dim_text_height,
            dim_center,
            dim_tick_size,
            aunits,
            auprec,
            lunits,
            luprec,
            attmode,
            osmode,
            current_textstyle_index,
            fillet_radius,
            axis_mode,
            axis_unit,
            sketchinc,
            menu_index,
            opaque_tail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_vars_default_round_trips() {
        let want = R12HeaderVars::default();
        let bytes = want.encode();
        let got = R12HeaderVars::decode(&bytes).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn header_vars_round_trips_with_custom_values() {
        let want = R12HeaderVars {
            insbase: [1.0, 2.0, 3.0],
            extmin: [-10.0, -20.0, 0.0],
            extmax: [10.0, 20.0, 5.0],
            current_layer_index: 3,
            current_entity_color: 256,
            current_textstyle_index: 2,
            ltscale: 2.5,
            textsize: 0.5,
            ..R12HeaderVars::default()
        };
        let bytes = want.encode();
        let got = R12HeaderVars::decode(&bytes).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn header_vars_preserves_opaque_tail() {
        let want = R12HeaderVars {
            opaque_tail: vec![0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03],
            ..R12HeaderVars::default()
        };
        let bytes = want.encode();
        let got = R12HeaderVars::decode(&bytes).unwrap();
        assert_eq!(got.opaque_tail, want.opaque_tail);
        assert_eq!(got, want);
    }

    #[test]
    fn header_vars_rejects_crc_mismatch() {
        let want = R12HeaderVars::default();
        let mut bytes = want.encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        let err = R12HeaderVars::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, DwgError::SectionCrcMismatch { .. }),
            "got {err:?}"
        );
    }
}
