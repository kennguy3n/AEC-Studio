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
        .map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("blocking task panicked: {e}"),
            )
        })?
}

#[napi(object)]
pub struct InitOptions {
    pub state_dir: String,
    pub projects_dir: String,
    pub templates_dir: String,
    pub max_recents: u32,
    /// 32-byte master key (hex-encoded).
    pub master_key_hex: String,
    /// Optional path to the directory holding installed extension
    /// packs (asset packs, template extensions, AI tool extensions,
    /// …). Each child directory must hold a `manifest.json`
    /// understood by [`aec_core::ExtensionLoader`]. Empty or absent
    /// (`None`) preserves the pre-Phase-14 behaviour where no
    /// extensions are loaded.
    pub extensions_dir: Option<String>,
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
        extensions_dir: opts
            .extensions_dir
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
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

/// Phase 17 Task 22: routed through [`spawn_blocking_napi`] so the
/// SQLCipher open (re-deriving the project key + WAL replay) never
/// blocks the napi worker thread / Electron main process JS event
/// loop on a cold project open. Concurrent status polls remain
/// responsive while the open is in flight.
#[napi]
pub async fn project_open(path: String) -> Result<ProjectSummaryJs> {
    spawn_blocking_napi(move || with_service(|svc| svc.project_open(&path)).map(Into::into)).await
}

/// Phase 17 Task 22: routed through [`spawn_blocking_napi`] for the
/// same reason as [`project_open`] — a project save can take tens of
/// milliseconds when the project graph is large (SQLCipher COMMIT +
/// fsync), and the Electron main process JS event loop must remain
/// responsive throughout.
#[napi]
pub async fn project_save(path: String) -> Result<ProjectSummaryJs> {
    spawn_blocking_napi(move || with_service(|svc| svc.project_save(&path)).map(Into::into)).await
}

/// JS-facing project thumbnail row. Mirrors
/// `crate::service::ProjectThumbnail`. The `png` field is a Node
/// `Buffer` (zero-copy byte view in V8) so the renderer can build
/// a `Uint8Array` / `Blob` without an intermediate base64 round-trip.
///
/// Phase 17 Group B Task 12.
#[napi(object)]
pub struct ProjectThumbnailJs {
    pub png: Buffer,
    pub width: u32,
    pub height: u32,
    pub updated_at: String,
}

impl From<crate::service::ProjectThumbnail> for ProjectThumbnailJs {
    fn from(t: crate::service::ProjectThumbnail) -> Self {
        Self {
            png: t.png.into(),
            width: t.width,
            height: t.height,
            updated_at: t.updated_at,
        }
    }
}

#[napi]
pub fn project_set_thumbnail(path: String, png: Buffer, width: u32, height: u32) -> Result<()> {
    // The `Buffer` arg is a zero-copy view onto the V8 backing
    // store. We deref it to `&[u8]` once at the boundary and the
    // bridge layer validates magic header + size before persisting.
    with_service(|svc| svc.project_set_thumbnail(&path, &png, width, height))
}

#[napi]
pub fn project_get_thumbnail(path: String) -> Result<Option<ProjectThumbnailJs>> {
    // Read-only on the service layer (`&self`). Use the read side
    // of the bridge `RwLock` so concurrent Home-page reads for
    // multiple recent projects run in parallel instead of
    // serialising against each other.
    with_service_ref_fallible(|svc| svc.project_get_thumbnail(&path)).map(|opt| opt.map(Into::into))
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

/// Phase 17 Task 22: routed through [`spawn_blocking_napi`] because
/// the audit chain backfill walks every entity touched by every
/// historical command. On a project graph with thousands of entities
/// this is several hundred milliseconds of pure CPU work.
#[napi]
pub async fn project_audit_sync(path: String) -> Result<u32> {
    spawn_blocking_napi(move || {
        with_service(|svc| svc.project_audit_sync(&path)).map(|n| n.min(u32::MAX as u64) as u32)
    })
    .await
}

/// JS-facing audit chain verification report. Mirrors
/// `aec_audit::ChainVerification` with file paths flattened to
/// strings so the JS side doesn't need any path-handling
/// dependencies.
#[napi(object)]
pub struct ChainVerificationJs {
    /// `"ok"` if the chain verified end-to-end; otherwise
    /// `"broken_at"`.
    pub status: String,
    /// Number of entries that were fully validated before the first
    /// break (or all entries, if the chain is intact).
    pub entries_checked: u32,
    /// Subset of `entries_checked` that were verified with
    /// linkage-only checks (their `hash_version` was the legacy v1
    /// algorithm whose stored hash requires the original payload to
    /// reproduce). The chain still reports `ok` in this case; UI
    /// surfaces can use this counter to gate downstream trust on
    /// the legacy fraction.
    pub entries_legacy_linkage_only: u32,
    /// `.jsonl` files that were actually opened and inspected, in
    /// the order they were walked. On a successful verification
    /// this includes every `.jsonl` under `<project>/audit/`; on an
    /// early break this only includes files up to and including the
    /// one in which the break occurred. Files discovered during
    /// directory traversal but never opened are NOT included.
    pub files_checked: Vec<String>,
    /// The latest valid `hash` head seen. For a fully-intact chain
    /// this equals the last entry's `hash`; for a broken chain it
    /// is the `hash` of the last entry that did verify.
    pub head_hash: String,
    /// `None` if `status == "ok"`; otherwise the file the break
    /// occurred in.
    pub break_file: Option<String>,
    /// `None` if `status == "ok"`; otherwise the 1-based line
    /// number of the broken entry.
    pub break_line: Option<u32>,
    /// `None` if `status == "ok"`; otherwise one of
    /// `"prev_hash_mismatch"`, `"hash_recompute_mismatch"`,
    /// `"unsupported_hash_version"`, `"legacy_hash_version_rejected"`,
    /// `"malformed_entry"`, `"io"`. `"legacy_hash_version_rejected"` is
    /// only producible when a caller wires a strict
    /// [`aec_audit::VerifyOptions`] through the service layer; the
    /// default `verify_chain` path used by the bridge cannot emit it.
    pub break_reason: Option<String>,
    /// `None` if `status == "ok"`; otherwise a human-readable
    /// description of the break (e.g. `"stored = blake3:dead,
    /// recomputed = blake3:abc"`). Stable enough for the UI's
    /// status pane; do NOT pattern-match on this string in
    /// production code — use `break_reason` instead.
    pub break_detail: Option<String>,
}

impl From<aec_audit::ChainVerification> for ChainVerificationJs {
    fn from(v: aec_audit::ChainVerification) -> Self {
        let entries_checked = v.entries_checked.min(u32::MAX as u64) as u32;
        let entries_legacy_linkage_only = v.entries_legacy_linkage_only.min(u32::MAX as u64) as u32;
        let files_checked = v
            .files_checked
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        match v.status {
            aec_audit::ChainStatus::Ok => Self {
                status: "ok".to_string(),
                entries_checked,
                entries_legacy_linkage_only,
                files_checked,
                head_hash: v.head_hash,
                break_file: None,
                break_line: None,
                break_reason: None,
                break_detail: None,
            },
            aec_audit::ChainStatus::BrokenAt { file, line, reason } => {
                let (reason_str, detail) = match &reason {
                    aec_audit::BreakReason::PrevHashMismatch { expected, found } => (
                        "prev_hash_mismatch",
                        format!("expected = {expected}, found = {found}"),
                    ),
                    aec_audit::BreakReason::HashRecomputeMismatch { stored, recomputed } => (
                        "hash_recompute_mismatch",
                        format!("stored = {stored}, recomputed = {recomputed}"),
                    ),
                    aec_audit::BreakReason::UnsupportedHashVersion { version, supported } => (
                        "unsupported_hash_version",
                        format!(
                            "entry hash_version = {version}, this build supports {:?}",
                            supported
                        ),
                    ),
                    aec_audit::BreakReason::LegacyHashVersionRejected {
                        version,
                        required_min,
                    } => (
                        "legacy_hash_version_rejected",
                        format!(
                            "entry hash_version = {version}, required minimum = {required_min}"
                        ),
                    ),
                    aec_audit::BreakReason::MalformedEntry { message } => {
                        ("malformed_entry", message.clone())
                    }
                    aec_audit::BreakReason::Io { message } => ("io", message.clone()),
                };
                Self {
                    status: "broken_at".to_string(),
                    entries_checked,
                    entries_legacy_linkage_only,
                    files_checked,
                    head_hash: v.head_hash,
                    break_file: Some(file.to_string_lossy().into_owned()),
                    break_line: Some(line.min(u32::MAX as u64) as u32),
                    break_reason: Some(reason_str.to_string()),
                    break_detail: Some(detail),
                }
            }
        }
    }
}

/// Verify the BLAKE3 hash chain of every `<project>/audit/*.jsonl`
/// file. Read-only — does not touch the SQLCipher DB, does not
/// require the master key, and does not invalidate any caches.
/// Safe to run concurrently with other reads.
///
/// The CPU cost is O(total audit bytes) and dominated by BLAKE3
/// hashing, which clocks at ~3 GiB/s on a modern laptop — so a
/// project with 10,000 audit entries (~5 MiB) verifies in under
/// 2 ms. We therefore run on the read side of the service lock
/// rather than `spawn_blocking_napi`-ing it.
/// Phase 17 Task 22: routed through [`spawn_blocking_napi`] because
/// the chain verification recomputes BLAKE3 over every recorded
/// entry and re-verifies the Merkle linkage. CPU-bound and grows
/// O(log_entries).
#[napi]
pub async fn project_audit_verify(path: String) -> Result<ChainVerificationJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.project_audit_verify(&path)).map(Into::into)
    })
    .await
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
/// (that's [`bim_attach_ifc`]).
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
/// Phase 17 Task 22: routed through [`spawn_blocking_napi`] because
/// `bim_attach_ifc` parses the entire `.ifc` file, computes the
/// dedup hash and writes the snapshot into the project. Multi-second
/// on real-world MEP federations.
#[napi]
pub async fn bim_attach_ifc(project_path: String, ifc_path: String) -> Result<BimAttachSummaryJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_attach_ifc(&project_path, &ifc_path))
            .map(Into::into)
    })
    .await
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
/// Phase 17 Task 22: routed through [`spawn_blocking_napi`]. The
/// command engine path takes the writer-side lock on `SERVICE`, runs
/// the command's `apply` function (which may walk hundreds of
/// entities and perform several SQLite writes), and appends to the
/// audit chain. The whole pipeline is blocking I/O + CPU and must
/// not stall the napi worker.
#[napi]
pub async fn command_apply(
    project_path: String,
    command_json: String,
) -> Result<CommandApplyResultJs> {
    let cmd: aec_command::commands::Command = serde_json::from_str(&command_json).map_err(|e| {
        Error::new(
            Status::InvalidArg,
            format!("command_apply: invalid command JSON: {e}"),
        )
    })?;
    spawn_blocking_napi(move || {
        with_service(|svc| svc.command_apply(&project_path, cmd)).map(Into::into)
    })
    .await
}

/// Undo the most recently applied command on `project_path`.
///
/// `active_scope` is one of `"design"`, `"draft"`, `"bim"`,
/// `"render"`, `"deliver"` — passed by the renderer to tag the
/// resulting audit envelope and to validate that the inverse
/// deltas don't cross a scope boundary (e.g. you can't undo a
/// design command while in the bim workflow).
///
/// Phase 17 Task 22: routed through [`spawn_blocking_napi`] for the
/// same reason as [`command_apply`].
#[napi]
pub async fn command_undo(
    project_path: String,
    active_scope: String,
) -> Result<CommandApplyResultJs> {
    let scope = parse_scope(&active_scope)?;
    spawn_blocking_napi(move || {
        with_service(|svc| svc.command_undo(&project_path, scope)).map(Into::into)
    })
    .await
}

/// Redo the most recently undone command. Symmetric counterpart
/// to [`command_undo`].
///
/// Phase 17 Task 22: routed through [`spawn_blocking_napi`].
#[napi]
pub async fn command_redo(
    project_path: String,
    active_scope: String,
) -> Result<CommandApplyResultJs> {
    let scope = parse_scope(&active_scope)?;
    spawn_blocking_napi(move || {
        with_service(|svc| svc.command_redo(&project_path, scope)).map(Into::into)
    })
    .await
}

/// List the project graph. Pass `kind_filter = None` for the full
/// graph; pass `Some(kind)` to narrow (e.g. `"wall"`, `"room"`,
/// `"camera"`). Read-only; safe to call concurrently with status
/// polls — routed through `with_service_ref_fallible`.
///
/// Phase 17 Task 22: dispatched to the napi blocking pool via
/// [`spawn_blocking_napi`]. On MEP federation projects the graph
/// list returns tens of thousands of rows; the SQL fetch plus
/// `Vec<EntityRecord>` materialisation and conversion to
/// `EntityRecordJs` would otherwise stall the napi worker thread
/// (and transitively the Electron main process JS event loop) for
/// hundreds of milliseconds.
#[napi]
pub async fn project_graph_list(
    project_path: String,
    kind_filter: Option<String>,
) -> Result<Vec<EntityRecordJs>> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| {
            svc.project_graph_list(&project_path, kind_filter.as_deref())
        })
        .map(|rs| rs.into_iter().map(Into::into).collect())
    })
    .await
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
/// would be a real-world bug worth surfacing). The seed library
/// ships `None` for every demo asset — see [`crate::service::
/// BridgeService::design_list_assets`] for why the list surface
/// deliberately doesn't base64-encode the underlying blob.
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
pub async fn design_list_assets(query: DesignListAssetsQueryJs) -> Result<Vec<AssetSummaryJs>> {
    let q = crate::service::AssetListQuery {
        search: query.search,
        tags: query.tags.unwrap_or_default(),
        style_tags: query.style_tags.unwrap_or_default(),
        limit: query.limit,
    };
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.design_list_assets(&q))
            .map(|rows| rows.into_iter().map(Into::into).collect())
    })
    .await
}

/// JS-facing renderer-side query parameters for
/// [`design_list_materials`]. Mirrors the `MaterialListQuery` shape
/// the renderer passes through `designListMaterials(query)` in
/// `apps/desktop/electron/bridge.ts`.
///
/// All fields are optional so the renderer can call this with an
/// empty object (`{}`) and get the full material library back. The
/// `style_tags` filter is the primary affordance the design-mode
/// `MaterialPanel` uses for the Scandinavian / Industrial / Japandi
/// tabs.
///
/// * `search` → renderer's case-insensitive name substring.
/// * `tags` → AND-matched against `PbrMaterial::tags`.
/// * `style_tags` → AND-matched against `PbrMaterial::style_tags`.
/// * `limit` → cap on result-set size. `None` returns everything.
#[napi(object)]
pub struct DesignListMaterialsQueryJs {
    pub search: Option<String>,
    pub tags: Option<Vec<String>>,
    pub style_tags: Option<Vec<String>>,
    pub limit: Option<u32>,
}

/// JS-facing PBR material summary. Field names align with the
/// TypeScript `MaterialSummary` interface in
/// `apps/desktop/electron/bridge.ts`.
///
/// `albedo` / `emissive` stay as linear-space `[r, g, b]` triples
/// (each component in `[0.0, 1.0]`) rather than CSS hex strings so
/// the renderer's PBR-style swatch sphere can shade directly from
/// the channel values — pre-stringifying here would force the
/// renderer to parse the CSS form back into floats for shading
/// math.
// napi-rs 2.x does NOT implement `FromNapiValue` for `f32` (JS numbers
// are IEEE-754 doubles, so only `f64` round-trips losslessly), which
// means `Vec<f32>` is also unsupported at the napi boundary. The
// `#[napi(object)]` derive macro generates BOTH `FromNapiValue` and
// `ToNapiValue` impls for object structs unconditionally, so any
// `Vec<f32>` field would fail to compile under `--all-features`.
// We use `Vec<f64>` here and cast to/from the service-layer `f32`
// linear-RGB representation at the conversion boundary.
#[napi(object)]
pub struct MaterialSummaryJs {
    pub material_id: String,
    pub name: String,
    pub albedo: Vec<f64>,
    pub metallic: f64,
    pub roughness: f64,
    pub ior: f64,
    pub transmission: f64,
    pub emissive: Vec<f64>,
    pub style_tags: Vec<String>,
    pub tags: Vec<String>,
}

impl From<crate::service::MaterialSummary> for MaterialSummaryJs {
    fn from(s: crate::service::MaterialSummary) -> Self {
        Self {
            material_id: s.material_id,
            name: s.name,
            albedo: s.albedo.iter().map(|v| *v as f64).collect(),
            metallic: s.metallic as f64,
            roughness: s.roughness as f64,
            ior: s.ior as f64,
            transmission: s.transmission as f64,
            emissive: s.emissive.iter().map(|v| *v as f64).collect(),
            style_tags: s.style_tags,
            tags: s.tags,
        }
    }
}

/// List materials from the process-wide PBR library matching
/// `query`. Read-only; routes through [`with_service_ref_fallible`]
/// so it runs concurrently with status polls, asset-list reads, and
/// other read-side endpoints without taking the service-wide write
/// lock.
///
/// The library is seeded once at boot from
/// `MaterialLibrary::with_default_pack()` (8 starter materials) —
/// the design-mode `MaterialPanel` calls this endpoint on mount to
/// populate the swatch grid and on every style-tag tab change to
/// re-filter. The bridge clamps `query.limit` to
/// [`crate::service::DESIGN_LIST_MATERIALS_MAX_LIMIT`] (10_000) so
/// a renderer bug sending a negative JS number that wraps to a
/// near-`u32::MAX` value through napi's `ToUint32()` coercion
/// can't allocate an unbounded result vector.
#[napi]
pub async fn design_list_materials(
    query: DesignListMaterialsQueryJs,
) -> Result<Vec<MaterialSummaryJs>> {
    let q = crate::service::MaterialListQuery {
        search: query.search,
        tags: query.tags.unwrap_or_default(),
        style_tags: query.style_tags.unwrap_or_default(),
        limit: query.limit,
    };
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.design_list_materials(&q))
            .map(|rows| rows.into_iter().map(Into::into).collect())
    })
    .await
}

/// JS-facing patch payload for [`design_update_material`]. Every
/// field is `Option` so the inspector can `PATCH`-style send only
/// the slider that moved; unset fields keep their current value.
///
/// `albedo` / `emissive` arrive as JS arrays of three numbers — the
/// `Vec<f64>` shape is forced by napi-rs's lack of fixed-size
/// array support at the napi-rs 2.x boundary AND by napi-rs's lack
/// of a `FromNapiValue` impl for `f32` (JS numbers are IEEE-754
/// doubles, so only `f64` round-trips through napi natively). The
/// bridge validates `len() == 3` before forwarding to the service-
/// layer `[f32; 3]` via `as f32` casts. Out-of-range values are
/// rejected by the service layer's `validate_material_update`,
/// surfacing through napi as an `Error::from_reason("invalid: …")`
/// the renderer can show on the inspector toast.
#[napi(object)]
pub struct MaterialUpdateJs {
    pub albedo: Option<Vec<f64>>,
    pub metallic: Option<f64>,
    pub roughness: Option<f64>,
    pub ior: Option<f64>,
    pub transmission: Option<f64>,
    pub emissive: Option<Vec<f64>>,
}

fn rgb_from_vec(field: &str, v: &[f64]) -> Result<[f32; 3]> {
    if v.len() != 3 {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{field} must have exactly 3 components, got {}", v.len()),
        ));
    }
    Ok([v[0] as f32, v[1] as f32, v[2] as f32])
}

/// Apply an inspector slider patch to a single material and return
/// the updated summary so the renderer can refresh its inspector
/// without a follow-up `design_list_materials` round-trip.
///
/// Validation runs *atomically* on the service side — a single
/// out-of-range slider can't half-apply a multi-field patch. The
/// service-layer guarantees:
///
/// * `metallic` / `roughness` / `transmission` in `[0.0, 1.0]`
/// * `ior` in `[1.0, 5.0]`
/// * `albedo` / `emissive` components in `[0.0, 1.0]`
///
/// All slider violations surface through napi as
/// `Error::from_reason("invalid: …")` — the renderer's
/// MaterialPanel inspector catches these and shows a toast without
/// invalidating the local UI state, so the user can re-drag the
/// slider into range without re-opening the inspector.
#[napi]
pub fn design_update_material(
    material_id: String,
    update: MaterialUpdateJs,
) -> Result<MaterialSummaryJs> {
    let albedo = update.albedo.as_deref().map(|v| rgb_from_vec("albedo", v));
    let emissive = update
        .emissive
        .as_deref()
        .map(|v| rgb_from_vec("emissive", v));
    let patch = crate::service::MaterialUpdate {
        albedo: albedo.transpose()?,
        metallic: update.metallic.map(|v| v as f32),
        roughness: update.roughness.map(|v| v as f32),
        ior: update.ior.map(|v| v as f32),
        transmission: update.transmission.map(|v| v as f32),
        emissive: emissive.transpose()?,
    };
    with_service_ref_fallible(|svc| svc.design_update_material(&material_id, &patch))
        .map(Into::into)
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
pub async fn export_pdf(params: ExportPdfParamsJs) -> Result<ExportPdfResultJs> {
    let body = params.body_lines.unwrap_or_default();
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| {
            svc.export_pdf(&params.out_path, &params.project_name, &body)
        })
        .map(Into::into)
    })
    .await
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
pub async fn export_dxf(params: ExportDxfParamsJs) -> Result<ExportDxfResultJs> {
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
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| {
            svc.export_dxf(&params.out_path, &params.project_name, &walls)
        })
        .map(Into::into)
    })
    .await
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
pub async fn export_ifc(params: ExportIfcParamsJs) -> Result<ExportIfcResultJs> {
    let storeys = params.storey_names.unwrap_or_default();
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| {
            svc.export_ifc(&params.out_path, &params.project_name, &storeys)
        })
        .map(Into::into)
    })
    .await
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
pub async fn export_gltf(params: ExportGltfParamsJs) -> Result<ExportGltfResultJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.export_gltf(&params.out_path, &params.project_name))
            .map(Into::into)
    })
    .await
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
    /// Path to the `.aecstudio` package. When supplied, the bridge
    /// builds a `DeliverPackContext` from the project graph so the
    /// proposal's cover paragraph cites the real room / material /
    /// template counts and embeds the project's floor-plan SVG.
    /// Optional for backward compatibility with renderer code that
    /// pre-dates Phase 14.
    pub project_path: Option<String>,
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
pub async fn export_build_proposal_pack(
    params: ExportProposalPackParamsJs,
) -> Result<ExportProposalPackResultJs> {
    spawn_blocking_napi(move || {
        let client = params.client_name.as_deref().unwrap_or("(client)");
        with_service_ref_fallible(|svc| {
            svc.export_proposal_pack(
                &params.out_path,
                &params.project_name,
                client,
                params.project_path.as_deref(),
            )
        })
        .map(Into::into)
    })
    .await
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
    /// Path to the open `.aecstudio` package. When supplied the
    /// bridge opens its SQLCipher DB and builds a real
    /// `DeliverPackContext` from the project graph (renders dir,
    /// material / BOQ schedules, sheets, IFC string, floor-plan
    /// SVG). Renderer code threads the active project path here.
    pub project_path: Option<String>,
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
pub async fn deliver_build_pack(
    params: DeliverBuildPackParamsJs,
) -> Result<DeliverBuildPackResultJs> {
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
        project_path: params.project_path,
    };
    // `with_service_ref_fallible` takes `FnOnce` (see helper declaration
    // ~600 LoC above) so the closure can consume `svc_params` directly
    // via `move` — no `.clone()` needed. Saves a `DeliverBuildPackParams`
    // copy per call. Flagged in PR-S round 2 ANALYSIS_0003.
    spawn_blocking_napi(move || {
        with_service_ref_fallible(move |svc| svc.deliver_build_pack(svc_params)).map(Into::into)
    })
    .await
}

/// JS-facing result of [`project_export_package`]. Mirrors the
/// renderer's `ProjectExportPackageResult` TS interface — `entries`
/// is the source-file count (excluding the auto-generated
/// `_aec_archive_manifest.json`) and `total_bytes` is the sum of
/// source payload bytes so the renderer's "Exported NNN files
/// (MM MB)" status line matches the bridge's view.
#[napi(object)]
pub struct ProjectExportPackageResultJs {
    pub out_path: String,
    pub entries: u32,
    /// `f64` so JS `number` carries the full uncompressed-bytes
    /// value without BigInt — the largest realistic project package
    /// is a few hundred MB, well under 2^53.
    pub total_bytes: f64,
}

impl From<crate::service::ProjectExportPackageResult> for ProjectExportPackageResultJs {
    fn from(r: crate::service::ProjectExportPackageResult) -> Self {
        Self {
            out_path: r.out_path,
            entries: r.entries,
            total_bytes: r.total_bytes as f64,
        }
    }
}

/// Pack the project package directory at `project_path` into a
/// portable ZIP archive at `out_path`. The archive embeds the full
/// package (encrypted `project.sqlite` + nonce + sub-directories
/// + per-source `blake3` hashes in `_aec_archive_manifest.json`).
/// Routes through [`crate::service::BridgeService::project_export_package`]
/// under the singleton read lock — long archive walks don't block
/// status polls.
#[napi]
pub async fn project_export_package(
    project_path: String,
    out_path: String,
) -> Result<ProjectExportPackageResultJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(move |svc| svc.project_export_package(&project_path, &out_path))
            .map(Into::into)
    })
    .await
}

// ============================================================
// design.* command façades (PR-W, Phase 1 + 2)
// ============================================================
//
// These four functions are thin façades over
// [`crate::service::BridgeService::command_apply`]. The renderer's
// `BridgeBackend.design{PaintMaterial,SetLighting,SaveCamera,
// PlaceFurniture}` interface predates the unified `commandApply`
// path; rather than break the renderer surface, the napi side
// constructs the matching `CommandKind` variant from the params
// payload and routes through the standard persistence pipeline.
//
// `params_json` is a JSON-stringified object matching the
// corresponding `aec_command::commands::{material::PaintMaterial,
// lighting::SetLighting, camera::SaveCamera, furniture::
// PlaceFurniture}` struct exactly — the renderer's `adaptNative()`
// just JSON.stringify's the params object.

/// JS-facing result of [`design_paint_material`] / [`design_set_lighting`].
/// Mirrors the renderer's `{ ok: true }` shape.
#[napi(object)]
pub struct DesignAckJs {
    pub ok: bool,
}

/// JS-facing result of [`design_place_furniture`]. Mirrors the
/// renderer's `{ entityId: string }` shape.
#[napi(object)]
pub struct DesignEntityIdJs {
    pub entity_id: String,
}

/// JS-facing result of [`design_save_camera`]. Mirrors the
/// renderer's `{ cameraId: string }` shape.
#[napi(object)]
pub struct DesignCameraIdJs {
    pub camera_id: String,
}

fn parse_design_params<T: serde::de::DeserializeOwned>(
    method: &str,
    params_json: &str,
) -> Result<T> {
    serde_json::from_str::<T>(params_json).map_err(|e| {
        Error::new(
            Status::InvalidArg,
            format!("{method}: invalid params JSON: {e}"),
        )
    })
}

/// Paint a material on an existing entity. `params_json` must
/// deserialise into [`aec_command::commands::material::PaintMaterial`].
#[napi]
pub fn design_paint_material(project_path: String, params_json: String) -> Result<DesignAckJs> {
    let inner: aec_command::commands::material::PaintMaterial =
        parse_design_params("design_paint_material", &params_json)?;
    let cmd = aec_command::commands::Command::user(
        aec_command::commands::CommandKind::PaintMaterial(inner),
    );
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DesignAckJs { ok: true })
}

/// Set the project's lighting preset. `params_json` must
/// deserialise into [`aec_command::commands::lighting::SetLighting`].
#[napi]
pub fn design_set_lighting(project_path: String, params_json: String) -> Result<DesignAckJs> {
    let inner: aec_command::commands::lighting::SetLighting =
        parse_design_params("design_set_lighting", &params_json)?;
    let cmd = aec_command::commands::Command::user(
        aec_command::commands::CommandKind::SetLighting(inner),
    );
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DesignAckJs { ok: true })
}

/// Save a camera. `params_json` must deserialise into
/// [`aec_command::commands::camera::SaveCamera`]. Returns the
/// camera's entity id (echoes back the input `entity_id` so the
/// renderer's stub shape is preserved).
#[napi]
pub fn design_save_camera(project_path: String, params_json: String) -> Result<DesignCameraIdJs> {
    let inner: aec_command::commands::camera::SaveCamera =
        parse_design_params("design_save_camera", &params_json)?;
    let camera_id = inner.entity_id.to_string();
    let cmd =
        aec_command::commands::Command::user(aec_command::commands::CommandKind::SaveCamera(inner));
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DesignCameraIdJs { camera_id })
}

/// Place a furniture instance referencing a catalogue asset.
/// `params_json` must deserialise into
/// [`aec_command::commands::furniture::PlaceFurniture`].
#[napi]
pub fn design_place_furniture(
    project_path: String,
    params_json: String,
) -> Result<DesignEntityIdJs> {
    let inner: aec_command::commands::furniture::PlaceFurniture =
        parse_design_params("design_place_furniture", &params_json)?;
    let entity_id = inner.entity_id.to_string();
    let cmd = aec_command::commands::Command::user(
        aec_command::commands::CommandKind::PlaceFurniture(inner),
    );
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DesignEntityIdJs { entity_id })
}

// ============================================================
// bim.* classification + property mutation (PR-W, Phase 3 + 4)
// ============================================================

/// JS-facing per-entity classification assignment row. Mirrors the
/// renderer's `BimClassifyAssignment` TS interface.
#[napi(object)]
pub struct BimClassifyAssignmentJs {
    pub entity_id: String,
    pub code: String,
    pub title: String,
}

impl From<crate::service::BimClassifyAssignment> for BimClassifyAssignmentJs {
    fn from(a: crate::service::BimClassifyAssignment) -> Self {
        Self {
            entity_id: a.entity_id,
            code: a.code,
            title: a.title,
        }
    }
}

/// JS-facing result of [`bim_classify`]. Mirrors the renderer's
/// `BimClassifyResult` interface.
///
/// See [`crate::service::BimClassifyResult`] for the full counter
/// contract; in summary:
/// * `classified` — entities whose DB row was actually modified
///   (the count the renderer should use for "Undo classify?" /
///   "N entities re-classified" toasts).
/// * `unchanged` — entities the scheme recognised but whose row
///   already carried the target value (a no-op re-run).
/// * `skipped` — entities whose `kind` wasn't recognised by the
///   scheme's lookup table at all.
///
/// `details` carries one row per *recognised* entity (i.e. one
/// row per entity contributing to `classified + unchanged`).
#[napi(object)]
pub struct BimClassifyResultJs {
    pub scheme: String,
    pub classified: u32,
    pub unchanged: u32,
    pub skipped: u32,
    pub details: Vec<BimClassifyAssignmentJs>,
}

impl From<crate::service::BimClassifyResult> for BimClassifyResultJs {
    fn from(r: crate::service::BimClassifyResult) -> Self {
        Self {
            scheme: r.scheme,
            classified: r.classified,
            unchanged: r.unchanged,
            skipped: r.skipped,
            details: r.details.into_iter().map(Into::into).collect(),
        }
    }
}

/// Walk every entity in the project graph and assign a
/// classification from `scheme`. Supported schemes: `"ifc"`,
/// `"uniformat-ii"`, `"omniclass-21"`. Routes through
/// [`crate::service::BridgeService::bim_classify`].
///
/// Routed through `with_service_ref_fallible` (read lock) because
/// `bim_classify` only mutates the project's own SQLite DB — the
/// `BridgeService` singleton is NOT written. Each call opens a
/// fresh `ProjectPackage::open_with_master_key_and_database`
/// connection, so concurrent classifications against different
/// projects don't share a connection. Concurrent classifications
/// against the *same* project are serialised by SQLite's WAL-mode
/// busy-timeout (set by `bim_classify` itself), not the bridge-wide
/// `RwLock`. This keeps the read lock free for concurrent status
/// polls (`runtimeStatus`, `renderListJobs`, etc.) during a
/// potentially long classification walk.
#[napi]
pub async fn bim_classify(project_path: String, scheme: String) -> Result<BimClassifyResultJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(move |svc| svc.bim_classify(&project_path, &scheme))
            .map(Into::into)
    })
    .await
}

/// JS-facing result of [`bim_set_property`]. Mirrors the
/// renderer's `BimSetPropertyResult` interface — `previousValue`
/// is the prior value (if any) so the renderer can wire undo
/// without an extra round-trip.
#[napi(object)]
pub struct BimSetPropertyResultJs {
    pub entity_id: String,
    pub pset: String,
    pub key: String,
    pub previous_value: Option<String>,
}

impl From<crate::service::BimSetPropertyResult> for BimSetPropertyResultJs {
    fn from(r: crate::service::BimSetPropertyResult) -> Self {
        Self {
            entity_id: r.entity_id,
            pset: r.pset,
            key: r.key,
            previous_value: r.previous_value,
        }
    }
}

/// Set a property on a BIM entity. The value lands in a
/// `components` row of kind `aec/property/<pset>` so it survives
/// a `bim_attach_ifc` re-attach (which wipes `bim/%` for changed
/// entities). Routes through
/// [`crate::service::BridgeService::bim_set_property`].
///
/// Routed through `with_service_ref_fallible` (read lock) for the
/// same reasons as [`bim_classify`]: the `BridgeService` itself is
/// NOT mutated (the writes go to the project's SQLite DB, not to
/// the singleton), and each call opens its own DB connection.
/// Concurrent property edits on the same project are serialised
/// by SQLite's WAL-mode busy-timeout, not by the bridge-wide
/// `RwLock`. This keeps read-only status polls responsive during
/// a batch property update.
///
/// Property edits **bypass** the [`crate::service::BridgeService`]
/// command engine, so they do **not** participate in undo/redo at
/// the engine level. The returned `previousValue` is provided so the
/// renderer's local undo stack can re-call `bim_set_property` with
/// the prior value — this is a UI-level undo, not an engine-level
/// one. If a future change wants engine-level undo for property
/// edits, this method needs to be reframed as a `Command::user`
/// variant and routed through `command_apply`.
#[napi]
pub fn bim_set_property(
    project_path: String,
    entity_id: String,
    pset: String,
    key: String,
    value: String,
) -> Result<BimSetPropertyResultJs> {
    with_service_ref_fallible(move |svc| {
        svc.bim_set_property(&project_path, &entity_id, &pset, &key, &value)
    })
    .map(Into::into)
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
pub async fn bim_export_ifc(ifc_path: String, out_path: String) -> Result<BimExportIfcSummaryJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_export_ifc(&ifc_path, &out_path)).map(Into::into)
    })
    .await
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
pub async fn bim_validate(ifc_path: String) -> Result<BimValidateReportJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_validate(&ifc_path)).map(Into::into)
    })
    .await
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
pub async fn bim_diff(before_path: String, after_path: String) -> Result<BimDiffSummaryJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_diff(&before_path, &after_path)).map(Into::into)
    })
    .await
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
pub async fn bim_generate_schedule(
    ifc_path: String,
    kind: String,
    out_path: String,
) -> Result<BimScheduleSummaryJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_generate_schedule(&ifc_path, &kind, &out_path))
            .map(Into::into)
    })
    .await
}

/// JS-facing shape returned by [`bim_read_schedule_rows`]. Mirrors
/// `BimScheduleRows` in `apps/desktop/electron/bridge.ts`.
///
/// `header` is the worksheet's header-row column display names in
/// order; `rows` is one row per non-empty body row, keyed by header
/// display name. We materialize `rows` as `HashMap<String, String>`
/// (not `BTreeMap`) because napi-rs's object marshalling is wired
/// for `HashMap`; the renderer treats both as plain objects, so the
/// difference is internal to the bridge.
#[napi(object)]
pub struct BimScheduleRowsJs {
    pub header: Vec<String>,
    pub rows: Vec<std::collections::HashMap<String, String>>,
}

impl From<crate::service::BimScheduleRows> for BimScheduleRowsJs {
    fn from(r: crate::service::BimScheduleRows) -> Self {
        Self {
            header: r.header,
            rows: r
                .rows
                .into_iter()
                .map(|m| m.into_iter().collect::<std::collections::HashMap<_, _>>())
                .collect(),
        }
    }
}

/// Read rows back from a previously-written XLSX schedule. Called
/// from the renderer's `ScheduleView` immediately after
/// `bim_generate_schedule` so the table can render real row data
/// inline rather than just the row count.
///
/// The implementation routes through `with_service_ref_fallible`
/// because the XLSX read is read-only at the `BridgeService` layer
/// (no snapshot cache writes, no graph mutation). Errors are
/// flattened to `napi::Error` via the same `to_napi_error` mapping
/// used by every other bridge surface.
#[napi]
pub async fn bim_read_schedule_rows(xlsx_path: String) -> Result<BimScheduleRowsJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.bim_read_schedule_rows(&xlsx_path)).map(Into::into)
    })
    .await
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
    /// RFC3339 timestamp the render started (or `None` if it is
    /// still queued). Renderer constructs a JS `Date` to compute
    /// elapsed time / ETA in `RenderQueue.tsx`.
    pub started_at: Option<String>,
    /// RFC3339 timestamp the render completed.
    pub completed_at: Option<String>,
    /// Absolute path to the output image on disk (`None` while the
    /// job is queued or running).
    pub output_path: Option<String>,
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
            started_at: s.started_at,
            completed_at: s.completed_at,
            output_path: s.output_path,
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

/// Read the output image bytes for a completed render job. Bound to
/// the renderer as `aec.render.getOutputImage({ jobId })`. The
/// returned `Buffer` is base64-encoded by the renderer and used as
/// the `src` of an `<img>` for `RenderPreview` /
/// `BeforeAfterCompare`.
///
/// Declared `async` and routed through [`spawn_blocking_napi`] so
/// the underlying `std::fs::read` of a possibly-tens-of-MB PNG runs
/// on the tokio blocking pool, not the libuv main thread. A 4K
/// render output is large enough (cold page cache + slow disk) that
/// a synchronous read would stall the libuv loop for hundreds of
/// milliseconds, blocking every other IPC handler including
/// `viewport:requestFrame`. The service-side fix (see
/// [`BridgeService::render_get_output_image`]) already drops the
/// `render_state` mutex before the read so other render-state
/// callers stay live; promoting the napi wrapper completes the fix
/// by also keeping the JS event loop responsive. Same pattern as
/// [`bim_import_ifc`].
#[napi]
pub async fn render_get_output_image(job_id: String) -> Result<RenderOutputImageJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.render_get_output_image(&job_id)).map(Into::into)
    })
    .await
}

/// Compute the SSIM between two completed render jobs. Bound to the
/// renderer as `aec.render.compareSsim({ aJobId, bJobId })`.
///
/// Declared `async` and routed through [`spawn_blocking_napi`]
/// because SSIM (Wang et al. 2004) is O(width*height) over two
/// decoded images: at 4K that's two PNG decodes plus a sliding 8×8
/// window over ~33 M pixels, easily reaching a couple of seconds.
/// Running synchronously on the libuv main thread would freeze the
/// entire UI — every IPC handler queues behind this single compare
/// click. The service-side fix (see
/// [`BridgeService::render_compare_ssim`]) already drops the
/// `render_state` mutex before SSIM so other render-state callers
/// stay live; the async promotion here completes the fix by moving
/// the heavy compute to the tokio blocking pool. Same pattern as
/// [`bim_import_ifc`] / [`render_get_output_image`].
#[napi]
pub async fn render_compare_ssim(
    a_job_id: String,
    b_job_id: String,
) -> Result<RenderCompareResultJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.render_compare_ssim(&a_job_id, &b_job_id))
            .map(Into::into)
    })
    .await
}

/// Set the HDRI environment map for future render jobs. Pass `null`
/// to fall back to the procedural Hosek-Wilkie sky. Bound as
/// `aec.render.setEnvironmentMap({ path, intensity })`.
#[napi]
pub fn render_set_environment_map(path: Option<String>, intensity: Option<f64>) -> Result<()> {
    with_service_ref_fallible(|svc| {
        svc.render_set_environment_map(path.as_deref(), intensity.map(|v| v as f32))
    })
}

/// JS-facing render output image returned by [`render_get_output_image`].
#[napi(object)]
pub struct RenderOutputImageJs {
    pub job_id: String,
    pub path: String,
    pub bytes: napi::bindgen_prelude::Buffer,
}

impl From<crate::service::RenderOutputImage> for RenderOutputImageJs {
    fn from(o: crate::service::RenderOutputImage) -> Self {
        Self {
            job_id: o.job_id,
            path: o.path,
            bytes: o.bytes.into(),
        }
    }
}

/// JS-facing render compare result returned by [`render_compare_ssim`].
#[napi(object)]
pub struct RenderCompareResultJs {
    pub a_job_id: String,
    pub b_job_id: String,
    pub ssim: f64,
}

impl From<crate::service::RenderCompareResult> for RenderCompareResultJs {
    fn from(o: crate::service::RenderCompareResult) -> Self {
        Self {
            a_job_id: o.a_job_id,
            b_job_id: o.b_job_id,
            ssim: o.ssim,
        }
    }
}

// ---------------------------------------------------------------
// AI endpoints — local LLM sidecar surface (Phase 10, PR-V)
// ---------------------------------------------------------------
//
// All six methods route through `with_service_ref_fallible` because
// mutation of the sidecar handle / pending diff map happens behind
// [`AiState`]'s interior synchronisation (per-field `Mutex`/`RwLock`).
// That means:
//
//   * the singleton `RwLock<BridgeService>` is held in *read* mode
//     the whole time, so unrelated read-only endpoints
//     (`project_engine_status` polls, `bim_export_ifc`, etc.) keep
//     flowing — important for the status pane while a plan is
//     running.
//
// **All six AI endpoints are `#[napi] async fn`** routed through
// `spawn_blocking_napi`, even the ones whose hot path completes in
// microseconds. The reason is the **first call** of a session:
// `ensure_ready` synchronously spawns the `llama-server` child and
// waits up to `DEFAULT_SPAWN_TIMEOUT` (30 s) for its `/health`
// probe. While that spawn is in flight, `ai_cancel_job` blocks on
// the handle-slot mutex inside [`AiState`] (it must, to avoid
// racing `take()` against a freshly-`Some()` write). Were
// `ai_cancel_job` a synchronous napi function, the libuv main
// thread would freeze for the entire 30 s window — the user's
// "Stop" click would not be processed, the Electron UI would hang,
// and the JS-side IPC queue would back up.
//
// Wrapping in `spawn_blocking_napi` moves the blocking work to the
// tokio blocking thread pool. The libuv main thread stays free; the
// renderer can keep polling `ai_runtime_status` (which returns
// `"loading"` from the `runtime` `RwLock` read side instantly —
// see the [`AiState`] module doc on lock granularity) and keep
// processing user input. The cancel itself still has to wait for
// the spawn to complete before it can `take()` the handle, but
// that wait happens on a worker thread, not the JS event loop.
//
// The cost of the `Promise<T>` wrapping is one tokio task hop per
// call (~microseconds). For status polls firing every ~500 ms this
// is invisible against the IPC round-trip cost. We accept it to
// get unconditional JS event-loop responsiveness, which is the
// architecturally correct fix for the cold-spawn UI freeze.

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
pub struct AiAcceptOutcomeJs {
    pub ok: bool,
    pub diff_id: String,
    pub op_count: u32,
    pub applied_count: u32,
    pub skipped: Vec<AiAcceptSkippedJsRow>,
    pub command_ids: Vec<String>,
    pub audit_chain_head: String,
}

#[napi(object)]
pub struct AiAcceptSkippedJsRow {
    pub op_index: u32,
    pub reason: String,
}

#[napi(object)]
pub struct AiRejectOutcomeJs {
    pub ok: bool,
    pub diff_id: String,
    pub op_count: u32,
    pub reason: Option<String>,
    pub audit_chain_head: String,
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
///
/// Declared `async` and routed through `spawn_blocking_napi` for
/// consistency with the rest of the AI surface; see the AI
/// endpoints concurrency block above for the rationale.
#[napi]
pub async fn ai_list_tools() -> Result<Vec<AiToolJs>> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(super::service::BridgeService::ai_list_tools)
            .map(|v| v.into_iter().map(Into::into).collect::<Vec<AiToolJs>>())
    })
    .await
}

/// JS-facing wire shape of an
/// [`aec_core::ExtensionLoadDiagnostic`]. Mirrors the renderer's
/// `ExtensionLoadDiagnostic` interface in
/// `apps/desktop/electron/bridge.ts`. `extension_id` is optional
/// (None when the manifest could not even be parsed) and surfaces as
/// `null` over the wire. `stage` is the stable wire string from
/// [`aec_core::ExtensionLoadStage::as_wire_str`].
#[napi(object)]
pub struct ExtensionLoadDiagnosticJs {
    /// Manifest `id` if the loader got far enough to parse it.
    /// `None` becomes `null` in JS.
    pub extension_id: Option<String>,
    /// Extension directory (or manifest file path) on disk. Encoded
    /// as a UTF-8 string with the platform's native separators —
    /// the renderer is responsible for any cosmetic shortening.
    pub path: String,
    /// Stable stage tag — one of `manifest_read`, `manifest_parse`,
    /// `manifest_validation`, `unsafe_path`,
    /// `signature_verification`, `duplicate_id`,
    /// `asset_pack_install`, `ai_tool_resolution`. The renderer maps
    /// each tag to a user-facing label.
    pub stage: String,
    /// Human-readable error string — produced from the underlying
    /// typed error's `Display` impl so the renderer can show it as
    /// the diagnostic detail without further interpretation.
    pub message: String,
}

impl From<&aec_core::ExtensionLoadDiagnostic> for ExtensionLoadDiagnosticJs {
    fn from(d: &aec_core::ExtensionLoadDiagnostic) -> Self {
        Self {
            extension_id: d.extension_id.clone(),
            path: d.path.to_string_lossy().into_owned(),
            stage: d.stage.as_wire_str().to_string(),
            message: d.message.clone(),
        }
    }
}

/// Return every per-extension boot failure that was captured during
/// [`crate::service::BridgeService::new`]. The renderer reaches this
/// method through the `extensions:listLoadDiagnostics` IPC and
/// surfaces the list as a read-only Settings diagnostics card.
///
/// Returns an empty vector when no extensions failed to load — the
/// renderer treats that as the signal to hide the diagnostics card
/// entirely. The list is frozen for the lifetime of the bridge
/// (extensions are not hot-reloaded in this revision).
///
/// Sync read because the captured diagnostics live in process
/// memory and the slice copy is O(N) over a list that is almost
/// always empty in production (broken extensions are rare).
#[napi]
pub fn extension_load_diagnostics() -> Result<Vec<ExtensionLoadDiagnosticJs>> {
    with_service_ref(|svc| {
        svc.extension_load_diagnostics()
            .iter()
            .map(ExtensionLoadDiagnosticJs::from)
            .collect()
    })
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
    project_path: String,
    tool: String,
    scope: String,
    prompt: String,
    context_json: String,
    max_entities_modified: u32,
) -> Result<AiPlanResultJs> {
    let scope = parse_scope(&scope)?;
    spawn_blocking_napi(move || {
        let r = with_service_ref_fallible(|svc| {
            svc.ai_plan(
                &project_path,
                &tool,
                scope,
                &prompt,
                &context_json,
                max_entities_modified,
            )
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

/// Accept a pending diff and apply it to the project graph.
///
/// Phase 11 task 10: this is the entry point through which AI
/// suggestions become real, undoable mutations. The diff is
/// converted into a `Vec<Command>` and persisted in a single SQL
/// transaction; the result carries the per-op apply telemetry the
/// renderer needs to show "4 applied, 1 skipped" and to wire the
/// command ids into its undo stack.
///
/// `async` for the cold-spawn responsiveness reason in the AI
/// endpoints block above, AND because the apply now opens a
/// SQLCipher connection and walks a transaction — a meaningful
/// amount of blocking IO that must not run on the libuv main
/// thread.
#[napi]
pub async fn ai_accept_diff(diff_id: String) -> Result<AiAcceptOutcomeJs> {
    spawn_blocking_napi(move || {
        with_service(|svc| svc.ai_accept_diff(&diff_id)).map(|r| AiAcceptOutcomeJs {
            ok: r.ok,
            diff_id: r.diff_id,
            op_count: r.op_count,
            applied_count: r.applied_count,
            skipped: r
                .skipped
                .into_iter()
                .map(|s| AiAcceptSkippedJsRow {
                    op_index: s.op_index,
                    reason: s.reason,
                })
                .collect(),
            command_ids: r.command_ids,
            audit_chain_head: r.audit_chain_head,
        })
    })
    .await
}

/// Reject a pending diff and log the rejection (with optional
/// `reason`) to the project's AI audit trail.
#[napi]
pub async fn ai_reject_diff(diff_id: String, reason: Option<String>) -> Result<AiRejectOutcomeJs> {
    spawn_blocking_napi(move || {
        // Devin Review `ANALYSIS_0001` (round 1): use the *read*
        // lock (`with_service_ref_fallible`) rather than the write
        // lock (`with_service`). The reject path does not mutate
        // `BridgeService` directly — `ai_state.peek_diff` /
        // `finalize_diff` already take `&self` and own their own
        // interior locks, and the audit append is a static helper.
        // Holding only a read lock here means concurrent status
        // polls and render-job listings no longer serialize behind
        // a reject's audit-disk-I/O. (Accept must continue to use
        // the write lock because `command_apply_on_conn` mutates
        // the project graph through `&mut self`.)
        with_service_ref_fallible(|svc| svc.ai_reject_diff(&diff_id, reason.as_deref())).map(|r| {
            AiRejectOutcomeJs {
                ok: r.ok,
                diff_id: r.diff_id,
                op_count: r.op_count,
                reason: r.reason,
                audit_chain_head: r.audit_chain_head,
            }
        })
    })
    .await
}

/// Cancel any in-flight AI work by killing the sidecar process.
/// `job_id` is accepted for forward compatibility but currently
/// ignored — there's only one in-flight plan at a time (see the
/// concurrency rationale on the AI endpoints block above).
///
/// `async` and routed through `spawn_blocking_napi` because during
/// a cold-spawn this call blocks on the handle-slot mutex inside
/// [`AiState`] for the full `DEFAULT_SPAWN_TIMEOUT` window
/// (~30 s). The blocking-pool wrapper keeps the libuv main thread
/// free during that wait so the renderer UI stays responsive.
#[napi]
pub async fn ai_cancel_job(job_id: String) -> Result<AiCancelResultJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.ai_cancel_job(&job_id)).map(|r| AiCancelResultJs {
            cancelled: r.cancelled,
        })
    })
    .await
}

/// Read the sidecar's current lifecycle state. The renderer polls
/// this every ~500 ms while a plan is in flight to drive the model
/// loading indicator.
///
/// `async` and routed through `spawn_blocking_napi` so the libuv
/// main thread is never blocked, even by the
/// `RwLockReadGuard::lock()` syscall during a state transition. On
/// the hot path the actual work is O(microseconds); the blocking-
/// pool hop is the cost of unconditional UI responsiveness.
#[napi]
pub async fn ai_runtime_status() -> Result<AiRuntimeStatusJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(super::service::BridgeService::ai_runtime_status).map(|r| {
            AiRuntimeStatusJs {
                state: r.state,
                last_error: r.last_error,
                pending_diff_ids: r.pending_diff_ids,
            }
        })
    })
    .await
}

#[napi(object)]
pub struct AiModelTierInfoJs {
    pub tier: String,
    pub name: String,
    pub filename: String,
    pub size_bytes: BigInt,
    pub available: bool,
    pub size_on_disk: BigInt,
}

#[napi(object)]
pub struct AiModelAvailabilityJs {
    pub tiers: Vec<AiModelTierInfoJs>,
    pub active_tier: String,
    pub models_dir: String,
}

#[napi(object)]
pub struct AiDownloadProgressJs {
    pub tier: String,
    pub downloaded: BigInt,
    pub total: BigInt,
    /// One of `"downloading" | "verifying" | "completed" | "failed"`.
    pub state: String,
    pub message: Option<String>,
}

#[napi(object)]
pub struct AiDownloadResultJs {
    pub tier: String,
    pub path: String,
    pub size_bytes: BigInt,
}

/// Snapshot of which Ternary-Bonsai tiers are available on disk and
/// which one is currently the active tier. The Settings page calls
/// this on mount and after every download / delete to redraw its
/// per-tier badges.
#[napi]
pub async fn ai_model_availability() -> Result<AiModelAvailabilityJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(super::service::BridgeService::ai_model_availability).map(|r| {
            AiModelAvailabilityJs {
                tiers: r
                    .tiers
                    .into_iter()
                    .map(|t| AiModelTierInfoJs {
                        tier: t.tier,
                        name: t.name,
                        filename: t.filename,
                        size_bytes: BigInt::from(t.size_bytes),
                        available: t.available,
                        size_on_disk: BigInt::from(t.size_on_disk),
                    })
                    .collect(),
                active_tier: r.active_tier,
                models_dir: r.models_dir,
            }
        })
    })
    .await
}

/// Download the GGUF for `tier` to the configured `models_dir` and
/// verify its BLAKE3. Blocks for the duration of the download
/// (typically tens of seconds for the 1.7B model on a fast link, up
/// to a few minutes for 8B). Routed through `spawn_blocking_napi` so
/// the libuv main thread stays free for `ai_download_progress` polls
/// (which run every ~500 ms to drive the Settings progress bar) and
/// every other IPC call.
///
/// **Lock discipline.** The `SERVICE` `RwLock` reader guard is held
/// **only** long enough for [`BridgeService::ai_prepare_download`] to
/// clone the descriptor + paths + the shared progress `Arc<Mutex<…>>`
/// out of the model_manager (microseconds). The reader guard is then
/// dropped before [`BridgeService::run_ai_download`] kicks off the
/// HTTPS transfer, so writer-side callers (`project_save`,
/// `command_apply`, `bridge_init`) run in parallel with the
/// multi-minute download instead of waiting on the bridge `RwLock`.
#[napi]
pub async fn ai_download_model(tier: String) -> Result<AiDownloadResultJs> {
    spawn_blocking_napi(move || {
        // Phase 1: brief read-lock to capture an owned download
        // context, then drop the SERVICE `RwLock` reader guard.
        let ctx = with_service_ref_fallible(|svc| svc.ai_prepare_download(&tier))?;
        // Phase 2: HTTPS download + BLAKE3 verify + rename without
        // holding ANY `BridgeService` lock. Concurrent `with_service`
        // writers (project_save / command_apply / etc.) run while the
        // download is in flight.
        let r =
            BridgeService::run_ai_download(ctx).map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(AiDownloadResultJs {
            tier: r.tier,
            path: r.path,
            size_bytes: BigInt::from(r.size_bytes),
        })
    })
    .await
}

/// Return the most recent download progress snapshot, or `null` when
/// no download has run this session. The Settings download panel
/// polls this every ~500 ms while a download is in flight; otherwise
/// the renderer can just call it once on mount.
///
/// Sync read because the slot is a single `Mutex<Option<...>>` and
/// every read is O(microseconds).
#[napi]
pub fn ai_download_progress() -> Result<Option<AiDownloadProgressJs>> {
    with_service_ref_fallible(super::service::BridgeService::ai_download_progress).map(|opt| {
        opt.map(|p| AiDownloadProgressJs {
            tier: p.tier,
            downloaded: BigInt::from(p.downloaded),
            total: BigInt::from(p.total),
            state: p.state,
            message: p.message,
        })
    })
}

/// Overwrite the active tier on the in-process model manager. Does
/// NOT respawn the sidecar — the next `ai_plan` cold-spawn picks up
/// the new tier's GGUF via `ModelManager::active_config`.
#[napi]
pub async fn ai_set_active_tier(tier: String) -> Result<()> {
    spawn_blocking_napi(move || with_service_ref_fallible(|svc| svc.ai_set_active_tier(&tier)))
        .await
}

// ============================================================
// draft.* / deliver.* (Group A, Phase 10)
// ============================================================

/// JS-facing result of [`draft_import_dxf`]. Mirrors the TS
/// `DraftImportDxfResult` interface (entityCount / layerCount /
/// blockCount / skippedCount in camelCase).
#[napi(object)]
pub struct DraftImportDxfJs {
    pub entity_count: u32,
    pub layer_count: u32,
    pub block_count: u32,
    pub skipped_count: u32,
}

/// JS-facing result of [`draft_export_dxf`].
#[napi(object)]
pub struct DraftExportDxfJs {
    pub path: String,
    pub entity_count: u32,
    pub file_size: u32,
}

#[napi(object)]
pub struct DraftEntityIdJs {
    pub entity_id: String,
}

#[napi(object)]
pub struct DraftSheetIdJs {
    pub sheet_id: String,
}

/// Draw a single primitive (line / polyline / arc / circle / ellipse
/// / spline / hatch / text). `params_json` deserialises into
/// [`aec_command::commands::draft::DrawPrimitive`].
#[napi]
pub fn draft_draw_primitive(project_path: String, params_json: String) -> Result<DraftEntityIdJs> {
    let inner: aec_command::commands::draft::DrawPrimitive =
        parse_design_params("draft_draw_primitive", &params_json)?;
    let entity_id = inner.entity_id.to_string();
    let cmd = aec_command::commands::Command::user(
        aec_command::commands::CommandKind::DrawPrimitive(inner),
    );
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DraftEntityIdJs { entity_id })
}

/// Apply a 2D edit tool (move / copy / rotate / scale / mirror /
/// offset / trim / extend / fillet / chamfer / stretch).
/// `params_json` deserialises into
/// [`aec_command::commands::draft::EditTool`].
#[napi]
pub fn draft_edit_tool(project_path: String, params_json: String) -> Result<DesignAckJs> {
    let inner: aec_command::commands::draft::EditTool =
        parse_design_params("draft_edit_tool", &params_json)?;
    let cmd =
        aec_command::commands::Command::user(aec_command::commands::CommandKind::EditTool(inner));
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DesignAckJs { ok: true })
}

/// Create a sheet for plotting. `params_json` deserialises into
/// [`aec_command::commands::draft::CreateSheet`].
#[napi]
pub fn draft_create_sheet(project_path: String, params_json: String) -> Result<DraftSheetIdJs> {
    let inner: aec_command::commands::draft::CreateSheet =
        parse_design_params("draft_create_sheet", &params_json)?;
    let sheet_id = inner.entity_id.to_string();
    let cmd = aec_command::commands::Command::user(
        aec_command::commands::CommandKind::CreateSheet(inner),
    );
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DraftSheetIdJs { sheet_id })
}

/// Upsert layer state (color / linetype / lineweight / visibility /
/// freeze / lock / plottable / description). `params_json`
/// deserialises into
/// [`aec_command::commands::draft::SetLayerState`].
#[napi]
pub fn draft_set_layer_state(project_path: String, params_json: String) -> Result<DesignAckJs> {
    let inner: aec_command::commands::draft::SetLayerState =
        parse_design_params("draft_set_layer_state", &params_json)?;
    let cmd = aec_command::commands::Command::user(
        aec_command::commands::CommandKind::SetLayerState(inner),
    );
    with_service(|svc| svc.command_apply(&project_path, cmd))?;
    Ok(DesignAckJs { ok: true })
}

/// Import a DXF file into the project graph. Each importable DXF
/// entity is routed through `command_apply` so the import is
/// journaled / auditable / undo-able. Async because reads can be
/// large (10s of MB DXF files are common).
#[napi]
pub async fn draft_import_dxf(project_path: String, dxf_path: String) -> Result<DraftImportDxfJs> {
    spawn_blocking_napi(move || {
        with_service(|svc| svc.draft_import_dxf(&project_path, &dxf_path)).map(|r| {
            DraftImportDxfJs {
                entity_count: r.entity_count,
                layer_count: r.layer_count,
                block_count: r.block_count,
                skipped_count: r.skipped_count,
            }
        })
    })
    .await
}

/// Export the project graph's draft primitives to a DXF file.
/// Async because writing can be large.
#[napi]
pub async fn draft_export_dxf(project_path: String, dxf_path: String) -> Result<DraftExportDxfJs> {
    spawn_blocking_napi(move || {
        with_service_ref_fallible(|svc| svc.draft_export_dxf(&project_path, &dxf_path)).map(|r| {
            DraftExportDxfJs {
                path: r.path,
                entity_count: r.entity_count,
                // u64 doesn't cross the NAPI boundary cleanly on all
                // hosts; the renderer already shows file_size as a
                // human-readable string and a 4 GiB cap on a draft
                // DXF export is well beyond any reasonable project.
                file_size: u32::try_from(r.file_size).unwrap_or(u32::MAX),
            }
        })
    })
    .await
}

/// JS-facing mirror of [`crate::service::RevisionSummary`]. The
/// service serialises with `#[serde(rename_all = "camelCase")]` so
/// the field order here matches the renderer's TS interface
/// exactly. We re-serialise through JSON rather than mapping fields
/// because the nested `tracked_entities` Vec doesn't cross NAPI
/// directly.
#[napi(object)]
pub struct RevisionSummaryJs {
    /// JSON-serialised
    /// [`crate::service::RevisionSummary`]. The renderer
    /// `JSON.parse`s this once on receipt to obtain a typed
    /// `RevisionSummary`. We pass JSON instead of a flat NAPI
    /// object because the inner `tracked_entities` list does not
    /// flatten cleanly to a NAPI struct.
    pub summary_json: String,
}

#[napi(object)]
pub struct RevisionDiffJs {
    /// JSON-serialised
    /// [`crate::service::RevisionDiffReport`].
    pub diff_json: String,
}

#[napi(object)]
pub struct RevisionTrackedEntityJs {
    pub category: String,
    pub id: String,
    pub payload_hash: String,
    pub label: Option<String>,
}

/// Create a tagged revision snapshot.
///
/// Routed through [`spawn_blocking_napi`] because the underlying
/// service call opens the encrypted project package, reads the
/// entire entity table to compute the tracked-entity list when the
/// caller supplies none, and writes the snapshot file to disk — all
/// blocking I/O that would otherwise stall the Electron main
/// (libuv) thread on large projects.
#[napi]
pub async fn deliver_create_revision(
    project_path: String,
    tag: String,
    description: String,
    entities: Option<Vec<RevisionTrackedEntityJs>>,
) -> Result<RevisionSummaryJs> {
    let caller = entities.map(|v| {
        v.into_iter()
            .map(|e| crate::service::RevisionTrackedEntity {
                category: e.category,
                id: e.id,
                payload_hash: e.payload_hash,
                label: e.label,
            })
            .collect()
    });
    spawn_blocking_napi(move || {
        let rev = with_service(|svc| {
            svc.deliver_create_revision(&project_path, &tag, &description, caller)
        })?;
        let summary_json = serde_json::to_string(&rev).map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("deliver_create_revision: serialize: {e}"),
            )
        })?;
        Ok(RevisionSummaryJs { summary_json })
    })
    .await
}

/// List all revision snapshots in chronological order.
///
/// Routed through [`spawn_blocking_napi`] for the same reason as
/// [`deliver_create_revision`]: enumerating revisions opens the
/// project package and reads the on-disk snapshot index.
#[napi]
pub async fn deliver_list_revisions(project_path: String) -> Result<Vec<RevisionSummaryJs>> {
    spawn_blocking_napi(move || {
        let revs = with_service_ref_fallible(|svc| svc.deliver_list_revisions(&project_path))?;
        revs.into_iter()
            .map(|r| {
                let summary_json = serde_json::to_string(&r).map_err(|e| {
                    Error::new(
                        Status::GenericFailure,
                        format!("deliver_list_revisions: serialize: {e}"),
                    )
                })?;
                Ok(RevisionSummaryJs { summary_json })
            })
            .collect()
    })
    .await
}

/// Diff two revisions.
///
/// Routed through [`spawn_blocking_napi`] because the comparison
/// loads both snapshots from disk and walks the entity tables to
/// classify each entity as added/removed/modified — work that grows
/// linearly with project size.
#[napi]
pub async fn deliver_compare_revisions(
    project_path: String,
    base_id: String,
    head_id: String,
) -> Result<RevisionDiffJs> {
    spawn_blocking_napi(move || {
        let diff = with_service_ref_fallible(|svc| {
            svc.deliver_compare_revisions(&project_path, &base_id, &head_id)
        })?;
        let diff_json = serde_json::to_string(&diff).map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("deliver_compare_revisions: serialize: {e}"),
            )
        })?;
        Ok(RevisionDiffJs { diff_json })
    })
    .await
}

// ===== KChat (Phase 12) =====

/// Renderer-shaped KChat connection status. Mirrors
/// [`crate::kchat_state::KChatStatusReport`].
#[napi(object)]
pub struct KChatStatusJs {
    pub state: String,
    pub publisher_kind: String,
    pub instance_json: Option<String>,
    /// Per-project [`KChatConfig::default_thread_id`][cfg] (or `None`
    /// when no project is open / the project omitted the field).
    /// Surfaces through the same status payload the renderer
    /// already polls every 5 s; the Deliver page's review panel
    /// reads it directly and falls back to the publisher-side
    /// default constant only when this is `None`.
    ///
    /// [cfg]: aec_core::kchat_config::KChatConfig
    pub default_thread_id: Option<String>,
    /// Master enable switch mirroring
    /// [`aec_core::kchat_config::KChatConfig::enabled`]. The
    /// Electron `kchat:publish` IPC handler reads this through the
    /// status payload *before* enqueueing into the loopback HTTP
    /// queue and refuses the publish when `false`. Exposed via the
    /// same payload the renderer already polls so the Settings
    /// toggle and the publish-gate stay in lockstep without a
    /// per-call bridge round trip.
    pub enabled: bool,
}

#[napi(object)]
pub struct KChatPublishParamsJs {
    /// Serialised [`aec_core::kchat::ArtifactCard`] (JSON). The
    /// renderer assembles this from form values and sends the
    /// stringified JSON across the bridge.
    pub card_json: String,
}

#[napi(object)]
pub struct KChatPublishResultJs {
    pub message_id: String,
    pub thread_id: String,
    pub published_at: String,
}

#[napi(object)]
pub struct KChatIngestParamsJs {
    pub thread_id: String,
    pub since_iso: Option<String>,
}

#[napi(object)]
pub struct KChatIngestResultJs {
    pub thread_id: String,
    /// JSON-encoded `Vec<ReviewComment>`.
    pub comments_json: String,
    /// JSON-encoded `Vec<ReviewCard>`.
    pub cards_json: String,
}

fn kchat_status_to_js(rep: crate::kchat_state::KChatStatusReport) -> KChatStatusJs {
    // Phase 15: the loopback-API snapshot (port, port-file path,
    // extension heartbeat, queue depth) is owned by the Electron
    // main process, not by the Rust bridge — so `instance` on the
    // Rust side is always `None`. The Electron `kchat:status`
    // IPC handler synthesises the loopback snapshot directly from
    // `kchatAppState.ts` and bypasses this napi export for the
    // production path. We still serialise any payload that might
    // be there (future Rust-side loopback client) for forward
    // compatibility.
    let instance_json = rep
        .instance
        .as_ref()
        .map(|info| serde_json::to_string(info).unwrap_or_default());
    KChatStatusJs {
        state: rep.state,
        publisher_kind: rep.publisher_kind,
        instance_json,
        default_thread_id: rep.default_thread_id,
        enabled: rep.enabled,
    }
}

#[napi]
pub fn kchat_status() -> Result<KChatStatusJs> {
    let rep = with_service_ref(super::service::BridgeService::kchat_status)?;
    Ok(kchat_status_to_js(rep))
}

#[napi]
pub fn kchat_reload() -> Result<KChatStatusJs> {
    let rep = with_service_ref(super::service::BridgeService::kchat_reload)?;
    Ok(kchat_status_to_js(rep))
}

/// Flip the master KChat enable switch. When `enabled` is `false`,
/// subsequent `kchat_publish` / `kchat_ingest_reviews` calls refuse
/// to touch the publisher (`KChatError::Disabled`). Mirrored onto
/// the status payload so the Electron-side `kchat:publish` gate and
/// the renderer's Settings card observe the same flag without a
/// per-call bridge round trip.
#[napi]
pub fn kchat_set_enabled(enabled: bool) -> Result<KChatStatusJs> {
    let rep = with_service_ref(|svc| {
        svc.kchat_set_enabled(enabled);
        svc.kchat_status()
    })?;
    Ok(kchat_status_to_js(rep))
}

/// Read the current value of the master KChat enable switch. Cheap
/// read-lock on the bridge-side state; exposed separately so the
/// renderer's Settings card can hydrate its toggle without parsing
/// the full status payload.
#[napi]
pub fn kchat_is_enabled() -> Result<bool> {
    with_service_ref(super::service::BridgeService::kchat_is_enabled)
}

/// Phase 15 — Electron host signals that the loopback API has bound
/// on `127.0.0.1`. Promotes the Rust-side `publisher_kind` marker
/// to `loopback_http` so any future Rust-side consumer (telemetry,
/// audit, the in-process journey tests) sees the same kind the
/// Electron `kchat:status` IPC reports to the renderer. Idempotent;
/// safe to call on every Electron startup.
#[napi]
pub fn kchat_mark_loopback_active() -> Result<KChatStatusJs> {
    let rep = with_service_ref(super::service::BridgeService::kchat_mark_loopback_active)?;
    Ok(kchat_status_to_js(rep))
}

/// Phase 15 — Electron host signals that the loopback API is being
/// torn down (typically during `app.on("will-quit", ...)`). Demotes
/// the marker back to `in_memory` so the next snapshot is honest
/// about the headless state. Idempotent.
#[napi]
pub fn kchat_mark_loopback_inactive() -> Result<KChatStatusJs> {
    let rep = with_service_ref(super::service::BridgeService::kchat_mark_loopback_inactive)?;
    Ok(kchat_status_to_js(rep))
}

#[napi]
pub fn kchat_publish(params: KChatPublishParamsJs) -> Result<KChatPublishResultJs> {
    let card: aec_core::kchat::ArtifactCard = serde_json::from_str(&params.card_json)
        .map_err(|e| Error::new(Status::GenericFailure, format!("kchat_publish parse: {e}")))?;
    let result = with_service_ref_fallible(|svc| svc.kchat_publish(card))?;
    Ok(KChatPublishResultJs {
        message_id: result.message_id,
        thread_id: result.thread_id,
        published_at: result.published_at.to_rfc3339(),
    })
}

#[napi]
pub fn kchat_ingest_reviews(params: KChatIngestParamsJs) -> Result<KChatIngestResultJs> {
    let report = with_service_ref_fallible(|svc| {
        svc.kchat_ingest_reviews(&params.thread_id, params.since_iso.clone())
    })?;
    let comments_json = serde_json::to_string(&report.comments).map_err(|e| {
        Error::new(
            Status::GenericFailure,
            format!("kchat_ingest_reviews serialize: {e}"),
        )
    })?;
    let cards_json = serde_json::to_string(&report.cards).map_err(|e| {
        Error::new(
            Status::GenericFailure,
            format!("kchat_ingest_reviews serialize cards: {e}"),
        )
    })?;
    Ok(KChatIngestResultJs {
        thread_id: report.thread_id,
        comments_json,
        cards_json,
    })
}

// ----- Viewport (Phase 12) ---------------------------------
//
// The viewport methods on `BridgeService` route through
// [`crate::viewport_service::ViewportService`], which owns its own
// wgpu device and pipelines. The N-API surface is intentionally
// flat: each call serializes the result through JSON (rather than
// returning a heavily-typed struct) because the renderer's
// `bridge.ts` ultimately re-parses these into TypeScript domain
// types, and a `string` payload is easier to evolve than a
// generated `#[napi(object)]` struct.

#[napi(object)]
pub struct ViewportStatusJs {
    pub state: String,
    pub width: u32,
    pub height: u32,
    pub frame_index: f64,
    /// JSON-encoded `Option<GpuDescriptor>`. `null` when no adapter.
    pub gpu_descriptor_json: Option<String>,
}

fn viewport_status_to_js(rep: crate::viewport_service::ViewportStatusReport) -> ViewportStatusJs {
    let gpu_descriptor_json = rep
        .gpu_descriptor
        .as_ref()
        .map(|d| serde_json::to_string(d).unwrap_or_default());
    ViewportStatusJs {
        state: rep.state,
        width: rep.width,
        height: rep.height,
        frame_index: rep.frame_index as f64,
        gpu_descriptor_json,
    }
}

#[napi(object)]
pub struct ViewportResizeParamsJs {
    pub width: u32,
    pub height: u32,
}

#[napi]
pub fn viewport_resize(params: ViewportResizeParamsJs) -> Result<ViewportStatusJs> {
    let report = with_service_ref_fallible(|svc| svc.viewport_resize(params.width, params.height))?;
    Ok(viewport_status_to_js(report))
}

#[napi(object)]
pub struct ViewportInputParamsJs {
    /// One of `"orbit"`, `"pan"`, `"zoom"`, `"reset"`.
    pub kind: String,
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    pub delta: Option<f64>,
}

#[napi(object)]
pub struct ViewportCameraJs {
    /// JSON-encoded
    /// [`crate::viewport_service::ViewportCameraReport`].
    pub camera_json: String,
}

#[napi]
pub fn viewport_input(params: ViewportInputParamsJs) -> Result<ViewportCameraJs> {
    let input = parse_viewport_input(&params)
        .map_err(|e| Error::new(Status::GenericFailure, format!("viewport_input parse: {e}")))?;
    let cam = with_service_ref_fallible(|svc| svc.viewport_input(input))?;
    let camera_json = serde_json::to_string(&cam)
        .map_err(|e| Error::new(Status::GenericFailure, format!("camera serialize: {e}")))?;
    Ok(ViewportCameraJs { camera_json })
}

fn parse_viewport_input(
    p: &ViewportInputParamsJs,
) -> std::result::Result<crate::viewport_service::ViewportInput, String> {
    use crate::viewport_service::ViewportInput;
    match p.kind.as_str() {
        "orbit" => Ok(ViewportInput::Orbit {
            dx: p.dx.unwrap_or(0.0) as f32,
            dy: p.dy.unwrap_or(0.0) as f32,
        }),
        "pan" => Ok(ViewportInput::Pan {
            dx: p.dx.unwrap_or(0.0) as f32,
            dy: p.dy.unwrap_or(0.0) as f32,
        }),
        "zoom" => Ok(ViewportInput::Zoom {
            delta: p.delta.unwrap_or(0.0) as f32,
        }),
        "reset" => Ok(ViewportInput::Reset),
        other => Err(format!("unknown input kind '{other}'")),
    }
}

#[napi(object)]
pub struct ViewportFrameJs {
    pub frame_index: f64,
    pub width: u32,
    pub height: u32,
    pub state: String,
    pub camera_json: String,
}

#[napi]
pub fn viewport_request_frame() -> Result<ViewportFrameJs> {
    let report = with_service_ref_fallible(super::service::BridgeService::viewport_request_frame)?;
    let camera_json = serde_json::to_string(&report.camera)
        .map_err(|e| Error::new(Status::GenericFailure, format!("camera serialize: {e}")))?;
    Ok(ViewportFrameJs {
        frame_index: report.frame_index as f64,
        width: report.width,
        height: report.height,
        state: report.state,
        camera_json,
    })
}

#[napi]
pub fn viewport_status() -> Result<ViewportStatusJs> {
    let rep = with_service_ref(super::service::BridgeService::viewport_status)?;
    Ok(viewport_status_to_js(rep))
}

/// JS-facing viewport frame buffer payload. Phase 17 Group D Task 21.
///
/// The `bytes` field wraps the Rust `Vec<u8>` via `Buffer::from_data`
/// so V8 owns a zero-copy view into the allocation — no per-frame
/// memcpy of the RGBA pixels across the JS boundary. The buffer is
/// freed when V8 garbage-collects the wrapper, so the renderer can
/// keep the bytes around for as long as its canvas-paint loop needs
/// without an explicit handle to drop.
///
/// `frame_index` is monotonic across calls for a given viewport size
/// and increments only when the camera state changes (frame
/// coalescing). The renderer's rAF loop compares the value to its
/// last painted index and skips the `putImageData` when they match.
#[napi(object)]
pub struct ViewportFrameBufferJs {
    pub bytes: napi::bindgen_prelude::Buffer,
    pub width: u32,
    pub height: u32,
    pub frame_index: f64,
}

/// Read the current viewport's CPU-side RGBA8 frame buffer. Returns
/// `None` when the viewport has not been resized yet (the bridge
/// treats the "no surface" case as "show the unavailable overlay").
#[napi]
pub fn viewport_read_frame_buffer() -> Result<Option<ViewportFrameBufferJs>> {
    let opt = with_service_ref(super::service::BridgeService::viewport_read_frame_buffer)?;
    Ok(opt.map(|fb| ViewportFrameBufferJs {
        width: fb.width,
        height: fb.height,
        frame_index: fb.frame_index as f64,
        bytes: fb.pixels.into(),
    }))
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

#[cfg(test)]
mod chain_verification_js_tests {
    //! Locks the conversion from `aec_audit::ChainVerification` to
    //! `ChainVerificationJs` so that every `BreakReason` variant maps to
    //! a stable, documented `break_reason` string. The Rust enum and the
    //! TypeScript `AuditChainVerification.breakReason` union must stay
    //! in lock-step — if a new variant is added in `aec_audit`, this
    //! test (plus the non-exhaustive-match compile error) is the gate
    //! that forces the bridge layer and `apps/desktop/electron/bridge.ts`
    //! to be updated together.
    use super::*;
    use aec_audit::{BreakReason, ChainStatus, ChainVerification};
    use std::path::PathBuf;

    fn convert(status: ChainStatus) -> ChainVerificationJs {
        let v = ChainVerification {
            status,
            entries_checked: 0,
            entries_legacy_linkage_only: 0,
            files_checked: vec![PathBuf::from("audit/test.jsonl")],
            head_hash: "blake3:genesis".to_string(),
        };
        v.into()
    }

    #[test]
    fn ok_status_clears_break_fields() {
        let js = convert(ChainStatus::Ok);
        assert_eq!(js.status, "ok");
        assert!(js.break_reason.is_none());
        assert!(js.break_detail.is_none());
        assert!(js.break_file.is_none());
        assert!(js.break_line.is_none());
    }

    #[test]
    fn prev_hash_mismatch_maps_to_stable_string() {
        let js = convert(ChainStatus::BrokenAt {
            file: PathBuf::from("audit/test.jsonl"),
            line: 7,
            reason: BreakReason::PrevHashMismatch {
                expected: "blake3:e".to_string(),
                found: "blake3:f".to_string(),
            },
        });
        assert_eq!(js.status, "broken_at");
        assert_eq!(js.break_reason.as_deref(), Some("prev_hash_mismatch"));
        assert!(js.break_detail.unwrap().contains("blake3:e"));
        assert_eq!(js.break_line, Some(7));
    }

    #[test]
    fn hash_recompute_mismatch_maps_to_stable_string() {
        let js = convert(ChainStatus::BrokenAt {
            file: PathBuf::from("audit/test.jsonl"),
            line: 1,
            reason: BreakReason::HashRecomputeMismatch {
                stored: "blake3:a".to_string(),
                recomputed: "blake3:b".to_string(),
            },
        });
        assert_eq!(js.break_reason.as_deref(), Some("hash_recompute_mismatch"));
    }

    #[test]
    fn unsupported_hash_version_maps_to_stable_string() {
        let js = convert(ChainStatus::BrokenAt {
            file: PathBuf::from("audit/test.jsonl"),
            line: 1,
            reason: BreakReason::UnsupportedHashVersion {
                version: 99,
                supported: vec![1, 2],
            },
        });
        assert_eq!(js.break_reason.as_deref(), Some("unsupported_hash_version"));
        let detail = js.break_detail.unwrap();
        assert!(detail.contains("99"));
        assert!(detail.contains("[1, 2]"));
    }

    #[test]
    fn legacy_hash_version_rejected_maps_to_stable_string() {
        // The strict_v2_only verify mode emits this variant when a v1
        // entry is found. The bridge must translate it to the
        // documented `"legacy_hash_version_rejected"` string so the
        // renderer can pattern-match without runtime surprise.
        let js = convert(ChainStatus::BrokenAt {
            file: PathBuf::from("audit/legacy.jsonl"),
            line: 3,
            reason: BreakReason::LegacyHashVersionRejected {
                version: 1,
                required_min: 2,
            },
        });
        assert_eq!(js.status, "broken_at");
        assert_eq!(
            js.break_reason.as_deref(),
            Some("legacy_hash_version_rejected")
        );
        let detail = js.break_detail.unwrap();
        assert!(detail.contains("hash_version = 1"));
        assert!(detail.contains("required minimum = 2"));
        assert_eq!(js.break_line, Some(3));
        assert_eq!(js.break_file.as_deref(), Some("audit/legacy.jsonl"));
    }

    #[test]
    fn malformed_entry_maps_to_stable_string() {
        let js = convert(ChainStatus::BrokenAt {
            file: PathBuf::from("audit/test.jsonl"),
            line: 1,
            reason: BreakReason::MalformedEntry {
                message: "expected `}`".to_string(),
            },
        });
        assert_eq!(js.break_reason.as_deref(), Some("malformed_entry"));
        assert_eq!(js.break_detail.as_deref(), Some("expected `}`"));
    }

    #[test]
    fn io_error_maps_to_stable_string() {
        let js = convert(ChainStatus::BrokenAt {
            file: PathBuf::from("audit/test.jsonl"),
            line: 1,
            reason: BreakReason::Io {
                message: "permission denied".to_string(),
            },
        });
        assert_eq!(js.break_reason.as_deref(), Some("io"));
        assert_eq!(js.break_detail.as_deref(), Some("permission denied"));
    }
}
