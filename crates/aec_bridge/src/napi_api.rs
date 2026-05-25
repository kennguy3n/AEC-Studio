//! N-API wrappers around [`BridgeService`]. Only compiled with the
//! `napi` feature (set when packaging the desktop app for Electron).
//!
//! Every JS-facing struct in this file is intentionally aligned, field
//! for field, with the corresponding TypeScript interface in
//! `apps/desktop/electron/bridge.ts`. The bridge layer in TypeScript
//! casts the N-API return values to those interfaces with no
//! transformation, so any drift between this file and the TS interfaces
//! is a runtime bug surfaced as `undefined` on the renderer side.

#![cfg(feature = "napi")]

use std::path::PathBuf;
use std::sync::RwLock;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::service::{BridgeConfig, BridgeService};

/// Process-wide bridge singleton. The Electron main process initialises
/// this once at startup; every subsequent call goes through one of the
/// three `with_service*` helpers.
///
/// Stored under [`RwLock`] (not [`std::sync::Mutex`]) so the *read-only*
/// endpoints can run concurrently *with each other*. Concretely: an
/// Electron status-pane poll of [`project_engine_status`], a
/// `runtime_status` refresh from a renderer thread, and a
/// `project_list_recents` call from the Home page no longer serialize
/// against each other — each takes the read side of the lock and they
/// proceed in parallel.
///
/// **What this does NOT do:** it does *not* let read-only endpoints
/// run concurrently with mutating endpoints. The writer-side
/// [`with_service`] still acquires exclusive access for
/// `project_open` / `project_save` / `project_audit_sync` /
/// `project_create_from_template`, blocking until every outstanding
/// reader releases. A mid-poll status read therefore still excludes
/// a `Cmd-S` triggered `project_save` for its duration and vice
/// versa. Given the per-call latency budgets (~50 µs for a cache-hit
/// `project_engine_status`, ~1 ms for a `project_save`) this is
/// already a substantial win over the previous `Mutex` which
/// serialized *all* endpoints against each other.
static SERVICE: RwLock<Option<BridgeService>> = RwLock::new(None);

/// Run `f` with **mutable** access to the bridge singleton, converting
/// all lock poisoning and "not initialised" errors into typed N-API
/// errors so the renderer can recover gracefully instead of crashing
/// the host.
///
/// Acquires the writer side of [`SERVICE`]'s [`RwLock`]. This excludes
/// all concurrent readers AND writers for the duration of `f`. Use
/// this for the mutating endpoints (`project_open`, `project_save`,
/// `project_audit_sync`, `project_create_from_template`) and for any
/// future endpoint that needs to mutate the [`BridgeService`]
/// in-memory state (recents, caches, ...).
fn with_service<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&mut BridgeService) -> std::result::Result<R, crate::service::BridgeServiceError>,
{
    let mut guard = SERVICE
        .write()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let svc = guard
        .as_mut()
        .ok_or_else(|| Error::from_reason("bridge not initialised"))?;
    f(svc).map_err(|e| Error::from_reason(e.to_string()))
}

/// Same as [`with_service`] but for **infallible** read-only callers.
/// Acquires the reader side of [`SERVICE`]'s [`RwLock`] so other
/// readers can run concurrently. Use this for endpoints that take
/// `&self` on [`BridgeService`] AND cannot fail (e.g. `runtime_status`
/// where any error has been pushed into the construction of the
/// returned struct).
///
/// For *fallible* read-only endpoints (the common case for
/// SQL-touching reads like `project_engine_status` that return
/// [`crate::service::BridgeServiceError`]), use
/// [`with_service_ref_fallible`] instead — it threads the `Result`
/// through the closure properly.
fn with_service_ref<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&BridgeService) -> R,
{
    let guard = SERVICE
        .read()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let svc = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("bridge not initialised"))?;
    Ok(f(svc))
}

/// Same as [`with_service_ref`] but the closure may return a
/// [`crate::service::BridgeServiceError`].
///
/// Acquires the reader side of [`SERVICE`]'s [`RwLock`]. Multiple
/// fallible reads can run concurrently with each other AND with
/// infallible reads via [`with_service_ref`]; only the writer-side
/// [`with_service`] excludes them.
///
/// This is the right primitive for SQL-touching read endpoints whose
/// service-layer method is `&self` but returns `Result<_, _>` — the
/// motivating case for adding this helper was `project_engine_status`,
/// which previously had to take the write lock just to thread the
/// `Result` (blocking every other endpoint for the duration of an
/// O(50 µs) status read).
fn with_service_ref_fallible<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&BridgeService) -> std::result::Result<R, crate::service::BridgeServiceError>,
{
    let guard = SERVICE
        .read()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let svc = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("bridge not initialised"))?;
    f(svc).map_err(|e| Error::from_reason(e.to_string()))
}

/// Bridge between napi-rs's async `#[napi]` machinery and the
/// blocking [`BridgeService`] methods that talk to SQLCipher / the
/// LLM sidecar / the IFC parser.
///
/// `#[napi] async fn` runs on napi-rs's bundled tokio runtime; the
/// closure here is then dispatched to tokio's *blocking* thread pool
/// (the same pool used by `tokio::fs`), freeing the napi worker
/// thread and — transitively — the Electron main-process JS event
/// loop. The `SERVICE` `RwLockReadGuard` / `RwLockWriteGuard`
/// acquired inside `f` lives entirely inside the closure, so it
/// never crosses an `.await` and the std locks' lack of `Send` is
/// not a problem.
///
/// If the blocking thread panics, we surface it as a generic napi
/// error rather than letting it bubble up as an unwinding panic
/// (which on a `#[napi]` boundary aborts the host).
async fn spawn_blocking_napi<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: Send + 'static,
{
    napi::bindgen_prelude::spawn_blocking(f)
        .await
        .map_err(|e| Error::new(Status::GenericFailure, format!("blocking task panicked: {e}")))?
}

#[napi(object)]
pub struct InitOptions {
    pub state_dir: String,
    pub projects_dir: String,
    pub templates_dir: String,
    pub max_recents: u32,
    /// 32-byte master key (hex-encoded).
    pub master_key_hex: String,
}

#[napi]
pub fn bridge_init(opts: InitOptions) -> Result<()> {
    let bytes = hex_decode(&opts.master_key_hex)
        .ok_or_else(|| Error::from_reason("master_key_hex must be 64 hex chars"))?;
    if bytes.len() != 32 {
        return Err(Error::from_reason("master_key_hex must decode to 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    let cfg = BridgeConfig {
        state_dir: PathBuf::from(opts.state_dir),
        projects_dir: PathBuf::from(opts.projects_dir),
        templates_dir: PathBuf::from(opts.templates_dir),
        max_recents: opts.max_recents as usize,
    };
    let svc = BridgeService::new(cfg, key).map_err(|e| Error::from_reason(e.to_string()))?;
    // Same poison-tolerant pattern as `with_service` so a previously-
    // panicked init can't bring down the Electron host with a cryptic
    // `PoisonError` — surface the failure as a typed N-API error.
    let mut guard = SERVICE
        .write()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    *guard = Some(svc);
    Ok(())
}

/// JS-facing project summary. Field names are chosen so napi-rs's
/// snake_case → camelCase conversion produces exactly the names the
/// TypeScript `ProjectSummary` interface expects (`projectId`, `name`,
/// `path`, `templateKey`, `modifiedAt`).
///
/// Drift between this struct and `apps/desktop/electron/bridge.ts`
/// `ProjectSummary` is enforced informally by tests on both sides — keep
/// them aligned when adding fields.
#[napi(object)]
pub struct ProjectSummaryJs {
    pub project_id: String,
    pub name: String,
    pub path: String,
    /// Renamed from `template_id` so napi-rs exposes the JS field as
    /// `templateKey`, matching the TypeScript interface.
    pub template_key: Option<String>,
    /// ISO-8601 timestamp of the last manifest write (the recents store
    /// uses the last-opened moment, which is the user-perceived
    /// freshness on the Home dashboard). Always populated.
    pub modified_at: String,
}

impl From<crate::service::ProjectSummary> for ProjectSummaryJs {
    fn from(s: crate::service::ProjectSummary) -> Self {
        Self {
            project_id: s.project_id.to_string(),
            name: s.name,
            path: s.path,
            template_key: s.template_id,
            modified_at: s.updated_at.to_rfc3339(),
        }
    }
}

#[napi]
pub fn project_create_from_template(
    template_key: String,
    project_name: String,
) -> Result<ProjectSummaryJs> {
    with_service(|svc| svc.project_create_from_template(&template_key, &project_name))
        .map(Into::into)
}

#[napi]
pub fn project_open(path: String) -> Result<ProjectSummaryJs> {
    with_service(|svc| svc.project_open(&path)).map(Into::into)
}

#[napi]
pub fn project_save(path: String) -> Result<ProjectSummaryJs> {
    with_service(|svc| svc.project_save(&path)).map(Into::into)
}

#[napi]
pub fn project_list_recents() -> Result<Vec<ProjectSummaryJs>> {
    // `project_list_recents` is `&self` on `BridgeService` (it just
    // reads the in-memory recents store), so the bridge singleton
    // only needs a *read* lock for the duration of the call. Using
    // `with_service_ref_fallible` instead of `with_service` lets the
    // Home page's recents list run concurrently with status-pane
    // polls and with each other — only the mutating endpoints
    // (project_open / save / sync / create) exclude these reads.
    with_service_ref_fallible(BridgeService::project_list_recents)
        .map(|v| v.into_iter().map(Into::into).collect())
}

/// JS-facing engine status. Field names map directly to the TS
/// `EngineStatus` interface in `apps/desktop/electron/bridge.ts`.
/// `auditChainByScope` is a flat object whose keys are
/// `Scope::as_str` (`design`, `draft`, etc.) — using an object rather
/// than an array means the renderer can look up a specific scope's
/// count in O(1) without a `find` call.
///
/// Values stored as `u32` are guaranteed to fit by construction: even
/// a project with one entry per millisecond for a year (~31.5B) would
/// exceed u32, but the SQLite primary key column is `INTEGER` which
/// SQLite represents as a 64-bit signed value internally; the
/// renderer's status pane never needs more than 32-bit precision.
/// The cast saturates at `u32::MAX` if a project ever does run hot.
#[napi(object)]
pub struct EngineStatusJs {
    pub schema_version: u32,
    pub audit_chain_head: String,
    pub audit_entry_count: u32,
    pub audit_chain_sql_count: u32,
    pub audit_chain_by_scope: std::collections::HashMap<String, u32>,
}

impl From<crate::service::EngineStatusReport> for EngineStatusJs {
    fn from(r: crate::service::EngineStatusReport) -> Self {
        Self {
            schema_version: r.schema_version,
            audit_chain_head: r.audit_chain_head,
            audit_entry_count: r.audit_entry_count.min(u32::MAX as u64) as u32,
            audit_chain_sql_count: r.audit_chain_sql_count.min(u32::MAX as u64) as u32,
            audit_chain_by_scope: r
                .audit_chain_by_scope
                .into_iter()
                .map(|(k, v)| (k, v.min(u32::MAX as u64) as u32))
                .collect(),
        }
    }
}

#[napi]
pub fn project_engine_status(path: String) -> Result<EngineStatusJs> {
    // `project_engine_status` is `&self` on `BridgeService`, so the
    // bridge singleton only needs a *read* lock for the duration of
    // the call. Using `with_service_ref_fallible` instead of
    // `with_service` lets *concurrent reads* run in parallel —
    // multiple renderer threads polling status, the Home page
    // refreshing the recents list, and a `runtime_status` ping all
    // proceed without serializing against each other.
    //
    // It does NOT make the status read concurrent with `project_save`
    // or any other mutating endpoint — those acquire the write side
    // and the lock contract excludes readers for the writer's
    // duration (and vice versa). The cache invalidation that
    // `project_save` performs under its write guard ensures the next
    // status read after the save observes the post-save state.
    with_service_ref_fallible(|svc| svc.project_engine_status(&path)).map(Into::into)
}

#[napi]
pub fn project_audit_sync(path: String) -> Result<u32> {
    with_service(|svc| svc.project_audit_sync(&path)).map(|n| n.min(u32::MAX as u64) as u32)
}

/// JS-facing parse-only IFC import summary. Mirrors the renderer's
/// `BimImportSummary` interface in `apps/desktop/electron/bridge.ts`.
///
/// Numeric fields are sized as `u32` to match the renderer's status-pane
/// formatting — no real-world IFC file produces > 4B entities of any
/// individual kind, and overflow is saturating-clamped at `u32::MAX`
/// for safety rather than panicking.
#[napi(object)]
pub struct BimImportSummaryJs {
    pub path: String,
    pub schema: String,
    pub spatial_nodes: u32,
    pub elements: u32,
    pub psets: u32,
    pub qsets: u32,
    pub aggregations: u32,
    pub containments: u32,
    pub materials: u32,
    pub material_layer_sets: u32,
    pub material_assignments: u32,
    pub records_seen: u32,
}

impl From<crate::service::BimImportSummary> for BimImportSummaryJs {
    fn from(r: crate::service::BimImportSummary) -> Self {
        let clamp = |n: u64| n.min(u32::MAX as u64) as u32;
        Self {
            path: r.path,
            schema: r.schema,
            spatial_nodes: clamp(r.spatial_nodes),
            elements: clamp(r.elements),
            psets: clamp(r.psets),
            qsets: clamp(r.qsets),
            aggregations: clamp(r.aggregations),
            containments: clamp(r.containments),
            materials: clamp(r.materials),
            material_layer_sets: clamp(r.material_layer_sets),
            material_assignments: clamp(r.material_assignments),
            records_seen: clamp(r.records_seen),
        }
    }
}

/// Parse an `.ifc` file from disk and return a structured import
/// summary. The renderer uses this for the "Import BIM" preview
/// panel — the file is NOT yet folded into the active project
/// (that's the `bim_attach_*` follow-up in PR-L).
///
/// Declared `async` and dispatched via [`napi::tokio::task::spawn_blocking`]
/// so the multi-second-to-multi-minute IFC parse does NOT block the
/// Electron main process's JS event loop — concurrent N-API calls
/// (status polls, cancel buttons, IPC) continue to schedule while the
/// parse runs on a tokio blocking-pool thread. The acquired
/// `SERVICE.read()` guard lives entirely inside the `spawn_blocking`
/// closure, so it never crosses an `.await` boundary and the
/// `std::sync::RwLockReadGuard`'s lack of `Send` is irrelevant.
#[napi]
pub async fn bim_import_ifc(path: String) -> Result<BimImportSummaryJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_import_ifc(&path)).map(BimImportSummaryJs::from)
    })
    .await
}

/// JS-facing cheap file-size check. Mirrors the renderer's
/// `BimFileSizeCheck` interface in `apps/desktop/electron/bridge.ts`.
///
/// `file_size_bytes` and `threshold_bytes` are exposed as `f64`
/// (JS `number`) so the renderer can do arithmetic on them directly
/// (`fileSizeBytes / (1024 * 1024)` for humanised MB strings,
/// `fileSizeBytes >= thresholdBytes` for the large-file guard)
/// without BigInt / Number incompatibility. `f64` has exact
/// integer precision up to 2^53 ≈ 9 PB — well beyond any
/// conceivable IFC file — and matches the TS interface declaration
/// of `number` in `bridge.ts` and `preload.ts`.
///
/// Consistency: this deliberately follows the same pattern as
/// [`BimImportSummaryJs`], which uses `u32` (safe `number`) for
/// entity counts. When this method is wired to the native bridge
/// (i.e. moved from `NATIVE_FALLBACK_METHODS` to
/// `NATIVE_WIRED_METHODS`), the JS side receives `number`
/// directly — no BigInt / Number coercion footgun.
#[napi(object)]
pub struct BimFileSizeCheckJs {
    pub path: String,
    pub file_size_bytes: f64,
    pub large_file_warning: bool,
    pub threshold_bytes: f64,
}

impl From<crate::service::BimFileSizeCheck> for BimFileSizeCheckJs {
    fn from(r: crate::service::BimFileSizeCheck) -> Self {
        Self {
            path: r.path,
            file_size_bytes: r.file_size_bytes as f64,
            large_file_warning: r.large_file_warning,
            threshold_bytes: r.threshold_bytes as f64,
        }
    }
}

/// Cheap pre-parse stat of an `.ifc` file. The renderer's
/// file-picker UI calls this *before* invoking `bim_import_ifc`
/// so it can show a confirm dialog ("This file is 412 MB; parsing
/// may take a while — continue?") on large files *before* the
/// user commits to the multi-second parse path.
///
/// One `fs::metadata` + one `fs::canonicalize` — no file read,
/// no parse, no allocation beyond the canonicalised path.
/// Routed through `with_service_ref_fallible` (read-only).
#[napi]
pub fn bim_check_file_size(path: String) -> Result<BimFileSizeCheckJs> {
    with_service_ref_fallible(|svc| svc.bim_check_file_size(&path)).map(Into::into)
}

/// JS-facing post-attach summary. Mirrors the renderer's
/// `BimAttachSummary` interface in `apps/desktop/electron/bridge.ts`.
///
/// Numeric fields follow the same `u32` saturating-clamp pattern as
/// [`BimImportSummaryJs`] — no real-world IFC file produces > 4 B
/// entities of any individual kind, and overflow is clamped at
/// `u32::MAX` for safety rather than panicking. JS receives plain
/// `number`s (no BigInt / Number coercion footgun).
#[napi(object)]
pub struct BimAttachSummaryJs {
    pub path: String,
    pub project_path: String,
    pub parse_cache_hit: bool,
    pub spatial_nodes_inserted: u32,
    pub spatial_nodes_updated: u32,
    pub spatial_nodes_unchanged: u32,
    pub elements_inserted: u32,
    pub elements_updated: u32,
    pub elements_unchanged: u32,
    pub components_inserted: u32,
    pub relations_inserted: u32,
    pub cache_rows: u32,
}

impl From<crate::service::BimAttachSummary> for BimAttachSummaryJs {
    fn from(r: crate::service::BimAttachSummary) -> Self {
        let clamp = |n: u64| n.min(u32::MAX as u64) as u32;
        Self {
            path: r.path,
            project_path: r.project_path,
            parse_cache_hit: r.parse_cache_hit,
            spatial_nodes_inserted: clamp(r.spatial_nodes_inserted),
            spatial_nodes_updated: clamp(r.spatial_nodes_updated),
            spatial_nodes_unchanged: clamp(r.spatial_nodes_unchanged),
            elements_inserted: clamp(r.elements_inserted),
            elements_updated: clamp(r.elements_updated),
            elements_unchanged: clamp(r.elements_unchanged),
            components_inserted: clamp(r.components_inserted),
            relations_inserted: clamp(r.relations_inserted),
            cache_rows: clamp(r.cache_rows),
        }
    }
}

/// Attach a parsed IFC snapshot into the active project's
/// SQLCipher database. Folds spatial nodes, elements, Psets,
/// materials, and aggregation / containment relations into the
/// project graph, deduping against the existing rows so a
/// re-attach of the same file with identical content is cheap
/// (reports `_unchanged` instead of `_inserted` / `_updated`).
///
/// Routes through `with_service_ref_fallible` (the reader-side
/// lock helper) — `BridgeService::bim_attach_ifc` is `&self`;
/// the project-DB mutation lives behind a `Mutex`-guarded
/// `rusqlite::Connection` inside the project package, and the
/// snapshot-cache / status-cache invalidation use interior
/// mutability. Same pattern as [`bim_import_ifc`] /
/// [`bim_check_file_size`].
///
/// On a successful attach the service invalidates the
/// `project_engine_status` connection cache for the affected
/// project so subsequent polls see the new entities / components
/// rows.
///
/// The `parse_cache_hit` field on the result tells the renderer
/// whether the in-process snapshot cache served the parse — useful
/// for the loading-indicator UX (cache hit is sub-millisecond;
/// miss is the multi-second STEP parse path that `bim_import_ifc`
/// would otherwise re-run).
#[napi]
pub fn bim_attach_ifc(project_path: String, ifc_path: String) -> Result<BimAttachSummaryJs> {
    with_service_ref_fallible(|svc| svc.bim_attach_ifc(&project_path, &ifc_path)).map(Into::into)
}

/// JS-facing command-apply result. Mirrors
/// `apps/desktop/electron/bridge.ts`'s `CommandApplyResult`. The
/// renderer consumes `commandId` for audit-pane linking, `applied`
/// as opaque-but-roundtrippable JSON (the renderer renders new
/// entities directly from each `Create` delta, updates from
/// `Update`, deletes from `Delete`), and `undoLen` / `redoLen` to
/// keep the undo / redo toolbar buttons in sync without a follow-up
/// query.
///
/// `applied` carries `Vec<EntityDelta>` as a JSON-stringified
/// payload (rather than a structured napi object) for two reasons:
/// (1) `EntityDelta`'s `Create` / `Update` / `Delete` shape is
/// already serde-tagged in `aec_command::commands::project_graph`
/// and re-deriving the same tagged union in napi-rs would be lossy
/// (napi-rs unions don't preserve the tag), and (2) the renderer
/// already has `JSON.parse` infrastructure for delta application.
#[napi(object)]
pub struct CommandApplyResultJs {
    /// `CommandId` as the underlying string (e.g. `"cmd_..."`).
    /// `CommandId` doesn't roundtrip through napi-rs as a custom
    /// type — exposing the string form here matches every other
    /// id-bearing field in the bridge surface.
    pub command_id: String,
    /// JSON-stringified `Vec<EntityDelta>`. See the doc comment on
    /// [`CommandApplyResultJs`] for the rationale.
    pub applied_json: String,
    /// Post-call undo-stack depth. `0` means "nothing to undo".
    pub undo_len: u32,
    /// Post-call redo-stack depth. `0` means "nothing to redo".
    pub redo_len: u32,
}

impl From<crate::service::CommandApplyResult> for CommandApplyResultJs {
    fn from(r: crate::service::CommandApplyResult) -> Self {
        Self {
            command_id: r.command_id.to_string(),
            // `serde_json::to_string` on `Vec<EntityDelta>` is
            // infallible for the shapes produced by the engine
            // (all fields are owned strings / numbers / serde Values
            // that already roundtripped through `serde_json::Value`
            // on the way in), so we serialise here without an
            // additional fallible step in the napi return path.
            applied_json: serde_json::to_string(&r.applied).unwrap_or_else(|_| "[]".to_string()),
            undo_len: r.undo_len,
            redo_len: r.redo_len,
        }
    }
}

/// JS-facing entity record. Mirrors the renderer's
/// `apps/desktop/electron/bridge.ts` `EntityRecord` interface for
/// `projectGraphList`.
///
/// `body_json` carries `serde_json::Value` as a JSON string for the
/// same reason as `CommandApplyResultJs::applied_json` — the
/// renderer-side delta consumer already handles parsing.
#[napi(object)]
pub struct EntityRecordJs {
    pub id: String,
    pub kind: String,
    pub parent: Option<String>,
    pub body_json: String,
}

impl From<aec_command::commands::EntityRecord> for EntityRecordJs {
    fn from(r: aec_command::commands::EntityRecord) -> Self {
        Self {
            id: r.id.to_string(),
            kind: r.kind,
            parent: r.parent.map(|p| p.to_string()),
            body_json: serde_json::to_string(&r.body).unwrap_or_else(|_| "null".to_string()),
        }
    }
}

/// Apply a typed command to the project graph.
///
/// `command_json` is a serialised [`aec_command::commands::Command`]
/// (the `command_id` / `ts` / `scope` / `actor` / `kind` envelope
/// produced by the renderer-side command helpers). The engine
/// rebuilds itself from the on-disk graph + journal, executes the
/// command via [`aec_command::engine::CommandEngine::execute_persistent`]
/// so the SQL and in-memory layers advance in lock-step, and
/// returns the resulting deltas + audit envelope.
///
/// Routes through `with_service` (the `&mut self` helper) because
/// [`crate::service::BridgeService::command_apply`] takes `&mut self`
/// to mutate the engine-status cache after a command lands — without
/// taking the write lock here, a status poll racing against the apply
/// could read a stale row count. The DB connection itself serialises
/// writes through `Mutex<rusqlite::Connection>`, so the outer lock is
/// not protecting on-disk state, only the in-memory caches that hang
/// off [`crate::service::BridgeService`].
#[napi]
pub fn command_apply(project_path: String, command_json: String) -> Result<CommandApplyResultJs> {
    let cmd: aec_command::commands::Command = serde_json::from_str(&command_json).map_err(|e| {
        Error::new(
            Status::InvalidArg,
            format!("command_apply: invalid command JSON: {e}"),
        )
    })?;
    with_service(|svc| svc.command_apply(&project_path, cmd)).map(Into::into)
}

/// Undo the most recently applied command on `project_path`.
///
/// `active_scope` is one of `"design"`, `"draft"`, `"bim"`,
/// `"render"`, `"deliver"` — passed by the renderer to tag the
/// resulting audit envelope and to validate that the inverse
/// deltas don't cross a scope boundary (e.g. you can't undo a
/// design command while in the bim workflow).
#[napi]
pub fn command_undo(project_path: String, active_scope: String) -> Result<CommandApplyResultJs> {
    let scope = parse_scope(&active_scope)?;
    with_service(|svc| svc.command_undo(&project_path, scope)).map(Into::into)
}

/// Redo the most recently undone command. Symmetric counterpart
/// to [`command_undo`].
#[napi]
pub fn command_redo(project_path: String, active_scope: String) -> Result<CommandApplyResultJs> {
    let scope = parse_scope(&active_scope)?;
    with_service(|svc| svc.command_redo(&project_path, scope)).map(Into::into)
}

/// List the project graph. Pass `kind_filter = None` for the full
/// graph; pass `Some(kind)` to narrow (e.g. `"wall"`, `"room"`,
/// `"camera"`). Read-only; safe to call concurrently with status
/// polls — routed through `with_service_ref_fallible`.
#[napi]
pub fn project_graph_list(
    project_path: String,
    kind_filter: Option<String>,
) -> Result<Vec<EntityRecordJs>> {
    with_service_ref_fallible(|svc| svc.project_graph_list(&project_path, kind_filter.as_deref()))
        .map(|rs| rs.into_iter().map(Into::into).collect())
}

/// JS-facing renderer-side query parameters for
/// [`design_list_assets`]. Mirrors the `DesignAssetQuery` shape the
/// renderer passes through `designListAssets(query)` in
/// `apps/desktop/electron/bridge.ts`.
///
/// All fields are optional so the renderer can call this with an
/// empty object (`{}`) and get the seed library back. Field-name
/// alignment (snake_case here → camelCase on the JS side via
/// `#[napi(object)]`) is by convention:
///
/// * `search` → renderer's case-insensitive name substring.
/// * `tags` → AND-matched tag list. Empty / missing falls through
///   to "no filter".
/// * `style_tags` → AND-matched style-tag list. Same semantics.
/// * `limit` → cap on result-set size. `None` falls through to the
///   bridge default (24, matching the asset-browser grid page).
#[napi(object)]
pub struct DesignListAssetsQueryJs {
    pub search: Option<String>,
    pub tags: Option<Vec<String>>,
    pub style_tags: Option<Vec<String>>,
    pub limit: Option<u32>,
}

/// JS-facing asset-browser card. Mirrors `AssetSummary` in
/// `apps/desktop/electron/bridge.ts`.
///
/// `thumbnail_data_uri` is intentionally `Option<String>` (not
/// `String`) so the renderer can distinguish "no thumbnail yet"
/// (placeholder card) from "thumbnail is an empty data URI" (which
/// would be a real-world bug worth surfacing). The PR-U seed library
/// is `None` for every demo asset — real thumbnail wiring lands in a
/// follow-up.
#[napi(object)]
pub struct AssetSummaryJs {
    pub asset_id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub style_tags: Vec<String>,
    pub vendor: Option<String>,
    pub thumbnail_data_uri: Option<String>,
}

impl From<crate::service::AssetSummary> for AssetSummaryJs {
    fn from(s: crate::service::AssetSummary) -> Self {
        Self {
            asset_id: s.asset_id,
            name: s.name,
            tags: s.tags,
            style_tags: s.style_tags,
            vendor: s.vendor,
            thumbnail_data_uri: s.thumbnail_data_uri,
        }
    }
}

/// List assets from the global asset library matching `query`.
/// Read-only; routes through `with_service_ref_fallible` so it can
/// run concurrently with other read-side endpoints (`runtime_status`,
/// `project_engine_status`, etc).
///
/// The DB is lazy-opened inside [`crate::asset_state::AssetState`] on
/// the first call of this endpoint, so a bridge boot that never
/// touches the asset browser pays zero SQLite open + schema-bootstrap
/// + seed cost.
#[napi]
pub fn design_list_assets(query: DesignListAssetsQueryJs) -> Result<Vec<AssetSummaryJs>> {
    let q = crate::service::AssetListQuery {
        search: query.search,
        tags: query.tags.unwrap_or_default(),
        style_tags: query.style_tags.unwrap_or_default(),
        limit: query.limit,
    };
    with_service_ref_fallible(|svc| svc.design_list_assets(&q))
        .map(|rows| rows.into_iter().map(Into::into).collect())
}

fn parse_scope(s: &str) -> Result<aec_core::types::Scope> {
    match s {
        "design" => Ok(aec_core::types::Scope::Design),
        "draft" => Ok(aec_core::types::Scope::Draft),
        "bim" => Ok(aec_core::types::Scope::Bim),
        "render" => Ok(aec_core::types::Scope::Render),
        "deliver" => Ok(aec_core::types::Scope::Deliver),
        other => Err(Error::new(
            Status::InvalidArg,
            format!("unknown scope `{other}` (expected design / draft / bim / render / deliver)"),
        )),
    }
}

/// Typed params for [`export_pdf`]. `out_path` and `project_name`
/// are mandatory; `body_lines` may be empty (the export crate
/// substitutes a placeholder overview page so the PDF still has
/// content beyond the cover).
#[napi(object)]
pub struct ExportPdfParamsJs {
    pub out_path: String,
    pub project_name: String,
    /// Optional body lines. `None` and an empty array are equivalent
    /// — both let the export crate substitute its placeholder
    /// overview text so the PDF still has > 1 page.
    pub body_lines: Option<Vec<String>>,
}

/// JS-facing result of [`export_pdf`]. Mirrors the renderer's
/// `{ outPath: string; pages: number }` return shape on
/// `BridgeBackend.exportPdf`.
#[napi(object)]
pub struct ExportPdfResultJs {
    pub out_path: String,
    pub pages: u32,
}

impl From<crate::service::ExportPdfResult> for ExportPdfResultJs {
    fn from(r: crate::service::ExportPdfResult) -> Self {
        Self {
            out_path: r.out_path,
            pages: r.pages,
        }
    }
}

/// Export a real PDF summary for the project. Routed through
/// `with_service_ref_fallible` (the read-only helper) because
/// `aec_export` is stateless and the bridge service holds no
/// per-export caches today — concurrent status polls must not be
/// blocked by a multi-second PDF assembly.
#[napi]
pub fn export_pdf(params: ExportPdfParamsJs) -> Result<ExportPdfResultJs> {
    let body = params.body_lines.unwrap_or_default();
    with_service_ref_fallible(|svc| svc.export_pdf(&params.out_path, &params.project_name, &body))
        .map(Into::into)
}

/// Typed params for [`export_dxf`]. `walls_mm` is `Vec<[f64; 4]>`
/// (each item is `[x1, y1, x2, y2]` in millimetres). The renderer
/// can pass `None` for no walls (the export still produces a
/// title-block-only DXF that downstream tools can open).
#[napi(object)]
pub struct ExportDxfParamsJs {
    pub out_path: String,
    pub project_name: String,
    /// Optional wall segments in millimetres. Each item is
    /// `[x1, y1, x2, y2]`. Validated at the napi boundary —
    /// arrays of the wrong length surface as a typed error.
    pub walls_mm: Option<Vec<Vec<f64>>>,
}

/// JS-facing result of [`export_dxf`].
#[napi(object)]
pub struct ExportDxfResultJs {
    pub out_path: String,
}

impl From<crate::service::ExportDxfResult> for ExportDxfResultJs {
    fn from(r: crate::service::ExportDxfResult) -> Self {
        Self {
            out_path: r.out_path,
        }
    }
}

/// Export a real DXF drawing for the project. Validates the
/// renderer-supplied wall arrays at the napi boundary so the
/// service layer can rely on a typed `(f64, f64, f64, f64)` tuple.
#[napi]
pub fn export_dxf(params: ExportDxfParamsJs) -> Result<ExportDxfResultJs> {
    let walls_raw = params.walls_mm.unwrap_or_default();
    let mut walls: Vec<(f64, f64, f64, f64)> = Vec::with_capacity(walls_raw.len());
    for (i, seg) in walls_raw.iter().enumerate() {
        if seg.len() != 4 {
            return Err(Error::new(
                Status::InvalidArg,
                format!(
                    "export_dxf: walls_mm[{i}] must be [x1, y1, x2, y2] (got len {})",
                    seg.len()
                ),
            ));
        }
        walls.push((seg[0], seg[1], seg[2], seg[3]));
    }
    with_service_ref_fallible(|svc| svc.export_dxf(&params.out_path, &params.project_name, &walls))
        .map(Into::into)
}

/// Typed params for [`export_ifc`]. When `storey_names` is `None`
/// or empty, the export crate adds a single default storey so the
/// IFC has a complete Project → Site → Building → Storey chain.
#[napi(object)]
pub struct ExportIfcParamsJs {
    pub out_path: String,
    pub project_name: String,
    pub storey_names: Option<Vec<String>>,
}

/// JS-facing result of [`export_ifc`].
#[napi(object)]
pub struct ExportIfcResultJs {
    pub out_path: String,
}

impl From<crate::service::ExportIfcResult> for ExportIfcResultJs {
    fn from(r: crate::service::ExportIfcResult) -> Self {
        Self {
            out_path: r.out_path,
        }
    }
}

/// Export a real ISO-10303-21 IFC4 STEP file for the project.
/// Uses the same `IfcWriter` the bridge's `bim_attach_ifc` round-
/// trips through, so the output is byte-compatible with the
/// dedup hashing pipeline.
#[napi]
pub fn export_ifc(params: ExportIfcParamsJs) -> Result<ExportIfcResultJs> {
    let storeys = params.storey_names.unwrap_or_default();
    with_service_ref_fallible(|svc| {
        svc.export_ifc(&params.out_path, &params.project_name, &storeys)
    })
    .map(Into::into)
}

/// Typed params for [`export_gltf`].
#[napi(object)]
pub struct ExportGltfParamsJs {
    pub out_path: String,
    pub project_name: String,
}

/// JS-facing result of [`export_gltf`].
#[napi(object)]
pub struct ExportGltfResultJs {
    pub out_path: String,
}

impl From<crate::service::ExportGltfResult> for ExportGltfResultJs {
    fn from(r: crate::service::ExportGltfResult) -> Self {
        Self {
            out_path: r.out_path,
        }
    }
}

/// Export a minimal-but-valid glTF 2.0 JSON file for the project.
/// Three.js' `GLTFLoader` and Khronos's glTF-Validator both accept
/// the output.
#[napi]
pub fn export_gltf(params: ExportGltfParamsJs) -> Result<ExportGltfResultJs> {
    with_service_ref_fallible(|svc| svc.export_gltf(&params.out_path, &params.project_name))
        .map(Into::into)
}

/// Typed params for [`export_build_proposal_pack`].
#[napi(object)]
pub struct ExportProposalPackParamsJs {
    pub out_path: String,
    pub project_name: String,
    /// `client_name` is informational only — appears on the cover
    /// page. The export still succeeds when `None` is passed (the
    /// cover renders `"(client)"` as the placeholder).
    pub client_name: Option<String>,
}

/// JS-facing result of [`export_build_proposal_pack`].
#[napi(object)]
pub struct ExportProposalPackResultJs {
    pub out_path: String,
}

impl From<crate::service::ExportProposalPackResult> for ExportProposalPackResultJs {
    fn from(r: crate::service::ExportProposalPackResult) -> Self {
        Self {
            out_path: r.out_path,
        }
    }
}

/// Export a real client-facing proposal PDF for the project. The
/// renderer's `BridgeBackend.exportBuildProposalPack` calls this.
#[napi]
pub fn export_build_proposal_pack(
    params: ExportProposalPackParamsJs,
) -> Result<ExportProposalPackResultJs> {
    let client = params.client_name.as_deref().unwrap_or("(client)");
    with_service_ref_fallible(|svc| {
        svc.export_proposal_pack(&params.out_path, &params.project_name, client)
    })
    .map(Into::into)
}

/// Typed params for [`deliver_build_pack`]. `kind` is one of
/// `"concept"`, `"interior"`, `"contractor"`, `"bim"` (validated
/// against `aec_export::DeliverPackKind::parse`).
#[napi(object)]
pub struct DeliverBuildPackParamsJs {
    pub kind: String,
    pub out_path: String,
    /// Project label printed on the in-archive PDF summary; the
    /// renderer defaults this to the open project's name.
    pub project_name: Option<String>,
    pub include_renders: Option<bool>,
    pub include_sheets: Option<bool>,
    pub include_ifc: Option<bool>,
    pub include_boq: Option<bool>,
    pub include_proposal: Option<bool>,
    /// `region` is currently accepted-and-stored by the renderer
    /// for compliance metadata, but the bridge doesn't use it
    /// today (the pack manifest stays region-agnostic). Reserved
    /// here so the renderer can keep passing it without breaking
    /// the napi shape.
    pub region: Option<String>,
}

/// JS-facing result of [`deliver_build_pack`]. Mirrors the
/// renderer's `DeliverPackResult` TS interface so the renderer can
/// use the value as-is for the file-list preview pane.
#[napi(object)]
pub struct DeliverBuildPackResultJs {
    pub out_path: String,
    pub contents: Vec<String>,
    /// Total bytes of payload files in the pack (manifest excluded).
    /// `f64` for the same precision rationale as
    /// [`BimFileSizeCheckJs::file_size_bytes`] — exact integer
    /// precision up to 2^53 ≈ 9 PB.
    pub total_bytes: f64,
}

impl From<crate::service::DeliverPackResult> for DeliverBuildPackResultJs {
    fn from(r: crate::service::DeliverPackResult) -> Self {
        Self {
            out_path: r.out_path,
            contents: r.contents,
            total_bytes: r.total_bytes as f64,
        }
    }
}

/// Assemble a deliverable ZIP archive for the project. Routes
/// through `with_service_ref_fallible` (read-only) — the export
/// crate is stateless and the renderer's preview pane polls in
/// parallel with the archive assembly.
#[napi]
pub fn deliver_build_pack(params: DeliverBuildPackParamsJs) -> Result<DeliverBuildPackResultJs> {
    let project_name = params.project_name.unwrap_or_else(|| "Project".to_string());
    let svc_params = crate::service::DeliverBuildPackParams {
        out_path: params.out_path,
        kind: params.kind,
        project_name,
        // Default each include_* flag to `true` to match the JS in-process
        // backend's `?? true` semantics (`apps/desktop/electron/bridge.ts`'s
        // `packContents`). Renderer callers may legitimately omit these
        // optional booleans and expect the "full pack for this kind" — the
        // native path would otherwise silently drop renders/sheets/boq/
        // ifc/proposal whenever the renderer didn't explicitly set them.
        options: crate::service::DeliverPackInventoryFlags {
            include_renders: params.include_renders.unwrap_or(true),
            include_sheets: params.include_sheets.unwrap_or(true),
            include_ifc: params.include_ifc.unwrap_or(true),
            include_boq: params.include_boq.unwrap_or(true),
            include_proposal: params.include_proposal.unwrap_or(true),
        },
    };
    // `with_service_ref_fallible` takes `FnOnce` (see helper declaration
    // ~600 LoC above) so the closure can consume `svc_params` directly
    // via `move` — no `.clone()` needed. Saves a `DeliverBuildPackParams`
    // copy per call. Flagged in PR-S round 2 ANALYSIS_0003.
    with_service_ref_fallible(move |svc| svc.deliver_build_pack(svc_params)).map(Into::into)
}

/// JS-facing summary of [`crate::service::BridgeService::bim_export_ifc`].
/// Mirrors the renderer's `BimExportIfcSummary` interface in
/// `apps/desktop/electron/bridge.ts`.
///
/// `bytes_written` is `f64` (JS `number`) for the same reason
/// [`BimFileSizeCheckJs::file_size_bytes`] is — `f64` has exact
/// integer precision up to 2^53 (~9 PB), well beyond any
/// conceivable IFC file, and avoids the BigInt / Number
/// incompatibility footgun.
#[napi(object)]
pub struct BimExportIfcSummaryJs {
    pub source_path: String,
    pub out_path: String,
    pub schema: String,
    pub bytes_written: f64,
    pub parse_cache_hit: bool,
}

impl From<crate::service::BimExportIfcSummary> for BimExportIfcSummaryJs {
    fn from(r: crate::service::BimExportIfcSummary) -> Self {
        Self {
            source_path: r.source_path,
            out_path: r.out_path,
            schema: r.schema,
            bytes_written: r.bytes_written as f64,
            parse_cache_hit: r.parse_cache_hit,
        }
    }
}

/// Parse an `.ifc` file, re-serialise the resulting snapshot back
/// to STEP-21, and write the bytes to `out_path`. The output is
/// byte-identical to what `bim_attach_ifc`'s snapshot would write
/// — both paths share `IfcWriter::to_string_with_materials`. The
/// snapshot cache fronts repeated calls against the same source.
///
/// Routes through `with_service_ref_fallible` (the reader-side
/// `RwLock` helper) because `BridgeService::bim_export_ifc` is
/// `&self` — the only `BridgeService` state it touches is the
/// snapshot cache via interior mutability. The function *does*
/// write to `out_path` on disk (so it isn't pure read-only at
/// the syscall layer), but that filesystem side effect is
/// orthogonal to the `BridgeService` lock contract. Concurrent
/// callers targeting the same `out_path` would race on the
/// filesystem itself, not on any in-memory state guarded by the
/// lock. See `with_service_ref_fallible` in `napi_api.rs:32`
/// for the broader rationale.
#[napi]
pub fn bim_export_ifc(ifc_path: String, out_path: String) -> Result<BimExportIfcSummaryJs> {
    with_service_ref_fallible(|svc| svc.bim_export_ifc(&ifc_path, &out_path)).map(Into::into)
}

/// JS-facing validation finding. Mirrors the renderer's
/// `BimValidationFinding` interface in
/// `apps/desktop/electron/bridge.ts`.
#[napi(object)]
pub struct BimValidationFindingJs {
    pub severity: String,
    pub code: String,
    pub element: Option<String>,
    pub description: String,
    pub suggestion: Option<String>,
}

impl From<crate::service::BimValidationFinding> for BimValidationFindingJs {
    fn from(r: crate::service::BimValidationFinding) -> Self {
        Self {
            severity: r.severity,
            code: r.code,
            element: r.element,
            description: r.description,
            suggestion: r.suggestion,
        }
    }
}

/// JS-facing summary of [`crate::service::BridgeService::bim_validate`].
/// Mirrors the renderer's `BimValidateReport` interface in
/// `apps/desktop/electron/bridge.ts`. Errors / warnings / infos
/// are pre-split into three vectors so the renderer's three-panel
/// view can render directly.
#[napi(object)]
pub struct BimValidateReportJs {
    pub ok: bool,
    pub source_path: String,
    pub schema: String,
    pub errors: Vec<BimValidationFindingJs>,
    pub warnings: Vec<BimValidationFindingJs>,
    pub infos: Vec<BimValidationFindingJs>,
    pub parse_cache_hit: bool,
}

impl From<crate::service::BimValidateReport> for BimValidateReportJs {
    fn from(r: crate::service::BimValidateReport) -> Self {
        Self {
            ok: r.ok,
            source_path: r.source_path,
            schema: r.schema,
            errors: r.errors.into_iter().map(Into::into).collect(),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
            infos: r.infos.into_iter().map(Into::into).collect(),
            parse_cache_hit: r.parse_cache_hit,
        }
    }
}

/// Parse an `.ifc` file and run the rule-based BIM validator
/// against the resulting snapshot. Findings are split by severity
/// (errors / warnings / infos). The relations side of the
/// validator runs with an empty `RelationStore` because the IFC
/// reader folds spatial relationships directly into
/// `Project.nodes[*].elements`; remaining checks operate on the
/// snapshot's stores directly.
///
/// Routes through `with_service_ref_fallible` (read-only).
#[napi]
pub fn bim_validate(ifc_path: String) -> Result<BimValidateReportJs> {
    with_service_ref_fallible(|svc| svc.bim_validate(&ifc_path)).map(Into::into)
}

/// JS-facing property-level change. `before` / `after` are JSON-
/// stringified `PropertyValue` so the napi layer doesn't have to
/// encode the tagged-union variants (Boolean / Logical / Real /
/// etc.) into a typed shape — the renderer parses them with
/// `JSON.parse` and renders the appropriate widget.
#[napi(object)]
pub struct BimDiffPropertyChangeJs {
    pub pset: String,
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl From<crate::service::BimDiffPropertyChange> for BimDiffPropertyChangeJs {
    fn from(r: crate::service::BimDiffPropertyChange) -> Self {
        Self {
            pset: r.pset,
            key: r.key,
            before: r.before,
            after: r.after,
        }
    }
}

/// JS-facing element-level change inside a `BimDiffSummary::modified`
/// list. The `key` is the join key built by `aec_bim::diff` — GUID
/// first, falling back to `class:name`.
///
/// **Pair invariant**: `class_before` and `class_after` come from
/// `aec_bim::diff::ElementDelta::class_changed: Option<(String,
/// String)>` via the `From` impl below, which splits the tuple so
/// `#[napi(object)]` can serialise it (napi-rs `#[napi(object)]`
/// doesn't carry nested `Option<#[napi(object)]>` cleanly). The
/// split is purely an FFI shape concern: the two fields are
/// *always* `(None, None)` or *always* `(Some(_), Some(_))` — the
/// mixed states `(None, Some(_))` / `(Some(_), None)` are
/// unreachable by construction. The same applies to `name_before`
/// / `name_after`. The TS-side `BimDiffElementChange` interface in
/// `apps/desktop/electron/bridge.ts` documents the same invariant
/// for renderer consumers.
#[napi(object)]
pub struct BimDiffElementChangeJs {
    pub key: String,
    pub class_before: Option<String>,
    pub class_after: Option<String>,
    pub name_before: Option<String>,
    pub name_after: Option<String>,
    pub property_deltas: Vec<BimDiffPropertyChangeJs>,
}

impl From<crate::service::BimDiffElementChange> for BimDiffElementChangeJs {
    fn from(r: crate::service::BimDiffElementChange) -> Self {
        Self {
            key: r.key,
            class_before: r.class_before,
            class_after: r.class_after,
            name_before: r.name_before,
            name_after: r.name_after,
            property_deltas: r.property_deltas.into_iter().map(Into::into).collect(),
        }
    }
}

/// JS-facing summary of [`crate::service::BridgeService::bim_diff`].
/// Mirrors the renderer's `BimDiffSummary` interface in
/// `apps/desktop/electron/bridge.ts`. `diff_id` is content-
/// addressed (BLAKE3 of canonical (before, after) path pair) so
/// the renderer can dedup repeated diffs and cache rendered views.
#[napi(object)]
pub struct BimDiffSummaryJs {
    pub diff_id: String,
    pub before_path: String,
    pub after_path: String,
    pub before_schema: String,
    pub after_schema: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<BimDiffElementChangeJs>,
    pub before_cache_hit: bool,
    pub after_cache_hit: bool,
}

impl From<crate::service::BimDiffSummary> for BimDiffSummaryJs {
    fn from(r: crate::service::BimDiffSummary) -> Self {
        Self {
            diff_id: r.diff_id,
            before_path: r.before_path,
            after_path: r.after_path,
            before_schema: r.before_schema,
            after_schema: r.after_schema,
            added: r.added,
            removed: r.removed,
            modified: r.modified.into_iter().map(Into::into).collect(),
            before_cache_hit: r.before_cache_hit,
            after_cache_hit: r.after_cache_hit,
        }
    }
}

/// Parse two `.ifc` files (independently snapshot-cache fronted)
/// and run `aec_bim::diff::diff_projects` to produce an element-
/// level diff. `diff_id` is *input*-addressed (BLAKE3 of the
/// canonical `(before, after)` path pair, **not** the file bytes)
/// so the same path pair always produces the same id even if the
/// files change. Content-aware invalidation happens one layer
/// down in the snapshot cache (keyed on `(canonical_path, mtime,
/// size)`).
///
/// Routes through `with_service_ref_fallible` (read-only).
#[napi]
pub fn bim_diff(before_path: String, after_path: String) -> Result<BimDiffSummaryJs> {
    with_service_ref_fallible(|svc| svc.bim_diff(&before_path, &after_path)).map(Into::into)
}

/// JS-facing summary of
/// [`crate::service::BridgeService::bim_generate_schedule`]. Mirrors
/// the renderer's `BimScheduleSummary` interface in
/// `apps/desktop/electron/bridge.ts`. `bytes_written` is `f64` for
/// the same precision reason as [`BimExportIfcSummaryJs::bytes_written`].
#[napi(object)]
pub struct BimScheduleSummaryJs {
    pub schedule_id: String,
    pub kind: String,
    pub source_path: String,
    pub out_path: String,
    pub rows: u32,
    pub columns: u32,
    pub bytes_written: f64,
    pub parse_cache_hit: bool,
}

impl From<crate::service::BimScheduleSummary> for BimScheduleSummaryJs {
    fn from(r: crate::service::BimScheduleSummary) -> Self {
        Self {
            schedule_id: r.schedule_id,
            kind: r.kind,
            source_path: r.source_path,
            out_path: r.out_path,
            rows: r.rows,
            columns: r.columns,
            bytes_written: r.bytes_written as f64,
            parse_cache_hit: r.parse_cache_hit,
        }
    }
}

/// Parse an `.ifc` file and generate one of the four supported
/// schedules (`"door"` / `"window"` / `"room"` / `"material"`),
/// writing the result to `out_path` as an XLSX workbook.
/// `schedule_id` is *input*-addressed (BLAKE3 of `(kind, canonical
/// source path)`, **not** the file bytes), so the same
/// `(kind, source)` pair always produces the same id even if the
/// IFC changes. Content-aware invalidation happens one layer down
/// in the snapshot cache (keyed on `(canonical_path, mtime,
/// size)`).
///
/// Routes through `with_service_ref_fallible` (the reader-side
/// `RwLock` helper) because `BridgeService::bim_generate_schedule`
/// is `&self` — the only `BridgeService` state it touches is the
/// snapshot cache via interior mutability. The function *does*
/// write the XLSX bytes to `out_path` on disk (so it isn't pure
/// read-only at the syscall layer), but that filesystem side
/// effect is orthogonal to the `BridgeService` lock contract.
/// Concurrent callers targeting the same `out_path` would race
/// on the filesystem itself, not on any in-memory state guarded
/// by the lock. Same caveat as `bim_export_ifc` above.
#[napi]
pub fn bim_generate_schedule(
    ifc_path: String,
    kind: String,
    out_path: String,
) -> Result<BimScheduleSummaryJs> {
    with_service_ref_fallible(|svc| svc.bim_generate_schedule(&ifc_path, &kind, &out_path))
        .map(Into::into)
}

/// JS-facing CPU descriptor. Mirrors `RuntimeStatus["cpu"]` in
/// `apps/desktop/electron/bridge.ts`.
#[napi(object)]
pub struct CpuProfileJs {
    pub model: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
}

/// JS-facing GPU descriptor. Mirrors the (non-null branch of)
/// `RuntimeStatus["gpu"]` in `apps/desktop/electron/bridge.ts`.
#[napi(object)]
pub struct GpuProfileJs {
    pub vendor: String,
    pub model: String,
    pub vram_mb: u32,
}

/// JS-facing hardware status. Field names + tier casing are deliberately
/// chosen to match the TypeScript `RuntimeStatus` interface so the
/// renderer can use the value as-is. In particular:
///
/// * `tier` is PascalCase (`"Low"` / `"Medium"` / `"High"` / `"Pro"`)
///   because the `is-tier-<Tier>` CSS classes in
///   `renderer/src/styles/components.css` use PascalCase.
/// * `cpu` and `gpu` are nested objects (not flat fields) because the
///   React components destructure them that way.
/// * `gpu` is nullable; outside Electron there is no wgpu adapter to
///   describe, and a misleading "software" placeholder would route
///   tier-dependent UI through the wrong code paths.
/// * RAM is reported in MiB; the TS `formatGb` helper renders it.
#[napi(object)]
pub struct RuntimeStatusJs {
    pub tier: String,
    pub cpu: CpuProfileJs,
    pub ram_total_mb: u32,
    pub ram_available_mb: u32,
    pub gpu: Option<GpuProfileJs>,
    pub os: String,
}

#[napi]
pub fn runtime_status() -> Result<RuntimeStatusJs> {
    with_service_ref(|svc| {
        let r = svc.runtime_status();
        RuntimeStatusJs {
            tier: r.tier.display().to_string(),
            cpu: CpuProfileJs {
                model: r.cpu.model.clone(),
                physical_cores: r.cpu.physical_cores,
                logical_cores: r.cpu.logical_cores,
            },
            // Cap at u32::MAX (~4 PiB). Any real host below that.
            ram_total_mb: r.total_ram_mb.min(u32::MAX as u64) as u32,
            ram_available_mb: r.available_ram_mb.min(u32::MAX as u64) as u32,
            gpu: r.gpu.as_ref().map(|g| GpuProfileJs {
                vendor: g.vendor.clone(),
                model: g.model.clone(),
                vram_mb: g.vram_mb,
            }),
            os: r.os.clone(),
        }
    })
}

// ----- Render endpoints (Phase 10 PR-R) -----
//
// JS-facing render queue + preset + doctor surface. Every struct in this
// section is field-aligned with the corresponding TS interface in
// `apps/desktop/electron/bridge.ts` — drift is a runtime bug surfaced
// as `undefined` on the renderer side.
//
// All endpoints route through `with_service_ref_fallible`: the service
// methods are `&self` with interior mutability behind a `Mutex` (see
// `BridgeService::render_state`), so the napi singleton's outer
// `RwLock` only needs the *read* side. This means a long
// `render_enqueue_batch` (N cameras × M presets) does NOT block a
// concurrent `project_engine_status` poll for status-pane refresh.

/// JS-facing render-job summary. Mirrors the `RenderJob` interface
/// in `apps/desktop/electron/bridge.ts`. The full Rust
/// [`aec_render::RenderJob`] carries a `RenderScene` + per-frame
/// resume markers; both are intentionally absent here — the queue
/// view never displays them, and shipping the scene through every
/// `render_list_jobs` poll would be cripplingly expensive for
/// non-trivial projects.
#[napi(object)]
pub struct RenderJobJs {
    pub job_id: String,
    pub status: String,
    pub preset: String,
    /// `0.0..=1.0`. Carried as `f64` for the napi-rs Number
    /// conversion (`f32` would force an extra `as f64` on every
    /// progress update for no precision gain).
    pub progress: f64,
    pub camera_id: Option<String>,
    pub batch_id: Option<String>,
}

impl From<crate::service::RenderJobSummary> for RenderJobJs {
    fn from(s: crate::service::RenderJobSummary) -> Self {
        Self {
            job_id: s.job_id,
            status: s.status,
            preset: s.preset,
            progress: s.progress as f64,
            camera_id: s.camera_id,
            batch_id: s.batch_id,
        }
    }
}

/// JS-facing render-batch progress aggregate. Mirrors the
/// `renderBatchProgress` return shape in
/// `apps/desktop/electron/bridge.ts`. Per-status counts are
/// `u32` because no realistic project ever queues more than 4
/// billion render jobs at once; the cast at the service-layer
/// projection saturates at `u32::MAX` defensively.
#[napi(object)]
pub struct RenderBatchProgressJs {
    pub batch_id: String,
    pub total: u32,
    pub queued: u32,
    pub running: u32,
    pub completed: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub average_progress: f64,
}

impl From<crate::service::RenderBatchProgressReport> for RenderBatchProgressJs {
    fn from(r: crate::service::RenderBatchProgressReport) -> Self {
        Self {
            batch_id: r.batch_id,
            total: r.total,
            queued: r.queued,
            running: r.running,
            completed: r.completed,
            failed: r.failed,
            cancelled: r.cancelled,
            average_progress: r.average_progress as f64,
        }
    }
}

/// JS-facing render-doctor finding. Mirrors the per-finding object
/// shape returned by `renderCheckMaterials` in
/// `apps/desktop/electron/bridge.ts`.
///
/// `material_id` is nullable because the doctor uses placeholder
/// strings (`<material>` / `<unknown>`) for findings that aren't
/// associated with a real material id; the service-layer
/// projection turns those into `None` rather than leaking the
/// placeholder through.
#[napi(object)]
pub struct RenderMaterialFindingJs {
    pub code: String,
    pub severity: String,
    pub material_id: Option<String>,
    pub message: String,
    pub fix: Option<String>,
}

impl From<crate::service::RenderMaterialFinding> for RenderMaterialFindingJs {
    fn from(f: crate::service::RenderMaterialFinding) -> Self {
        Self {
            code: f.code,
            severity: f.severity,
            material_id: f.material_id,
            message: f.message,
            fix: f.fix,
        }
    }
}

#[napi(object)]
pub struct RenderCheckMaterialsJs {
    pub findings: Vec<RenderMaterialFindingJs>,
}

impl From<crate::service::RenderCheckMaterialsReport> for RenderCheckMaterialsJs {
    fn from(r: crate::service::RenderCheckMaterialsReport) -> Self {
        Self {
            findings: r.findings.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
pub struct RenderDiagnoseJs {
    pub job_id: String,
    pub suggestions: Vec<String>,
}

impl From<crate::service::RenderDiagnoseReport> for RenderDiagnoseJs {
    fn from(r: crate::service::RenderDiagnoseReport) -> Self {
        Self {
            job_id: r.job_id,
            suggestions: r.suggestions,
        }
    }
}

#[napi(object)]
pub struct RenderEnqueueJs {
    pub job_id: String,
}

#[napi(object)]
pub struct RenderEnqueueBatchJs {
    pub batch_id: String,
    pub job_ids: Vec<String>,
}

#[napi(object)]
pub struct RenderCancelJs {
    pub cancelled: bool,
}

#[napi(object)]
pub struct RenderApplyPresetJs {
    pub ok: bool,
    pub active_preset_id: String,
}

/// Submit a render job for `camera_id` at the resolved `preset_id`.
///
/// `priority` defaults to `0` when callers send a missing / null
/// value (napi-rs surfaces `Option<f64>` for nullable numbers; we
/// truncate to `i32` because the queue's priority field is `i32`).
/// `scene_json` is the optional serialised [`aec_render::RenderScene`]
/// for the job — the doctor / diagnose paths run against this scene.
#[napi]
pub fn render_enqueue(
    camera_id: String,
    preset_id: String,
    priority: Option<f64>,
    scene_json: Option<String>,
) -> Result<RenderEnqueueJs> {
    let priority = priority.map_or(0, |p| p as i32);
    with_service_ref_fallible(|svc| {
        svc.render_enqueue(&camera_id, &preset_id, priority, scene_json.as_deref())
    })
    .map(|r| RenderEnqueueJs { job_id: r.job_id })
}

/// Submit a batch: one job per (camera × preset) pair. Empty
/// `camera_ids` or empty `preset_ids` is a hard error — silently
/// substituting defaults would hide a renderer-side dropdown bug.
#[napi]
pub fn render_enqueue_batch(
    camera_ids: Vec<String>,
    preset_ids: Vec<String>,
    scene_json: Option<String>,
) -> Result<RenderEnqueueBatchJs> {
    with_service_ref_fallible(|svc| {
        svc.render_enqueue_batch(&camera_ids, &preset_ids, scene_json.as_deref())
    })
    .map(|r| RenderEnqueueBatchJs {
        batch_id: r.batch_id,
        job_ids: r.job_ids,
    })
}

/// Return progress for the given batch id, or `null` (via
/// `Option::None`) when no jobs match. Mirrors the JS contract
/// where a stale batch id is a no-op on the UI side.
#[napi]
pub fn render_batch_progress(batch_id: String) -> Result<Option<RenderBatchProgressJs>> {
    with_service_ref_fallible(|svc| svc.render_batch_progress(&batch_id))
        .map(|opt| opt.map(Into::into))
}

#[napi]
pub fn render_list_jobs() -> Result<Vec<RenderJobJs>> {
    with_service_ref_fallible(super::service::BridgeService::render_list_jobs)
        .map(|v| v.into_iter().map(Into::into).collect())
}

#[napi]
pub fn render_cancel_job(job_id: String) -> Result<RenderCancelJs> {
    with_service_ref_fallible(|svc| svc.render_cancel_job(&job_id)).map(|r| RenderCancelJs {
        cancelled: r.cancelled,
    })
}

#[napi]
pub fn render_apply_preset(preset_id: String) -> Result<RenderApplyPresetJs> {
    with_service_ref_fallible(|svc| svc.render_apply_preset(&preset_id)).map(|r| {
        RenderApplyPresetJs {
            ok: r.ok,
            active_preset_id: r.active_preset_id,
        }
    })
}

#[napi]
pub fn render_diagnose(job_id: String) -> Result<RenderDiagnoseJs> {
    with_service_ref_fallible(|svc| svc.render_diagnose(&job_id)).map(Into::into)
}

#[napi]
pub fn render_check_materials() -> Result<RenderCheckMaterialsJs> {
    with_service_ref_fallible(super::service::BridgeService::render_check_materials).map(Into::into)
}

// ---------------------------------------------------------------
// AI endpoints — local LLM sidecar surface (Phase 10, PR-V)
// ---------------------------------------------------------------
//
// All six methods route through `with_service_ref_fallible` because
// mutation of the sidecar handle / pending diff map happens inside
// the inner `Mutex<AiState>` on `BridgeService`. That means:
//
//   * the singleton `RwLock` is held in *read* mode the whole time,
//     so `project_engine_status` polls keep flowing — important for
//     the status pane while a plan is running
//
// `ai_plan` is the only blocking caller in the set — its sidecar HTTP
// completion can take up to `request_timeout` (default 120 s). It is
// therefore declared `async` and routed through `spawn_blocking_napi`
// so the Electron main process's JS event loop stays free for the
// duration. `ai_runtime_status`, `ai_cancel_job`, `ai_list_tools`,
// `ai_accept_diff`, and `ai_reject_diff` all complete in microseconds
// (lock acquire + small memory ops) — they stay synchronous because
// the napi-rs `Promise<T>` wrapping would add observable overhead
// against zero benefit. The renderer can still poll them WHILE an
// `ai_plan` is in flight because the blocking work is on the tokio
// blocking-pool thread, not the libuv main thread.
//
// Inside `BridgeService`, the `Mutex<AiState>` guard is also dropped
// before `planner.dispatch()` (see `service.rs:lock_ai_state` +
// `ai_plan`), so concurrent `ai_runtime_status` / `ai_cancel_job`
// calls don't have to wait for the LLM completion to acquire the
// state mutex either. The two are complementary fixes — the napi
// `async` flip unblocks the JS event loop; the bridge-side guard
// drop unblocks the AiState mutex.

/// JS-facing AI tool descriptor for the renderer's tool picker.
/// Wire shape matches `AiTool` in `apps/desktop/electron/bridge.ts`.
#[napi(object)]
pub struct AiToolJs {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub allowed_scopes: Vec<String>,
    pub max_entities_modified: u32,
    pub grammar_key: String,
    pub child_tools: Vec<String>,
}

impl From<crate::service::AiToolDescriptor> for AiToolJs {
    fn from(d: crate::service::AiToolDescriptor) -> Self {
        Self {
            name: d.name,
            display_name: d.display_name,
            description: d.description,
            allowed_scopes: d.allowed_scopes,
            max_entities_modified: d.max_entities_modified,
            grammar_key: d.grammar_key,
            child_tools: d.child_tools,
        }
    }
}

/// JS-facing AI plan result. The `parsed` field is shipped to the
/// renderer as a JSON string rather than a typed napi value because
/// (a) the payload shape varies per-tool (each grammar emits a
/// different schema), and (b) napi-rs `serde_json::Value` support
/// would force a deep clone through `JsObject` — same trade-off as
/// `CommandApplyResultJs::applied_json`. The renderer already has
/// `JSON.parse` infrastructure for tool-call dispatch.
#[napi(object)]
pub struct AiPlanResultJs {
    pub diff_id: String,
    pub parsed_json: String,
    pub tool: String,
    pub entities_modified: u32,
}

#[napi(object)]
pub struct AiDiffOutcomeJs {
    pub ok: bool,
    pub diff_id: String,
}

#[napi(object)]
pub struct AiCancelResultJs {
    pub cancelled: bool,
}

#[napi(object)]
pub struct AiRuntimeStatusJs {
    /// One of `"idle"`, `"loading"`, `"ready"`, `"failed"`.
    pub state: String,
    /// Populated only after a Failed transition. Always cleared on
    /// the next successful `Ready` transition.
    pub last_error: Option<String>,
    /// Diff ids the renderer has not yet accepted or rejected.
    /// `ai_cancel_job` deliberately does NOT clear this list — it
    /// only aborts the in-flight LLM completion. Pending diffs are
    /// already-generated proposals that the user can still review
    /// (accept / reject) after a cancel; the only way to drop them
    /// is `ai_accept_diff` or `ai_reject_diff` on each one.
    pub pending_diff_ids: Vec<String>,
}

/// Enumerate the local AI tools available to the planner. Mirrors
/// `BridgeService::ai_list_tools` — see that doc for the contract.
#[napi]
pub fn ai_list_tools() -> Result<Vec<AiToolJs>> {
    with_service_ref_fallible(super::service::BridgeService::ai_list_tools)
        .map(|v| v.into_iter().map(Into::into).collect())
}

/// Plan a single AI action against the local LLM sidecar.
///
/// `tool` is the snake_case tool name (e.g. `"style_assistant"`).
/// `scope` is one of `design` / `draft` / `bim` / `render` / `deliver`.
/// `context_json` is the renderer's caller-supplied JSON context;
/// empty string is treated as an empty object.
/// `max_entities_modified` caps how many entities the resulting diff
/// may touch (the safety validator enforces this).
///
/// Declared `async` and routed through
/// [`napi::tokio::task::spawn_blocking`] so the up-to-120 s sidecar
/// HTTP completion does NOT block the Electron main process's JS
/// event loop. Concurrent N-API calls (notably [`ai_runtime_status`]
/// polled every ~500 ms and [`ai_cancel_job`] when the user clicks
/// "Stop") schedule on the libuv main thread *while* the plan is
/// in flight, because the actual blocking work runs on tokio's
/// blocking thread pool. The renderer therefore sees a responsive
/// UI and a working cancel button — both of which were
/// architecturally broken when `ai_plan` was synchronous, since
/// the napi sync-fn dispatcher ran it on libuv main directly.
///
/// `parse_scope` is intentionally outside the `spawn_blocking`
/// closure so an invalid `scope` argument errors immediately on
/// the JS-facing thread without scheduling any work — the renderer
/// sees a synchronously-rejected `Promise` for the invalid-args
/// case rather than a tail-end async error.
#[napi]
pub async fn ai_plan(
    tool: String,
    scope: String,
    prompt: String,
    context_json: String,
    max_entities_modified: u32,
) -> Result<AiPlanResultJs> {
    let scope = parse_scope(&scope)?;
    spawn_blocking_napi(move || {
        let r = with_service_ref_fallible(|svc| {
            svc.ai_plan(&tool, scope, &prompt, &context_json, max_entities_modified)
        })?;
        let parsed_json = serde_json::to_string(&r.parsed).map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("ai_plan: parsed payload re-serialise failed: {e}"),
            )
        })?;
        Ok(AiPlanResultJs {
            diff_id: r.diff_id,
            parsed_json,
            tool: r.tool,
            entities_modified: r.entities_modified,
        })
    })
    .await
}

/// Accept a pending diff. Idempotent at the renderer level: a second
/// accept on the same id is an error (the first removed it).
#[napi]
pub fn ai_accept_diff(diff_id: String) -> Result<AiDiffOutcomeJs> {
    with_service_ref_fallible(|svc| svc.ai_accept_diff(&diff_id)).map(|r| AiDiffOutcomeJs {
        ok: r.ok,
        diff_id: r.diff_id,
    })
}

/// Reject a pending diff. Same idempotency note as `ai_accept_diff`.
#[napi]
pub fn ai_reject_diff(diff_id: String) -> Result<AiDiffOutcomeJs> {
    with_service_ref_fallible(|svc| svc.ai_reject_diff(&diff_id)).map(|r| AiDiffOutcomeJs {
        ok: r.ok,
        diff_id: r.diff_id,
    })
}

/// Cancel any in-flight AI work by killing the sidecar process.
/// `job_id` is accepted for forward compatibility but currently
/// ignored — there's only one in-flight plan at a time (see the
/// concurrency rationale on the AI endpoints block above).
#[napi]
pub fn ai_cancel_job(job_id: String) -> Result<AiCancelResultJs> {
    with_service_ref_fallible(|svc| svc.ai_cancel_job(&job_id)).map(|r| AiCancelResultJs {
        cancelled: r.cancelled,
    })
}

/// Read the sidecar's current lifecycle state. Cheap, lock-only —
/// the renderer polls this every ~500 ms while a plan is in flight.
#[napi]
pub fn ai_runtime_status() -> Result<AiRuntimeStatusJs> {
    with_service_ref_fallible(super::service::BridgeService::ai_runtime_status).map(|r| {
        AiRuntimeStatusJs {
            state: r.state,
            last_error: r.last_error,
            pending_diff_ids: r.pending_diff_ids,
        }
    })
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16).ok()?;
        out.push(byte);
    }
    Some(out)
}
