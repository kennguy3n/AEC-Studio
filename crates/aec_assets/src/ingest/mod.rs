//! Format ingest. Reads external mesh formats into [`aec_geometry::Mesh`].

use std::path::Path;

use aec_core::types::Units;
use aec_geometry::Mesh;

pub mod gltf;
pub mod ifc;
pub mod native;
pub mod obj;

/// Errors returned by ingest readers.
///
/// Note: this enum used to carry an `Asset(#[from] AssetError)` wrapper
/// for the rare case where an ingest reader needed to surface an
/// asset-layer error. The variant was never actually constructed, and
/// after adding `AssetError::Ingest(#[from] IngestError)` (the
/// opposite direction, used by `import_path`) the two types formed a
/// mutually-recursive infinite-size cycle. We dropped the unused
/// wrapper rather than boxing it so the error hierarchy stays
/// strictly one-way: ingest errors flow up into asset errors, never
/// the other direction.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unsupported or unrecognised format")]
    UnsupportedFormat,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
}

/// Detected source format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngestFormat {
    /// glTF 2.0 JSON (.gltf) with separate buffer files.
    Gltf,
    /// glTF 2.0 binary (.glb) — single packed file.
    Glb,
    /// Wavefront `.obj` (and optional `.mtl`, which we ignore).
    Obj,
    /// ISO-10303-21 STEP file containing an IFC schema. Reuses the
    /// `aec_bim` tessellator from PR5.
    Ifc,
    /// Native bincode-encoded [`Mesh`] blob; only valid inside this
    /// workspace build.
    Native,
}

impl IngestFormat {
    /// Spec-defined default unit for this format.
    ///
    /// Use this to populate [`PathImportMetadata::source_units`] when
    /// the caller has no out-of-band information about the file's
    /// authoring unit. Callers MAY override (e.g. when a project
    /// convention asserts a different unit), but the default returned
    /// here matches the format's specification so that pipelines that
    /// blindly trust `default_units(detected_format)` do not introduce
    /// a 1000× scale error.
    ///
    /// Per-format rationale:
    ///
    /// | Format         | Default            | Source                                                 |
    /// |----------------|--------------------|--------------------------------------------------------|
    /// | `Gltf`, `Glb`  | [`Units::M`]       | glTF 2.0 §3.5.4: distances are in metres.              |
    /// | `Ifc`          | [`Units::Mm`]      | We tessellate via `aec_bim` whose canonical unit is mm |
    /// |                |                    | (matches the asset DB's internal unit and the most    |
    /// |                |                    | common `IFCUNITASSIGNMENT(LENGTHUNIT)` we see).        |
    /// | `Obj`          | [`Units::Mm`]      | Wavefront `.obj` is unit-less by spec; we adopt the    |
    /// |                |                    | asset DB's canonical unit as the no-conversion default |
    /// |                |                    | so an OBJ authored to project scale round-trips        |
    /// |                |                    | exactly. Override when the OBJ is authored in metres   |
    /// |                |                    | (common for game-engine exports).                      |
    /// | `Native`       | [`Units::Mm`]      | The bincode `Mesh` blob is already in the asset DB's   |
    /// |                |                    | canonical unit; no conversion needed.                  |
    pub fn default_units(self) -> Units {
        match self {
            Self::Gltf | Self::Glb => Units::M,
            Self::Ifc | Self::Obj | Self::Native => Units::Mm,
        }
    }
}

/// A mesh that has been read from a source format.
#[derive(Debug, Clone)]
pub struct IngestedMesh {
    pub mesh: Mesh,
    pub format: IngestFormat,
    /// Path or label of the source — surfaced in errors and audit logs.
    pub source_label: String,
}

/// Sniff the format of a payload from its extension and/or magic bytes.
///
/// Returns `None` when the extension is unknown *and* the content does
/// not match any recognised magic prefix.
pub fn detect_format(path: &Path, data: &[u8]) -> Option<IngestFormat> {
    if let Some(ext) = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
    {
        match ext.as_str() {
            "gltf" => return Some(IngestFormat::Gltf),
            "glb" => return Some(IngestFormat::Glb),
            "obj" => return Some(IngestFormat::Obj),
            "ifc" | "step" | "stp" => return Some(IngestFormat::Ifc),
            "bin" | "aecmesh" => return Some(IngestFormat::Native),
            _ => {}
        }
    }
    // Fall back to content sniffing — these magics are decisive.
    if data.starts_with(b"glTF") {
        return Some(IngestFormat::Glb);
    }
    if data.starts_with(b"{") && find_subslice(data, b"\"asset\"").is_some() {
        return Some(IngestFormat::Gltf);
    }
    if data.starts_with(b"ISO-10303-21;") {
        return Some(IngestFormat::Ifc);
    }
    if data.starts_with(b"# ")
        || data.starts_with(b"o ")
        || data.starts_with(b"v ")
        || data.starts_with(b"mtllib ")
    {
        return Some(IngestFormat::Obj);
    }
    None
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Parse a payload whose format is already known.
pub fn ingest_bytes(data: &[u8], format: IngestFormat) -> Result<IngestedMesh, IngestError> {
    match format {
        IngestFormat::Gltf | IngestFormat::Glb => gltf::parse_bytes(data, "<bytes>", format),
        IngestFormat::Obj => obj::parse_bytes(data, "<bytes>"),
        IngestFormat::Ifc => ifc::parse_bytes(data, "<bytes>"),
        IngestFormat::Native => native::parse(data, "<bytes>"),
    }
}

/// Read + parse a file from disk; the format is auto-detected.
pub fn ingest_path(path: &Path) -> Result<IngestedMesh, IngestError> {
    let bytes = std::fs::read(path)?;
    let format = detect_format(path, &bytes).ok_or(IngestError::UnsupportedFormat)?;
    match format {
        IngestFormat::Gltf | IngestFormat::Glb => gltf::parse_path(path, format),
        IngestFormat::Obj => obj::parse_path(path),
        IngestFormat::Ifc => ifc::parse_bytes(&bytes, path.to_string_lossy()),
        IngestFormat::Native => native::parse(&bytes, path.to_string_lossy()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detect_glb_by_magic() {
        let p = PathBuf::from("anonymous");
        assert_eq!(detect_format(&p, b"glTF\x02"), Some(IngestFormat::Glb));
    }

    #[test]
    fn detect_gltf_by_json_payload() {
        let p = PathBuf::from("anon");
        assert_eq!(
            detect_format(&p, b"{\"asset\":{\"version\":\"2.0\"}}"),
            Some(IngestFormat::Gltf)
        );
    }

    #[test]
    fn detect_ifc_by_header() {
        let p = PathBuf::from("anon");
        assert_eq!(
            detect_format(&p, b"ISO-10303-21;\nHEADER;\n"),
            Some(IngestFormat::Ifc)
        );
    }

    #[test]
    fn detect_obj_by_extension() {
        let p = PathBuf::from("teapot.OBJ");
        assert_eq!(detect_format(&p, b"junk"), Some(IngestFormat::Obj));
    }

    #[test]
    fn detect_unknown_returns_none() {
        let p = PathBuf::from("anon.xyz");
        assert_eq!(detect_format(&p, b"hello"), None);
    }

    #[test]
    fn find_subslice_works() {
        assert_eq!(find_subslice(b"hello world", b"world"), Some(6));
        assert_eq!(find_subslice(b"abcdef", b"zzz"), None);
        assert_eq!(find_subslice(b"abc", b""), None);
    }
}
