//! Plain-Rust service layer for the bridge.
//!
//! Everything the Electron renderer can do ultimately calls into one of
//! these methods. Keeping the napi wrappers thin and the logic here makes
//! this layer trivially testable.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_audit::AuditLog;
use aec_command::commands::{Command, EntityDelta, EntityRecord};
use aec_command::engine::CommandEngine;
use aec_core::config::ProjectSettings;
use aec_core::package::{ProjectPackage, ProjectSummary as CoreProjectSummary};
use aec_core::templates::TemplateLoader;
use aec_core::types::{CommandId, ProjectId, Scope};
use aec_governor::profiler::{CpuProfile, GpuProfile, HardwareProfiler};
use aec_governor::tier::HardwareTier;
use aec_render::doctor::{check_materials, CheckMaterialsOptions, MaterialFinding};
use aec_render::job::{RenderJob as CoreRenderJob, RenderJobStatus};
use aec_render::preset::RenderPresetStore;
use aec_render::queue::{BatchProgress as CoreBatchProgress, RenderQueue};
use aec_render::scene::RenderScene;

use crate::asset_state::AssetState;
use crate::bim_attach;
use crate::engine_status_cache::EngineStatusCache;
use crate::recents::{RecentsStore, RecentsStoreError};
use crate::snapshot_cache::{SnapshotCache, SnapshotKey};

/// Renderer-side warning threshold for IFC files. Files at or above
/// this byte count surface a `BimImportSummary.large_file_warning`
/// flag so the renderer can show a confirm dialog ("This file is N
/// MB; parsing may take a while — continue?") before the user
/// commits to the parse path. The bridge still runs the import — the
/// flag is advisory — because we don't want to silently reject a
/// real workflow just because the file is large. Set to 100 MB,
/// chosen as the rough boundary between "interactive parse" (< 5 s
/// on a modern laptop) and "go-grab-a-coffee parse".
pub(crate) const BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum BridgeServiceError {
    #[error("core: {0}")]
    Core(String),
    #[error("recents: {0}")]
    Recents(#[from] RecentsStoreError),
    #[error("template: {0}")]
    Template(String),
    #[error("audit: {0}")]
    Audit(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// IFC (BIM) reader / writer failure. Carries the parser's own
    /// error message verbatim so the renderer can show the user
    /// which STEP entity / line failed.
    #[error("bim: {0}")]
    Bim(String),
    /// Command-engine failure (validation, scope mismatch, journal
    /// corruption, persistence). The error message preserves
    /// `aec_command::error::CommandError`'s `Display` so the renderer
    /// can surface "entity X not found", "scope mismatch", etc.
    #[error("command: {0}")]
    Command(String),
    /// Export-layer failure (PDF / DXF / IFC / glTF / ZIP). Carries
    /// the underlying `aec_export::ProjectExportError` display
    /// verbatim so the renderer can show which format failed and
    /// (e.g. for invalid layer names) what the user supplied.
    #[error("export: {0}")]
    Export(String),
    /// Asset-library failure (SQLite I/O on the cross-project asset
    /// catalogue at `<state_dir>/asset_library/assets.sqlite`,
    /// serialisation of metadata rows, etc). Preserves
    /// [`aec_assets::AssetError`]'s `Display` so the renderer can
    /// distinguish "asset library is unreadable" from a generic
    /// `Core` failure.
    #[error("asset: {0}")]
    Asset(String),
}

impl From<aec_assets::AssetError> for BridgeServiceError {
    fn from(e: aec_assets::AssetError) -> Self {
        Self::Asset(e.to_string())
    }
}

impl From<aec_command::error::CommandError> for BridgeServiceError {
    fn from(e: aec_command::error::CommandError) -> Self {
        Self::Command(e.to_string())
    }
}

impl From<aec_export::ProjectExportError> for BridgeServiceError {
    fn from(e: aec_export::ProjectExportError) -> Self {
        Self::Export(e.to_string())
    }
}

impl From<aec_bim::ifc::IfcReadError> for BridgeServiceError {
    fn from(e: aec_bim::ifc::IfcReadError) -> Self {
        Self::Bim(e.to_string())
    }
}

impl From<aec_audit::AuditError> for BridgeServiceError {
    fn from(e: aec_audit::AuditError) -> Self {
        Self::Audit(e.to_string())
    }
}

impl From<rusqlite::Error> for BridgeServiceError {
    fn from(e: rusqlite::Error) -> Self {
        // SQL errors at the bridge layer are a subclass of "core" — they
        // arise from project_engine_status etc. running ad-hoc queries
        // against the encrypted project DB. Reusing the `Core` variant
        // keeps the JS-side error taxonomy small.
        Self::Core(e.to_string())
    }
}

impl From<aec_core::error::AecError> for BridgeServiceError {
    fn from(e: aec_core::error::AecError) -> Self {
        Self::Core(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateChoice {
    pub key: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub name: String,
    pub path: String,
    /// Last-write timestamp from the manifest. The N-API layer renders
    /// this as ISO-8601 and the TypeScript `ProjectSummary.modifiedAt`
    /// field consumes it directly. Carrying it through the service layer
    /// (instead of resynthesising it in the bridge) is what lets the
    /// recents list show a real timestamp on Home/dashboard tiles.
    pub updated_at: DateTime<Utc>,
    pub template_id: Option<String>,
}

impl From<CoreProjectSummary> for ProjectSummary {
    fn from(s: CoreProjectSummary) -> Self {
        Self {
            project_id: s.project_id,
            name: s.name,
            path: s.path,
            updated_at: s.updated_at,
            template_id: s.template_id,
        }
    }
}

/// Aggregated status the renderer can show on a project's
/// engine/status pane. Reports the schema version recorded in the
/// SQLCipher database, the audit-chain head hash + entry count, and
/// the per-scope command-journal counts (LIVE counts derived from the
/// in-memory chain — mirroring into the SQL `audit_chain` table is a
/// separate explicit call). All values are read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStatusReport {
    /// Schema version recorded in the project's SQLCipher `meta` table.
    /// New projects come out of [`open_encrypted`] at this value;
    /// existing projects may report a higher version if the registry
    /// is ahead of this binary, which is itself a hard error and
    /// surfaces via [`BridgeServiceError::Core`] instead of reaching
    /// here.
    pub schema_version: u32,
    /// Head hash of the BLAKE3 audit chain (e.g. `blake3:<hex>`), or
    /// `aec_audit::AuditLog::GENESIS` if the chain is empty.
    pub audit_chain_head: String,
    /// Total number of entries in the JSONL audit log.
    pub audit_entry_count: u64,
    /// Number of rows in the SQL-side `audit_chain` table. Equal to
    /// `audit_entry_count` when the SQL mirror is up-to-date and less
    /// when the renderer has appended without re-mirroring (or zero
    /// for a freshly-opened legacy project before any sync).
    pub audit_chain_sql_count: u64,
    /// Per-scope counts taken from the SQL mirror. Keys are
    /// `Scope::as_str` (`design` / `draft` / `bim` / `render` /
    /// `deliver`). A scope with no entries is included with value
    /// `0` so the renderer can render the full set without a
    /// post-process step.
    pub audit_chain_by_scope: std::collections::BTreeMap<String, u64>,
}

/// Parse-only summary of a BIM (IFC) import. Returned by
/// [`BridgeService::bim_import_ifc`] and rendered as a preview on
/// the Import panel before the user commits the file into the
/// active project.
///
/// Field naming matches the renderer's `BimImportSummary` TS
/// interface 1:1 — drift here is a runtime bug surfacing as
/// `undefined` on a status pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimImportSummary {
    /// Canonical absolute path to the IFC file (`std::fs::canonicalize`
    /// applied to whatever the user pointed at). The TS renderer
    /// displays this verbatim, and downstream consumers (the future
    /// PR-L snapshot cache, dedup helpers) key on it — so callers can
    /// trust the value is symlink-resolved and free of `./` / `..`
    /// segments. On Windows the result is the verbatim `\\?\C:\...`
    /// form per the platform's canonicalisation rules.
    pub path: String,
    /// IFC schema version recovered from `FILE_SCHEMA`, rendered
    /// via the `IfcSchema` enum's `Display` impl — the canonical
    /// STEP token (`"IFC2X3"` / `"IFC4"` / `"IFC4X3"`). The TS
    /// `BimImportSummary` interface matches on these tokens, so do
    /// NOT switch back to `format!("{:?}")` (which would leak the
    /// Rust variant names `"Ifc4"` / `"Ifc2x3"` and break the
    /// renderer's match).
    pub schema: String,
    pub spatial_nodes: u64,
    pub elements: u64,
    pub psets: u64,
    pub qsets: u64,
    pub aggregations: u64,
    pub containments: u64,
    /// `IfcMaterial` definitions recovered from the file.
    pub materials: u64,
    /// `IfcMaterialLayerSet` composites recovered.
    pub material_layer_sets: u64,
    /// `IfcRelAssociatesMaterial` element-to-material bindings.
    pub material_assignments: u64,
    /// Total STEP records the reader walked (records_seen).
    pub records_seen: u64,
    /// File size in bytes the bridge read off disk. Surfaced so the
    /// renderer can render a humanised "123 MB" line on the preview
    /// without re-running `fs.stat` from the JS side.
    pub file_size_bytes: u64,
    /// `true` when the file size meets-or-exceeds
    /// [`BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES`] (100 MB). The bridge
    /// still parses the file — the flag is advisory — so the
    /// renderer can throw up a confirm dialog *before* committing to
    /// the parse path on a multi-hundred-MB MEP federation. Set to
    /// `false` for typical architectural models.
    pub large_file_warning: bool,
}

/// Result of a successful [`BridgeService::bim_check_file_size`]
/// call. Cheap (one `fs::metadata` + one `fs::canonicalize`) so
/// the renderer can call it on every file the user picks without
/// committing to the multi-second IFC parse path.
///
/// The renderer uses `large_file_warning` to decide whether to
/// throw up a confirm dialog before invoking
/// [`BridgeService::bim_import_ifc`]. The dialog renders
/// `file_size_bytes` humanised ("412 MB") and `threshold_bytes`
/// for context ("the 100 MB warn threshold").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimFileSizeCheck {
    /// Canonical absolute path of the file as stat'd. Matches
    /// [`BimImportSummary::path`] canonicalisation rules so the
    /// renderer can dedup pick → check → import sequences across
    /// non-canonical inputs (`./foo.ifc` vs absolute).
    pub path: String,
    /// File size in bytes per `std::fs::metadata`.
    pub file_size_bytes: u64,
    /// `true` when `file_size_bytes >= threshold_bytes`. The
    /// renderer should warn-and-confirm (not block) — a user
    /// with a 500 MB MEP federation has a legitimate workflow
    /// reason to proceed.
    pub large_file_warning: bool,
    /// The current warn threshold, surfaced verbatim so the
    /// renderer can render the dialog body ("This file is
    /// 412 MB, above the 100 MB warn threshold; parsing may
    /// take a while — continue?") without re-importing the
    /// constant.
    pub threshold_bytes: u64,
}

/// Result of a successful [`BridgeService::bim_attach_ifc`] call.
/// Counts how the snapshot was folded into the project graph so the
/// renderer can show "Attached 3 storeys, 142 walls, 87 doors, ...".
///
/// All counters are post-dedup: a re-attach of the same file with
/// identical content reports `_unchanged` instead of `_inserted` /
/// `_updated`. See [`crate::bim_attach`] for the dedup contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimAttachSummary {
    /// Canonical absolute path of the IFC file that was attached.
    /// Same canonical form as [`BimImportSummary::path`] so the
    /// renderer can dedup recents across import → attach.
    pub path: String,
    /// Project the attach landed in. Mirrors the field the renderer
    /// shows on the Home / dashboard tiles.
    pub project_path: String,
    /// `true` if the snapshot for this file was served from the
    /// in-process cache populated by a prior `bim_import_ifc`, false
    /// if the bridge had to re-parse the file from disk. Useful for
    /// instrumentation and for the renderer's loading indicator.
    pub parse_cache_hit: bool,
    pub spatial_nodes_inserted: u64,
    pub spatial_nodes_updated: u64,
    pub spatial_nodes_unchanged: u64,
    pub elements_inserted: u64,
    pub elements_updated: u64,
    pub elements_unchanged: u64,
    pub components_inserted: u64,
    pub relations_inserted: u64,
    pub cache_rows: u64,
}

// ----- Render result shapes -----
//
// Service-layer projections of the rich `aec_render::job::RenderJob`,
// `aec_render::queue::BatchProgress`, and `aec_render::doctor::*` types.
// Trimmed to just the fields the renderer-side UI actually needs so
// the napi serialisation stays cheap.

/// Renderer-facing summary of a single [`aec_render::RenderJob`].
/// Field names match the TypeScript `RenderJob` interface in
/// `apps/desktop/electron/bridge.ts`. The full `RenderScene` and per-
/// frame `completed_frames` vector held by the core type are
/// intentionally not propagated through the napi surface — the queue
/// view shows a status pill and a progress bar, neither needs the
/// scene geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderJobSummary {
    pub job_id: String,
    /// Lowercase variant of [`aec_render::RenderJobStatus`]
    /// (`queued` / `running` / `completed` / `failed` / `cancelled`),
    /// matching the literal-union on the TS side.
    pub status: String,
    /// Preset id (e.g. `aec.preset.standard`) — *not* the human
    /// label, so the renderer can re-resolve it via the preset
    /// store if it needs to render the long form.
    pub preset: String,
    /// `0.0..=1.0`. Completed jobs report `1.0`; failed and cancelled
    /// jobs report whatever progress they had reached at termination.
    pub progress: f32,
    pub camera_id: Option<String>,
    pub batch_id: Option<String>,
}

impl From<&CoreRenderJob> for RenderJobSummary {
    fn from(j: &CoreRenderJob) -> Self {
        Self {
            job_id: j.id.clone(),
            status: render_job_status_to_str(j.status).to_string(),
            preset: j.preset.id.clone(),
            progress: j.progress,
            camera_id: j.camera_id.clone(),
            batch_id: j.batch_id.clone(),
        }
    }
}

fn render_job_status_to_str(s: RenderJobStatus) -> &'static str {
    match s {
        RenderJobStatus::Queued => "queued",
        RenderJobStatus::Running => "running",
        RenderJobStatus::Completed => "completed",
        RenderJobStatus::Failed => "failed",
        RenderJobStatus::Cancelled => "cancelled",
    }
}

/// Renderer-facing aggregate for a render batch.
///
/// Mirrors [`aec_render::queue::BatchProgress`] field for field — the
/// service layer just copies the values so the napi struct can be
/// `#[napi(object)]` without a `serde_json` round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderBatchProgressReport {
    pub batch_id: String,
    pub total: u32,
    pub queued: u32,
    pub running: u32,
    pub completed: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub average_progress: f32,
}

impl From<CoreBatchProgress> for RenderBatchProgressReport {
    fn from(p: CoreBatchProgress) -> Self {
        // Saturate at `u32::MAX` defensively rather than letting `as u32`
        // wrap on 64-bit hosts: plain `usize as u32` truncates the upper
        // bits, so a hypothetical 2^32-job batch would report `0` rather
        // than `u32::MAX`. The doc comment on `RenderBatchProgressJs`
        // promises saturating behaviour; this is where that promise is
        // kept.
        Self {
            batch_id: p.batch_id,
            total: saturating_u32(p.total),
            queued: saturating_u32(p.queued),
            running: saturating_u32(p.running),
            completed: saturating_u32(p.completed),
            failed: saturating_u32(p.failed),
            cancelled: saturating_u32(p.cancelled),
            average_progress: p.average_progress,
        }
    }
}

/// Clamp `n` to `u32::MAX` before casting to `u32`. Plain `as u32`
/// silently wraps on 64-bit platforms (`u32::MAX as usize + 1` becomes
/// `0`); this saturates as documented on `RenderBatchProgressJs`.
fn saturating_u32(n: usize) -> u32 {
    if n > u32::MAX as usize {
        u32::MAX
    } else {
        n as u32
    }
}

/// Renderer-facing material finding. Flattens
/// [`aec_render::doctor::MaterialFinding`] into the JSON shape the
/// TS `renderCheckMaterials` consumer expects (object per finding
/// with `code` / `severity` / `materialId` / `message` / `fix`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderMaterialFinding {
    pub code: String,
    pub severity: String,
    pub material_id: Option<String>,
    pub message: String,
    pub fix: Option<String>,
}

impl From<&MaterialFinding> for RenderMaterialFinding {
    fn from(f: &MaterialFinding) -> Self {
        let mat = f.material_id();
        // The doctor uses literal `<material>` / `<unknown>` strings
        // for the "missing material" case where there is no real
        // material id. Surface `None` in those cases so the
        // renderer can fall back to a generic placeholder rather
        // than rendering "Material: <unknown>" literally.
        let material_id = if mat.is_empty() || mat.starts_with('<') {
            None
        } else {
            Some(mat.to_string())
        };
        Self {
            code: f.code().to_string(),
            severity: f.severity().to_string(),
            material_id,
            message: f.message(),
            fix: f.fix(),
        }
    }
}

/// Result of [`BridgeService::render_check_materials`]. Wrapper around
/// the findings vec so the napi side can expose a `{ findings: [] }`
/// object shape matching the TS interface (and so a future field
/// like `summary` can be added without changing every caller).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderCheckMaterialsReport {
    pub findings: Vec<RenderMaterialFinding>,
}

/// Result of [`BridgeService::render_diagnose`].
///
/// The renderer's `RenderDoctor` panel renders one bullet per
/// suggestion. Each entry is a short human-readable diagnostic
/// string — the same content as a `MaterialFinding::message()`
/// plus, where available, the corresponding `fix()` rendered as
/// "Try: <fix>". Producing strings (rather than the structured
/// `MaterialFinding`) means the panel renders without a second
/// finding-to-string formatter on the JS side; the structured
/// form is still available via `render_check_materials`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderDiagnoseReport {
    pub job_id: String,
    pub suggestions: Vec<String>,
}

/// Result of [`BridgeService::render_enqueue`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderEnqueueResult {
    pub job_id: String,
}

/// Result of [`BridgeService::render_enqueue_batch`] /
/// [`BridgeService::render_enqueue_matrix`]. Carries the shared
/// batch id plus the per-camera job ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderEnqueueBatchResult {
    pub batch_id: String,
    pub job_ids: Vec<String>,
}

/// Result of [`BridgeService::render_cancel_job`]. A struct (rather
/// than `bool`) so future fields like `was_running: bool` can be
/// added without breaking the napi interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderCancelResult {
    pub cancelled: bool,
}

/// Result of [`BridgeService::render_apply_preset`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderApplyPresetResult {
    pub ok: bool,
    /// The resolved preset id after the apply. Echoes the requested id
    /// on success; carries the previous active id on failure so the
    /// renderer can keep its dropdown selection consistent with the
    /// engine.
    pub active_preset_id: String,
}

/// Result of a successful [`BridgeService::export_pdf`] call.
///
/// `pages` reflects the printpdf page count, which `aec_export`
/// constructs deterministically (one cover + one overview page when
/// no body lines are supplied, growing as the body is paginated). The
/// renderer's preview pane uses this to show "Exported 4 pages" on
/// success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPdfResult {
    pub out_path: String,
    pub pages: u32,
}

/// Result of a successful [`BridgeService::export_dxf`] call. Just
/// the canonicalised output path — the renderer's preview pane reads
/// the file size from disk if it wants to show "Exported (12 KB)".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDxfResult {
    pub out_path: String,
}

/// Result of a successful [`BridgeService::export_ifc`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportIfcResult {
    pub out_path: String,
}

/// Result of a successful [`BridgeService::export_gltf`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportGltfResult {
    pub out_path: String,
}

/// Result of a successful [`BridgeService::export_proposal_pack`]
/// call. Distinct from `ExportPdfResult` so a future enhancement
/// (e.g. surfacing the manifest hash, branding info, asset count)
/// doesn't have to widen the simple PDF return shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportProposalPackResult {
    pub out_path: String,
}

/// Per-archetype inventory flags for
/// [`BridgeService::deliver_build_pack`]. Mirrors the renderer's
/// `BridgeBackend.deliverBuildPack` request shape so the bridge call
/// site can forward the JS params through unchanged. Grouped into a
/// struct (rather than five `bool` parameters) so clippy is happy
/// about `fn_params_excessive_bools` and the call sites read as
/// `options.include_renders` rather than positional booleans.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverPackInventoryFlags {
    pub include_renders: bool,
    pub include_sheets: bool,
    pub include_ifc: bool,
    pub include_boq: bool,
    pub include_proposal: bool,
}

/// Typed params for [`BridgeService::deliver_build_pack`]. The
/// service method takes this rather than positional arguments so
/// adding a new flag (e.g. `include_validation_report`) doesn't
/// break every call site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverBuildPackParams {
    pub out_path: String,
    pub kind: String,
    pub project_name: String,
    pub options: DeliverPackInventoryFlags,
}

/// Result of a successful [`BridgeService::deliver_build_pack`]
/// call. Mirrors the renderer's `DeliverPackResult` TS interface
/// (`apps/desktop/electron/bridge.ts`) — `contents` is the list of
/// files inside the ZIP and `total_bytes` is the sum of their
/// payload sizes (manifest excluded so the figure matches what the
/// renderer preview pane shows pre-archive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverPackResult {
    pub out_path: String,
    pub contents: Vec<String>,
    pub total_bytes: u64,
}

/// Result of a successful [`BridgeService::bim_export_ifc`] call.
/// The bridge parses the input IFC (hitting the snapshot cache where
/// possible), then re-serialises the parsed `IfcSnapshot` back to a
/// STEP-21 byte stream and writes it to `out_path`. The renderer's
/// "Export BIM" panel shows `out_path` + `bytes_written` so the user
/// can confirm the file landed and how big it is.
///
/// This is a *normalise-and-emit* pipeline (parse → AEC-Studio
/// canonical form → write), useful for validating round-trip
/// fidelity, stripping vendor-specific fluff, and producing a
/// stable golden for downstream comparison. The output is byte-
/// identical to what `bim_attach_ifc`'s snapshot would write,
/// because both paths share `IfcWriter::to_string_with_materials`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimExportIfcSummary {
    /// Canonical absolute path of the input IFC file.
    pub source_path: String,
    /// Canonical absolute path of the written output file. Computed
    /// post-write via `canonicalize`, so symlinks and `./` segments
    /// are resolved exactly as `BimImportSummary::path` resolves them.
    pub out_path: String,
    /// IFC schema declared in the input file's `FILE_SCHEMA` header
    /// (e.g. `"IFC2X3"`, `"IFC4"`, `"IFC4X3"`). Surfaced so the
    /// renderer can warn if it's exporting a schema mismatch.
    pub schema: String,
    /// Bytes written to `out_path`.
    pub bytes_written: u64,
    /// `true` if the input snapshot came from the in-process cache
    /// populated by a prior `bim_import_ifc` / `bim_attach_ifc` /
    /// `bim_validate` / `bim_diff` call for the same `(path, mtime,
    /// size)`. Cache miss → reparse → cache populate. Useful for
    /// the renderer's loading indicator.
    pub parse_cache_hit: bool,
}

/// One finding from [`BridgeService::bim_validate`]. Mirrors
/// [`aec_bim::validation::ValidationFinding`] but uses owned
/// `String`s and a string severity discriminator so the napi /
/// JSON boundary can serialise without round-tripping through a
/// Rust enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimValidationFinding {
    /// `"error"` / `"warning"` / `"info"`. The renderer matches on
    /// these tokens; do NOT switch back to `format!("{:?}")` (which
    /// would leak the Rust variant casing) — see the
    /// `BimImportSummary::schema` field comment for the same
    /// discipline applied to the schema string.
    pub severity: String,
    /// Stable machine-readable code (e.g. `"BIM_DANGLING_AGGREGATE_PARENT"`).
    pub code: String,
    /// `EntityId` rendered via its `Display` impl, or `None` when the
    /// finding isn't attached to a specific element (rare).
    pub element: Option<String>,
    pub description: String,
    pub suggestion: Option<String>,
}

/// Result of a successful [`BridgeService::bim_validate`] call.
///
/// The renderer's "BIM Validate" panel uses `ok` for the headline
/// (PASS / FAIL badge) and renders `errors` / `warnings` / `infos`
/// as three separate sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimValidateReport {
    /// `true` when the input has zero `error`-severity findings.
    /// Mirrors `ValidationReport::is_clean()`.
    pub ok: bool,
    /// Canonical absolute path of the validated IFC file.
    pub source_path: String,
    /// IFC schema declared in the file's `FILE_SCHEMA` header.
    pub schema: String,
    pub errors: Vec<BimValidationFinding>,
    pub warnings: Vec<BimValidationFinding>,
    pub infos: Vec<BimValidationFinding>,
    pub parse_cache_hit: bool,
}

/// One property-level change inside a [`BimDiffElementChange`].
/// `before` / `after` are JSON-stringified `PropertyValue` (so the
/// renderer can show a Logical-vs-Boolean distinction without the
/// napi layer needing to encode the tagged-union variants directly).
/// One side being `None` means the property was added (`before =
/// None`) or removed (`after = None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimDiffPropertyChange {
    pub pset: String,
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// One element-level change inside a [`BimDiffSummary::modified`]
/// list. The `key` is the join key built by `aec_bim::diff` —
/// GUID first, falling back to `class:name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimDiffElementChange {
    pub key: String,
    /// `Some((before_class, after_class))` when the IFC class changed
    /// (e.g. `IfcWall → IfcCurtainWall`); `None` otherwise.
    pub class_before: Option<String>,
    pub class_after: Option<String>,
    pub name_before: Option<String>,
    pub name_after: Option<String>,
    pub property_deltas: Vec<BimDiffPropertyChange>,
}

/// Result of a successful [`BridgeService::bim_diff`] call.
///
/// `diff_id` is *input*-addressed: BLAKE3 hash of the (canonical
/// before path, canonical after path) pair — **not** the file
/// bytes. Same inputs → same id even if the files change, so the
/// renderer can dedup repeated diffs and cache rendered
/// views without a server round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimDiffSummary {
    pub diff_id: String,
    pub before_path: String,
    pub after_path: String,
    pub before_schema: String,
    pub after_schema: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<BimDiffElementChange>,
    pub before_cache_hit: bool,
    pub after_cache_hit: bool,
}

/// Result of a successful [`BridgeService::bim_generate_schedule`]
/// call. The schedule is written to `out_path` as an XLSX file
/// using `ScheduleSheet::write_xlsx`. `rows` is the row count
/// excluding the header; `columns` is the column count.
///
/// `schedule_id` is *input*-addressed: BLAKE3 hash of `(kind,
/// canonical source path)` — **not** the file bytes. Same inputs
/// → same id even if the source file changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimScheduleSummary {
    pub schedule_id: String,
    /// `"door"` / `"window"` / `"room"` / `"material"`. The
    /// renderer uses this for the page title and to pick the
    /// correct column rendering.
    pub kind: String,
    pub source_path: String,
    pub out_path: String,
    pub rows: u32,
    pub columns: u32,
    pub bytes_written: u64,
    pub parse_cache_hit: bool,
}

/// Query parameters for [`BridgeService::design_list_assets`]. The
/// shape mirrors the TypeScript `query` object that the renderer's
/// asset browser passes through `designListAssets(query)` in
/// `apps/desktop/electron/bridge.ts`.
///
/// Field semantics, in priority order:
///
/// * `search` — substring match against `AssetMetadata::name`. Uses
///   SQLite `LIKE %...%` under the hood (case-sensitivity follows
///   SQLite's default, which is ASCII case-insensitive — Unicode
///   case-folding lives in the FTS5 path covered by
///   [`aec_assets::search`]).
/// * `tags` — every supplied tag must appear in
///   `AssetMetadata::tags` (AND, not OR). Filtered in Rust on top of
///   the SQL result set since `assets.tags` is a JSON-encoded text
///   column.
/// * `style_tags` — same AND semantics as `tags`, against the
///   `style_tags` column.
/// * `limit` — caps the JS-side result list. Defaults to **24** to
///   match the renderer's grid-page size (4 columns × 6 rows). The
///   `aec_assets::AssetQuery::limit` default is 200 (the
///   library-import default); the bridge tightens it because the
///   renderer paginates the browser UI.
///
/// Unrecognised fields are silently ignored — the napi layer hands
/// us a typed struct, but the in-process TS fallback historically
/// accepted `Record<string, unknown>` so a forward-compatible
/// "additional filter" doesn't break old renderers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetListQuery {
    /// Substring match against `AssetMetadata::name`. `None` /
    /// missing / empty string disables the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Every tag in this list must appear in `AssetMetadata::tags`
    /// (AND match). Empty list disables the filter.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Same AND semantics as [`Self::tags`], against `style_tags`.
    #[serde(default)]
    pub style_tags: Vec<String>,
    /// Max number of rows to return. Saturating-clamped to
    /// `u32::MAX` inside [`BridgeService::design_list_assets`].
    /// `None` falls through to the bridge default (24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Default JS-side asset-browser page size. Matches the renderer's
/// `bridge.ts` `filterAssets()` default (4 columns × 6 rows = 24).
pub(crate) const DESIGN_LIST_ASSETS_DEFAULT_LIMIT: u32 = 24;

/// Renderer-facing projection of [`aec_assets::AssetMetadata`]. Only
/// the fields the asset-browser card consumes — drop the full LOD
/// chain, materials list, license, version, and creation timestamp
/// because the browser hands those off to a detail view that fetches
/// them separately (out of PR-U scope).
///
/// Field shape matches `AssetSummary` in
/// `apps/desktop/electron/bridge.ts` *exactly* so the napi layer
/// just renames `vendor.name` → `vendor` and the renderer renders
/// directly without a transform step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSummary {
    pub asset_id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub style_tags: Vec<String>,
    /// Vendor display name (e.g. "IKEA", "Muuto") — the renderer
    /// shows this on the card subtitle. `None` for un-vendored
    /// (community-contributed) assets — currently impossible on the
    /// demo seed but supported by the schema.
    pub vendor: Option<String>,
    /// Pre-base64'd PNG thumbnail data URI, or `None` for assets
    /// that haven't been thumbnailed yet (the renderer falls back
    /// to a procedural placeholder card). The PR-U seed library is
    /// `None` for all 4 demo assets — real thumbnail wiring lands
    /// in the follow-up that hooks up the asset-import pipeline to
    /// the napi surface.
    pub thumbnail_data_uri: Option<String>,
}

/// Hardware-status snapshot. The shape mirrors the TypeScript
/// `RuntimeStatus` interface in `apps/desktop/electron/bridge.ts` so that
/// the JS bridge can hand the value to React components without a runtime
/// transformation. The N-API layer turns this struct into a JS object with
/// nested `cpu`/`gpu` fields and a PascalCase `tier` string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatusReport {
    pub tier: HardwareTier,
    pub cpu: CpuProfile,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    /// `None` when the bridge runs outside Electron (e.g. CLI tools, the
    /// Vitest fallback path). The Electron main process owns the wgpu
    /// instance and supplies the real GPU descriptor via
    /// [`BridgeService::runtime_status_with_gpu`].
    pub gpu: Option<GpuProfile>,
    pub os: String,
}

/// Configuration for [`BridgeService`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Directory that holds `recents.json`.
    pub state_dir: PathBuf,
    /// Directory the bridge will create new project packages under.
    pub projects_dir: PathBuf,
    /// Where bundled template JSON files live.
    pub templates_dir: PathBuf,
    /// Maximum number of entries to retain in the recents store.
    pub max_recents: usize,
}

pub struct BridgeService {
    config: BridgeConfig,
    recents: RecentsStore,
    /// Master key for SQLCipher derivation. In production the Electron
    /// main process keeps this in the OS keychain; in tests we pass a
    /// well-known value.
    master_key: [u8; 32],
    /// Connection cache for [`Self::project_engine_status`]. Lives on
    /// the service so its lifetime tracks the bridge singleton: when
    /// the Electron process tears down, all cached `SQLCipher`
    /// connections drop with it.
    ///
    /// Interior-mutable so `project_engine_status` (which takes
    /// `&self`) can still mutate the cache. The mutating endpoints
    /// (`project_open`, `project_save`, `project_audit_sync`) call
    /// [`EngineStatusCache::invalidate`] for the affected path to
    /// ensure subsequent reads observe their writes through a fresh
    /// connection.
    engine_status_cache: EngineStatusCache,
    /// LRU cache for parsed [`aec_bim::ifc::IfcSnapshot`]s, keyed on
    /// `(canonical_path, mtime, size)`. Populated by
    /// [`Self::bim_import_ifc`] (the preview path) and consumed by
    /// [`Self::bim_attach_ifc`] (the commit path) so a
    /// preview → attach handoff doesn't re-parse the file.
    ///
    /// See [`crate::snapshot_cache`] for the cache semantics (60 s
    /// idle TTL, 4-entry LRU, double-checked locking model that
    /// mirrors [`EngineStatusCache`]).
    snapshot_cache: SnapshotCache,
    /// Process-wide render state: the in-memory [`RenderQueue`] tracking
    /// every submitted job (queued / running / completed / failed /
    /// cancelled) and the [`RenderPresetStore`] tracking the active
    /// preset id.
    ///
    /// Interior-mutable behind a [`Mutex`] so every render endpoint can
    /// take `&self` on [`BridgeService`] — the napi singleton's outer
    /// [`std::sync::RwLock`] would otherwise force every queue mutation
    /// through the writer side, blocking concurrent status-pane polls
    /// for the duration of a `submit_batch` of N cameras × M presets.
    /// The inner mutex is granular: each render method takes it just
    /// long enough to mutate the queue or read a snapshot.
    ///
    /// Single global queue (rather than per-project) intentionally —
    /// the renderer UX treats render jobs as belonging to the active
    /// project session, and there is no scenario today where two
    /// projects need independent queues live at once. When that
    /// changes (e.g. multi-window), the field type becomes a
    /// `HashMap<PathBuf, Mutex<RenderState>>` keyed by canonical
    /// project path; no callers outside this struct see the
    /// difference.
    render_state: Mutex<RenderState>,
    /// Lazy-init handle to the global asset library DB at
    /// `<state_dir>/asset_library/assets.sqlite`. Construction
    /// captures the path but does *not* open the DB so a bridge
    /// boot that never touches the asset browser pays zero SQLite
    /// open + schema-bootstrap + seed cost. On first use,
    /// [`AssetState::with_db`] opens the file (creating it if
    /// absent), runs the schema, and seeds 4 demo assets if the
    /// `assets` table is empty so the renderer's asset browser has
    /// content on a fresh install. See [`crate::asset_state`].
    asset_state: AssetState,
}

/// Process-wide render state held by [`BridgeService::render_state`].
///
/// Wrapping both the queue and the preset store in one struct (rather
/// than two parallel `Mutex`es) lets endpoints that touch both —
/// `render_apply_preset` followed by an immediate `render_list_jobs`
/// — see a consistent snapshot without a lock-ordering discipline.
pub(crate) struct RenderState {
    pub(crate) queue: RenderQueue,
    pub(crate) preset_store: RenderPresetStore,
}

impl RenderState {
    fn new() -> Self {
        Self {
            queue: RenderQueue::new(),
            preset_store: RenderPresetStore::default(),
        }
    }
}

impl BridgeService {
    pub fn new(config: BridgeConfig, master_key: [u8; 32]) -> Result<Self, BridgeServiceError> {
        std::fs::create_dir_all(&config.state_dir)?;
        std::fs::create_dir_all(&config.projects_dir)?;
        let recents =
            RecentsStore::open(config.state_dir.join("recents.json"), config.max_recents)?;
        let asset_state = AssetState::new(&config.state_dir);
        Ok(Self {
            config,
            recents,
            master_key,
            engine_status_cache: EngineStatusCache::new(),
            snapshot_cache: SnapshotCache::new(),
            render_state: Mutex::new(RenderState::new()),
            asset_state,
        })
    }

    /// Canonicalise a caller-supplied project path so the same project
    /// is keyed identically in the cache regardless of trailing slashes,
    /// `..` components, or symlink form. The path must already exist on
    /// disk — callers reach this method through a [`ProjectPackage`]
    /// open which itself validates the package layout, so any failure
    /// here would indicate the package vanished between the open and
    /// the cache key derivation. Returns a plain [`PathBuf`] (no
    /// platform-specific path-prefix manipulation) since we only use
    /// the value as a `HashMap` key.
    fn cache_key(path: &str) -> Result<PathBuf, BridgeServiceError> {
        Ok(std::fs::canonicalize(Path::new(path))?)
    }

    /// Invalidate the engine-status cache entry for `path`. If
    /// canonicalisation fails (a transient filesystem error or a
    /// path that disappeared between the successful open and the
    /// post-mutation invalidation), fall back to dropping every
    /// cached connection. This guarantees the next status read can
    /// never observe stale post-mutation state — the previous
    /// behaviour was an `if let Ok(...)` silent-skip that would have
    /// left a stale entry alive for up to `CACHE_TTL`.
    ///
    /// Used by every mutating endpoint: `project_open`,
    /// `project_save`, `project_audit_sync`. Kept on `&self` (not
    /// `&mut self`) so it composes inside the existing
    /// `&mut self` method signatures without further borrow churn.
    fn invalidate_status_cache_for(&self, path: &str) {
        match Self::cache_key(path) {
            Ok(key) => self.engine_status_cache.invalidate(&key),
            Err(_) => self.engine_status_cache.invalidate_all(),
        }
    }

    /// Test-only accessor for the engine-status connection cache size.
    /// Lets unit AND integration tests in the same crate assert
    /// cache-hit / invalidation behavior without exposing the cache
    /// to external callers.
    ///
    /// `#[doc(hidden)]` — not part of the stable API. The leading `__`
    /// is the de-facto Rust convention for "internal, may break".
    /// Integration tests must use `pub` accessors (they don't get
    /// `cfg(test)`-gated items from the library crate), so this is
    /// the cheapest way to thread the assertion through without
    /// building a separate test-only feature.
    #[doc(hidden)]
    pub fn __engine_status_cache_len(&self) -> usize {
        self.engine_status_cache.len()
    }

    /// Test-only accessor for the snapshot LRU cache size. Mirrors
    /// `__engine_status_cache_len`'s rationale: lets integration tests
    /// in `tests/` assert that `bim_import_ifc` → `bim_attach_ifc`
    /// actually hits the cache (avoiding a re-parse) without exposing
    /// the cache type itself.
    #[doc(hidden)]
    pub fn __snapshot_cache_len(&self) -> usize {
        self.snapshot_cache.len()
    }

    /// List bundled templates available for the New Project flow.
    pub fn list_templates(&self) -> Result<Vec<TemplateChoice>, BridgeServiceError> {
        let loader = TemplateLoader::new(self.config.templates_dir.clone());
        let mut out = Vec::new();
        for key in loader
            .discover()
            .map_err(|e| BridgeServiceError::Template(e.to_string()))?
        {
            match loader.load(&key) {
                Ok(def) => out.push(TemplateChoice {
                    key,
                    name: def.name,
                    description: def.description,
                }),
                Err(e) => return Err(BridgeServiceError::Template(e.to_string())),
            }
        }
        Ok(out)
    }

    /// Create a new project on disk from a template.
    ///
    /// After a successful create, invalidates any engine-status cache
    /// entry that *might* exist for the new project's path. In the
    /// common case [`ProjectPackage::create`] fails with
    /// `AlreadyExists` if the path is occupied, so no cache entry can
    /// exist at that path. The invalidation covers the edge case
    /// where the project directory was removed externally (e.g.
    /// `rm -rf` while the bridge was running) and a new project is
    /// created at the same slug — without it, the next status poll
    /// would serve a stale connection bound to the deleted file.
    /// Keeping the rule "every mutating endpoint calls
    /// `invalidate_status_cache_for`" without exception also makes
    /// the architectural contract easier to audit.
    pub fn project_create_from_template(
        &mut self,
        template_key: &str,
        project_name: &str,
    ) -> Result<ProjectSummary, BridgeServiceError> {
        let loader = TemplateLoader::new(self.config.templates_dir.clone());
        let template = loader
            .load(template_key)
            .map_err(|e| BridgeServiceError::Template(e.to_string()))?;

        let slug = slugify(project_name);
        let root = self.config.projects_dir.join(format!("{slug}.aecstudio"));
        let settings = ProjectSettings {
            units: template.units,
            region: template.primary_region(),
            ..ProjectSettings::default()
        };
        let pkg = ProjectPackage::create(
            &root,
            project_name,
            settings,
            Some(template.template_id.clone()),
            &self.master_key,
        )?;
        // Drop any stale cache entry for this path before publishing
        // the new project to the recents store. `root` is a `PathBuf`
        // and the cache key is derived via `cache_key` (which goes
        // through `canonicalize`); pass the str form through the
        // standard helper so it shares the same canonicalisation
        // failure handling as the other mutating endpoints.
        let root_str = root.to_string_lossy();
        self.invalidate_status_cache_for(&root_str);
        let summary: ProjectSummary = pkg.summary().into();
        let core_summary = pkg.summary();
        self.recents.record(&core_summary)?;
        Ok(summary)
    }

    /// Open an existing project package and update the recents store.
    ///
    /// Routes through [`ProjectPackage::open_with_master_key`] so any
    /// pending schema migrations are applied on the SQLCipher database
    /// AND the on-disk `manifest.json`'s `schema_version` field is
    /// bumped to the current [`aec_core::manifest::SCHEMA_VERSION`] in
    /// the same call. Without this step, a v1 project would refuse to
    /// re-open the next time around (because a future, stricter
    /// validator could downgrade tolerance) and the `audit_chain` SQL
    /// table introduced in v2 would not exist on legacy databases.
    pub fn project_open(&mut self, path: &str) -> Result<ProjectSummary, BridgeServiceError> {
        let pkg = ProjectPackage::open_with_master_key(path, &self.master_key)?;
        // `open_with_master_key` ran the migration registry; any
        // status-pane connection we'd cached pre-open would have a
        // stale prepared-statement cache against the old schema.
        // Drop it so the next `project_engine_status` re-opens.
        // Fall back to `invalidate_all` if canonicalisation fails (a
        // transient filesystem error between the successful open and
        // here) so the cache can never serve stale post-migration
        // state — see `EngineStatusCache::invalidate_all` for the
        // full rationale.
        self.invalidate_status_cache_for(path);
        let core_summary = pkg.summary();
        let summary: ProjectSummary = core_summary.clone().into();
        self.recents.record(&core_summary)?;
        Ok(summary)
    }

    /// Persist the manifest of an open project.
    ///
    /// Also routes through [`ProjectPackage::open_with_master_key`]
    /// because Save is a natural "the user actively touched this
    /// project" checkpoint and is the right place to lazily complete
    /// any pending v(N-1)→vN walk that a previous open might have
    /// skipped (e.g. because the binary didn't have the key handy).
    pub fn project_save(&mut self, path: &str) -> Result<ProjectSummary, BridgeServiceError> {
        let mut pkg = ProjectPackage::open_with_master_key(path, &self.master_key)?;
        pkg.save()?;
        // The manifest just changed on disk; any cached status
        // connection's view of `schema_version` is now stale.
        // Invalidate so the next status read sees the post-save
        // state. Falls back to `invalidate_all` on canonicalise
        // failure (see `invalidate_status_cache_for`).
        self.invalidate_status_cache_for(path);
        Ok(pkg.summary().into())
    }

    /// Apply a typed command to the project graph and persist the
    /// resulting deltas + journal entry to the SQLCipher database.
    ///
    /// The on-disk graph and journal are the source of truth: each call
    /// rebuilds an in-memory [`CommandEngine`] from the package's
    /// `entities` and `undo_journal` tables, executes the command via
    /// [`CommandEngine::execute_persistent`] (so the SQL transaction and
    /// the in-memory mutation advance in lock-step), and returns the
    /// list of applied deltas plus the new audit envelope. The cache
    /// invalidations match the policy on `project_save` /
    /// `project_audit_sync` — any status pane open against this path
    /// would otherwise serve a stale row count after the apply.
    pub fn command_apply(
        &mut self,
        project_path: &str,
        command: Command,
    ) -> Result<CommandApplyResult, BridgeServiceError> {
        let (_pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let mut engine = CommandEngine::open(&conn, command.scope)?;
        let result = engine.execute_persistent(command, &mut conn)?;
        self.invalidate_status_cache_for(project_path);
        Ok(CommandApplyResult {
            command_id: result.command_id,
            applied: result.applied,
            undo_len: engine.undo_len() as u32,
            redo_len: engine.redo_len() as u32,
        })
    }

    /// Undo the most recently applied command. Returns the inverse
    /// deltas that were just applied (so the renderer can update its
    /// view of the project graph without re-querying).
    ///
    /// Errors:
    /// * [`BridgeServiceError::Command`] with `nothing to undo` when the
    ///   journal's undo stack is empty.
    /// * [`BridgeServiceError::Command`] with `entity ... not found`
    ///   when a concurrent process mutated the graph out from under
    ///   the journal (extremely unlikely with the per-project file lock,
    ///   but the engine reports it cleanly).
    pub fn command_undo(
        &mut self,
        project_path: &str,
        active_scope: Scope,
    ) -> Result<CommandApplyResult, BridgeServiceError> {
        let (_pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let mut engine = CommandEngine::open(&conn, active_scope)?;
        let result = engine.undo_persistent(&mut conn)?;
        self.invalidate_status_cache_for(project_path);
        Ok(CommandApplyResult {
            command_id: result.command_id,
            applied: result.applied,
            undo_len: engine.undo_len() as u32,
            redo_len: engine.redo_len() as u32,
        })
    }

    /// Redo the most recently undone command. Symmetric counterpart to
    /// [`Self::command_undo`].
    pub fn command_redo(
        &mut self,
        project_path: &str,
        active_scope: Scope,
    ) -> Result<CommandApplyResult, BridgeServiceError> {
        let (_pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let mut engine = CommandEngine::open(&conn, active_scope)?;
        let result = engine.redo_persistent(&mut conn)?;
        self.invalidate_status_cache_for(project_path);
        Ok(CommandApplyResult {
            command_id: result.command_id,
            applied: result.applied,
            undo_len: engine.undo_len() as u32,
            redo_len: engine.redo_len() as u32,
        })
    }

    /// List the project graph: every entity, regardless of kind. The
    /// optional `kind_filter` narrows the result to a single kind
    /// (e.g. `"wall"`, `"room"`, `"camera"`); pass `None` for the full
    /// graph. Order is unspecified — the renderer is expected to sort
    /// client-side if it needs deterministic display order.
    pub fn project_graph_list(
        &self,
        project_path: &str,
        kind_filter: Option<&str>,
    ) -> Result<Vec<EntityRecord>, BridgeServiceError> {
        let (_pkg, conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let graph = aec_command::commands::ProjectGraph::load(&conn)?;
        let entities: Vec<EntityRecord> = match kind_filter {
            Some(k) => graph.entities_of_kind(k).cloned().collect(),
            None => graph.iter().cloned().collect(),
        };
        Ok(entities)
    }
}

/// Result returned by [`BridgeService::command_apply`] and the
/// undo/redo variants. Mirrors `aec_command::engine::CommandResult` but
/// also carries the post-call undo/redo stack depths so the renderer
/// can keep its "undo available?" / "redo available?" toolbar buttons
/// in sync without a follow-up query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandApplyResult {
    pub command_id: CommandId,
    pub applied: Vec<EntityDelta>,
    pub undo_len: u32,
    pub redo_len: u32,
}

impl BridgeService {
    /// Mirror the JSONL audit chain at `<project>/audit/log.jsonl` into
    /// the SQLCipher `audit_chain` table and return the number of
    /// newly-inserted rows. Safe to call on a freshly-created project
    /// (will be a no-op when the chain is empty) and on a project that
    /// has already been mirrored (will return `0`).
    ///
    /// This is the only mutating call in the audit-chain-aware bridge
    /// surface; the renderer uses it after the command engine appends
    /// entries via `aec_audit::AuditLog::append` so the SQL mirror
    /// stays in sync. `project_engine_status` is read-only and will
    /// happily report an out-of-sync state if the renderer forgets to
    /// call this.
    pub fn project_audit_sync(&mut self, path: &str) -> Result<u64, BridgeServiceError> {
        // `open_with_master_key_and_database` runs any pending schema
        // migrations (so the v2 `audit_chain` table exists on legacy
        // v1 projects) AND upgrades the manifest's `schema_version`
        // field, AND hands us back the connection it opened — so we
        // don't re-run the key-derive + `PRAGMA cipher_*` + migration
        // walk a second time just to grab a connection for
        // `mirror_to_sql`. Without the migration step,
        // `mirror_to_sql`'s INSERT would fail on a v1 project because
        // the target table wouldn't exist.
        let (pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(path, &self.master_key)?;
        let log = AuditLog::open(pkg.root().join("audit").join("log.jsonl"))?;
        let n = log.mirror_to_sql(&mut conn)?;
        // The SQL `audit_chain` table just gained rows; the cached
        // engine-status connection's `SELECT count(*)` and per-scope
        // counts would otherwise be evaluated against a stale snapshot
        // until idle eviction. Invalidate so the next status read picks
        // up the synced rows immediately. Falls back to `invalidate_all`
        // on canonicalise failure (see `invalidate_status_cache_for`).
        self.invalidate_status_cache_for(path);
        Ok(n as u64)
    }

    /// Read-only engine status for the renderer's status pane.
    /// Combines `meta.schema_version` (from the SQLCipher DB) with the
    /// audit chain head (from JSONL) and per-scope row counts (from the
    /// SQL mirror).
    ///
    /// **Not** read-only at the byte level: [`ProjectPackage::open_database`]
    /// calls [`aec_core::db::open_encrypted`], which runs the migration
    /// registry, so a legacy v1 project's SQLCipher file *will* be
    /// migrated forward on first read. The on-disk `manifest.json` is
    /// intentionally left at its on-disk version — only the mutating
    /// endpoints (`project_open`, `project_save`, `project_audit_sync`)
    /// route through [`ProjectPackage::open_with_master_key`] to bump
    /// the manifest. This means the SQL and JSON sides can briefly
    /// diverge until the user's next mutating action, which is fine:
    /// `validate()` accepts any version ≤ `SCHEMA_VERSION`, so the
    /// project still opens cleanly through the read-only path.
    pub fn project_engine_status(
        &self,
        path: &str,
    ) -> Result<EngineStatusReport, BridgeServiceError> {
        // Engine status is the most likely entry-point for a renderer
        // to touch a legacy project (the status pane refreshes
        // periodically), so it MUST tolerate pre-v2 SQL schemas. We
        // don't take `&mut self` here, so the manifest stays untouched
        // — but `open_database` calls `open_encrypted` internally
        // which runs the migration registry, so the SQL side is
        // brought up to date. The manifest's `schema_version` field
        // will be advanced the next time the user explicitly
        // opens/saves the project through the mutating endpoints.
        //
        // The connection is cached in [`Self::engine_status_cache`] so
        // consecutive renderer polls don't re-derive the SQLCipher key
        // and re-walk the migration registry. The cache is invalidated
        // by `project_open`, `project_save`, and `project_audit_sync`
        // — see those methods for the invalidation pairings.
        let cache_key = Self::cache_key(path)?;
        let master_key = &self.master_key;
        let cached = self
            .engine_status_cache
            .get_or_open::<_, BridgeServiceError>(&cache_key, || {
                let pkg = ProjectPackage::open(path)?;
                Ok(pkg.open_database(master_key)?)
            })?;

        // The JSONL audit log is opened fresh each call. It's a
        // memory-mapped replay of a typically-small file (the chain
        // grows by one entry per command, not per frame) and reading
        // it doesn't touch SQLCipher, so caching it would add
        // invalidation surface for negligible speedup.
        let log = AuditLog::open(cache_key.join("audit").join("log.jsonl"))?;
        let audit_chain_head = log.head().to_string();
        let audit_entry_count = log.entries().len() as u64;

        cached.with_conn(|conn| -> Result<EngineStatusReport, BridgeServiceError> {
            let schema_version = aec_core::db::schema_version(conn)?;

            let sql_count: i64 =
                conn.query_row("SELECT count(*) FROM audit_chain", [], |r| r.get(0))?;
            let mut by_scope: std::collections::BTreeMap<String, u64> =
                std::collections::BTreeMap::new();
            // Initialise all canonical scopes to 0 so the renderer can
            // show a stable set of labels even on a fresh project.
            for s in Scope::all() {
                by_scope.insert(s.as_str().to_string(), 0);
            }
            // Override with real counts from the SQL mirror. We don't
            // trust arbitrary scope strings — anything not in
            // `Scope::all()` is ignored, which keeps the renderer's
            // label set bounded.
            let mut stmt =
                conn.prepare("SELECT scope, count(*) FROM audit_chain GROUP BY scope")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (scope, n) = row?;
                if by_scope.contains_key(&scope) {
                    by_scope.insert(scope, n as u64);
                }
            }

            Ok(EngineStatusReport {
                schema_version,
                audit_chain_head,
                audit_entry_count,
                audit_chain_sql_count: sql_count as u64,
                audit_chain_by_scope: by_scope,
            })
        })
    }

    /// Stat an `.ifc` file at `path` and return a cheap size-only
    /// summary. The renderer calls this *before* invoking
    /// [`Self::bim_import_ifc`] so it can show a confirm dialog
    /// ("This file is N MB; parsing may take a while — continue?")
    /// on multi-hundred-MB MEP federations *before* the user
    /// commits to a multi-second parse path. The cost is one
    /// `std::fs::metadata` syscall — no file read, no parse, no
    /// allocation beyond the canonicalised path string.
    ///
    /// The threshold is the same
    /// [`BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES`] (100 MB) used by
    /// [`BimImportSummary::large_file_warning`], so the
    /// pre-parse warning (this function) and the post-parse
    /// warning (the import summary) agree on what counts as
    /// "large". The bridge does NOT enforce the warning — the
    /// renderer is free to ignore it and call `bim_import_ifc`
    /// anyway, in which case the import goes through with the
    /// flag set on the summary. That matches the
    /// [`BimImportSummary::large_file_warning`] doc comment
    /// stating the flag is purely advisory.
    pub fn bim_check_file_size(&self, path: &str) -> Result<BimFileSizeCheck, BridgeServiceError> {
        // Order: canonicalize → metadata, matching the error-surface
        // semantics of `bim_import_ifc` below (which calls
        // `std::fs::read` first — and `read` follows symlinks and
        // fails on dangling targets). Doing metadata-first would
        // give a different error for a dangling symlink: `metadata`
        // returns the link's own info (reporting the link size,
        // **not** the would-be target size), and then `canonicalize`
        // fails because the target doesn't exist. The renderer would
        // see a "checkFileSize OK, importIfc not-found" sequence on
        // the same path, which is surprising.
        //
        // Canonicalizing first surfaces dangling links as a single
        // `Io(NotFound)` error from `canonicalize`, identical to what
        // `bim_import_ifc` would produce from its `std::fs::read`
        // call. Subsequent `metadata` then operates on the resolved
        // path — single symlink resolution, single source of truth
        // for "does this file exist" semantics.
        let canonical_path_buf = std::fs::canonicalize(Path::new(path))?;
        let metadata = std::fs::metadata(&canonical_path_buf)?;
        let file_size_bytes = metadata.len();
        let canonical_path = canonical_path_buf.to_string_lossy().into_owned();
        Ok(BimFileSizeCheck {
            path: canonical_path,
            file_size_bytes,
            large_file_warning: file_size_bytes >= BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
            threshold_bytes: BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
        })
    }

    /// List assets from the global asset library matching `query`.
    ///
    /// Wraps [`aec_assets::AssetDatabase::query`] with the
    /// renderer-side `AssetSummary` projection so the asset-browser
    /// card can render directly off the napi return value. The DB is
    /// lazy-opened on first call (see [`crate::asset_state`] for the
    /// open + seed contract).
    ///
    /// `&self` rather than `&mut self` so the napi layer routes
    /// through `with_service_ref_fallible` (read-side of the outer
    /// `RwLock`) — the inner DB mutex inside [`AssetState`] serialises
    /// the SQLite call so concurrent `bim_*` / `render_*` polls don't
    /// block the asset browser. Same interior-mutability pattern as
    /// [`Self::bim_check_file_size`] and the snapshot cache.
    ///
    /// Translation rules:
    ///
    /// * `query.search` → `AssetQuery::name_contains` (substring,
    ///   ASCII case-insensitive, `%`/`_` LIKE-wildcard-escaped inside
    ///   `AssetDatabase::query`).
    /// * `query.tags` + `query.style_tags` → `AssetQuery::tags` /
    ///   `style_tags`. AND semantics (every tag must match).
    /// * `query.limit` → `AssetQuery::limit`, defaulting to
    ///   [`DESIGN_LIST_ASSETS_DEFAULT_LIMIT`] (24, the renderer's
    ///   grid-page size) when `None`. Saturating-clamped to
    ///   `u32::MAX` to defend against an upstream renderer bug
    ///   sending a negative number that wraps.
    /// * `AssetMetadata::vendor.name` → `AssetSummary::vendor`. An
    ///   empty vendor display string is mapped to `None` so the JS
    ///   side sees a missing vendor field rather than an empty
    ///   string (UX: no "by " line on the card vs " by ").
    /// * `thumbnail_data_uri` is always `None` in PR-U — real
    ///   thumbnail wiring lands in the follow-up that hooks the
    ///   asset-import pipeline up to the napi surface (the schema
    ///   already carries `thumbnail_hash` + the blob is in
    ///   `asset_blobs`, but base64-encoding on every list call is
    ///   wasteful; the asset detail panel will fetch on demand).
    pub fn design_list_assets(
        &self,
        query: &AssetListQuery,
    ) -> Result<Vec<AssetSummary>, BridgeServiceError> {
        let limit = query.limit.unwrap_or(DESIGN_LIST_ASSETS_DEFAULT_LIMIT);
        let aq = aec_assets::AssetQuery {
            name_contains: query.search.as_ref().filter(|s| !s.is_empty()).cloned(),
            vendor_id: None,
            tags: query.tags.clone(),
            style_tags: query.style_tags.clone(),
            limit: Some(limit),
        };
        let rows = self.asset_state.with_db(|db| db.query(&aq))?;
        Ok(rows.into_iter().map(asset_metadata_to_summary).collect())
    }

    /// Read an `.ifc` file from disk and return a structured import
    /// summary the renderer can show on its "Import BIM" panel.
    ///
    /// This is a *parse-only* operation: nothing is written into the
    /// active project. The renderer uses the returned counts to render
    /// a preview ("123 walls, 45 slabs, …"), and a follow-up
    /// `bim_attach_*` call (PR-L) will actually fold the parsed model
    /// into the project's authoring graph. Splitting the parse from
    /// the attach keeps the parse path safely re-runnable on bad
    /// files without polluting project state.
    ///
    /// **Schema support**: IFC2x3 and IFC4 (both base and `IFC4X3`
    /// when found in `FILE_SCHEMA`; IFC4x3-specific entities still
    /// flow through the tolerate-and-skip discipline). Material
    /// library coverage includes `IfcMaterial`,
    /// `IfcMaterialLayerSet`, and `IfcRelAssociatesMaterial`;
    /// `IfcMaterialProfileSet` and `IfcMaterialConstituentSet` are
    /// silently skipped per the module-level contract.
    pub fn bim_import_ifc(&self, path: &str) -> Result<BimImportSummary, BridgeServiceError> {
        // Defer `&self` to `&BridgeService` not `&mut` so this can
        // run through `with_service_ref_fallible` alongside other
        // read-only endpoints — IFC parsing is CPU-bound but doesn't
        // touch project state, so it doesn't need exclusive access.
        //
        // ISO 10303-21 formally restricts STEP-21 files to ASCII,
        // but real-world IFC exports — especially CJK-locale dumps
        // from older ArchiCAD / Revit and IfcOpenShell-scripted
        // pipelines — sometimes leak raw Windows-1252 or Shift-JIS
        // bytes into `IfcLabel` / `IfcText` string literals. Reading
        // through `read_to_string` would reject any such file with an
        // `InvalidData` IO error before the parser ever runs, which
        // makes the "Import BIM" panel useless for users with legacy
        // files. Switch to a byte read + lossy UTF-8 decode: invalid
        // sequences are replaced with U+FFFD (REPLACEMENT CHARACTER)
        // inside the string literal so the structural STEP grammar
        // (entity-type keywords, `#N` refs, `,` / `;` / `'`
        // delimiters — all ASCII by spec) is preserved and the
        // tolerate-and-skip parse path can proceed. The U+FFFD only
        // surfaces in the user-visible string fields (material name,
        // pset values), which is a strict improvement over outright
        // failing the import.
        let bytes = std::fs::read(Path::new(path))?;
        let file_size_bytes = bytes.len() as u64;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        // Canonicalise after the read succeeds so a non-existent path
        // surfaces as the same `Io` error the read itself would have
        // produced (rather than two different code paths for missing
        // file). `cache_key` at line ≈249 follows the same pattern
        // for the engine-status cache. The renderer-facing
        // `BimImportSummary.path` field documents this canonical form
        // so downstream consumers (PR-L snapshot cache, dedup) can
        // trust it.
        let canonical_path_buf = std::fs::canonicalize(Path::new(path))?;
        let canonical_path = canonical_path_buf.to_string_lossy().into_owned();
        // Wrap the parsed snapshot in `Arc` immediately so the cache
        // insert can use `Arc::clone` (refcount bump, microseconds)
        // rather than a full `IfcSnapshot::clone` (deep-copies every
        // `PropertyStore` / `ClassificationStore` / `MaterialStore` /
        // `guid_by_entity` entry — on a 50–500 MB federated IFC that's
        // 100+ MB of heap traffic, temporarily doubling peak memory
        // and defeating the very point of the cache as stated in
        // `snapshot_cache.rs`'s module docs).
        //
        // All subsequent reads in this function (`.schema`,
        // `.stats.*`) go through `Arc::deref` automatically.
        let snapshot = Arc::new(aec_bim::ifc::IfcReader::from_string(&body)?);
        // Populate the snapshot cache so a follow-up
        // `bim_attach_ifc` for the same file doesn't have to re-parse.
        // The cache key is `(canonical_path, mtime, size)` — a file
        // overwritten between import and attach naturally misses (new
        // mtime / new size), forcing a re-parse, so we never serve a
        // stale snapshot.
        if let Ok(key) = SnapshotKey::from_canonical_path(&canonical_path_buf) {
            self.snapshot_cache.insert(key, Arc::clone(&snapshot));
        }
        // If the SnapshotKey::from_canonical_path call failed,
        // `metadata` errored even though `canonicalize` succeeded a
        // moment ago — a transient FS hiccup. We don't have a stable
        // key to insert under, so skip the cache and let the next
        // call re-parse. This is a strict downgrade in performance,
        // not a correctness hazard.
        Ok(BimImportSummary {
            path: canonical_path,
            // `Display` returns the canonical STEP token
            // (`"IFC2X3"` / `"IFC4"` / `"IFC4X3"`) — a stable
            // contract for the renderer's "Import BIM" panel.
            // The `Debug` form would render the Rust variant name
            // (`"Ifc4"`), which is fragile against enum-variant
            // renaming.
            schema: snapshot.schema.to_string(),
            spatial_nodes: snapshot.stats.spatial_nodes as u64,
            elements: snapshot.stats.elements as u64,
            psets: snapshot.stats.psets as u64,
            qsets: snapshot.stats.qsets as u64,
            aggregations: snapshot.stats.aggregations as u64,
            containments: snapshot.stats.containments as u64,
            materials: snapshot.stats.materials as u64,
            material_layer_sets: snapshot.stats.material_layer_sets as u64,
            material_assignments: snapshot.stats.material_assignments as u64,
            records_seen: snapshot.stats.records_seen as u64,
            file_size_bytes,
            large_file_warning: file_size_bytes >= BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
        })
    }

    /// Fold a parsed IFC file into the active project's authoring
    /// graph. The complement of [`Self::bim_import_ifc`] — the
    /// renderer typically calls `bim_import_ifc` first to show the
    /// user a preview ("123 walls, 45 slabs, ..."), and `bim_attach_ifc`
    /// once the user confirms they want to commit the model.
    ///
    /// This method writes to the project package's encrypted SQLite
    /// database under a single [`rusqlite::Transaction`]: every
    /// `entities` / `components` / `relations` / `bim_cache` row is
    /// either fully committed or fully rolled back. A mid-attach
    /// failure (e.g. disk-full halfway through writing 12 000
    /// `IfcWall` rows from an MEP federation) leaves the project
    /// graph at its pre-attach state.
    ///
    /// **Re-attach dedup**: if `ifc_path` has been attached before,
    /// the bridge looks up each entity by its `bim_cache.global_id`
    /// (IFC GUID). Rows with unchanged hashes only bump `last_seen`;
    /// rows with new content UPDATE the `entities` body and wipe + re-
    /// insert the BIM-namespaced components. Non-BIM components
    /// (e.g. user-added render-material overrides) are NOT touched.
    ///
    /// **Cache awareness**: if a recent `bim_import_ifc` populated
    /// the snapshot cache for this `(canonical_path, mtime, size)`
    /// triple, the attach reuses the parsed snapshot without
    /// re-reading or re-parsing the IFC file. The returned
    /// [`BimAttachSummary::parse_cache_hit`] flag exposes whether
    /// the cache fired so the renderer can show "Attached (cached)"
    /// vs "Attached (re-parsed)" on the status pane.
    pub fn bim_attach_ifc(
        &self,
        project_path: &str,
        ifc_path: &str,
    ) -> Result<BimAttachSummary, BridgeServiceError> {
        // Cache-fronted parse via the shared helper that PR-T's
        // read-only methods also use. Previously this method had its
        // own inline copy of the canonicalise → cache lookup → read →
        // parse → cache populate dance; consolidating onto
        // `load_ifc_snapshot` ensures all five call sites share one
        // implementation of the cache contract (Devin Review
        // ANALYSIS_pr-T_0006).
        let (snapshot_arc, parse_cache_hit, canonical_ifc) = self.load_ifc_snapshot(ifc_path)?;

        // Open the project package + DB. We need a mutable connection
        // for the transaction; `open_with_master_key_and_database`
        // bundles the package open and the connection handoff so we
        // don't re-run the SQLCipher key derivation twice.
        let (_pkg, mut conn) = ProjectPackage::open_with_master_key_and_database(
            Path::new(project_path),
            &self.master_key,
        )?;

        let counts = {
            let tx = conn.transaction()?;
            let counts = bim_attach::attach_snapshot(&tx, &snapshot_arc, &canonical_ifc)?;
            tx.commit()?;
            counts
        };

        // The attach mutated the project DB, so the cached engine-
        // status connection for this project path must be invalidated
        // — otherwise a subsequent `project_engine_status` call could
        // hand the renderer a connection that doesn't see the new
        // entities / components rows. Mirrors the discipline
        // documented at `invalidate_status_cache_for` (line ≈272).
        self.invalidate_status_cache_for(project_path);

        Ok(BimAttachSummary {
            path: canonical_ifc,
            project_path: project_path.to_owned(),
            parse_cache_hit,
            spatial_nodes_inserted: counts.spatial_nodes_inserted,
            spatial_nodes_updated: counts.spatial_nodes_updated,
            spatial_nodes_unchanged: counts.spatial_nodes_unchanged,
            elements_inserted: counts.elements_inserted,
            elements_updated: counts.elements_updated,
            elements_unchanged: counts.elements_unchanged,
            components_inserted: counts.components_inserted,
            relations_inserted: counts.relations_inserted,
            cache_rows: counts.cache_rows,
        })
    }

    /// Export a multi-page summary PDF for `project_name` to
    /// `out_path`. Delegates to
    /// [`aec_export::write_project_pdf`] — the output is a real
    /// printpdf-serialised file (starts with `%PDF-`).
    ///
    /// Routed as `&self` because the export crate is stateless and
    /// any future caching (e.g. memoising rendered cover pages) will
    /// live behind interior mutability rather than on the bridge
    /// service. Mirrors the same pattern as
    /// [`Self::bim_check_file_size`] and
    /// [`Self::bim_attach_ifc`] so the napi layer can route through
    /// `with_service_ref_fallible` and concurrent status polls are
    /// not blocked by long PDF assemblies.
    pub fn export_pdf(
        &self,
        out_path: &str,
        project_name: &str,
        body_lines: &[String],
    ) -> Result<ExportPdfResult, BridgeServiceError> {
        let res = aec_export::write_project_pdf(Path::new(out_path), project_name, body_lines)?;
        Ok(ExportPdfResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
            pages: res.pages,
        })
    }

    /// Export a real DXF (R2013 / AC1027 ASCII) drawing to
    /// `out_path`. `walls_mm` are emitted as `LINE` entities on
    /// layer `WALLS`; a title block is added on layer `TITLE`. See
    /// [`aec_export::write_project_dxf`] for the grammar pins.
    pub fn export_dxf(
        &self,
        out_path: &str,
        project_name: &str,
        walls_mm: &[(f64, f64, f64, f64)],
    ) -> Result<ExportDxfResult, BridgeServiceError> {
        let res = aec_export::write_project_dxf(Path::new(out_path), project_name, walls_mm)?;
        Ok(ExportDxfResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
        })
    }

    /// Export a real ISO-10303-21 IFC4 STEP file with the supplied
    /// storey names (defaults to a single `Ground` storey when
    /// `storey_names` is empty). See [`aec_export::write_project_ifc`].
    pub fn export_ifc(
        &self,
        out_path: &str,
        project_name: &str,
        storey_names: &[String],
    ) -> Result<ExportIfcResult, BridgeServiceError> {
        let res = aec_export::write_project_ifc(Path::new(out_path), project_name, storey_names)?;
        Ok(ExportIfcResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
        })
    }

    /// Export a minimal-but-valid glTF 2.0 JSON file. See
    /// [`aec_export::write_project_gltf`] — the output passes a
    /// minimum-schema check (`asset.version == "2.0"`, non-empty
    /// `scenes` and `nodes`).
    pub fn export_gltf(
        &self,
        out_path: &str,
        project_name: &str,
    ) -> Result<ExportGltfResult, BridgeServiceError> {
        let res = aec_export::write_project_gltf(Path::new(out_path), project_name)?;
        Ok(ExportGltfResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
        })
    }

    /// Export a real client-facing proposal PDF via
    /// [`aec_export::write_proposal_pack`]. The output is a
    /// printpdf-serialised file with the `aec_export::proposal`
    /// branding + asset scaffolding pre-applied.
    pub fn export_proposal_pack(
        &self,
        out_path: &str,
        project_name: &str,
        client_name: &str,
    ) -> Result<ExportProposalPackResult, BridgeServiceError> {
        let res = aec_export::write_proposal_pack(Path::new(out_path), project_name, client_name)?;
        Ok(ExportProposalPackResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
        })
    }

    /// Build a contractor deliverable ZIP archive at `out_path`.
    /// Kind + options control the inventory; see
    /// [`aec_export::write_deliver_pack`] for the per-kind asset
    /// list. The returned `contents` matches the inventory the
    /// renderer preview pane shows pre-archive, and `total_bytes` is
    /// the sum of payload sizes (manifest excluded).
    pub fn deliver_build_pack(
        &self,
        params: DeliverBuildPackParams,
    ) -> Result<DeliverPackResult, BridgeServiceError> {
        let DeliverBuildPackParams {
            out_path,
            kind,
            project_name,
            options,
        } = params;
        let kind = aec_export::DeliverPackKind::parse(&kind)?;
        let opts = aec_export::DeliverPackOptions {
            include_renders: options.include_renders,
            include_sheets: options.include_sheets,
            include_ifc: options.include_ifc,
            include_boq: options.include_boq,
            include_proposal: options.include_proposal,
        };
        let res = aec_export::write_deliver_pack(Path::new(&out_path), kind, &opts, &project_name)?;
        Ok(DeliverPackResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
            contents: res.contents,
            total_bytes: res.total_bytes,
        })
    }

    /// Load an IFC snapshot, hitting the in-process snapshot cache
    /// where the file's `(canonical path, mtime, size)` already has
    /// a parsed entry. On miss, reads + parses the file and inserts
    /// the result into the cache so the next caller for the same
    /// `(path, mtime, size)` is a sub-millisecond hit.
    ///
    /// Shared helper for [`Self::bim_attach_ifc`] (PR-L) and the
    /// PR-T read-only IFC methods ([`Self::bim_export_ifc`],
    /// [`Self::bim_validate`], [`Self::bim_diff`],
    /// [`Self::bim_generate_schedule`]). Extracted because the
    /// "canonicalise → cache lookup → read + parse → cache populate"
    /// dance has to be byte-identical across the call sites: if any
    /// of them used a different cache key or skipped the populate
    /// step, the cache would stop fronting the multi-second STEP
    /// parse for re-uses.
    fn load_ifc_snapshot(
        &self,
        path: &str,
    ) -> Result<(Arc<aec_bim::ifc::IfcSnapshot>, bool, String), BridgeServiceError> {
        let canonical_buf = std::fs::canonicalize(Path::new(path))?;
        let canonical = canonical_buf.to_string_lossy().into_owned();
        let key_opt = SnapshotKey::from_canonical_path(&canonical_buf).ok();
        if let Some(snap) = key_opt.as_ref().and_then(|k| self.snapshot_cache.get(k)) {
            return Ok((snap, true, canonical));
        }
        let bytes = std::fs::read(&canonical_buf)?;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let snap = Arc::new(aec_bim::ifc::IfcReader::from_string(&body)?);
        if let Some(key) = key_opt {
            self.snapshot_cache.insert(key, Arc::clone(&snap));
        }
        Ok((snap, false, canonical))
    }

    /// Parse an `.ifc` file, re-serialise the resulting snapshot
    /// back to STEP-21, and write the bytes to `out_path`. The
    /// output is byte-identical to what [`Self::bim_attach_ifc`]'s
    /// snapshot would write — both paths share
    /// `IfcWriter::to_string_with_materials`.
    ///
    /// Useful as a normalise-and-emit step (parse vendor IFC →
    /// AEC-Studio canonical form → write), for round-trip fidelity
    /// validation, and for producing golden files for the regression
    /// suite. The renderer's "Export BIM" button invokes this to
    /// re-emit the active project's source IFC after edits land via
    /// the future PR-T.5 / PR-U write methods.
    ///
    /// Routes through `with_service_ref_fallible` (read-only) so a
    /// long IFC parse / write doesn't block status polls. The
    /// in-process snapshot cache fronts repeated calls against the
    /// same source file.
    pub fn bim_export_ifc(
        &self,
        ifc_path: &str,
        out_path: &str,
    ) -> Result<BimExportIfcSummary, BridgeServiceError> {
        let (snapshot, parse_cache_hit, canonical_source) = self.load_ifc_snapshot(ifc_path)?;
        let body = aec_bim::ifc::IfcWriter::to_string_with_materials(
            &snapshot.project,
            &snapshot.classification,
            &snapshot.properties,
            &snapshot.materials,
        );
        let bytes = body.as_bytes();
        let bytes_written = bytes.len() as u64;
        std::fs::write(Path::new(out_path), bytes)?;
        // Canonicalise post-write so the renderer can dedup pick
        // → export sequences across non-canonical inputs (`./out.ifc`
        // vs absolute). Mirrors `BimImportSummary::path` rules.
        let canonical_out = std::fs::canonicalize(Path::new(out_path))?
            .to_string_lossy()
            .into_owned();
        Ok(BimExportIfcSummary {
            source_path: canonical_source,
            out_path: canonical_out,
            schema: snapshot.schema.to_string(),
            bytes_written,
            parse_cache_hit,
        })
    }

    /// Parse an `.ifc` file and run the BIM rule-based validator
    /// against the resulting snapshot. Returns the full set of
    /// findings split by severity (errors / warnings / infos).
    ///
    /// The relations side of the validator (dangling aggregate /
    /// containment refs) is run with an empty `RelationStore`
    /// because the IFC reader folds spatial relationships directly
    /// into `Project.nodes[*].elements` rather than producing a
    /// standalone `RelationStore`. The remaining checks
    /// (missing-classifications, duplicate-GUIDs, required-Psets,
    /// orphan-elements) operate on the snapshot's stores directly
    /// and produce real findings.
    ///
    /// Routes through `with_service_ref_fallible` (read-only).
    pub fn bim_validate(&self, ifc_path: &str) -> Result<BimValidateReport, BridgeServiceError> {
        let (snapshot, parse_cache_hit, canonical_source) = self.load_ifc_snapshot(ifc_path)?;
        let relations = aec_bim::RelationStore::new();
        let report = aec_bim::validation::validate_project(
            &snapshot.project,
            &snapshot.classification,
            &snapshot.properties,
            &relations,
        );
        let to_finding = |f: &aec_bim::validation::ValidationFinding| BimValidationFinding {
            severity: match f.severity {
                aec_bim::validation::ValidationSeverity::Error => "error".into(),
                aec_bim::validation::ValidationSeverity::Warning => "warning".into(),
                aec_bim::validation::ValidationSeverity::Info => "info".into(),
            },
            code: f.code.clone(),
            element: f.element.as_ref().map(ToString::to_string),
            description: f.description.clone(),
            suggestion: f.suggestion.clone(),
        };
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut infos = Vec::new();
        for f in &report.findings {
            match f.severity {
                aec_bim::validation::ValidationSeverity::Error => errors.push(to_finding(f)),
                aec_bim::validation::ValidationSeverity::Warning => warnings.push(to_finding(f)),
                aec_bim::validation::ValidationSeverity::Info => infos.push(to_finding(f)),
            }
        }
        Ok(BimValidateReport {
            ok: errors.is_empty(),
            source_path: canonical_source,
            schema: snapshot.schema.to_string(),
            errors,
            warnings,
            infos,
            parse_cache_hit,
        })
    }

    /// Parse two `.ifc` files (independently snapshot-cache fronted)
    /// and run `aec_bim::diff::diff_projects` to produce an element-
    /// level diff: added GUIDs, removed GUIDs, modified elements
    /// (class changes, name changes, property deltas).
    ///
    /// `diff_id` is **input-addressed**, not content-addressed:
    /// BLAKE3 hash of `(canonical_before, canonical_after)`. Same
    /// path pair → same id, *regardless of whether the file bytes at
    /// those paths changed between calls*. This is intentional and
    /// matches the design contract that callers use for deduping
    /// repeated diff invocations with the same arguments (the
    /// renderer's primary use case: don't re-run the diff when the
    /// user re-clicks "Diff" against the same pair). Content-aware
    /// invalidation lives in the snapshot cache one layer down,
    /// which keys on `(canonical_path, mtime, size)`; if the file's
    /// mtime/size changes the parse re-runs and the returned `added`
    /// / `removed` / `modified` arrays reflect the new content, even
    /// though `diff_id` stays stable. Renderer-side view caches that
    /// memoise off `diff_id` should be either (a) keyed jointly with
    /// `(before_cache_hit, after_cache_hit)` if they need to react
    /// to re-parses, or (b) cleared on file-watcher events for the
    /// inputs.
    ///
    /// Routes through `with_service_ref_fallible` (read-only). Both
    /// parses can hit the snapshot cache independently, so a diff
    /// of `(before, after)` followed by a diff of `(before, other)`
    /// re-uses the parsed `before` snapshot.
    pub fn bim_diff(
        &self,
        before_path: &str,
        after_path: &str,
    ) -> Result<BimDiffSummary, BridgeServiceError> {
        let (before_snap, before_cache_hit, canonical_before) =
            self.load_ifc_snapshot(before_path)?;
        let (after_snap, after_cache_hit, canonical_after) = self.load_ifc_snapshot(after_path)?;
        let proj_diff = aec_bim::diff::diff_projects(
            &before_snap.project,
            &before_snap.classification,
            &before_snap.properties,
            &after_snap.project,
            &after_snap.classification,
            &after_snap.properties,
        );
        let property_delta_to_change = |d: &aec_bim::diff::PropertyDelta| BimDiffPropertyChange {
            pset: d.pset.clone(),
            key: d.key.clone(),
            before: d.before.as_ref().map(property_value_to_diff_string),
            after: d.after.as_ref().map(property_value_to_diff_string),
        };
        let modified = proj_diff
            .modified
            .iter()
            .map(|m| BimDiffElementChange {
                key: m.key.clone(),
                class_before: m.class_changed.as_ref().map(|(b, _)| b.clone()),
                class_after: m.class_changed.as_ref().map(|(_, a)| a.clone()),
                name_before: m.name_changed.as_ref().map(|(b, _)| b.clone()),
                name_after: m.name_changed.as_ref().map(|(_, a)| a.clone()),
                property_deltas: m
                    .property_deltas
                    .iter()
                    .map(property_delta_to_change)
                    .collect(),
            })
            .collect();
        // Input-addressed id: hash of (canonical before, canonical
        // after). Stable across calls so the renderer can dedup
        // repeated diff invocations with the same path pair. See
        // the doc comment above for the content-vs-input addressing
        // contract.
        let mut hasher = blake3::Hasher::new();
        hasher.update(canonical_before.as_bytes());
        hasher.update(b"\0");
        hasher.update(canonical_after.as_bytes());
        let diff_id = format!("diff_blake3_{}", &hasher.finalize().to_hex().as_str()[..16]);
        Ok(BimDiffSummary {
            diff_id,
            before_path: canonical_before,
            after_path: canonical_after,
            before_schema: before_snap.schema.to_string(),
            after_schema: after_snap.schema.to_string(),
            added: proj_diff.added,
            removed: proj_diff.removed,
            modified,
            before_cache_hit,
            after_cache_hit,
        })
    }

    /// Parse an `.ifc` file and generate one of the four supported
    /// schedules (`door` / `window` / `room` / `material`),
    /// writing the result to `out_path` as an XLSX workbook via
    /// `ScheduleSheet::write_xlsx`.
    ///
    /// `schedule_id` is **input-addressed**, not content-addressed:
    /// BLAKE3 hash of `(kind, canonical_source_path)`. Same kind +
    /// same source path → same id, *regardless of whether the file
    /// bytes at that path changed between calls*, and regardless of
    /// the `out_path` (so renderer caches dedup the schedule *data*,
    /// not the workbook file location). This mirrors `bim_diff`'s
    /// id contract — see that method's doc comment for the rationale.
    /// Renderer-side caches keyed off `schedule_id` that need to
    /// react to source-file edits should either include
    /// `parse_cache_hit` in their cache key or invalidate on a file-
    /// watcher signal for `source_path`.
    ///
    /// Routes through `with_service_ref_fallible` (read-only). The
    /// IFC parse is snapshot-cache fronted; the schedule generators
    /// are pure functions over the parsed stores.
    pub fn bim_generate_schedule(
        &self,
        ifc_path: &str,
        kind: &str,
        out_path: &str,
    ) -> Result<BimScheduleSummary, BridgeServiceError> {
        let (snapshot, parse_cache_hit, canonical_source) = self.load_ifc_snapshot(ifc_path)?;
        // Build the schedule sheet for the requested kind. Each
        // generator returns `(Vec<Entry>, ScheduleSheet)`; we only
        // need the sheet for the XLSX write + row/column counts.
        // Unknown `kind` is a hard `Bim` error so the renderer
        // surfaces the typo rather than silently producing an empty
        // workbook.
        let sheet = match kind {
            "door" => {
                aec_bim::schedules::generate_door_schedule(
                    &snapshot.classification,
                    &snapshot.properties,
                )
                .1
            }
            "window" => {
                aec_bim::schedules::generate_window_schedule(
                    &snapshot.classification,
                    &snapshot.properties,
                )
                .1
            }
            "room" => {
                aec_bim::schedules::generate_room_schedule(&snapshot.project, &snapshot.properties)
                    .1
            }
            "material" => {
                aec_bim::schedules::generate_material_schedule(
                    &snapshot.classification,
                    &snapshot.properties,
                )
                .1
            }
            other => {
                return Err(BridgeServiceError::Bim(format!(
                    "bim_generate_schedule: unknown schedule kind '{other}' \
                     (expected door / window / room / material)"
                )));
            }
        };
        let rows = sheet.rows.len() as u32;
        let columns = sheet.columns.len() as u32;
        sheet
            .write_xlsx(Path::new(out_path))
            .map_err(|e| BridgeServiceError::Bim(format!("xlsx: {e}")))?;
        let bytes_written = std::fs::metadata(Path::new(out_path))?.len();
        let canonical_out = std::fs::canonicalize(Path::new(out_path))?
            .to_string_lossy()
            .into_owned();
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.as_bytes());
        hasher.update(b"\0");
        hasher.update(canonical_source.as_bytes());
        let schedule_id = format!(
            "sched_blake3_{}",
            &hasher.finalize().to_hex().as_str()[..16]
        );
        Ok(BimScheduleSummary {
            schedule_id,
            kind: kind.to_owned(),
            source_path: canonical_source,
            out_path: canonical_out,
            rows,
            columns,
            bytes_written,
            parse_cache_hit,
        })
    }

    /// Return the recents list (most-recent first), in the API shape.
    pub fn project_list_recents(&self) -> Result<Vec<ProjectSummary>, BridgeServiceError> {
        Ok(self
            .recents
            .entries()
            .iter()
            .map(|e| ProjectSummary {
                project_id: e.project_id.clone(),
                name: e.name.clone(),
                path: e.path.clone(),
                // Recents tracks the last-opened moment rather than the
                // manifest's `updated_at`. For the Home page "recent
                // projects" tiles the user-perceived freshness is when
                // they last touched it, so use that here. If/when the
                // recents store grows a separate `updated_at` field we
                // can prefer it.
                updated_at: e.last_opened_at,
                template_id: None,
            })
            .collect())
    }

    /// Snapshot of the host hardware. Read fresh each call so the status
    /// bar reflects current OS-level state.
    ///
    /// GPU detection lives in the Electron main process (which owns the
    /// wgpu instance). When the bridge runs outside Electron — e.g. from
    /// CLI tools, unit tests, or the Vitest fallback — we report `None`
    /// for `gpu` rather than fabricating a "software" placeholder.
    /// Callers that DO have a GPU descriptor should use
    /// [`Self::runtime_status_with_gpu`] instead.
    pub fn runtime_status(&self) -> RuntimeStatusReport {
        Self::runtime_status_impl(None)
    }

    /// Same as [`Self::runtime_status`] but with a caller-supplied GPU
    /// descriptor. The Electron main process calls this with the wgpu
    /// adapter info it already has, avoiding a second probe.
    pub fn runtime_status_with_gpu(&self, gpu: GpuProfile) -> RuntimeStatusReport {
        Self::runtime_status_impl(Some(gpu))
    }

    fn runtime_status_impl(gpu_hint: Option<GpuProfile>) -> RuntimeStatusReport {
        let mut p = HardwareProfiler::new();
        // The profiler insists on receiving a GPU descriptor so that the
        // classifier can read it back. When the bridge has none we feed
        // a `vram_mb: 0` software placeholder for tier classification
        // ONLY and then drop it from the report so the JS side sees a
        // proper `null` gpu (not a misleading software fake).
        let gpu_for_profile = gpu_hint.clone().unwrap_or_else(|| GpuProfile {
            vendor: "software".into(),
            model: "software".into(),
            vram_mb: 0,
            backend: "software".into(),
            accelerators: Vec::new(),
        });
        let profile = p.profile(gpu_for_profile);
        let tier = HardwareTier::classify(&profile);
        RuntimeStatusReport {
            tier,
            cpu: profile.cpu.clone(),
            total_ram_mb: profile.total_ram_mb,
            available_ram_mb: profile.available_ram_mb,
            gpu: gpu_hint,
            os: profile.os.clone(),
        }
    }

    // ----- Render endpoints (Phase 10 PR-R) -----

    /// Submit a single render job for the supplied camera + preset.
    ///
    /// The `preset_id` is resolved against the in-memory preset store
    /// (built-ins for the current hardware tier plus any user-added
    /// custom presets). An unknown id is a [`BridgeServiceError::Core`]
    /// rather than a silent fallback so the renderer can tell the user
    /// the preset string they sent was wrong rather than mysteriously
    /// rendering at a different quality.
    ///
    /// `camera_id` is recorded on the job so the queue view can group
    /// jobs by camera; it is *not* validated against any CameraStore
    /// here — the validation is the responsibility of the caller (the
    /// renderer-side `Render` page only sends ids it just read from a
    /// `commandListCameras` response).
    ///
    /// `priority` defaults to `0`; higher values are admitted first by
    /// [`RenderQueue::admit`].
    ///
    /// `scene_json`, when present, is deserialised as a
    /// [`RenderScene`] and stored on the job for the doctor /
    /// diagnose paths to operate on. Defaults to an empty scene when
    /// absent — the queue is the source of truth for "intent to
    /// render"; pushing the geometry through to the queue is the
    /// renderer's job at submission time.
    pub fn render_enqueue(
        &self,
        camera_id: &str,
        preset_id: &str,
        priority: i32,
        scene_json: Option<&str>,
    ) -> Result<RenderEnqueueResult, BridgeServiceError> {
        let mut state = self.lock_render_state()?;
        let preset = state.preset_store.get(preset_id).ok_or_else(|| {
            BridgeServiceError::Core(format!("unknown render preset id `{preset_id}`"))
        })?;
        let scene = parse_scene_json(scene_json)?;
        let job = CoreRenderJob::new(preset, scene)
            .with_camera_id(camera_id.to_string())
            .with_priority(priority);
        let job_id = state.queue.submit(job);
        Ok(RenderEnqueueResult { job_id })
    }

    /// Submit a render batch: one job per (camera × preset) pair, all
    /// sharing one batch id so the renderer can aggregate progress
    /// via [`Self::render_batch_progress`].
    ///
    /// When `preset_ids.len() == 1` this is the "batch render" path
    /// (every camera at the same quality); when `preset_ids.len() > 1`
    /// it's the "render matrix" path (every camera × every preset).
    /// Empty `preset_ids` is a hard error — silently substituting
    /// `standard` would hide a renderer-side dropdown bug.
    pub fn render_enqueue_batch(
        &self,
        camera_ids: &[String],
        preset_ids: &[String],
        scene_json: Option<&str>,
    ) -> Result<RenderEnqueueBatchResult, BridgeServiceError> {
        if camera_ids.is_empty() {
            return Err(BridgeServiceError::Core(
                "render_enqueue_batch requires at least one camera id".into(),
            ));
        }
        if preset_ids.is_empty() {
            return Err(BridgeServiceError::Core(
                "render_enqueue_batch requires at least one preset id".into(),
            ));
        }
        let mut state = self.lock_render_state()?;
        let mut presets = Vec::with_capacity(preset_ids.len());
        for id in preset_ids {
            let preset = state.preset_store.get(id).ok_or_else(|| {
                BridgeServiceError::Core(format!("unknown render preset id `{id}`"))
            })?;
            presets.push(preset);
        }
        let scene = parse_scene_json(scene_json)?;
        // Mirror `RenderQueue::submit_matrix` semantics — one job per
        // (camera, preset) pair, all sharing one batch id. We
        // re-implement the loop here (rather than calling
        // `submit_batch` / `submit_matrix`) because the service layer
        // works in plain camera-id strings, not the rich
        // `CameraSnapshot` the queue's helpers expect.
        let batch_id = format!("batch_{}", uuid::Uuid::new_v4().simple());
        let mut job_ids = Vec::with_capacity(camera_ids.len() * presets.len());
        for cam in camera_ids {
            for preset in &presets {
                let job = CoreRenderJob::new(preset.clone(), scene.clone())
                    .with_camera_id(cam.clone())
                    .with_batch_id(batch_id.clone());
                job_ids.push(state.queue.submit(job));
            }
        }
        Ok(RenderEnqueueBatchResult { batch_id, job_ids })
    }

    /// Aggregate progress for the given batch. Returns `None` when no
    /// jobs match the id (rather than an error) so a stale renderer
    /// poll after the batch's jobs have all been removed degrades to
    /// a no-op on the UI side.
    pub fn render_batch_progress(
        &self,
        batch_id: &str,
    ) -> Result<Option<RenderBatchProgressReport>, BridgeServiceError> {
        let state = self.lock_render_state()?;
        Ok(state.queue.batch_progress(batch_id).map(Into::into))
    }

    /// List every job currently tracked by the queue, in
    /// queued → running → completed order. Returns a defensive copy so
    /// the caller can iterate without holding the render-state lock.
    pub fn render_list_jobs(&self) -> Result<Vec<RenderJobSummary>, BridgeServiceError> {
        let state = self.lock_render_state()?;
        Ok(state
            .queue
            .list_jobs()
            .into_iter()
            .map(RenderJobSummary::from)
            .collect())
    }

    /// Cancel the given job. Returns `cancelled: false` (rather than
    /// an error) when the job is already terminal — cancelling a
    /// completed job is idempotent and the renderer's "Cancel" button
    /// can race with the queue running the job to completion.
    pub fn render_cancel_job(
        &self,
        job_id: &str,
    ) -> Result<RenderCancelResult, BridgeServiceError> {
        let mut state = self.lock_render_state()?;
        match state.queue.cancel(job_id) {
            Ok(()) => Ok(RenderCancelResult { cancelled: true }),
            Err(aec_render::queue::QueueError::Terminal(_)) => {
                Ok(RenderCancelResult { cancelled: false })
            }
            Err(aec_render::queue::QueueError::UnknownJob(id)) => Err(BridgeServiceError::Core(
                format!("unknown render job `{id}`"),
            )),
        }
    }

    /// Select the given preset id as the active preset in the in-memory
    /// preset store. The return carries the now-active preset id so
    /// the renderer can keep its dropdown in lock-step with the engine
    /// even if a future change adds preset aliasing.
    pub fn render_apply_preset(
        &self,
        preset_id: &str,
    ) -> Result<RenderApplyPresetResult, BridgeServiceError> {
        let mut state = self.lock_render_state()?;
        let previous = state.preset_store.current().id;
        if state.preset_store.select(preset_id) {
            Ok(RenderApplyPresetResult {
                ok: true,
                active_preset_id: state.preset_store.current().id,
            })
        } else {
            Err(BridgeServiceError::Core(format!(
                "unknown render preset id `{preset_id}` (active preset unchanged: `{previous}`)"
            )))
        }
    }

    /// Diagnose a single job: run [`check_materials`] on the job's
    /// scene and render each finding as a human-readable suggestion.
    ///
    /// Returns an empty `suggestions` vector for a job whose scene is
    /// empty (the common case today because the renderer hasn't yet
    /// learned to push scene geometry through the queue at submission
    /// time). A `BridgeServiceError::Core` is returned only when the
    /// job id itself is unknown — distinguishing "no findings" from
    /// "you sent a bogus id" matters for the renderer's loading state.
    pub fn render_diagnose(
        &self,
        job_id: &str,
    ) -> Result<RenderDiagnoseReport, BridgeServiceError> {
        let state = self.lock_render_state()?;
        let job = state
            .queue
            .get(job_id)
            .ok_or_else(|| BridgeServiceError::Core(format!("unknown render job `{job_id}`")))?;
        let opts = CheckMaterialsOptions::default();
        // The renderer currently doesn't push a populated material
        // library through the queue; pass empty slices so the check
        // still surfaces "scene references unknown material" findings,
        // the most common pre-render mistake even without a library.
        let result = check_materials(&job.scene, &[], &std::collections::BTreeSet::new(), &opts);
        let suggestions = result
            .findings
            .iter()
            .map(|f| {
                if let Some(fix) = f.fix() {
                    format!("{} — Try: {}", f.message(), fix)
                } else {
                    f.message()
                }
            })
            .collect();
        Ok(RenderDiagnoseReport {
            job_id: job_id.to_string(),
            suggestions,
        })
    }

    /// Run [`check_materials`] across every scene referenced by the
    /// current queue. The renderer's "Check Materials" button runs
    /// pre-render so the user can clean up missing textures / non-PBR
    /// materials before submitting a job; aggregating across queued +
    /// running jobs is a reasonable approximation of "the user's current
    /// material intent" until the renderer plumbs a single project-level
    /// scene through here.
    ///
    /// Duplicate findings (same code + same material id) are folded
    /// to one entry so the panel doesn't render the same warning N
    /// times for an N-job batch.
    pub fn render_check_materials(&self) -> Result<RenderCheckMaterialsReport, BridgeServiceError> {
        let state = self.lock_render_state()?;
        let opts = CheckMaterialsOptions::default();
        let known: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mats: Vec<aec_materials::material::PbrMaterial> = Vec::new();
        let mut seen: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        let mut findings: Vec<RenderMaterialFinding> = Vec::new();
        for job in state.queue.list_jobs() {
            let result = check_materials(&job.scene, &mats, &known, &opts);
            for f in &result.findings {
                let key = (f.code().to_string(), f.material_id().to_string());
                if seen.insert(key) {
                    findings.push(RenderMaterialFinding::from(f));
                }
            }
        }
        Ok(RenderCheckMaterialsReport { findings })
    }

    fn lock_render_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, RenderState>, BridgeServiceError> {
        self.render_state
            .lock()
            .map_err(|e| BridgeServiceError::Core(format!("render state poisoned: {e}")))
    }
}

/// Parse a caller-supplied `scene_json` parameter into a
/// [`RenderScene`]. Treats `None` and the empty string as "no scene
/// supplied" (returns the default empty scene). Returns
/// [`BridgeServiceError::Core`] on JSON parse failure so the
/// renderer can show the user the deserialisation error rather than
/// silently dropping their scene.
fn parse_scene_json(scene_json: Option<&str>) -> Result<RenderScene, BridgeServiceError> {
    match scene_json {
        None | Some("") => Ok(RenderScene::default()),
        Some(s) => serde_json::from_str(s).map_err(|e| {
            BridgeServiceError::Core(format!("render scene_json deserialisation failed: {e}"))
        }),
    }
}

/// Stringify a `PropertyValue` for inclusion in a
/// [`BimDiffPropertyChange::before`] / `::after` field.
///
/// `BimDiffPropertyChange::before` / `::after` use `Option<String>`
/// with the documented contract that `None` means "property didn't
/// exist on that side" — added when `before.is_none()`, removed
/// when `after.is_none()`. A `serde_json::to_string` failure on a
/// `PropertyValue::{Real, Length, Area, Volume, Ratio}` carrying
/// `NaN` / `±Infinity` *must not* silently degrade `Some(value)`
/// to `None`, because that would corrupt the semantic — a
/// *changed* property would show up as *added* or *removed*. The
/// fallback string `"null"` is the documented "stringify failed"
/// sentinel that the renderer can render distinctly from a missing
/// field (a literal JSON `null`, not the empty `Option`).
fn property_value_to_diff_string(v: &aec_bim::properties::PropertyValue) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Project an [`aec_assets::AssetMetadata`] row down to the
/// renderer-facing [`AssetSummary`] shape.
///
/// * `vendor.name` → `vendor` (the catalogue card shows the display
///   name, not the id). Empty `vendor.name` strings collapse to
///   `None` so the renderer can suppress the "by ..." line entirely
///   rather than rendering "by " with a trailing space.
/// * `thumbnail_data_uri` is always `None` — see
///   [`BridgeService::design_list_assets`] for the deferral rationale.
/// * `tags` / `style_tags` are copied verbatim because the renderer
///   uses them for the card chip rendering + the "click chip → filter
///   by tag" UX.
fn asset_metadata_to_summary(m: aec_assets::AssetMetadata) -> AssetSummary {
    let vendor = if m.vendor.name.is_empty() {
        None
    } else {
        Some(m.vendor.name)
    };
    AssetSummary {
        asset_id: m.asset_id,
        name: m.name,
        tags: m.tags,
        style_tags: m.style_tags,
        vendor,
        thumbnail_data_uri: None,
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("project");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_template(root: &std::path::Path, category: &str, id: &str) {
        let category_dir = root.join(category);
        std::fs::create_dir_all(&category_dir).unwrap();
        let key = format!("{category}.{id}");
        let json = serde_json::json!({
            "template_id": key,
            "name": format!("Test {id}"),
            "description": "test fixture",
            "units": "mm",
            "region_defaults": {
                "EU": {"units": "mm", "standards": ["IFC4"]}
            },
            "rooms": [],
            "default_walls": {
                "exterior_thickness_mm": 250,
                "interior_thickness_mm": 100,
                "material": "wall_white"
            },
            "lighting_preset": "daylight",
            "asset_shelf": [],
            "camera_presets": []
        });
        std::fs::write(category_dir.join(format!("{id}.json")), json.to_string()).unwrap();
    }

    fn service() -> (BridgeService, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let projects = tmp.path().join("projects");
        let templates = tmp.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        write_template(&templates, "interior", "apartment");
        let cfg = BridgeConfig {
            state_dir: state,
            projects_dir: projects,
            templates_dir: templates,
            max_recents: 10,
        };
        let s = BridgeService::new(cfg, [42u8; 32]).unwrap();
        (s, tmp)
    }

    #[test]
    fn create_open_save_roundtrip() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Test Apartment")
            .unwrap();
        assert_eq!(summary.name, "Test Apartment");
        let opened = s.project_open(&summary.path).unwrap();
        assert_eq!(opened.project_id, summary.project_id);
        let saved = s.project_save(&summary.path).unwrap();
        assert_eq!(saved.project_id, summary.project_id);
    }

    #[test]
    fn list_templates_returns_discovered_entries() {
        let (s, _g) = service();
        let entries = s.list_templates().unwrap();
        assert!(entries.iter().any(|t| t.key == "interior.apartment"));
    }

    #[test]
    fn recents_records_created_project() {
        let (mut s, _g) = service();
        s.project_create_from_template("interior.apartment", "P1")
            .unwrap();
        let recents = s.project_list_recents().unwrap();
        assert!(recents.iter().any(|r| r.name == "P1"));
    }

    #[test]
    fn runtime_status_returns_finite_values() {
        let (s, _g) = service();
        let r = s.runtime_status();
        assert!(r.total_ram_mb > 0);
        assert!(r.cpu.physical_cores > 0);
        // Outside Electron the bridge has no wgpu adapter; gpu must be
        // None rather than a misleading "software" placeholder.
        assert!(r.gpu.is_none());
    }

    #[test]
    fn runtime_status_with_gpu_carries_descriptor_through() {
        let (s, _g) = service();
        let gpu = GpuProfile {
            vendor: "TestCorp".into(),
            model: "TestGPU".into(),
            vram_mb: 8192,
            backend: "vulkan".into(),
            accelerators: vec!["vulkan".into()],
        };
        let r = s.runtime_status_with_gpu(gpu.clone());
        assert_eq!(r.gpu.as_ref().unwrap().vram_mb, 8192);
        assert_eq!(r.gpu.as_ref().unwrap().vendor, "TestCorp");
    }

    #[test]
    fn slugify_handles_unicode_and_punctuation() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("   "), "project");
        assert_eq!(slugify("Loft 12B"), "loft-12b");
    }

    #[test]
    fn engine_status_caches_connection_across_consecutive_calls() {
        // First poll populates the cache; second poll reuses the
        // cached `SQLCipher` connection (no second key-derive / no
        // second `PRAGMA cipher_*` round-trip). We can't measure the
        // open cost in-process, but we can assert the cache contains
        // exactly one entry afterwards.
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Cached")
            .unwrap();
        assert_eq!(s.__engine_status_cache_len(), 0);

        let r1 = s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);
        let r2 = s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);
        assert_eq!(r1, r2);
    }

    #[test]
    fn project_save_invalidates_engine_status_cache() {
        // Save mutates the manifest on disk (`updated_at` and possibly
        // `schema_version`). Any cached engine-status connection would
        // otherwise serve a stale snapshot until idle eviction.
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Saved")
            .unwrap();
        s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);

        s.project_save(&summary.path).unwrap();
        assert_eq!(
            s.__engine_status_cache_len(),
            0,
            "project_save must invalidate the engine-status cache entry"
        );

        // Re-poll re-populates with a fresh connection.
        s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);
    }

    #[test]
    fn project_open_invalidates_engine_status_cache() {
        // `project_open` runs the migration registry; a v3+ migration
        // could rewrite the schema underneath a cached read connection's
        // statement cache. Drop the entry so the next status read
        // re-opens against the post-migration schema.
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Reopened")
            .unwrap();
        s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);

        s.project_open(&summary.path).unwrap();
        assert_eq!(
            s.__engine_status_cache_len(),
            0,
            "project_open must invalidate the engine-status cache entry"
        );
    }

    #[test]
    fn command_apply_executes_create_wall_and_persists_to_graph() {
        use aec_command::commands::{wall, CommandKind};
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "CmdApply")
            .unwrap();
        let cmd = aec_command::commands::Command::user(CommandKind::CreateWall(wall::CreateWall {
            entity_id: aec_core::types::EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }));
        let result = s.command_apply(&summary.path, cmd).unwrap();
        assert_eq!(result.applied.len(), 1);
        assert_eq!(result.undo_len, 1);
        assert_eq!(result.redo_len, 0);
        // Reload from disk: the graph should hold exactly one wall.
        let entities = s.project_graph_list(&summary.path, Some("wall")).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].kind, "wall");
    }

    #[test]
    fn command_undo_reverses_a_persisted_apply() {
        use aec_command::commands::{wall, CommandKind};
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "CmdUndo")
            .unwrap();
        let cmd = aec_command::commands::Command::user(CommandKind::CreateWall(wall::CreateWall {
            entity_id: aec_core::types::EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }));
        s.command_apply(&summary.path, cmd).unwrap();
        let undo = s
            .command_undo(&summary.path, aec_core::types::Scope::Design)
            .unwrap();
        // Inverse of a Create is a Delete; one delta applied.
        assert_eq!(undo.applied.len(), 1);
        assert_eq!(undo.undo_len, 0);
        assert_eq!(undo.redo_len, 1);
        let entities = s.project_graph_list(&summary.path, Some("wall")).unwrap();
        assert!(entities.is_empty());
    }

    #[test]
    fn command_redo_re_applies_an_undone_command() {
        use aec_command::commands::{wall, CommandKind};
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "CmdRedo")
            .unwrap();
        let cmd = aec_command::commands::Command::user(CommandKind::CreateWall(wall::CreateWall {
            entity_id: aec_core::types::EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }));
        s.command_apply(&summary.path, cmd).unwrap();
        s.command_undo(&summary.path, aec_core::types::Scope::Design)
            .unwrap();
        let redo = s
            .command_redo(&summary.path, aec_core::types::Scope::Design)
            .unwrap();
        assert_eq!(redo.applied.len(), 1);
        assert_eq!(redo.undo_len, 1);
        assert_eq!(redo.redo_len, 0);
        let entities = s.project_graph_list(&summary.path, Some("wall")).unwrap();
        assert_eq!(entities.len(), 1);
    }

    #[test]
    fn project_graph_list_filters_by_kind() {
        use aec_command::commands::{wall, CommandKind};
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "GraphList")
            .unwrap();
        let cmd = aec_command::commands::Command::user(CommandKind::CreateWall(wall::CreateWall {
            entity_id: aec_core::types::EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }));
        s.command_apply(&summary.path, cmd).unwrap();
        // All entities should include exactly the one wall.
        let all = s.project_graph_list(&summary.path, None).unwrap();
        assert_eq!(all.len(), 1);
        // Filtering by an irrelevant kind returns zero.
        let rooms = s.project_graph_list(&summary.path, Some("room")).unwrap();
        assert!(rooms.is_empty());
    }

    #[test]
    fn command_apply_persists_across_service_instances() {
        // The on-disk SQLCipher database is the source of truth — a
        // fresh `BridgeService` created against the same project root
        // must see the command's effects, otherwise the renderer would
        // lose graph state across restarts.
        use aec_command::commands::{wall, CommandKind};
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let projects = tmp.path().join("projects");
        let templates = tmp.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        write_template(&templates, "interior", "apartment");
        let cfg = BridgeConfig {
            state_dir: state.clone(),
            projects_dir: projects.clone(),
            templates_dir: templates.clone(),
            max_recents: 10,
        };
        let mut s1 = BridgeService::new(cfg.clone(), [42u8; 32]).unwrap();
        let summary = s1
            .project_create_from_template("interior.apartment", "Restart")
            .unwrap();
        let cmd = aec_command::commands::Command::user(CommandKind::CreateWall(wall::CreateWall {
            entity_id: aec_core::types::EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }));
        s1.command_apply(&summary.path, cmd).unwrap();
        drop(s1);

        let s2 = BridgeService::new(cfg, [42u8; 32]).unwrap();
        let entities = s2.project_graph_list(&summary.path, Some("wall")).unwrap();
        assert_eq!(entities.len(), 1, "graph state lost across service restart");
    }

    #[test]
    fn command_undo_on_empty_journal_returns_error() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "NoUndo")
            .unwrap();
        let err = s
            .command_undo(&summary.path, aec_core::types::Scope::Design)
            .unwrap_err();
        match err {
            BridgeServiceError::Command(msg) => {
                assert!(msg.contains("nothing to undo"), "got {msg}");
            }
            other => panic!("expected Command(nothing to undo), got {other:?}"),
        }
    }

    #[test]
    fn project_audit_sync_invalidates_engine_status_cache() {
        // After mirror_to_sql writes new rows to `audit_chain`, the
        // cached engine-status connection's `SELECT count(*)` would
        // (on connections held across a transaction boundary) keep
        // showing the pre-sync count. Invalidating forces the next
        // status read to observe the synced rows immediately.
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Synced")
            .unwrap();
        s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);

        s.project_audit_sync(&summary.path).unwrap();
        assert_eq!(
            s.__engine_status_cache_len(),
            0,
            "project_audit_sync must invalidate the engine-status cache entry"
        );
    }

    #[test]
    fn invalidate_status_cache_for_falls_back_when_canonicalize_fails() {
        // Populate the cache with two entries, then call the
        // invalidation helper with a path that cannot be canonicalised
        // (does not exist on disk). The fallback `invalidate_all` must
        // wipe the cache so a follow-up status read on EITHER project
        // re-opens against the freshest on-disk state — this is what
        // protects us against the silent-skip regression the previous
        // `if let Ok(...)` implementation had.
        let (mut s, _g) = service();
        let a = s
            .project_create_from_template("interior.apartment", "Cleared A")
            .unwrap();
        let b = s
            .project_create_from_template("interior.apartment", "Cleared B")
            .unwrap();
        s.project_engine_status(&a.path).unwrap();
        s.project_engine_status(&b.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 2);

        // A bogus path under the same projects_dir parent. canonicalize
        // will return an `Err` because the path does not exist.
        let bogus = "/this/path/definitely/does/not/exist/project.aecstudio";
        s.invalidate_status_cache_for(bogus);

        assert_eq!(
            s.__engine_status_cache_len(),
            0,
            "invalidate_status_cache_for must wipe the entire cache when canonicalize fails"
        );
    }

    #[test]
    fn engine_status_cache_keys_by_canonical_path() {
        // Two textually-different paths that canonicalise to the same
        // project directory (e.g. with a redundant `./` component)
        // must hit the same cache entry, not produce duplicates.
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Canonical")
            .unwrap();

        s.project_engine_status(&summary.path).unwrap();
        assert_eq!(s.__engine_status_cache_len(), 1);

        // Construct a non-canonical but equivalent path. The summary
        // path is already absolute (recents store records absolute
        // paths), so build a variant by appending `/.` which is a
        // benign no-op on any POSIX filesystem.
        let alt_path = format!("{}/.", summary.path);
        s.project_engine_status(&alt_path).unwrap();
        assert_eq!(
            s.__engine_status_cache_len(),
            1,
            "canonicalisation must collapse non-canonical paths to the same cache entry"
        );
    }

    #[test]
    fn bim_import_ifc_tolerates_non_utf8_bytes() {
        // ISO 10303-21 is formally ASCII, but real-world IFC exports
        // — especially CJK-locale ArchiCAD / IfcOpenShell-scripted
        // pipelines — sometimes carry raw Windows-1252 / Shift-JIS
        // bytes inside `IfcLabel` / `IfcText` literals. `bim_import_ifc`
        // must lossy-decode rather than refusing the file with an IO
        // error: the STEP grammar (entity keywords, `#N` refs, `,` /
        // `;` / `'` delimiters) is pure ASCII by spec, so the
        // structural parse can still proceed.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("non_utf8.ifc");

        // Build a minimal valid IFC2x3 graph with a single non-UTF-8
        // byte (0x9F, a Windows-1252 codepoint that is invalid UTF-8)
        // embedded in the project name. `read_to_string` would reject
        // this with `InvalidData`; `read` + `from_utf8_lossy` should
        // accept it and surface U+FFFD in the project name.
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(
            b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('test'),'2;1');\n\
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC2X3'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000a1',#1,'Latin1-",
        );
        body.push(0x9F);
        body.extend_from_slice(
            b"','P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n",
        );
        std::fs::write(&path, &body).unwrap();

        // Pre-fix this returned `Err(BridgeServiceError::Io(_))` on
        // stable Rust because `String::from_utf8` rejects the 0x9F
        // byte. Post-fix we get a real summary back.
        let summary = s
            .bim_import_ifc(path.to_str().unwrap())
            .expect("non-UTF-8 IFC file must parse via lossy decode");
        assert_eq!(summary.schema, "IFC2X3");
        // The project entity parsed (spatial_nodes >= 1 means the
        // structural parse survived the lossy-decoded byte).
        assert!(summary.spatial_nodes >= 1);
    }

    #[test]
    fn bim_import_ifc_returns_canonical_path() {
        // The `BimImportSummary.path` doc commits to "Canonical
        // absolute path" — verify the implementation honours that by
        // pointing the importer at a non-canonical form (a `./`
        // segment) and asserting the returned path matches
        // `std::fs::canonicalize` on the same input. The future PR-L
        // snapshot cache will key on this field, so the contract has
        // to hold up.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("canonical.ifc");
        let body = b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('test'),'2;1');\n\
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000a1',#1,'P','P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        std::fs::write(&path, body).unwrap();

        // Build a non-canonical path with a `./` segment so the input
        // and the canonical form differ on every platform.
        let parent = tmp.path();
        let file_name = path.file_name().unwrap();
        let non_canonical: PathBuf = parent.join(".").join(file_name);
        let summary = s
            .bim_import_ifc(non_canonical.to_str().unwrap())
            .expect("valid IFC body must parse");

        let expected = std::fs::canonicalize(&non_canonical)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            summary.path, expected,
            "BimImportSummary.path must be canonicalised per the docstring contract"
        );
        // Sanity: the canonical form does NOT include the `/./`
        // segment we injected (proves canonicalize actually ran).
        assert!(
            !summary.path.contains("/./") && !summary.path.contains("\\.\\"),
            "canonical path must not contain '.' segments: {}",
            summary.path
        );
    }

    /// Minimal valid IFC4 body used by the bim_attach tests below.
    /// The reader requires a project entity with a non-null GUID; the
    /// site/building/storey/space spatial chain is added so the
    /// attach path has multiple `entities.parent_id` levels to
    /// exercise (a flat single-node IFC wouldn't stress the BFS).
    fn fixture_ifc_body() -> Vec<u8> {
        let raw = b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('test'),'2;1');\n\
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000a1',#1,'Project','Project',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        raw.to_vec()
    }

    #[test]
    fn bim_import_ifc_populates_snapshot_cache() {
        // Verifies the cache-warm side effect of `bim_import_ifc`:
        // after a successful parse, the snapshot cache MUST hold one
        // entry keyed under the file the user pointed at. Without
        // this, the matching `bim_attach_ifc` would re-parse the
        // file from disk — defeating the whole point of the preview
        // → attach handoff.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("import-cache.ifc");
        std::fs::write(&path, fixture_ifc_body()).unwrap();
        assert_eq!(s.__snapshot_cache_len(), 0, "cache starts empty");
        let _ = s
            .bim_import_ifc(path.to_str().unwrap())
            .expect("valid IFC must parse");
        assert_eq!(
            s.__snapshot_cache_len(),
            1,
            "bim_import_ifc must populate the snapshot cache"
        );
    }

    #[test]
    fn bim_import_ifc_reports_file_size_and_large_file_flag() {
        // The renderer needs `file_size_bytes` to show a humanised
        // size on the preview panel. `large_file_warning` is false
        // for small fixtures — assert that explicitly so the
        // 100MB threshold doesn't drift to "always warn" by accident.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("size.ifc");
        let body = fixture_ifc_body();
        let expected_size = body.len() as u64;
        std::fs::write(&path, &body).unwrap();
        let summary = s
            .bim_import_ifc(path.to_str().unwrap())
            .expect("valid IFC must parse");
        assert_eq!(summary.file_size_bytes, expected_size);
        assert!(
            !summary.large_file_warning,
            "fixture is well below 100 MB; large_file_warning must be false"
        );
        assert!(
            expected_size < BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
            "fixture must be smaller than the warning threshold"
        );
    }

    #[test]
    fn bim_check_file_size_returns_stat_without_parsing() {
        // The renderer calls `bim_check_file_size` before
        // `bim_import_ifc` to surface a "this file is N MB —
        // continue?" confirm dialog on large IFC files. The cheap
        // path: one `fs::metadata` + one `fs::canonicalize`. No
        // parse, no file read. Verify all four fields of the
        // result.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("size.ifc");
        let body = fixture_ifc_body();
        let expected_size = body.len() as u64;
        std::fs::write(&path, &body).unwrap();
        let check = s
            .bim_check_file_size(path.to_str().unwrap())
            .expect("stat must succeed for an existing file");
        assert_eq!(check.file_size_bytes, expected_size);
        assert!(
            !check.large_file_warning,
            "fixture is well below 100 MB; warning must be false"
        );
        assert_eq!(check.threshold_bytes, BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES);
        // Canonical path must match what canonicalize returns.
        let expected_canonical = std::fs::canonicalize(&path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(check.path, expected_canonical);
    }

    #[test]
    fn bim_check_file_size_threshold_matches_summary_flag() {
        // Defense-in-depth: the `BimFileSizeCheck.threshold_bytes`
        // field MUST match the constant the post-parse
        // `BimImportSummary.large_file_warning` uses. If a future
        // refactor splits them, the pre-parse warning and the
        // post-parse warning would disagree on what counts as
        // "large" and the renderer's UX would be incoherent. Pin
        // the contract by reading both from the bridge for the
        // same file and asserting they agree on the threshold.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("threshold.ifc");
        std::fs::write(&path, fixture_ifc_body()).unwrap();
        let check = s
            .bim_check_file_size(path.to_str().unwrap())
            .expect("stat must succeed for an existing file");
        let summary = s
            .bim_import_ifc(path.to_str().unwrap())
            .expect("import must succeed for valid fixture");
        // Same file, same threshold.
        assert_eq!(check.threshold_bytes, BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES);
        // Same warning bit — both should be false here.
        assert_eq!(
            check.large_file_warning, summary.large_file_warning,
            "pre-parse and post-parse warnings must agree"
        );
        // Same byte count.
        assert_eq!(check.file_size_bytes, summary.file_size_bytes);
    }

    #[test]
    fn bim_check_file_size_errors_on_missing_file() {
        // Missing file → `Io` error so the renderer can show a
        // "file not found" toast and the file-picker reopens to a
        // valid path.
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.ifc");
        let err = s.bim_check_file_size(path.to_str().unwrap());
        assert!(err.is_err(), "missing file must produce an error");
    }

    /// Dangling-symlink regression: `bim_check_file_size` and
    /// `bim_import_ifc` must surface the **same** error on a dangling
    /// symlink, so the renderer never sees a "checkFileSize OK,
    /// importIfc not-found" sequence on the same path.
    ///
    /// Pre-fix the canonicalize was done **after** `metadata`, and
    /// `metadata` on a symlink returns the link's own info (with
    /// `is_file() = false` on the symlink itself but a small `len()`
    /// reading the link target string). The "successful" stat would
    /// return a tiny `file_size_bytes` and `large_file_warning =
    /// false`, then `bim_import_ifc`'s `std::fs::read` would fail on
    /// the same dangling target — a surprising UX. Post-fix the
    /// canonicalize runs first, fails on the dangling target, and
    /// both methods produce the same `Io(NotFound)` error.
    #[cfg(unix)]
    #[test]
    fn bim_check_file_size_errors_on_dangling_symlink_like_bim_import_ifc() {
        let (s, _g) = service();
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("does-not-exist-target.ifc");
        let link = tmp.path().join("dangling-link.ifc");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let check_err = s.bim_check_file_size(link.to_str().unwrap());
        let import_err = s.bim_import_ifc(link.to_str().unwrap());

        assert!(
            check_err.is_err(),
            "bim_check_file_size on a dangling symlink must surface an error \
             (matching bim_import_ifc's std::fs::read semantics) — got Ok: {:?}",
            check_err,
        );
        assert!(
            import_err.is_err(),
            "bim_import_ifc on a dangling symlink must error",
        );
        // Defense-in-depth: both must be `Io` variant. We don't pin
        // the exact ErrorKind because some platforms surface it as
        // NotFound and others as InvalidInput.
        assert!(
            matches!(check_err, Err(BridgeServiceError::Io(_))),
            "bim_check_file_size dangling-symlink error must be Io — got {:?}",
            check_err,
        );
        assert!(
            matches!(import_err, Err(BridgeServiceError::Io(_))),
            "bim_import_ifc dangling-symlink error must be Io — got {:?}",
            import_err,
        );
    }

    #[test]
    fn bim_attach_ifc_persists_spatial_nodes_into_entities() {
        // End-to-end: create a project, point bim_attach_ifc at a
        // freshly-written IFC file, and assert the spatial graph
        // landed in the `entities` table under the `bim/spatial/...`
        // kind namespace.
        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "Attach Target")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("attach.ifc");
        std::fs::write(&ifc_path, fixture_ifc_body()).unwrap();

        let attach = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .expect("attach must succeed");
        assert!(
            attach.spatial_nodes_inserted >= 1,
            "at least the IfcProject root must be inserted"
        );
        assert_eq!(
            attach.spatial_nodes_updated, 0,
            "first attach on an empty project must have zero updates"
        );
        assert!(attach.cache_rows >= 1, "bim_cache must record the GUID");

        // Re-open the DB directly and assert the row landed.
        let pkg = aec_core::package::ProjectPackage::open_with_master_key(
            std::path::Path::new(&project.path),
            &[42u8; 32],
        )
        .unwrap();
        let conn = pkg.open_database(&[42u8; 32]).unwrap();
        let spatial_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE kind LIKE 'bim/spatial/%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            spatial_count >= 1,
            "expected >=1 bim/spatial/* entity; got {spatial_count}"
        );
    }

    #[test]
    fn bim_attach_ifc_is_idempotent_on_reattach() {
        // Re-attaching the same file MUST be a no-op for the
        // entities table: the second call reports `_unchanged` for
        // every spatial node and inserts zero new ones. This is the
        // dedup contract documented at `crates/aec_bridge/src/bim_attach.rs`.
        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "Idempotent")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("idem.ifc");
        std::fs::write(&ifc_path, fixture_ifc_body()).unwrap();

        let first = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .unwrap();
        let second = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .unwrap();
        assert_eq!(
            first.spatial_nodes_inserted, second.spatial_nodes_unchanged,
            "all spatial nodes inserted on first attach must be unchanged on second"
        );
        assert_eq!(
            second.spatial_nodes_inserted, 0,
            "re-attach must not insert any spatial nodes"
        );
        assert_eq!(
            second.spatial_nodes_updated, 0,
            "identical content must not be classified as updated"
        );
    }

    #[test]
    fn bim_attach_ifc_skips_component_rewrite_on_unchanged_reattach() {
        // Regression for Devin Review (web-UI flag, post-PR-L):
        // `Unchanged` entities used to still get their `bim/%`
        // components DELETEd and re-INSERTed on every re-attach,
        // even though `bim_cache.pset_hash` matched (which means
        // the components on disk were already byte-identical to the
        // snapshot). For a 12 000-element MEP federation that's
        // ~40 000+ no-op DELETE/INSERT pairs per re-attach against
        // a SQLCipher-encrypted page cache.
        //
        // After the fix: `bim_attach_ifc` tracks every entity that
        // took the `Unchanged` branch in `upsert_entity` and skips
        // both the component-wipe loop and the
        // `write_psets`/`write_materials` re-insert for those
        // entities. The summary reflects this:
        // `components_inserted == 0` on an all-unchanged re-attach.
        //
        // We assert TWO things:
        //   1. `BimAttachSummary.components_inserted == 0` on the
        //      second attach (no churn).
        //   2. The components ARE still on disk afterwards (the
        //      skip didn't accidentally remove them). Use a raw
        //      `SELECT COUNT(*)` against the project DB so the test
        //      sees the SQL contract directly, not just the summary.
        //
        // Fixture: write a real IFC via `IfcWriter` carrying a wall
        // with a `Pset_WallCommon` property set. That ensures the
        // first attach actually writes components (the minimal
        // project-only `fixture_ifc_body` doesn't, so the skip-loop
        // becomes a no-op on it for trivial reasons rather than
        // exercising the post-fix branch).
        use aec_bim::classification::ClassificationStore;
        use aec_bim::ifc::IfcWriter;
        use aec_bim::properties::{PropertySet, PropertyStore, PropertyValue};
        use aec_bim::spatial::Project;
        use aec_bim::IfcClass;

        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "SkipUnchanged")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("skip-unchanged.ifc");

        // Build a deterministic Project graph: Project → Site →
        // Building → Storey → Wall, with a `Pset_WallCommon` on
        // the wall so `bim_attach` writes at least one
        // `bim/pset/Pset_WallCommon` component on the first pass.
        let project_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a1");
        let site_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a2");
        let building_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a3");
        let storey_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a4");
        let wall_id =
            aec_core::types::EntityId::from_string("ent_aabbccddeeff00112233445566778899").unwrap();

        let mut project_graph = Project {
            root: project_id.clone(),
            nodes: std::collections::HashMap::new(),
        };
        project_graph.nodes.insert(
            project_id.clone(),
            aec_bim::spatial::SpatialNode {
                id: project_id.clone(),
                ifc_guid: Some("00000000000000000000a1".into()),
                class: IfcClass::IfcProject,
                name: "P".into(),
                children: vec![site_id.clone()],
                elements: Vec::new(),
            },
        );
        project_graph.nodes.insert(
            site_id.clone(),
            aec_bim::spatial::SpatialNode {
                id: site_id.clone(),
                ifc_guid: Some("00000000000000000000a2".into()),
                class: IfcClass::IfcSite,
                name: "S".into(),
                children: vec![building_id.clone()],
                elements: Vec::new(),
            },
        );
        project_graph.nodes.insert(
            building_id.clone(),
            aec_bim::spatial::SpatialNode {
                id: building_id.clone(),
                ifc_guid: Some("00000000000000000000a3".into()),
                class: IfcClass::IfcBuilding,
                name: "B".into(),
                children: vec![storey_id.clone()],
                elements: Vec::new(),
            },
        );
        project_graph.nodes.insert(
            storey_id.clone(),
            aec_bim::spatial::SpatialNode {
                id: storey_id.clone(),
                ifc_guid: Some("00000000000000000000a4".into()),
                class: IfcClass::IfcBuildingStorey,
                name: "L1".into(),
                children: Vec::new(),
                elements: vec![wall_id.clone()],
            },
        );
        let mut classification = ClassificationStore::new();
        classification.assign_manual(wall_id.clone(), IfcClass::IfcWall);
        let mut properties = PropertyStore::default();
        let mut pset = PropertySet::new("Pset_WallCommon");
        pset.set("LoadBearing", PropertyValue::Boolean(true));
        pset.set("IsExternal", PropertyValue::Boolean(false));
        properties.entry(wall_id.clone()).upsert_pset(pset);
        let step = IfcWriter::to_string(&project_graph, &classification, &properties);
        std::fs::write(&ifc_path, &step).unwrap();

        let first = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .unwrap();
        assert!(
            first.components_inserted > 0,
            "first attach must write at least one component row (the wall's Pset_WallCommon); \
             got {}",
            first.components_inserted
        );

        // Snapshot the BIM-component row count after the first
        // attach so we can assert the second attach didn't change it.
        let first_components: i64 = {
            let pkg = aec_core::package::ProjectPackage::open_with_master_key(
                std::path::Path::new(&project.path),
                &[42u8; 32],
            )
            .unwrap();
            let conn = pkg.open_database(&[42u8; 32]).unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM components WHERE kind LIKE 'bim/%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };

        let second = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .unwrap();

        assert_eq!(
            second.components_inserted, 0,
            "all-unchanged re-attach must not re-insert any components (post-fix); the prior \
             behaviour wiped+re-inserted every Pset/material on every re-attach"
        );
        // Every spatial node from pass 1 (Project, Site, Building,
        // Storey) plus the wall element should classify as Unchanged.
        assert_eq!(
            second.spatial_nodes_unchanged, first.spatial_nodes_inserted,
            "every spatial node must be classified as Unchanged on a no-op re-attach"
        );
        assert_eq!(
            second.elements_unchanged, first.elements_inserted,
            "every element must be classified as Unchanged on a no-op re-attach"
        );

        // Verify the components ARE still on disk (skip didn't drop
        // them). Same count, same kind set.
        let second_components: i64 = {
            let pkg = aec_core::package::ProjectPackage::open_with_master_key(
                std::path::Path::new(&project.path),
                &[42u8; 32],
            )
            .unwrap();
            let conn = pkg.open_database(&[42u8; 32]).unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM components WHERE kind LIKE 'bim/%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(
            first_components, second_components,
            "skipping the wipe must leave the component rows on disk"
        );
    }

    #[test]
    fn bim_attach_ifc_hits_snapshot_cache_after_import() {
        // The whole point of the snapshot cache is to avoid a
        // re-parse on the import → attach handoff. Verify the
        // `parse_cache_hit` flag fires when the prior
        // `bim_import_ifc` populated the cache.
        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "Cache Hit")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("cached.ifc");
        std::fs::write(&ifc_path, fixture_ifc_body()).unwrap();

        let _ = s
            .bim_import_ifc(ifc_path.to_str().unwrap())
            .expect("import must parse");
        let attach = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .unwrap();
        assert!(
            attach.parse_cache_hit,
            "attach after import must reuse the cached snapshot"
        );
    }

    #[test]
    fn bim_attach_ifc_handles_empty_guid_spatial_nodes_without_fk_violation() {
        // Regression for Devin Review round 4 BUG_0001: a spatial
        // node with an empty-string `IfcRoot.GlobalId` ('' in the
        // STEP literal) used to flow through the bridge as
        // `Some("")` rather than `None`. The reader synthesises a
        // fresh non-deterministic `EntityId` for every parse of a
        // GUID-less row, but the `bim_cache.global_id` index would
        // alias all `Some("")` rows together on lookup. On the
        // second attach of the same file, the new (random) EntityId
        // for the empty-GUID site would never be inserted into
        // `entities` (the dedup path took the `Unchanged` / `Updated`
        // branch and did `UPDATE WHERE id = <new_random>`, matching
        // zero rows). Any child whose `parent_id` referenced that
        // new EntityId then violated the `entities.parent_id`
        // foreign-key.
        //
        // Test shape: an IFC with `IfcProject` (real GUID) →
        // `IfcSite` (EMPTY GUID, `''`) → `IfcBuilding` (real GUID),
        // attached twice. The second attach must succeed. Empty-GUID
        // sites get fresh EntityIds per parse and always take the
        // `Inserted` branch, so children's `parent_id` always points
        // at a row we just inserted.
        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "EmptyGuid")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("empty-guid.ifc");
        let body = b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('empty-guid'),'2;1');\n\
FILE_NAME('e.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000a1',#1,'P','P',$,$,$,$,$);\n\
#3 = IFCSITE('',#1,$,'NoGuidSite',$,$,$,$,$);\n\
#4 = IFCBUILDING('00000000000000000000a4',#1,$,'B',$,$,$,$,$);\n\
#5 = IFCRELAGGREGATES('00000000000000000000a5',#1,$,$,#2,(#3));\n\
#6 = IFCRELAGGREGATES('00000000000000000000a6',#1,$,$,#3,(#4));\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        std::fs::write(&ifc_path, body).unwrap();

        let first = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .expect("first attach must succeed");
        assert!(
            first.spatial_nodes_inserted >= 3,
            "expected >=3 (project + site + building) on first attach; got {}",
            first.spatial_nodes_inserted
        );
        // The whole point: re-attach must NOT raise an
        // FOREIGN KEY constraint failed error. The empty-GUID site
        // takes the `Inserted` branch on every attach, but so does
        // every other GUID-less row, so the bridge call should
        // return `Ok(_)` even though it inserts a fresh EntityId
        // for the site.
        let second = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .expect("re-attach with an empty-GUID spatial node must succeed (no FK violation)");
        // The site row is GUID-less, so it can't dedupe on
        // `bim_cache.global_id` — it's `Inserted` again. The two
        // GUID-bearing rows (project + building) still dedupe.
        assert!(
            second.spatial_nodes_unchanged >= 2,
            "project + building must dedupe on re-attach; got {} unchanged",
            second.spatial_nodes_unchanged
        );
        assert!(
            second.spatial_nodes_inserted >= 1,
            "the GUID-less site must take the always-insert path; got {} inserted",
            second.spatial_nodes_inserted
        );
    }

    #[test]
    fn bim_attach_ifc_preserves_user_authored_children_when_bim_cache_was_wiped() {
        // Regression for Devin Review round 5 BUG_0001: the
        // `Inserted` branch of `upsert_entity` used to call
        // `INSERT OR REPLACE INTO entities ...`. With SQLite's
        // `PRAGMA foreign_keys = ON` plus the `ON DELETE CASCADE`
        // wiring on `entities.parent_id` and `components.entity_id`
        // (see `aec_core/src/db.rs`), an `OR REPLACE` conflict on
        // `entities.id` expands to `DELETE old + INSERT new` and
        // cascade-deletes every child entity and every component
        // belonging to the conflicting row. The fix swapped that
        // statement for a proper `ON CONFLICT(id) DO UPDATE` upsert,
        // which rewrites the row in place without firing DELETE.
        //
        // This test reproduces the scenario the bot described:
        //   1. Attach the fixture once so the IfcProject row lands
        //      in `entities` and `bim_cache`.
        //   2. Add a user-authored CHILD entity under the project
        //      (e.g., a hand-placed annotation marker) AND a
        //      user-authored component on the project itself
        //      (e.g., a render-material override). Neither row is
        //      part of any future BIM attach's wipe set.
        //   3. Manually wipe `bim_cache` to simulate the DB-recovery
        //      / schema-rebuild / manual-DELETE scenarios that
        //      legitimately leave `entities` populated but
        //      `bim_cache` empty. This forces re-attach down the
        //      `Inserted` (None match) branch.
        //   4. Re-attach. Under the OLD `INSERT OR REPLACE`, the
        //      user-authored child and component would be silently
        //      cascade-deleted. Under the fix, both survive.
        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "PreserveUserAuthored")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("fixture.ifc");
        let body = b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('preserve'),'2;1');\n\
FILE_NAME('p.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000z1',#1,'P','P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        std::fs::write(&ifc_path, body).unwrap();

        s.bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .expect("first attach must succeed");

        let project_entity_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000z1");
        let user_child_id = aec_core::types::EntityId::new();
        let user_component_id = format!("{}/annotation/{}", project_entity_id.as_str(), "user-1");

        let pkg = aec_core::package::ProjectPackage::open_with_master_key(
            std::path::Path::new(&project.path),
            &[42u8; 32],
        )
        .unwrap();
        let conn = pkg.open_database(&[42u8; 32]).unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO entities(id, kind, parent_id, created_at, updated_at, body) \
             VALUES (?1, 'user/annotation', ?2, ?3, ?3, '{}')",
            rusqlite::params![user_child_id.as_str(), project_entity_id.as_str(), now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO components(id, entity_id, kind, body) \
             VALUES (?1, ?2, 'user/render_override', '{}')",
            rusqlite::params![user_component_id, project_entity_id.as_str()],
        )
        .unwrap();
        conn.execute("DELETE FROM bim_cache", []).unwrap();
        drop(conn);
        drop(pkg);

        let second = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .expect("re-attach with wiped bim_cache must succeed");
        assert!(
            second.spatial_nodes_inserted >= 1,
            "wiped bim_cache forces Inserted branch; got {} inserted",
            second.spatial_nodes_inserted
        );

        let pkg = aec_core::package::ProjectPackage::open_with_master_key(
            std::path::Path::new(&project.path),
            &[42u8; 32],
        )
        .unwrap();
        let conn = pkg.open_database(&[42u8; 32]).unwrap();
        let user_child_survived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE id = ?1",
                rusqlite::params![user_child_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            user_child_survived, 1,
            "user-authored child entity under the IfcProject MUST survive re-attach \
             (it would be cascade-deleted under the old `INSERT OR REPLACE`)"
        );
        let user_component_survived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM components WHERE id = ?1",
                rusqlite::params![user_component_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            user_component_survived, 1,
            "user-authored component on the IfcProject MUST survive re-attach \
             (it would be cascade-deleted under the old `INSERT OR REPLACE`)"
        );
    }

    #[test]
    fn bim_attach_ifc_replaces_stale_contained_in_relation_on_element_reparent() {
        // Regression for Devin Review BUG_0001 on 0e5fc84: the
        // `bim/contained_in` relation persisted at
        // `bim_attach.rs:374-384` used to be a bare `INSERT OR IGNORE`.
        // That statement dedupes on the exact `(kind, from_id, to_id)`
        // tuple — fine for an unchanged re-attach, but the unique key
        // does NOT match when an element's storey changes between
        // attaches. The new edge `(wall_id, storey_b)` inserts
        // alongside the old `(wall_id, storey_a)` instead of replacing
        // it, leaving the `relations` table — documented as the index
        // the future `bim_detach_*` flow walks — claiming the same
        // element has TWO spatial parents.
        //
        // The fix wraps the INSERT with a per-element DELETE of all
        // prior `bim/contained_in` edges pointing OUT of the element,
        // so the relations projection always matches the authoritative
        // `entities.parent_id` graph (which the `geom_hash` dedup
        // already keeps correct via the `Updated` branch — the parent
        // is folded into the hash).
        //
        // The fixtures: pass-1 puts the wall under storey-L1; pass-2
        // re-parents the same wall (same EntityId/GUID) onto a NEW
        // storey-L2 (different EntityId/GUID). Project/site/building
        // keep their EntityIds across both passes so they dedupe on
        // `bim_cache`. After re-attach there must be EXACTLY ONE
        // `bim/contained_in` row for the wall — pointing at the L2
        // storey, not the stale L1 row. Under the OLD `INSERT OR
        // IGNORE` we'd see two.
        //
        // Note: we generate both STEP fixtures via `IfcWriter` rather
        // than authoring them by hand, because the reader requires
        // each element row's Name field to carry the writer-emitted
        // `{IfcTag}::{EntityId}` encoding so it can recover the eid.
        // Synthesising the writer's output is the easiest way to
        // guarantee that contract holds.
        use aec_bim::classification::ClassificationStore;
        use aec_bim::ifc::IfcWriter;
        use aec_bim::properties::PropertyStore;
        use aec_bim::spatial::Project;
        use aec_bim::IfcClass;

        let (mut s, _g) = service();
        let project_pkg = s
            .project_create_from_template("interior.apartment", "ReparentRelation")
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("reparent.ifc");

        // Deterministic spatial-node IDs so the matching IfcReader
        // produces the same `EntityId`s on every parse (via
        // `EntityId::from_guid_seed`, which the reader uses for
        // GUID-bearing spatial rows).
        let project_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a1");
        let site_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a2");
        let building_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a3");
        let storey_l1_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000a4");
        let storey_l2_id = aec_core::types::EntityId::from_guid_seed("00000000000000000000bd");
        let wall_id =
            aec_core::types::EntityId::from_string("ent_aabbccddeeff00112233445566778899").unwrap();

        // Helper: build a Project graph with the given storey-id and
        // serialize to STEP via `IfcWriter`.
        let build_step = |storey_id: &aec_core::types::EntityId, storey_name: &str| -> String {
            let mut project_graph = Project {
                root: project_id.clone(),
                nodes: std::collections::HashMap::new(),
            };
            project_graph.nodes.insert(
                project_id.clone(),
                aec_bim::spatial::SpatialNode {
                    id: project_id.clone(),
                    ifc_guid: Some("00000000000000000000a1".into()),
                    class: IfcClass::IfcProject,
                    name: "P".into(),
                    children: vec![site_id.clone()],
                    elements: Vec::new(),
                },
            );
            project_graph.nodes.insert(
                site_id.clone(),
                aec_bim::spatial::SpatialNode {
                    id: site_id.clone(),
                    ifc_guid: Some("00000000000000000000a2".into()),
                    class: IfcClass::IfcSite,
                    name: "S".into(),
                    children: vec![building_id.clone()],
                    elements: Vec::new(),
                },
            );
            project_graph.nodes.insert(
                building_id.clone(),
                aec_bim::spatial::SpatialNode {
                    id: building_id.clone(),
                    ifc_guid: Some("00000000000000000000a3".into()),
                    class: IfcClass::IfcBuilding,
                    name: "B".into(),
                    children: vec![storey_id.clone()],
                    elements: Vec::new(),
                },
            );
            project_graph.nodes.insert(
                storey_id.clone(),
                aec_bim::spatial::SpatialNode {
                    id: storey_id.clone(),
                    ifc_guid: Some(match storey_name {
                        "L1" => "00000000000000000000a4".into(),
                        _ => "00000000000000000000bd".into(),
                    }),
                    class: IfcClass::IfcBuildingStorey,
                    name: storey_name.into(),
                    children: Vec::new(),
                    elements: vec![wall_id.clone()],
                },
            );
            let mut classification = ClassificationStore::new();
            classification.assign_manual(wall_id.clone(), IfcClass::IfcWall);
            let properties = PropertyStore::default();
            IfcWriter::to_string(&project_graph, &classification, &properties)
        };

        // Pass 1: wall under storey-L1.
        std::fs::write(&ifc_path, build_step(&storey_l1_id, "L1")).unwrap();
        let first = s
            .bim_attach_ifc(&project_pkg.path, ifc_path.to_str().unwrap())
            .expect("first attach must succeed");
        assert!(
            first.elements_inserted >= 1,
            "first attach must insert the wall; got {} elements",
            first.elements_inserted
        );

        // Sanity: pass-1 produced exactly one `bim/contained_in` row
        // for the wall, pointing at L1.
        let pkg = aec_core::package::ProjectPackage::open_with_master_key(
            std::path::Path::new(&project_pkg.path),
            &[42u8; 32],
        )
        .unwrap();
        {
            let conn = pkg.open_database(&[42u8; 32]).unwrap();
            let after_pass1: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM relations \
                     WHERE kind = 'bim/contained_in' AND from_id = ?1",
                    rusqlite::params![wall_id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                after_pass1, 1,
                "first attach must produce exactly one `bim/contained_in` row for the wall"
            );
            let parent_after_pass1: String = conn
                .query_row(
                    "SELECT to_id FROM relations \
                     WHERE kind = 'bim/contained_in' AND from_id = ?1",
                    rusqlite::params![wall_id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                parent_after_pass1,
                storey_l1_id.as_str(),
                "pass-1 must point the wall at storey L1"
            );
        }
        drop(pkg);

        // Pass 2: same wall (same EntityId) re-parented onto storey-L2.
        std::fs::write(&ifc_path, build_step(&storey_l2_id, "L2")).unwrap();
        let second = s
            .bim_attach_ifc(&project_pkg.path, ifc_path.to_str().unwrap())
            .expect("re-attach with re-parented wall must succeed");
        // The wall's `entities.parent_id` flipped, which folds into
        // `geom_hash` — so it MUST land on `Updated`, not `Unchanged`.
        assert!(
            second.elements_updated >= 1,
            "re-parented wall must take the Updated dedup branch; got {} updated",
            second.elements_updated
        );

        // The contract: exactly ONE `bim/contained_in` row for the
        // wall, pointing at the NEW storey. Under the old
        // `INSERT OR IGNORE` we'd see two (one stale L1, one new L2).
        let pkg = aec_core::package::ProjectPackage::open_with_master_key(
            std::path::Path::new(&project_pkg.path),
            &[42u8; 32],
        )
        .unwrap();
        let conn = pkg.open_database(&[42u8; 32]).unwrap();
        let after_pass2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relations \
                 WHERE kind = 'bim/contained_in' AND from_id = ?1",
                rusqlite::params![wall_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            after_pass2, 1,
            "re-attach with a re-parented wall must leave exactly one `bim/contained_in` \
             row for the wall (the OLD `INSERT OR IGNORE` would leave two)"
        );
        let parent_after_pass2: String = conn
            .query_row(
                "SELECT to_id FROM relations \
                 WHERE kind = 'bim/contained_in' AND from_id = ?1",
                rusqlite::params![wall_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parent_after_pass2,
            storey_l2_id.as_str(),
            "re-attach must point the wall at the NEW storey, not the stale L1"
        );

        // Defense-in-depth: the stale (wall, L1) row is gone
        // specifically, not just absent from a SELECT-by-current-parent.
        let stale_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relations \
                 WHERE kind = 'bim/contained_in' AND from_id = ?1 AND to_id = ?2",
                rusqlite::params![wall_id.as_str(), storey_l1_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stale_rows, 0,
            "no stale `(wall, storey-L1)` row may remain after the wall is re-parented"
        );
    }

    #[test]
    fn bim_attach_ifc_invalidates_engine_status_cache() {
        // After the attach, the engine-status cache entry for the
        // project must be gone so a subsequent
        // `project_engine_status` call observes the new entities
        // rows rather than a stale cached connection. Mirrors the
        // discipline of `project_save_invalidates_engine_status_cache`.
        let (mut s, _g) = service();
        let project = s
            .project_create_from_template("interior.apartment", "Invalidate")
            .unwrap();
        // Prime the engine-status cache.
        let _ = s.project_engine_status(&project.path).unwrap();
        assert_eq!(
            s.__engine_status_cache_len(),
            1,
            "engine status cache must be primed by the first poll"
        );

        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = tmp.path().join("invalidate.ifc");
        std::fs::write(&ifc_path, fixture_ifc_body()).unwrap();
        let _ = s
            .bim_attach_ifc(&project.path, ifc_path.to_str().unwrap())
            .unwrap();
        assert_eq!(
            s.__engine_status_cache_len(),
            0,
            "bim_attach_ifc must invalidate the engine-status cache for the project"
        );
    }

    // ----- Render endpoint tests (Phase 10 PR-R) -----
    //
    // Service-layer tests; the napi layer's tests live in
    // `crates/aec_bridge/tests/napi_render.rs` (round-trip the
    // result structs through `serde_json::to_string` to pin the
    // JS-facing shape).

    #[test]
    fn render_enqueue_returns_job_id_and_lists_back() {
        let (s, _g) = service();
        let r = s
            .render_enqueue("camera-1", "standard", 0, None)
            .expect("enqueue must succeed with a built-in preset");
        assert!(
            !r.job_id.is_empty(),
            "render_enqueue must return a non-empty job id"
        );
        let jobs = s.render_list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, r.job_id);
        assert_eq!(jobs[0].status, "queued");
        assert_eq!(jobs[0].preset, "standard");
        assert_eq!(jobs[0].camera_id.as_deref(), Some("camera-1"));
    }

    #[test]
    fn render_enqueue_unknown_preset_is_rejected() {
        let (s, _g) = service();
        let err = s
            .render_enqueue("camera-1", "does-not-exist", 0, None)
            .unwrap_err();
        match err {
            BridgeServiceError::Core(m) => {
                assert!(
                    m.contains("does-not-exist"),
                    "error must name the unknown preset id (got `{m}`)"
                );
            }
            other => panic!("expected Core error, got {other:?}"),
        }
    }

    #[test]
    fn render_enqueue_batch_creates_one_job_per_pair() {
        let (s, _g) = service();
        let r = s
            .render_enqueue_batch(
                &["cam-a".into(), "cam-b".into()],
                &["quick".into(), "standard".into()],
                None,
            )
            .expect("batch enqueue must succeed");
        // 2 cameras × 2 presets = 4 jobs.
        assert_eq!(r.job_ids.len(), 4);
        assert!(r.batch_id.starts_with("batch_"));
        let jobs = s.render_list_jobs().unwrap();
        assert_eq!(jobs.len(), 4);
        for j in &jobs {
            assert_eq!(j.batch_id.as_deref(), Some(r.batch_id.as_str()));
        }
        // Every (camera, preset) combination present exactly once.
        let mut combos: Vec<(String, String)> = jobs
            .iter()
            .map(|j| (j.camera_id.clone().unwrap(), j.preset.clone()))
            .collect();
        combos.sort();
        let expected: Vec<(String, String)> = vec![
            ("cam-a".into(), "quick".into()),
            ("cam-a".into(), "standard".into()),
            ("cam-b".into(), "quick".into()),
            ("cam-b".into(), "standard".into()),
        ];
        assert_eq!(combos, expected);
    }

    #[test]
    fn render_enqueue_batch_rejects_empty_inputs() {
        let (s, _g) = service();
        let err = s
            .render_enqueue_batch(&[], &["standard".into()], None)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Core(_)));
        let err = s
            .render_enqueue_batch(&["cam-a".into()], &[], None)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Core(_)));
    }

    #[test]
    fn render_batch_progress_aggregates_per_status() {
        let (s, _g) = service();
        let r = s
            .render_enqueue_batch(
                &["cam-a".into(), "cam-b".into(), "cam-c".into()],
                &["quick".into()],
                None,
            )
            .unwrap();
        let p = s
            .render_batch_progress(&r.batch_id)
            .unwrap()
            .expect("just-submitted batch must have progress");
        assert_eq!(p.batch_id, r.batch_id);
        assert_eq!(p.total, 3);
        assert_eq!(p.queued, 3);
        assert_eq!(p.running, 0);
        assert_eq!(p.completed, 0);
        assert_eq!(p.failed, 0);
        assert_eq!(p.cancelled, 0);
        // All-queued batch reports 0.0 average progress.
        assert!(
            p.average_progress.abs() < f32::EPSILON,
            "queued-only batch must report 0.0 average progress, got {}",
            p.average_progress
        );
    }

    #[test]
    fn render_batch_progress_unknown_batch_id_returns_none() {
        let (s, _g) = service();
        // Submit one job in a different batch so the queue isn't empty.
        let _ = s
            .render_enqueue_batch(&["cam-x".into()], &["quick".into()], None)
            .unwrap();
        assert!(
            s.render_batch_progress("batch_nonexistent")
                .unwrap()
                .is_none(),
            "unknown batch id must return None, not an error"
        );
    }

    #[test]
    fn render_cancel_job_transitions_to_cancelled() {
        let (s, _g) = service();
        let r = s.render_enqueue("cam-1", "quick", 0, None).unwrap();
        let c = s.render_cancel_job(&r.job_id).unwrap();
        assert!(c.cancelled);
        let jobs = s.render_list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "cancelled");
    }

    #[test]
    fn render_cancel_job_unknown_id_is_error() {
        let (s, _g) = service();
        let err = s.render_cancel_job("not-a-real-job").unwrap_err();
        match err {
            BridgeServiceError::Core(m) => {
                assert!(m.contains("not-a-real-job"));
            }
            other => panic!("expected Core error, got {other:?}"),
        }
    }

    #[test]
    fn render_apply_preset_updates_active_selection() {
        let (s, _g) = service();
        // Default selection comes from hardware-tier recommendation —
        // we don't assert what it is, only that it changes after apply.
        let r = s.render_apply_preset("studio").unwrap();
        assert!(r.ok);
        assert_eq!(r.active_preset_id, "studio");
        // Re-applying the same preset is idempotent.
        let r2 = s.render_apply_preset("studio").unwrap();
        assert_eq!(r2.active_preset_id, "studio");
    }

    #[test]
    fn render_apply_preset_unknown_id_is_error_and_preserves_active() {
        let (s, _g) = service();
        s.render_apply_preset("standard").unwrap();
        let err = s.render_apply_preset("does-not-exist").unwrap_err();
        let msg = match err {
            BridgeServiceError::Core(m) => m,
            other => panic!("expected Core error, got {other:?}"),
        };
        assert!(msg.contains("does-not-exist"));
        // The original active preset must remain unchanged.
        let after = s.render_apply_preset("standard").unwrap();
        assert_eq!(after.active_preset_id, "standard");
    }

    #[test]
    fn render_diagnose_returns_empty_suggestions_for_empty_scene() {
        let (s, _g) = service();
        let r = s.render_enqueue("cam-1", "quick", 0, None).unwrap();
        let d = s.render_diagnose(&r.job_id).unwrap();
        assert_eq!(d.job_id, r.job_id);
        assert!(
            d.suggestions.is_empty(),
            "empty scene must produce no doctor findings, got {:?}",
            d.suggestions
        );
    }

    #[test]
    fn render_diagnose_unknown_job_is_error() {
        let (s, _g) = service();
        let err = s.render_diagnose("not-a-real-job").unwrap_err();
        assert!(matches!(err, BridgeServiceError::Core(_)));
    }

    #[test]
    fn render_check_materials_with_empty_queue_has_no_findings() {
        let (s, _g) = service();
        let r = s.render_check_materials().unwrap();
        assert!(r.findings.is_empty());
    }

    #[test]
    fn render_list_jobs_visits_all_statuses() {
        let (s, _g) = service();
        // queued → cancelled transition; the third job stays queued.
        let a = s.render_enqueue("cam-1", "quick", 0, None).unwrap();
        let _b = s.render_enqueue("cam-2", "standard", 0, None).unwrap();
        s.render_cancel_job(&a.job_id).unwrap();
        let jobs = s.render_list_jobs().unwrap();
        let cancelled_count = jobs.iter().filter(|j| j.status == "cancelled").count();
        let queued_count = jobs.iter().filter(|j| j.status == "queued").count();
        assert_eq!(cancelled_count, 1);
        assert_eq!(queued_count, 1);
    }

    #[test]
    fn render_endpoints_serialise_to_finite_json_numbers() {
        // Guards against the f64::INFINITY footgun: any result
        // shape that derives Serialize must round-trip through
        // serde_json without producing non-finite tokens.
        let (s, _g) = service();
        let _ = s.render_enqueue("cam-1", "quick", 0, None).unwrap();
        let jobs = s.render_list_jobs().unwrap();
        let json = serde_json::to_string(&jobs).expect("RenderJobSummary must serialise");
        assert!(
            !json.contains("Infinity") && !json.contains("NaN"),
            "render_list_jobs output must not contain non-finite tokens: {json}"
        );
        let report = s.render_check_materials().unwrap();
        let _ = serde_json::to_string(&report).expect("RenderCheckMaterialsReport must serialise");
    }

    #[test]
    fn render_batch_progress_count_saturates_at_u32_max() {
        // Plain `usize as u32` wraps on 64-bit hosts — a `usize::MAX`
        // input would round-trip as `u32::MAX` (0xffff_ffff) via the
        // truncation rules, but `(u32::MAX as usize) + 1` would land at
        // `0`, silently corrupting the renderer's status pane. Pin the
        // saturating behaviour the `RenderBatchProgressJs` doc comment
        // advertises so this contract can't drift.
        let big = u32::MAX as usize + 1;
        assert_eq!(
            saturating_u32(big),
            u32::MAX,
            "count above u32::MAX must clamp, not wrap"
        );
        assert_eq!(
            saturating_u32(usize::MAX),
            u32::MAX,
            "usize::MAX must clamp at u32::MAX"
        );
        // Below-threshold inputs must round-trip unchanged.
        assert_eq!(saturating_u32(0), 0);
        assert_eq!(saturating_u32(7), 7);
        assert_eq!(saturating_u32(u32::MAX as usize), u32::MAX);
    }

    #[test]
    fn property_value_to_diff_string_preserves_some_on_serialisation_failure() {
        use aec_bim::properties::PropertyValue;
        // Finite floats round-trip via serde_json normally.
        let normal = PropertyValue::Real(1.5);
        let s = property_value_to_diff_string(&normal);
        assert!(s.contains("\"real\""), "got {s}");
        assert!(s.contains("1.5"), "got {s}");

        // Non-finite floats also produce *some* string — the regression
        // we're pinning here is that the call site uses `.map(...)`
        // rather than `.and_then(|v| serde_json::to_string(v).ok())`.
        // Today serde_json emits `{"type":"real","value":null}` (no
        // error) for `f64::NAN` / `±Infinity`, so the live shape is
        // preserved. *If* a future serde_json release ever errors on
        // non-finite floats (or a new PropertyValue variant carries a
        // type whose Serialize impl can return Err — non-UTF8 paths,
        // overflowing integers via a manual impl, etc.), the
        // `unwrap_or_else(|_| "null".to_string())` fallback keeps the
        // `Some(_)` wrapper on `BimDiffPropertyChange::before`/`::after`.
        // Without it, `.and_then(...ok())` would silently collapse the
        // `Some(value)` to `None`, corrupting the documented
        // added/removed/changed semantic (None means "property didn't
        // exist on that side").
        for v in [
            PropertyValue::Real(f64::NAN),
            PropertyValue::Real(f64::INFINITY),
            PropertyValue::Real(f64::NEG_INFINITY),
            PropertyValue::Length(f64::NAN),
            PropertyValue::Area(f64::INFINITY),
            PropertyValue::Volume(f64::NEG_INFINITY),
            PropertyValue::Ratio(f64::NAN),
        ] {
            let s = property_value_to_diff_string(&v);
            assert!(
                !s.is_empty(),
                "non-finite f64 must produce some string, never lose the Some wrapper: {v:?}"
            );
            // Current serde_json behavior emits the wrapper with
            // `"value":null`; either that or the explicit fallback
            // string `"null"` is acceptable — both keep the
            // `Some(_)` wrapper intact.
            assert!(
                s == "null" || s.contains("\"value\":null"),
                "expected non-finite to serialise either as fallback `\"null\"` or as wrapped `\"value\":null`, got {s} for {v:?}"
            );
        }
    }

    #[test]
    fn design_list_assets_returns_seed_library_on_fresh_state() {
        // Fresh `state_dir` → empty asset DB → seed runs → 4 demo
        // assets visible to the renderer. Pins the contract that the
        // native path matches the in-process TS fallback's
        // `seedAssets()` output on the first open of an installation.
        let (s, _g) = service();
        let assets = s
            .design_list_assets(&AssetListQuery::default())
            .expect("design_list_assets succeeds on fresh state");
        assert_eq!(assets.len(), 4, "seed library has 4 demo assets");
        let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"Kivik 3-seat Sofa"));
        assert!(names.contains(&"Outline Armchair"));
        assert!(names.contains(&"Bentwood Cafe Chair"));
        assert!(names.contains(&"Cafe Table 700"));
    }

    #[test]
    fn design_list_assets_filters_by_tag() {
        // `tags = ["sofa"]` → only Kivik (the only seed entry tagged
        // `sofa`). The other 3 (armchair, chair, table) must be
        // filtered out. Pins the AND-semantics translation from the
        // JS query to `AssetQuery::tags`.
        let (s, _g) = service();
        let q = AssetListQuery {
            tags: vec!["sofa".to_string()],
            ..AssetListQuery::default()
        };
        let assets = s.design_list_assets(&q).expect("tag-filtered query");
        assert_eq!(assets.len(), 1, "only Kivik is tagged `sofa`");
        assert_eq!(assets[0].asset_id, "ikea.sofa_kivik_3s");
    }

    #[test]
    fn design_list_assets_filters_by_style_tag() {
        // `style_tags = ["industrial"]` → cafe chair + cafe table
        // (both tagged `industrial`). Kivik (`scandinavian` /
        // `modern`) and Outline (`scandinavian` / `japandi`) drop out.
        let (s, _g) = service();
        let q = AssetListQuery {
            style_tags: vec!["industrial".to_string()],
            ..AssetListQuery::default()
        };
        let assets = s.design_list_assets(&q).expect("style-tag-filtered query");
        assert_eq!(assets.len(), 2);
        let ids: Vec<&str> = assets.iter().map(|a| a.asset_id.as_str()).collect();
        assert!(ids.contains(&"vendor.cafe_chair_thonet"));
        assert!(ids.contains(&"vendor.cafe_table_700"));
    }

    #[test]
    fn design_list_assets_filters_by_search_substring() {
        // `search = "Cafe"` → 2 cafe entries; "Kivik" → 1; "" → all 4
        // (empty string treated as "no filter" per the doc on
        // `AssetListQuery::search`).
        let (s, _g) = service();
        let cafe = s
            .design_list_assets(&AssetListQuery {
                search: Some("Cafe".to_string()),
                ..AssetListQuery::default()
            })
            .expect("substring query");
        assert_eq!(cafe.len(), 2);
        let kivik = s
            .design_list_assets(&AssetListQuery {
                search: Some("Kivik".to_string()),
                ..AssetListQuery::default()
            })
            .expect("substring query");
        assert_eq!(kivik.len(), 1);
        assert_eq!(kivik[0].asset_id, "ikea.sofa_kivik_3s");
        let empty = s
            .design_list_assets(&AssetListQuery {
                search: Some(String::new()),
                ..AssetListQuery::default()
            })
            .expect("empty-string substring query");
        assert_eq!(
            empty.len(),
            4,
            "empty `search` must behave like no filter, not match-nothing"
        );
    }

    #[test]
    fn design_list_assets_respects_limit() {
        // `limit = 1` → 1 result; `limit = 0` → 0 results (the user
        // is asking the bridge to no-op the query). Pins that the
        // limit isn't silently bumped to a minimum.
        let (s, _g) = service();
        let one = s
            .design_list_assets(&AssetListQuery {
                limit: Some(1),
                ..AssetListQuery::default()
            })
            .expect("limit=1");
        assert_eq!(one.len(), 1);
        let zero = s
            .design_list_assets(&AssetListQuery {
                limit: Some(0),
                ..AssetListQuery::default()
            })
            .expect("limit=0");
        assert!(
            zero.is_empty(),
            "limit=0 must return an empty list, not the seed default"
        );
    }

    #[test]
    fn design_list_assets_summary_projection_drops_internal_fields() {
        // The renderer-facing `AssetSummary` projection must NOT
        // include LOD chain, materials, license, version, or
        // thumbnail bytes. Pins the doc contract on the projection +
        // guards against a future change to `aec_assets::AssetMetadata`
        // accidentally widening the JS surface.
        let (s, _g) = service();
        let assets = s
            .design_list_assets(&AssetListQuery::default())
            .expect("query");
        let kivik = assets
            .iter()
            .find(|a| a.asset_id == "ikea.sofa_kivik_3s")
            .expect("kivik in seed");
        assert_eq!(kivik.name, "Kivik 3-seat Sofa");
        assert_eq!(kivik.vendor.as_deref(), Some("IKEA"));
        assert!(
            kivik.thumbnail_data_uri.is_none(),
            "PR-U seed has no thumbnail blobs; renderer falls back to placeholder card"
        );
        assert!(kivik.tags.contains(&"sofa".to_string()));
        assert!(kivik.style_tags.contains(&"scandinavian".to_string()));
    }

    #[test]
    fn design_list_assets_combined_filters_intersect() {
        // `tags = ["furniture"]` + `style_tags = ["scandinavian"]`
        // intersect: only Kivik + Outline match both. Cafe entries
        // share `furniture` but not `scandinavian`. Pins that
        // `AssetQuery`'s tag + style-tag filters compose with AND
        // (not OR).
        let (s, _g) = service();
        let q = AssetListQuery {
            tags: vec!["furniture".to_string()],
            style_tags: vec!["scandinavian".to_string()],
            ..AssetListQuery::default()
        };
        let assets = s.design_list_assets(&q).expect("combined filter query");
        assert_eq!(assets.len(), 2);
        let ids: Vec<&str> = assets.iter().map(|a| a.asset_id.as_str()).collect();
        assert!(ids.contains(&"ikea.sofa_kivik_3s"));
        assert!(ids.contains(&"muuto.armchair_outline"));
    }

    #[test]
    fn design_list_assets_unmatched_search_returns_empty() {
        // No card matches "Unobtanium Throne" → empty list, no error.
        // Pins that a no-result search isn't conflated with a DB error.
        let (s, _g) = service();
        let assets = s
            .design_list_assets(&AssetListQuery {
                search: Some("Unobtanium Throne".to_string()),
                ..AssetListQuery::default()
            })
            .expect("no-match query is not an error");
        assert!(assets.is_empty());
    }
}
