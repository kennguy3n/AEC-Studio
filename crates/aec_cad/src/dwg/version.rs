//! DWG file format version, signature detection, and per-version capability flags.
//!
//! Every DWG file begins with a 6-byte ASCII signature `AC10NN` where
//! `NN` identifies the release. The mapping below is from the Open
//! Design Alliance's *OpenDesign Specification for .dwg files* (multiple
//! editions, 2005–present) cross-referenced with AutoCAD release notes.

use super::error::{DwgError, DwgResult};

/// One of the eight DWG release families this codec supports.
///
/// The numeric ordering is *chronological*, not lexicographic: R12
/// (1992) < R14 (1997) < R2000 (1999) < … < R2018 (2017).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    /// AC1009. AutoCAD R11/R12 (1990-1992). Fixed-record entity
    /// format; no class section; no bit-encoded objects.
    R12,
    /// AC1014. AutoCAD R14 (1997). First version with the modern
    /// bit-encoded object format.
    R14,
    /// AC1015. AutoCAD 2000 (1999). Added LWPOLYLINE,
    /// SPLINE refinements, object dictionary.
    R2000,
    /// AC1018. AutoCAD 2004 (2003). Introduced the page-based
    /// system section model and per-section LZ77-style compression.
    R2004,
    /// AC1021. AutoCAD 2007 (2006). Strings switched from CP1252
    /// (TV / Text Value) to UTF-16LE (T / Text).
    R2007,
    /// AC1024. AutoCAD 2010 (2009). Most-deployed modern format.
    R2010,
    /// AC1027. AutoCAD 2013 (2012). Minor object additions.
    R2013,
    /// AC1032. AutoCAD 2018 (2017). New second-header layout;
    /// encrypted handle pages.
    R2018,
}

impl Version {
    /// The 6-byte ASCII signature this version writes at file offset 0.
    pub const fn signature(self) -> &'static [u8; 6] {
        match self {
            Self::R12 => b"AC1009",
            Self::R14 => b"AC1014",
            Self::R2000 => b"AC1015",
            Self::R2004 => b"AC1018",
            Self::R2007 => b"AC1021",
            Self::R2010 => b"AC1024",
            Self::R2013 => b"AC1027",
            Self::R2018 => b"AC1032",
        }
    }

    /// Parse a 6-byte AC tag. Unknown / unsupported tags
    /// produce [`DwgError::UnsupportedVersion`].
    pub fn from_signature(sig: &[u8; 6]) -> DwgResult<Self> {
        for v in Self::all() {
            if sig == v.signature() {
                return Ok(*v);
            }
        }
        Err(DwgError::UnsupportedVersion(
            String::from_utf8_lossy(sig).into_owned(),
        ))
    }

    /// All supported versions in chronological order.
    pub const fn all() -> &'static [Version] {
        &[
            Self::R12,
            Self::R14,
            Self::R2000,
            Self::R2004,
            Self::R2007,
            Self::R2010,
            Self::R2013,
            Self::R2018,
        ]
    }

    /// True for R13+ (i.e. everything except R12) — these versions
    /// share the bit-encoded modern format with class and object-map
    /// sections.
    pub const fn is_modern(self) -> bool {
        !matches!(self, Self::R12)
    }

    /// True for R2004+ — these versions use page-based system
    /// sections with compression.
    pub const fn has_paged_system_sections(self) -> bool {
        matches!(
            self,
            Self::R2004 | Self::R2007 | Self::R2010 | Self::R2013 | Self::R2018
        )
    }

    /// True for R2007+ — strings encoded as UTF-16LE (T type).
    /// Earlier versions use CP1252 (TV type).
    pub const fn uses_utf16_strings(self) -> bool {
        matches!(self, Self::R2007 | Self::R2010 | Self::R2013 | Self::R2018)
    }

    /// True for R2018 only — handle pages are obfuscated with a
    /// magic-byte XOR mask.
    pub const fn has_encrypted_handle_pages(self) -> bool {
        matches!(self, Self::R2018)
    }

    /// Maintenance release byte stored in the file header. Documented
    /// from the OpenDesign spec § "File header — maintenance version".
    pub const fn maintenance_release(self) -> u8 {
        match self {
            Self::R12 => 0,
            Self::R14 => 0,
            Self::R2000 => 6,
            Self::R2004 => 0,
            Self::R2007 => 0,
            Self::R2010 => 4,
            Self::R2013 => 9,
            Self::R2018 => 1,
        }
    }

    /// Human-readable name suitable for logs and UI.
    pub const fn name(self) -> &'static str {
        match self {
            Self::R12 => "AutoCAD R12",
            Self::R14 => "AutoCAD R14",
            Self::R2000 => "AutoCAD 2000",
            Self::R2004 => "AutoCAD 2004",
            Self::R2007 => "AutoCAD 2007",
            Self::R2010 => "AutoCAD 2010",
            Self::R2013 => "AutoCAD 2013",
            Self::R2018 => "AutoCAD 2018",
        }
    }
}

/// Detect the DWG version from the first 6 bytes of a file. Returns
/// `None` if the bytes don't look like a DWG signature at all (e.g.
/// the caller passed a DXF file by mistake). Use [`Version::from_signature`]
/// when you want a hard error for unsupported AC tags.
pub fn detect(bytes: &[u8]) -> Option<Version> {
    if bytes.len() < 6 {
        return None;
    }
    let mut sig = [0u8; 6];
    sig.copy_from_slice(&bytes[..6]);
    Version::from_signature(&sig).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_round_trip() {
        for v in Version::all() {
            let detected = Version::from_signature(v.signature()).unwrap();
            assert_eq!(detected, *v);
        }
    }

    #[test]
    fn detect_works_on_short_buffer() {
        // Short reads return None rather than panicking.
        assert!(detect(b"").is_none());
        assert!(detect(b"AC").is_none());
        assert!(detect(b"AC102").is_none());
    }

    #[test]
    fn detect_returns_version_for_each_supported_tag() {
        for v in Version::all() {
            let mut buf = v.signature().to_vec();
            buf.extend_from_slice(b"\0\0\0\0\0\0\0\0\0\0");
            assert_eq!(detect(&buf), Some(*v));
        }
    }

    #[test]
    fn detect_returns_none_for_unknown_tag() {
        // AC1010 was R10 (1988) — not supported by this codec.
        assert!(detect(b"AC1010\0\0\0\0").is_none());
        // Random bytes that happen to be 6 chars don't fool detection.
        assert!(detect(b"NOTDWG\0\0\0\0").is_none());
        // DXF files start with "  0" (group code 0) — must not match.
        assert!(detect(b"  0\nSECTION\n").is_none());
    }

    #[test]
    fn capability_flags_are_consistent() {
        assert!(!Version::R12.is_modern());
        for v in &[
            Version::R14,
            Version::R2000,
            Version::R2004,
            Version::R2007,
            Version::R2010,
            Version::R2013,
            Version::R2018,
        ] {
            assert!(v.is_modern(), "{v:?} should be modern");
        }

        // R2004+ have paged sections; earlier do not.
        assert!(!Version::R12.has_paged_system_sections());
        assert!(!Version::R14.has_paged_system_sections());
        assert!(!Version::R2000.has_paged_system_sections());
        assert!(Version::R2004.has_paged_system_sections());
        assert!(Version::R2018.has_paged_system_sections());

        // R2007+ are UTF-16LE.
        assert!(!Version::R2004.uses_utf16_strings());
        assert!(Version::R2007.uses_utf16_strings());
        assert!(Version::R2018.uses_utf16_strings());

        // Only R2018 has encrypted handle pages.
        for v in Version::all() {
            assert_eq!(
                v.has_encrypted_handle_pages(),
                *v == Version::R2018,
                "{v:?} encryption flag wrong"
            );
        }
    }
}
