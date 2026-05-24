//! Plain-Rust service layer for the bridge.
//!
//! Everything the Electron renderer can do ultimately calls into one of
//! these methods. Keeping the napi wrappers thin and the logic here makes
//! this layer trivially testable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_audit::AuditLog;
use aec_core::config::ProjectSettings;
use aec_core::package::{ProjectPackage, ProjectSummary as CoreProjectSummary};
use aec_core::templates::TemplateLoader;
use aec_core::types::{ProjectId, Scope};
use aec_governor::profiler::{CpuProfile, GpuProfile, HardwareProfiler};
use aec_governor::tier::HardwareTier;

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
}

impl BridgeService {
    pub fn new(config: BridgeConfig, master_key: [u8; 32]) -> Result<Self, BridgeServiceError> {
        std::fs::create_dir_all(&config.state_dir)?;
        std::fs::create_dir_all(&config.projects_dir)?;
        let recents =
            RecentsStore::open(config.state_dir.join("recents.json"), config.max_recents)?;
        Ok(Self {
            config,
            recents,
            master_key,
            engine_status_cache: EngineStatusCache::new(),
            snapshot_cache: SnapshotCache::new(),
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
        let canonical_ifc_buf = std::fs::canonicalize(Path::new(ifc_path))?;
        let canonical_ifc = canonical_ifc_buf.to_string_lossy().into_owned();

        // Snapshot cache: try a hit first; on miss, read + parse the
        // file and populate the cache for any next attach.
        let key_opt = SnapshotKey::from_canonical_path(&canonical_ifc_buf).ok();
        let (snapshot_arc, parse_cache_hit) =
            if let Some(snap) = key_opt.as_ref().and_then(|k| self.snapshot_cache.get(k)) {
                (snap, true)
            } else {
                // Cache miss (or no cache key available) — read and
                // parse the file ourselves. We still populate the cache
                // on a successful parse if we have a key, so the next
                // attach for the same `(path, mtime, size)` is fast.
                let bytes = std::fs::read(&canonical_ifc_buf)?;
                let body = String::from_utf8_lossy(&bytes).into_owned();
                let parsed = aec_bim::ifc::IfcReader::from_string(&body)?;
                let arc = Arc::new(parsed);
                if let Some(key) = key_opt {
                    self.snapshot_cache.insert(key, Arc::clone(&arc));
                }
                (arc, false)
            };

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
}
