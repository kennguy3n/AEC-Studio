//! R13-R2000 header-variables section.
//!
//! Logical layout of the section on disk:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ Start sentinel (16 bytes — HEADER_VARS_BEGIN)        │
//! ├──────────────────────────────────────────────────────┤
//! │ RL  size_in_bits           ← bit-encoded (= 32-bit)  │
//! ├──────────────────────────────────────────────────────┤
//! │ ... ~150 system variables, packed bit-encoded ...    │
//! ├──────────────────────────────────────────────────────┤
//! │ CRC-X25 (2 bytes, byte-aligned)                      │
//! ├──────────────────────────────────────────────────────┤
//! │ End sentinel (16 bytes — HEADER_VARS_END)            │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! The 150-variable body is genuinely large; this module models it as a
//! **structured pass-through**: on read we capture the bit-level body
//! verbatim into a `Vec<u8>` (preserving any vendor-specific tail bits
//! we don't yet interpret), and on write we re-emit it as a single
//! contiguous bit run. That gives us a *lossless* round-trip even for
//! header variables we don't expose programmatically, which matters
//! because AutoCAD aborts loading a file if any header variable it
//! reads back differs from what it wrote (the values include CRC-like
//! hashes that span the entire variable set).
//!
//! The structured accessors come in alongside the round-trip wiring;
//! callers can ask for the variables they care about (`insbase`,
//! `extmin`, `extmax`, `clayer`, …) via helper methods that decode
//! against a fixed offset table per version.

use crate::dwg::bits::{crc_x25, BitReader, BitWriter};
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::header_vars_body::{encode_body, HeaderVars, MaintVersion};
use crate::dwg::file::sentinels::{HEADER_VARS_BEGIN, HEADER_VARS_END};
use crate::dwg::version::Version;

/// The header-variables section, parsed but stored as an opaque blob.
///
/// Lossless round-trip is the contract: a buffer read from disk and
/// written back unchanged via this type produces byte-identical output
/// when the input was byte-aligned (which the on-disk format always
/// is, because the leading sentinel is byte-aligned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderVarsSection {
    pub version: Version,
    /// The bit-encoded body, including the leading RL size prefix and
    /// trailing CRC byte pair. Sentinels are *not* included.
    pub body: Vec<u8>,
}

impl HeaderVarsSection {
    /// Parse the section from a byte buffer starting at `offset`. The
    /// buffer must contain the start sentinel at `offset`, followed by
    /// the body, the CRC, and the end sentinel.
    pub fn parse(version: Version, bytes: &[u8], offset: usize) -> DwgResult<Self> {
        // Sentinel guard: 16 bytes.
        if bytes.len() < offset + 16 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let begin = &bytes[offset..offset + 16];
        if begin != HEADER_VARS_BEGIN.as_slice() {
            let mut got = [0u8; 16];
            got.copy_from_slice(begin);
            return Err(DwgError::InvalidSentinel {
                section: "header_variables",
                expected: HEADER_VARS_BEGIN,
                got,
            });
        }
        // The first 4 bytes after the sentinel are the bit-encoded
        // size_in_bits prefix. We need that to locate the CRC + end
        // sentinel without interpreting every header variable.
        let after_sentinel = offset + 16;
        if bytes.len() < after_sentinel + 4 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let mut reader = BitReader::new(&bytes[after_sentinel..]);
        // The size RL is a *byte* count of the body that follows the
        // size field itself, matching LibreDWG's `dwg_decode_header`
        // (`crcpos = pvz + size + 4`). The previous codebase
        // documented this as a `size_in_bits` field, which only
        // happened to agree with the encoder when `size == 0`; for
        // non-empty bodies it would read 8x too few bytes.
        let size_in_bytes = reader.read_rl()? as usize;
        // Body bytes = 4 (RL) + size_in_bytes + 2 (CRC).
        let body_total = 4 + size_in_bytes + 2;
        if bytes.len() < after_sentinel + body_total + 16 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let body = bytes[after_sentinel..after_sentinel + body_total].to_vec();
        // End sentinel.
        let end_off = after_sentinel + body_total;
        let end = &bytes[end_off..end_off + 16];
        if end != HEADER_VARS_END.as_slice() {
            let mut got = [0u8; 16];
            got.copy_from_slice(end);
            return Err(DwgError::InvalidSentinel {
                section: "header_variables",
                expected: HEADER_VARS_END,
                got,
            });
        }
        Ok(Self { version, body })
    }

    /// Encode the section (start sentinel + body + end sentinel) into
    /// `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&HEADER_VARS_BEGIN);
        out.extend_from_slice(&self.body);
        out.extend_from_slice(&HEADER_VARS_END);
    }

    /// Build a minimal-valid section. The body contains:
    /// - 4 bytes RL size_in_bytes = 0
    /// - 2 bytes CRC-X25 over the size field (with seed 0xc0c1)
    ///
    /// This is the bare-frame form. LibreDWG (and AutoCAD) will
    /// reject this for any version that expects to actually decode
    /// the variable table — use [`Self::libredwg_conformant`] for
    /// any file headed for the LibreDWG oracle or AutoCAD.
    pub fn minimal(version: Version) -> Self {
        let mut w = BitWriter::new();
        // size_in_bytes = 0 (no variable bits).
        w.write_rl(0).expect("scratch BitWriter never overflows");
        let mut body = w.into_bytes();
        // CRC-X25 over the body so far.
        let crc = crc_x25(0xc0c1, &body);
        body.extend_from_slice(&crc.to_le_bytes());
        Self { version, body }
    }

    /// Build a LibreDWG-conformant section with the full ~150-field
    /// bit-packed header variable body. Defaults are taken from
    /// [`HeaderVars::default`]; callers that need to pin specific
    /// values can use [`Self::with_vars`] instead.
    ///
    /// For modern (R14+) files this is what gets emitted into the
    /// AcDb:Header section; it is the only form that LibreDWG's
    /// `dwg_decode_header_variables` can parse without buffer-
    /// overflow errors.
    pub fn libredwg_conformant(version: Version) -> Self {
        Self::with_vars(version, &HeaderVars::default())
    }

    /// Build the section using a caller-supplied [`HeaderVars`].
    /// The maintenance-version byte is taken from
    /// [`Version::maintenance_release`].
    pub fn with_vars(version: Version, vars: &HeaderVars) -> Self {
        let body = encode_body(version, MaintVersion(version.maintenance_release()), vars)
            .expect("encode_body is infallible for in-range defaults");
        Self { version, body }
    }

    /// Validate the trailing CRC-X25 against the body.
    pub fn verify_crc(&self) -> DwgResult<()> {
        if self.body.len() < 6 {
            return Err(DwgError::InternalInvariant(format!(
                "header-vars body shorter than RL prefix + CRC: {} bytes",
                self.body.len()
            )));
        }
        let payload_end = self.body.len() - 2;
        let stored = u16::from_le_bytes([self.body[payload_end], self.body[payload_end + 1]]);
        let computed = crc_x25(0xc0c1, &self.body[..payload_end]);
        if stored != computed {
            return Err(DwgError::SectionCrcMismatch {
                section: "header_variables",
                computed: u32::from(computed),
                stored: u32::from(stored),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_section_round_trips_via_buffer() {
        let section = HeaderVarsSection::minimal(Version::R2000);
        let mut buf = Vec::new();
        section.encode(&mut buf);
        let parsed = HeaderVarsSection::parse(Version::R2000, &buf, 0).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn minimal_section_passes_crc() {
        let section = HeaderVarsSection::minimal(Version::R2000);
        section.verify_crc().expect("minimal section must verify");
    }

    #[test]
    fn parse_rejects_wrong_start_sentinel() {
        let mut buf = vec![0u8; 64];
        // Leave the start sentinel zeroed.
        assert!(matches!(
            HeaderVarsSection::parse(Version::R2000, &buf, 0),
            Err(DwgError::InvalidSentinel { .. })
        ));
        let _ = &mut buf;
    }

    #[test]
    fn parse_rejects_wrong_end_sentinel() {
        let section = HeaderVarsSection::minimal(Version::R2000);
        let mut buf = Vec::new();
        section.encode(&mut buf);
        // Corrupt the end sentinel.
        let len = buf.len();
        buf[len - 16] ^= 0xff;
        assert!(matches!(
            HeaderVarsSection::parse(Version::R2000, &buf, 0),
            Err(DwgError::InvalidSentinel { .. })
        ));
    }

    #[test]
    fn verify_crc_detects_corruption() {
        let mut section = HeaderVarsSection::minimal(Version::R2000);
        section.body[0] ^= 0xff;
        assert!(matches!(
            section.verify_crc(),
            Err(DwgError::SectionCrcMismatch { .. })
        ));
    }
}
