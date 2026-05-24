//! Plain-Rust service layer for the bridge.
//!
//! Everything the Electron renderer can do ultimately calls into one of
//! these methods. Keeping the napi wrappers thin and the logic here makes
//! this layer trivially testable.

use std::path::{Path, PathBuf};

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

use crate::engine_status_cache::EngineStatusCache;
use crate::recents::{RecentsStore, RecentsStoreError};

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
        if let Ok(key) = Self::cache_key(path) {
            self.engine_status_cache.invalidate(&key);
        }
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
        // connection's view of `schema_version` is now stale. Invalidate
        // so the next status read sees the post-save state.
        if let Ok(key) = Self::cache_key(path) {
            self.engine_status_cache.invalidate(&key);
        }
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
        // up the synced rows immediately.
        if let Ok(key) = Self::cache_key(path) {
            self.engine_status_cache.invalidate(&key);
        }
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
}
