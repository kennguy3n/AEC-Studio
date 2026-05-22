//! Bit-aligned reader for the DWG R13+ stream.
//!
//! Tracks a `(byte_off, bit_off)` cursor inside a borrowed `&[u8]`.
//! All primitive decoders advance the cursor by the exact number of
//! bits documented in the OpenDesign spec.
//!
//! Bit ordering inside each byte: **MSB first**.  Reading the high
//! bit first matches AutoCAD's on-disk encoding (verified against
//! LibreDWG test fixtures).

use crate::dwg::error::{DwgError, DwgResult};

pub struct BitReader<'a> {
    data: &'a [u8],
    byte: usize,
    bit: u8, // 0..=7, counting MSB first (0 = bit 7 of the byte)
}

impl<'a> BitReader<'a> {
    /// Wrap a byte slice. Position starts at the first bit (MSB of
    /// byte 0).
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }

    /// Wrap a byte slice with a starting offset (`byte_off`, in bytes).
    /// The bit cursor starts at the MSB of that byte.
    pub fn with_offset(data: &'a [u8], byte_off: usize) -> Self {
        Self {
            data,
            byte: byte_off,
            bit: 0,
        }
    }

    /// Current cursor position.  Useful for sub-section bounds checks
    /// and for matching against documented offsets in fixtures.
    pub fn position(&self) -> (usize, u8) {
        (self.byte, self.bit)
    }

    /// Total bits read since the start of the buffer.
    pub fn bit_position(&self) -> u64 {
        (self.byte as u64) * 8 + u64::from(self.bit)
    }

    /// True if the cursor has reached or passed the end of the buffer.
    pub fn is_eof(&self) -> bool {
        self.byte >= self.data.len()
    }

    /// Number of bits not yet consumed (`(data.len()*8) - bit_position`).
    pub fn remaining_bits(&self) -> u64 {
        let total = (self.data.len() as u64).saturating_mul(8);
        total.saturating_sub(self.bit_position())
    }

    /// Restore the cursor to an absolute bit position previously
    /// returned by [`Self::bit_position`].
    pub fn set_bit_position(&mut self, bit_pos: u64) -> DwgResult<()> {
        let byte = (bit_pos / 8) as usize;
        let bit = (bit_pos % 8) as u8;
        if byte > self.data.len() {
            return Err(DwgError::UnexpectedEof { byte, bit });
        }
        self.byte = byte;
        self.bit = bit;
        Ok(())
    }

    /// Move the cursor to absolute byte/bit position.
    pub fn seek(&mut self, byte: usize, bit: u8) -> DwgResult<()> {
        if bit >= 8 {
            return Err(DwgError::InternalInvariant(format!(
                "seek bit {bit} out of range 0..=7"
            )));
        }
        if byte > self.data.len() {
            return Err(DwgError::UnexpectedEof { byte, bit });
        }
        self.byte = byte;
        self.bit = bit;
        Ok(())
    }

    /// Advance the cursor to the next byte boundary (no-op if already
    /// aligned). Used between bit-aligned data and byte-aligned data
    /// (e.g. raw IEEE-754 doubles inside a wider bit stream sometimes
    /// appear after a section-padding bit).
    pub fn align_to_byte(&mut self) {
        if self.bit != 0 {
            self.byte += 1;
            self.bit = 0;
        }
    }

    /// Read one bit (B). Returns true for 1, false for 0.
    pub fn read_b(&mut self) -> DwgResult<bool> {
        if self.byte >= self.data.len() {
            return Err(DwgError::UnexpectedEof {
                byte: self.byte,
                bit: self.bit,
            });
        }
        let b = self.data[self.byte];
        // Bit 7 is read first.
        let mask = 1u8 << (7 - self.bit);
        let v = (b & mask) != 0;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Ok(v)
    }

    /// Read 2 bits (BB) as an integer 0..=3.
    pub fn read_bb(&mut self) -> DwgResult<u8> {
        let hi = self.read_b()? as u8;
        let lo = self.read_b()? as u8;
        Ok((hi << 1) | lo)
    }

    /// Read 3 bits (3B) as an integer 0..=7. Used in R24+ for some
    /// object subtype tags.
    pub fn read_3b(&mut self) -> DwgResult<u8> {
        let a = self.read_b()? as u8;
        let b = self.read_b()? as u8;
        let c = self.read_b()? as u8;
        Ok((a << 2) | (b << 1) | c)
    }

    /// Read `n` raw bits as a big-endian u32. `n` must be 1..=32.
    pub fn read_bits_u32(&mut self, n: u8) -> DwgResult<u32> {
        if n == 0 || n > 32 {
            return Err(DwgError::InternalInvariant(format!(
                "read_bits_u32 width {n} out of range 1..=32"
            )));
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.read_b()?);
        }
        Ok(v)
    }

    /// Read `n` raw bits as a big-endian u64. `n` must be 1..=64.
    pub fn read_bits_u64(&mut self, n: u8) -> DwgResult<u64> {
        if n == 0 || n > 64 {
            return Err(DwgError::InternalInvariant(format!(
                "read_bits_u64 width {n} out of range 1..=64"
            )));
        }
        let mut v: u64 = 0;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.read_b()?);
        }
        Ok(v)
    }

    /// Read `n` whole bytes (after any necessary alignment).
    /// Does NOT realign — the reader stays bit-aligned and just
    /// peels off `8*n` bits.
    pub fn read_bytes(&mut self, n: usize) -> DwgResult<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.read_bits_u32(8)? as u8);
        }
        Ok(out)
    }

    /// Read a Bit Short (BS, signed 16-bit-equivalent).
    /// Control bits:
    /// - `00` → followed by 16-bit raw little-endian short
    /// - `01` → followed by 8-bit unsigned byte
    /// - `10` → value is 0
    /// - `11` → value is 256
    pub fn read_bs(&mut self) -> DwgResult<i32> {
        match self.read_bb()? {
            0b00 => {
                // 16-bit raw, little-endian.
                let lo = self.read_bits_u32(8)? as u16;
                let hi = self.read_bits_u32(8)? as u16;
                Ok(i32::from(((hi << 8) | lo) as i16))
            }
            0b01 => Ok(self.read_bits_u32(8)? as i32),
            0b10 => Ok(0),
            0b11 => Ok(256),
            other => Err(DwgError::InvalidBitPattern {
                type_name: "BS",
                bits: other,
            }),
        }
    }

    /// Read a Bit-encoded Object Type (BOT, R2010+).
    /// 2-bit shape prefix:
    /// - `00` → followed by RC (8 bits); value range [0, 255]
    /// - `01` → followed by RC + 0x1f0; value range [0x1f0, 0x2ef]
    /// - else → followed by RS (16 bits)
    ///
    /// See LibreDWG `bit_read_BOT` (bits.c:713).
    pub fn read_bot(&mut self) -> DwgResult<u16> {
        match self.read_bb()? {
            0 => Ok(self.read_bits_u32(8)? as u16),
            1 => Ok((self.read_bits_u32(8)? as u16).wrapping_add(0x1f0)),
            _ => Ok(self.read_rs()?),
        }
    }

    /// Read a Bit Long (BL, signed 32-bit-equivalent).
    /// Control bits:
    /// - `00` → followed by 32-bit raw little-endian long
    /// - `01` → followed by 8-bit unsigned byte
    /// - `10` → value is 0
    /// - `11` → reserved; treated as parse error
    pub fn read_bl(&mut self) -> DwgResult<i64> {
        match self.read_bb()? {
            0b00 => {
                let b0 = self.read_bits_u32(8)?;
                let b1 = self.read_bits_u32(8)?;
                let b2 = self.read_bits_u32(8)?;
                let b3 = self.read_bits_u32(8)?;
                let v = (b3 << 24) | (b2 << 16) | (b1 << 8) | b0;
                Ok(i64::from(v as i32))
            }
            0b01 => Ok(i64::from(self.read_bits_u32(8)?)),
            0b10 => Ok(0),
            other => Err(DwgError::InvalidBitPattern {
                type_name: "BL",
                bits: other,
            }),
        }
    }

    /// Read a Bit Long unsigned (BLu) — same encoding as BL but
    /// interpreted as unsigned. Used for handle code prefixes and a
    /// few flag fields.
    pub fn read_blu(&mut self) -> DwgResult<u64> {
        match self.read_bb()? {
            0b00 => {
                let b0 = self.read_bits_u32(8)?;
                let b1 = self.read_bits_u32(8)?;
                let b2 = self.read_bits_u32(8)?;
                let b3 = self.read_bits_u32(8)?;
                Ok(u64::from((b3 << 24) | (b2 << 16) | (b1 << 8) | b0))
            }
            0b01 => Ok(u64::from(self.read_bits_u32(8)?)),
            0b10 => Ok(0),
            other => Err(DwgError::InvalidBitPattern {
                type_name: "BLu",
                bits: other,
            }),
        }
    }

    /// Read a Bit LongLong (BLL): 3-bit length prefix encoded as
    /// `(BB << 1) | B`, followed by `len` little-endian payload bytes.
    /// Mirrors [`crate::dwg::bits::writer::BitWriter::write_bll`] and
    /// LibreDWG's `bit_read_BLL` for `REQUIREDVERSIONS` /
    /// `preview_size`.
    pub fn read_bll(&mut self) -> DwgResult<u64> {
        let len_hi = self.read_bb()?;
        let len_lo = u8::from(self.read_b()?);
        let len = (len_hi << 1) | len_lo;
        let mut value: u64 = 0;
        for i in 0..len {
            let byte = u64::from(self.read_bits_u32(8)?);
            value |= byte << (i * 8);
        }
        Ok(value)
    }

    /// Raw Char (RC): exactly 8 bits, bit-aligned. Symmetric counterpart
    /// to [`crate::dwg::bits::writer::BitWriter::write_rc`].
    pub fn read_rc(&mut self) -> DwgResult<u8> {
        Ok(self.read_bits_u32(8)? as u8)
    }

    /// Read a Bit Double (BD, IEEE-754 64-bit).
    /// Control bits:
    /// - `00` → followed by 64-bit raw little-endian IEEE-754
    /// - `01` → value is 1.0
    /// - `10` → value is 0.0
    /// - `11` → reserved; treated as parse error
    pub fn read_bd(&mut self) -> DwgResult<f64> {
        match self.read_bb()? {
            0b00 => {
                let mut bytes = [0u8; 8];
                for byte in bytes.iter_mut() {
                    *byte = self.read_bits_u32(8)? as u8;
                }
                Ok(f64::from_le_bytes(bytes))
            }
            0b01 => Ok(1.0),
            0b10 => Ok(0.0),
            other => Err(DwgError::InvalidBitPattern {
                type_name: "BD",
                bits: other,
            }),
        }
    }

    /// Read a raw IEEE-754 64-bit double (RD), little-endian, bit-aligned.
    pub fn read_rd(&mut self) -> DwgResult<f64> {
        let mut bytes = [0u8; 8];
        for byte in bytes.iter_mut() {
            *byte = self.read_bits_u32(8)? as u8;
        }
        Ok(f64::from_le_bytes(bytes))
    }

    /// Read a Raw Long (RL): unsigned 32-bit little-endian, bit-aligned.
    pub fn read_rl(&mut self) -> DwgResult<u32> {
        let mut bytes = [0u8; 4];
        for byte in bytes.iter_mut() {
            *byte = self.read_bits_u32(8)? as u8;
        }
        Ok(u32::from_le_bytes(bytes))
    }

    /// Read a Raw Short (RS): unsigned 16-bit little-endian, bit-aligned.
    pub fn read_rs(&mut self) -> DwgResult<u16> {
        let lo = self.read_bits_u32(8)? as u8;
        let hi = self.read_bits_u32(8)? as u8;
        Ok(u16::from_le_bytes([lo, hi]))
    }

    /// 3 × BD point.
    pub fn read_3bd(&mut self) -> DwgResult<[f64; 3]> {
        Ok([self.read_bd()?, self.read_bd()?, self.read_bd()?])
    }

    /// 2 × RD point.
    pub fn read_2rd(&mut self) -> DwgResult<[f64; 2]> {
        Ok([self.read_rd()?, self.read_rd()?])
    }

    /// 3 × RD point.
    pub fn read_3rd(&mut self) -> DwgResult<[f64; 3]> {
        Ok([self.read_rd()?, self.read_rd()?, self.read_rd()?])
    }

    /// Bit Extrusion (BE) for R2000+: see [`BitWriter::write_be_r2000_plus`].
    /// When the prefix bit is `1`, the extrusion is the OCS default
    /// `(0, 0, 1)`; otherwise it's three full BD values.
    ///
    /// [`BitWriter::write_be_r2000_plus`]: super::writer::BitWriter::write_be_r2000_plus
    pub fn read_be_r2000_plus(&mut self) -> DwgResult<[f64; 3]> {
        if self.read_b()? {
            Ok([0.0, 0.0, 1.0])
        } else {
            let x = self.read_bd()?;
            let y = self.read_bd()?;
            let mut z = self.read_bd()?;
            if x == 0.0 && y == 0.0 {
                z = if z <= 0.0 { -1.0 } else { 1.0 };
            }
            Ok([x, y, z])
        }
    }

    /// Bit Thickness (BT) for R2000+: see [`BitWriter::write_bt_r2000_plus`].
    ///
    /// [`BitWriter::write_bt_r2000_plus`]: super::writer::BitWriter::write_bt_r2000_plus
    pub fn read_bt_r2000_plus(&mut self) -> DwgResult<f64> {
        if self.read_b()? {
            Ok(0.0)
        } else {
            self.read_bd()
        }
    }

    /// Version-aware BT reader. R14 stores a plain BD; R2000+ adds
    /// the 1-bit "is default" prefix. Mirrors LibreDWG
    /// `bit_read_BT` at `bits.c:1297`.
    pub fn read_bt(&mut self, version: crate::dwg::version::Version) -> DwgResult<f64> {
        use crate::dwg::version::Version;
        if matches!(version, Version::R12 | Version::R14) {
            self.read_bd()
        } else {
            self.read_bt_r2000_plus()
        }
    }

    /// Version-aware BE reader. R14 stores a plain 3BD with the
    /// LibreDWG z-normalisation when x=y=0; R2000+ adds the
    /// 1-bit "is default (0,0,1)" prefix. Mirrors LibreDWG
    /// `bit_read_BE` at `bits.c:1112`.
    pub fn read_be(&mut self, version: crate::dwg::version::Version) -> DwgResult<[f64; 3]> {
        use crate::dwg::version::Version;
        if matches!(version, Version::R12 | Version::R14) {
            let x = self.read_bd()?;
            let y = self.read_bd()?;
            let mut z = self.read_bd()?;
            if x == 0.0 && y == 0.0 {
                z = if z <= 0.0 { -1.0 } else { 1.0 };
            }
            Ok([x, y, z])
        } else {
            self.read_be_r2000_plus()
        }
    }

    /// Bit Double With Default (DD). The writer in this crate always
    /// emits `00` (use default) or `11` (full RD); for full
    /// AutoCAD-emitted file compatibility we also handle the `01`
    /// (4-byte patch) and `10` (6-byte patch) shapes by patching the
    /// low bytes of the default's little-endian representation.
    pub fn read_dd(&mut self, default: f64) -> DwgResult<f64> {
        match self.read_bb()? {
            0b00 => Ok(default),
            0b01 => {
                // Patch the low 4 bytes; keep the upper 4 bytes of the
                // default. Per the OpenDesign spec, the patch is
                // little-endian regardless of host endianness.
                let mut bytes = default.to_le_bytes();
                for byte in &mut bytes[0..4] {
                    *byte = self.read_bits_u32(8)? as u8;
                }
                Ok(f64::from_le_bytes(bytes))
            }
            0b10 => {
                // Patch the low 6 bytes (with a documented quirk: the
                // first two on-wire bytes go into byte positions 4-5,
                // and the remaining four go into byte positions 0-3,
                // matching LibreDWG's `bit_read_DD` implementation).
                let mut bytes = default.to_le_bytes();
                bytes[4] = self.read_bits_u32(8)? as u8;
                bytes[5] = self.read_bits_u32(8)? as u8;
                for byte in &mut bytes[0..4] {
                    *byte = self.read_bits_u32(8)? as u8;
                }
                Ok(f64::from_le_bytes(bytes))
            }
            0b11 => self.read_rd(),
            other => Err(DwgError::InvalidBitPattern {
                type_name: "DD",
                bits: other,
            }),
        }
    }

    /// 2 DDs with respective component defaults.
    pub fn read_2dd(&mut self, default: [f64; 2]) -> DwgResult<[f64; 2]> {
        Ok([self.read_dd(default[0])?, self.read_dd(default[1])?])
    }

    /// Modular Char (MC, signed). Variable-length: up to 5 bytes,
    /// 7 data bits per byte, MSB is the continuation flag. The sign
    /// bit is the next-to-MSB of the *last* byte.
    ///
    /// The accumulator is u64 because an i32 at the extremes of its
    /// range (`i32::MIN`, `i32::MAX`) requires 5 MC bytes —
    /// 5 × 7 = 35 bits — to round-trip through `write_mc` without
    /// losing the sign bit. Using u32 here would overflow the shift
    /// (`1 << 34`) on the 5th byte and silently corrupt the value.
    pub fn read_mc(&mut self) -> DwgResult<i32> {
        let mut value: u64 = 0;
        let mut shift: u32 = 0;
        for byte_idx in 0..5 {
            let byte = self.read_bits_u32(8)?;
            let payload = u64::from(byte & 0x7f);
            value |= payload << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                // Final byte. Sign bit is bit 6 of this last byte (i.e.
                // the high bit of the 7-bit payload).
                let sign_bit_mask: u64 = 1u64 << (shift - 1);
                let signed = if value & sign_bit_mask != 0 {
                    // Sign-extend across the full u64, then truncate to
                    // i32. The width of the signed window is `shift`
                    // bits, so the mask of "data bits" is
                    // `(1 << shift) - 1` and inverting it produces the
                    // sign-extension high bits.
                    let extended = value | !((1u64 << shift) - 1);
                    extended as i64 as i32
                } else {
                    value as i32
                };
                return Ok(signed);
            }
            if byte_idx == 4 {
                return Err(DwgError::ModularOverflow {
                    type_name: "MC",
                    bytes: 5,
                });
            }
        }
        Err(DwgError::ModularOverflow {
            type_name: "MC",
            bytes: 5,
        })
    }

    /// Unsigned Modular Char (UMC). Variable-length 7-bit LE chunks
    /// with the 0x80 bit of each byte as continuation flag. Up to 8
    /// bytes (sufficient for any handle value).
    ///
    /// Returns the decoded value as u64; the first byte holds the
    /// least-significant 7 bits and each subsequent byte adds the
    /// next 7 bits. Last byte has the continuation flag cleared.
    ///
    /// See LibreDWG `bit_read_UMC` (bits.c:1006).
    pub fn read_umc(&mut self) -> DwgResult<u64> {
        let mut value: u64 = 0;
        let mut shift: u32 = 0;
        for byte_idx in 0..8 {
            let byte = self.read_bits_u32(8)?;
            let payload = u64::from(byte & 0x7f);
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if byte_idx == 7 {
                return Err(DwgError::ModularOverflow {
                    type_name: "UMC",
                    bytes: 8,
                });
            }
        }
        Err(DwgError::ModularOverflow {
            type_name: "UMC",
            bytes: 8,
        })
    }

    /// Modular Short (MS, unsigned). Variable-length 15-bit LE chunks
    /// with the high bit of the high byte of each chunk as continuation.
    pub fn read_ms(&mut self) -> DwgResult<u32> {
        let mut value: u32 = 0;
        let mut shift = 0;
        for chunk_idx in 0..4 {
            let lo = self.read_bits_u32(8)?;
            let hi = self.read_bits_u32(8)?;
            let chunk = ((hi & 0x7f) << 8) | lo;
            value |= chunk << shift;
            shift += 15;
            if hi & 0x80 == 0 {
                return Ok(value);
            }
            if chunk_idx == 3 {
                return Err(DwgError::ModularOverflow {
                    type_name: "MS",
                    bytes: 8,
                });
            }
        }
        Err(DwgError::ModularOverflow {
            type_name: "MS",
            bytes: 8,
        })
    }

    /// Read a Handle reference (H). Format:
    /// - 4-bit code (high nybble of first byte)
    /// - 4-bit byte count `n` (low nybble of first byte)
    /// - `n` raw bytes, MSB first, as the handle value
    pub fn read_h(&mut self) -> DwgResult<HandleRef> {
        let first = self.read_bits_u32(8)? as u8;
        let code = first >> 4;
        let n = first & 0x0f;
        if n > 8 {
            return Err(DwgError::InvalidHandle { code, bytes: n });
        }
        let mut value: u64 = 0;
        for _ in 0..n {
            value = (value << 8) | u64::from(self.read_bits_u32(8)?);
        }
        Ok(HandleRef { code, value })
    }

    /// Text Value (TV) — CP1252 string used in pre-R2007 files.
    /// Layout: BS length, then `len` ASCII/CP1252 bytes (no
    /// null terminator).
    pub fn read_tv(&mut self) -> DwgResult<String> {
        let len = self.read_bs()?;
        if len < 0 {
            return Err(DwgError::InvalidStringEncoding {
                field: "TV",
                message: format!("negative length {len}"),
            });
        }
        let bytes = self.read_bytes(len as usize)?;
        // CP1252 is a superset of Latin-1 for the 0..=0xff range when
        // mapped 1:1; ASCII is a subset. Convert bytes → char by direct
        // mapping (matches AutoCAD's interpretation of CP1252).
        let s: String = bytes.iter().map(|&b| b as char).collect();
        Ok(s)
    }

    /// Text (T) — UTF-16LE string used in R2007+.
    /// Layout: BS length (number of UTF-16 code units), then
    /// `len * 2` bytes of UTF-16LE.
    pub fn read_t(&mut self) -> DwgResult<String> {
        let len = self.read_bs()?;
        if len < 0 {
            return Err(DwgError::InvalidStringEncoding {
                field: "T",
                message: format!("negative length {len}"),
            });
        }
        let bytes = self.read_bytes((len as usize) * 2)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).map_err(|e| DwgError::InvalidStringEncoding {
            field: "T",
            message: e.to_string(),
        })
    }

    /// Color (CMC, "Color Map Color"). R2004+ extended color encoding.
    /// Layout: BS index. If index has high byte 0xc1..=0xc3, additional
    /// RGB bytes follow.
    pub fn read_cmc(&mut self) -> DwgResult<Color> {
        let index = self.read_bs()?;
        let raw_index = (index & 0xff) as u8;
        let kind = ((index >> 8) & 0xff) as u8;
        match kind {
            0xc0 => Ok(Color::ByLayer),
            0xc1 => Ok(Color::ByBlock),
            0xc2 => {
                // True color: 3 raw bytes (RGB).
                let r = self.read_bits_u32(8)? as u8;
                let g = self.read_bits_u32(8)? as u8;
                let b = self.read_bits_u32(8)? as u8;
                Ok(Color::Rgb(r, g, b))
            }
            0xc3 => {
                // Named color: TV string.
                let name = self.read_tv()?;
                Ok(Color::Named(name))
            }
            _ => Ok(Color::Index(raw_index)),
        }
    }
}

/// Parsed handle reference. The semantics of `code` depend on the
/// surrounding object kind — see OpenDesign spec § "Handle codes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HandleRef {
    pub code: u8,
    pub value: u64,
}

/// Parsed CMC color value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Color {
    /// Standard ACI palette index (0..=255).
    Index(u8),
    /// Inherit from layer.
    ByLayer,
    /// Inherit from containing block.
    ByBlock,
    /// True color RGB.
    Rgb(u8, u8, u8),
    /// Named color (e.g. "Red").
    Named(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_b_consumes_one_bit() {
        // 0b1011_0010
        let mut r = BitReader::new(&[0xb2]);
        assert!(r.read_b().unwrap());
        assert!(!r.read_b().unwrap());
        assert!(r.read_b().unwrap());
        assert!(r.read_b().unwrap());
        assert!(!r.read_b().unwrap());
        assert!(!r.read_b().unwrap());
        assert!(r.read_b().unwrap());
        assert!(!r.read_b().unwrap());
        assert_eq!(r.position(), (1, 0));
    }

    #[test]
    fn read_b_eof() {
        let mut r = BitReader::new(&[0xff]);
        for _ in 0..8 {
            r.read_b().unwrap();
        }
        assert!(matches!(
            r.read_b(),
            Err(DwgError::UnexpectedEof { byte: 1, bit: 0 })
        ));
    }

    #[test]
    fn read_bb_combines_two_bits() {
        // 0b11_00_01_10
        let mut r = BitReader::new(&[0b11_00_01_10]);
        assert_eq!(r.read_bb().unwrap(), 0b11);
        assert_eq!(r.read_bb().unwrap(), 0b00);
        assert_eq!(r.read_bb().unwrap(), 0b01);
        assert_eq!(r.read_bb().unwrap(), 0b10);
    }

    #[test]
    fn read_bs_zero_pattern() {
        // BB=10 → 0
        let mut r = BitReader::new(&[0b10_000000]);
        assert_eq!(r.read_bs().unwrap(), 0);
        assert_eq!(r.position(), (0, 2));
    }

    #[test]
    fn read_bs_256_pattern() {
        // BB=11 → 256
        let mut r = BitReader::new(&[0b11_000000]);
        assert_eq!(r.read_bs().unwrap(), 256);
    }

    #[test]
    fn read_bs_8bit_pattern() {
        // BB=01, then byte 0x42
        let mut r = BitReader::new(&[0b01_010000, 0b10_000000]);
        assert_eq!(r.read_bs().unwrap(), 0x42);
    }

    #[test]
    fn read_bs_16bit_pattern() {
        // BB=00, then LE short 0x1234 → bytes 0x34, 0x12
        // After 2 control bits, the next 16 bits come from:
        //   - first byte: bits 2..=7 of byte 0 (6 bits)
        //   - second byte: bits 0..=7 of byte 1 (8 bits)
        //   - third byte: bits 0..=1 of byte 2 (2 bits)
        // To make this exact we'll handcraft: start at bit 0 with BB=00,
        // then load 0x34, 0x12 across the next 16 bits.
        // Easier: build with the writer in a round-trip test below.
        let bytes = make_bits_be(&[
            (2, 0b00), // BB=00 selector
            (8, 0x34), // low byte
            (8, 0x12), // high byte
        ]);
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bs().unwrap(), 0x1234);
    }

    #[test]
    fn read_bd_special_values() {
        // BB=01 → 1.0
        let mut r1 = BitReader::new(&[0b01_000000]);
        assert_eq!(r1.read_bd().unwrap(), 1.0);
        // BB=10 → 0.0
        let mut r0 = BitReader::new(&[0b10_000000]);
        assert_eq!(r0.read_bd().unwrap(), 0.0);
    }

    #[test]
    fn read_bd_arbitrary_value() {
        let val = std::f64::consts::PI;
        let mut bits: Vec<(u8, u64)> = vec![(2, 0b00)];
        for b in val.to_le_bytes() {
            bits.push((8, u64::from(b)));
        }
        let buf = make_bits_be(&bits);
        let mut r = BitReader::new(&buf);
        assert_eq!(r.read_bd().unwrap(), val);
    }

    #[test]
    fn read_mc_small_positive() {
        // MC = +5 → 1 byte with payload 0b0000101 and continuation=0.
        let mut r = BitReader::new(&[0b0000_0101]);
        assert_eq!(r.read_mc().unwrap(), 5);
    }

    #[test]
    fn read_mc_negative_one() {
        // MC = -1 → 1 byte 0b01111111: payload bits are 1111111
        // (continuation=0, sign bit=1 in the 6th-bit-of-payload position
        // because that's the MSB of the 7-bit payload).
        let mut r = BitReader::new(&[0b0111_1111]);
        assert_eq!(r.read_mc().unwrap(), -1);
    }

    #[test]
    fn read_mc_two_byte_value() {
        // MC = 200. Binary 0b1100_1000 (8 bits). With 7 data bits per
        // byte, that's 0b0000001 (high) << 7 | 0b1001000 (low) = 200.
        // First byte (low): continuation=1, payload=0b1001000 → 0xc8
        // Second byte (high): continuation=0, payload=0b0000001 → 0x01
        let mut r = BitReader::new(&[0xc8, 0x01]);
        assert_eq!(r.read_mc().unwrap(), 200);
    }

    #[test]
    fn read_mc_five_byte_extremes_round_trip() {
        // Values at the extremes of i32 require a 5-byte MC encoding
        // (5 × 7 = 35 bits, needed for sign-extended 32-bit ints).
        // Both writer and reader must agree at the u32-overflow boundary;
        // historically the reader's u32 accumulator overflowed on the
        // 5th byte and corrupted the value silently.
        use crate::dwg::bits::writer::BitWriter;
        for value in [i32::MAX, i32::MIN, 0x4000_0000, -0x4000_0001, 0x0fff_ffff] {
            let mut w = BitWriter::new();
            w.write_mc(value).unwrap();
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            let decoded = r.read_mc().unwrap();
            assert_eq!(decoded, value, "round-trip failed for {value}");
        }
    }

    #[test]
    fn read_ms_single_chunk() {
        // MS = 0x1234. Single chunk: lo=0x34, hi=0x12 (continuation=0).
        let mut r = BitReader::new(&[0x34, 0x12]);
        assert_eq!(r.read_ms().unwrap(), 0x1234);
    }

    #[test]
    fn read_ms_two_chunks() {
        // MS = 0x10000. With 15 bits per chunk:
        //   chunk 0: 0x00000 = lo=0x00, hi=0x80 (continuation bit set)
        //   chunk 1: 0x00002 = lo=0x02, hi=0x00 (terminator)
        // Reassembled: (0 << 0) | (0b10 << 15) = 0x10000.
        let mut r = BitReader::new(&[0x00, 0x80, 0x02, 0x00]);
        assert_eq!(r.read_ms().unwrap(), 0x10000);
    }

    #[test]
    fn read_handle_zero_byte_count() {
        // Handle code=0x5, n=0 → value=0.
        let mut r = BitReader::new(&[0x50]);
        let h = r.read_h().unwrap();
        assert_eq!(h.code, 0x5);
        assert_eq!(h.value, 0);
    }

    #[test]
    fn read_handle_two_byte_value() {
        // Handle code=0x3, n=2, value bytes 0x12, 0x34 → 0x1234.
        let mut r = BitReader::new(&[0x32, 0x12, 0x34]);
        let h = r.read_h().unwrap();
        assert_eq!(h.code, 0x3);
        assert_eq!(h.value, 0x1234);
    }

    #[test]
    fn read_handle_rejects_overlong() {
        // n=9 is invalid (max 8 bytes of handle value).
        let mut r = BitReader::new(&[0x09, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(
            r.read_h(),
            Err(DwgError::InvalidHandle { code: 0, bytes: 9 })
        ));
    }

    #[test]
    fn align_to_byte_is_idempotent_when_aligned() {
        let mut r = BitReader::new(&[0xff, 0xff]);
        r.align_to_byte();
        assert_eq!(r.position(), (0, 0));
        r.read_b().unwrap();
        r.align_to_byte();
        assert_eq!(r.position(), (1, 0));
        r.align_to_byte();
        assert_eq!(r.position(), (1, 0));
    }

    #[test]
    fn read_tv_decodes_ascii() {
        // BS length = 5 (BB=01, byte 0x05), then "Hello".
        let mut bits: Vec<(u8, u64)> = vec![(2, 0b01), (8, 5)];
        for c in b"Hello" {
            bits.push((8, u64::from(*c)));
        }
        let buf = make_bits_be(&bits);
        let mut r = BitReader::new(&buf);
        assert_eq!(r.read_tv().unwrap(), "Hello");
    }

    #[test]
    fn read_t_decodes_utf16le() {
        // BS length = 3 code units, then UTF-16LE of "Hi!"
        let units: Vec<u16> = "Hi!".encode_utf16().collect();
        let mut bits: Vec<(u8, u64)> = vec![(2, 0b01), (8, 3)];
        for u in units {
            bits.push((8, u64::from(u & 0xff)));
            bits.push((8, u64::from(u >> 8)));
        }
        let buf = make_bits_be(&bits);
        let mut r = BitReader::new(&buf);
        assert_eq!(r.read_t().unwrap(), "Hi!");
    }

    /// Test helper: build a byte buffer that, when read MSB-first,
    /// produces the requested `(width, value)` sequence of fields.
    pub(crate) fn make_bits_be(fields: &[(u8, u64)]) -> Vec<u8> {
        let total_bits: u32 = fields.iter().map(|(w, _)| u32::from(*w)).sum();
        let total_bytes = total_bits.div_ceil(8) as usize;
        let mut out = vec![0u8; total_bytes];
        let mut bit_pos: u32 = 0;
        for &(width, value) in fields {
            for i in 0..width {
                // Bit `i` of `value` from the high end of the field.
                let v_bit = (value >> (width - 1 - i)) & 1;
                let byte = (bit_pos as usize) / 8;
                let off = 7 - ((bit_pos as usize) % 8) as u8;
                if v_bit != 0 {
                    out[byte] |= 1 << off;
                }
                bit_pos += 1;
            }
        }
        out
    }
}
