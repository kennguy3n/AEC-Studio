//! Error type for native DWG codec.
//!
//! Distinct from `CadError` because DWG's binary format admits failure
//! modes (truncated bit streams, CRC mismatches, unknown object classes,
//! encrypted-handle-page parse failures) that have no counterpart in the
//! ASCII DXF reader.

use thiserror::Error;

use super::version::Version;

pub type DwgResult<T> = std::result::Result<T, DwgError>;

#[derive(Debug, Error)]
pub enum DwgError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("unexpected end of DWG input at byte {byte}, bit {bit}")]
    UnexpectedEof { byte: usize, bit: u8 },

    #[error("invalid DWG file signature: expected `AC10NN`, got `{0:?}`")]
    InvalidSignature([u8; 6]),

    #[error("unsupported DWG version `{0}`")]
    UnsupportedVersion(String),

    #[error("file header CRC mismatch: computed {computed:#06x}, stored {stored:#06x}")]
    HeaderCrcMismatch { computed: u16, stored: u16 },

    #[error("section `{section}` CRC mismatch: computed {computed:#010x}, stored {stored:#010x}")]
    SectionCrcMismatch {
        section: &'static str,
        computed: u32,
        stored: u32,
    },

    #[error(
        "invalid section sentinel for `{section}`: expected `{expected:02x?}`, got `{got:02x?}`"
    )]
    InvalidSentinel {
        section: &'static str,
        expected: [u8; 16],
        got: [u8; 16],
    },

    #[error("invalid bit-pattern for `{type_name}`: control bits `{bits:#b}` not defined in spec")]
    InvalidBitPattern { type_name: &'static str, bits: u8 },

    #[error("invalid handle reference: code {code:#x}, byte count {bytes}")]
    InvalidHandle { code: u8, bytes: u8 },

    #[error("invalid string encoding in `{field}`: {message}")]
    InvalidStringEncoding {
        field: &'static str,
        message: String,
    },

    #[error(
        "modular value exceeds expected size: type `{type_name}`, after {bytes} continuation bytes"
    )]
    ModularOverflow {
        type_name: &'static str,
        bytes: usize,
    },

    #[error(
        "unsupported in version `{version:?}`: {what}. \
         The native DWG codec supports a working subset across R12 → R2018; \
         this combination is not yet implemented."
    )]
    UnsupportedInVersion { version: Version, what: String },

    #[error("malformed object: class `{class}`, offset {offset:#x}: {message}")]
    MalformedObject {
        class: String,
        offset: u64,
        message: String,
    },

    #[error("object map references handle {handle:#x} at offset {offset:#x} beyond file size {file_size}")]
    DanglingHandle {
        handle: u64,
        offset: u64,
        file_size: usize,
    },

    #[error("invalid layer name `{0}` (must be 1-255 chars, no `<>/\\\":;?*|,=`)")]
    InvalidLayerName(String),

    #[error("output buffer overflow: write would exceed {limit} bytes")]
    WriteOverflow { limit: usize },

    #[error("internal codec invariant violated: {0} (this is a bug — please report)")]
    InternalInvariant(String),
}
