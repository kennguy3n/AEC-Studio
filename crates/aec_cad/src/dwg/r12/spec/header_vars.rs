//! AC1009 header variables — the full `numheader_vars = 205` set.
//!
//! Encodes the byte sequence LibreDWG's `decode_preR13_header_variables`
//! consumes (see `header_variables_r11.spec` in LibreDWG, which is
//! the single source of truth for this layout). The encoding is
//! straight byte-aligned little-endian for all numeric fields; the
//! big variants are:
//!
//! * `RC` — 1 byte
//! * `RS` — 2 bytes LE
//! * `RL` — 4 bytes LE
//! * `RD` — 8 bytes LE (IEEE-754 double)
//! * `2RD` — two consecutive `RD`s (16 bytes)
//! * `3RD` — three consecutive `RD`s (24 bytes)
//! * `RLL_BE` — 8 bytes big-endian (used only by `HANDSEED`)
//! * `TFv(n)` — `n` bytes, ASCII string padded with NULs (or all-NUL)
//! * `TIMERLL` — two `RL`s (days, ms), 8 bytes total
//! * Handle `code=2` — 2-byte LE signed index (`-1 = 0xFFFF` = null)
//!
//! For R12 (`numheader_vars = 205`) every gated `if (... <= N) return 0`
//! check is bypassed, so we emit all fields including `VISRETAIN`
//! at the tail.
//!
//! Five of the ten section-table descriptors are emitted INSIDE this
//! block at the offsets called out in the spec source:
//!
//! * `UCS`      — after `MIRRTEXT`            (spec line 269)
//! * `VPORT`    — after `SURFTAB2`            (spec line 326)
//! * `APPID`    — after `UCSNAME`/start of >158 region (spec line 333)
//! * `DIMSTYLE` — after `unknown_520`         (spec line 339)
//! * `VX`       — after `PINSBASE`            (spec line 376)
//!
//! [`R12HeaderVars::encode_with_section_tables`] takes a
//! [`crate::dwg::r12::spec::section_table::R12SectionTables`] reference
//! so the descriptors can be written at the right positions.

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::r12::spec::section_table::{
    R12SectionTables, SectionTableHeader, SECTION_TABLE_HEADER_LEN,
};

/// AC1009 header variables. Every field is owned here; defaults are
/// what fresh-canvas AutoCAD files emit (`limmax = (12.0, 9.0)` etc.).
#[derive(Debug, Clone, PartialEq)]
pub struct R12HeaderVars {
    // --- always emitted (R2.0+) ---
    pub insbase: [f64; 3],
    pub plinegen: u16,
    pub extmin: [f64; 3],
    pub extmax: [f64; 3],
    pub limmin: [f64; 2],
    pub limmax: [f64; 2],
    pub viewctr: [f64; 3],
    pub viewsize: f64,
    pub snapmode: u16,
    pub snapunit: [f64; 2],
    pub snapbase: [f64; 2],
    pub snapang: f64,
    pub snapstyle: u16,
    pub snapisopair: u16,
    pub gridmode: u16,
    pub gridunit: [f64; 2],
    pub orthomode: u16,
    pub regenmode: u16,
    pub fillmode: u16,
    pub qtextmode: u16,
    pub dragmode: u16,
    pub ltscale: f64,
    pub textsize: f64,
    pub tracewid: f64,
    /// CLAYER handle index. Null = `-1` (`0xFFFF`).
    pub clayer: i16,
    pub old_cecolor_lo: u32,
    pub old_cecolor_hi: u32,
    pub unknown_5: u16,
    pub psltscale: u16,
    pub treedepth: u16,
    pub unknown_6: u16,
    pub aspect_ratio: f64,
    pub lunits: u16,
    pub luprec: u16,
    pub axismode: u16,
    pub axisunit: [f64; 2],
    pub sketchinc: f64,
    pub filletrad: f64,
    pub aunits: u16,
    pub auprec: u16,
    pub textstyle: i16,
    pub osmode: u16,
    pub attmode: u16,
    /// MENU — fixed 15 bytes, NUL-padded ASCII.
    pub menu: [u8; 15],
    pub dimscale: f64,
    pub dimasz: f64,
    pub dimexo: f64,
    pub dimdli: f64,
    pub dimexe: f64,
    pub dimtp: f64,
    pub dimtm: f64,
    pub dimtxt: f64,
    pub dimcen: f64,
    pub dimtsz: f64,
    // --- numheader_vars > 74 ---
    pub dimtol: u8,
    pub dimlim: u8,
    pub dimtih: u8,
    pub dimtoh: u8,
    pub dimse1: u8,
    pub dimse2: u8,
    pub dimtad: u8,
    pub limcheck: u8,
    /// MENUEXT — fixed 46 bytes (NOT null-terminated).
    pub menuext: [u8; 46],
    pub elevation: f64,
    pub thickness: f64,
    pub viewdir: [f64; 3],
    pub vpoint_x: [f64; 3],
    pub vpoint_y: [f64; 3],
    pub vpoint_z: [f64; 3],
    pub vpoint_x_alt: [f64; 3],
    pub vpoint_y_alt: [f64; 3],
    pub vpoint_z_alt: [f64; 3],
    pub flag_3d: u16,
    pub blipmode: u16,
    // --- numheader_vars > 83 ---
    pub dimzin: u8,
    pub dimrnd: f64,
    pub dimdle: f64,
    pub dimblk_t: [u8; 33],
    pub circle_zoom: u16,
    pub coords: u16,
    pub cecolor_index: u16,
    pub celtype: i16,
    pub tdcreate: [u32; 2],
    pub tdupdate: [u32; 2],
    pub tdindwg: [u32; 2],
    pub tdusrtimer: [u32; 2],
    pub usrtimer: u16,
    pub fastzoom: u16,
    pub skpoly: u16,
    pub unknown_mon: u16,
    pub unknown_day: u16,
    pub unknown_year: u16,
    pub unknown_hour: u16,
    pub unknown_min: u16,
    pub unknown_sec: u16,
    pub unknown_ms: u16,
    pub angbase: f64,
    pub angdir: u16,
    // --- numheader_vars > 101 ---
    pub pdmode: u16,
    pub pdsize: f64,
    pub plinewid: f64,
    // --- numheader_vars > 104 ---
    pub useri1: i16,
    pub useri2: i16,
    pub useri3: i16,
    pub useri4: i16,
    pub useri5: i16,
    pub userr1: f64,
    pub userr2: f64,
    pub userr3: f64,
    pub userr4: f64,
    pub userr5: f64,
    // --- numheader_vars > 114 ---
    pub dimalt: u8,
    pub dimaltd: u8,
    pub dimaso: u8,
    pub dimsho: u8,
    pub dimpost: [u8; 16],
    pub dimapost: [u8; 16],
    // --- numheader_vars > 120 ---
    pub dimaltf: f64,
    pub dimlfac: f64,
    // --- numheader_vars > 122 ---
    pub splinesegs: u16,
    pub splframe: u16,
    pub attreq: u16,
    pub attdia: u16,
    pub chamfera: f64,
    pub chamferb: f64,
    pub mirrtext: u16,
    // --- numheader_vars > 129 (PRER13_SECTION_HDR(UCS) here) ---
    pub codepage: u16,
    pub ucsorg: [f64; 3],
    pub ucsxdir: [f64; 3],
    pub ucsydir: [f64; 3],
    pub target: [f64; 3],
    pub lenslength: f64,
    pub viewtwist: f64,
    pub frontz: f64,
    pub backz: f64,
    pub viewmode: u16,
    pub dimtofl: u8,
    pub dimblk1_t: [u8; 33],
    pub dimblk2_t: [u8; 33],
    pub dimsah: u8,
    pub dimtix: u8,
    pub dimsoxd: u8,
    pub dimtvp: f64,
    pub unknown_string: [u8; 33],
    pub handling: u16,
    pub handseed: u64,
    pub surfu: u16,
    pub surfv: u16,
    pub surftype: u16,
    pub surftab1: u16,
    pub surftab2: u16,
    // --- PRER13_SECTION_HDR(VPORT) here ---
    pub flatland: u16,
    pub splinetype: u16,
    pub ucsicon: u16,
    pub ucsname: i16,
    // --- numheader_vars > 158 (PRER13_SECTION_HDR(APPID) here) ---
    pub worldview: u16,
    // --- numheader_vars > 160 ---
    pub unknown_51e: u16,
    pub unknown_520: u16,
    // --- PRER13_SECTION_HDR(DIMSTYLE) here ---
    pub unknown_52c: i16,
    pub unknown_52e: u16,
    pub unknown_530: u8,
    pub dimclrd_c: u16,
    pub dimclre_c: u16,
    pub dimclrt_c: u16,
    pub shadedge: u16,
    pub shadedif: u16,
    pub unknown_59: u16,
    pub unitmode: u16,
    pub unit1_ratio: f64,
    pub unit2_ratio: f64,
    pub unit3_ratio: f64,
    pub unit4_ratio: f64,
    pub unit1_name: [u8; 32],
    pub unit2_name: [u8; 32],
    pub unit3_name: [u8; 32],
    pub unit4_name: [u8; 32],
    pub dimtfac: f64,
    pub pucsorg: [f64; 3],
    pub pucsxdir: [f64; 3],
    pub pucsydir: [f64; 3],
    pub pucsname: i16,
    pub tilemode: u16,
    pub plimcheck: u16,
    pub unknown_10: u16,
    pub pextmin: [f64; 3],
    pub pextmax: [f64; 3],
    pub plimmin: [f64; 2],
    pub plimmax: [f64; 2],
    pub pinsbase: [f64; 3],
    // --- PRER13_SECTION_HDR(VX) here ---
    pub maxactvp: u16,
    pub dimgap: f64,
    pub pelevation: f64,
    // --- numheader_vars > 204 ---
    pub visretain: u16,
}

impl Default for R12HeaderVars {
    /// Defaults match what LibreDWG emits for a freshly-initialised
    /// R12 header (see `dwg.spec` and the `init_dwg` helpers).
    fn default() -> Self {
        Self {
            insbase: [0.0; 3],
            plinegen: 0,
            extmin: [f64::MAX; 3],
            extmax: [f64::MIN; 3],
            limmin: [0.0; 2],
            limmax: [12.0, 9.0],
            viewctr: [6.0, 4.5, 0.0],
            viewsize: 9.0,
            snapmode: 0,
            snapunit: [0.5, 0.5],
            snapbase: [0.0, 0.0],
            snapang: 0.0,
            snapstyle: 0,
            snapisopair: 0,
            gridmode: 0,
            gridunit: [0.5, 0.5],
            orthomode: 0,
            regenmode: 1,
            fillmode: 1,
            qtextmode: 0,
            dragmode: 2,
            ltscale: 1.0,
            textsize: 0.2,
            tracewid: 0.05,
            clayer: -1,
            old_cecolor_lo: 0,
            old_cecolor_hi: 0,
            unknown_5: 0,
            psltscale: 1,
            treedepth: 3020,
            unknown_6: 0,
            aspect_ratio: 0.0,
            lunits: 2,
            luprec: 4,
            axismode: 0,
            axisunit: [0.0, 0.0],
            sketchinc: 0.1,
            filletrad: 0.0,
            aunits: 0,
            auprec: 0,
            textstyle: -1,
            osmode: 0,
            attmode: 1,
            menu: [0u8; 15],
            dimscale: 1.0,
            dimasz: 0.18,
            dimexo: 0.0625,
            dimdli: 0.38,
            dimexe: 0.18,
            dimtp: 0.0,
            dimtm: 0.0,
            dimtxt: 0.18,
            dimcen: 0.09,
            dimtsz: 0.0,
            dimtol: 0,
            dimlim: 0,
            dimtih: 1,
            dimtoh: 1,
            dimse1: 0,
            dimse2: 0,
            dimtad: 0,
            limcheck: 0,
            menuext: [0u8; 46],
            elevation: 0.0,
            thickness: 0.0,
            viewdir: [0.0, 0.0, 1.0],
            vpoint_x: [1.0, 0.0, 0.0],
            vpoint_y: [0.0, 1.0, 0.0],
            vpoint_z: [0.0, 0.0, 1.0],
            vpoint_x_alt: [1.0, 0.0, 0.0],
            vpoint_y_alt: [0.0, 1.0, 0.0],
            vpoint_z_alt: [0.0, 0.0, 1.0],
            flag_3d: 0,
            blipmode: 0,
            dimzin: 0,
            dimrnd: 0.0,
            dimdle: 0.0,
            dimblk_t: [0u8; 33],
            circle_zoom: 100,
            coords: 1,
            cecolor_index: 256,
            celtype: -1,
            tdcreate: [0, 0],
            tdupdate: [0, 0],
            tdindwg: [0, 0],
            tdusrtimer: [0, 0],
            usrtimer: 1,
            fastzoom: 1,
            skpoly: 0,
            unknown_mon: 0,
            unknown_day: 0,
            unknown_year: 0,
            unknown_hour: 0,
            unknown_min: 0,
            unknown_sec: 0,
            unknown_ms: 0,
            angbase: 0.0,
            angdir: 0,
            pdmode: 0,
            pdsize: 0.0,
            plinewid: 0.0,
            useri1: 0,
            useri2: 0,
            useri3: 0,
            useri4: 0,
            useri5: 0,
            userr1: 0.0,
            userr2: 0.0,
            userr3: 0.0,
            userr4: 0.0,
            userr5: 0.0,
            dimalt: 0,
            dimaltd: 2,
            dimaso: 1,
            dimsho: 1,
            dimpost: [0u8; 16],
            dimapost: [0u8; 16],
            dimaltf: 25.4,
            dimlfac: 1.0,
            splinesegs: 8,
            splframe: 0,
            attreq: 1,
            attdia: 0,
            chamfera: 0.0,
            chamferb: 0.0,
            mirrtext: 1,
            codepage: 30,
            ucsorg: [0.0; 3],
            ucsxdir: [1.0, 0.0, 0.0],
            ucsydir: [0.0, 1.0, 0.0],
            target: [0.0; 3],
            lenslength: 50.0,
            viewtwist: 0.0,
            frontz: 0.0,
            backz: 0.0,
            viewmode: 0,
            dimtofl: 0,
            dimblk1_t: [0u8; 33],
            dimblk2_t: [0u8; 33],
            dimsah: 0,
            dimtix: 0,
            dimsoxd: 0,
            dimtvp: 0.0,
            unknown_string: [0u8; 33],
            handling: 1,
            handseed: 0x20,
            surfu: 6,
            surfv: 6,
            surftype: 6,
            surftab1: 6,
            surftab2: 6,
            flatland: 0,
            splinetype: 6,
            ucsicon: 1,
            ucsname: -1,
            worldview: 1,
            unknown_51e: 0,
            unknown_520: 0,
            unknown_52c: 0,
            unknown_52e: 0,
            unknown_530: 0,
            dimclrd_c: 0,
            dimclre_c: 0,
            dimclrt_c: 0,
            shadedge: 3,
            shadedif: 70,
            unknown_59: 0,
            unitmode: 0,
            unit1_ratio: 0.0,
            unit2_ratio: 0.0,
            unit3_ratio: 0.0,
            unit4_ratio: 0.0,
            unit1_name: [0u8; 32],
            unit2_name: [0u8; 32],
            unit3_name: [0u8; 32],
            unit4_name: [0u8; 32],
            dimtfac: 1.0,
            pucsorg: [0.0; 3],
            pucsxdir: [1.0, 0.0, 0.0],
            pucsydir: [0.0, 1.0, 0.0],
            pucsname: -1,
            tilemode: 1,
            plimcheck: 0,
            unknown_10: 0,
            pextmin: [f64::MAX; 3],
            pextmax: [f64::MIN; 3],
            plimmin: [0.0; 2],
            plimmax: [12.0, 9.0],
            pinsbase: [0.0; 3],
            maxactvp: 64,
            dimgap: 0.09,
            pelevation: 0.0,
            visretain: 0,
        }
    }
}

// --- primitive writers (byte-aligned LE; matches LibreDWG's
//     `bit_write_RC/RS/RL/RD` when `dat->bit == 0`).

fn w_rc(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}
fn w_rs(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn w_rsd(buf: &mut Vec<u8>, v: i16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn w_rl(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn w_rd(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn w_2rd(buf: &mut Vec<u8>, v: [f64; 2]) {
    w_rd(buf, v[0]);
    w_rd(buf, v[1]);
}
fn w_3rd(buf: &mut Vec<u8>, v: [f64; 3]) {
    w_rd(buf, v[0]);
    w_rd(buf, v[1]);
    w_rd(buf, v[2]);
}
fn w_rll_be(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_be_bytes());
}
fn w_tfv(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(bytes);
}
fn w_timerll(buf: &mut Vec<u8>, v: [u32; 2]) {
    w_rl(buf, v[0]);
    w_rl(buf, v[1]);
}
fn w_section(buf: &mut Vec<u8>, h: &SectionTableHeader) {
    h.encode_append(buf);
}

// --- primitive readers ---

fn r_rc(b: &[u8], cur: &mut usize) -> DwgResult<u8> {
    if *cur + 1 > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 1,
            bit: 0,
        });
    }
    let v = b[*cur];
    *cur += 1;
    Ok(v)
}
fn r_rs(b: &[u8], cur: &mut usize) -> DwgResult<u16> {
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
fn r_rsd(b: &[u8], cur: &mut usize) -> DwgResult<i16> {
    Ok(r_rs(b, cur)? as i16)
}
fn r_rl(b: &[u8], cur: &mut usize) -> DwgResult<u32> {
    if *cur + 4 > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 4,
            bit: 0,
        });
    }
    let v = u32::from_le_bytes([b[*cur], b[*cur + 1], b[*cur + 2], b[*cur + 3]]);
    *cur += 4;
    Ok(v)
}
fn r_rd(b: &[u8], cur: &mut usize) -> DwgResult<f64> {
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
fn r_2rd(b: &[u8], cur: &mut usize) -> DwgResult<[f64; 2]> {
    Ok([r_rd(b, cur)?, r_rd(b, cur)?])
}
fn r_3rd(b: &[u8], cur: &mut usize) -> DwgResult<[f64; 3]> {
    Ok([r_rd(b, cur)?, r_rd(b, cur)?, r_rd(b, cur)?])
}
fn r_rll_be(b: &[u8], cur: &mut usize) -> DwgResult<u64> {
    if *cur + 8 > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + 8,
            bit: 0,
        });
    }
    let mut z = [0u8; 8];
    z.copy_from_slice(&b[*cur..*cur + 8]);
    *cur += 8;
    Ok(u64::from_be_bytes(z))
}
fn r_tfv<const N: usize>(b: &[u8], cur: &mut usize) -> DwgResult<[u8; N]> {
    if *cur + N > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + N,
            bit: 0,
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&b[*cur..*cur + N]);
    *cur += N;
    Ok(out)
}
fn r_timerll(b: &[u8], cur: &mut usize) -> DwgResult<[u32; 2]> {
    Ok([r_rl(b, cur)?, r_rl(b, cur)?])
}
fn r_section(b: &[u8], cur: &mut usize) -> DwgResult<SectionTableHeader> {
    if *cur + SECTION_TABLE_HEADER_LEN > b.len() {
        return Err(DwgError::UnexpectedEof {
            byte: *cur + SECTION_TABLE_HEADER_LEN,
            bit: 0,
        });
    }
    let h = SectionTableHeader::parse(&b[*cur..*cur + SECTION_TABLE_HEADER_LEN])?;
    *cur += SECTION_TABLE_HEADER_LEN;
    Ok(h)
}

impl R12HeaderVars {
    /// Encode the header variables. Five embedded section
    /// descriptors are pulled from `tables` (UCS, VPORT, APPID,
    /// DIMSTYLE, VX); the leading five (BLOCK, LAYER, STYLE, LTYPE,
    /// VIEW) live in the file's contiguous section-table region and
    /// are emitted by the assembler, not here.
    pub fn encode_with_section_tables(&self, tables: &R12SectionTables) -> Vec<u8> {
        let mut b = Vec::with_capacity(2048);

        // R2.0+ always emitted.
        w_3rd(&mut b, self.insbase);
        w_rs(&mut b, self.plinegen);
        w_3rd(&mut b, self.extmin);
        w_3rd(&mut b, self.extmax);
        w_2rd(&mut b, self.limmin);
        w_2rd(&mut b, self.limmax);
        w_3rd(&mut b, self.viewctr);
        w_rd(&mut b, self.viewsize);
        w_rs(&mut b, self.snapmode);
        w_2rd(&mut b, self.snapunit);
        w_2rd(&mut b, self.snapbase);
        w_rd(&mut b, self.snapang);
        w_rs(&mut b, self.snapstyle);
        w_rs(&mut b, self.snapisopair);
        w_rs(&mut b, self.gridmode);
        w_2rd(&mut b, self.gridunit);
        w_rs(&mut b, self.orthomode);
        w_rs(&mut b, self.regenmode);
        w_rs(&mut b, self.fillmode);
        w_rs(&mut b, self.qtextmode);
        w_rs(&mut b, self.dragmode);
        w_rd(&mut b, self.ltscale);
        w_rd(&mut b, self.textsize);
        w_rd(&mut b, self.tracewid);
        w_rsd(&mut b, self.clayer);
        w_rl(&mut b, self.old_cecolor_lo);
        w_rl(&mut b, self.old_cecolor_hi);
        w_rs(&mut b, self.unknown_5);
        w_rs(&mut b, self.psltscale);
        w_rs(&mut b, self.treedepth);
        w_rs(&mut b, self.unknown_6);
        w_rd(&mut b, self.aspect_ratio);
        w_rs(&mut b, self.lunits);
        w_rs(&mut b, self.luprec);
        w_rs(&mut b, self.axismode);
        w_2rd(&mut b, self.axisunit);
        w_rd(&mut b, self.sketchinc);
        w_rd(&mut b, self.filletrad);
        w_rs(&mut b, self.aunits);
        w_rs(&mut b, self.auprec);
        w_rsd(&mut b, self.textstyle);
        w_rs(&mut b, self.osmode);
        w_rs(&mut b, self.attmode);
        w_tfv(&mut b, &self.menu);
        w_rd(&mut b, self.dimscale);
        w_rd(&mut b, self.dimasz);
        w_rd(&mut b, self.dimexo);
        w_rd(&mut b, self.dimdli);
        w_rd(&mut b, self.dimexe);
        w_rd(&mut b, self.dimtp);
        w_rd(&mut b, self.dimtm);
        w_rd(&mut b, self.dimtxt);
        w_rd(&mut b, self.dimcen);
        w_rd(&mut b, self.dimtsz);
        // numheader_vars > 74:
        w_rc(&mut b, self.dimtol);
        w_rc(&mut b, self.dimlim);
        w_rc(&mut b, self.dimtih);
        w_rc(&mut b, self.dimtoh);
        w_rc(&mut b, self.dimse1);
        w_rc(&mut b, self.dimse2);
        w_rc(&mut b, self.dimtad);
        w_rc(&mut b, self.limcheck);
        w_tfv(&mut b, &self.menuext);
        w_rd(&mut b, self.elevation);
        w_rd(&mut b, self.thickness);
        w_3rd(&mut b, self.viewdir);
        w_3rd(&mut b, self.vpoint_x);
        w_3rd(&mut b, self.vpoint_y);
        w_3rd(&mut b, self.vpoint_z);
        w_3rd(&mut b, self.vpoint_x_alt);
        w_3rd(&mut b, self.vpoint_y_alt);
        w_3rd(&mut b, self.vpoint_z_alt);
        w_rs(&mut b, self.flag_3d);
        w_rs(&mut b, self.blipmode);
        // numheader_vars > 83:
        w_rc(&mut b, self.dimzin);
        w_rd(&mut b, self.dimrnd);
        w_rd(&mut b, self.dimdle);
        w_tfv(&mut b, &self.dimblk_t);
        w_rs(&mut b, self.circle_zoom);
        w_rs(&mut b, self.coords);
        w_rs(&mut b, self.cecolor_index);
        w_rsd(&mut b, self.celtype);
        w_timerll(&mut b, self.tdcreate);
        w_timerll(&mut b, self.tdupdate);
        w_timerll(&mut b, self.tdindwg);
        w_timerll(&mut b, self.tdusrtimer);
        w_rs(&mut b, self.usrtimer);
        w_rs(&mut b, self.fastzoom);
        w_rs(&mut b, self.skpoly);
        w_rs(&mut b, self.unknown_mon);
        w_rs(&mut b, self.unknown_day);
        w_rs(&mut b, self.unknown_year);
        w_rs(&mut b, self.unknown_hour);
        w_rs(&mut b, self.unknown_min);
        w_rs(&mut b, self.unknown_sec);
        w_rs(&mut b, self.unknown_ms);
        w_rd(&mut b, self.angbase);
        w_rs(&mut b, self.angdir);
        // numheader_vars > 101:
        w_rs(&mut b, self.pdmode);
        w_rd(&mut b, self.pdsize);
        w_rd(&mut b, self.plinewid);
        // numheader_vars > 104:
        w_rsd(&mut b, self.useri1);
        w_rsd(&mut b, self.useri2);
        w_rsd(&mut b, self.useri3);
        w_rsd(&mut b, self.useri4);
        w_rsd(&mut b, self.useri5);
        w_rd(&mut b, self.userr1);
        w_rd(&mut b, self.userr2);
        w_rd(&mut b, self.userr3);
        w_rd(&mut b, self.userr4);
        w_rd(&mut b, self.userr5);
        // numheader_vars > 114:
        w_rc(&mut b, self.dimalt);
        w_rc(&mut b, self.dimaltd);
        w_rc(&mut b, self.dimaso);
        w_rc(&mut b, self.dimsho);
        w_tfv(&mut b, &self.dimpost);
        w_tfv(&mut b, &self.dimapost);
        // numheader_vars > 120:
        w_rd(&mut b, self.dimaltf);
        w_rd(&mut b, self.dimlfac);
        // numheader_vars > 122:
        w_rs(&mut b, self.splinesegs);
        w_rs(&mut b, self.splframe);
        w_rs(&mut b, self.attreq);
        w_rs(&mut b, self.attdia);
        w_rd(&mut b, self.chamfera);
        w_rd(&mut b, self.chamferb);
        w_rs(&mut b, self.mirrtext);
        // numheader_vars > 129 (UCS section header embedded):
        w_section(&mut b, &tables.ucs);
        w_rs(&mut b, self.codepage);
        w_3rd(&mut b, self.ucsorg);
        w_3rd(&mut b, self.ucsxdir);
        w_3rd(&mut b, self.ucsydir);
        w_3rd(&mut b, self.target);
        w_rd(&mut b, self.lenslength);
        w_rd(&mut b, self.viewtwist);
        w_rd(&mut b, self.frontz);
        w_rd(&mut b, self.backz);
        w_rs(&mut b, self.viewmode);
        w_rc(&mut b, self.dimtofl);
        w_tfv(&mut b, &self.dimblk1_t);
        w_tfv(&mut b, &self.dimblk2_t);
        w_rc(&mut b, self.dimsah);
        w_rc(&mut b, self.dimtix);
        w_rc(&mut b, self.dimsoxd);
        w_rd(&mut b, self.dimtvp);
        w_tfv(&mut b, &self.unknown_string);
        w_rs(&mut b, self.handling);
        w_rll_be(&mut b, self.handseed);
        w_rs(&mut b, self.surfu);
        w_rs(&mut b, self.surfv);
        w_rs(&mut b, self.surftype);
        w_rs(&mut b, self.surftab1);
        w_rs(&mut b, self.surftab2);
        // VPORT section header embedded:
        w_section(&mut b, &tables.vport);
        w_rs(&mut b, self.flatland);
        w_rs(&mut b, self.splinetype);
        w_rs(&mut b, self.ucsicon);
        w_rsd(&mut b, self.ucsname);
        // numheader_vars > 158 (APPID section header embedded):
        w_section(&mut b, &tables.appid);
        w_rs(&mut b, self.worldview);
        // numheader_vars > 160:
        w_rs(&mut b, self.unknown_51e);
        w_rs(&mut b, self.unknown_520);
        // DIMSTYLE section header embedded:
        w_section(&mut b, &tables.dimstyle);
        w_rsd(&mut b, self.unknown_52c);
        w_rs(&mut b, self.unknown_52e);
        w_rc(&mut b, self.unknown_530);
        w_rs(&mut b, self.dimclrd_c);
        w_rs(&mut b, self.dimclre_c);
        w_rs(&mut b, self.dimclrt_c);
        w_rs(&mut b, self.shadedge);
        w_rs(&mut b, self.shadedif);
        w_rs(&mut b, self.unknown_59);
        w_rs(&mut b, self.unitmode);
        w_rd(&mut b, self.unit1_ratio);
        w_rd(&mut b, self.unit2_ratio);
        w_rd(&mut b, self.unit3_ratio);
        w_rd(&mut b, self.unit4_ratio);
        w_tfv(&mut b, &self.unit1_name);
        w_tfv(&mut b, &self.unit2_name);
        w_tfv(&mut b, &self.unit3_name);
        w_tfv(&mut b, &self.unit4_name);
        w_rd(&mut b, self.dimtfac);
        w_3rd(&mut b, self.pucsorg);
        w_3rd(&mut b, self.pucsxdir);
        w_3rd(&mut b, self.pucsydir);
        w_rsd(&mut b, self.pucsname);
        w_rs(&mut b, self.tilemode);
        w_rs(&mut b, self.plimcheck);
        w_rs(&mut b, self.unknown_10);
        w_3rd(&mut b, self.pextmin);
        w_3rd(&mut b, self.pextmax);
        w_2rd(&mut b, self.plimmin);
        w_2rd(&mut b, self.plimmax);
        w_3rd(&mut b, self.pinsbase);
        // VX section header embedded:
        w_section(&mut b, &tables.vx);
        w_rs(&mut b, self.maxactvp);
        w_rd(&mut b, self.dimgap);
        w_rd(&mut b, self.pelevation);
        // numheader_vars > 204:
        w_rs(&mut b, self.visretain);

        b
    }

    /// Decode the header variables. Yields `(header_vars,
    /// embedded_section_descriptors)`: the five descriptors are
    /// merged into the caller's `R12SectionTables` after the
    /// leading five have been read separately.
    pub fn decode(bytes: &[u8]) -> DwgResult<DecodedHeaderVars> {
        let mut cur = 0usize;

        let insbase = r_3rd(bytes, &mut cur)?;
        let plinegen = r_rs(bytes, &mut cur)?;
        let extmin = r_3rd(bytes, &mut cur)?;
        let extmax = r_3rd(bytes, &mut cur)?;
        let limmin = r_2rd(bytes, &mut cur)?;
        let limmax = r_2rd(bytes, &mut cur)?;
        let viewctr = r_3rd(bytes, &mut cur)?;
        let viewsize = r_rd(bytes, &mut cur)?;
        let snapmode = r_rs(bytes, &mut cur)?;
        let snapunit = r_2rd(bytes, &mut cur)?;
        let snapbase = r_2rd(bytes, &mut cur)?;
        let snapang = r_rd(bytes, &mut cur)?;
        let snapstyle = r_rs(bytes, &mut cur)?;
        let snapisopair = r_rs(bytes, &mut cur)?;
        let gridmode = r_rs(bytes, &mut cur)?;
        let gridunit = r_2rd(bytes, &mut cur)?;
        let orthomode = r_rs(bytes, &mut cur)?;
        let regenmode = r_rs(bytes, &mut cur)?;
        let fillmode = r_rs(bytes, &mut cur)?;
        let qtextmode = r_rs(bytes, &mut cur)?;
        let dragmode = r_rs(bytes, &mut cur)?;
        let ltscale = r_rd(bytes, &mut cur)?;
        let textsize = r_rd(bytes, &mut cur)?;
        let tracewid = r_rd(bytes, &mut cur)?;
        let clayer = r_rsd(bytes, &mut cur)?;
        let old_cecolor_lo = r_rl(bytes, &mut cur)?;
        let old_cecolor_hi = r_rl(bytes, &mut cur)?;
        let unknown_5 = r_rs(bytes, &mut cur)?;
        let psltscale = r_rs(bytes, &mut cur)?;
        let treedepth = r_rs(bytes, &mut cur)?;
        let unknown_6 = r_rs(bytes, &mut cur)?;
        let aspect_ratio = r_rd(bytes, &mut cur)?;
        let lunits = r_rs(bytes, &mut cur)?;
        let luprec = r_rs(bytes, &mut cur)?;
        let axismode = r_rs(bytes, &mut cur)?;
        let axisunit = r_2rd(bytes, &mut cur)?;
        let sketchinc = r_rd(bytes, &mut cur)?;
        let filletrad = r_rd(bytes, &mut cur)?;
        let aunits = r_rs(bytes, &mut cur)?;
        let auprec = r_rs(bytes, &mut cur)?;
        let textstyle = r_rsd(bytes, &mut cur)?;
        let osmode = r_rs(bytes, &mut cur)?;
        let attmode = r_rs(bytes, &mut cur)?;
        let menu = r_tfv::<15>(bytes, &mut cur)?;
        let dimscale = r_rd(bytes, &mut cur)?;
        let dimasz = r_rd(bytes, &mut cur)?;
        let dimexo = r_rd(bytes, &mut cur)?;
        let dimdli = r_rd(bytes, &mut cur)?;
        let dimexe = r_rd(bytes, &mut cur)?;
        let dimtp = r_rd(bytes, &mut cur)?;
        let dimtm = r_rd(bytes, &mut cur)?;
        let dimtxt = r_rd(bytes, &mut cur)?;
        let dimcen = r_rd(bytes, &mut cur)?;
        let dimtsz = r_rd(bytes, &mut cur)?;
        let dimtol = r_rc(bytes, &mut cur)?;
        let dimlim = r_rc(bytes, &mut cur)?;
        let dimtih = r_rc(bytes, &mut cur)?;
        let dimtoh = r_rc(bytes, &mut cur)?;
        let dimse1 = r_rc(bytes, &mut cur)?;
        let dimse2 = r_rc(bytes, &mut cur)?;
        let dimtad = r_rc(bytes, &mut cur)?;
        let limcheck = r_rc(bytes, &mut cur)?;
        let menuext = r_tfv::<46>(bytes, &mut cur)?;
        let elevation = r_rd(bytes, &mut cur)?;
        let thickness = r_rd(bytes, &mut cur)?;
        let viewdir = r_3rd(bytes, &mut cur)?;
        let vpoint_x = r_3rd(bytes, &mut cur)?;
        let vpoint_y = r_3rd(bytes, &mut cur)?;
        let vpoint_z = r_3rd(bytes, &mut cur)?;
        let vpoint_x_alt = r_3rd(bytes, &mut cur)?;
        let vpoint_y_alt = r_3rd(bytes, &mut cur)?;
        let vpoint_z_alt = r_3rd(bytes, &mut cur)?;
        let flag_3d = r_rs(bytes, &mut cur)?;
        let blipmode = r_rs(bytes, &mut cur)?;
        let dimzin = r_rc(bytes, &mut cur)?;
        let dimrnd = r_rd(bytes, &mut cur)?;
        let dimdle = r_rd(bytes, &mut cur)?;
        let dimblk_t = r_tfv::<33>(bytes, &mut cur)?;
        let circle_zoom = r_rs(bytes, &mut cur)?;
        let coords = r_rs(bytes, &mut cur)?;
        let cecolor_index = r_rs(bytes, &mut cur)?;
        let celtype = r_rsd(bytes, &mut cur)?;
        let tdcreate = r_timerll(bytes, &mut cur)?;
        let tdupdate = r_timerll(bytes, &mut cur)?;
        let tdindwg = r_timerll(bytes, &mut cur)?;
        let tdusrtimer = r_timerll(bytes, &mut cur)?;
        let usrtimer = r_rs(bytes, &mut cur)?;
        let fastzoom = r_rs(bytes, &mut cur)?;
        let skpoly = r_rs(bytes, &mut cur)?;
        let unknown_mon = r_rs(bytes, &mut cur)?;
        let unknown_day = r_rs(bytes, &mut cur)?;
        let unknown_year = r_rs(bytes, &mut cur)?;
        let unknown_hour = r_rs(bytes, &mut cur)?;
        let unknown_min = r_rs(bytes, &mut cur)?;
        let unknown_sec = r_rs(bytes, &mut cur)?;
        let unknown_ms = r_rs(bytes, &mut cur)?;
        let angbase = r_rd(bytes, &mut cur)?;
        let angdir = r_rs(bytes, &mut cur)?;
        let pdmode = r_rs(bytes, &mut cur)?;
        let pdsize = r_rd(bytes, &mut cur)?;
        let plinewid = r_rd(bytes, &mut cur)?;
        let useri1 = r_rsd(bytes, &mut cur)?;
        let useri2 = r_rsd(bytes, &mut cur)?;
        let useri3 = r_rsd(bytes, &mut cur)?;
        let useri4 = r_rsd(bytes, &mut cur)?;
        let useri5 = r_rsd(bytes, &mut cur)?;
        let userr1 = r_rd(bytes, &mut cur)?;
        let userr2 = r_rd(bytes, &mut cur)?;
        let userr3 = r_rd(bytes, &mut cur)?;
        let userr4 = r_rd(bytes, &mut cur)?;
        let userr5 = r_rd(bytes, &mut cur)?;
        let dimalt = r_rc(bytes, &mut cur)?;
        let dimaltd = r_rc(bytes, &mut cur)?;
        let dimaso = r_rc(bytes, &mut cur)?;
        let dimsho = r_rc(bytes, &mut cur)?;
        let dimpost = r_tfv::<16>(bytes, &mut cur)?;
        let dimapost = r_tfv::<16>(bytes, &mut cur)?;
        let dimaltf = r_rd(bytes, &mut cur)?;
        let dimlfac = r_rd(bytes, &mut cur)?;
        let splinesegs = r_rs(bytes, &mut cur)?;
        let splframe = r_rs(bytes, &mut cur)?;
        let attreq = r_rs(bytes, &mut cur)?;
        let attdia = r_rs(bytes, &mut cur)?;
        let chamfera = r_rd(bytes, &mut cur)?;
        let chamferb = r_rd(bytes, &mut cur)?;
        let mirrtext = r_rs(bytes, &mut cur)?;
        let ucs_hdr = r_section(bytes, &mut cur)?;
        let codepage = r_rs(bytes, &mut cur)?;
        let ucsorg = r_3rd(bytes, &mut cur)?;
        let ucsxdir = r_3rd(bytes, &mut cur)?;
        let ucsydir = r_3rd(bytes, &mut cur)?;
        let target = r_3rd(bytes, &mut cur)?;
        let lenslength = r_rd(bytes, &mut cur)?;
        let viewtwist = r_rd(bytes, &mut cur)?;
        let frontz = r_rd(bytes, &mut cur)?;
        let backz = r_rd(bytes, &mut cur)?;
        let viewmode = r_rs(bytes, &mut cur)?;
        let dimtofl = r_rc(bytes, &mut cur)?;
        let dimblk1_t = r_tfv::<33>(bytes, &mut cur)?;
        let dimblk2_t = r_tfv::<33>(bytes, &mut cur)?;
        let dimsah = r_rc(bytes, &mut cur)?;
        let dimtix = r_rc(bytes, &mut cur)?;
        let dimsoxd = r_rc(bytes, &mut cur)?;
        let dimtvp = r_rd(bytes, &mut cur)?;
        let unknown_string = r_tfv::<33>(bytes, &mut cur)?;
        let handling = r_rs(bytes, &mut cur)?;
        let handseed = r_rll_be(bytes, &mut cur)?;
        let surfu = r_rs(bytes, &mut cur)?;
        let surfv = r_rs(bytes, &mut cur)?;
        let surftype = r_rs(bytes, &mut cur)?;
        let surftab1 = r_rs(bytes, &mut cur)?;
        let surftab2 = r_rs(bytes, &mut cur)?;
        let vport_hdr = r_section(bytes, &mut cur)?;
        let flatland = r_rs(bytes, &mut cur)?;
        let splinetype = r_rs(bytes, &mut cur)?;
        let ucsicon = r_rs(bytes, &mut cur)?;
        let ucsname = r_rsd(bytes, &mut cur)?;
        let appid_hdr = r_section(bytes, &mut cur)?;
        let worldview = r_rs(bytes, &mut cur)?;
        let unknown_51e = r_rs(bytes, &mut cur)?;
        let unknown_520 = r_rs(bytes, &mut cur)?;
        let dimstyle_hdr = r_section(bytes, &mut cur)?;
        let unknown_52c = r_rsd(bytes, &mut cur)?;
        let unknown_52e = r_rs(bytes, &mut cur)?;
        let unknown_530 = r_rc(bytes, &mut cur)?;
        let dimclrd_c = r_rs(bytes, &mut cur)?;
        let dimclre_c = r_rs(bytes, &mut cur)?;
        let dimclrt_c = r_rs(bytes, &mut cur)?;
        let shadedge = r_rs(bytes, &mut cur)?;
        let shadedif = r_rs(bytes, &mut cur)?;
        let unknown_59 = r_rs(bytes, &mut cur)?;
        let unitmode = r_rs(bytes, &mut cur)?;
        let unit1_ratio = r_rd(bytes, &mut cur)?;
        let unit2_ratio = r_rd(bytes, &mut cur)?;
        let unit3_ratio = r_rd(bytes, &mut cur)?;
        let unit4_ratio = r_rd(bytes, &mut cur)?;
        let unit1_name = r_tfv::<32>(bytes, &mut cur)?;
        let unit2_name = r_tfv::<32>(bytes, &mut cur)?;
        let unit3_name = r_tfv::<32>(bytes, &mut cur)?;
        let unit4_name = r_tfv::<32>(bytes, &mut cur)?;
        let dimtfac = r_rd(bytes, &mut cur)?;
        let pucsorg = r_3rd(bytes, &mut cur)?;
        let pucsxdir = r_3rd(bytes, &mut cur)?;
        let pucsydir = r_3rd(bytes, &mut cur)?;
        let pucsname = r_rsd(bytes, &mut cur)?;
        let tilemode = r_rs(bytes, &mut cur)?;
        let plimcheck = r_rs(bytes, &mut cur)?;
        let unknown_10 = r_rs(bytes, &mut cur)?;
        let pextmin = r_3rd(bytes, &mut cur)?;
        let pextmax = r_3rd(bytes, &mut cur)?;
        let plimmin = r_2rd(bytes, &mut cur)?;
        let plimmax = r_2rd(bytes, &mut cur)?;
        let pinsbase = r_3rd(bytes, &mut cur)?;
        let vx_hdr = r_section(bytes, &mut cur)?;
        let maxactvp = r_rs(bytes, &mut cur)?;
        let dimgap = r_rd(bytes, &mut cur)?;
        let pelevation = r_rd(bytes, &mut cur)?;
        let visretain = r_rs(bytes, &mut cur)?;

        Ok(DecodedHeaderVars {
            header_vars: Self {
                insbase,
                plinegen,
                extmin,
                extmax,
                limmin,
                limmax,
                viewctr,
                viewsize,
                snapmode,
                snapunit,
                snapbase,
                snapang,
                snapstyle,
                snapisopair,
                gridmode,
                gridunit,
                orthomode,
                regenmode,
                fillmode,
                qtextmode,
                dragmode,
                ltscale,
                textsize,
                tracewid,
                clayer,
                old_cecolor_lo,
                old_cecolor_hi,
                unknown_5,
                psltscale,
                treedepth,
                unknown_6,
                aspect_ratio,
                lunits,
                luprec,
                axismode,
                axisunit,
                sketchinc,
                filletrad,
                aunits,
                auprec,
                textstyle,
                osmode,
                attmode,
                menu,
                dimscale,
                dimasz,
                dimexo,
                dimdli,
                dimexe,
                dimtp,
                dimtm,
                dimtxt,
                dimcen,
                dimtsz,
                dimtol,
                dimlim,
                dimtih,
                dimtoh,
                dimse1,
                dimse2,
                dimtad,
                limcheck,
                menuext,
                elevation,
                thickness,
                viewdir,
                vpoint_x,
                vpoint_y,
                vpoint_z,
                vpoint_x_alt,
                vpoint_y_alt,
                vpoint_z_alt,
                flag_3d,
                blipmode,
                dimzin,
                dimrnd,
                dimdle,
                dimblk_t,
                circle_zoom,
                coords,
                cecolor_index,
                celtype,
                tdcreate,
                tdupdate,
                tdindwg,
                tdusrtimer,
                usrtimer,
                fastzoom,
                skpoly,
                unknown_mon,
                unknown_day,
                unknown_year,
                unknown_hour,
                unknown_min,
                unknown_sec,
                unknown_ms,
                angbase,
                angdir,
                pdmode,
                pdsize,
                plinewid,
                useri1,
                useri2,
                useri3,
                useri4,
                useri5,
                userr1,
                userr2,
                userr3,
                userr4,
                userr5,
                dimalt,
                dimaltd,
                dimaso,
                dimsho,
                dimpost,
                dimapost,
                dimaltf,
                dimlfac,
                splinesegs,
                splframe,
                attreq,
                attdia,
                chamfera,
                chamferb,
                mirrtext,
                codepage,
                ucsorg,
                ucsxdir,
                ucsydir,
                target,
                lenslength,
                viewtwist,
                frontz,
                backz,
                viewmode,
                dimtofl,
                dimblk1_t,
                dimblk2_t,
                dimsah,
                dimtix,
                dimsoxd,
                dimtvp,
                unknown_string,
                handling,
                handseed,
                surfu,
                surfv,
                surftype,
                surftab1,
                surftab2,
                flatland,
                splinetype,
                ucsicon,
                ucsname,
                worldview,
                unknown_51e,
                unknown_520,
                unknown_52c,
                unknown_52e,
                unknown_530,
                dimclrd_c,
                dimclre_c,
                dimclrt_c,
                shadedge,
                shadedif,
                unknown_59,
                unitmode,
                unit1_ratio,
                unit2_ratio,
                unit3_ratio,
                unit4_ratio,
                unit1_name,
                unit2_name,
                unit3_name,
                unit4_name,
                dimtfac,
                pucsorg,
                pucsxdir,
                pucsydir,
                pucsname,
                tilemode,
                plimcheck,
                unknown_10,
                pextmin,
                pextmax,
                plimmin,
                plimmax,
                pinsbase,
                maxactvp,
                dimgap,
                pelevation,
                visretain,
            },
            ucs_hdr,
            vport_hdr,
            appid_hdr,
            dimstyle_hdr,
            vx_hdr,
            consumed: cur,
        })
    }

    /// Exact byte width of the encoded form (matches the spec —
    /// always the same for `numheader_vars = 205`).
    pub fn encoded_len() -> usize {
        ENCODED_LEN
    }
}

/// Decode result: the parsed header variables plus the five
/// embedded section descriptors and the number of bytes consumed.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedHeaderVars {
    pub header_vars: R12HeaderVars,
    pub ucs_hdr: SectionTableHeader,
    pub vport_hdr: SectionTableHeader,
    pub appid_hdr: SectionTableHeader,
    pub dimstyle_hdr: SectionTableHeader,
    pub vx_hdr: SectionTableHeader,
    /// Total bytes consumed from the input slice.
    pub consumed: usize,
}

/// The fixed byte width of an AC1009 header_vars block with
/// `numheader_vars = 205`. Verified by [`tests::default_encoded_len_matches_constant`].
pub const ENCODED_LEN: usize = 1631;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_encoded_len_matches_constant() {
        let hv = R12HeaderVars::default();
        let tables = R12SectionTables::default();
        let bytes = hv.encode_with_section_tables(&tables);
        assert_eq!(
            bytes.len(),
            ENCODED_LEN,
            "default header_vars encoded length must be exactly {ENCODED_LEN}, was {}",
            bytes.len()
        );
    }

    #[test]
    fn default_header_vars_round_trip() {
        let want = R12HeaderVars::default();
        let want_tables = R12SectionTables {
            ucs: SectionTableHeader {
                size: 33,
                number: 1,
                flags_r11: 0,
                address: 0x100,
            },
            vport: SectionTableHeader {
                size: 100,
                number: 2,
                flags_r11: 0x80,
                address: 0x200,
            },
            ..Default::default()
        };
        let bytes = want.encode_with_section_tables(&want_tables);
        let decoded = R12HeaderVars::decode(&bytes).unwrap();
        assert_eq!(decoded.header_vars, want);
        assert_eq!(decoded.ucs_hdr, want_tables.ucs);
        assert_eq!(decoded.vport_hdr, want_tables.vport);
        assert_eq!(decoded.appid_hdr, want_tables.appid);
        assert_eq!(decoded.dimstyle_hdr, want_tables.dimstyle);
        assert_eq!(decoded.vx_hdr, want_tables.vx);
        assert_eq!(decoded.consumed, ENCODED_LEN);
    }

    #[test]
    fn custom_field_values_round_trip() {
        let hv = R12HeaderVars {
            insbase: [1.5, 2.5, 3.5],
            ltscale: 42.0,
            lunits: 4,
            handseed: 0xDEAD_BEEF_CAFE_BABE,
            clayer: 7,
            textstyle: 3,
            celtype: -2,
            ..Default::default()
        };
        let bytes = hv.encode_with_section_tables(&R12SectionTables::default());
        let decoded = R12HeaderVars::decode(&bytes).unwrap();
        assert_eq!(decoded.header_vars, hv);
    }

    #[test]
    fn truncated_buffer_errors_cleanly() {
        let hv = R12HeaderVars::default();
        let bytes = hv.encode_with_section_tables(&R12SectionTables::default());
        let truncated = &bytes[..bytes.len() - 4];
        let err = R12HeaderVars::decode(truncated).unwrap_err();
        assert!(matches!(err, DwgError::UnexpectedEof { .. }));
    }
}
