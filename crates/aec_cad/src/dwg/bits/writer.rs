//! Bit-aligned writer for the DWG R13+ stream.
//!
//! Mirror image of [`BitReader`]. Every primitive here produces bytes
//! that the reader recovers exactly — round-trip tests assert this for
//! each scalar type.

use crate::dwg::error::{DwgError, DwgResult};

use super::reader::{Color, HandleRef};

pub struct BitWriter {
    data: Vec<u8>,
    bit: u8, // 0..=7 — next bit position to write in `data.last_mut()`
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            bit: 0,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            data: Vec::with_capacity(cap),
            bit: 0,
        }
    }

    /// Consume the writer and return the produced bytes.  The final
    /// byte is zero-padded to the next byte boundary.
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    pub fn position(&self) -> (usize, u8) {
        (
            self.data.len().saturating_sub(usize::from(self.bit != 0)),
            self.bit,
        )
    }

    pub fn bit_position(&self) -> u64 {
        let bytes = if self.bit == 0 {
            self.data.len()
        } else {
            self.data.len() - 1
        };
        (bytes as u64) * 8 + u64::from(self.bit)
    }

    /// Overwrite 32 little-endian bits at `bit_pos` with `value`. The
    /// position must already have been written through (i.e.
    /// `bit_pos + 32 <= bit_position()`). Used by the object-record
    /// encoder to back-patch the R2010+ data-bitsize field once the
    /// data stream length is known.
    pub fn patch_rl_at(&mut self, bit_pos: u64, value: u32) -> DwgResult<()> {
        let cur = self.bit_position();
        if bit_pos + 32 > cur {
            return Err(DwgError::InternalInvariant(format!(
                "patch_rl_at({bit_pos}) would overrun buffer of {cur} bits"
            )));
        }
        let bytes = value.to_le_bytes();
        for (byte_idx, src) in bytes.iter().enumerate() {
            let base_bit = bit_pos + (byte_idx as u64) * 8;
            for i in 0..8u8 {
                let abs_bit = base_bit + u64::from(i);
                let byte_in_buf = (abs_bit / 8) as usize;
                let bit_in_byte = (abs_bit % 8) as u8;
                let mask = 1u8 << (7 - bit_in_byte);
                let new_bit = (*src >> (7 - i)) & 1;
                if new_bit == 1 {
                    self.data[byte_in_buf] |= mask;
                } else {
                    self.data[byte_in_buf] &= !mask;
                }
            }
        }
        Ok(())
    }

    /// Pad the current byte to the next byte boundary with zero bits.
    pub fn align_to_byte(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            // The current byte is already in `data`; the writer will
            // start a fresh one on the next `write_b`.
        }
    }

    pub fn write_b(&mut self, value: bool) -> DwgResult<()> {
        if self.bit == 0 {
            self.data.push(0);
        }
        if value {
            let byte_idx = self.data.len() - 1;
            self.data[byte_idx] |= 1 << (7 - self.bit);
        }
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
        }
        Ok(())
    }

    pub fn write_bb(&mut self, value: u8) -> DwgResult<()> {
        if value > 3 {
            return Err(DwgError::InternalInvariant(format!(
                "write_bb value {value} out of range 0..=3"
            )));
        }
        self.write_b((value >> 1) & 1 != 0)?;
        self.write_b(value & 1 != 0)
    }

    pub fn write_3b(&mut self, value: u8) -> DwgResult<()> {
        if value > 7 {
            return Err(DwgError::InternalInvariant(format!(
                "write_3b value {value} out of range 0..=7"
            )));
        }
        self.write_b((value >> 2) & 1 != 0)?;
        self.write_b((value >> 1) & 1 != 0)?;
        self.write_b(value & 1 != 0)
    }

    pub fn write_bits_u32(&mut self, n: u8, value: u32) -> DwgResult<()> {
        if n == 0 || n > 32 {
            return Err(DwgError::InternalInvariant(format!(
                "write_bits_u32 width {n} out of range 1..=32"
            )));
        }
        for i in 0..n {
            self.write_b((value >> (n - 1 - i)) & 1 != 0)?;
        }
        Ok(())
    }

    pub fn write_bits_u64(&mut self, n: u8, value: u64) -> DwgResult<()> {
        if n == 0 || n > 64 {
            return Err(DwgError::InternalInvariant(format!(
                "write_bits_u64 width {n} out of range 1..=64"
            )));
        }
        for i in 0..n {
            self.write_b((value >> (n - 1 - i)) & 1 != 0)?;
        }
        Ok(())
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> DwgResult<()> {
        for &b in bytes {
            self.write_bits_u32(8, u32::from(b))?;
        }
        Ok(())
    }

    /// Bit Short — encode using the smallest of the four shapes.
    ///
    /// The 16-bit shape (BB=0b00) carries a signed 16-bit value. Values
    /// outside `[-32768, 32767]` cannot be represented and the writer
    /// returns [`DwgError::InternalInvariant`] rather than silently
    /// truncating. The 0 and 256 shorthand (BB=0b10 and BB=0b11) and the
    /// unsigned-byte shape (BB=0b01, range `0..=255`) take precedence in
    /// that order — `write_bs(0)` produces 2 bits, `write_bs(256)`
    /// produces 2 bits, `write_bs(5)` produces 10 bits, and any other
    /// in-range value produces 18 bits.
    pub fn write_bs(&mut self, value: i32) -> DwgResult<()> {
        if value == 0 {
            self.write_bb(0b10)
        } else if value == 256 {
            self.write_bb(0b11)
        } else if (0..=255).contains(&value) {
            self.write_bb(0b01)?;
            self.write_bits_u32(8, value as u32)
        } else if (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&value) {
            self.write_bb(0b00)?;
            let v = value as i16 as u16;
            self.write_bits_u32(8, u32::from(v & 0xff))?;
            self.write_bits_u32(8, u32::from(v >> 8))
        } else {
            Err(DwgError::InternalInvariant(format!(
                "write_bs value {value} out of range [-32768, 32767]; \
                 use write_bl for wider values"
            )))
        }
    }

    /// Bit Long — encode using the smallest of the three shapes.
    pub fn write_bl(&mut self, value: i64) -> DwgResult<()> {
        if value == 0 {
            self.write_bb(0b10)
        } else if (0..=255).contains(&value) {
            self.write_bb(0b01)?;
            self.write_bits_u32(8, value as u32)
        } else {
            self.write_bb(0b00)?;
            let v = value as i32 as u32;
            self.write_bits_u32(8, v & 0xff)?;
            self.write_bits_u32(8, (v >> 8) & 0xff)?;
            self.write_bits_u32(8, (v >> 16) & 0xff)?;
            self.write_bits_u32(8, (v >> 24) & 0xff)
        }
    }

    /// Bit Long unsigned — same encoding as BL but for u64 values up
    /// to u32::MAX. Larger values would require a 64-bit shape that
    /// the BL primitive doesn't have, so we reject them.
    pub fn write_blu(&mut self, value: u64) -> DwgResult<()> {
        if value > u64::from(u32::MAX) {
            return Err(DwgError::InternalInvariant(format!(
                "write_blu value {value} exceeds u32::MAX"
            )));
        }
        if value == 0 {
            self.write_bb(0b10)
        } else if value <= 255 {
            self.write_bb(0b01)?;
            self.write_bits_u32(8, value as u32)
        } else {
            self.write_bb(0b00)?;
            let v = value as u32;
            self.write_bits_u32(8, v & 0xff)?;
            self.write_bits_u32(8, (v >> 8) & 0xff)?;
            self.write_bits_u32(8, (v >> 16) & 0xff)?;
            self.write_bits_u32(8, (v >> 24) & 0xff)
        }
    }

    /// Bit Double — encode using the smallest of the three shapes.
    pub fn write_bd(&mut self, value: f64) -> DwgResult<()> {
        if value == 0.0 {
            self.write_bb(0b10)
        } else if value == 1.0 {
            self.write_bb(0b01)
        } else {
            self.write_bb(0b00)?;
            for b in value.to_le_bytes() {
                self.write_bits_u32(8, u32::from(b))?;
            }
            Ok(())
        }
    }

    pub fn write_rd(&mut self, value: f64) -> DwgResult<()> {
        for b in value.to_le_bytes() {
            self.write_bits_u32(8, u32::from(b))?;
        }
        Ok(())
    }

    /// Write a Raw Long (RL): unsigned 32-bit little-endian, bit-aligned.
    pub fn write_rl(&mut self, value: u32) -> DwgResult<()> {
        for b in value.to_le_bytes() {
            self.write_bits_u32(8, u32::from(b))?;
        }
        Ok(())
    }

    /// Write a Raw Short (RS): unsigned 16-bit little-endian, bit-aligned.
    pub fn write_rs(&mut self, value: u16) -> DwgResult<()> {
        for b in value.to_le_bytes() {
            self.write_bits_u32(8, u32::from(b))?;
        }
        Ok(())
    }

    pub fn write_3bd(&mut self, value: [f64; 3]) -> DwgResult<()> {
        self.write_bd(value[0])?;
        self.write_bd(value[1])?;
        self.write_bd(value[2])
    }

    pub fn write_2rd(&mut self, value: [f64; 2]) -> DwgResult<()> {
        self.write_rd(value[0])?;
        self.write_rd(value[1])
    }

    pub fn write_3rd(&mut self, value: [f64; 3]) -> DwgResult<()> {
        self.write_rd(value[0])?;
        self.write_rd(value[1])?;
        self.write_rd(value[2])
    }

    /// Bit Extrusion (BE) for R2000+: 1-bit "is default" prefix; if
    /// the extrusion vector equals the OCS default `(0, 0, 1)` we
    /// write only the prefix bit, otherwise we write the prefix bit
    /// followed by three BD components. Mirrors LibreDWG's
    /// `bit_write_BE` so files written by either codec can be read by
    /// the other.
    pub fn write_be_r2000_plus(&mut self, value: [f64; 3]) -> DwgResult<()> {
        if value[0] == 0.0 && value[1] == 0.0 && value[2] == 1.0 {
            self.write_b(true)
        } else {
            self.write_b(false)?;
            self.write_bd(value[0])?;
            self.write_bd(value[1])?;
            // LibreDWG normalises the z component when x = y = 0 so
            // the on-wire vector matches what AutoCAD emits; we
            // mirror that here so round-tripping through external
            // tools is bit-stable.
            let z = if value[0] == 0.0 && value[1] == 0.0 {
                if value[2] <= 0.0 {
                    -1.0
                } else {
                    1.0
                }
            } else {
                value[2]
            };
            self.write_bd(z)
        }
    }

    /// Bit Thickness (BT) for R2000+: 1-bit "is default (zero)"
    /// prefix; non-zero thicknesses are written as the prefix bit + a
    /// BD. Matches LibreDWG's `bit_write_BT`.
    pub fn write_bt_r2000_plus(&mut self, value: f64) -> DwgResult<()> {
        if value == 0.0 {
            self.write_b(true)
        } else {
            self.write_b(false)?;
            self.write_bd(value)
        }
    }

    /// Bit Double With Default (DD): two control bits select between
    /// reusing the default verbatim (00), patching the low 4 bytes
    /// (01), patching the low 6 bytes (10), or reading a full 8-byte
    /// RD (11). For simplicity and round-trip stability we always
    /// emit `00` when `value == default` and `11` (raw RD) otherwise;
    /// this is a valid encoding per the OpenDesign specification and
    /// matches what LibreDWG emits when it can't infer a smaller
    /// patch shape.
    pub fn write_dd(&mut self, value: f64, default: f64) -> DwgResult<()> {
        if value.to_bits() == default.to_bits() {
            self.write_bb(0b00)
        } else {
            self.write_bb(0b11)?;
            self.write_rd(value)
        }
    }

    /// 2 DDs with respective component defaults.
    pub fn write_2dd(&mut self, value: [f64; 2], default: [f64; 2]) -> DwgResult<()> {
        self.write_dd(value[0], default[0])?;
        self.write_dd(value[1], default[1])
    }

    /// Modular Char (signed). Up to 4 bytes of 7-bit payload.
    pub fn write_mc(&mut self, value: i32) -> DwgResult<()> {
        // Determine the minimal byte count needed. We encode the value
        // as 32-bit two's-complement and peel off 7 bits per byte until
        // the remaining bits all match the sign bit AND the next byte's
        // payload high bit also matches it.
        let bytes: Vec<u8> = {
            let mut chunks = Vec::with_capacity(5);
            let mut remaining = value;
            loop {
                let payload = (remaining & 0x7f) as u8;
                let next = remaining >> 7;
                let payload_sign_bit = payload & 0x40 != 0;
                let next_is_sign_extension =
                    (next == 0 && !payload_sign_bit) || (next == -1 && payload_sign_bit);
                chunks.push(payload);
                if next_is_sign_extension {
                    break;
                }
                remaining = next;
                if chunks.len() == 4 {
                    chunks.push((remaining & 0x7f) as u8);
                    break;
                }
            }
            chunks
        };
        for (i, payload) in bytes.iter().enumerate() {
            let cont = if i == bytes.len() - 1 { 0u8 } else { 0x80 };
            self.write_bits_u32(8, u32::from(cont | payload))?;
        }
        Ok(())
    }

    /// Modular Short (unsigned). Up to 4 chunks of 15-bit LE payload.
    pub fn write_ms(&mut self, value: u32) -> DwgResult<()> {
        let mut chunks: Vec<u16> = Vec::with_capacity(3);
        let mut remaining = value;
        loop {
            let chunk = (remaining & 0x7fff) as u16;
            remaining >>= 15;
            chunks.push(chunk);
            if remaining == 0 {
                break;
            }
            if chunks.len() == 3 {
                if remaining != 0 {
                    chunks.push((remaining & 0x7fff) as u16);
                }
                break;
            }
        }
        for (i, chunk) in chunks.iter().enumerate() {
            let cont = if i == chunks.len() - 1 { 0u16 } else { 0x8000 };
            let combined = cont | chunk;
            self.write_bits_u32(8, u32::from(combined & 0xff))?;
            self.write_bits_u32(8, u32::from(combined >> 8))?;
        }
        Ok(())
    }

    pub fn write_h(&mut self, handle: HandleRef) -> DwgResult<()> {
        if handle.code > 0x0f {
            return Err(DwgError::InternalInvariant(format!(
                "handle code {} exceeds 4 bits",
                handle.code
            )));
        }
        // Count significant bytes (MSB first, drop leading zeros).
        let bytes_needed: u8 = if handle.value == 0 {
            0
        } else {
            let bits = 64 - handle.value.leading_zeros() as u8;
            bits.div_ceil(8)
        };
        if bytes_needed > 8 {
            return Err(DwgError::InternalInvariant(format!(
                "handle value 0x{:x} needs > 8 bytes",
                handle.value
            )));
        }
        let prefix = (handle.code << 4) | bytes_needed;
        self.write_bits_u32(8, u32::from(prefix))?;
        for i in (0..bytes_needed).rev() {
            let byte = ((handle.value >> (i * 8)) & 0xff) as u32;
            self.write_bits_u32(8, byte)?;
        }
        Ok(())
    }

    pub fn write_tv(&mut self, s: &str) -> DwgResult<()> {
        // CP1252 is a superset of ISO-8859-1 for our purposes.  We
        // reject any code point > 0xff so the round-trip is lossless.
        // The length prefix is the number of *output bytes*, which
        // equals the number of `chars()` after the > 0xff guard (each
        // char maps to exactly one CP1252 byte). Using `s.len()` here
        // would double-count multi-byte UTF-8 sequences in the
        // Rust-side `&str` and break the read-side `read_bytes(len)`.
        let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
        for ch in s.chars() {
            let cp = ch as u32;
            if cp > 0xff {
                return Err(DwgError::InvalidStringEncoding {
                    field: "TV",
                    message: format!(
                        "char `{ch}` (U+{cp:04X}) cannot be encoded in CP1252 — \
                         use the T (UTF-16) primitive for non-Latin text"
                    ),
                });
            }
            bytes.push(cp as u8);
        }
        self.write_bs(bytes.len() as i32)?;
        for b in bytes {
            self.write_bits_u32(8, u32::from(b))?;
        }
        Ok(())
    }

    pub fn write_t(&mut self, s: &str) -> DwgResult<()> {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.write_bs(units.len() as i32)?;
        for u in units {
            self.write_bits_u32(8, u32::from(u & 0xff))?;
            self.write_bits_u32(8, u32::from(u >> 8))?;
        }
        Ok(())
    }

    pub fn write_cmc(&mut self, color: &Color) -> DwgResult<()> {
        // The colour tag is the signed 16-bit value whose unsigned
        // representation is 0xc0kk for special colours (ByLayer = 0xc000,
        // ByBlock = 0xc100, RGB = 0xc200, Named = 0xc300). We pass the
        // signed equivalent directly so `write_bs` doesn't have to rely
        // on silent i32→i16 truncation — the `i16::from_le_bytes(...)`
        // form makes the bit pattern → signed value conversion explicit.
        const BY_LAYER: i32 = i16::from_le_bytes([0x00, 0xc0]) as i32;
        const BY_BLOCK: i32 = i16::from_le_bytes([0x00, 0xc1]) as i32;
        const RGB_TAG: i32 = i16::from_le_bytes([0x00, 0xc2]) as i32;
        const NAMED_TAG: i32 = i16::from_le_bytes([0x00, 0xc3]) as i32;
        match color {
            Color::Index(idx) => self.write_bs(i32::from(*idx)),
            Color::ByLayer => self.write_bs(BY_LAYER),
            Color::ByBlock => self.write_bs(BY_BLOCK),
            Color::Rgb(r, g, b) => {
                self.write_bs(RGB_TAG)?;
                self.write_bits_u32(8, u32::from(*r))?;
                self.write_bits_u32(8, u32::from(*g))?;
                self.write_bits_u32(8, u32::from(*b))
            }
            Color::Named(name) => {
                self.write_bs(NAMED_TAG)?;
                self.write_tv(name)
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    reason = "explicit |r| r.read_X() closures keep the write/read pairs symmetric \
              and grep-able in the round_trip helper; clippy's suggestion would \
              hide the symmetry behind method references"
)]
mod tests {
    use super::super::reader::BitReader;
    use super::*;

    fn round_trip<F, G, T: std::fmt::Debug + PartialEq>(write: F, read: G) -> T
    where
        F: FnOnce(&mut BitWriter) -> DwgResult<()>,
        G: FnOnce(&mut BitReader<'_>) -> DwgResult<T>,
    {
        let mut w = BitWriter::new();
        write(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        read(&mut r).unwrap()
    }

    #[test]
    fn b_round_trips() {
        assert!(!round_trip(|w| w.write_b(false), |r| r.read_b()));
        assert!(round_trip(|w| w.write_b(true), |r| r.read_b()));
    }

    #[test]
    fn bb_round_trips_all_values() {
        for v in 0..=3u8 {
            assert_eq!(v, round_trip(move |w| w.write_bb(v), |r| r.read_bb()));
        }
    }

    #[test]
    fn bs_round_trips_special_values() {
        for v in [0, 256, 1, 255, -1, -32768, 32767, 32, 100] {
            assert_eq!(v, round_trip(move |w| w.write_bs(v), |r| r.read_bs()));
        }
    }

    #[test]
    fn bs_rejects_values_outside_i16_range() {
        let mut w = BitWriter::new();
        // Just above i16::MAX (excluding the 0..=256 shorthand window).
        let err = w.write_bs(32_768).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
        // Just below i16::MIN.
        let err = w.write_bs(-32_769).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
        // 0xc000 used to silently truncate to -16384 via `as i16` cast;
        // the new contract is that the caller must pass the signed form
        // (CMC encoder does this explicitly).
        let err = w.write_bs(0xc000).unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
    }

    #[test]
    fn cmc_uses_signed_form_for_special_tags() {
        // Round-trip every Color variant: the tag bit pattern on the
        // wire is byte-identical between encoder/decoder regardless of
        // whether write_bs receives the signed (-16384) or unsigned
        // (0xc000) representation, so this serves as a regression test
        // that write_cmc no longer relies on silent i32 -> i16
        // truncation in write_bs.
        for color in [
            Color::Index(7),
            Color::ByLayer,
            Color::ByBlock,
            Color::Rgb(0x80, 0x40, 0xff),
            Color::Named("MAGENTA".into()),
        ] {
            let color2 = color.clone();
            let got = round_trip(move |w| w.write_cmc(&color2), |r| r.read_cmc());
            assert_eq!(color, got, "CMC round-trip failed for {color:?}");
        }
    }

    #[test]
    fn bl_round_trips_special_values() {
        for v in [0_i64, 1, 255, 256, 32767, -1, -2_147_483_648, 2_147_483_647] {
            assert_eq!(v, round_trip(move |w| w.write_bl(v), |r| r.read_bl()));
        }
    }

    #[test]
    fn blu_round_trips() {
        for v in [0_u64, 1, 255, 256, 65535, 65536, 4_000_000_000] {
            assert_eq!(v, round_trip(move |w| w.write_blu(v), |r| r.read_blu()));
        }
    }

    #[test]
    fn bd_round_trips_special_values() {
        for v in [0.0_f64, 1.0, -1.0, std::f64::consts::PI, f64::MIN, f64::MAX] {
            let got = round_trip(move |w| w.write_bd(v), |r| r.read_bd());
            assert_eq!(v.to_bits(), got.to_bits(), "BD round-trip failed for {v}");
        }
    }

    #[test]
    fn be_default_extrusion_uses_one_bit() {
        // OCS default (0, 0, 1) compresses to a single `1` bit.
        let mut w = BitWriter::new();
        w.write_be_r2000_plus([0.0, 0.0, 1.0]).unwrap();
        // Position cursor moved 1 bit forward; total emitted length
        // is still one byte (the padding bits don't affect the
        // bit_position metric we expose).
        assert_eq!(w.bit_position(), 1);
        // Round-trip recovers the default.
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_be_r2000_plus().unwrap(), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn be_explicit_extrusion_round_trips() {
        for vec in [
            [1.0, 0.0, 0.0],
            [0.5, 0.5, std::f64::consts::FRAC_1_SQRT_2],
            // Verifies the LibreDWG-compatible z-normalisation: when
            // x = y = 0 and the on-wire z is negative, the recovered
            // z is exactly -1.0 rather than the original input.
            [0.0, 0.0, -42.0],
        ] {
            let got = round_trip(
                move |w| w.write_be_r2000_plus(vec),
                |r| r.read_be_r2000_plus(),
            );
            if vec[0] == 0.0 && vec[1] == 0.0 {
                // Normalised path: z is ±1, sign matches input.
                assert!(got[2] == 1.0 || got[2] == -1.0);
            } else {
                assert_eq!(vec, got);
            }
        }
    }

    #[test]
    fn bt_round_trips() {
        // Default zero compresses to one bit; non-default values use
        // bit + full BD.
        let zero = round_trip(|w| w.write_bt_r2000_plus(0.0), |r| r.read_bt_r2000_plus());
        assert_eq!(zero, 0.0);
        let thick = round_trip(|w| w.write_bt_r2000_plus(2.5), |r| r.read_bt_r2000_plus());
        assert_eq!(thick, 2.5);
    }

    #[test]
    fn dd_round_trips_via_default_and_raw_shapes() {
        // Value equal to default → BB=00 shape, no bytes after.
        let same = round_trip(|w| w.write_dd(4.0, 4.0), |r| r.read_dd(4.0));
        assert_eq!(same, 4.0);
        // Differing value → BB=11 shape, full 8-byte RD.
        let diff = round_trip(|w| w.write_dd(7.25, 4.0), |r| r.read_dd(4.0));
        assert_eq!(diff, 7.25);
    }

    #[test]
    fn rd_round_trips() {
        for v in [0.0_f64, std::f64::consts::E, -42.0, 1e308] {
            let got = round_trip(move |w| w.write_rd(v), |r| r.read_rd());
            assert_eq!(v.to_bits(), got.to_bits());
        }
    }

    #[test]
    fn point_3bd_round_trips() {
        let p = [1.0, 2.0, 3.0];
        let got: [f64; 3] = round_trip(move |w| w.write_3bd(p), |r| r.read_3bd());
        assert_eq!(got, p);
    }

    #[test]
    fn mc_round_trips() {
        for v in [
            0_i32, 1, -1, 63, -64, 127, -128, 128, 255, -255, 1024, -1024, 1_048_575, -1_048_576,
        ] {
            let got = round_trip(move |w| w.write_mc(v), |r| r.read_mc());
            assert_eq!(got, v, "MC round-trip failed for {v}");
        }
    }

    #[test]
    fn ms_round_trips() {
        for v in [0_u32, 1, 32767, 32768, 65535, 0x10000, 0x7fff_ffff] {
            let got = round_trip(move |w| w.write_ms(v), |r| r.read_ms());
            assert_eq!(got, v, "MS round-trip failed for {v:#x}");
        }
    }

    #[test]
    fn handle_round_trips() {
        for h in [
            HandleRef {
                code: 0x0,
                value: 0,
            },
            HandleRef {
                code: 0x5,
                value: 0x42,
            },
            HandleRef {
                code: 0x3,
                value: 0xdead_beef,
            },
            HandleRef {
                code: 0xc,
                value: 0xffff_ffff_ffff_ffff,
            },
        ] {
            let got = round_trip(move |w| w.write_h(h), |r| r.read_h());
            assert_eq!(got, h, "H round-trip failed for {h:?}");
        }
    }

    #[test]
    fn tv_round_trips_ascii_and_latin1() {
        for s in ["", "Hello", "café", "MTEXT \\P{}"] {
            let owned = s.to_string();
            let got = round_trip(move |w| w.write_tv(&owned), |r| r.read_tv());
            assert_eq!(got, s);
        }
    }

    #[test]
    fn tv_rejects_non_latin1() {
        let mut w = BitWriter::new();
        let err = w.write_tv("漢字").unwrap_err();
        assert!(matches!(err, DwgError::InvalidStringEncoding { .. }));
    }

    #[test]
    fn t_round_trips_utf16() {
        for s in ["", "ASCII", "漢字テスト", "🚀 rocket"] {
            let owned = s.to_string();
            let got = round_trip(move |w| w.write_t(&owned), |r| r.read_t());
            assert_eq!(got, s);
        }
    }

    #[test]
    fn cmc_round_trips() {
        for c in [
            Color::Index(7),
            Color::ByLayer,
            Color::ByBlock,
            Color::Rgb(255, 128, 0),
            Color::Named("Crimson".into()),
        ] {
            let want = c.clone();
            let got = round_trip(move |w| w.write_cmc(&c), |r| r.read_cmc());
            assert_eq!(got, want);
        }
    }

    #[test]
    fn round_trip_mixed_stream() {
        // Sanity-check a heterogeneous stream survives intact, modeling
        // a real entity's mix of B, BS, BD, H, T fields.
        let mut w = BitWriter::new();
        w.write_b(true).unwrap();
        w.write_bs(-42).unwrap();
        w.write_bd(std::f64::consts::PI).unwrap();
        w.write_h(HandleRef {
            code: 0x3,
            value: 0x1234,
        })
        .unwrap();
        w.write_t("hello world").unwrap();
        w.write_3bd([1.0, 2.0, 3.0]).unwrap();
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert!(r.read_b().unwrap());
        assert_eq!(r.read_bs().unwrap(), -42);
        assert_eq!(r.read_bd().unwrap(), std::f64::consts::PI);
        assert_eq!(
            r.read_h().unwrap(),
            HandleRef {
                code: 0x3,
                value: 0x1234
            }
        );
        assert_eq!(r.read_t().unwrap(), "hello world");
        assert_eq!(r.read_3bd().unwrap(), [1.0, 2.0, 3.0]);
    }
}
