//! Extension system for AEC Studio.
//!
//! Extensions ship as a directory of JSON manifests plus optional payload
//! files (asset packs, template JSON, schedule definitions, etc.). The
//! design follows §8 of `PROPOSAL.md`:
//!
//! * Every extension declares an [`ExtensionType`] (asset pack, template,
//!   schedule, export target, AI tool, importer).
//! * Every extension declares the [`Permission`]s it needs up-front.
//! * Extensions can be signed with Ed25519 (see
//!   [`crate::extension_permissions::verify_signature`]); unsigned
//!   extensions still load but the loader records a `signed: false` flag
//!   so the UI can surface a warning.
//! * The [`ExtensionLoader`] reads a flat directory of `<id>/manifest.json`
//!   files, validates each one through
//!   [`crate::extension_permissions::validate_manifest`], optionally
//!   verifies its signature, and hands the validated set to
//!   [`ExtensionRegistry`].
//!
//! This module deliberately stays free of behaviour: every per-type "host"
//! (asset DB integration, template loader merge, schedule registry, export
//! target list, AI tool dispatch) lives in the crate that already owns the
//! corresponding subsystem.
//!
//! ```no_run
//! use aec_core::extensions::{ExtensionLoader, LoadOptions};
//!
//! let loader = ExtensionLoader::new("/path/to/extensions");
//! // `LoadOptions::default()` is the conservative production default and
//! // rejects any unsigned manifest. For local dev / examples where the
//! // extension directory isn't signed yet, use `LoadOptions::allow_unsigned()`.
//! let registry = loader.load(&LoadOptions::allow_unsigned()).unwrap();
//! for ext in registry.iter() {
//!     println!("{} {} ({:?})", ext.manifest.id, ext.manifest.version, ext.manifest.kind);
//! }
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::extension_permissions::{
    validate_manifest, verify_signature_against, ManifestError, SignatureError, TrustStore,
};

// ---------------------------------------------------------------------------
// Public IDs and enums
// ---------------------------------------------------------------------------

/// Stable identifier for an extension. Format is free-form but the loader
/// rejects any id containing path separators (`/`, `\`) or relative-path
/// markers (`..`) so extension ids are always safe to use as directory
/// names or registry keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExtensionId(pub String);

impl ExtensionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ExtensionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Categories of extension. New variants are additive — never remove or
/// renumber existing ones because manifests on disk depend on the
/// `serde(rename_all = "snake_case")` encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionType {
    AssetPack,
    Template,
    Schedule,
    ExportTarget,
    AiTool,
    Importer,
}

impl ExtensionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AssetPack => "asset_pack",
            Self::Template => "template",
            Self::Schedule => "schedule",
            Self::ExportTarget => "export_target",
            Self::AiTool => "ai_tool",
            Self::Importer => "importer",
        }
    }

    /// Every recognised type, in stable declaration order. Used by the
    /// validator to reject unknown types early.
    pub const ALL: &'static [ExtensionType] = &[
        ExtensionType::AssetPack,
        ExtensionType::Template,
        ExtensionType::Schedule,
        ExtensionType::ExportTarget,
        ExtensionType::AiTool,
        ExtensionType::Importer,
    ];
}

/// Permissions an extension can request. The set is closed; a manifest
/// asking for any other string is rejected by
/// [`validate_manifest`](crate::extension_permissions::validate_manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    FilesystemRead,
    FilesystemWrite,
    Network,
    GeometryRead,
    GeometryWrite,
    AiTools,
    AuditLog,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemRead => "filesystem_read",
            Self::FilesystemWrite => "filesystem_write",
            Self::Network => "network",
            Self::GeometryRead => "geometry_read",
            Self::GeometryWrite => "geometry_write",
            Self::AiTools => "ai_tools",
            Self::AuditLog => "audit_log",
        }
    }

    pub const ALL: &'static [Permission] = &[
        Permission::FilesystemRead,
        Permission::FilesystemWrite,
        Permission::Network,
        Permission::GeometryRead,
        Permission::GeometryWrite,
        Permission::AiTools,
        Permission::AuditLog,
    ];
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// Detached Ed25519 signature attached to a manifest. The signed payload
/// is the manifest serialized with the `signature` field elided (see
/// [`canonical_payload_bytes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSignature {
    /// Currently only `"ed25519"`.
    pub algorithm: String,
    /// 32-byte Ed25519 public key, hex-encoded (64 chars, lowercase).
    pub public_key_hex: String,
    /// 64-byte Ed25519 signature, hex-encoded (128 chars, lowercase).
    pub signature_hex: String,
}

/// On-disk schema for `extensions/<id>/manifest.json`.
///
/// The type-specific payload lives in dedicated structs (e.g.
/// [`AssetPackBody`]) and is materialised by the host crate that wires the
/// extension into the rest of the workspace. The manifest itself only
/// carries the type-agnostic metadata + the appropriate `body` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: ExtensionId,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub kind: ExtensionType,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub signature: Option<ExtensionSignature>,
    pub license: String,
    #[serde(default)]
    pub description: String,

    // Type-specific bodies. Exactly one must match the `kind` field; the
    // others must be `None`. The validator enforces this invariant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_pack: Option<AssetPackBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<TemplateBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_target: Option<ExportTargetBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_tool: Option<AiToolBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importer: Option<ImporterBody>,
}

// ---------------------------------------------------------------------------
// Type-specific bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetEntryKind {
    Furniture,
    Material,
    Preset,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetEntry {
    pub asset_id: String,
    pub name: String,
    pub kind: AssetEntryKind,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Path relative to the extension root (e.g. `furniture/sofa.glb`).
    pub source_path: PathBuf,
    /// BLAKE3 of the source file, hex-encoded. The host re-hashes on
    /// import to detect tampering between manifest signing and load.
    #[serde(default)]
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetPackBody {
    pub vendor: String,
    pub entries: Vec<AssetEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateBody {
    /// `<category>.<id>` key (e.g. `interior.boutique_hotel`).
    pub key: String,
    /// Path relative to the extension root pointing at the
    /// `TemplateDefinition` JSON document.
    pub definition_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleColumnDef {
    pub key: String,
    pub header: String,
    pub value_type: ScheduleValueType,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleValueType {
    String,
    Integer,
    Float,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleFormulaDef {
    pub name: String,
    /// Free-form expression understood by the schedule host (e.g.
    /// `"area_m2 * unit_cost"`). The host parses and evaluates it; the
    /// loader only checks that the string is non-empty.
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleBody {
    /// Stable schedule identifier (e.g. `"acme.fire_door_schedule"`).
    pub schedule_id: String,
    pub display_name: String,
    pub columns: Vec<ScheduleColumnDef>,
    #[serde(default)]
    pub formulas: Vec<ScheduleFormulaDef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Pdf,
    Xlsx,
    Zip,
    Ifc,
    Json,
    Glb,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportTargetBody {
    pub target_id: String,
    pub display_name: String,
    pub format: ExportFormat,
    /// Path inside the extension dir that the host invokes (typically a
    /// JSON descriptor or a script the host can interpret). The loader
    /// only verifies the file exists; semantics are host-specific.
    pub entry_path: PathBuf,
    #[serde(default)]
    pub default_extension: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiToolBody {
    pub tool_id: String,
    pub display_name: String,
    pub description: String,
    /// Allowed workflow scopes (`design`, `draft`, `bim`, `render`,
    /// `deliver`). Encoded as strings rather than [`crate::types::Scope`]
    /// to keep the manifest decoupled from the workspace enum order; the
    /// loader maps them to [`crate::types::Scope`] and stores the parsed
    /// list on [`LoadedExtension::scopes`].
    pub allowed_scopes: Vec<String>,
    pub max_entities_modified: u32,
    /// GBNF grammar key the AI runtime will load.
    pub grammar_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImporterBody {
    pub importer_id: String,
    pub display_name: String,
    /// File extensions handled (`["skp", "3ds"]`). Lower-case, no leading
    /// dot. The loader normalises and rejects empty entries.
    pub extensions: Vec<String>,
    pub entry_path: PathBuf,
}

// ---------------------------------------------------------------------------
// Loader / Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest parse error in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("manifest validation failed for {id}: {errors:?}")]
    Validation {
        id: String,
        errors: Vec<ManifestError>,
    },
    #[error("signature verification failed for {id}: {source}")]
    Signature {
        id: String,
        #[source]
        source: SignatureError,
    },
    #[error("duplicate extension id: {0}")]
    DuplicateId(ExtensionId),
    #[error("extension dir contains unsafe path component (..): {path}")]
    UnsafePath { path: PathBuf },
}

/// Stages of the extension boot pipeline at which a per-extension
/// failure can be surfaced to the host. The strings are stable wire
/// tags (the napi → IPC → renderer path serialises this verbatim) so
/// every variant doubles as a UI-facing label; renaming a variant is
/// a breaking change for the Settings diagnostics card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionLoadStage {
    /// `fs::read_to_string(manifest.json)` failed — usually a
    /// permissions issue or a vanished directory after the loader
    /// enumerated it.
    ManifestRead,
    /// `serde_json` rejected the manifest payload.
    ManifestParse,
    /// The manifest parsed but failed structural validation
    /// (unknown kind, missing required body, out-of-range numeric
    /// fields, etc.).
    ManifestValidation,
    /// A `body` payload path tried to escape the extension dir.
    UnsafePath,
    /// The signature block did not verify against the supplied trust
    /// store (or the embedded self-signed key when no store was
    /// provided).
    SignatureVerification,
    /// Two extensions on disk declared the same `id`. We keep the
    /// first one we saw — duplicate-id semantics match `registry.insert`.
    DuplicateId,
    /// An asset-pack extension parsed cleanly but the asset host
    /// rejected one of its entries (missing blob, blake3 mismatch,
    /// permission denied).
    AssetPackInstall,
    /// An ai-tool extension parsed cleanly but the AI tool resolver
    /// rejected it (unknown scope, missing body, permission denied).
    AiToolResolution,
}

impl ExtensionLoadStage {
    /// Stable wire tag used by the napi + IPC layers. Renaming this
    /// is a breaking change for any Settings UI that switches on it.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::ManifestRead => "manifest_read",
            Self::ManifestParse => "manifest_parse",
            Self::ManifestValidation => "manifest_validation",
            Self::UnsafePath => "unsafe_path",
            Self::SignatureVerification => "signature_verification",
            Self::DuplicateId => "duplicate_id",
            Self::AssetPackInstall => "asset_pack_install",
            Self::AiToolResolution => "ai_tool_resolution",
        }
    }
}

/// One per-extension boot failure. Captured by
/// [`ExtensionLoader::load_with_diagnostics`] (and emitted from the
/// `aec_bridge` boot path for asset-pack / ai-tool host failures) so
/// the renderer Settings UI can surface broken extensions instead of
/// silently dropping them.
///
/// Important: this struct is the wire format for the
/// `extensions:listLoadDiagnostics` IPC (renderer ↔ Electron) and the
/// `extension_load_diagnostics()` napi method (Electron ↔ Rust bridge).
/// Fields are stable; new variants of [`ExtensionLoadStage`] must come
/// with a backwards-compatible wire-string update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionLoadDiagnostic {
    /// Manifest `id` if we got far enough to parse it. `None` when
    /// the loader couldn't even read or parse the manifest.
    pub extension_id: Option<String>,
    /// Extension directory on disk (or the manifest file path when
    /// the failure happened before the dir was confirmed valid). This
    /// is surfaced as a relative-friendly hint in the renderer — the
    /// full path is intentional because asset packs can ship outside
    /// the install dir and the user needs to know which on-disk copy
    /// is broken.
    pub path: PathBuf,
    pub stage: ExtensionLoadStage,
    /// Human-readable error string, suitable for direct display in
    /// the Settings diagnostics card. Produced by `Display`-printing
    /// the underlying typed error so callers don't have to know the
    /// specific error enum.
    pub message: String,
}

impl ExtensionLoadDiagnostic {
    /// Convenience constructor that captures the stable wire string
    /// for the stage at the same time it captures the message.
    pub fn new(
        extension_id: Option<String>,
        path: impl Into<PathBuf>,
        stage: ExtensionLoadStage,
        message: impl Into<String>,
    ) -> Self {
        Self {
            extension_id,
            path: path.into(),
            stage,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Optional trust store. When set, only extensions signed by a known
    /// key are accepted; an unsigned or unknown-key manifest produces
    /// [`LoadError::Signature`].
    pub trust_store: Option<TrustStore>,
    /// If true, unsigned manifests are allowed but
    /// [`LoadedExtension::signed`] is `false`. The derived `Default` is
    /// `false` — i.e. `LoadOptions::default()` rejects unsigned manifests,
    /// matching the conservative production posture documented in
    /// `EXTENSIONS.md`. Local dev tooling that wants to load an unsigned
    /// directory should construct via [`Self::allow_unsigned`] instead.
    pub allow_unsigned: bool,
}

impl LoadOptions {
    pub fn allow_unsigned() -> Self {
        Self {
            trust_store: None,
            allow_unsigned: true,
        }
    }

    pub fn strict(trust: TrustStore) -> Self {
        Self {
            trust_store: Some(trust),
            allow_unsigned: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedExtension {
    pub manifest: ExtensionManifest,
    /// Resolved root of the extension on disk. Type-specific hosts read
    /// `body` payload paths relative to this directory.
    pub root: PathBuf,
    /// True if the signature was present *and* verified against the trust
    /// store. False for unsigned / dev-mode loads.
    pub signed: bool,
}

#[derive(Debug)]
pub struct ExtensionLoader {
    root: PathBuf,
}

impl ExtensionLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read every `<root>/<dir>/manifest.json`, validate it, optionally
    /// verify its signature, and return an [`ExtensionRegistry`].
    ///
    /// Strict: the first per-extension failure aborts the whole load
    /// and is returned as the typed [`LoadError`]. Use this from tests
    /// and dev tools that want the invariant that the extension
    /// directory is fully valid; production boot calls
    /// [`Self::load_with_diagnostics`] instead so a single broken
    /// extension doesn't take the runtime offline.
    pub fn load(&self, opts: &LoadOptions) -> Result<ExtensionRegistry, LoadError> {
        let mut registry = ExtensionRegistry::default();
        if !self.root.exists() {
            return Ok(registry);
        }
        for ext_dir in enumerate_extension_dirs(&self.root)? {
            match try_load_single_extension(&ext_dir, opts) {
                SingleLoadOutcome::Skipped => continue,
                SingleLoadOutcome::Loaded(loaded) => registry.insert(*loaded)?,
                SingleLoadOutcome::Failed { error, .. } => return Err(error),
            }
        }
        Ok(registry)
    }

    /// Fault-tolerant counterpart of [`Self::load`].
    ///
    /// Each per-extension failure is recorded in the returned
    /// [`ExtensionLoadDiagnostic`] vector instead of aborting the load.
    /// Healthy extensions still populate the returned
    /// [`ExtensionRegistry`]. This is what the `aec_bridge` boot path
    /// calls so a single broken extension doesn't take the entire
    /// runtime offline — the failures are then surfaced through the
    /// `extensions:listLoadDiagnostics` IPC for the Settings UI.
    ///
    /// The pre-iteration steps (root existence, directory enumeration)
    /// still return `Err` because they are not extension-scoped — if
    /// we can't even read `<root>/`, every diagnostic would be the
    /// same and the host should treat that as a hard error.
    pub fn load_with_diagnostics(
        &self,
        opts: &LoadOptions,
    ) -> Result<(ExtensionRegistry, Vec<ExtensionLoadDiagnostic>), LoadError> {
        let mut registry = ExtensionRegistry::default();
        let mut diagnostics: Vec<ExtensionLoadDiagnostic> = Vec::new();
        if !self.root.exists() {
            return Ok((registry, diagnostics));
        }
        for ext_dir in enumerate_extension_dirs(&self.root)? {
            match try_load_single_extension(&ext_dir, opts) {
                SingleLoadOutcome::Skipped => {}
                SingleLoadOutcome::Loaded(loaded) => {
                    let loaded = *loaded;
                    let id_str = loaded.manifest.id.0.clone();
                    if let Err(err) = registry.insert(loaded) {
                        // `insert` only fails on duplicate id today,
                        // but match exhaustively so future variants
                        // surface as diagnostics rather than being
                        // collapsed into the duplicate-id stage.
                        let stage = match &err {
                            LoadError::DuplicateId(_) => ExtensionLoadStage::DuplicateId,
                            _ => ExtensionLoadStage::ManifestValidation,
                        };
                        diagnostics.push(ExtensionLoadDiagnostic::new(
                            Some(id_str),
                            ext_dir,
                            stage,
                            err.to_string(),
                        ));
                    }
                }
                SingleLoadOutcome::Failed {
                    id,
                    path,
                    stage,
                    error,
                } => {
                    diagnostics.push(ExtensionLoadDiagnostic::new(
                        id,
                        path,
                        stage,
                        error.to_string(),
                    ));
                }
            }
        }
        Ok((registry, diagnostics))
    }
}

/// Enumerate every `<root>/<entry>` whose `file_type()` reports a
/// directory, sorted lexicographically so the per-extension iteration
/// order is deterministic across runs. Shared between
/// [`ExtensionLoader::load`] and [`ExtensionLoader::load_with_diagnostics`]
/// so the two paths cannot drift on what counts as an extension
/// candidate.
fn enumerate_extension_dirs(root: &Path) -> Result<Vec<PathBuf>, LoadError> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| LoadError::Io {
        path: root.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| LoadError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;
        let ft = entry.file_type().map_err(|e| LoadError::Io {
            path: entry.path(),
            source: e,
        })?;
        if ft.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

/// Internal result of attempting to load a single extension dir.
/// Lets the strict and tolerant entrypoints share the per-extension
/// pipeline (manifest read → parse → unsafe-path check → validation →
/// signature verification) without duplicating it.
///
/// `Loaded` is boxed so the `Failed`/`Skipped` paths don't pay the
/// full `LoadedExtension` payload on every call (`LoadedExtension`
/// carries the full deserialized manifest plus signing metadata,
/// which is large relative to the rest of the variants). This keeps
/// `clippy::large_enum_variant` happy without sacrificing the
/// per-extension fault-tolerance the type exists to enable.
enum SingleLoadOutcome {
    /// Directory did not contain a `manifest.json` — silently skipped
    /// to match the existing strict loader's behaviour.
    Skipped,
    /// Manifest read + parsed + validated + signed-check passed.
    Loaded(Box<LoadedExtension>),
    /// Per-extension typed failure. The tolerant entrypoint maps this
    /// straight into [`ExtensionLoadDiagnostic`]; the strict entrypoint
    /// discards `id`/`path`/`stage` and bubbles `error` as-is.
    Failed {
        /// Manifest `id` if the failure happened after the manifest
        /// was parsed; `None` for read/parse failures.
        id: Option<String>,
        /// Path to be reported in the diagnostic — the manifest file
        /// path for read/parse failures, the extension directory for
        /// downstream stages. Pre-computed in the helper so the
        /// tolerant entrypoint doesn't have to inspect `error`.
        path: PathBuf,
        /// Stable wire tag for the renderer Settings card.
        stage: ExtensionLoadStage,
        /// Typed [`LoadError`]; the strict entrypoint returns this
        /// directly, the tolerant entrypoint `Display`-formats it for
        /// `ExtensionLoadDiagnostic::message`.
        error: LoadError,
    },
}

/// Attempt to load a single extension directory. Errors are mapped to
/// the appropriate [`ExtensionLoadStage`] so the renderer Settings
/// card can surface a precise label for each failure mode; the strict
/// entrypoint ignores the stage tag and bubbles the typed error.
///
/// This is the single source of truth for the per-extension pipeline
/// shared by [`ExtensionLoader::load`] (strict) and
/// [`ExtensionLoader::load_with_diagnostics`] (tolerant). Any future
/// change to manifest validation, unsafe-path detection, or signature
/// verification only needs to land here.
fn try_load_single_extension(ext_dir: &Path, opts: &LoadOptions) -> SingleLoadOutcome {
    let manifest_path = ext_dir.join("manifest.json");
    if !manifest_path.is_file() {
        return SingleLoadOutcome::Skipped;
    }
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(e) => {
            return SingleLoadOutcome::Failed {
                id: None,
                path: manifest_path.clone(),
                stage: ExtensionLoadStage::ManifestRead,
                error: LoadError::Io {
                    path: manifest_path,
                    source: e,
                },
            };
        }
    };
    let manifest: ExtensionManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            return SingleLoadOutcome::Failed {
                id: None,
                path: manifest_path.clone(),
                stage: ExtensionLoadStage::ManifestParse,
                error: LoadError::Parse {
                    path: manifest_path,
                    source: e,
                },
            };
        }
    };

    // Reject relative-path escapes early. The signature-verified
    // payloads can still reference relative paths but they MUST
    // stay inside the extension dir.
    if path_escapes_root(ext_dir, &manifest) {
        return SingleLoadOutcome::Failed {
            id: Some(manifest.id.0.clone()),
            path: ext_dir.to_path_buf(),
            stage: ExtensionLoadStage::UnsafePath,
            error: LoadError::UnsafePath {
                path: ext_dir.to_path_buf(),
            },
        };
    }

    let validation_errors = validate_manifest(&manifest);
    if !validation_errors.is_empty() {
        let id = manifest.id.0.clone();
        return SingleLoadOutcome::Failed {
            id: Some(id.clone()),
            path: ext_dir.to_path_buf(),
            stage: ExtensionLoadStage::ManifestValidation,
            error: LoadError::Validation {
                id,
                errors: validation_errors,
            },
        };
    }

    let signed = match (&manifest.signature, &opts.trust_store) {
        (Some(sig), Some(trust)) => match verify_signature_against(&manifest, sig, trust) {
            Ok(()) => true,
            Err(e) => {
                return SingleLoadOutcome::Failed {
                    id: Some(manifest.id.0.clone()),
                    path: ext_dir.to_path_buf(),
                    stage: ExtensionLoadStage::SignatureVerification,
                    error: LoadError::Signature {
                        id: manifest.id.0.clone(),
                        source: e,
                    },
                };
            }
        },
        (Some(sig), None) => {
            // No trust store provided — still verify the signature is
            // *self-consistent* (i.e. signed by the embedded public
            // key). This catches accidental corruption and means an
            // extension that ships with `signature: {}` can't
            // masquerade as a verified one.
            let self_trust = match TrustStore::single_from_hex(&sig.public_key_hex) {
                Ok(ts) => ts,
                Err(e) => {
                    return SingleLoadOutcome::Failed {
                        id: Some(manifest.id.0.clone()),
                        path: ext_dir.to_path_buf(),
                        stage: ExtensionLoadStage::SignatureVerification,
                        error: LoadError::Signature {
                            id: manifest.id.0.clone(),
                            source: e,
                        },
                    };
                }
            };
            match verify_signature_against(&manifest, sig, &self_trust) {
                Ok(()) => false,
                Err(e) => {
                    return SingleLoadOutcome::Failed {
                        id: Some(manifest.id.0.clone()),
                        path: ext_dir.to_path_buf(),
                        stage: ExtensionLoadStage::SignatureVerification,
                        error: LoadError::Signature {
                            id: manifest.id.0.clone(),
                            source: e,
                        },
                    };
                }
            }
        }
        (None, Some(_) | None) => {
            if !opts.allow_unsigned {
                return SingleLoadOutcome::Failed {
                    id: Some(manifest.id.0.clone()),
                    path: ext_dir.to_path_buf(),
                    stage: ExtensionLoadStage::SignatureVerification,
                    error: LoadError::Signature {
                        id: manifest.id.0.clone(),
                        source: SignatureError::MissingSignature,
                    },
                };
            }
            false
        }
    };

    SingleLoadOutcome::Loaded(Box::new(LoadedExtension {
        manifest,
        root: ext_dir.to_path_buf(),
        signed,
    }))
}

fn path_escapes_root(_root: &Path, manifest: &ExtensionManifest) -> bool {
    fn bad(p: &Path) -> bool {
        p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    }
    let body_paths: Vec<&PathBuf> = [
        manifest.template.as_ref().map(|t| &t.definition_path),
        manifest.export_target.as_ref().map(|e| &e.entry_path),
        manifest.importer.as_ref().map(|i| &i.entry_path),
    ]
    .into_iter()
    .flatten()
    .collect();
    if body_paths.iter().any(|p| bad(p)) {
        return true;
    }
    if let Some(ap) = &manifest.asset_pack {
        if ap.entries.iter().any(|e| bad(&e.source_path)) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Canonical payload (used by signing and verification)
// ---------------------------------------------------------------------------

/// Produce the bytes that an extension publisher signs. The bytes are the
/// manifest serialized to canonical JSON (sorted keys, no whitespace) with
/// the `signature` field replaced by `null`. Anyone with the public key
/// can deterministically reproduce these bytes from the on-disk manifest.
pub fn canonical_payload_bytes(manifest: &ExtensionManifest) -> Vec<u8> {
    let mut clone = manifest.clone();
    clone.signature = None;
    let value = serde_json::to_value(&clone).expect("manifest is always JSON-serialisable");
    canonical_json(&value)
}

fn canonical_json(v: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(&mut out, v);
    out
}

fn write_canonical(buf: &mut Vec<u8>, v: &serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Null => buf.extend_from_slice(b"null"),
        Value::Bool(b) => buf.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => buf.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => {
            // serde_json's `to_string` for a string Value escapes
            // consistently — reuse it.
            let s = serde_json::to_string(s).expect("string is always JSON-serialisable");
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Array(items) => {
            buf.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                write_canonical(buf, item);
            }
            buf.push(b']');
        }
        Value::Object(map) => {
            // BTreeMap preserves key order so iteration is deterministic
            // regardless of how the input was deserialised.
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            buf.push(b'{');
            for (i, (k, val)) in sorted.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                let key_str = serde_json::to_string(k).expect("string key");
                buf.extend_from_slice(key_str.as_bytes());
                buf.push(b':');
                write_canonical(buf, val);
            }
            buf.push(b'}');
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ExtensionRegistry {
    by_id: BTreeMap<ExtensionId, LoadedExtension>,
}

impl ExtensionRegistry {
    pub fn insert(&mut self, ext: LoadedExtension) -> Result<(), LoadError> {
        if self.by_id.contains_key(&ext.manifest.id) {
            return Err(LoadError::DuplicateId(ext.manifest.id.clone()));
        }
        self.by_id.insert(ext.manifest.id.clone(), ext);
        Ok(())
    }

    pub fn get(&self, id: &ExtensionId) -> Option<&LoadedExtension> {
        self.by_id.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &LoadedExtension> {
        self.by_id.values()
    }

    pub fn by_kind(&self, kind: ExtensionType) -> impl Iterator<Item = &LoadedExtension> {
        self.by_id.values().filter(move |e| e.manifest.kind == kind)
    }

    /// Look up a `Template` extension whose body declares `key`.
    /// Returns the first match in id-sorted order (registry is a
    /// `BTreeMap`), which keeps lookups deterministic when two
    /// extensions collide on a key. Hosts that need conflict
    /// detection can iterate `by_kind` themselves.
    pub fn find_template(&self, key: &str) -> Option<&LoadedExtension> {
        self.by_kind(ExtensionType::Template)
            .find(|e| e.manifest.template.as_ref().is_some_and(|b| b.key == key))
    }

    /// Look up a `Schedule` extension by id. Schedule bodies don't
    /// carry a separate key, so the manifest id IS the lookup key.
    pub fn find_schedule(&self, ext_id: &ExtensionId) -> Option<&LoadedExtension> {
        self.get(ext_id)
            .filter(|e| e.manifest.kind == ExtensionType::Schedule)
    }

    /// Look up an `ExportTarget` extension by the body's `target_id`.
    pub fn find_export_target(&self, target_id: &str) -> Option<&LoadedExtension> {
        self.by_kind(ExtensionType::ExportTarget).find(|e| {
            e.manifest
                .export_target
                .as_ref()
                .is_some_and(|b| b.target_id == target_id)
        })
    }

    /// Look up an `AiTool` extension by the body's `tool_id`.
    pub fn find_ai_tool(&self, tool_id: &str) -> Option<&LoadedExtension> {
        self.by_kind(ExtensionType::AiTool).find(|e| {
            e.manifest
                .ai_tool
                .as_ref()
                .is_some_and(|b| b.tool_id == tool_id)
        })
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_permissions::keygen_test_only;
    use std::fs;

    fn write_manifest(root: &Path, id: &str, manifest: &ExtensionManifest) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        let raw = serde_json::to_string_pretty(manifest).unwrap();
        fs::write(&path, raw).unwrap();
        path
    }

    fn asset_pack_manifest(id: &str) -> ExtensionManifest {
        ExtensionManifest {
            id: ExtensionId(id.to_string()),
            name: format!("{id} pack"),
            version: "1.0.0".into(),
            kind: ExtensionType::AssetPack,
            permissions: vec![Permission::FilesystemRead, Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: "test pack".into(),
            asset_pack: Some(AssetPackBody {
                vendor: "ACME".into(),
                entries: vec![AssetEntry {
                    asset_id: "sofa-001".into(),
                    name: "Sofa".into(),
                    kind: AssetEntryKind::Furniture,
                    tags: vec!["sofa".into(), "living-room".into()],
                    source_path: PathBuf::from("furniture/sofa.glb"),
                    blake3: String::new(),
                }],
            }),
            template: None,
            schedule: None,
            export_target: None,
            ai_tool: None,
            importer: None,
        }
    }

    #[test]
    fn serde_roundtrip_preserves_every_field() {
        let m = asset_pack_manifest("acme.sofa");
        let raw = serde_json::to_string(&m).unwrap();
        let m2: ExtensionManifest = serde_json::from_str(&raw).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn loader_skips_directories_without_manifest_json() {
        let td = tempfile::tempdir().unwrap();
        // Empty dir — no manifest, must NOT fail.
        fs::create_dir_all(td.path().join("empty-pack")).unwrap();
        let reg = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn loader_reads_and_indexes_two_packs() {
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "a", &asset_pack_manifest("a"));
        write_manifest(td.path(), "b", &asset_pack_manifest("b"));
        let reg = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.by_kind(ExtensionType::AssetPack).count(), 2);
        assert!(reg.get(&ExtensionId("a".into())).is_some());
    }

    #[test]
    fn loader_rejects_invalid_permission_via_validator() {
        // A manifest that asks for a permission the validator can't see
        // can't actually be constructed via the enum, so prove the
        // *validator* runs from the loader by tampering at the JSON layer.
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("bad-perm");
        fs::create_dir_all(&dir).unwrap();
        let raw = r#"{
            "id": "bad",
            "name": "Bad",
            "version": "1.0.0",
            "type": "asset_pack",
            "permissions": ["world_domination"],
            "license": "AGPL-3.0",
            "asset_pack": {"vendor":"x","entries":[]}
        }"#;
        fs::write(dir.join("manifest.json"), raw).unwrap();
        let err = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap_err();
        assert!(matches!(err, LoadError::Parse { .. }));
    }

    #[test]
    fn loader_rejects_unsafe_paths() {
        let td = tempfile::tempdir().unwrap();
        let mut m = asset_pack_manifest("escape");
        if let Some(ap) = m.asset_pack.as_mut() {
            ap.entries[0].source_path = PathBuf::from("../../etc/passwd");
        }
        write_manifest(td.path(), "escape", &m);
        let err = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap_err();
        assert!(matches!(err, LoadError::UnsafePath { .. }));
    }

    #[test]
    fn loader_rejects_duplicate_ids() {
        // Two directories holding manifests with the same `id` are
        // structurally impossible (loader iterates dirs), so simulate by
        // inserting into the registry directly.
        let mut reg = ExtensionRegistry::default();
        let a = LoadedExtension {
            manifest: asset_pack_manifest("dup"),
            root: PathBuf::from("/a"),
            signed: false,
        };
        let b = LoadedExtension {
            manifest: asset_pack_manifest("dup"),
            root: PathBuf::from("/b"),
            signed: false,
        };
        reg.insert(a).unwrap();
        let err = reg.insert(b).unwrap_err();
        assert!(matches!(err, LoadError::DuplicateId(_)));
    }

    #[test]
    fn signed_manifest_round_trips_through_loader_with_trust_store() {
        let (sk, pk_hex) = keygen_test_only();
        let mut m = asset_pack_manifest("signed");

        // Sign with the freshly minted key.
        use ed25519_dalek::Signer;
        let payload = canonical_payload_bytes(&m);
        let sig = sk.sign(&payload);
        m.signature = Some(ExtensionSignature {
            algorithm: "ed25519".into(),
            public_key_hex: pk_hex.clone(),
            signature_hex: hex::encode(sig.to_bytes()),
        });

        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "signed", &m);

        let trust = TrustStore::single_from_hex(&pk_hex).unwrap();
        let opts = LoadOptions {
            trust_store: Some(trust),
            allow_unsigned: false,
        };
        let reg = ExtensionLoader::new(td.path()).load(&opts).unwrap();
        let loaded = reg.get(&ExtensionId("signed".into())).unwrap();
        assert!(loaded.signed, "trusted signature must be marked signed");
    }

    #[test]
    fn unsigned_manifest_loads_but_signed_flag_is_false() {
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "unsigned", &asset_pack_manifest("unsigned"));
        let reg = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let loaded = reg.get(&ExtensionId("unsigned".into())).unwrap();
        assert!(!loaded.signed);
    }

    #[test]
    fn load_with_diagnostics_returns_empty_for_clean_registry() {
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "a", &asset_pack_manifest("a"));
        write_manifest(td.path(), "b", &asset_pack_manifest("b"));
        let (reg, diags) = ExtensionLoader::new(td.path())
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();
        assert_eq!(reg.len(), 2);
        assert!(diags.is_empty());
    }

    #[test]
    fn load_with_diagnostics_captures_unparseable_manifest_and_keeps_healthy_one() {
        let td = tempfile::tempdir().unwrap();
        // Healthy extension.
        write_manifest(td.path(), "ok", &asset_pack_manifest("ok"));
        // Broken extension — manifest.json that isn't JSON.
        let broken_dir = td.path().join("broken");
        fs::create_dir_all(&broken_dir).unwrap();
        fs::write(broken_dir.join("manifest.json"), "{ not valid json").unwrap();

        let (reg, diags) = ExtensionLoader::new(td.path())
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();
        assert_eq!(reg.len(), 1, "healthy extension must still load");
        assert!(reg.get(&ExtensionId("ok".into())).is_some());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::ManifestParse);
        assert!(diags[0].extension_id.is_none(), "id is unknown pre-parse");
        assert!(diags[0].path.ends_with("broken/manifest.json"));
        assert!(!diags[0].message.is_empty());
    }

    #[test]
    fn load_with_diagnostics_captures_validation_failure() {
        let td = tempfile::tempdir().unwrap();
        // Validation-rejected manifest: world_domination is not a real
        // permission and serde fails at parse-time, so target a
        // post-parse validation rejection by zeroing required body.
        let dir = td.path().join("invalid");
        fs::create_dir_all(&dir).unwrap();
        let raw = r#"{
            "id": "invalid",
            "name": "Invalid",
            "version": "1.0.0",
            "type": "asset_pack",
            "permissions": [],
            "license": "AGPL-3.0",
            "asset_pack": null
        }"#;
        fs::write(dir.join("manifest.json"), raw).unwrap();

        let (reg, diags) = ExtensionLoader::new(td.path())
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();
        assert_eq!(reg.len(), 0);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::ManifestValidation);
        assert_eq!(diags[0].extension_id.as_deref(), Some("invalid"));
    }

    #[test]
    fn load_with_diagnostics_captures_unsafe_path() {
        let td = tempfile::tempdir().unwrap();
        let mut m = asset_pack_manifest("escape");
        if let Some(ap) = m.asset_pack.as_mut() {
            ap.entries[0].source_path = PathBuf::from("../../etc/passwd");
        }
        write_manifest(td.path(), "escape", &m);

        let (reg, diags) = ExtensionLoader::new(td.path())
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();
        assert_eq!(reg.len(), 0);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::UnsafePath);
        assert_eq!(diags[0].extension_id.as_deref(), Some("escape"));
    }

    #[test]
    fn load_with_diagnostics_captures_missing_signature_when_required() {
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "unsigned", &asset_pack_manifest("unsigned"));
        let (_pk_sk, pk_hex) = keygen_test_only();
        let trust = TrustStore::single_from_hex(&pk_hex).unwrap();
        let opts = LoadOptions {
            trust_store: Some(trust),
            allow_unsigned: false,
        };

        let (reg, diags) = ExtensionLoader::new(td.path())
            .load_with_diagnostics(&opts)
            .unwrap();
        assert_eq!(reg.len(), 0);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::SignatureVerification);
        assert_eq!(diags[0].extension_id.as_deref(), Some("unsigned"));
    }

    #[test]
    fn load_with_diagnostics_wire_strings_are_stable() {
        // Pin the wire tags — renaming any of them is a breaking
        // change for the renderer Settings card.
        assert_eq!(
            ExtensionLoadStage::ManifestRead.as_wire_str(),
            "manifest_read"
        );
        assert_eq!(
            ExtensionLoadStage::ManifestParse.as_wire_str(),
            "manifest_parse"
        );
        assert_eq!(
            ExtensionLoadStage::ManifestValidation.as_wire_str(),
            "manifest_validation"
        );
        assert_eq!(ExtensionLoadStage::UnsafePath.as_wire_str(), "unsafe_path");
        assert_eq!(
            ExtensionLoadStage::SignatureVerification.as_wire_str(),
            "signature_verification"
        );
        assert_eq!(
            ExtensionLoadStage::DuplicateId.as_wire_str(),
            "duplicate_id"
        );
        assert_eq!(
            ExtensionLoadStage::AssetPackInstall.as_wire_str(),
            "asset_pack_install"
        );
        assert_eq!(
            ExtensionLoadStage::AiToolResolution.as_wire_str(),
            "ai_tool_resolution"
        );
    }

    #[test]
    fn canonical_payload_is_stable_across_field_order() {
        // Build two equivalent manifests where the JSON differs only in
        // key order; canonical_payload_bytes must produce identical
        // bytes.
        let m = asset_pack_manifest("stable");
        let bytes_a = canonical_payload_bytes(&m);

        let raw = serde_json::to_string(&m).unwrap();
        let parsed: ExtensionManifest = serde_json::from_str(&raw).unwrap();
        let bytes_b = canonical_payload_bytes(&parsed);
        assert_eq!(bytes_a, bytes_b);
    }

    // ---------------------------------------------------------------
    // Shared-helper contract: pin that the strict (`load`) and
    // tolerant (`load_with_diagnostics`) entrypoints agree on every
    // observable per-extension outcome. Both call into the same
    // `try_load_single_extension` helper; these tests fail if a
    // future change drifts one path relative to the other.

    #[test]
    fn shared_helper_strict_and_tolerant_agree_on_happy_path() {
        // Two clean extensions: strict returns Ok(registry of 2),
        // tolerant returns the same registry + empty diagnostics, and
        // the resolved LoadedExtension records match byte-for-byte
        // (id, root, signed flag).
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "a", &asset_pack_manifest("a"));
        write_manifest(td.path(), "b", &asset_pack_manifest("b"));

        let loader = ExtensionLoader::new(td.path());
        let strict = loader.load(&LoadOptions::allow_unsigned()).unwrap();
        let (tolerant, diags) = loader
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();

        assert_eq!(strict.len(), 2);
        assert_eq!(tolerant.len(), 2);
        assert!(
            diags.is_empty(),
            "tolerant must not emit diagnostics for clean fixture"
        );

        for id in ["a", "b"] {
            let ext_id = ExtensionId(id.into());
            let s = strict.get(&ext_id).expect("strict missing id");
            let t = tolerant.get(&ext_id).expect("tolerant missing id");
            assert_eq!(s.manifest, t.manifest, "{id}: manifest mismatch");
            assert_eq!(s.root, t.root, "{id}: root mismatch");
            assert_eq!(s.signed, t.signed, "{id}: signed flag mismatch");
        }
    }

    #[test]
    fn shared_helper_unsafe_path_strict_returns_typed_error_tolerant_records_matching_stage() {
        // Same fixture run through both entrypoints must produce:
        //   strict   → Err(LoadError::UnsafePath { path: ext_dir })
        //   tolerant → diagnostic { stage: UnsafePath, path: ext_dir,
        //              message: <Display of the same typed error> }
        let td = tempfile::tempdir().unwrap();
        let mut m = asset_pack_manifest("escape");
        if let Some(ap) = m.asset_pack.as_mut() {
            ap.entries[0].source_path = PathBuf::from("../../etc/passwd");
        }
        write_manifest(td.path(), "escape", &m);

        let loader = ExtensionLoader::new(td.path());
        let strict_err = loader.load(&LoadOptions::allow_unsigned()).unwrap_err();
        let (_reg, diags) = loader
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();

        let strict_path = match &strict_err {
            LoadError::UnsafePath { path } => path.clone(),
            other => panic!("expected UnsafePath, got {other:?}"),
        };
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::UnsafePath);
        assert_eq!(
            diags[0].extension_id.as_deref(),
            Some("escape"),
            "tolerant path carries manifest id once parse succeeded"
        );
        assert_eq!(diags[0].path, strict_path);
        assert_eq!(diags[0].message, strict_err.to_string());
    }

    #[test]
    fn shared_helper_validation_strict_returns_typed_error_tolerant_records_matching_stage() {
        // Build a manifest that parses but fails structural validation
        // by clearing the type-specific body (manifest declares
        // `type: asset_pack` but `asset_pack: null` — the validator
        // rejects this as a body/kind mismatch).
        let td = tempfile::tempdir().unwrap();
        let mut m = asset_pack_manifest("invalid");
        m.asset_pack = None;
        write_manifest(td.path(), "invalid", &m);

        let loader = ExtensionLoader::new(td.path());
        let strict_err = loader.load(&LoadOptions::allow_unsigned()).unwrap_err();
        let (_reg, diags) = loader
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();

        assert!(
            matches!(strict_err, LoadError::Validation { .. }),
            "expected Validation, got {strict_err:?}"
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::ManifestValidation);
        assert_eq!(diags[0].extension_id.as_deref(), Some("invalid"));
        assert_eq!(diags[0].message, strict_err.to_string());
    }

    #[test]
    fn shared_helper_manifest_parse_strict_returns_typed_error_tolerant_records_matching_stage() {
        // Broken JSON: strict path produces LoadError::Parse, tolerant
        // path produces a diagnostic with the same message at the
        // ManifestParse stage.
        let td = tempfile::tempdir().unwrap();
        let broken_dir = td.path().join("broken");
        fs::create_dir_all(&broken_dir).unwrap();
        fs::write(broken_dir.join("manifest.json"), "{ not valid json").unwrap();

        let loader = ExtensionLoader::new(td.path());
        let strict_err = loader.load(&LoadOptions::allow_unsigned()).unwrap_err();
        let (_reg, diags) = loader
            .load_with_diagnostics(&LoadOptions::allow_unsigned())
            .unwrap();

        let strict_path = match &strict_err {
            LoadError::Parse { path, .. } => path.clone(),
            other => panic!("expected Parse, got {other:?}"),
        };
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::ManifestParse);
        assert!(
            diags[0].extension_id.is_none(),
            "id is unknown before parse"
        );
        assert_eq!(diags[0].path, strict_path);
        assert_eq!(diags[0].message, strict_err.to_string());
    }

    #[test]
    fn shared_helper_missing_signature_strict_returns_typed_error_tolerant_records_matching_stage()
    {
        // With a trust store and allow_unsigned=false, an unsigned
        // manifest is rejected at the signature stage on both paths.
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "unsigned", &asset_pack_manifest("unsigned"));
        let (_sk, pk_hex) = keygen_test_only();
        let trust = TrustStore::single_from_hex(&pk_hex).unwrap();
        let opts = LoadOptions {
            trust_store: Some(trust),
            allow_unsigned: false,
        };

        let loader = ExtensionLoader::new(td.path());
        let strict_err = loader.load(&opts).unwrap_err();
        let (_reg, diags) = loader.load_with_diagnostics(&opts).unwrap();

        assert!(
            matches!(strict_err, LoadError::Signature { .. }),
            "expected Signature, got {strict_err:?}"
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].stage, ExtensionLoadStage::SignatureVerification);
        assert_eq!(diags[0].extension_id.as_deref(), Some("unsigned"));
        assert_eq!(diags[0].message, strict_err.to_string());
    }
}
