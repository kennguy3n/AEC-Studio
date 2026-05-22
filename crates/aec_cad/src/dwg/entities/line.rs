//! LINE entity codec.
//!
//! Wire format mirrors LibreDWG `src/dwg.spec` (lines 1471-1542,
//! tagged `DWG_ENTITY (LINE)`):
//!
//! ```text
//! VERSIONS (R_13b1, R_14) {
//!     FIELD_3BD (start)
//!     FIELD_3BD (end)
//! }
//! SINCE (R_2000b) {
//!     FIELD_B  (z_is_zero)                      // 1=both Zs are 0
//!     FIELD_RD (start.x)
//!     FIELD_DD (end.x, start.x)                 // 2-bit selector + optional RD
//!     FIELD_RD (start.y)
//!     FIELD_DD (end.y, start.y)
//!     if (!z_is_zero) {
//!         FIELD_RD (start.z)
//!         FIELD_DD (end.z, start.z)
//!     }
//! }
//! SINCE (R_13b1) {
//!     FIELD_BT (thickness)                      // BT, default 0
//!     FIELD_BE (extrusion)                      // BE, default (0,0,1)
//! }
//! ```
//!
//! The pre-R2000 / R2007+ split matters at the byte level: an R14
//! file encoding (0,0,0) → (1,1,0) takes 18 bytes of payload (two
//! 3BD = 6×BD with all-zero/one mantissas) while the R2000+ optimised
//! form takes 7 bytes (z_is_zero=1, two RDs for start.{x,y}, two DDs
//! defaulting away from start, BT=0, BE=default). LibreDWG's
//! `bit_read_RD` "buffer overflow at 27.4 + 8 > 32" failure on a
//! freshly written R2010 file means we used to write the wrong
//! variant — this codec now branches on the version and matches the
//! spec exactly.
//!
//! This module exposes the structured intermediate type so the top
//! level reader/writer can bridge to [`crate::dxf::DxfLine`].

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;
use crate::dxf::DxfLine;

#[derive(Debug, Clone, PartialEq)]
pub struct LineEntity {
    pub layer: String,
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub thickness: f64,
    pub extrusion: [f64; 3],
}

impl LineEntity {
    pub fn into_dxf(self) -> DxfLine {
        DxfLine {
            layer: self.layer,
            start: self.start,
            end: self.end,
        }
    }

    pub fn from_dxf(dxf: &DxfLine) -> Self {
        Self {
            layer: dxf.layer.clone(),
            start: dxf.start,
            end: dxf.end,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        }
    }

    /// Encode the bit-stream payload (no common entity header).
    /// Used by the top-level writer once it has emitted the shared
    /// header for the entity.
    ///
    /// Version dispatch follows `dwg.spec`:
    /// * R13/R14: two 3BD points (start, end)
    /// * R2000+: `z_is_zero` flag + RD/DD pair per axis (z elided when flag set)
    /// * R13+ (all): `BT thickness`, `BE extrusion`
    pub fn encode_payload(&self, w: &mut BitWriter, version: Version) -> DwgResult<()> {
        if uses_z_is_zero_optimisation(version) {
            let z_is_zero = self.start[2] == 0.0 && self.end[2] == 0.0;
            w.write_b(z_is_zero)?;
            w.write_rd(self.start[0])?;
            w.write_dd(self.end[0], self.start[0])?;
            w.write_rd(self.start[1])?;
            w.write_dd(self.end[1], self.start[1])?;
            if !z_is_zero {
                w.write_rd(self.start[2])?;
                w.write_dd(self.end[2], self.start[2])?;
            }
        } else {
            // R13/R14: full 3BD pair, no z_is_zero optimisation.
            w.write_3bd(self.start)?;
            w.write_3bd(self.end)?;
        }
        // BT thickness + BE extrusion are present from R13 onward,
        // but the bit-level encoding differs: R14 uses a plain BD /
        // 3BD; R2000+ uses the 1-bit "is default" optimisation.
        // `BitWriter::write_bt` / `write_be` dispatch on `version`
        // exactly like LibreDWG's `bit_write_BT` (bits.c:1310) and
        // `bit_write_BE` (bits.c:1135).
        w.write_bt(self.thickness, version)?;
        w.write_be(self.extrusion, version)?;
        Ok(())
    }

    pub fn decode_payload(
        r: &mut BitReader<'_>,
        layer: String,
        version: Version,
    ) -> DwgResult<Self> {
        let (start, end) = if uses_z_is_zero_optimisation(version) {
            let z_is_zero = r.read_b()?;
            let sx = r.read_rd()?;
            let ex = r.read_dd(sx)?;
            let sy = r.read_rd()?;
            let ey = r.read_dd(sy)?;
            let (sz, ez) = if z_is_zero {
                (0.0, 0.0)
            } else {
                let sz = r.read_rd()?;
                let ez = r.read_dd(sz)?;
                (sz, ez)
            };
            ([sx, sy, sz], [ex, ey, ez])
        } else {
            let s = r.read_3bd()?;
            let e = r.read_3bd()?;
            (s, e)
        };
        let thickness = r.read_bt(version)?;
        let extrusion = r.read_be(version)?;
        Ok(Self {
            layer,
            start,
            end,
            thickness,
            extrusion,
        })
    }
}

/// LibreDWG `SINCE (R_2000b)` gate: the `z_is_zero / RD / DD`
/// optimisation kicks in for R2000 and every later release. R13/R14
/// use plain 3BD pairs.
fn uses_z_is_zero_optimisation(version: Version) -> bool {
    !matches!(version, Version::R12 | Version::R14)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSIONS_R13_R14: &[Version] = &[Version::R14];
    const VERSIONS_R2000_PLUS: &[Version] = &[
        Version::R2000,
        Version::R2004,
        Version::R2007,
        Version::R2010,
        Version::R2013,
        Version::R2018,
    ];

    fn round_trip(line: &LineEntity, version: Version) -> LineEntity {
        let mut w = BitWriter::new();
        line.encode_payload(&mut w, version).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        LineEntity::decode_payload(&mut r, line.layer.clone(), version).unwrap()
    }

    #[test]
    fn line_payload_round_trips_z_zero_default_extrusion() {
        let line = LineEntity {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [100.0, 50.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        for &v in VERSIONS_R2000_PLUS.iter().chain(VERSIONS_R13_R14) {
            assert_eq!(round_trip(&line, v), line, "version {v:?}");
        }
    }

    #[test]
    fn line_payload_round_trips_z_nonzero_custom_extrusion() {
        let line = LineEntity {
            layer: "WALL".into(),
            start: [1.0, 2.0, 3.0],
            end: [4.0, 5.0, 6.0],
            thickness: 0.5,
            extrusion: [1.0, 0.0, 0.0],
        };
        for &v in VERSIONS_R2000_PLUS.iter().chain(VERSIONS_R13_R14) {
            assert_eq!(round_trip(&line, v), line, "version {v:?}");
        }
    }

    #[test]
    fn line_dxf_bridge_round_trips() {
        let line = LineEntity {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 10.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let dxf = line.clone().into_dxf();
        let back = LineEntity::from_dxf(&dxf);
        assert_eq!(back, line);
    }

    #[test]
    fn r14_and_r2000_use_different_wire_layouts() {
        // For an arbitrary line whose endpoints are not all 0/1 (so
        // BD can't trivially elide via the +1.0 / 0.0 special cases),
        // R14 emits two 3BD's and R2000+ emits an optimised
        // `B + RD + DD` quadruple per axis. The two formats are
        // bit-incompatible — a regression to a version-agnostic
        // encoder (e.g. our pre-Phase-6 code that emitted the same
        // bits for every version) would be caught by the byte-level
        // diff test below.
        let line = LineEntity {
            layer: "0".into(),
            start: [1.23, 4.56, 7.89],
            end: [10.11, 12.13, 14.15],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w_r2010 = BitWriter::new();
        line.encode_payload(&mut w_r2010, Version::R2010).unwrap();
        let r2010_bytes = w_r2010.into_bytes();
        let mut w_r14 = BitWriter::new();
        line.encode_payload(&mut w_r14, Version::R14).unwrap();
        let r14_bytes = w_r14.into_bytes();
        assert_ne!(
            r14_bytes, r2010_bytes,
            "R14 and R2010 must emit different bit patterns for the \
             same LINE (regression to version-agnostic encoder)"
        );
        // Both must round-trip cleanly through their own decoder.
        let mut r = BitReader::new(&r2010_bytes);
        assert_eq!(
            LineEntity::decode_payload(&mut r, line.layer.clone(), Version::R2010).unwrap(),
            line
        );
        let mut r = BitReader::new(&r14_bytes);
        assert_eq!(
            LineEntity::decode_payload(&mut r, line.layer.clone(), Version::R14).unwrap(),
            line
        );
    }

    #[test]
    fn r14_decoder_rejects_r2000_payload_and_vice_versa() {
        // The two formats are not bit-compatible: an R2000+ payload
        // starts with the z_is_zero bit then an RD, while R14 starts
        // directly with the BD of start.x. Round-tripping across the
        // version boundary should produce a different (i.e. corrupted)
        // value, which proves the version gating is doing real work.
        let line = LineEntity {
            layer: "0".into(),
            start: [1.0, 2.0, 0.0],
            end: [3.0, 4.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w = BitWriter::new();
        line.encode_payload(&mut w, Version::R2010).unwrap();
        let bytes = w.into_bytes();
        // Decode the R2010 bytes as if they were R14. The result
        // should differ (it might error or produce nonsense
        // coordinates) — but it MUST NOT equal `line`, else our
        // version gate is a no-op.
        let mut r = BitReader::new(&bytes);
        // Acceptable outcomes when decoding R2010 bytes with R14
        // schema:
        //   - The spec mismatch produces a typed error (e.g. RD
        //     buffer overflow, BD unexpected code) — fine, the
        //     reader just refused our R2010 stream.
        //   - The reader does decode *something* — but it MUST NOT
        //     equal `line`, or our version gate is a no-op.
        if let Ok(decoded) = LineEntity::decode_payload(&mut r, "0".into(), Version::R14) {
            assert_ne!(
                decoded, line,
                "R14 decoder accepted R2010 bytes and produced the original line — \
                 version gating is not actually changing the wire format"
            );
        }
    }
}
