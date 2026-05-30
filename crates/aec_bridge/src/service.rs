//! Plain-Rust service layer for the bridge.
//!
//! Everything the Electron renderer can do ultimately calls into one of
//! these methods. Keeping the napi wrappers thin and the logic here makes
//! this layer trivially testable.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_ai::{
    AiAuditLogger, DiffEngine, DiffStatus, GrammarRegistry as AiGrammarRegistry,
    PlanRequest as AiPlanRequest, ToolName as AiToolName, ToolPlanner, ToolSchema as AiToolSchema,
    ToolSchemaRegistry as AiToolSchemaRegistry,
};
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

use crate::ai_state::{AiState, AiStateError, PendingDiff, DEFAULT_SPAWN_TIMEOUT};
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

/// Process-wide cache for the AI tool-schema registry. `defaults()`
/// parses a bundled JSON catalogue every call; `ai_plan` is called
/// interactively per user prompt, so we initialise once and serve
/// every subsequent call from the same immutable reference. The
/// registry is `Clone + Send + Sync` and has no per-service state,
/// so caching it as a static is safe.
static AI_TOOL_SCHEMAS: OnceLock<AiToolSchemaRegistry> = OnceLock::new();

/// Process-wide cache for the AI grammar registry. Same rationale as
/// [`AI_TOOL_SCHEMAS`] — the bundled GBNF blobs are parsed once and
/// shared as a read-only handle across every `ai_plan` invocation.
static AI_GRAMMARS: OnceLock<AiGrammarRegistry> = OnceLock::new();

fn ai_tool_schemas() -> &'static AiToolSchemaRegistry {
    AI_TOOL_SCHEMAS.get_or_init(AiToolSchemaRegistry::defaults)
}

fn ai_grammars() -> &'static AiGrammarRegistry {
    AI_GRAMMARS.get_or_init(AiGrammarRegistry::defaults)
}

/// Map an extension's `grammar_key` to the [`AiToolName`] that owns
/// it on the host side.
///
/// When multiple host tools declare the same `grammar_key` (today
/// `plan_detection` and `plan_to_wall` both declare
/// `grammar_key: "plan_detection"`), prefer the tool whose
/// wire-format name equals the `grammar_key` itself — the
/// "canonical home" for that grammar. Falls back to the first by
/// sorted name so the resolution is deterministic across calls if
/// no canonical home exists (which would indicate a catalogue bug,
/// guarded by [`AiToolSchemaRegistry::defaults_match_canonical_json`]).
///
/// Pure helper so it can be exercised by unit tests with synthetic
/// schema arrangements — see [`tests::canonical_builtin_for_grammar_key_*`].
fn canonical_builtin_for_grammar_key(
    grammar_key: &str,
    schemas: &AiToolSchemaRegistry,
) -> Option<AiToolName> {
    let mut matches = schemas
        .iter_sorted()
        .filter(|s| s.grammar_key == grammar_key);
    let first = matches.next()?;
    if first.name.as_str() == grammar_key {
        return Some(first.name);
    }
    Some(
        matches
            .find(|s| s.name.as_str() == grammar_key)
            .map_or(first.name, |s| s.name),
    )
}

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
    /// Input-validation failure at the bridge boundary — distinct
    /// from [`Self::Command`] (which is an `aec_command` validator)
    /// and [`Self::Bim`] (IFC parser). Used by
    /// [`BridgeService::bim_classify`] / [`BridgeService::bim_set_property`]
    /// for "unknown scheme", "empty pset", "entity not in project"
    /// etc., so the renderer can show a clean "invalid argument"
    /// toast without scraping a parser stack trace.
    #[error("invalid: {0}")]
    Invalid(String),
    /// Local-LLM (sidecar / planner / safety validator / diff engine)
    /// failure. The error message preserves
    /// [`crate::ai_state::AiStateError`]'s `Display` so the renderer can
    /// distinguish a spawn failure, a transport timeout, or a safety
    /// rejection from one another.
    #[error("ai: {0}")]
    Ai(String),
}

impl From<aec_assets::AssetError> for BridgeServiceError {
    fn from(e: aec_assets::AssetError) -> Self {
        Self::Asset(e.to_string())
    }
}

impl From<AiStateError> for BridgeServiceError {
    fn from(e: AiStateError) -> Self {
        Self::Ai(e.to_string())
    }
}

impl From<aec_ai::PlanError> for BridgeServiceError {
    fn from(e: aec_ai::PlanError) -> Self {
        Self::Ai(e.to_string())
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

/// On-disk thumbnail blob for a project, returned by
/// [`BridgeService::project_get_thumbnail`].
///
/// Phase 17 Group B Task 12. The PNG bytes are the same as what the
/// renderer captured via `captureThumbnailPng` — already encoded by
/// the time they hit the bridge. The `width` / `height` fields
/// mirror the captured dimensions so the renderer can hint its
/// `<img>` element with `width` / `height` attributes and avoid
/// layout shift before the image decodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectThumbnail {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// ISO-8601 timestamp from when the thumbnail was written.
    pub updated_at: String,
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
    /// Path to the `.aecstudio` project package. When supplied, the
    /// bridge opens the project's encrypted DB and builds a real
    /// `DeliverPackContext` so the pack carries actual project
    /// content — renders from `<project>/renders/`, schedules built
    /// from the project graph, sheets serialised to real PDFs, an
    /// IFC string from the graph, and a floor-plan SVG. When `None`
    /// the bridge falls back to an empty `DeliverPackContext` for
    /// backward compatibility with callers that don't have an open
    /// project (e.g. early renderer code paths that exported before
    /// Phase 13 wired the active-project tracker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
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

/// Result of a successful [`BridgeService::project_export_package`]
/// call. Mirrors the shape of the renderer's
/// `BridgeBackend.projectExportPackage` return value
/// (`{ outPath: string }`) with extras for the file count + payload
/// bytes that the renderer's "Exported NNN files (MM MB)" status
/// pane can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectExportPackageResult {
    pub out_path: String,
    /// Number of source files included in the archive. Excludes the
    /// auto-generated `_aec_archive_manifest.json` so this matches
    /// the count the renderer's "Files: N" indicator shows.
    pub entries: u32,
    /// Sum of source-file payload bytes (manifest excluded). Useful
    /// for the renderer's "Exported NNN MB" progress indicator.
    pub total_bytes: u64,
}

/// Result of a successful [`BridgeService::bim_classify`] call.
///
/// `classified` is the number of entities that received an updated
/// classification. `scheme` echoes the canonical scheme name back to
/// the renderer so the UI status pane (`Bim.tsx`) can confirm which
/// table was used. `details` carries per-entity assignments so the
/// renderer can populate the property panel without a follow-up
/// query.
///
/// **Counter semantics** (`classified` / `unchanged` / `skipped`):
///
/// * `classified` — entities whose database row was actually mutated
///   by this call (a `kind` rewrite for IFC, a new or modified
///   `components` row for Uniformat-II / OmniClass-21). This is the
///   number the renderer should use to gate "Undo classify?" prompts
///   or "N entities re-classified" toasts: it reflects real change.
/// * `unchanged` — entities that matched the scheme's lookup table
///   but whose database row already carried the target value, so no
///   write was issued. Surfacing this separately lets the renderer
///   distinguish "already-classified project, no-op rerun" from
///   "nothing matched at all".
/// * `skipped` — entities whose `kind` is not recognised by the
///   scheme's lookup table at all (e.g. a custom `kind` no scheme
///   maps). The renderer can warn about these.
///
/// `classified + unchanged + skipped == total_entities_walked` is the
/// invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimClassifyResult {
    pub scheme: String,
    pub classified: u32,
    pub unchanged: u32,
    pub skipped: u32,
    pub details: Vec<BimClassifyAssignment>,
}

/// One row of the [`BimClassifyResult::details`] vector. Mirrors the
/// renderer's `BimClassifyAssignment` interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimClassifyAssignment {
    pub entity_id: String,
    /// For `ifc` scheme — the assigned IFC class name (`"IfcWall"`,
    /// `"IfcDoor"`, etc.). For other schemes — the canonical code
    /// (`"B2010"`, `"21-02 20 10"`).
    pub code: String,
    /// Human-readable description from the table. Empty string for
    /// schemes where the code itself is descriptive enough.
    pub title: String,
}

/// Result of a successful [`BridgeService::bim_set_property`] call.
/// Echoes back the entity / pset / key the property landed on plus
/// the **previous** value (if any) so the renderer's undo gesture
/// has the data it needs without a follow-up query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimSetPropertyResult {
    pub entity_id: String,
    pub pset: String,
    pub key: String,
    pub previous_value: Option<String>,
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

/// Rows read back from a previously-written schedule XLSX via
/// [`BridgeService::bim_read_schedule_rows`]. Mirror of
/// `BimScheduleRows` in `apps/desktop/electron/bridge.ts` — kept
/// shape-stable so the renderer's `ScheduleView` can render the
/// table from a single round-trip:
/// `bim_generate_schedule(...)` → `bim_read_schedule_rows(...)` →
/// `rows`.
///
/// `header` is the ordered list of column display names from row 0
/// of the worksheet; `rows` is one `BTreeMap<header_name, cell>`
/// per body row. The map is alphabetized by `BTreeMap` semantics,
/// so the renderer iterates `header` for column order and indexes
/// the row map by name. Cell values are always strings — the
/// writer in [`aec_bim::schedules::xlsx::write_into`] uses
/// `write_string_with_format`, so round-tripping back to strings is
/// loss-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BimScheduleRows {
    /// Column display names in the order they appear in the
    /// worksheet header row. Useful for the renderer to lay out
    /// table columns; the row maps themselves are keyed by these
    /// same names.
    pub header: Vec<String>,
    /// One entry per non-empty body row in the worksheet. Every
    /// map has exactly `header.len()` entries, with empty cells
    /// stored as `""` (never absent) so the renderer can iterate
    /// the columns uniformly.
    pub rows: Vec<std::collections::BTreeMap<String, String>>,
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
///   `AssetMetadata::tags` (AND, not OR). Pushed into SQL as one
///   `AND EXISTS (SELECT 1 FROM json_each(assets.tags) ...)` clause
///   per tag inside [`aec_assets::db::AssetDatabase::query`] so the
///   `LIMIT` composes correctly (filter first, slice last).
/// * `style_tags` — same AND semantics as `tags`, against the
///   `style_tags` column, also pushed into SQL via `json_each`.
/// * `limit` — caps the JS-side result list. Defaults to **24** to
///   match the renderer's grid-page size (4 columns × 6 rows). The
///   `aec_assets::AssetQuery::limit` default is 200 (the
///   library-import default); the bridge tightens it because the
///   renderer paginates the browser UI. Saturating-clamped to
///   [`DESIGN_LIST_ASSETS_MAX_LIMIT`] (10_000) inside
///   [`BridgeService::design_list_assets`] so an upstream renderer
///   bug — including a JS negative number that wraps to a near-
///   `u32::MAX` value through napi's `ToUint32()` coercion — can't
///   force the SQLite call to materialise an unbounded result set.
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
    /// [`DESIGN_LIST_ASSETS_MAX_LIMIT`] inside
    /// [`BridgeService::design_list_assets`] so even a JS negative
    /// number that wraps to ~`u32::MAX` through napi's `ToUint32()`
    /// coercion can't force an unbounded SQLite materialisation.
    /// `None` falls through to the bridge default (24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Default JS-side asset-browser page size. Matches the renderer's
/// `bridge.ts` `filterAssets()` default (4 columns × 6 rows = 24).
pub(crate) const DESIGN_LIST_ASSETS_DEFAULT_LIMIT: u32 = 24;

/// Hard upper bound on a single `design_list_assets` page size, used
/// to back up the saturating-clamp promise on [`AssetListQuery::limit`].
/// 10_000 is well above any plausible renderer use (the asset browser
/// paginates at 24 per page; a power user scrolling "show all" still
/// stays in the low thousands for any human-curated library) while
/// also being small enough that the worst-case `AssetSummary`
/// allocation stays bounded.
pub(crate) const DESIGN_LIST_ASSETS_MAX_LIMIT: u32 = 10_000;

/// Hard upper bound on a single `design_list_materials` page size.
/// 10_000 is well above any plausible material library (the bundled
/// starter pack ships 8; even a fully populated industry library —
/// IKEA + Muuto + Vitra combined — runs in the hundreds). Mirroring
/// [`DESIGN_LIST_ASSETS_MAX_LIMIT`]'s saturating-clamp pattern so an
/// upstream renderer bug sending a JS negative number can't force
/// the in-process library to materialise an unbounded result set.
pub(crate) const DESIGN_LIST_MATERIALS_MAX_LIMIT: u32 = 10_000;

/// Validate every populated field in a [`MaterialUpdate`] before the
/// in-place mutation runs in [`BridgeService::design_update_material`].
///
/// Returns [`BridgeServiceError::Invalid`] with a human-readable
/// message naming the offending field on the first violation —
/// callers re-call after the user corrects the slider, so we don't
/// need to aggregate errors. See the per-field range rationale in
/// the [`BridgeService::design_update_material`] doc comment.
fn validate_material_update(update: &MaterialUpdate) -> Result<(), BridgeServiceError> {
    fn check_unit(name: &str, v: f32) -> Result<(), BridgeServiceError> {
        if !(0.0..=1.0).contains(&v) || !v.is_finite() {
            return Err(BridgeServiceError::Invalid(format!(
                "{name} must be in [0.0, 1.0]; got {v}"
            )));
        }
        Ok(())
    }
    fn check_rgb(name: &str, v: [f32; 3]) -> Result<(), BridgeServiceError> {
        for (i, c) in v.iter().enumerate() {
            if !(0.0..=1.0).contains(c) || !c.is_finite() {
                return Err(BridgeServiceError::Invalid(format!(
                    "{name}[{i}] must be in [0.0, 1.0]; got {c}"
                )));
            }
        }
        Ok(())
    }
    if let Some(v) = update.metallic {
        check_unit("metallic", v)?;
    }
    if let Some(v) = update.roughness {
        check_unit("roughness", v)?;
    }
    if let Some(v) = update.transmission {
        check_unit("transmission", v)?;
    }
    if let Some(v) = update.ior {
        if !(1.0..=5.0).contains(&v) || !v.is_finite() {
            return Err(BridgeServiceError::Invalid(format!(
                "ior must be in [1.0, 5.0]; got {v}"
            )));
        }
    }
    if let Some(v) = update.albedo {
        check_rgb("albedo", v)?;
    }
    if let Some(v) = update.emissive {
        check_rgb("emissive", v)?;
    }
    Ok(())
}

/// Renderer-facing projection of [`aec_materials::material::PbrMaterial`].
/// Shape pinned by the TypeScript `MaterialSummary` interface in
/// `apps/desktop/electron/bridge.ts` so the napi layer can hand the
/// value to the design-mode `MaterialPanel` without a transform step.
///
/// `albedo` / `emissive` are kept as `[f32; 3]` (linear RGB, each
/// component in `[0.0, 1.0]`) rather than a flattened CSS string so
/// the renderer can compute the PBR-style sphere thumbnail directly
/// from the channel values — pre-stringifying here would force the
/// renderer to parse the CSS form back into floats for the swatch
/// shading math.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialSummary {
    pub material_id: String,
    pub name: String,
    /// Linear-space RGB albedo, each component in `[0.0, 1.0]`.
    pub albedo: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub ior: f32,
    pub transmission: f32,
    /// Linear-space emissive RGB (additive radiance). `[0, 0, 0]` for
    /// non-emissive materials — the most common case.
    pub emissive: [f32; 3],
    pub style_tags: Vec<String>,
    pub tags: Vec<String>,
}

fn pbr_to_summary(m: &aec_materials::material::PbrMaterial) -> MaterialSummary {
    MaterialSummary {
        material_id: m.id.clone(),
        name: m.name.clone(),
        albedo: m.albedo,
        metallic: m.metallic,
        roughness: m.roughness,
        ior: m.ior,
        transmission: m.transmission,
        emissive: m.emissive,
        style_tags: m.style_tags.clone(),
        tags: m.tags.clone(),
    }
}

/// Query parameters for [`BridgeService::design_list_materials`]. All
/// fields are optional; an empty query returns the full library
/// sorted by display name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterialListQuery {
    /// Case-insensitive substring match against `name`. Empty /
    /// `None` falls through to "no filter".
    pub search: Option<String>,
    /// AND-matched against `PbrMaterial::style_tags`. Empty falls
    /// through to "no filter".
    pub style_tags: Vec<String>,
    /// AND-matched against `PbrMaterial::tags`.
    pub tags: Vec<String>,
    /// Cap on result-set size. `None` returns everything.
    pub limit: Option<u32>,
}

/// Patch payload for [`BridgeService::design_update_material`]. Only
/// the fields the design-mode inspector exposes are editable —
/// texture maps, AO, vendor metadata, and the id itself are
/// read-only at this layer because they're set when the library is
/// authored (asset-pack import / JSON load) rather than mutated
/// through the live UI.
///
/// Every field is `Option` so the inspector can `PATCH`-style send
/// only the slider that moved; unset fields keep their current
/// value. The bridge validates each field individually
/// (`metallic` / `roughness` / `transmission` must lie in
/// `[0.0, 1.0]`; `ior` must be `>= 1.0` because all real-world
/// transmissive materials sit on or above vacuum's `n = 1`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterialUpdate {
    pub albedo: Option<[f32; 3]>,
    pub metallic: Option<f32>,
    pub roughness: Option<f32>,
    pub ior: Option<f32>,
    pub transmission: Option<f32>,
    pub emissive: Option<[f32; 3]>,
}

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
    /// to a procedural placeholder card). The seed library ships
    /// `None` for all 4 demo assets — `aec_assets::pipeline` knows
    /// how to generate thumbnails on import, but the `design_list_*`
    /// napi surface deliberately does NOT base64-encode every
    /// thumbnail blob on each list call; the asset detail panel
    /// fetches the blob on demand instead.
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

/// Descriptor for one local AI tool surfaced by `ai_list_tools`.
///
/// Shape pinned by the TypeScript `AiTool` interface in
/// `apps/desktop/electron/bridge.ts`. We don't reuse [`AiToolSchema`]
/// directly because the renderer wants:
///   * scopes as a string array, not the enum
///   * child-tool names as strings, not the typed `ToolName`
///   * a stable camelCase wire shape (napi-rs serialises this struct
///     for `aiListTools`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiToolDescriptor {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub allowed_scopes: Vec<String>,
    pub max_entities_modified: u32,
    pub grammar_key: String,
    pub child_tools: Vec<String>,
}

impl From<&AiToolSchema> for AiToolDescriptor {
    fn from(s: &AiToolSchema) -> Self {
        Self {
            name: s.name.as_str().to_owned(),
            display_name: s.display_name.clone(),
            description: s.description.clone(),
            allowed_scopes: s
                .allowed_scopes
                .iter()
                .map(|sc| sc.as_str().to_owned())
                .collect(),
            max_entities_modified: s.max_entities_modified,
            grammar_key: s.grammar_key.clone(),
            child_tools: s
                .child_tools
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect(),
        }
    }
}

/// Result of [`BridgeService::ai_plan`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiPlanResult {
    /// Identifier of the pending diff. The renderer keeps this around
    /// and passes it to `ai_accept_diff` / `ai_reject_diff`.
    pub diff_id: String,
    /// The parsed JSON the model emitted, after grammar matching +
    /// safety validation. Same shape the in-process fallback returned.
    pub parsed: serde_json::Value,
    /// The tool that produced the diff. Surfacing it lets the renderer
    /// route to the right preview panel without re-deriving from the
    /// request.
    pub tool: String,
    /// Number of entities the planner reported the diff touches. The
    /// renderer pins this against its UX cap when deciding whether
    /// to render a per-entity diff list or a summarised count.
    pub entities_modified: u32,
}

/// Result of [`BridgeService::ai_accept_diff`].
///
/// Carries the full apply telemetry so the renderer can show the
/// user exactly what landed in the project graph: how many ops the
/// model proposed, how many were applied, how many were skipped
/// (and why), and the resulting per-op command ids for later undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAcceptOutcome {
    /// Mirrors the TS `{ accepted: true }` shape (always `true` on
    /// success; the call returns `Err` on failure).
    pub ok: bool,
    pub diff_id: String,
    /// Total operations the diff carried (matches
    /// `diff.operations.len()` from `ai_plan`).
    pub op_count: u32,
    /// Count of *operations* (not commands) that the converter
    /// mapped to at least one typed command. Bounded above by
    /// `op_count` and by definition `<= op_count`. The renderer
    /// surfaces this as "Applied X of Y operations" — Y is
    /// `op_count`, X is this field.
    ///
    /// Note: a single operation can expand into multiple commands
    /// (e.g. a polyline wall `Insert` with N points emits N-1
    /// `CreateWall` commands). `command_ids.len()` reflects the
    /// command count; `applied_count` reflects the operation count.
    /// The two can differ in either direction:
    ///   * one op → many commands (multi-segment polyline);
    ///   * one op → many `skipped` entries plus some commands (a
    ///     polyline that mixes valid segments with zero-length
    ///     duplicates lands its valid segments and records the
    ///     dupes — `applied_count` still increments by one for
    ///     that op).
    pub applied_count: u32,
    /// Operations the converter could not translate — unknown
    /// entity kinds, dangling targets, render_doctor diagnostics,
    /// material bindings without a target entity. Surfacing these
    /// lets the renderer show "Applied 4 of 5 — 1 skipped" instead
    /// of silently dropping a partial accept.
    pub skipped: Vec<AiAcceptSkippedJs>,
    /// `Command::command_id` of every applied command, in apply
    /// order. The renderer pins these so a later "Undo last AI
    /// action" call can pop the matching journal entries.
    pub command_ids: Vec<String>,
    /// Hash chain head of the AI audit log AFTER this accept was
    /// recorded. The renderer surfaces this in the AI panel's
    /// provenance tooltip; verification tools can walk the chain
    /// from genesis to this head.
    pub audit_chain_head: String,
}

/// One skipped operation surfaced from the `ai_apply` converter.
/// Mirrors [`aec_command::SkippedOperation`] but stays inside the
/// service crate so the napi layer doesn't need a direct dep on
/// `aec_command`'s internal types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAcceptSkippedJs {
    pub op_index: u32,
    pub reason: String,
}

/// Result of [`BridgeService::ai_reject_diff`].
///
/// Mirrors `AiAcceptOutcome` shape-wise (same `audit_chain_head`
/// field) so the renderer can use a single "diff lifecycle" toast
/// shape for both outcomes. `op_count` is reported so the renderer
/// can show "Rejected (4 ops, 0 applied)" symmetrically with the
/// accept variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRejectOutcome {
    pub ok: bool,
    pub diff_id: String,
    pub op_count: u32,
    /// Optional reason supplied by the renderer (`reason: "too
    /// many entities"`, `reason: "wrong room"`, etc.). Logged into
    /// the AI audit chain so the provenance UI can show *why* the
    /// user rejected the suggestion. `None` is recorded as an
    /// empty string in the audit envelope.
    pub reason: Option<String>,
    pub audit_chain_head: String,
}

/// Intermediate state produced by phases 1+2 of `ai_accept_diff`
/// (pre-commit + SQL commit). Holds the data the post-commit
/// phases need: the project root for the audit append, the scope
/// the user authored the plan under, the original diff for the
/// audit envelope, and the per-op outcome fields the final
/// `AiAcceptOutcome` will surface.
///
/// Crate-private — callers outside this module never see this
/// shape. Splitting it out is the structural piece of the
/// `BUG_0001 (round 3)` fix: it lets `ai_accept_diff` finalize the
/// pending-diff registry entry between the SQL commit (phase 2)
/// and the AI audit append (phase 4) so a failed audit cannot
/// leave the diff retry-pending after the graph has already been
/// mutated.
#[derive(Debug)]
struct AiAcceptCommitted {
    project_root: PathBuf,
    plan_scope: Scope,
    diff: aec_ai::Diff,
    op_count: u32,
    applied_count: u32,
    skipped: Vec<AiAcceptSkippedJs>,
    command_ids: Vec<String>,
}

/// Result of [`BridgeService::ai_cancel_job`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiCancelResult {
    pub cancelled: bool,
}

/// Live sidecar status snapshot for `ai_runtime_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRuntimeStatusReport {
    /// Lowercase variant of [`aec_ai::RuntimeState`]: one of
    /// `"idle" | "loading" | "ready" | "failed"`.
    pub state: String,
    pub last_error: Option<String>,
    /// Currently-tracked pending diff ids. Useful for the renderer's
    /// AI sidebar to re-render on reconnect.
    pub pending_diff_ids: Vec<String>,
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
    /// Optional directory holding installed extension packs. Each
    /// child directory must hold a `manifest.json` understood by
    /// [`aec_core::ExtensionLoader`]. When supplied, the bridge:
    ///
    /// * loads every manifest at boot,
    /// * builds a [`aec_core::PermissionEnforcer`] from the registry,
    /// * installs every asset-pack extension into the asset library
    ///   DB (so `design_list_assets` surfaces extension assets), and
    /// * routes [`BridgeService::project_create_from_template`] and
    ///   [`BridgeService::list_templates`] through
    ///   [`aec_core::TemplateLoader::load_with_extensions`] so
    ///   extension templates take precedence over shipped templates
    ///   with the same key.
    ///
    /// `None` (the default) preserves the pre-Phase-14 behaviour: no
    /// extensions, all bridge surfaces operate against the built-in
    /// registries.
    pub extensions_dir: Option<PathBuf>,
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
    /// Process-wide local-LLM state: the lifecycle state machine, the
    /// optional spawned `llama-server` child handle, and the map of
    /// pending diffs awaiting accept/reject.
    ///
    /// Held **by value** rather than behind an outer `Mutex` because
    /// [`AiState`] provides per-field interior mutability (an
    /// `RwLock` for lifecycle state, a `Mutex` for the spawn slot,
    /// and a `Mutex` for the diff registry). This means an in-flight
    /// `ai_plan` cold-spawn holds only the spawn-slot mutex —
    /// concurrent `ai_runtime_status` polls take the runtime
    /// `RwLock` *read* side and observe the published `Loading`
    /// state instantly, and `ai_accept_diff` / `ai_reject_diff`
    /// touch only the diff registry. See `crate::ai_state` module
    /// doc for the full concurrency rationale.
    ai_state: AiState,
    /// Process-wide KChat accounting state. In Phase 15 the
    /// in-process publisher is always an
    /// [`aec_core::InMemoryPublisher`] — the real publisher is the
    /// loopback HTTP API hosted by the Electron main process (see
    /// `apps/desktop/electron/kchat/kchatLocalApi.ts`). The state
    /// owns the per-project enable flag, the default-thread cache,
    /// and a `loopback_http` / `in_memory` publisher-kind marker
    /// the Electron host promotes via
    /// [`crate::kchat_state::KChatState::mark_loopback_active`].
    kchat_state: crate::kchat_state::KChatState,
    /// Real-time viewport service. Owns the wgpu device, the four
    /// core render pipelines, and the off-screen surface. See
    /// [`crate::viewport_service`] for the rationale around the
    /// "real device when available, fallback otherwise" pattern.
    viewport_service: crate::viewport_service::ViewportService,
    /// Loaded extension registry. Empty when
    /// [`BridgeConfig::extensions_dir`] is `None`. Populated once at
    /// boot — extensions are immutable at runtime in this revision
    /// (a future patch can add a `bridge.reloadExtensions()` IPC
    /// once the renderer needs hot-reload UX).
    extension_registry: aec_core::ExtensionRegistry,
    /// Permission enforcer derived from the loaded registry. Consulted
    /// by extension-aware code paths (AI tool dispatch, asset import,
    /// schedule registry, …) before routing a runtime operation to an
    /// extension. Empty enforcer when no extensions loaded; every
    /// permission check returns
    /// [`aec_core::PermissionCheck::Denied`] for an unknown
    /// extension id (the safe default).
    permission_enforcer: aec_core::PermissionEnforcer,
    /// Per-extension boot failures buffered during
    /// [`BridgeService::new`]. The bridge boot path is
    /// intentionally fault-tolerant — a broken extension cannot take
    /// the whole runtime offline — but those silent failures used to
    /// be invisible to the user. The renderer reads this vector
    /// through the `extension_load_diagnostics()` napi method ↔
    /// `extensions:listLoadDiagnostics` IPC and renders a read-only
    /// Settings card so the user knows which extension didn't load
    /// and why.
    ///
    /// Populated at boot from three sources and frozen for the
    /// lifetime of the service:
    /// 1. [`aec_core::ExtensionLoader::load_with_diagnostics`] — manifest read/parse/validate, signature, duplicate id, unsafe path.
    /// 2. [`aec_assets::install_asset_packs_collect_errors`] — asset-pack install failures (missing blob, blake3 mismatch, permission denied).
    /// 3. [`aec_ai::list_extension_ai_tools`] — AI-tool resolution failures (unknown scope, missing body, permission denied).
    ///
    /// Order is deterministic (sources are processed in the listed
    /// order; each source iterates the registry in `iter_sorted`
    /// order). Empty when there are no failures — the renderer hides
    /// the diagnostics card entirely in that case.
    extension_load_diagnostics: Vec<aec_core::ExtensionLoadDiagnostic>,
    /// Process-wide PBR material library backing the design-mode
    /// `MaterialPanel`. Phase 17 Group B Task 11 surfaced the library
    /// through `design_list_materials` / `design_update_material`
    /// so the renderer can present a live swatch grid with PBR
    /// preview thumbnails and editable sliders instead of the
    /// hardcoded 8-swatch placeholder it shipped with.
    ///
    /// The library is seeded from
    /// [`aec_materials::library::MaterialLibrary::with_default_pack`]
    /// at boot — the same starter pack the asset pipeline uses for
    /// new projects — and any subsequent
    /// [`Self::design_update_material`] call mutates the in-process
    /// copy. The library is intentionally a *bridge-wide* singleton
    /// rather than per-project state: in the present revision, the
    /// renderer owns one material panel that follows the active
    /// project, and a future per-project material override needs a
    /// dedicated schema (project-scoped overrides + library-scoped
    /// defaults) that we don't want to half-build here. When that
    /// lands, the field type becomes
    /// `HashMap<PathBuf, Mutex<MaterialLibrary>>` and the existing
    /// `design_list_materials` / `design_update_material` callers
    /// route through it — the napi surface stays the same.
    material_library: Mutex<aec_materials::library::MaterialLibrary>,
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

/// Extract the offending extension id from a typed
/// [`aec_ai::ExtensionAiToolError`]. All current variants carry an
/// `ext` field (the extension id), and exhaustive matching makes
/// adding a new variant a compile error here so we can't drop the id
/// silently if the error enum grows. Returns the bare `String`
/// (not `Option<String>`) because every current variant guarantees
/// the id is present — if a future variant lands that genuinely
/// can't name the extension, this signature must change in
/// lockstep with the matching arm so the diagnostic path stays
/// explicit.
fn ai_tool_error_ext_id(err: &aec_ai::ExtensionAiToolError) -> String {
    match err {
        aec_ai::ExtensionAiToolError::PermissionDenied { ext, .. }
        | aec_ai::ExtensionAiToolError::NotAnAiTool { ext }
        | aec_ai::ExtensionAiToolError::UnknownScope { ext, .. } => ext.clone(),
    }
}

/// Synthesise a deterministic `components.id` for an `aec/`-prefixed
/// overlay row from `(entity_id, component_kind)`, then upsert the
/// row's body — but only when the prior body (if any) differs from
/// the proposed body. Returns `Ok(true)` if a write was issued,
/// `Ok(false)` if the existing row already matched.
///
/// The deterministic `id` ensures repeated classification calls
/// update the same row instead of accumulating orphans. The "only
/// when changed" semantic exists so [`BridgeService::bim_classify`]
/// can report a `classified` counter that reflects real database
/// mutations — see [`BimClassifyResult`] for the contract. The
/// comparison is done on the serialised JSON form (`to_string()`),
/// which is what's stored, so any whitespace differences would
/// register as a change (serde_json's serialiser is deterministic
/// for the inputs we feed here).
///
/// Uniqueness on `(entity_id, kind)` is enforced by the v4 migration
/// (`v4_components_natural_key`) so the `ON CONFLICT(entity_id,
/// kind)` clause is guaranteed to match at most one row.
fn upsert_classification_component_if_changed(
    tx: &Transaction<'_>,
    entity_id: &str,
    component_kind: &str,
    body: &serde_json::Value,
) -> rusqlite::Result<bool> {
    let new_body = body.to_string();
    let existing: Option<String> = tx
        .query_row(
            "SELECT body FROM components WHERE entity_id = ?1 AND kind = ?2",
            params![entity_id, component_kind],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    if existing.as_deref() == Some(new_body.as_str()) {
        return Ok(false);
    }
    let comp_id = format!("comp_{}_{}", entity_id, component_kind.replace('/', "_"));
    tx.execute(
        "INSERT INTO components(id, entity_id, kind, body) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(entity_id, kind) DO UPDATE SET id = excluded.id, body = excluded.body",
        params![comp_id, entity_id, component_kind, new_body],
    )?;
    Ok(true)
}

impl BridgeService {
    pub fn new(config: BridgeConfig, master_key: [u8; 32]) -> Result<Self, BridgeServiceError> {
        std::fs::create_dir_all(&config.state_dir)?;
        std::fs::create_dir_all(&config.projects_dir)?;
        let recents =
            RecentsStore::open(config.state_dir.join("recents.json"), config.max_recents)?;
        let asset_state = AssetState::new(&config.state_dir);

        // ---- Extension loading ----
        //
        // When the caller pointed us at an `extensions_dir`, walk it
        // through `ExtensionLoader`, build the
        // `PermissionEnforcer` from the resulting registry, and feed
        // every asset-pack extension into the asset library DB so
        // extension-shipped assets show up alongside the seed library
        // on the next `design_list_assets` query.
        //
        // `LoadOptions::allow_unsigned()` is intentional for the
        // bridge boot path — the trust store lives one layer up (the
        // Electron host injects publisher keys before bridge init in
        // production, and the integration tests in this crate ship
        // unsigned manifests). Production builds tighten this by
        // passing a populated `TrustStore` through a future
        // `extensions_trust` field on `BridgeConfig`.
        //
        // Errors during extension loading or asset-pack install do
        // NOT crash the bridge: an unparseable / missing manifest in
        // an extension dir would otherwise block the whole renderer
        // from booting. Instead we degrade to an empty registry plus
        // a buffered [`ExtensionLoadDiagnostic`] entry per failure
        // — see `extension_load_diagnostics` on `BridgeService` for
        // the renderer surfacing.
        let mut extension_load_diagnostics: Vec<aec_core::ExtensionLoadDiagnostic> = Vec::new();
        let (extension_registry, permission_enforcer) = match config.extensions_dir.as_ref() {
            Some(dir) if dir.is_dir() => {
                let loader = aec_core::ExtensionLoader::new(dir);
                let (registry, loader_diags) = loader
                    .load_with_diagnostics(&aec_core::LoadOptions::allow_unsigned())
                    .unwrap_or_else(|err| {
                        // A top-level loader error (root dir
                        // disappeared mid-enumeration, permission
                        // denied on the dir itself) is rare; surface
                        // it as a single ManifestRead diagnostic and
                        // continue with an empty registry.
                        (
                            aec_core::ExtensionRegistry::default(),
                            vec![aec_core::ExtensionLoadDiagnostic::new(
                                None,
                                dir.clone(),
                                aec_core::ExtensionLoadStage::ManifestRead,
                                err.to_string(),
                            )],
                        )
                    });
                extension_load_diagnostics.extend(loader_diags);
                let enforcer = aec_core::PermissionEnforcer::from_registry(&registry);
                // Run the asset-pack host once at boot. Any
                // per-extension failure (missing file, blake3
                // mismatch, permission denied) is captured into
                // `extension_load_diagnostics` but does NOT fail the
                // bridge boot — the asset rows simply don't show up,
                // which is the same UX the user gets when the
                // extension is uninstalled. The Settings diagnostics
                // card surfaces the full list to the user.
                if registry
                    .iter()
                    .any(|e| matches!(e.manifest.kind, aec_core::ExtensionType::AssetPack))
                {
                    // `with_db_mut` returns `Result<_, AssetError>`
                    // around our inner `(InstallSummary,
                    // Vec<(id, path, AssetExtensionError)>)`. The
                    // outer error happens when the asset DB itself
                    // is unreachable — captured as a single
                    // `AssetPackInstall` diagnostic. Per-extension
                    // errors are unpacked from the inner vector.
                    let install_result = asset_state.with_db_mut(|db| {
                        Ok(aec_assets::install_asset_packs_collect_errors(
                            db, &registry, &enforcer,
                        ))
                    });
                    match install_result {
                        Ok((_summary, per_ext_errs)) => {
                            for (ext_id, ext_path, err) in per_ext_errs {
                                extension_load_diagnostics.push(
                                    aec_core::ExtensionLoadDiagnostic::new(
                                        Some(ext_id),
                                        ext_path,
                                        aec_core::ExtensionLoadStage::AssetPackInstall,
                                        err.to_string(),
                                    ),
                                );
                            }
                        }
                        Err(db_err) => {
                            extension_load_diagnostics.push(
                                aec_core::ExtensionLoadDiagnostic::new(
                                    None,
                                    dir.clone(),
                                    aec_core::ExtensionLoadStage::AssetPackInstall,
                                    db_err.to_string(),
                                ),
                            );
                        }
                    }
                }
                // Run the AI-tool resolver once at boot to surface
                // resolution failures (unknown scope, missing body,
                // permission denied) before the renderer ever calls
                // `ai_list_tools`. Tool dispatch goes through the
                // registry lazily, so without this pre-walk a broken
                // AI-tool extension would only show up the first
                // time the user opened the AI sidebar.
                if registry
                    .iter()
                    .any(|e| matches!(e.manifest.kind, aec_core::ExtensionType::AiTool))
                {
                    let (_ok, errs) = aec_ai::list_extension_ai_tools(&registry, &enforcer);
                    for err in errs {
                        // The error carries the offending ext id; we
                        // re-resolve the on-disk path through the
                        // registry so the renderer can show the user
                        // exactly which extension directory to look at.
                        let ext_id = ai_tool_error_ext_id(&err);
                        let path = registry
                            .get(&aec_core::ExtensionId(ext_id.clone()))
                            .map_or_else(|| dir.clone(), |e| e.root.clone());
                        extension_load_diagnostics.push(aec_core::ExtensionLoadDiagnostic::new(
                            Some(ext_id),
                            path,
                            aec_core::ExtensionLoadStage::AiToolResolution,
                            err.to_string(),
                        ));
                    }
                }
                (registry, enforcer)
            }
            _ => (
                aec_core::ExtensionRegistry::default(),
                aec_core::PermissionEnforcer::new(),
            ),
        };

        Ok(Self {
            config,
            recents,
            master_key,
            engine_status_cache: EngineStatusCache::new(),
            snapshot_cache: SnapshotCache::new(),
            render_state: Mutex::new(RenderState::new()),
            asset_state,
            ai_state: AiState::new(default_ai_runtime_config()),
            kchat_state: crate::kchat_state::KChatState::new(),
            viewport_service: crate::viewport_service::ViewportService::new(),
            extension_registry,
            permission_enforcer,
            extension_load_diagnostics,
            material_library: Mutex::new(
                aec_materials::library::MaterialLibrary::with_default_pack(),
            ),
        })
    }

    /// Borrow the process-wide [`KChatState`]. Used by tests that
    /// need to install a mock publisher / socket fixture before
    /// driving the bridge through the public KChat endpoints.
    #[doc(hidden)]
    pub fn __kchat_state(&self) -> &crate::kchat_state::KChatState {
        &self.kchat_state
    }

    /// Borrow the buffered extension load diagnostics that were
    /// captured during [`Self::new`]. The renderer reaches this slice
    /// through the `extension_load_diagnostics()` napi method, which
    /// converts each entry to the JS-friendly wire shape consumed by
    /// the `extensions:listLoadDiagnostics` IPC.
    ///
    /// Returns an empty slice when no extensions failed to load —
    /// callers can use that as the signal to hide the Settings
    /// diagnostics card entirely.
    pub fn extension_load_diagnostics(&self) -> &[aec_core::ExtensionLoadDiagnostic] {
        &self.extension_load_diagnostics
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
        // Walk the union of shipped templates + extension-supplied
        // templates. `discover_with_extensions` returns extension
        // keys at the head of the list when they collide with a
        // shipped key — the subsequent `load_with_extensions` honours
        // that precedence by reading the extension's
        // `definition_path` first.
        for key in loader
            .discover_with_extensions(&self.extension_registry)
            .map_err(|e| BridgeServiceError::Template(e.to_string()))?
        {
            match loader.load_with_extensions(&self.extension_registry, &key) {
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
        // Consult the extension registry before the shipped templates
        // tree so an extension-supplied template with the same key
        // wins. Falls back to the on-disk `templates/` tree when the
        // registry has no matching `ExtensionType::Template` entry.
        // Mirrors the merge order documented on
        // [`aec_core::TemplateLoader::load_with_extensions`].
        let template = loader
            .load_with_extensions(&self.extension_registry, template_key)
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

        // Instantiate the template into real entities on the project
        // graph. Geometry templates (apartment / villa / office / ...)
        // emit a batch of `CreateWall` + `CreateFloor` + `CreateCeiling`
        // + `CreateRoom` + `SetLighting` + `SaveCamera` commands; the
        // batch lands in a single SQL transaction via
        // `execute_persistent_batch` so a half-failed instantiation
        // never leaves the project graph in a partial state. Sheet-only
        // / layer-only templates (drafting, renovation overlay) emit
        // zero commands here — their content is realised by other
        // mode-specific seed steps.
        //
        // The skipped-rooms diagnostic from
        // `template_to_commands` is captured in the project's audit
        // sidecar `<root>/audit/template_instantiation.json` so a
        // user / reviewer can see exactly which rooms (if any) the
        // instantiator declined to materialise. The file is written
        // even on a clean batch (with `skipped: []`) so its presence
        // is a positive signal: "this project went through the real
        // template path, not a no-op fast path".
        let outcome = aec_command::template_apply::template_to_commands(&template);
        if let Err(e) = apply_template_outcome(&pkg, &self.master_key, template_key, outcome) {
            // Roll back the half-created project so the next call to
            // `project_create_from_template` with the same name isn't
            // blocked by an `AlreadyExists` error against a useless
            // shell.
            let _ = std::fs::remove_dir_all(&root);
            return Err(e);
        }

        // Drop any stale cache entry for this path before publishing
        // the new project to the recents store. `root` is a `PathBuf`
        // and the cache key is derived via `cache_key` (which goes
        // through `canonicalize`); pass the str form through the
        // standard helper so it shares the same canonicalisation
        // failure handling as the other mutating endpoints.
        let root_str = root.to_string_lossy();
        self.invalidate_status_cache_for(&root_str);
        // Adopt the fresh project's KChat config (typically `None`
        // for a template-created project — templates don't carry a
        // thread id) so a subsequent `kchat:status` poll reflects
        // the just-created project rather than the previously-open
        // one's stale thread. The renderer will issue its usual
        // `project_open` follow-up, but applying here keeps the
        // bridge consistent the moment creation succeeds.
        self.apply_kchat_project_config(pkg.manifest().settings.kchat.as_ref());
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
        // Adopt the just-opened project's KChat config so the
        // renderer's `kchat:status` poll surfaces this project's
        // `default_thread_id` and `enabled` flag on the very next
        // tick. When the manifest omits the field we clear the
        // bridge-side cache so the Deliver page's review panel
        // falls back to the publisher-side default thread instead
        // of pointing at the previously-open project.
        self.apply_kchat_project_config(pkg.manifest().settings.kchat.as_ref());
        let core_summary = pkg.summary();
        let summary: ProjectSummary = core_summary.clone().into();
        self.recents.record(&core_summary)?;
        Ok(summary)
    }

    /// Apply (or clear) the per-project KChat configuration on the
    /// bridge-wide [`crate::kchat_state::KChatState`]. Called from
    /// every endpoint that opens or re-opens a project so the
    /// renderer's `kchat:status` poll mirrors the on-disk manifest.
    fn apply_kchat_project_config(&self, config: Option<&aec_core::kchat_config::KChatConfig>) {
        match config {
            Some(c) => self.kchat_state.apply_project_config(c),
            None => self.kchat_state.clear_project_config(),
        }
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
        // Mirror the saved manifest's KChat config onto the
        // bridge-wide state so a save that toggles `enabled` or
        // changes `default_thread_id` is reflected on the next
        // `kchat:status` poll without forcing a project re-open.
        self.apply_kchat_project_config(pkg.manifest().settings.kchat.as_ref());
        Ok(pkg.summary().into())
    }

    /// Persist a PNG-encoded thumbnail for a project, written as the
    /// singleton row in the SQLCipher `project_thumbnail` table.
    ///
    /// Phase 17 Group B Task 12 — every save updates this so the Home
    /// page's recent-project grid shows the actual viewport state for
    /// each project rather than a placeholder gradient. The bridge
    /// stays format-agnostic: the renderer captures the viewport's
    /// current canvas as PNG bytes (the simplest reproducible
    /// snapshot — see the renderer's `captureThumbnailPng` helper)
    /// and hands us the encoded buffer plus its dimensions. We
    /// validate (non-empty, PNG magic bytes, dimensions in range)
    /// and `INSERT OR REPLACE` so the row gets overwritten in place.
    ///
    /// The `CHECK (singleton = 1)` constraint in the v5 migration
    /// keeps the table to at most one row per project, which lets us
    /// avoid a "delete-then-insert" race window where a reader could
    /// observe a missing thumbnail mid-update.
    pub fn project_set_thumbnail(
        &mut self,
        path: &str,
        png_bytes: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), BridgeServiceError> {
        if png_bytes.is_empty() {
            return Err(BridgeServiceError::Invalid(
                "project_set_thumbnail: PNG buffer is empty".into(),
            ));
        }
        // PNG magic header — `89 50 4E 47 0D 0A 1A 0A`. Reject early
        // so the renderer sees a typed error rather than a cryptic
        // SQL constraint failure on read.
        const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
        if png_bytes.len() < PNG_MAGIC.len() || &png_bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
            return Err(BridgeServiceError::Invalid(
                "project_set_thumbnail: buffer does not start with PNG magic header".into(),
            ));
        }
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(BridgeServiceError::Invalid(format!(
                "project_set_thumbnail: dimensions out of range (1..=4096); \
                 got {width}×{height}",
            )));
        }
        // Cap the stored blob at 1 MiB to keep the project file size
        // sane — a 256×192 PNG of a typical interior is ~30–80 KiB,
        // so 1 MiB is ~20× the expected size and only an unusually
        // verbose source would brush against it.
        const MAX_THUMBNAIL_BYTES: usize = 1024 * 1024;
        if png_bytes.len() > MAX_THUMBNAIL_BYTES {
            return Err(BridgeServiceError::Invalid(format!(
                "project_set_thumbnail: PNG buffer is {} bytes; max is {} bytes",
                png_bytes.len(),
                MAX_THUMBNAIL_BYTES,
            )));
        }
        let (_pkg, conn) =
            ProjectPackage::open_with_master_key_and_database(path, &self.master_key)?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO project_thumbnail \
             (singleton, png, width, height, updated_at) \
             VALUES (1, ?1, ?2, ?3, ?4)",
            rusqlite::params![png_bytes, width as i64, height as i64, now],
        )?;
        Ok(())
    }

    /// Read the persisted thumbnail PNG bytes for a project, or `None`
    /// when no thumbnail has been written yet.
    ///
    /// `&self` — read-only at the manifest level, but the SQLCipher
    /// open routes through [`ProjectPackage::open_database`] which
    /// runs the migration registry. A legacy v4 project that has
    /// never had a thumbnail saved will return `None` after the
    /// migration creates the (empty) `project_thumbnail` table.
    pub fn project_get_thumbnail(
        &self,
        path: &str,
    ) -> Result<Option<ProjectThumbnail>, BridgeServiceError> {
        let pkg = ProjectPackage::open(path)?;
        let conn = pkg.open_database(&self.master_key)?;
        let result: rusqlite::Result<(Vec<u8>, i64, i64, String)> = conn.query_row(
            "SELECT png, width, height, updated_at \
             FROM project_thumbnail \
             WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        );
        match result {
            Ok((png, width, height, updated_at)) => Ok(Some(ProjectThumbnail {
                png,
                width: width as u32,
                height: height as u32,
                updated_at,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(BridgeServiceError::Core(e.to_string())),
        }
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
        self.command_apply_on_conn(project_path, &mut conn, command)
    }

    /// Internal helper: apply a single command against an already-open
    /// SQLCipher connection. Used by `command_apply` (which opens the
    /// connection itself) and by callers that need to share a
    /// connection across multiple steps (e.g.
    /// [`Self::deliver_create_revision`], which journals the
    /// `CreateRevision` command *and* enumerates the on-disk graph
    /// for the revision snapshot without re-opening the project).
    fn command_apply_on_conn(
        &mut self,
        project_path: &str,
        conn: &mut rusqlite::Connection,
        command: Command,
    ) -> Result<CommandApplyResult, BridgeServiceError> {
        let mut engine = CommandEngine::open(&*conn, command.scope)?;
        let result = engine.execute_persistent(command, conn)?;
        self.invalidate_status_cache_for(project_path);
        Ok(CommandApplyResult {
            command_id: result.command_id,
            applied: result.applied,
            undo_len: engine.undo_len() as u32,
            redo_len: engine.redo_len() as u32,
        })
    }

    /// Apply a sequence of commands as a single atomic batch.
    ///
    /// Opens the project package once, opens one [`CommandEngine`]
    /// for the batch's shared scope (every command must agree on
    /// `Command::scope`), and routes the whole sequence through
    /// [`CommandEngine::execute_persistent_batch`] so a multi-thousand
    /// command DXF / IFC ingest collapses to a single SQL transaction,
    /// one engine open, and one status-cache invalidation rather than
    /// N of each.
    ///
    /// Returns the per-command [`CommandApplyResult`]s in input
    /// order, matching what N back-to-back `command_apply` calls
    /// would have produced (minus the per-command undo/redo length
    /// snapshot — those reflect the post-batch state on every entry,
    /// which is what every current caller wants).
    pub fn command_apply_batch(
        &mut self,
        project_path: &str,
        commands: Vec<Command>,
    ) -> Result<Vec<CommandApplyResult>, BridgeServiceError> {
        if commands.is_empty() {
            return Ok(Vec::new());
        }
        // Every command in the batch must agree on scope so a single
        // engine can validate + persist them. Mixed-scope batches
        // are rejected here rather than producing a confusing
        // engine-level scope-mismatch error half-way through.
        let scope = commands[0].scope;
        for cmd in &commands {
            if cmd.scope != scope {
                return Err(BridgeServiceError::Command(format!(
                    "command_apply_batch: mixed-scope batch ({scope:?} vs {:?})",
                    cmd.scope,
                )));
            }
        }
        let (_pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let mut engine = CommandEngine::open(&conn, scope)?;
        let results = engine.execute_persistent_batch(commands, &mut conn)?;
        self.invalidate_status_cache_for(project_path);
        let undo_len = engine.undo_len() as u32;
        let redo_len = engine.redo_len() as u32;
        Ok(results
            .into_iter()
            .map(|r| CommandApplyResult {
                command_id: r.command_id,
                applied: r.applied,
                undo_len,
                redo_len,
            })
            .collect())
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

    /// Verify the BLAKE3 chain integrity of every audit log under
    /// `<project>/audit/` (all `.jsonl` files). Walks the files in
    /// lexicographic order, recomputes each entry's `hash` from its
    /// immutable fields, and reports the first broken link.
    ///
    /// Read-only — does not modify the project package or the SQL
    /// mirror, and does not touch the engine-status cache. Safe to
    /// run concurrently with reads.
    pub fn project_audit_verify(
        &self,
        path: &str,
    ) -> Result<aec_audit::ChainVerification, BridgeServiceError> {
        // Read-only — `ProjectPackage::open` only reads `manifest.json`
        // and validates the package layout (including the presence of
        // the `audit/` subdirectory). It does NOT touch the SQLCipher
        // DB, so no master-key unwrap is required for chain
        // verification (the audit log itself is plaintext JSONL by
        // design — its integrity is protected by the BLAKE3 chain,
        // not by encryption).
        let pkg = ProjectPackage::open(path)?;
        let dir = pkg.root().join("audit");
        let verification = aec_audit::verify_chain(&dir)?;
        Ok(verification)
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
    ///   [`DESIGN_LIST_ASSETS_MAX_LIMIT`] (10_000) to defend against
    ///   an upstream renderer bug sending a JS negative number that
    ///   wraps to a near-`u32::MAX` value through napi's
    ///   `ToUint32()` coercion — without the clamp such a value
    ///   would reach SQLite as `LIMIT 4294967295` and materialise an
    ///   unbounded result set on a real (multi-thousand-row) asset
    ///   library.
    /// * `AssetMetadata::vendor.name` → `AssetSummary::vendor`. An
    ///   empty vendor display string is mapped to `None` so the JS
    ///   side sees a missing vendor field rather than an empty
    ///   string (UX: no "by " line on the card vs " by ").
    /// * `thumbnail_data_uri` is always `None` on the list surface —
    ///   the schema carries `thumbnail_hash` and the blob lives in
    ///   `asset_blobs`, but base64-encoding every thumbnail on each
    ///   list call is wasteful, so the asset detail panel fetches
    ///   the blob on demand instead.
    pub fn design_list_assets(
        &self,
        query: &AssetListQuery,
    ) -> Result<Vec<AssetSummary>, BridgeServiceError> {
        let limit = query
            .limit
            .unwrap_or(DESIGN_LIST_ASSETS_DEFAULT_LIMIT)
            .min(DESIGN_LIST_ASSETS_MAX_LIMIT);
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

    /// List PBR materials from the in-process
    /// [`aec_materials::library::MaterialLibrary`] for the design-mode
    /// `MaterialPanel`. Read-only; takes the library mutex just long
    /// enough to filter + clone the matching summaries, then drops
    /// it before returning.
    ///
    /// Filter semantics mirror [`aec_materials::library::MaterialQuery`]:
    ///
    /// * `query.search` → ASCII case-insensitive substring on
    ///   `name`. Empty / missing falls through to "no filter".
    /// * `query.style_tags` → AND-matched against
    ///   `PbrMaterial::style_tags`. Empty falls through to "no filter".
    /// * `query.tags` → AND-matched against `PbrMaterial::tags`.
    /// * `query.limit` → saturating-clamped to
    ///   [`DESIGN_LIST_MATERIALS_MAX_LIMIT`] (10_000) — well above any
    ///   plausible material library — to defend against an upstream
    ///   renderer bug sending a JS negative number that wraps to a
    ///   near-`u32::MAX` value through napi's `ToUint32()` coercion.
    pub fn design_list_materials(
        &self,
        query: &MaterialListQuery,
    ) -> Result<Vec<MaterialSummary>, BridgeServiceError> {
        let lib = self
            .material_library
            .lock()
            .map_err(|_| BridgeServiceError::Core("material_library mutex poisoned".to_string()))?;
        let q = aec_materials::library::MaterialQuery {
            tags: query.tags.clone(),
            style_tags: query.style_tags.clone(),
            vendor_id: None,
            name_contains: query.search.as_ref().filter(|s| !s.is_empty()).cloned(),
            limit: query
                .limit
                .map(|n| (n.min(DESIGN_LIST_MATERIALS_MAX_LIMIT)) as usize),
        };
        Ok(lib.query(&q).into_iter().map(pbr_to_summary).collect())
    }

    /// Apply a slider patch to a single material and return the
    /// updated summary so the renderer can refresh the inspector
    /// without a follow-up `design_list_materials` round-trip.
    ///
    /// Validation runs *before* the in-place mutation so a single
    /// out-of-range slider can't half-apply a multi-field patch:
    ///
    /// * `metallic` / `roughness` / `transmission` must lie in
    ///   `[0.0, 1.0]` (PBR convention; outside this range the BSDF
    ///   becomes ill-defined and the rasteriser's tone-mapper either
    ///   clips or NaN-propagates).
    /// * `ior` must satisfy `1.0 <= ior <= 5.0`. The lower bound
    ///   forbids vacuum-or-below indices that would make Snell's
    ///   law trivially fail; the upper bound is well above any
    ///   architectural material (diamond is 2.42, sapphire 1.77).
    /// * `albedo` / `emissive` components must lie in `[0.0, 1.0]` —
    ///   the renderer treats values above 1 as HDR emissive and
    ///   below 0 as a coding bug, so the slider surface clamps to
    ///   the same range the inspector advertises.
    ///
    /// Returns [`BridgeServiceError::Invalid`] on any range violation
    /// and [`BridgeServiceError::Core`] on poisoned mutex / unknown
    /// material id. The `unknown id` case is folded into `Invalid`
    /// rather than `Core` because the renderer recovers from it by
    /// re-listing the library, not by reporting a runtime fault.
    pub fn design_update_material(
        &self,
        material_id: &str,
        update: &MaterialUpdate,
    ) -> Result<MaterialSummary, BridgeServiceError> {
        validate_material_update(update)?;
        let mut lib = self
            .material_library
            .lock()
            .map_err(|_| BridgeServiceError::Core("material_library mutex poisoned".to_string()))?;
        let current = lib.get(material_id).cloned().ok_or_else(|| {
            BridgeServiceError::Invalid(format!("material `{material_id}` not found"))
        })?;
        let mut next = current;
        if let Some(albedo) = update.albedo {
            next.albedo = albedo;
        }
        if let Some(metallic) = update.metallic {
            next.metallic = metallic;
        }
        if let Some(roughness) = update.roughness {
            next.roughness = roughness;
        }
        if let Some(ior) = update.ior {
            next.ior = ior;
        }
        if let Some(transmission) = update.transmission {
            next.transmission = transmission;
        }
        if let Some(emissive) = update.emissive {
            next.emissive = emissive;
        }
        let summary = pbr_to_summary(&next);
        lib.upsert(next);
        Ok(summary)
    }

    /// Read an `.ifc` file from disk and return a structured import
    /// summary the renderer can show on its "Import BIM" panel.
    ///
    /// This is a *parse-only* operation: nothing is written into the
    /// active project. The renderer uses the returned counts to render
    /// a preview ("123 walls, 45 slabs, …"), and a subsequent
    /// [`Self::bim_attach_ifc`] call folds the parsed model into the
    /// project's authoring graph. Splitting the parse from the
    /// attach keeps the parse path safely re-runnable on bad files
    /// without polluting project state.
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
    /// [`aec_export::write_proposal_pack_with_context`]. When
    /// `project_path` is supplied, opens the project's encrypted
    /// graph and threads real room / material / template counts and
    /// the project's floor-plan SVG into the proposal cover.
    ///
    /// Both the no-`project_path` case AND the case where the path
    /// turned out to be stale / corrupt fall back to the default
    /// empty context (the PDF is still a valid
    /// `printpdf`-serialised file). Mirrors the
    /// graceful-degradation contract documented on the
    /// `export:buildProposalPack` IPC handler — see
    /// `apps/desktop/electron/ipc.ts`.
    pub fn export_proposal_pack(
        &self,
        out_path: &str,
        project_name: &str,
        client_name: &str,
        project_path: Option<&str>,
    ) -> Result<ExportProposalPackResult, BridgeServiceError> {
        let built = match project_path {
            Some(p) if !p.is_empty() => {
                crate::deliver_context::try_build_for_project(p, &self.master_key)
            }
            _ => None,
        };
        let ctx = built.as_ref().map(|b| b.as_ctx()).unwrap_or_default();
        let res = aec_export::write_proposal_pack_with_context(
            Path::new(out_path),
            project_name,
            client_name,
            &ctx,
        )?;
        Ok(ExportProposalPackResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
        })
    }

    /// Build a contractor deliverable ZIP archive at `out_path`.
    /// Kind + options control the inventory; see
    /// [`aec_export::write_deliver_pack_with_context`] for the per-
    /// kind asset list.
    ///
    /// When `params.project_path` is supplied, the bridge opens the
    /// project's encrypted SQLCipher DB, builds a [`ProjectGraph`],
    /// and constructs a real `DeliverPackContext` (renders dir,
    /// material + BOQ schedules, sheets, IFC string, floor-plan SVG,
    /// room / material / template metadata). When absent, the pack
    /// degrades to a structurally valid archive built from a default
    /// empty context.
    ///
    /// The returned `contents` matches the inventory the renderer
    /// preview pane shows pre-archive, and `total_bytes` is the sum
    /// of payload sizes (manifest excluded).
    pub fn deliver_build_pack(
        &self,
        params: DeliverBuildPackParams,
    ) -> Result<DeliverPackResult, BridgeServiceError> {
        let DeliverBuildPackParams {
            out_path,
            kind,
            project_name,
            options,
            project_path,
        } = params;
        let kind = aec_export::DeliverPackKind::parse(&kind)?;
        let opts = aec_export::DeliverPackOptions {
            include_renders: options.include_renders,
            include_sheets: options.include_sheets,
            include_ifc: options.include_ifc,
            include_boq: options.include_boq,
            include_proposal: options.include_proposal,
        };
        // When the renderer threads through a `project_path`, open
        // the encrypted package and build a real `DeliverPackContext`
        // from its graph. Otherwise (or when the path turns out to
        // be stale / corrupt — `peekActiveProjectPath()` can return a
        // path to a project the user has since deleted or moved) fall
        // back to the default empty context. Callers still get a
        // structurally valid ZIP, just without project-specific data.
        // Mirrors the graceful-degradation contract documented on the
        // `deliver:buildPack` IPC handler.
        let built = match project_path.as_deref() {
            Some(p) if !p.is_empty() => {
                crate::deliver_context::try_build_for_project(p, &self.master_key)
            }
            _ => None,
        };
        let ctx = built.as_ref().map(|b| b.as_ctx()).unwrap_or_default();
        let res = aec_export::write_deliver_pack_with_context(
            Path::new(&out_path),
            kind,
            &opts,
            &project_name,
            &ctx,
        )?;
        Ok(DeliverPackResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
            contents: res.contents,
            total_bytes: res.total_bytes,
        })
    }

    /// Pack the full project package directory (`.aecstudio`) at
    /// `project_path` into a portable ZIP archive at `out_path`. The
    /// archive includes the encrypted `project.sqlite`, `project.nonce`,
    /// all sub-directories ([`aec_core::package::PACKAGE_DIRS`]), and
    /// an auto-generated `_aec_archive_manifest.json` describing the
    /// archive shape. The bytes pass the `PK\x03\x04` magic check —
    /// see [`aec_export::write_project_package_zip`].
    ///
    /// Distinct from [`Self::deliver_build_pack`], which assembles a
    /// *client-facing* PDF + render + IFC bundle. This method is the
    /// "move this project to another machine" gesture — extract the
    /// archive and `ProjectPackage::open` reads the package as-is
    /// (provided the user has the same master key).
    ///
    /// Routed as `&self` because the export crate is stateless; no
    /// project DB connection is opened (the encrypted `.sqlite` is
    /// copied at the filesystem level). Mirrors [`Self::export_pdf`]
    /// / [`Self::deliver_build_pack`] so concurrent status polls
    /// are not blocked by long archive walks.
    pub fn project_export_package(
        &self,
        project_path: &str,
        out_path: &str,
    ) -> Result<ProjectExportPackageResult, BridgeServiceError> {
        let res =
            aec_export::write_project_package_zip(Path::new(project_path), Path::new(out_path))?;
        Ok(ProjectExportPackageResult {
            out_path: res.out_path.to_string_lossy().into_owned(),
            entries: res.entries,
            total_bytes: res.total_bytes,
        })
    }

    /// Walk every entity in the project graph and assign a
    /// classification from `scheme`. Supported schemes are
    /// `"ifc"` (assign / update `entities.kind` to an IFC class),
    /// `"uniformat-ii"` (write the ASTM E1557 code as a `components`
    /// row), and `"omniclass-21"` (write the CSI OmniClass Table-21
    /// code as a `components` row). See
    /// [`aec_bim::classification_tables`] for the embedded code
    /// tables. Returns per-entity assignments so the renderer's
    /// property panel can populate without a follow-up
    /// `project_graph_list` call.
    ///
    /// Classification overrides live under the `aec/classification/`
    /// component-kind prefix, **not** `bim/`, so they survive a
    /// `bim_attach_ifc` re-attach (which wipes `bim/%` for changed
    /// entities — see `bim_attach.rs:463`). The prefix is supplied
    /// by [`aec_bim::classification_tables::ClassificationScheme::component_kind`].
    ///
    /// # Concurrency
    ///
    /// The entity scan and the per-entity writes must observe the
    /// same DB snapshot — two concurrent `bim_classify` calls (or a
    /// `bim_classify` interleaved with `bim_set_property` /
    /// `command_apply`) would otherwise race: caller A reads
    /// `(id, kind)` for every row, caller B commits a `kind` change
    /// for some `id`, then caller A's write uses B's stale `kind`
    /// in its `tag == *kind` comparison and overwrites B's update.
    ///
    /// The fix mirrors [`Self::bim_set_property`]: open the
    /// transaction with [`TransactionBehavior::Immediate`] so the
    /// RESERVED lock is acquired up-front, then run the entity
    /// SELECT INSIDE that transaction. Combined with the
    /// `busy_timeout` pragma (`apply_pragmas`), concurrent callers
    /// serialise at SQLite's RESERVED lock and each sees the
    /// previous caller's committed snapshot, not a stale one.
    pub fn bim_classify(
        &self,
        project_path: &str,
        scheme: &str,
    ) -> Result<BimClassifyResult, BridgeServiceError> {
        let scheme = aec_bim::classification_tables::ClassificationScheme::parse(scheme)
            .ok_or_else(|| {
                BridgeServiceError::Invalid(format!(
                    "unknown classification scheme: {scheme} (supported: ifc, uniformat-ii, omniclass-21)"
                ))
            })?;

        let (_pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;

        // Open the transaction as IMMEDIATE up-front so the
        // RESERVED lock is acquired before the entity scan runs.
        // This closes the TOCTOU window that a default DEFERRED
        // transaction would leave open between the SELECT below
        // and the per-entity UPDATE/upsert further down — see the
        // `# Concurrency` doc on this method.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Collect entities first so we don't hold a prepared
        // statement open while we mutate. The `query_map().collect()`
        // pattern drains every row into the `Vec` before the
        // statement is dropped, so it's safe to run inside the
        // transaction and subsequently UPDATE / upsert the same
        // table without statement-lifetime conflicts.
        let entities: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, kind FROM entities")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut details: Vec<BimClassifyAssignment> = Vec::with_capacity(entities.len());
        let mut classified: u32 = 0;
        let mut unchanged: u32 = 0;
        let mut skipped: u32 = 0;
        let now = chrono::Utc::now().to_rfc3339();

        for (id, kind) in &entities {
            let Some(ifc_class) = aec_bim::classification_tables::classify_kind(kind) else {
                skipped += 1;
                continue;
            };
            match scheme {
                aec_bim::classification_tables::ClassificationScheme::Ifc => {
                    let tag = ifc_class.ifc_tag().to_string();
                    // `classified` is the **change** count, not the
                    // walk-progress count: only increment when the
                    // DB actually moves, so a renderer using it to
                    // gate "Undo classify?" prompts sees the right
                    // value on a re-run of an already-classified
                    // project (which is a no-op).
                    if tag == *kind {
                        unchanged += 1;
                    } else {
                        tx.execute(
                            "UPDATE entities SET kind = ?1, updated_at = ?2 WHERE id = ?3",
                            params![tag, now, id],
                        )?;
                        classified += 1;
                    }
                    details.push(BimClassifyAssignment {
                        entity_id: id.clone(),
                        code: tag,
                        title: String::new(),
                    });
                }
                aec_bim::classification_tables::ClassificationScheme::UniformatIi => {
                    let Some(code) = aec_bim::classification_tables::ifc_to_uniformat(&ifc_class)
                    else {
                        skipped += 1;
                        continue;
                    };
                    let component_kind = scheme.component_kind().expect("non-ifc scheme has kind");
                    let body = serde_json::json!({
                        "scheme": scheme.as_str(),
                        "code": code.code,
                        "title": code.title,
                        "level": code.level,
                        "source": "auto",
                    });
                    // Same change-count semantic as the IFC branch:
                    // if a prior `components` row for this
                    // (entity_id, kind) already carries the same
                    // body, the upsert is a no-op and we don't
                    // count it. Querying first costs one extra
                    // SELECT but lets the renderer trust
                    // `classified`.
                    if upsert_classification_component_if_changed(&tx, id, component_kind, &body)? {
                        classified += 1;
                    } else {
                        unchanged += 1;
                    }
                    details.push(BimClassifyAssignment {
                        entity_id: id.clone(),
                        code: code.code.to_string(),
                        title: code.title.to_string(),
                    });
                }
                aec_bim::classification_tables::ClassificationScheme::Omniclass21 => {
                    let Some(code) = aec_bim::classification_tables::ifc_to_omniclass(&ifc_class)
                    else {
                        skipped += 1;
                        continue;
                    };
                    let component_kind = scheme.component_kind().expect("non-ifc scheme has kind");
                    let body = serde_json::json!({
                        "scheme": scheme.as_str(),
                        "code": code.code,
                        "title": code.title,
                        "level": code.level,
                        "source": "auto",
                    });
                    if upsert_classification_component_if_changed(&tx, id, component_kind, &body)? {
                        classified += 1;
                    } else {
                        unchanged += 1;
                    }
                    details.push(BimClassifyAssignment {
                        entity_id: id.clone(),
                        code: code.code.to_string(),
                        title: code.title.to_string(),
                    });
                }
            }
        }
        tx.commit()?;

        // Invalidate the engine-status cache so the renderer's next
        // status poll picks up the new entity-kind histogram. Use the
        // shared helper that canonicalises `project_path` first —
        // the cache is keyed on canonical paths, so passing the raw
        // input would silently leave a stale entry for one full TTL.
        self.invalidate_status_cache_for(project_path);

        Ok(BimClassifyResult {
            scheme: scheme.as_str().into(),
            classified,
            unchanged,
            skipped,
            details,
        })
    }

    /// Set a single property on a BIM entity. The property lands in
    /// a `components` row of kind `aec/property/<pset>` (NOT
    /// `bim/property/...`) — the `aec/` prefix is what makes user
    /// overrides survive a `bim_attach_ifc` re-attach (which wipes
    /// `bim/%` for changed entities).
    ///
    /// `pset` is the Property Set name (e.g. `"Pset_WallCommon"`,
    /// or `"AECStudio_Custom"` for free-form additions). `key` is
    /// the property name within that set (e.g. `"FireRating"`,
    /// `"IsExternal"`). `value` is serialised as a JSON `string`
    /// — the renderer's property editor handles type coercion
    /// before calling this method.
    ///
    /// Returns the previous value (if any) so the renderer can wire
    /// undo via [`Self::bim_set_property`] of the prior value
    /// without an extra round trip.
    ///
    /// # Concurrency
    ///
    /// The read-modify-write of the property `body` JSON must be
    /// atomic across concurrent callers — two `bim_set_property`
    /// calls targeting the same `(entity_id, pset)` but different
    /// `key`s would otherwise race: each reads the same prior body,
    /// merges its own key, and the second writer's `ON CONFLICT
    /// … DO UPDATE` overwrites the first writer's result, silently
    /// dropping one key.
    ///
    /// To prevent that, the prior-body read runs **inside** the same
    /// transaction as the write, and the transaction is opened with
    /// [`TransactionBehavior::Immediate`] so it acquires SQLite's
    /// RESERVED lock immediately on entry (not lazily on first write).
    /// Combined with the `busy_timeout` pragma set in
    /// `apply_pragmas`, concurrent callers serialise cleanly: the
    /// second caller's `BEGIN IMMEDIATE` blocks until the first
    /// caller's transaction commits, at which point the second
    /// caller's read sees the first caller's write.
    pub fn bim_set_property(
        &self,
        project_path: &str,
        entity_id: &str,
        pset: &str,
        key: &str,
        value: &str,
    ) -> Result<BimSetPropertyResult, BridgeServiceError> {
        if pset.trim().is_empty() {
            return Err(BridgeServiceError::Invalid("pset must not be empty".into()));
        }
        if key.trim().is_empty() {
            return Err(BridgeServiceError::Invalid("key must not be empty".into()));
        }

        let (_pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;

        let component_kind = format!("aec/property/{pset}");

        // Open the transaction as IMMEDIATE so the RESERVED lock is
        // acquired up-front — every step (entity-existence check,
        // prior-body read, write) runs against the same locked
        // snapshot. The deferred default would acquire the lock only
        // on the first write, which is exactly the TOCTOU window we
        // need to close.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Make sure the entity exists — silent inserts against
        // missing IDs are a footgun (and the FK on `components` would
        // catch it, but the error string is opaque). Caller-facing
        // `EntityNotFound` is more useful.
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM entities WHERE id = ?1",
                params![entity_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(BridgeServiceError::Invalid(format!(
                "entity {entity_id} not found in project graph"
            )));
        }

        // Read previous body (if any) so we can extract the prior
        // value for the response. This read must be INSIDE the
        // IMMEDIATE transaction so concurrent writers see each
        // other's commits before merging — see the doc comment.
        let prev_body: Option<String> = tx
            .query_row(
                "SELECT body FROM components WHERE entity_id = ?1 AND kind = ?2",
                params![entity_id, &component_kind],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let mut body_obj: serde_json::Map<String, serde_json::Value> = match prev_body.as_deref() {
            Some(s) => serde_json::from_str(s).unwrap_or_default(),
            None => serde_json::Map::new(),
        };
        let previous_value = body_obj.get(key).and_then(|v| {
            v.as_str()
                .map(ToString::to_string)
                .or_else(|| Some(v.to_string()))
        });
        body_obj.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
        let new_body = serde_json::Value::Object(body_obj);

        // `(entity_id, kind)` is the natural PK for our overlay rows.
        // `components.id` is the SQLite PK, but it doesn't carry
        // semantic meaning — we synthesise a deterministic value
        // from `(entity_id, kind)` so re-applying a property update
        // doesn't accumulate orphaned rows.
        let comp_id = format!("comp_{}_{}", entity_id, &component_kind.replace('/', "_"));
        tx.execute(
            "INSERT INTO components(id, entity_id, kind, body) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(entity_id, kind) DO UPDATE SET id = excluded.id, body = excluded.body",
            params![comp_id, entity_id, component_kind, new_body.to_string()],
        )?;
        tx.commit()?;

        // Canonicalise via the shared helper so the invalidation
        // hits the same cache key the original `project_engine_status`
        // call inserted (the cache is keyed on canonical paths).
        self.invalidate_status_cache_for(project_path);
        Ok(BimSetPropertyResult {
            entity_id: entity_id.to_string(),
            pset: pset.to_string(),
            key: key.to_string(),
            previous_value,
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
    /// [`Self::bim_attach_ifc`] and the command engine
    /// (`command_apply`).
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

    /// Read row data back from a previously-written XLSX schedule.
    ///
    /// The renderer's `ScheduleView` calls this immediately after
    /// `bim_generate_schedule` so the table can display the actual
    /// row contents inline (not just the row count). This closes the
    /// XLSX-to-row-list round trip: every cell rendered in the UI
    /// goes through `aec_bim::xlsx_reader::read_xlsx_rows`, which
    /// reads the file calamine-style — the same file the writer just
    /// produced — so manual edits, future schedule extensions that
    /// mutate cells before save, and the renderer's display all
    /// agree on ground truth.
    ///
    /// Error mapping: every variant of
    /// [`aec_bim::xlsx_reader::XlsxReadError`] flattens to
    /// [`BridgeServiceError::Bim`] with a single-line description.
    /// The renderer side maps these to a user-facing toast via the
    /// existing IPC error envelope; no new error variant is needed
    /// on the bridge surface.
    pub fn bim_read_schedule_rows(
        &self,
        xlsx_path: &str,
    ) -> Result<BimScheduleRows, BridgeServiceError> {
        let (header, rows) =
            aec_bim::xlsx_reader::read_xlsx_rows_with_header(Path::new(xlsx_path), None)
                .map_err(|e| BridgeServiceError::Bim(format!("read_schedule_rows: {e}")))?;
        Ok(BimScheduleRows { header, rows })
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

    /// Phase 12 Task 29 — persist the in-memory render queue to a JSON
    /// file on disk. Used at session shutdown / quit so a subsequent
    /// service boot can resume an in-flight render batch via
    /// [`Self::render_queue_restore`]. Atomic write-then-rename in the
    /// underlying `RenderQueue::persist`.
    pub fn render_queue_persist(&self, path: &str) -> Result<(), BridgeServiceError> {
        let state = self.lock_render_state()?;
        state
            .queue
            .persist(std::path::Path::new(path))
            .map_err(|e| BridgeServiceError::Core(format!("render_queue_persist: {e}")))
    }

    /// Phase 12 Task 29 — restore a previously-persisted render queue
    /// from disk into the in-memory state. Missing-file is *not* an
    /// error: a fresh boot before the first persist() call legitimately
    /// has no queue file yet, and the symmetric `load` returns an
    /// empty queue in that case.
    pub fn render_queue_restore(&self, path: &str) -> Result<(), BridgeServiceError> {
        let mut state = self.lock_render_state()?;
        state.queue = aec_render::queue::RenderQueue::load(std::path::Path::new(path))
            .map_err(|e| BridgeServiceError::Core(format!("render_queue_restore: {e}")))?;
        Ok(())
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

    /// Test-only accessor: replace the AI state with one pre-wired to a
    /// caller-supplied transport (e.g. a mock TCP server). Used by the
    /// `sidecar_mock` integration test to drive `ai_plan` end-to-end
    /// without spawning a real `llama-server`.
    ///
    /// Takes `&mut self` because [`AiState`] is held by value on
    /// [`BridgeService`]; the test harness has unique ownership of
    /// the service for the duration of the install.
    #[doc(hidden)]
    pub fn __test_install_ai_state(&mut self, state: AiState) {
        self.ai_state = state;
    }

    // ---------------------------------------------------------------
    // AI endpoints
    // ---------------------------------------------------------------
    //
    // Read-only AI endpoints (`ai_list_tools`, `ai_plan`,
    // `ai_runtime_status`, `ai_cancel_job`) take `&self` and run
    // fully concurrent with one another — the three internal
    // primitives on [`AiState`] (an `RwLock` for lifecycle state,
    // a `Mutex` for the spawn slot, a `Mutex` for the diff
    // registry) are independent, so:
    //
    //  - `ai_runtime_status` polls (the hot path: every ~500 ms while
    //    a plan is in flight) only take the `runtime` `RwLock` *read*
    //    side and run fully concurrent with everything else.
    //  - `ai_plan` holds the spawn-slot mutex only across
    //    `ensure_ready` (typically microseconds on the warm path, up
    //    to `DEFAULT_SPAWN_TIMEOUT` on the first call), then drops
    //    it before the blocking sidecar HTTP completion.
    //  - `ai_cancel_job` takes the spawn-slot mutex to terminate the
    //    handle, so it serialises with `ensure_ready` (correct: we
    //    must not race a `take()` against a freshly-`Some()` write).
    //
    // [`Self::ai_accept_diff`] and [`Self::ai_reject_diff`] take
    // `&mut self`. They mutate the project graph (accept) or the
    // AI audit log (both) and therefore go through
    // [`Self::command_apply_batch`] / [`Self::ai_audit_append`]
    // which require unique access to the service so the SQL
    // transaction, journal, and audit envelope are journaled
    // atomically. The bridge's outer `RwLock<BridgeService>` (held
    // by the napi shim) takes the write side for these two
    // methods, serialising them against every other bridge call
    // for the duration of the apply. This is intentional — the
    // accept path mutates SQLCipher state behind the user's most
    // recent gesture and must not race with concurrent reads of
    // the same project graph.
    //
    // The napi layer adds the second half of the fix: every blocking
    // AI endpoint is `#[napi] async fn` routed through
    // `spawn_blocking_napi`, so even when a cold-spawn is in flight
    // the Electron main process's JS event loop stays free for
    // unrelated work. See [`crate::ai_state`] module doc and
    // [`crate::napi_api`] AI endpoints block for the full design.

    /// Enumerate the local AI tools the planner is willing to dispatch.
    /// The renderer's "AI sidebar" calls this once at session start to
    /// populate the tool picker.
    ///
    /// The returned list is the union of:
    ///
    /// * **built-in tools** — the closed set defined by
    ///   [`aec_ai::AiToolSchema`] (style_assistant, lighting_assistant,
    ///   …), ordered by tool name; and
    /// * **extension AI tools** — every
    ///   [`aec_core::ExtensionType::AiTool`] in the loaded registry
    ///   whose manifest declares
    ///   [`aec_core::Permission::AiTools`]. Each extension tool
    ///   surfaces with its manifest `tool_id` as `name` and the
    ///   manifest-declared scopes / grammar key / cap on
    ///   `max_entities_modified`. Extensions missing the AI-tools
    ///   permission are *omitted* from the list — the
    ///   `resolve_extension_ai_tool` permission gate is the safe
    ///   default (an unprivileged extension shouldn't appear in the
    ///   planner's tool picker).
    pub fn ai_list_tools(&self) -> Result<Vec<AiToolDescriptor>, BridgeServiceError> {
        // `iter_sorted` already orders the built-in slice by tool
        // name, but extension tools come out of
        // `list_extension_ai_tools` in registry iteration order. A
        // final sort across the merged set is what makes the wire
        // payload deterministic across calls regardless of which
        // extensions are loaded — the renderer pins these by name
        // and any binary-search consumer downstream depends on the
        // full list being sorted.
        let mut tools: Vec<AiToolDescriptor> = ai_tool_schemas()
            .iter_sorted()
            .map(AiToolDescriptor::from)
            .collect();

        // Merge in extension AI tools. We discard the
        // `(_, errs)` half of `list_extension_ai_tools` here —
        // permission-denied extensions are *expected* to surface in
        // `errs`, not in the returned list, and the AI sidebar
        // shouldn't be reporting them. Production builds can surface
        // these via a dedicated diagnostics IPC.
        let (ext_tools, _errs) =
            aec_ai::list_extension_ai_tools(&self.extension_registry, &self.permission_enforcer);
        let schemas = ai_tool_schemas();
        for t in ext_tools {
            // Defense-in-depth: filter out extension tools whose
            // `tool_id` collides with a built-in `AiToolName`. The
            // manifest convention is dotted names (e.g.
            // `acme.layouter`), so collisions are unlikely in
            // practice, but if one slips through we'd ship a
            // duplicate entry to the renderer's tool picker — and
            // `resolve_ai_tool_alias` would dispatch the call to
            // the built-in instead of the extension because
            // `AiToolName::from_wire_str` matches the closed enum
            // first. That makes the extension entry visible in the
            // picker but unreachable via dispatch — a confusing UX
            // we'd rather prevent at the source. Dropping the
            // colliding entry surfaces the conflict honestly: the
            // built-in keeps its slot, the extension is omitted,
            // and the extension author sees their tool fail to
            // appear (which prompts a rename to a properly
            // namespaced `tool_id`). The drop is silent in the
            // public surface; a future diagnostics IPC can surface
            // the conflict to the extension author.
            if AiToolName::from_wire_str(&t.tool_id).is_some() {
                continue;
            }
            // Resolve the canonical host tool that will actually
            // dispatch this extension when `ai_plan` runs (same
            // helper `resolve_ai_tool_alias` uses). The advertised
            // cap MUST be clamped against the host tool's own
            // `max_entities_modified` — the planner's diff-safety
            // validator enforces the host cap regardless of what
            // the extension manifest declares, so advertising the
            // raw extension cap (e.g. 100) when the host caps at
            // 16 would surface slider values in the renderer that
            // are guaranteed to be rejected at dispatch. Skipping
            // tools with an unknown grammar_key matches
            // `resolve_ai_tool_alias`, which errors out on the
            // same condition — a tool that can't be dispatched
            // should not appear in the picker either.
            let Some(host_schema) = canonical_builtin_for_grammar_key(&t.grammar_key, schemas)
                .and_then(|name| schemas.get(name))
            else {
                continue;
            };
            let advertised_cap = t
                .max_entities_modified
                .min(host_schema.max_entities_modified);
            // The advertised `allowed_scopes` MUST be the
            // INTERSECTION of the extension's declared scopes and
            // the host tool's schema scopes — same architectural
            // pattern as the cap clamp above. The host schema is
            // load-bearing: the planner's grammar/diff validator
            // enforces the host scopes regardless of what the
            // extension manifest declares, so advertising a scope
            // that's only in the extension's set (but missing from
            // the host's) would surface a value in the renderer
            // that `resolve_ai_tool_alias` is guaranteed to reject
            // at dispatch. Mirrors the cap-clamp posture: shrink
            // toward the host's smaller set rather than the
            // extension's wider one.
            let advertised_scopes: Vec<String> = t
                .allowed_scopes
                .iter()
                .filter(|sc| host_schema.allowed_scopes.contains(sc))
                .map(|sc| sc.as_str().to_owned())
                .collect();
            // An extension whose declared scopes share zero overlap
            // with the host schema is unreachable at dispatch — the
            // intersection is empty, so every `ai_plan` call would
            // be rejected at the scope check below. Drop the tool
            // from the picker entirely, matching the unreachable-
            // grammar_key and host-cap=0 dispositions above.
            if advertised_scopes.is_empty() {
                continue;
            }
            tools.push(AiToolDescriptor {
                name: t.tool_id,
                display_name: t.display_name,
                description: t.description,
                allowed_scopes: advertised_scopes,
                max_entities_modified: advertised_cap,
                grammar_key: t.grammar_key,
                // Extensions don't compose child tools today —
                // their `ai_tool` body declares a single
                // `grammar_key`, so the child-tools list is
                // intentionally empty.
                child_tools: Vec::new(),
            });
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tools)
    }

    /// Plan a single AI action against the local LLM sidecar.
    ///
    /// On first call the sidecar is lazily spawned (cold-load ~5 s for
    /// a 7B q4 model on SSD). Subsequent calls reuse the running
    /// process. The full path is:
    ///
    ///   1. parse `tool` into [`AiToolName`]; reject unknowns
    ///   2. ensure the sidecar is `Ready`, spawning if needed
    ///   3. build a [`AiPlanRequest`] from the caller params
    ///   4. dispatch through [`ToolPlanner::dispatch`] (this is the
    ///      one place that talks to the model — see planner doc)
    ///   5. convert the typed response into a [`Diff`] via
    ///      [`DiffEngine::build`]
    ///   6. register the diff in `AiState::pending_diffs` so a later
    ///      `ai_accept_diff` / `ai_reject_diff` can resolve it
    ///   7. return the diff id + parsed payload to the renderer
    pub fn ai_plan(
        &self,
        project_path: &str,
        tool: &str,
        scope: Scope,
        prompt: &str,
        context_json: &str,
        max_entities_modified: u32,
    ) -> Result<AiPlanResult, BridgeServiceError> {
        // Resolve the wire-format tool string into the closed
        // [`AiToolName`] the planner + diff engine know how to
        // dispatch. Built-in tool ids (`style_assistant`, etc.)
        // resolve directly; extension-supplied `tool_id`s
        // (advertised by [`Self::ai_list_tools`]) resolve through
        // their declared `grammar_key` — every extension AI tool
        // must declare a grammar that the host already ships,
        // because the diff engine can only translate model output
        // shapes it has a `build_*` arm for. The renderer's
        // attribution string (returned in [`AiPlanResult::tool`])
        // is restored to the extension `tool_id` after dispatch
        // so the audit log and tool-picker round-trip correctly.
        let (tool_name, extension_attribution, effective_max_entities) =
            self.resolve_ai_tool_alias(tool, scope, max_entities_modified)?;
        let max_entities_modified = effective_max_entities;
        let context: serde_json::Value = if context_json.is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(context_json).map_err(|e| {
                BridgeServiceError::Ai(format!("ai_plan context_json deserialisation failed: {e}"))
            })?
        };
        // The registries are parsed from JSON in `defaults()` (see
        // `ai_tools.json` and the GBNF blobs). That parse is small
        // (~2 KiB) but `ai_plan` is called interactively, so we cache
        // both via `OnceLock` to amortise the cost across the session.
        // Both types are immutable read-only collections behind shared
        // references, so the cache is sound under concurrent reads.
        let planner = ToolPlanner::new(ai_tool_schemas(), ai_grammars());
        let request = AiPlanRequest {
            tool: tool_name,
            scope,
            prompt: prompt.to_owned(),
            context,
            max_entities_modified,
        };
        // We split the work into three phases so no AiState lock is
        // held across the blocking call into the sidecar:
        //
        //   1. `ensure_ready` (which may spawn the sidecar, up to
        //      `DEFAULT_SPAWN_TIMEOUT`) returns an owned
        //      `SidecarTransport`. During the spawn it holds the
        //      handle-slot mutex internally; the `runtime` `RwLock`
        //      is only briefly acquired to publish state transitions
        //      (`Idle` → `Loading` → `Ready`/`Failed`), and is
        //      *released* before the spawn blocks on `/health`.
        //      Concurrent `ai_runtime_status` polls therefore observe
        //      `Loading` immediately and never block on the spawn.
        //   2. dispatch through the planner without holding any
        //      AiState lock at all — the transport is an owned
        //      stateless dial-out descriptor.
        //   3. insert the resulting diff into `pending_diffs` (its
        //      own mutex, independent of sidecar lifecycle).
        //
        // `SidecarTransport` is `{ port: u16, request_timeout:
        // Duration }` (see `aec_ai::transport::SidecarTransport`), so
        // taking it by value is a 12-byte move. The owning
        // `SidecarHandle` (which holds the child process) stays
        // inside `AiState::handle_slot`.
        //
        // First-call note: on the first `ai_plan` of a session,
        // `ensure_ready` synchronously spawns the sidecar and waits
        // up to `DEFAULT_SPAWN_TIMEOUT` (30 s) for its `/health`
        // probe. The handle-slot mutex IS held for that window, so a
        // racing `ai_cancel_job` blocks (correctly — we must not
        // race `take()` against a freshly-`Some()` write). Crucially
        // the renderer's status polls are *not* blocked: the
        // `runtime` `RwLock` is dropped before the spawn waits on
        // `/health`, and the napi `ai_runtime_status` is
        // `spawn_blocking`-wrapped so the libuv main thread is free
        // throughout.
        let transport = self.ai_state.ensure_ready(DEFAULT_SPAWN_TIMEOUT)?;
        let response = planner.dispatch(&request, &transport)?;
        let diff = DiffEngine::build(&response);
        let parsed = response.parsed.clone();
        // `entities_modified` reflects the *actual* number of
        // operations the resulting diff will apply, derived from
        // `DiffEngine::build` (which already knows the per-tool
        // shape — `proposals[]`, `furniture_ids[]`, `polylines[]`,
        // etc.).
        //
        // `response.entities_modified` was historically the request
        // cap, which made this field misleading; the planner now
        // returns the actual count via
        // `count_response_entities` (the same per-tool logic
        // `DiffEngine::build` uses), so the two numbers SHOULD agree
        // for the 4 tools the diff engine knows about. We still
        // report `diff.operations.len()` here as defense in depth —
        // if the per-tool counter and the diff builder ever diverge
        // (e.g., one is updated and the other forgotten), the bridge
        // continues to report what the renderer will actually
        // observe.
        let entities = u32::try_from(diff.operations.len()).unwrap_or(u32::MAX);
        debug_assert_eq!(
            entities, response.entities_modified,
            "DiffEngine::build and planner::count_response_entities must agree",
        );
        // Capture the project path with the pending diff so the
        // later `ai_accept_diff` / `ai_reject_diff` knows which
        // project package to open. See `PendingDiff` rustdoc for
        // the "plan on A, switch to B, accept the A diff" rationale.
        let diff_id = self.ai_state.insert_diff(project_path, scope, diff)?;
        Ok(AiPlanResult {
            diff_id: diff_id.as_str().to_owned(),
            parsed,
            // Surface the extension's `tool_id` (when the call
            // originated from an extension AI tool) so the
            // renderer's tool picker can route the result back
            // to the originating extension. Built-in tools
            // return their canonical wire-format name unchanged.
            tool: extension_attribution.unwrap_or_else(|| tool_name.as_str().to_owned()),
            entities_modified: entities,
        })
    }

    /// Resolve a wire-format `ai_plan` tool string into:
    ///   * a built-in [`AiToolName`] the planner + diff engine can
    ///     dispatch (every AI tool, including extension-provided
    ///     ones, ultimately routes through a built-in grammar +
    ///     diff engine `build_*` arm because the diff engine is
    ///     closed to known output shapes);
    ///   * an optional extension attribution string (`Some` when
    ///     the call originated from an extension AI tool, `None`
    ///     for built-in calls); and
    ///   * the effective `max_entities_modified` cap, clamped by
    ///     the extension's manifest cap when applicable so the
    ///     extension can never authorise a larger blast radius
    ///     than its manifest declares (defense-in-depth on top of
    ///     the planner's own safety validator).
    ///
    /// For built-in tools this is a single closed-enum lookup; for
    /// extension tools we additionally enforce:
    ///   * the extension is loaded + has the `AiTools` permission
    ///     (already checked by `list_extension_ai_tools`);
    ///   * the extension's `grammar_key` maps to a known built-in
    ///     [`AiToolName`] (extensions piggy-back on the host's
    ///     grammar + diff engine surface — they cannot introduce a
    ///     new model output shape);
    ///   * the requested `scope` is in the INTERSECTION of the
    ///     extension's declared `allowed_scopes` and the host
    ///     tool's schema `allowed_scopes` — the same intersection
    ///     `ai_list_tools` advertises to the renderer, so a scope
    ///     surfaced in the picker is guaranteed to dispatch
    ///     cleanly (cap-clamp pattern, applied to scopes).
    ///
    /// Note on grammar_key disambiguation: more than one built-in
    /// schema may declare the same `grammar_key` when the variants
    /// share both a model output shape and a diff-engine arm
    /// (today `plan_detection` and `plan_to_wall` both declare
    /// `grammar_key: "plan_detection"` and both route through
    /// [`aec_ai::diff_engine::build_plan_detection`]). When that
    /// happens, this resolver picks the host tool whose
    /// wire-format name equals the `grammar_key` ("canonical home"
    /// for that grammar) rather than falling out of sorted-name
    /// order non-deterministically. This contract is mirrored in
    /// `crates/aec_ai/data/ai_tools.json`, where the primary tool
    /// for a grammar shares its `id` with `grammar_key`. If the
    /// catalogue ever ships a grammar_key with no matching tool
    /// name, we fall back to the sorted-name first match so the
    /// resolution stays deterministic.
    ///
    /// Note on per-call cost: this rebuilds the extension AI tool
    /// list on every `ai_plan` for an extension tool by walking
    /// the loaded extension registry, checking `Permission::AiTools`
    /// on each, and parsing declared scopes. We deliberately do
    /// NOT cache this list inside [`BridgeService`] today, because:
    ///   * the cost is bounded by the number of `AiTools`-declaring
    ///     extensions (single-digit at the expected catalogue
    ///     scale; permission check short-circuits the rest), and
    ///   * a stale cache here would be a *correctness* bug rather
    ///     than a perf bug — the planner would dispatch to a tool
    ///     the extension no longer owns or has permission for.
    ///
    /// If the extension count ever grows to where the per-call
    /// walk dominates `ai_plan` latency, the right fix is a single
    /// `extensions_changed` event source feeding a shared cached
    /// `Arc<Vec<ExtensionAiToolMeta>>` reused by both
    /// [`Self::ai_list_tools`] and this resolver — invalidated on
    /// (a) boot, (b) any future hot-reload IPC, and (c) any future
    /// permission-mutation IPC. Until then keeping the per-call
    /// walk is the simpler correctness story.
    fn resolve_ai_tool_alias(
        &self,
        tool: &str,
        scope: Scope,
        max_entities_modified: u32,
    ) -> Result<(AiToolName, Option<String>, u32), BridgeServiceError> {
        if let Some(name) = AiToolName::from_wire_str(tool) {
            return Ok((name, None, max_entities_modified));
        }
        let (ext_tools, _errs) =
            aec_ai::list_extension_ai_tools(&self.extension_registry, &self.permission_enforcer);
        let ext = ext_tools
            .into_iter()
            .find(|t| t.tool_id == tool)
            .ok_or_else(|| BridgeServiceError::Ai(format!("unknown ai tool `{tool}`")))?;
        // Pick the host tool that owns this grammar_key FIRST, so
        // the scope check below can clamp the effective scope set
        // against the host's `allowed_scopes` — the same
        // intersection `ai_list_tools` advertises to the renderer.
        // When more than one match (today `plan_detection` and
        // `plan_to_wall` both declare `grammar_key:
        // "plan_detection"`), prefer the canonical home — see
        // [`canonical_builtin_for_grammar_key`] and the doc comment
        // on this method.
        let schemas = ai_tool_schemas();
        let builtin =
            canonical_builtin_for_grammar_key(&ext.grammar_key, schemas).ok_or_else(|| {
                BridgeServiceError::Ai(format!(
                    "extension ai tool `{}` declares unknown grammar_key `{}`",
                    ext.tool_id, ext.grammar_key
                ))
            })?;
        // Effective scope set is the INTERSECTION of the extension's
        // declared scopes and the host tool's schema scopes — the
        // same shape `ai_list_tools` advertises to the renderer.
        // The host schema is load-bearing: the planner's
        // grammar/diff validator enforces it regardless of what the
        // extension manifest claims, so a scope only present in the
        // extension's set is unreachable at dispatch. Mirroring the
        // cap-clamp pattern below: shrink toward the host's smaller
        // set, not the extension's wider one.
        //
        // The `.expect("…")` is safe for the same reason as the cap
        // clamp: `canonical_builtin_for_grammar_key` above only
        // returns `Some(name)` for names that exist in `schemas`.
        let host_schema = schemas
            .get(builtin)
            .expect("canonical_builtin_for_grammar_key only returns names present in schemas");
        if !ext.allowed_scopes.contains(&scope) || !host_schema.allowed_scopes.contains(&scope) {
            let effective: Vec<&str> = ext
                .allowed_scopes
                .iter()
                .filter(|sc| host_schema.allowed_scopes.contains(sc))
                .map(|sc| sc.as_str())
                .collect();
            return Err(BridgeServiceError::Ai(format!(
                "extension ai tool `{}` does not allow scope `{}` (effective allowed: {:?})",
                ext.tool_id,
                scope.as_str(),
                effective,
            )));
        }
        // The effective cap must be the smallest of:
        //   - the caller's requested cap (`max_entities_modified`),
        //   - the extension's manifest cap (`ext.max_entities_modified`),
        //   - the host tool's schema cap (`host_schema.max_entities_modified`).
        //
        // The host cap is load-bearing: the planner's diff-safety
        // validator enforces it regardless of what the extension or
        // the caller claims, so a value that exceeds the host cap
        // would be rejected at dispatch even when the extension
        // manifest declares a higher ceiling (e.g. extension says
        // 100, host caps at 16). Clamping here keeps the
        // contract symmetric with `ai_list_tools`'s advertised cap
        // and means the renderer's slider never produces a value
        // the planner is going to reject.
        let effective_cap = max_entities_modified
            .min(ext.max_entities_modified)
            .min(host_schema.max_entities_modified);
        Ok((builtin, Some(ext.tool_id), effective_cap))
    }

    /// Apply an accepted AI diff to the project graph.
    ///
    /// Phase 11 task 10 — the previous incarnation of this method
    /// just dropped the pending diff entry after marking it
    /// accepted. That left the project graph unchanged even though
    /// the renderer's AI panel had already moved on, which broke
    /// every downstream contract (undo, audit, render, export).
    ///
    /// The real apply path is a four-phase sequence:
    ///
    /// **Phase 1 – pre-commit (recoverable):** Peek the
    /// `PendingDiff` from `AiState::pending_diffs` (yielding a
    /// clone of both the `Diff` and the project path), open the
    /// project package + SQLCipher connection, load the current
    /// `ProjectGraph`, convert the `Diff` into a `Vec<Command>` via
    /// [`aec_command::diff_to_commands`], and validate the entire
    /// batch against the engine's scope. Any failure here returns
    /// before SQL is touched, so the pending diff stays in the
    /// registry for retry. The graph loaded for the converter is
    /// moved into the engine via [`CommandEngine::open_with_graph`]
    /// so the `entities` table is not re-read — see
    /// `ANALYSIS_0003 (round 2)`.
    ///
    /// **Phase 2 – commit (irreversible):** Run
    /// [`CommandEngine::execute_persistent_batch`] which writes
    /// every delta + journal entry in one SQL transaction. The
    /// transaction's `commit()` is the point of no return: once it
    /// returns Ok the graph has been mutated on disk. If `commit`
    /// fails the whole batch is rolled back and the pending diff
    /// stays in the registry — `diff_to_commands` is deterministic
    /// for the same input graph so retry produces the same commands
    /// without duplicating entity IDs.
    ///
    /// **Phase 3 – finalize (must run after phase 2):** Remove the
    /// `PendingDiff` from `AiState`. This step lives between commit
    /// and audit append because retrying the accept after a
    /// successful commit would re-enter `diff_to_commands`, which
    /// generates *fresh* `EntityId::new()` UUIDs for every `Insert`
    /// op — committing a second time would duplicate every inserted
    /// entity. Finalizing here guarantees that path is unreachable.
    /// See `BUG_0001 (round 3)`.
    ///
    /// **Phase 4 – AI audit append (post-commit):** Append an
    /// `AiAuditRecord { status: Accepted, .. }` to
    /// `<project>/audit/ai_audit.jsonl`. The main command audit
    /// chain already has the per-command entries from phase 2; the
    /// AI audit chain answers "how many of the model's proposals
    /// did the user accept?" on its own JSONL log. If this step
    /// fails (disk full, audit dir replaced with a file, etc.) we
    /// surface the error so the renderer can prompt for an audit
    /// chain re-export — but the graph mutation is durable and the
    /// pending diff is already gone, so there is no retry-induced
    /// duplication. The chain verifier (`project_audit_chain`)
    /// detects the missing AI envelope on next walk.
    ///
    /// Operations the converter could not translate (unknown
    /// entity kind, dangling target, missing payload field) are
    /// reported in [`AiAcceptOutcome::skipped`] rather than
    /// erroring the whole accept — the user already reviewed the
    /// diff and clicked Accept, so the service commits whatever
    /// subset the schema understands.
    pub fn ai_accept_diff(&mut self, diff_id: &str) -> Result<AiAcceptOutcome, BridgeServiceError> {
        // Phase 1 + Phase 2: pre-commit + commit. Any failure here
        // leaves the pending diff in the registry so the renderer
        // can retry — prior to the round-2 peek/finalize split, a
        // transient failure (e.g. SQLCipher key mismatch on a
        // moved project) silently lost the diff with no recovery
        // path.
        let pending = self.ai_state.peek_diff(diff_id)?;
        let committed = self.ai_accept_diff_commit(pending)?;
        // Phase 3: finalize BEFORE the post-commit audit append.
        //
        // `BUG_0001 (round 3)`: the previous structure ran audit
        // append inside the commit helper and only finalized once
        // both succeeded. That ordering broke the peek/finalize
        // contract for retry-safety: if the audit append failed
        // *after* the SQL commit, the diff was left in the registry
        // for retry — but a retry would re-enter
        // `diff_to_commands`, which generates fresh
        // `EntityId::new()` UUIDs for every `Insert` op, and a
        // second successful commit would silently double every
        // inserted entity. Finalizing here closes the
        // retry-duplication window: once SQL is committed the diff
        // is unreachable for retry.
        //
        // A `finalize_diff` failure here is genuinely anomalous
        // (would require a concurrent finalize for the same id
        // racing ahead of us) — we surface it instead of
        // swallowing. In that pathological case the graph is
        // committed and the diff is *still* in the registry; the
        // next retry would duplicate. Acceptable trade-off vs
        // silently masking a real concurrency bug.
        self.ai_state.finalize_diff(diff_id)?;
        // Phase 4: post-commit, post-finalize audit append. If this
        // fails, the graph is durable and the pending diff is
        // already gone — no retry is possible (or needed). The
        // error propagates so the renderer can re-export the audit
        // chain (see `project_audit_sync`).
        let audit_chain_head = Self::ai_audit_append_at_root(
            &committed.project_root,
            committed.plan_scope,
            &committed.diff,
            DiffStatus::Accepted,
            None,
        )?;
        Ok(AiAcceptOutcome {
            ok: true,
            diff_id: diff_id.to_owned(),
            op_count: committed.op_count,
            applied_count: committed.applied_count,
            skipped: committed.skipped,
            command_ids: committed.command_ids,
            audit_chain_head,
        })
    }

    /// Phases 1+2 of `ai_accept_diff`: open the project, convert
    /// the diff to commands, and commit the batch in a single SQL
    /// transaction. Returns the post-commit data the caller needs
    /// to (a) finalize the registry entry and (b) write the AI
    /// audit envelope.
    ///
    /// Splitting this off from the audit-append step is the
    /// `BUG_0001 (round 3)` fix — see [`Self::ai_accept_diff`] for
    /// the four-phase rationale.
    fn ai_accept_diff_commit(
        &mut self,
        pending: PendingDiff,
    ) -> Result<AiAcceptCommitted, BridgeServiceError> {
        let project_path = pending.project_path.clone();
        // There are **two** scopes in play during an AI accept, and
        // they intentionally do not have to agree:
        //
        //   * `plan_scope` — the engine scope the user *invoked*
        //     the plan from. For `plan_detection` / `plan_to_wall`
        //     this can be `Draft` (a 2D drafter detecting walls in
        //     an imported plan) even though the resulting commands
        //     are `Design` walls. The AI tool registry
        //     (`crates/aec_ai/data/ai_tools.json`) declares
        //     `allowed_scopes: ["design", "draft"]` for exactly
        //     this workflow. We retain it as a provenance label on
        //     the AI audit envelope so the audit log answers "what
        //     UI mode produced this accept?" honestly.
        //
        //   * `engine_scope` — the scope the `CommandEngine` must
        //     be opened at to apply the emitted commands. This is
        //     derived from `conversion.commands[0].scope`, which
        //     is the intrinsic scope of the `CommandKind` itself
        //     (`Design` for `CreateWall`, `Draft` for
        //     `DrawPrimitive`, `Deliver` for `CreateRevision`). The
        //     journal entries the engine writes get tagged with
        //     this scope so undo/redo validation works: a later
        //     `Cmd-Z` issued from a Design session can undo a
        //     wall-create even if the original accept happened in
        //     a Draft session.
        //
        // `BUG_0001 (round 4)` fix: opening the engine at
        // `plan_scope` rather than `engine_scope` broke the
        // Draft-launched plan_detection / plan_to_wall path
        // because the batch-scope guard (added in round 3) rejects
        // when `commands[0].scope (Design) != engine.active_scope
        // (Draft)`. Splitting the two scopes here restores the
        // intended behaviour and keeps the audit log faithful.
        let plan_scope = pending.scope;
        let diff = pending.diff;
        let op_count = u32::try_from(diff.operations.len()).unwrap_or(u32::MAX);
        // Open the package + graph once. We need the graph for the
        // converter's `Update` / `Delete` dispatch and the
        // connection for `command_apply_batch`. We keep the `pkg`
        // binding (instead of `_pkg`) so the AI audit append can
        // reuse it without a second key-derive + PRAGMA cipher
        // dance — see `ANALYSIS_0003`.
        let (pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(&project_path, &self.master_key)?;
        let graph = aec_command::ProjectGraph::load(&conn)
            .map_err(|e| BridgeServiceError::Command(e.to_string()))?;
        let conversion =
            aec_command::diff_to_commands(&diff, &graph, aec_command::ApplyDefaults::default());
        let skipped: Vec<AiAcceptSkippedJs> = conversion
            .skipped
            .iter()
            .map(|s| AiAcceptSkippedJs {
                op_index: u32::try_from(s.op_index).unwrap_or(u32::MAX),
                reason: s.reason.clone(),
            })
            .collect();
        // Capture the operation-level applied count from the
        // converter *before* `conversion.commands` is moved into
        // the engine below. This is the `BUG_0001 (round 5)` fix:
        // the converter knows which operations produced at least
        // one command, so it reports the operation count directly
        // rather than us trying to derive it from `commands.len()`
        // (commands can fan out per op) or
        // `op_count - skipped.len()` (a polyline can produce both
        // commands and per-segment skips for the same op).
        let applied_count = u32::try_from(conversion.applied_op_count).unwrap_or(u32::MAX);
        // `command_apply_batch` accepts an empty Vec as a no-op,
        // which is what we want when every operation in the diff
        // is unsupported (e.g. all render_doctor diagnostics). The
        // accept still completes successfully — the AI audit log
        // will capture the attempted-but-skipped operations.
        let applied_results: Vec<CommandApplyResult> = if conversion.commands.is_empty() {
            Vec::new()
        } else {
            // Reuse the graph we already loaded for `diff_to_commands`
            // instead of letting `CommandEngine::open` re-read the
            // `entities` table a second time. The graph is moved
            // into the engine here (we no longer need it after the
            // converter ran) — this is the
            // `ANALYSIS_0003 (round 2)` double-load fix.
            //
            // Engine scope = `commands[0].scope` (the intrinsic
            // scope of the emitted commands), NOT `plan_scope`
            // (the UI launch context). The batch's internal
            // consistency guard already requires every command in
            // a batch to share the same scope, so any command
            // satisfies the role of "canonical batch scope".
            //
            // This is the `BUG_0001 (round 4)` fix — opening at
            // `plan_scope` tripped the batch scope guard whenever
            // the renderer dispatched a Draft-launched
            // `plan_detection` (which legitimately emits Design
            // walls).
            let engine_scope = conversion.commands[0].scope;
            let mut engine =
                aec_command::CommandEngine::open_with_graph(&conn, engine_scope, graph)
                    .map_err(|e| BridgeServiceError::Command(e.to_string()))?;
            let results = engine
                .execute_persistent_batch(conversion.commands, &mut conn)
                .map_err(|e| BridgeServiceError::Command(e.to_string()))?;
            self.invalidate_status_cache_for(&project_path);
            let undo_len = engine.undo_len() as u32;
            let redo_len = engine.redo_len() as u32;
            results
                .into_iter()
                .map(|r| CommandApplyResult {
                    command_id: r.command_id,
                    applied: r.applied,
                    undo_len,
                    redo_len,
                })
                .collect()
        };
        let command_ids: Vec<String> = applied_results
            .iter()
            .map(|r| r.command_id.as_str().to_owned())
            .collect();
        // SQL is committed; capture the project root we already
        // hold so phase 4 (audit append) doesn't have to re-open
        // the package. The `pkg` binding is intentionally dropped
        // at end of scope — the audit logger only needs the project
        // root path, not a keyed package handle.
        let project_root = pkg.root().to_path_buf();
        drop(pkg);
        Ok(AiAcceptCommitted {
            project_root,
            plan_scope,
            diff,
            op_count,
            applied_count,
            skipped,
            command_ids,
        })
    }

    /// Mark a pending diff as rejected and record the rejection in
    /// the project's AI audit log.
    ///
    /// Phase 11 task 11 — the previous incarnation just dropped
    /// the pending entry. The real reject path logs the rejection
    /// to `<project>/audit/ai_audit.jsonl` via
    /// [`AiAuditLogger::log_rejection`] so the AI provenance trail
    /// retains *all* model proposals (accepted and rejected) for
    /// later analysis. The `reason` argument is free-form text
    /// supplied by the renderer; an empty `reason` is recorded as
    /// the empty string rather than as missing.
    ///
    /// Devin Review `ANALYSIS_0001` (round 1): takes `&self`
    /// (not `&mut self`) so the napi shim can use
    /// `with_service_ref_fallible` (a read-lock on the service
    /// singleton). The reject path does not mutate `BridgeService`
    /// directly — `ai_state.peek_diff` / `finalize_diff` already
    /// take `&self` and route all state changes through the
    /// internal locks inside [`crate::ai_state::AiState`], and the
    /// audit append (`Self::ai_audit_append_at_root`) is a static
    /// associated function. Keeping reject under a *read* lock
    /// means a renderer that fires off `status_poll` /
    /// `list_render_jobs` while a reject is in flight no longer
    /// serializes against the reject's disk I/O — they run
    /// concurrently. (Accept *must* hold the write lock because
    /// `command_apply_on_conn` mutates the project graph.)
    pub fn ai_reject_diff(
        &self,
        diff_id: &str,
        reason: Option<&str>,
    ) -> Result<AiRejectOutcome, BridgeServiceError> {
        // `BUG_0001 (round 2)`: same peek-then-finalize discipline
        // as `ai_accept_diff` — see that method for the full
        // rationale. If the audit append fails (disk-full, missing
        // project, etc.), the pending entry survives so the
        // renderer can retry the reject (or escalate via the
        // pending-diff inspector).
        //
        // **Ordering note (`ANALYSIS_0001`):** the reject path
        // intentionally runs *audit append before finalize*, the
        // mirror of the accept path's *finalize before audit*. The
        // asymmetry is by design and reflects the different
        // worst-case failure modes:
        //   * Accept has a SQL commit that mutates the project
        //     graph; finalizing AFTER commit prevents a transient
        //     audit-append failure from re-entering the converter
        //     on retry (which generates fresh `EntityId::new()`
        //     UUIDs and would double-insert every wall/furniture
        //     row). Worst case the accept path defends against is
        //     *data-integrity violation* — duplicated graph
        //     entities.
        //   * Reject does not mutate the graph, so the only state
        //     at risk is the AI audit log itself. Auditing BEFORE
        //     finalize means a transient finalize failure (rare:
        //     would require a concurrent finalize racing this
        //     thread) leaves the diff pending and the audit
        //     containing a reject entry. The renderer's retry
        //     would then write a duplicate audit entry — visible,
        //     deduplicable by `diff_id`, but harmless. The
        //     alternative ordering (finalize → audit) would expose
        //     a strictly worse failure mode: an audit-append
        //     failure after the diff is already finalized would
        //     SILENTLY drop the rejection from the security log
        //     with no retry path. For an audit / forensic surface,
        //     "loud duplicate" beats "silent gap" — so this is the
        //     correct asymmetry.
        let pending = self.ai_state.peek_diff(diff_id)?;
        let outcome = Self::ai_reject_diff_inner(diff_id, pending, reason)?;
        self.ai_state.finalize_diff(diff_id)?;
        Ok(outcome)
    }

    // Static associated function: the reject path no longer needs
    // any `BridgeService` state (the master key is no longer
    // consulted now that we use manifest-only `ProjectPackage::open`
    // — see `ANALYSIS_0005 (round 3)`), so leaving this as a
    // `&mut self` method would trip `clippy::unused_self`.
    fn ai_reject_diff_inner(
        diff_id: &str,
        pending: PendingDiff,
        reason: Option<&str>,
    ) -> Result<AiRejectOutcome, BridgeServiceError> {
        let project_path = pending.project_path.clone();
        let scope = pending.scope;
        let diff = pending.diff;
        let op_count = u32::try_from(diff.operations.len()).unwrap_or(u32::MAX);
        // Reject does not mutate the graph, so the only state it
        // touches is the AI audit log. We open the package via the
        // manifest-only `ProjectPackage::open` rather than
        // `open_with_master_key`: the audit append only needs the
        // project root path (the AI audit JSONL lives at
        // `<root>/audit/ai_audit.jsonl`), and the manifest-only
        // open still validates that the directory is a real
        // project package (`is_dir` + `PACKAGE_DIRS` walk) so a
        // stale `pending.project_path` pointing at a moved /
        // deleted project still surfaces as an error before the
        // audit append tries to create a phantom directory tree.
        // This is the `ANALYSIS_0005 (round 3)` fix — the full
        // keyed open ran `derive_project_key` + the SQLCipher
        // `PRAGMA cipher_*` sequence + the migration walk all for
        // a path lookup, ~1-2 ms of avoidable work per reject.
        let pkg = ProjectPackage::open(&project_path)?;
        let audit_chain_head =
            Self::ai_audit_append_at_root(pkg.root(), scope, &diff, DiffStatus::Rejected, reason)?;
        Ok(AiRejectOutcome {
            ok: true,
            diff_id: diff_id.to_owned(),
            op_count,
            reason: reason.map(std::string::ToString::to_string),
            audit_chain_head,
        })
    }

    /// Internal helper: append an `AiAuditRecord` to the project's
    /// AI audit log (`<project>/audit/ai_audit.jsonl`) and return
    /// the new chain head.
    ///
    /// Takes the project root path by reference so callers that
    /// already validated the package (the accept path holds a
    /// `ProjectPackage` opened for the SQL commit; the reject path
    /// holds a manifest-only `ProjectPackage::open`) can hand the
    /// root through without re-deriving the project key or
    /// re-issuing the `PRAGMA cipher_*` sequence. This is the
    /// shape `ANALYSIS_0003 (round 2)` + `ANALYSIS_0005 (round 3)`
    /// converged to — neither caller needs a keyed package handle
    /// for the audit append, only the root path.
    ///
    /// The AI audit log is a separate hash chain from the main
    /// command audit log (`<project>/audit/log.jsonl`) so the AI
    /// lifecycle (plan → accept / reject) lives on its own
    /// tamper-evident trail. AI-accepted commands ALSO appear in
    /// the main log via `command_apply_batch`; this second log
    /// answers "how many of the model's proposals did the user
    /// accept?" without grepping through every command's actor
    /// field.
    fn ai_audit_append_at_root(
        project_root: &Path,
        scope: Scope,
        diff: &aec_ai::Diff,
        status: DiffStatus,
        reason: Option<&str>,
    ) -> Result<String, BridgeServiceError> {
        let ai_audit_path = project_root.join("audit").join("ai_audit.jsonl");
        let mut logger = AiAuditLogger::open(&ai_audit_path)?;
        match status {
            DiffStatus::Accepted => {
                logger.log_acceptance(diff, scope)?;
            }
            DiffStatus::Rejected => {
                logger.log_rejection(diff, scope, reason.unwrap_or(""))?;
            }
            DiffStatus::Pending => {
                // `Pending` is a registry-only state and never
                // reaches this path — the audit log only records
                // terminal transitions.
                return Err(BridgeServiceError::Ai(
                    "ai_audit_append called with Pending status".into(),
                ));
            }
        }
        Ok(logger.head().to_string())
    }

    /// Cancel any in-flight or queued AI work by killing the sidecar
    /// process. Idempotent: cancelling when no sidecar is running is a
    /// no-op. The next `ai_plan` call will lazily respawn.
    ///
    /// During a cold-spawn this method blocks on the `handle_slot`
    /// mutex until the spawn completes (the spawn is non-cancellable
    /// today; making the underlying `Child::wait` interruptible is a
    /// follow-up). The napi `ai_cancel_job` is `spawn_blocking`-
    /// wrapped, so the libuv main thread is free during that wait
    /// and the renderer UI stays responsive.
    pub fn ai_cancel_job(&self, _job_id: &str) -> Result<AiCancelResult, BridgeServiceError> {
        self.ai_state.cancel_job()?;
        Ok(AiCancelResult { cancelled: true })
    }

    /// Read the current sidecar lifecycle state. Cheap, lock-free in
    /// the contention sense — takes the `runtime` `RwLock` *read*
    /// side and the `pending_diffs` mutex, neither of which is held
    /// for more than microseconds anywhere else in the codebase. The
    /// renderer polls this every ~500 ms while an `ai_plan` is in
    /// flight to show the user a "model loading" or "model ready"
    /// indicator; concurrent polls during a cold-spawn observe the
    /// `Loading` state immediately.
    pub fn ai_runtime_status(&self) -> Result<AiRuntimeStatusReport, BridgeServiceError> {
        let snap = self.ai_state.snapshot()?;
        Ok(AiRuntimeStatusReport {
            state: state_string(snap.state),
            last_error: snap.last_error,
            pending_diff_ids: snap.pending_diff_ids,
        })
    }

    // ----- Draft scope (DXF import/export + drawing + sheet/layer ----- //

    /// Import a DXF file at `dxf_path` into the project graph. Each
    /// importable DXF entity (line / polyline / arc / circle /
    /// ellipse / text) is converted to a modelling
    /// [`aec_cad::primitives::Primitive`], wrapped in
    /// [`aec_command::commands::draft::DrawPrimitive`], and applied
    /// through [`Self::command_apply`] so each import is journaled,
    /// auditable, and undo-able. Entities without a modelling
    /// counterpart (Insert / Dimension / Spline / Hatch) are skipped
    /// for now &mdash; see [`aec_cad::dxf::dxf_to_primitive`] for the
    /// supported set.
    pub fn draft_import_dxf(
        &mut self,
        project_path: &str,
        dxf_path: &str,
    ) -> Result<DraftImportDxfResult, BridgeServiceError> {
        use aec_cad::dxf::{dxf_to_primitive, DxfReader};
        use aec_command::commands::draft::DrawPrimitive;
        use aec_command::commands::CommandKind;
        use aec_core::types::EntityId;
        use std::fs::File;

        let f = File::open(dxf_path).map_err(|e| {
            BridgeServiceError::Io(std::io::Error::other(format!(
                "draft_import_dxf: open {dxf_path}: {e}"
            )))
        })?;
        let doc = DxfReader::read(f)
            .map_err(|e| BridgeServiceError::Invalid(format!("draft_import_dxf: parse: {e}")))?;
        let mut layer_count = doc.layers.len() as u32;
        let block_count = doc.block_records.len() as u32;
        // Two-pass: convert every supported DXF entity to a
        // `DrawPrimitive` command up front, count the rest as
        // skipped, and apply the whole batch in one SQL transaction.
        // Earlier versions issued N independent `command_apply`
        // calls — O(N) project-package opens, O(N) engine reads of
        // the entire entity table, O(N) audit-chain extensions.
        // Routing through `command_apply_batch` collapses that to a
        // single open, a single engine load, and a single
        // transaction; the audit chain still records each gesture
        // distinctly (see `execute_persistent_batch` phase 3).
        let mut commands = Vec::with_capacity(doc.entities.len());
        let mut skipped = 0u32;
        for entity in &doc.entities {
            match dxf_to_primitive(entity) {
                Some(prim) => {
                    commands.push(Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
                        entity_id: EntityId::new(),
                        primitive: prim,
                    })));
                }
                None => skipped += 1,
            }
        }
        let entity_count = commands.len() as u32;
        self.command_apply_batch(project_path, commands)?;
        // Ensure we always report at least one layer (the "0" layer
        // exists by default in every DXF document).
        if layer_count == 0 {
            layer_count = 1;
        }
        Ok(DraftImportDxfResult {
            entity_count,
            layer_count,
            block_count,
            skipped_count: skipped,
        })
    }

    /// Export the project graph's draft primitives to a DXF file at
    /// `dxf_path`. Walks the on-disk graph (rebuilt from the
    /// SQLCipher `entities` table), filters to primitive records, and
    /// converts each through
    /// [`aec_cad::dxf::primitive_to_dxf`].
    pub fn draft_export_dxf(
        &self,
        project_path: &str,
        dxf_path: &str,
    ) -> Result<DraftExportDxfResult, BridgeServiceError> {
        use aec_cad::dxf::{primitive_to_dxf, DxfDocument, DxfWriter};
        use aec_command::commands::draft::DrawPrimitive;
        use std::fs::File;

        let (_pkg, conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let engine = CommandEngine::open(&conn, Scope::Draft)?;
        let mut doc = DxfDocument::default();
        for rec in engine.graph().iter() {
            if rec.kind != "primitive" {
                continue;
            }
            let prim: DrawPrimitive = match serde_json::from_value(rec.body.clone()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if let Some(d) = primitive_to_dxf(&prim.primitive) {
                doc.entities.push(d);
            }
        }
        let entity_count = doc.entities.len() as u32;
        let mut f = File::create(dxf_path).map_err(|e| {
            BridgeServiceError::Io(std::io::Error::other(format!(
                "draft_export_dxf: create {dxf_path}: {e}"
            )))
        })?;
        DxfWriter::write(&doc, &mut f)
            .map_err(|e| BridgeServiceError::Export(format!("draft_export_dxf: write: {e}")))?;
        let file_size = std::fs::metadata(dxf_path).map_or(0, |m| m.len());
        Ok(DraftExportDxfResult {
            path: dxf_path.to_string(),
            entity_count,
            file_size,
        })
    }

    // ----- Deliver scope (revision snapshot + diff) ----- //

    /// Capture a revision snapshot of the project.
    ///
    /// The snapshot includes (a) the project graph entities, hashed
    /// via BLAKE3 of their canonical serialised form, (b) the audit
    /// chain head pointer at snapshot time, and (c) manifest
    /// metadata for UI display. The snapshot is persisted to
    /// `<project>/revisions/<id>.json` atomically (write to .tmp,
    /// rename). The journaled
    /// [`aec_command::commands::deliver::CreateRevision`] command
    /// records the user-visible gesture in the audit chain.
    pub fn deliver_create_revision(
        &mut self,
        project_path: &str,
        tag: &str,
        description: &str,
        caller_entities: Option<Vec<RevisionTrackedEntity>>,
    ) -> Result<RevisionSummary, BridgeServiceError> {
        use aec_command::commands::deliver::CreateRevision;
        use aec_command::commands::CommandKind;
        use aec_core::revision::{RevisionDraft, RevisionEntity, RevisionStore};

        // 1) Open package + journal the command (audit trail + undo
        //    so the gesture is reversible if a user mis-tags). We
        //    share the same `conn` between the command-apply step
        //    and the post-apply enumeration in (2) so the snapshot
        //    sees exactly the state the command just committed —
        //    `command_apply_on_conn` reuses the connection instead
        //    of opening a second one. (Earlier iterations had two
        //    independent opens; if `CreateRevision` ever gains
        //    graph-mutating deltas, the outer conn would see stale
        //    state until the next reopen.)
        let (pkg, mut conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let cmd = Command::user(CommandKind::CreateRevision(CreateRevision {
            tag: tag.to_string(),
            description: description.to_string(),
            revision_id: None,
        }));
        let apply_res = self.command_apply_on_conn(project_path, &mut conn, cmd)?;

        // 2) Build the tracked-entity list. Prefer caller-supplied
        //    entries when present (so the renderer can include
        //    domain-specific entities the bridge can't reach, e.g.
        //    schedule rows held in renderer memory). Otherwise
        //    enumerate the on-disk graph and hash each entity's
        //    canonical body via BLAKE3.
        let mut draft = RevisionDraft::new(
            pkg.manifest().project_id.clone(),
            tag.to_string(),
            description.to_string(),
            // Audit head at the moment of snapshot. We re-read the
            // log here rather than threading it through
            // command_apply because the JSONL is the source of
            // truth and command_apply doesn't expose the head.
            read_audit_head(pkg.root())?,
            pkg.manifest().name.clone(),
            pkg.manifest().app_version.clone(),
        );
        if let Some(entries) = caller_entities {
            for e in entries {
                draft = draft.add_entity(RevisionEntity {
                    category: e.category,
                    id: e.id,
                    payload_hash: e.payload_hash,
                    label: e.label,
                });
            }
        } else {
            // The deliver-scope command engine doesn't expose the
            // graph, so re-open as Design (the scope that owns the
            // entity store) to enumerate tracked entities for the
            // snapshot.
            let engine = CommandEngine::open(&conn, Scope::Design)?;
            for rec in engine.graph().iter() {
                let canonical = serde_json::to_vec(&rec.body)
                    .map_err(|e| BridgeServiceError::Command(format!("revision serialize: {e}")))?;
                let hash = blake3::hash(&canonical).to_hex().to_string();
                draft = draft.add_entity(RevisionEntity {
                    category: rec.kind.clone(),
                    id: rec.id.to_string(),
                    payload_hash: hash,
                    label: None,
                });
            }
        }

        // 3) Persist to revisions/<id>.json.
        let store = RevisionStore::open(pkg.root().join("revisions"))?;
        let revision = store.create(draft)?;

        // Discard the unused command result detail; we surface the
        // revision summary instead.
        let _ = apply_res;

        Ok(revision_to_summary(revision))
    }

    /// Return the project's revision summaries in chronological
    /// order. Suitable for the Deliver-mode "Versions" pane.
    pub fn deliver_list_revisions(
        &self,
        project_path: &str,
    ) -> Result<Vec<RevisionSummary>, BridgeServiceError> {
        use aec_core::revision::RevisionStore;
        let (pkg, _conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let store = RevisionStore::open(pkg.root().join("revisions"))?;
        Ok(store.list()?.into_iter().map(revision_to_summary).collect())
    }

    /// Diff two revisions at the tracked-entity level. Both revisions
    /// must exist in the project's `revisions/` directory.
    pub fn deliver_compare_revisions(
        &self,
        project_path: &str,
        base_id: &str,
        head_id: &str,
    ) -> Result<RevisionDiffReport, BridgeServiceError> {
        use aec_core::revision::RevisionStore;
        use aec_core::version_diff::compare_revisions;

        let (pkg, _conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, &self.master_key)?;
        let store = RevisionStore::open(pkg.root().join("revisions"))?;
        let base = store
            .get(base_id)?
            .ok_or_else(|| BridgeServiceError::Invalid(format!("revision not found: {base_id}")))?;
        let head = store
            .get(head_id)?
            .ok_or_else(|| BridgeServiceError::Invalid(format!("revision not found: {head_id}")))?;
        let diff = compare_revisions(&base, &head);
        let mut by_category = std::collections::BTreeMap::new();
        for (cat, counts) in &diff.by_category {
            by_category.insert(
                cat.clone(),
                RevisionDiffCounts {
                    added: counts.added as u32,
                    removed: counts.removed as u32,
                    modified: counts.modified as u32,
                    unchanged: counts.unchanged as u32,
                },
            );
        }
        let changes = diff
            .changes
            .into_iter()
            .map(|c| RevisionEntityChange {
                category: c.category,
                id: c.id,
                kind: match c.kind {
                    aec_core::version_diff::EntityChangeKind::Added => "added".into(),
                    aec_core::version_diff::EntityChangeKind::Removed => "removed".into(),
                    aec_core::version_diff::EntityChangeKind::Modified => "modified".into(),
                    aec_core::version_diff::EntityChangeKind::Unchanged => "unchanged".into(),
                },
                before_hash: c.before_hash,
                after_hash: c.after_hash,
                label: c.label,
            })
            .collect();
        Ok(RevisionDiffReport {
            base_revision_id: base_id.to_string(),
            head_revision_id: head_id.to_string(),
            by_category,
            changes,
        })
    }

    // ===== KChat (Phase 12) =====

    /// Snapshot the current KChat connection state. Cheap; the
    /// renderer polls this from the status indicator every ~5 s.
    pub fn kchat_status(&self) -> crate::kchat_state::KChatStatusReport {
        self.kchat_state.status()
    }

    /// Re-run KChat discovery and replace the active publisher.
    /// Triggered by the Settings page's "Reload KChat connection"
    /// button.
    pub fn kchat_reload(&self) -> crate::kchat_state::KChatStatusReport {
        self.kchat_state.reload()
    }

    /// Phase 12 Task 30 — flip the master KChat enable switch. When
    /// `enabled` is `false`, subsequent [`Self::kchat_publish`] and
    /// [`Self::kchat_ingest_reviews`] calls return
    /// [`aec_core::kchat::KChatError::Disabled`] without touching the
    /// transport. Used by the Settings page's "Disable KChat" toggle.
    pub fn kchat_set_enabled(&self, enabled: bool) {
        self.kchat_state.set_enabled(enabled);
    }

    /// Phase 12 Task 30 — mirror of [`Self::kchat_set_enabled`].
    pub fn kchat_is_enabled(&self) -> bool {
        self.kchat_state.is_enabled()
    }

    /// Phase 15 — promote the Rust-side `publisher_kind` marker to
    /// `loopback_http`. Called by the Electron host once
    /// `kchatLocalApi` has bound on `127.0.0.1` so the bridge's
    /// status snapshot (consumed by future Rust-side telemetry /
    /// audit consumers) agrees with the Electron-side
    /// `kchat:status` IPC payload. Returns the post-mutation
    /// status so the caller can avoid a second snapshot.
    pub fn kchat_mark_loopback_active(&self) -> crate::kchat_state::KChatStatusReport {
        self.kchat_state.mark_loopback_active();
        self.kchat_state.status()
    }

    /// Phase 15 — demote the Rust-side `publisher_kind` marker
    /// back to `in_memory`. Called by the Electron host on
    /// shutdown so the next status snapshot surfaces the headless
    /// state honestly.
    pub fn kchat_mark_loopback_inactive(&self) -> crate::kchat_state::KChatStatusReport {
        self.kchat_state.mark_loopback_inactive();
        self.kchat_state.status()
    }

    /// Publish an artifact card through the active publisher.
    pub fn kchat_publish(
        &self,
        card: aec_core::kchat::ArtifactCard,
    ) -> Result<aec_core::kchat::PublishResult, BridgeServiceError> {
        self.kchat_state
            .publish(card)
            .map_err(|e| BridgeServiceError::Core(format!("kchat publish: {e}")))
    }

    /// Pull review comments newer than `since_iso` from the active
    /// publisher's thread. Returns `(comments, cards)`. The in-memory
    /// fallback always returns `(vec![], vec![])`.
    pub fn kchat_ingest_reviews(
        &self,
        thread_id: &str,
        since_iso: Option<String>,
    ) -> Result<KChatIngestReport, BridgeServiceError> {
        let (comments, cards) = self
            .kchat_state
            .ingest_reviews(thread_id, since_iso)
            .map_err(|e| BridgeServiceError::Core(format!("kchat ingest: {e}")))?;
        Ok(KChatIngestReport {
            thread_id: thread_id.to_string(),
            comments,
            cards,
        })
    }

    // ----- Viewport (Phase 12) ---------------------------------
    //
    // The viewport service owns its own GPU device and pipelines and
    // is safe to call even when no adapter is available — it will
    // simply report `"unavailable"` instead of crashing. See
    // [`crate::viewport_service`] for the design rationale.

    /// Borrow the process-wide viewport service. Exposed for tests
    /// and the N-API layer; downstream code should prefer the
    /// typed methods below.
    pub fn __viewport_service(&self) -> &crate::viewport_service::ViewportService {
        &self.viewport_service
    }

    /// Resize the viewport's off-screen surface.
    pub fn viewport_resize(
        &self,
        width: u32,
        height: u32,
    ) -> Result<crate::viewport_service::ViewportStatusReport, BridgeServiceError> {
        self.viewport_service
            .resize(width, height)
            .map_err(|e| BridgeServiceError::Invalid(format!("viewport resize: {e}")))?;
        Ok(self.viewport_service.status())
    }

    /// Apply a mouse / camera input to the viewport.
    pub fn viewport_input(
        &self,
        input: crate::viewport_service::ViewportInput,
    ) -> Result<crate::viewport_service::ViewportCameraReport, BridgeServiceError> {
        self.viewport_service.apply_input(input);
        Ok(self.viewport_service.camera_report())
    }

    /// Request the next viewport frame. The actual pixel bytes are
    /// available via the underlying [`SurfaceManager`]; this method
    /// returns a deterministic summary (frame index, camera state,
    /// `presented` / `coalesced` / `unavailable`) that the renderer
    /// can use to drive its diagnostic UI.
    pub fn viewport_request_frame(
        &self,
    ) -> Result<crate::viewport_service::ViewportFrameReport, BridgeServiceError> {
        self.viewport_service
            .request_frame()
            .map_err(|e| BridgeServiceError::Core(format!("viewport frame: {e}")))
    }

    /// Status report for the viewport diagnostics panel.
    pub fn viewport_status(&self) -> crate::viewport_service::ViewportStatusReport {
        self.viewport_service.status()
    }
}

/// Bridge-shaped review-ingest report. Renderer-facing alias for the
/// `(comments, cards)` tuple — `serde`-friendly so it crosses the
/// N-API boundary cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KChatIngestReport {
    pub thread_id: String,
    pub comments: Vec<aec_core::kchat::ReviewComment>,
    pub cards: Vec<aec_core::kchat::ReviewCard>,
}

fn revision_to_summary(r: aec_core::revision::Revision) -> RevisionSummary {
    RevisionSummary {
        revision_id: r.id,
        tag: r.tag,
        description: r.description,
        created_at: r.created_at.to_rfc3339(),
        audit_chain_head: r.audit_chain_head,
        manifest_name: r.manifest_name,
        manifest_app_version: r.manifest_app_version,
        tracked_entities: r
            .tracked_entities
            .into_iter()
            .map(|e| RevisionTrackedEntity {
                category: e.category,
                id: e.id,
                payload_hash: e.payload_hash,
                label: e.label,
            })
            .collect(),
    }
}

/// Read the head BLAKE3 hash of the project's append-only audit log
/// (an empty string if the chain hasn't been initialised yet — a
/// brand-new project before its first command).
fn read_audit_head(project_root: &Path) -> Result<String, BridgeServiceError> {
    let path = project_root.join("audit").join("log.jsonl");
    if !path.exists() {
        return Ok(String::new());
    }
    let log = AuditLog::open(path)?;
    Ok(log.head().to_string())
}

/// Result returned by [`BridgeService::draft_import_dxf`]. Counts are
/// post-import; `skipped_count` covers DXF entities that don't have a
/// modelling primitive counterpart yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftImportDxfResult {
    pub entity_count: u32,
    pub layer_count: u32,
    pub block_count: u32,
    pub skipped_count: u32,
}

/// Result returned by [`BridgeService::draft_export_dxf`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftExportDxfResult {
    pub path: String,
    pub entity_count: u32,
    pub file_size: u64,
}

/// JSON-friendly mirror of [`aec_core::revision::Revision`] used by
/// the [`BridgeService::deliver_*`] endpoints. Field names align 1:1
/// (via serde rename) with the renderer-side `RevisionSummary`
/// interface in `apps/desktop/electron/bridge.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionSummary {
    pub revision_id: String,
    pub tag: String,
    pub description: String,
    pub created_at: String,
    pub audit_chain_head: String,
    pub manifest_name: String,
    pub manifest_app_version: String,
    pub tracked_entities: Vec<RevisionTrackedEntity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionTrackedEntity {
    pub category: String,
    pub id: String,
    pub payload_hash: String,
    pub label: Option<String>,
}

/// JSON-friendly mirror of [`aec_core::version_diff::VersionDiff`].
/// Field names align 1:1 (via serde rename) with the renderer-side
/// `VersionDiffSummary` interface in `apps/desktop/electron/bridge.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionDiffReport {
    pub base_revision_id: String,
    pub head_revision_id: String,
    pub by_category: std::collections::BTreeMap<String, RevisionDiffCounts>,
    pub changes: Vec<RevisionEntityChange>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionDiffCounts {
    pub added: u32,
    pub removed: u32,
    pub modified: u32,
    pub unchanged: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionEntityChange {
    pub category: String,
    pub id: String,
    /// One of `"added"` / `"removed"` / `"modified"` / `"unchanged"`.
    pub kind: String,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub label: Option<String>,
}

/// Render an [`aec_ai::RuntimeState`] as the lowercase wire string the
/// renderer expects.
fn state_string(s: aec_ai::RuntimeState) -> String {
    match s {
        aec_ai::RuntimeState::Idle => "idle".into(),
        aec_ai::RuntimeState::Loading => "loading".into(),
        aec_ai::RuntimeState::Ready => "ready".into(),
        aec_ai::RuntimeState::Failed => "failed".into(),
    }
}

/// Default sidecar runtime config: in production this comes from
/// `workers/ai/config.json`; here we bake the same defaults so the
/// bridge can boot without the file on disk (the renderer never
/// reads this — the sidecar Python wrapper does — but the bridge
/// needs a `RuntimeConfig` to drive the lifecycle state machine).
fn default_ai_runtime_config() -> aec_ai::RuntimeConfig {
    aec_ai::RuntimeConfig::default()
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

/// Persist the commands emitted by
/// [`aec_command::template_apply::template_to_commands`] into the
/// freshly-created project's SQLCipher database, then drop a JSON
/// sidecar in `<project>/audit/template_instantiation.json` that
/// captures the entity counts, the originating template key, and any
/// rooms that were skipped by the fail-soft instantiator.
///
/// Atomicity: the whole batch lands in one SQL transaction inside
/// `CommandEngine::execute_persistent_batch`. If any single command
/// fails validation the transaction is rolled back and the function
/// returns an error; the caller deletes the now-incomplete project
/// directory in that branch.
///
/// Takes `outcome` **by value** (Devin Review `ANALYSIS_0005` on
/// PR #51): the previous shape took `&InstantiationOutcome` and
/// then deep-cloned `outcome.commands` into
/// `execute_persistent_batch`. The villa template emits ~77
/// commands (11 rooms × 7 commands each + lighting + cameras),
/// each carrying a serialised JSON body, so the clone was a real
/// allocation hot-spot dominated only by the SQL transaction that
/// follows. Consuming the outcome lets us move
/// `outcome.commands` straight into the engine and clone nothing.
///
/// Engine scope is derived from `outcome.commands[0].scope`
/// (Devin Review `ANALYSIS_0004` on PR #51): the old shape
/// hardcoded `Scope::Design`, which works today because every
/// `CommandKind` emitted by the template path returns
/// `Scope::Design` from its `scope()` method, but a future
/// template feature emitting a non-Design command (e.g. a Draft
/// `DrawPrimitive` for a 2D plan template, or a Render
/// `SaveCamera` variant) would silently trip a `ScopeMismatch`
/// inside the batch guard. Reading the scope off the commands
/// themselves matches the AI accept path's pattern
/// (`engine_scope = conversion.commands[0].scope`) and removes
/// the brittle-against-extension assumption.
fn apply_template_outcome(
    pkg: &ProjectPackage,
    master_key: &[u8; 32],
    template_key: &str,
    outcome: aec_command::template_apply::InstantiationOutcome,
) -> Result<(), BridgeServiceError> {
    // Destructure once so we can move `commands` into the engine
    // and still borrow the other fields for the sidecar JSON. The
    // batch-internal-consistency guard inside
    // `execute_persistent_batch` will reject any command whose
    // scope differs from `commands[0].scope`, so picking the first
    // command's scope is both correct and uniquely defined.
    let aec_command::template_apply::InstantiationOutcome {
        commands,
        rooms,
        camera_ids,
        lighting_preset,
        skipped,
    } = outcome;
    let applied_command_count = commands.len();
    if !commands.is_empty() {
        let engine_scope = commands[0].scope;
        let mut conn = pkg.open_database(master_key)?;
        let mut engine = CommandEngine::open(&conn, engine_scope)?;
        engine.execute_persistent_batch(commands, &mut conn)?;
        // Drop the connection eagerly so the project package's SQLite
        // file is closed before we touch the audit sidecar.
        drop(engine);
        drop(conn);
    }

    let sidecar_dir = pkg.root().join("audit");
    std::fs::create_dir_all(&sidecar_dir)?;
    let sidecar = sidecar_dir.join("template_instantiation.json");
    let body = serde_json::json!({
        "template_key": template_key,
        "applied_command_count": applied_command_count,
        "room_count": rooms.len(),
        "camera_count": camera_ids.len(),
        "lighting_preset": lighting_preset,
        "skipped": skipped.iter().map(|s| serde_json::json!({
            "storey": s.storey_name,
            "room": s.room_name,
            "reason": s.reason,
        })).collect::<Vec<_>>(),
        "rooms": rooms.iter().map(|r| serde_json::json!({
            "room_id": r.room_id.as_str(),
            "wall_ids": r.wall_ids.iter().map(aec_core::types::EntityId::as_str).collect::<Vec<_>>(),
            "floor_id": r.floor_id.as_str(),
            "ceiling_id": r.ceiling_id.as_str(),
            "storey": r.storey_name,
            "footprint_origin_mm": r.footprint_origin_mm,
            "footprint_size_mm": r.footprint_size_mm,
            "height_mm": r.height_mm,
        })).collect::<Vec<_>>(),
        "camera_ids": camera_ids.iter().map(aec_core::types::EntityId::as_str).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|e| BridgeServiceError::Core(format!("serialise template sidecar: {e}")))?;
    std::fs::write(&sidecar, bytes)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_template(root: &std::path::Path, category: &str, id: &str) {
        // Default: zero-room, zero-camera, no lighting preset — yields
        // an empty command batch on instantiation so tests that focus
        // on later command_apply / command_undo behaviour aren't
        // perturbed by template-emitted entries in the undo journal.
        write_template_with(root, category, id, &[], None, &[]);
    }

    /// Variant of [`write_template`] for tests that want explicit
    /// template content (rooms / lighting preset / cameras). Returns
    /// the template key so the call site can pass it back to
    /// `project_create_from_template`.
    fn write_template_with(
        root: &std::path::Path,
        category: &str,
        id: &str,
        rooms: &[(&str, f64, f64, f64)],
        lighting_preset: Option<&str>,
        cameras: &[(&str, [f64; 3], [f64; 3], f64)],
    ) -> String {
        let category_dir = root.join(category);
        std::fs::create_dir_all(&category_dir).unwrap();
        let key = format!("{category}.{id}");
        let rooms_json: Vec<serde_json::Value> = rooms
            .iter()
            .map(|(name, width, depth, height)| {
                serde_json::json!({
                    "name": name,
                    "width_mm": width,
                    "depth_mm": depth,
                    "height_mm": height,
                    "origin_mm": [0.0, 0.0, 0.0],
                })
            })
            .collect();
        let cameras_json: Vec<serde_json::Value> = cameras
            .iter()
            .map(|(name, loc, target, focal)| {
                serde_json::json!({
                    "name": name,
                    "location_mm": loc,
                    "target_mm": target,
                    "focal_length_mm": focal,
                })
            })
            .collect();
        let json = serde_json::json!({
            "template_id": key,
            "name": format!("Test {id}"),
            "description": "test fixture",
            "units": "mm",
            "region_defaults": {
                "EU": {"units": "mm", "standards": ["IFC4"]}
            },
            "rooms": rooms_json,
            "default_walls": {
                "exterior_thickness_mm": 250,
                "interior_thickness_mm": 100,
                "material": "wall_white"
            },
            "lighting_preset": lighting_preset,
            "asset_shelf": [],
            "camera_presets": cameras_json
        });
        std::fs::write(category_dir.join(format!("{id}.json")), json.to_string()).unwrap();
        key
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
            extensions_dir: None,
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

    /// `project_open` must adopt the just-opened project's
    /// [`KChatConfig::default_thread_id`] so the renderer's
    /// `kchat:status` poll surfaces it immediately. Verifies the end-
    /// to-end path: write the per-project thread to `manifest.json`,
    /// then call `project_open` and assert the bridge-wide
    /// `KChatState::status` reports the new thread.
    #[test]
    fn project_open_adopts_kchat_default_thread_id_from_manifest() {
        use aec_core::kchat_config::KChatConfig;
        use aec_core::ProjectManifest;

        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "ThreadProj")
            .unwrap();

        // Fresh project: no per-project thread until the manifest
        // is amended.
        assert!(
            s.kchat_status().default_thread_id.is_none(),
            "freshly-created project has no per-project thread"
        );

        // Inject the thread into manifest.json on disk, then re-open
        // through the service so we exercise the real
        // `project_open` → `apply_kchat_project_config` path.
        let manifest_path = std::path::Path::new(&summary.path).join("manifest.json");
        let mut manifest: ProjectManifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest.settings.kchat = Some(KChatConfig::enabled_with_thread("manifest-thread-7"));
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        s.project_open(&summary.path).unwrap();
        let status = s.kchat_status();
        assert_eq!(
            status.default_thread_id,
            Some("manifest-thread-7".to_string()),
            "project_open must push KChatConfig::default_thread_id into the status payload"
        );
    }

    /// Opening a project that omits `kchat` in its manifest must
    /// clear any stale per-project thread the bridge cached from
    /// the previously-open project — otherwise the Deliver page
    /// would point at the wrong thread after a project switch.
    #[test]
    fn project_open_clears_stale_thread_when_new_manifest_omits_kchat() {
        use aec_core::kchat_config::KChatConfig;
        use aec_core::ProjectManifest;

        let (mut s, _g) = service();

        // First project: persist a thread id into its manifest and
        // open it so the bridge caches the thread.
        let first = s
            .project_create_from_template("interior.apartment", "FirstProj")
            .unwrap();
        let first_manifest = std::path::Path::new(&first.path).join("manifest.json");
        let mut m: ProjectManifest =
            serde_json::from_str(&std::fs::read_to_string(&first_manifest).unwrap()).unwrap();
        m.settings.kchat = Some(KChatConfig::enabled_with_thread("first-thread"));
        std::fs::write(&first_manifest, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        s.project_open(&first.path).unwrap();
        assert_eq!(
            s.kchat_status().default_thread_id,
            Some("first-thread".to_string())
        );

        // Second project: no kchat config in its manifest. Opening
        // it must clear the cached thread from FirstProj.
        let second = s
            .project_create_from_template("interior.apartment", "SecondProj")
            .unwrap();
        s.project_open(&second.path).unwrap();
        assert!(
            s.kchat_status().default_thread_id.is_none(),
            "opening a project without a thread must clear the prior project's cache"
        );
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

    /// Round-trip the thumbnail blob through the
    /// `project_set_thumbnail` / `project_get_thumbnail` pair and
    /// verify the exact bytes come back unchanged. Phase 17 Group B
    /// Task 12: the Home page's recent-project grid depends on these
    /// two methods being a faithful mirror.
    #[test]
    fn project_thumbnail_set_get_roundtrip() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "ThumbProj")
            .unwrap();

        // Fresh project: no thumbnail yet.
        let empty = s.project_get_thumbnail(&summary.path).unwrap();
        assert!(empty.is_none(), "fresh project should have no thumbnail");

        // Minimal valid PNG: 1×1 transparent pixel. Captured here so
        // the test exercises the real magic-byte validator end-to-end
        // (not a stub).
        let png: [u8; 67] = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
            0x00, 0x00, 0x00, 0x0D, // IHDR length
            0x49, 0x48, 0x44, 0x52, // IHDR
            0x00, 0x00, 0x00, 0x01, // width = 1
            0x00, 0x00, 0x00, 0x01, // height = 1
            0x08, 0x06, 0x00, 0x00, 0x00, // bit depth / color type / etc.
            0x1F, 0x15, 0xC4, 0x89, // CRC
            0x00, 0x00, 0x00, 0x0D, // IDAT length
            0x49, 0x44, 0x41, 0x54, // IDAT
            0x78, 0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D,
            0xB4, // IDAT data
            0x00, 0x00, 0x00, 0x00, // IEND length
            0x49, 0x45, 0x4E, 0x44, // IEND
            0xAE, 0x42, 0x60, 0x82, // CRC
        ];
        s.project_set_thumbnail(&summary.path, &png, 256, 192)
            .unwrap();

        let got = s.project_get_thumbnail(&summary.path).unwrap().unwrap();
        assert_eq!(got.png, png.to_vec(), "PNG bytes must round-trip exactly");
        assert_eq!(got.width, 256);
        assert_eq!(got.height, 192);
        assert!(
            !got.updated_at.is_empty(),
            "updated_at must be a non-empty ISO-8601 timestamp"
        );

        // Second write replaces the row in place (singleton constraint).
        let png2: Vec<u8> = {
            let mut v = png.to_vec();
            v[20] = 0x02; // tweak one byte so we can tell them apart
            v
        };
        s.project_set_thumbnail(&summary.path, &png2, 512, 384)
            .unwrap();
        let got2 = s.project_get_thumbnail(&summary.path).unwrap().unwrap();
        assert_eq!(got2.png, png2, "second write must replace the blob");
        assert_eq!(got2.width, 512);
        assert_eq!(got2.height, 384);
    }

    /// Every invalid input path on `project_set_thumbnail` must
    /// return a typed `Invalid` error before any SQL is written, so
    /// the renderer can show a clean toast instead of a constraint-
    /// violation stack trace.
    #[test]
    fn project_set_thumbnail_rejects_invalid_input() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "ValidateThumb")
            .unwrap();

        // Empty buffer.
        let err = s
            .project_set_thumbnail(&summary.path, &[], 256, 192)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Invalid(_)));

        // Wrong magic header.
        let err = s
            .project_set_thumbnail(&summary.path, b"NOTPNG\x00\x00rest", 256, 192)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Invalid(_)));

        // Zero dimension.
        let err = s
            .project_set_thumbnail(&summary.path, b"\x89PNG\r\n\x1a\nXX", 0, 192)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Invalid(_)));

        // Over-large dimension.
        let err = s
            .project_set_thumbnail(&summary.path, b"\x89PNG\r\n\x1a\nXX", 256, 5000)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Invalid(_)));

        // Over-large buffer (1 MiB + 1).
        let mut big = vec![0u8; 1024 * 1024 + 1];
        big[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let err = s
            .project_set_thumbnail(&summary.path, &big, 256, 192)
            .unwrap_err();
        assert!(matches!(err, BridgeServiceError::Invalid(_)));

        // None of those should have left a row behind.
        let got = s.project_get_thumbnail(&summary.path).unwrap();
        assert!(
            got.is_none(),
            "rejected writes must leave the thumbnail row untouched"
        );
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
            extensions_dir: None,
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
        // `std::fs::canonicalize` on the same input. The snapshot
        // cache keys on this field, so the contract has to hold up.
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
    fn design_list_assets_saturating_clamps_to_max_limit() {
        // Regression test for the saturating-clamp promise on
        // `AssetListQuery::limit`. A renderer-side bug (or a malicious
        // caller) sending `limit = u32::MAX` would otherwise reach
        // SQLite as `LIMIT 4294967295` and materialise an unbounded
        // result set on a real library. The bridge clamps the
        // effective limit to `DESIGN_LIST_ASSETS_MAX_LIMIT` (10_000).
        //
        // We can't directly observe the clamped value through the
        // public surface (the SQL LIMIT is internal), but we *can*
        // pin that the call succeeds with a sane bound: the seed
        // library has 4 assets, so the response should be exactly
        // 4 — same as the unbounded query — not an error and not
        // a wrapped/truncated list. The companion test
        // `design_list_assets_respects_limit` proves smaller limits
        // still take effect (so the clamp isn't an unconditional
        // override of the user's choice). Together they pin the
        // saturating-clamp invariant.
        let (s, _g) = service();
        let res = s
            .design_list_assets(&AssetListQuery {
                limit: Some(u32::MAX),
                ..AssetListQuery::default()
            })
            .expect("u32::MAX limit must succeed (clamped, not unbounded)");
        assert_eq!(
            res.len(),
            4,
            "clamped query must still return the full seed library"
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
            "seed library has no thumbnail blobs on the list surface; renderer falls back to placeholder card"
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

    // ============================================================
    // Phase 17 Group B Task 11: design_list_materials / design_update_material
    // ============================================================

    #[test]
    fn design_list_materials_returns_default_pack_on_fresh_state() {
        // Fresh service → library seeded with
        // `MaterialLibrary::with_default_pack` (8 starter materials).
        // Pins the contract that the design-mode panel sees real
        // PBR data on the first open, not an empty list that the
        // renderer would have to fall back to hardcoded swatches for.
        let (s, _g) = service();
        let mats = s
            .design_list_materials(&MaterialListQuery::default())
            .expect("design_list_materials succeeds on fresh state");
        assert_eq!(mats.len(), 8, "starter pack has 8 materials");
        // Names are stable across runs (defined in `with_default_pack`)
        // and the renderer relies on them for the swatch label.
        let names: Vec<&str> = mats.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"Light Oak"));
        assert!(names.contains(&"Walnut"));
        assert!(names.contains(&"Carrara Marble"));
        assert!(names.contains(&"Brushed Brass"));
    }

    #[test]
    fn design_list_materials_filters_by_style_tag() {
        // `style_tags = ["industrial"]` → walnut + polished concrete
        // (both tagged `industrial` in the starter pack). Marble
        // (`classical` / `minimal`) and matte white (`minimal`)
        // drop out. Pins AND-tag semantics.
        let (s, _g) = service();
        let q = MaterialListQuery {
            style_tags: vec!["industrial".to_string()],
            ..MaterialListQuery::default()
        };
        let mats = s
            .design_list_materials(&q)
            .expect("style-tag-filtered query");
        assert!(!mats.is_empty());
        for m in &mats {
            assert!(
                m.style_tags.iter().any(|t| t == "industrial"),
                "every result must carry the requested style_tag; got {:?}",
                m.style_tags
            );
        }
    }

    #[test]
    fn design_list_materials_filters_by_search_substring() {
        // `search = "Oak"` matches "Light Oak" only; `search = ""`
        // falls through to "no filter" (matches all 8). Pins the
        // empty-string-as-no-filter behaviour the renderer relies on.
        let (s, _g) = service();
        let oak = s
            .design_list_materials(&MaterialListQuery {
                search: Some("Oak".to_string()),
                ..MaterialListQuery::default()
            })
            .expect("substring query");
        assert_eq!(oak.len(), 1);
        assert_eq!(oak[0].name, "Light Oak");
        let empty = s
            .design_list_materials(&MaterialListQuery {
                search: Some(String::new()),
                ..MaterialListQuery::default()
            })
            .expect("empty-string substring query");
        assert_eq!(empty.len(), 8, "empty `search` must behave like no filter");
    }

    #[test]
    fn design_list_materials_saturating_clamps_to_max_limit() {
        // Regression on the saturating-clamp promise: `limit =
        // u32::MAX` must not propagate into an unbounded
        // materialisation. Mirrors the asset-list test of the same
        // shape.
        let (s, _g) = service();
        let res = s
            .design_list_materials(&MaterialListQuery {
                limit: Some(u32::MAX),
                ..MaterialListQuery::default()
            })
            .expect("u32::MAX limit must succeed (clamped, not unbounded)");
        assert_eq!(res.len(), 8, "clamped query returns the full starter pack");
    }

    #[test]
    fn design_update_material_applies_patch_and_returns_summary() {
        // A patch on `metallic` + `roughness` + `albedo` must be
        // visible on the returned summary AND on the next list call.
        // Pins the in-process state mutation contract.
        let (s, _g) = service();
        let before = s
            .design_list_materials(&MaterialListQuery::default())
            .unwrap();
        let oak = before
            .iter()
            .find(|m| m.material_id == "mat:oak_light")
            .cloned()
            .expect("oak is in starter pack");
        let updated = s
            .design_update_material(
                "mat:oak_light",
                &MaterialUpdate {
                    metallic: Some(0.4),
                    roughness: Some(0.2),
                    albedo: Some([0.5, 0.3, 0.1]),
                    ..MaterialUpdate::default()
                },
            )
            .expect("patch applies");
        assert!((updated.metallic - 0.4).abs() < 1e-6);
        assert!((updated.roughness - 0.2).abs() < 1e-6);
        assert_eq!(updated.albedo, [0.5, 0.3, 0.1]);
        // Untouched fields preserve the previous value.
        assert!((updated.ior - oak.ior).abs() < 1e-6);
        assert_eq!(updated.material_id, "mat:oak_light");
        let after = s
            .design_list_materials(&MaterialListQuery::default())
            .unwrap();
        let next_oak = after
            .iter()
            .find(|m| m.material_id == "mat:oak_light")
            .expect("oak still present after update");
        assert!((next_oak.metallic - 0.4).abs() < 1e-6);
        assert_eq!(next_oak.albedo, [0.5, 0.3, 0.1]);
    }

    #[test]
    fn design_update_material_rejects_out_of_range_values() {
        // Out-of-range sliders must surface as `Invalid`, not
        // silently clamp. The renderer relies on the error to bring
        // the slider back inside the legal range (and shows a toast
        // explaining why). Mutates nothing on rejection.
        let (s, _g) = service();
        let cases: Vec<(&str, MaterialUpdate)> = vec![
            (
                "metallic > 1.0",
                MaterialUpdate {
                    metallic: Some(1.5),
                    ..MaterialUpdate::default()
                },
            ),
            (
                "roughness < 0.0",
                MaterialUpdate {
                    roughness: Some(-0.1),
                    ..MaterialUpdate::default()
                },
            ),
            (
                "transmission NaN",
                MaterialUpdate {
                    transmission: Some(f32::NAN),
                    ..MaterialUpdate::default()
                },
            ),
            (
                "ior below vacuum",
                MaterialUpdate {
                    ior: Some(0.5),
                    ..MaterialUpdate::default()
                },
            ),
            (
                "ior above diamond+",
                MaterialUpdate {
                    ior: Some(10.0),
                    ..MaterialUpdate::default()
                },
            ),
            (
                "albedo component > 1.0",
                MaterialUpdate {
                    albedo: Some([1.2, 0.0, 0.0]),
                    ..MaterialUpdate::default()
                },
            ),
        ];
        let before = s
            .design_list_materials(&MaterialListQuery::default())
            .unwrap();
        let oak_before = before
            .iter()
            .find(|m| m.material_id == "mat:oak_light")
            .cloned()
            .expect("oak present");
        for (case, update) in cases {
            let err = s
                .design_update_material("mat:oak_light", &update)
                .expect_err(case);
            assert!(
                matches!(err, BridgeServiceError::Invalid(_)),
                "case `{case}` should surface as Invalid; got {err:?}"
            );
        }
        let after = s
            .design_list_materials(&MaterialListQuery::default())
            .unwrap();
        let oak_after = after
            .iter()
            .find(|m| m.material_id == "mat:oak_light")
            .cloned()
            .expect("oak still present");
        assert_eq!(
            oak_before, oak_after,
            "rejected updates must not mutate the library"
        );
    }

    #[test]
    fn design_update_material_unknown_id_surfaces_invalid() {
        // A renderer that snapshots the library, then issues an
        // update against an id that was meanwhile removed from the
        // library, must see an `Invalid` error so it can re-list and
        // surface a friendly "material no longer available" toast —
        // not a generic `Core` runtime error.
        let (s, _g) = service();
        let err = s
            .design_update_material(
                "mat:does_not_exist",
                &MaterialUpdate {
                    metallic: Some(0.5),
                    ..MaterialUpdate::default()
                },
            )
            .expect_err("unknown id rejected");
        assert!(matches!(err, BridgeServiceError::Invalid(_)));
    }

    // ============================================================
    // PR-W Phase 1: project_export_package
    // ============================================================

    #[test]
    fn project_export_package_writes_a_zip_with_all_source_files() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Pack-Test")
            .unwrap();
        let out = tempfile::NamedTempFile::new().unwrap();
        let out_path = out.path().with_extension("aecpkg.zip");
        let _ = std::fs::remove_file(&out_path);
        let res = s
            .project_export_package(&summary.path, out_path.to_str().unwrap())
            .expect("export package");
        assert!(res.entries > 0, "archive must contain at least one file");
        assert!(res.total_bytes > 0);
        // Magic bytes — first 4 bytes of any ZIP file are `PK\x03\x04`.
        let bytes = std::fs::read(&out_path).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
    }

    // ============================================================
    // PR-W Phase 3: bim_classify
    // ============================================================

    /// Seed one wall into the project so the classification /
    /// property tests have something to operate on. The test
    /// templates ship with empty `rooms: []` so we can't rely on
    /// the template to provision entities.
    fn seed_one_wall(s: &mut BridgeService, summary: &ProjectSummary) -> aec_core::types::EntityId {
        use aec_command::commands::{wall, CommandKind};
        let entity_id = aec_core::types::EntityId::new();
        let cmd = aec_command::commands::Command::user(CommandKind::CreateWall(wall::CreateWall {
            entity_id: entity_id.clone(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }));
        s.command_apply(&summary.path, cmd).expect("seed wall");
        entity_id
    }

    #[test]
    fn bim_classify_uniformat_assigns_b2010_to_walls() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Classify-Test")
            .unwrap();
        seed_one_wall(&mut s, &summary);
        let res = s
            .bim_classify(&summary.path, "uniformat-ii")
            .expect("uniformat classify");
        assert_eq!(res.scheme, "uniformat-ii");
        assert!(res.classified > 0, "seeded wall must be classified");
        let walls: Vec<&BimClassifyAssignment> =
            res.details.iter().filter(|a| a.code == "B2010").collect();
        assert!(
            !walls.is_empty(),
            "expected at least one wall classified as B2010 (Exterior Walls)"
        );
    }

    #[test]
    fn bim_classify_unknown_scheme_returns_invalid_error() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Bad-Scheme")
            .unwrap();
        let err = s
            .bim_classify(&summary.path, "masterformat")
            .expect_err("unknown scheme is rejected");
        assert!(
            matches!(err, BridgeServiceError::Invalid(ref m) if m.contains("masterformat")),
            "got: {err:?}"
        );
    }

    #[test]
    fn bim_classify_omniclass_assigns_21_codes() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "OmniClass-Test")
            .unwrap();
        seed_one_wall(&mut s, &summary);
        let res = s.bim_classify(&summary.path, "omniclass-21").unwrap();
        assert!(res.classified > 0);
        // Codes must all start with "21-" (OmniClass Table 21 prefix).
        for d in &res.details {
            assert!(
                d.code.starts_with("21-"),
                "omniclass code missing 21- prefix: {}",
                d.code
            );
        }
    }

    #[test]
    fn bim_classify_uniformat_is_idempotent() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Idempotent")
            .unwrap();
        seed_one_wall(&mut s, &summary);
        let first = s.bim_classify(&summary.path, "uniformat-ii").unwrap();
        let second = s.bim_classify(&summary.path, "uniformat-ii").unwrap();
        // The second call must see exactly the same set of
        // recognised entities (so `details` is the same length)
        // but no new database mutations (so `classified` is now
        // 0 and `unchanged` carries the count). This is the
        // "change-count" semantic of `classified`: it tracks the
        // delta against the prior state, not the walk size.
        assert_eq!(
            first.details.len(),
            second.details.len(),
            "details rows must be stable across reruns"
        );
        assert!(
            first.classified > 0,
            "first call should report real mutations"
        );
        assert_eq!(
            second.classified, 0,
            "second call must report zero new mutations"
        );
        assert_eq!(
            second.unchanged, first.classified,
            "every previously-classified entity must show up as `unchanged` on the rerun"
        );
    }

    #[test]
    fn bim_classify_ifc_idempotent_no_op_reports_unchanged() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Idempotent-IFC")
            .unwrap();
        seed_one_wall(&mut s, &summary);
        let first = s.bim_classify(&summary.path, "ifc").unwrap();
        let second = s.bim_classify(&summary.path, "ifc").unwrap();
        // First IFC call rewrites `entities.kind` from "wall" to
        // "IfcWall" — that's a real DB change. Second call sees
        // every entity already at `IfcWall` and must report it
        // as `unchanged`, not `classified`.
        assert!(first.classified > 0, "first IFC call must mutate");
        assert_eq!(
            second.classified, 0,
            "second IFC call must not double-count already-IFC kinds"
        );
        assert_eq!(
            second.unchanged, first.classified,
            "every IFC-rewritten entity must be `unchanged` on the rerun"
        );
    }

    // ============================================================
    // PR-W Phase 4: bim_set_property
    // ============================================================

    #[test]
    fn bim_set_property_stores_then_overwrites_value() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Property-Test")
            .unwrap();
        let entity_id = seed_one_wall(&mut s, &summary);
        let target_id = entity_id.to_string();

        let first = s
            .bim_set_property(
                &summary.path,
                &target_id,
                "Pset_WallCommon",
                "FireRating",
                "60min",
            )
            .expect("set property");
        assert_eq!(first.previous_value, None);

        // Re-set with a different value — previous_value must echo
        // the prior write.
        let second = s
            .bim_set_property(
                &summary.path,
                &target_id,
                "Pset_WallCommon",
                "FireRating",
                "120min",
            )
            .expect("update property");
        assert_eq!(second.previous_value.as_deref(), Some("60min"));
        assert_eq!(second.pset, "Pset_WallCommon");
        assert_eq!(second.key, "FireRating");
    }

    #[test]
    fn bim_set_property_rejects_missing_entity() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Missing-Entity")
            .unwrap();
        let err = s
            .bim_set_property(&summary.path, "ent_does_not_exist", "Pset_X", "Y", "1")
            .expect_err("missing entity rejected");
        assert!(
            matches!(err, BridgeServiceError::Invalid(ref m) if m.contains("not found")),
            "got: {err:?}"
        );
    }

    #[test]
    fn bim_set_property_rejects_empty_pset_or_key() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Empty-Pset")
            .unwrap();
        let entity_id = seed_one_wall(&mut s, &summary);
        let target_id = entity_id.to_string();
        assert!(matches!(
            s.bim_set_property(&summary.path, &target_id, "  ", "k", "v"),
            Err(BridgeServiceError::Invalid(_))
        ));
        assert!(matches!(
            s.bim_set_property(&summary.path, &target_id, "Pset", "  ", "v"),
            Err(BridgeServiceError::Invalid(_))
        ));
    }

    /// Regression for the read-modify-write race fixed by moving the
    /// prior-body SELECT inside an `IMMEDIATE` transaction (PR-W
    /// round 3): two sequential `bim_set_property` calls writing
    /// *different* keys under the same `(entity, pset)` must both
    /// land in the merged body. Before the fix, the second call's
    /// read happened outside the transaction, so a concurrent first
    /// caller (or, in the sequential case, a stale read) would
    /// silently drop one key. With the IMMEDIATE-tx fix this
    /// sequential case is straightforward and the test pins the
    /// merge semantics so future refactors can't regress it.
    #[test]
    fn bim_set_property_merges_distinct_keys_into_same_pset() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Merge-Pset")
            .unwrap();
        let entity_id = seed_one_wall(&mut s, &summary);
        let target_id = entity_id.to_string();

        s.bim_set_property(
            &summary.path,
            &target_id,
            "Pset_WallCommon",
            "FireRating",
            "60min",
        )
        .expect("set FireRating");
        s.bim_set_property(
            &summary.path,
            &target_id,
            "Pset_WallCommon",
            "LoadBearing",
            "true",
        )
        .expect("set LoadBearing");
        s.bim_set_property(
            &summary.path,
            &target_id,
            "Pset_WallCommon",
            "IsExternal",
            "false",
        )
        .expect("set IsExternal");

        // Read the merged body directly from the DB and assert all
        // three keys are present. If the TOCTOU bug regressed (or the
        // merge dropped keys), one of these `get` calls would return
        // `None`.
        let (_pkg, conn) =
            ProjectPackage::open_with_master_key_and_database(&summary.path, &s.master_key)
                .expect("open project");
        let body: String = conn
            .query_row(
                "SELECT body FROM components WHERE entity_id = ?1 AND kind = ?2",
                params![&target_id, "aec/property/Pset_WallCommon"],
                |r| r.get(0),
            )
            .expect("body present");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("body parses");
        assert_eq!(
            parsed.get("FireRating").and_then(|v| v.as_str()),
            Some("60min"),
            "FireRating preserved across distinct-key writes"
        );
        assert_eq!(
            parsed.get("LoadBearing").and_then(|v| v.as_str()),
            Some("true"),
            "LoadBearing preserved"
        );
        assert_eq!(
            parsed.get("IsExternal").and_then(|v| v.as_str()),
            Some("false"),
            "IsExternal preserved"
        );
    }

    // ============================================================
    // Group A Phase 10: draft / deliver scope wiring
    // ============================================================

    fn seed_one_primitive(s: &mut BridgeService, summary: &ProjectSummary) {
        use aec_cad::primitives::{Line, Primitive};
        use aec_command::commands::draft::DrawPrimitive;
        use aec_command::commands::CommandKind;
        let cmd = aec_command::commands::Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: aec_core::types::EntityId::new(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        }));
        s.command_apply(&summary.path, cmd).expect("seed primitive");
    }

    #[test]
    fn draft_export_dxf_emits_all_primitives_then_reimports_them() {
        let (mut s, g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Dxf-Roundtrip")
            .unwrap();
        seed_one_primitive(&mut s, &summary);
        // Export to a temp DXF file.
        let dxf_path = g.path().join("export.dxf");
        let res = s
            .draft_export_dxf(&summary.path, dxf_path.to_str().unwrap())
            .expect("export DXF");
        assert_eq!(res.entity_count, 1);
        assert!(res.file_size > 0);
        // Now re-import into the same project — should add one more
        // primitive (the import is additive).
        let imp = s
            .draft_import_dxf(&summary.path, dxf_path.to_str().unwrap())
            .expect("import DXF");
        assert_eq!(imp.entity_count, 1);
        // The "0" layer is always present.
        assert!(imp.layer_count >= 1);
    }

    #[test]
    fn deliver_create_then_list_revisions_returns_camelcase_summary() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Revision-Test")
            .unwrap();
        let rev = s
            .deliver_create_revision(&summary.path, "rev-1", "first snapshot", None)
            .expect("create revision");
        assert_eq!(rev.tag, "rev-1");
        assert_eq!(rev.description, "first snapshot");
        assert!(!rev.revision_id.is_empty(), "revision_id is generated");
        // Empty new project — tracked_entities reflects whatever the
        // template seeded (could be zero or more, but the field
        // exists and serialises as `trackedEntities`).
        let json = serde_json::to_value(&rev).unwrap();
        assert!(json.get("trackedEntities").is_some());
        assert!(json.get("revisionId").is_some());

        // listing returns the same revision (by revision_id).
        let list = s.deliver_list_revisions(&summary.path).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].revision_id, rev.revision_id);
    }

    #[test]
    fn deliver_compare_revisions_reports_added_and_modified() {
        use aec_cad::primitives::{Line, Primitive};
        use aec_command::commands::draft::DrawPrimitive;
        use aec_command::commands::CommandKind;
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Diff-Test")
            .unwrap();
        // r1: snapshot the empty project.
        let r1 = s
            .deliver_create_revision(&summary.path, "r1", "empty", None)
            .unwrap();
        // Add a primitive between snapshots.
        let id = aec_core::types::EntityId::new();
        let cmd = aec_command::commands::Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        }));
        s.command_apply(&summary.path, cmd).unwrap();
        let r2 = s
            .deliver_create_revision(&summary.path, "r2", "after add", None)
            .unwrap();
        // Compare.
        let diff = s
            .deliver_compare_revisions(&summary.path, &r1.revision_id, &r2.revision_id)
            .expect("diff");
        assert_eq!(diff.base_revision_id, r1.revision_id);
        assert_eq!(diff.head_revision_id, r2.revision_id);
        // The added line shows up either as a new entity in `head`
        // (no entry in `base`) → Added in by_category.
        let counts = diff.by_category.get("primitive").unwrap_or_else(|| {
            panic!(
                "expected `primitive` category in diff; got keys: {:?}",
                diff.by_category.keys().collect::<Vec<_>>()
            )
        });
        assert!(counts.added >= 1, "added count: {counts:?}");
        assert_eq!(counts.removed, 0);
    }

    #[test]
    fn revision_summary_serializes_in_camel_case() {
        let rev = RevisionSummary {
            revision_id: "rev_a".into(),
            tag: "v1".into(),
            description: "desc".into(),
            created_at: "2026-05-25T00:00:00Z".into(),
            audit_chain_head: "abc".into(),
            manifest_name: "Demo".into(),
            manifest_app_version: "0.1.0".into(),
            tracked_entities: vec![RevisionTrackedEntity {
                category: "wall".into(),
                id: "e1".into(),
                payload_hash: "h".into(),
                label: None,
            }],
        };
        let json = serde_json::to_value(&rev).unwrap();
        assert!(json.get("revisionId").is_some());
        assert!(json.get("trackedEntities").is_some());
        assert!(json.get("auditChainHead").is_some());
        assert!(json.get("manifestAppVersion").is_some());
        // No snake_case leaks.
        assert!(json.get("revision_id").is_none());
        assert!(json.get("tracked_entities").is_none());
    }

    #[test]
    fn diff_report_serializes_in_camel_case() {
        let diff = RevisionDiffReport {
            base_revision_id: "a".into(),
            head_revision_id: "b".into(),
            by_category: std::collections::BTreeMap::from([(
                "wall".into(),
                RevisionDiffCounts {
                    added: 1,
                    ..Default::default()
                },
            )]),
            changes: vec![RevisionEntityChange {
                category: "wall".into(),
                id: "w1".into(),
                kind: "added".into(),
                before_hash: None,
                after_hash: Some("h".into()),
                label: None,
            }],
        };
        let json = serde_json::to_value(&diff).unwrap();
        assert!(json.get("baseRevisionId").is_some());
        assert!(json.get("headRevisionId").is_some());
        assert!(json.get("byCategory").is_some());
        let ch = json["changes"][0].clone();
        assert!(ch.get("beforeHash").is_some());
        assert!(ch.get("afterHash").is_some());
    }

    /// Build a synthetic [`AiToolSchemaRegistry`] for the grammar_key
    /// disambiguation tests. The registry shipped via
    /// [`AiToolSchemaRegistry::defaults`] happens to put the canonical
    /// home of `plan_detection` first in sorted-name order, so it can
    /// only exercise the canonical-home-is-sorted-first branch. To
    /// pin the other two branches (canonical home is NOT sorted-first,
    /// and no canonical home exists at all) we need to drive the
    /// helper with a registry whose entries we can choose. Building
    /// from a `Vec<(ToolName, grammar_key)>` keeps each test case
    /// self-documenting.
    fn schemas_with(entries: &[(AiToolName, &str)]) -> AiToolSchemaRegistry {
        let mut r = AiToolSchemaRegistry::new();
        for (name, grammar_key) in entries {
            r.insert(AiToolSchema {
                name: *name,
                display_name: name.as_str().to_owned(),
                allowed_scopes: vec![Scope::Design],
                max_entities_modified: 8,
                grammar_key: (*grammar_key).to_owned(),
                description: String::new(),
                child_tools: Vec::new(),
            });
        }
        r
    }

    #[test]
    fn canonical_builtin_for_grammar_key_picks_canonical_home_when_sorted_first() {
        // The bundled catalogue already exercises this branch:
        // `plan_detection` and `plan_to_wall` both declare
        // `grammar_key: "plan_detection"`, and `plan_detection`
        // sorts before `plan_to_wall` alphabetically. The canonical
        // home — the tool whose name matches the grammar_key — must
        // be picked, NOT just the first match.
        let schemas = AiToolSchemaRegistry::defaults();
        let picked = canonical_builtin_for_grammar_key("plan_detection", &schemas)
            .expect("plan_detection grammar_key has known home in the default catalogue");
        assert_eq!(
            picked,
            AiToolName::PlanDetection,
            "the tool whose name matches the grammar_key must win even when more than one tool \
             declares that grammar_key",
        );
    }

    #[test]
    fn canonical_builtin_for_grammar_key_picks_canonical_home_when_not_sorted_first() {
        // Synthesize a registry where the canonical-home tool sorts
        // AFTER an aliasing tool by name. Without the canonical-home
        // tie-break, the helper would pick the sorted-first match
        // (`CadCleanup`, since `cad_cleanup` < `plan_to_wall`),
        // which would silently send extension dispatches through the
        // wrong diff-engine arm. With the tie-break, the canonical
        // home (`PlanToWall`, whose name equals the grammar_key)
        // must win.
        let schemas = schemas_with(&[
            (AiToolName::PlanToWall, "plan_to_wall"),
            (AiToolName::CadCleanup, "plan_to_wall"),
        ]);
        let picked = canonical_builtin_for_grammar_key("plan_to_wall", &schemas)
            .expect("synthetic registry declares the grammar_key");
        assert_eq!(
            picked,
            AiToolName::PlanToWall,
            "canonical home must be selected even when an aliasing tool sorts before it by name",
        );
    }

    #[test]
    fn canonical_builtin_for_grammar_key_falls_back_to_sorted_first_when_no_canonical_home() {
        // Defensive branch: if a future catalogue ever ships a
        // grammar_key with NO matching tool name (which would be a
        // catalogue bug — `defaults_match_canonical_json` guards
        // against it for the bundled JSON), the helper must still
        // resolve deterministically. We synthesize that scenario by
        // pointing two tools at a grammar_key that no host tool
        // owns; the helper must return the sorted-first match
        // (`CadCleanup` < `Classification`).
        let schemas = schemas_with(&[
            (AiToolName::Classification, "made_up_grammar"),
            (AiToolName::CadCleanup, "made_up_grammar"),
        ]);
        let picked = canonical_builtin_for_grammar_key("made_up_grammar", &schemas)
            .expect("synthetic registry declares the grammar_key");
        assert_eq!(
            picked,
            AiToolName::CadCleanup,
            "with no canonical home the helper must fall back to sorted-first deterministically",
        );
    }

    #[test]
    fn canonical_builtin_for_grammar_key_returns_none_for_unknown_grammar_key() {
        // A grammar_key that no host tool declares must surface as
        // `None` so the resolver can return an `extension ai tool
        // declares unknown grammar_key` error instead of silently
        // routing to some unrelated tool.
        let schemas = AiToolSchemaRegistry::defaults();
        assert_eq!(
            canonical_builtin_for_grammar_key("totally_made_up", &schemas),
            None,
        );
    }
}
