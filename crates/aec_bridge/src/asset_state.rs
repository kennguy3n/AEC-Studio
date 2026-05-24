//! Lazy-initialised handle to the global asset library DB.
//!
//! ## Why a separate module?
//!
//! The asset library is a *cross-project* resource. Unlike per-project
//! `assets/` directories (which the asset import pipeline writes into
//! each project package), the **library** is the catalogue of all
//! authored / installed / vendor-shipped assets the user has available
//! to drop into any project. It lives at a single well-known path
//! under the bridge's [`crate::service::BridgeConfig::state_dir`] so
//! every project the user opens sees the same library.
//!
//! Three properties matter:
//!
//! * **Singleton, lazy-init**: the user can launch AEC Studio without
//!   touching the asset browser (they might open a project and head
//!   straight to render or BIM). We don't want to pay SQLite-open +
//!   schema-create + seed cost on every bridge construction. So the
//!   DB handle is `Mutex<Option<AssetDatabase>>` and is created on
//!   first call to `with_db_mut` / `with_db`.
//!
//! * **Interior mutability**: every `BridgeService` method that hits
//!   the asset library takes `&self` (the napi side routes through
//!   `with_service_ref_fallible` — read-side of the outer `RwLock`).
//!   The DB itself takes `&mut Connection` for writes, so we serialise
//!   writes behind a `Mutex` internal to `AssetState`. Same pattern
//!   as [`crate::snapshot_cache::SnapshotCache`] and the not-yet-merged
//!   `RenderState` from PR-R.
//!
//! * **First-open seeding**: a fresh install with an empty DB would
//!   show an empty asset browser, which is a regression versus the
//!   in-process TS fallback (which seeded 4 demo assets so the UI
//!   has *something* to render). On first open we detect an empty
//!   `assets` table and seed the same 4-asset demo library that the
//!   in-process fallback exposed. The seed is also content-addressable
//!   in the sense that callers can re-issue `design_list_assets` and
//!   always get the same 4 entries on a fresh install.
//!
//! ## Threading model
//!
//! The contract for callers is:
//!
//! 1. Acquire the outer `BridgeServiceSingleton` read lock via
//!    `with_service_ref_fallible` (same as `bim_check_file_size`,
//!    `bim_export_ifc`, etc).
//! 2. Call `BridgeService::design_list_assets(...)`.
//! 3. That method calls `self.assets.with_db(...)` which takes the
//!    inner asset-state `Mutex`, then runs the query, then drops the
//!    inner lock before returning.
//!
//! The inner `Mutex` is *only* held across single DB operations
//! (query / upsert). It never wraps any net-IO or long-running
//! computation, so a `render_diagnose` or `bim_import_ifc` call
//! cannot block the asset browser even on slow hardware.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aec_assets::{
    AssetDatabase, AssetError, AssetMetadata, License, LodChain, MeshBlob, ThumbnailKind, Vendor,
};

/// Filename for the global asset library DB under
/// [`crate::service::BridgeConfig::state_dir`]. Sits under an
/// `asset_library/` subdirectory so the future addition of LOD blob
/// files (`blobs/<hash>.bin`) and per-asset thumbnails
/// (`thumbs/<hash>.png`) has a natural home next to the metadata
/// file.
const ASSET_LIBRARY_SUBDIR: &str = "asset_library";
const ASSET_LIBRARY_FILENAME: &str = "assets.sqlite";

/// Lazy-init handle to the global asset library DB.
///
/// Public visibility is `pub(crate)` — only `BridgeService` constructs
/// or borrows from this struct; renderer code reaches the library
/// through `BridgeService::design_list_assets` and never holds the DB
/// handle directly.
pub(crate) struct AssetState {
    /// Path the DB lives at. Captured at construction so the lazy-init
    /// path inside `with_db_mut` doesn't need to re-derive it.
    db_path: PathBuf,
    /// `None` until the first `with_db` / `with_db_mut` call opens
    /// (or creates) the DB on disk. Wrapped in a `Mutex` so multiple
    /// `&self` callers can race the first init without UB; the inner
    /// `AssetDatabase::open` is idempotent (it's `CREATE IF NOT EXISTS`
    /// schema bootstrap) so even if both sides of a race observe
    /// `None`, the second open simply re-attaches to the same on-disk
    /// file.
    db: Mutex<Option<AssetDatabase>>,
}

impl AssetState {
    /// Construct an `AssetState` rooted at
    /// `<state_dir>/asset_library/assets.sqlite`. Does *not* open the
    /// DB — that happens on first `with_db` / `with_db_mut`.
    pub(crate) fn new(state_dir: &Path) -> Self {
        let db_path = state_dir
            .join(ASSET_LIBRARY_SUBDIR)
            .join(ASSET_LIBRARY_FILENAME);
        Self {
            db_path,
            db: Mutex::new(None),
        }
    }

    /// Run `f` against a read-only handle on the asset DB. If the DB
    /// has not yet been opened, opens it (creating the on-disk file
    /// and schema if absent) and seeds it with the 4-asset demo
    /// library the in-process TS fallback used to ship.
    pub(crate) fn with_db<R>(
        &self,
        f: impl FnOnce(&AssetDatabase) -> Result<R, AssetError>,
    ) -> Result<R, AssetError> {
        let mut guard = self.db.lock().expect("asset state mutex poisoned");
        if guard.is_none() {
            let db = open_and_seed(&self.db_path)?;
            *guard = Some(db);
        }
        let db = guard.as_ref().expect("asset DB was just initialised above");
        f(db)
    }

    /// Like [`Self::with_db`] but exposes a `&mut AssetDatabase` so the
    /// caller can `upsert_metadata` / `put_blob` etc. Used by tests
    /// (and, in the future, the asset import pipeline once it's wired
    /// to the napi surface).
    #[allow(dead_code)] // wired in a follow-up PR (asset import pipeline)
    pub(crate) fn with_db_mut<R>(
        &self,
        f: impl FnOnce(&mut AssetDatabase) -> Result<R, AssetError>,
    ) -> Result<R, AssetError> {
        let mut guard = self.db.lock().expect("asset state mutex poisoned");
        if guard.is_none() {
            let db = open_and_seed(&self.db_path)?;
            *guard = Some(db);
        }
        let db = guard.as_mut().expect("asset DB was just initialised above");
        f(db)
    }

    /// Path the DB lives at. Used by tests to inspect / wipe between
    /// runs.
    #[cfg(test)]
    pub(crate) fn db_path(&self) -> &Path {
        &self.db_path
    }
}

/// Open the DB at `path` (creating the parent dir + schema if absent)
/// and seed the 4-asset demo library on the first open of an empty
/// table. The seed only runs when `SELECT count(*) FROM assets = 0`,
/// so subsequent opens of the same on-disk file are idempotent.
fn open_and_seed(path: &Path) -> Result<AssetDatabase, AssetError> {
    let mut db = AssetDatabase::open(path)?;
    // `AssetQuery::default()` returns every asset up to the default
    // limit (200). A fresh DB has zero rows so the empty check is
    // cheap; on an installed library with thousands of assets the
    // query is also bounded by the LIMIT clause inside
    // `AssetDatabase::query`.
    let existing = db.query(&aec_assets::AssetQuery::default())?;
    if existing.is_empty() {
        seed_demo_assets(&mut db)?;
    }
    Ok(db)
}

/// Insert the 4 demo assets that the in-process TS fallback used to
/// expose. These are catalogue entries only (no mesh blobs / no
/// thumbnail blobs) so the asset browser can render the cards even
/// before the user installs a real asset pack.
///
/// The exact ids / names / tags / style-tags / vendor names match the
/// in-process fallback's `seedAssets()` in `apps/desktop/electron/bridge.ts`
/// so a side-by-side comparison between dev (in-process) and prod
/// (native) builds shows identical asset cards.
fn seed_demo_assets(db: &mut AssetDatabase) -> Result<(), AssetError> {
    for (asset_id, name, vendor_id, vendor_name, tags, style_tags) in DEMO_ASSETS {
        let vendor = Vendor {
            id: (*vendor_id).to_string(),
            name: (*vendor_name).to_string(),
            url: None,
        };
        let mut meta = AssetMetadata::new(*asset_id, *name, vendor, "demo-1", License::Custom);
        meta.tags = tags.iter().map(|s| (*s).to_string()).collect();
        meta.style_tags = style_tags.iter().map(|s| (*s).to_string()).collect();
        // No real mesh blob — the demo library is catalogue-only.
        // `LodChain::from_ratios(0, &[])` uses the documented default
        // ratio chain `[1.0, 0.5, 0.25]`, but with a `base_triangles`
        // of 0 the per-level triangle counts all clamp to `1`
        // (`(0.0 * ratio).round().max(1.0)` in
        // `aec_assets/src/lod.rs`). That's fine for a catalogue-only
        // seed: the 3-level chain keeps the schema's `asset_lods` rows
        // + the metadata's `lods` vec aligned so `upsert_metadata`
        // doesn't trip its "LOD level missing from `lods`" guard, and
        // each LOD points at the same `demo-placeholder` hash because
        // there's no real mesh blob to disambiguate by. A real asset
        // import would hand `LodChain` + `Vec<MeshBlob>` from the
        // simplification pipeline so the triangle counts reflect the
        // actual mesh.
        let chain = LodChain::from_ratios(0, &[]);
        meta.lods = chain
            .levels
            .iter()
            .map(|_| MeshBlob {
                mesh_hash: "demo-placeholder".to_string(),
                vertex_count: 0,
                triangle_count: 0,
            })
            .collect();
        meta.thumbnail_kind = ThumbnailKind::Placeholder;
        meta.thumbnail_hash = "demo-placeholder".to_string();
        db.upsert_metadata(&meta, &chain)?;
    }
    Ok(())
}

/// The 4 demo assets, copied verbatim from
/// `apps/desktop/electron/bridge.ts`'s `seedAssets()` so dev (in-process
/// TS fallback) and prod (native AssetDatabase) builds render the
/// same asset cards.
const DEMO_ASSETS: &[(&str, &str, &str, &str, &[&str], &[&str])] = &[
    (
        "ikea.sofa_kivik_3s",
        "Kivik 3-seat Sofa",
        "ikea",
        "IKEA",
        &["furniture", "sofa", "living"],
        &["scandinavian", "modern"],
    ),
    (
        "muuto.armchair_outline",
        "Outline Armchair",
        "muuto",
        "Muuto",
        &["furniture", "armchair", "living"],
        &["scandinavian", "japandi"],
    ),
    (
        "vendor.cafe_chair_thonet",
        "Bentwood Cafe Chair",
        "thonet",
        "Thonet",
        &["furniture", "chair", "cafe"],
        &["industrial", "classic"],
    ),
    (
        "vendor.cafe_table_700",
        "Cafe Table 700",
        "atelier",
        "Atelier",
        &["furniture", "table", "cafe"],
        &["industrial"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_state() -> (TempDir, AssetState) {
        let tmp = TempDir::new().expect("temp dir");
        let state = AssetState::new(tmp.path());
        (tmp, state)
    }

    #[test]
    fn first_open_seeds_4_demo_assets() {
        let (_tmp, state) = fresh_state();
        let assets = state
            .with_db(|db| db.query(&aec_assets::AssetQuery::default()))
            .expect("query succeeds on first open");
        assert_eq!(assets.len(), 4);
        let ids: Vec<&str> = assets.iter().map(|a| a.asset_id.as_str()).collect();
        assert!(ids.contains(&"ikea.sofa_kivik_3s"));
        assert!(ids.contains(&"muuto.armchair_outline"));
        assert!(ids.contains(&"vendor.cafe_chair_thonet"));
        assert!(ids.contains(&"vendor.cafe_table_700"));
    }

    #[test]
    fn second_open_does_not_re_seed() {
        // Open + close + re-open the same on-disk DB; the seed must
        // not duplicate rows. This pins the idempotence contract on
        // `open_and_seed`.
        let tmp = TempDir::new().expect("temp dir");
        {
            let state = AssetState::new(tmp.path());
            state
                .with_db(|db| db.query(&aec_assets::AssetQuery::default()))
                .expect("seed");
        }
        let state2 = AssetState::new(tmp.path());
        let assets = state2
            .with_db(|db| db.query(&aec_assets::AssetQuery::default()))
            .expect("query");
        assert_eq!(assets.len(), 4, "re-open must not duplicate seed rows");
    }

    #[test]
    fn lazy_init_does_not_open_db_until_first_call() {
        // Construction alone must not touch disk — verify by pointing
        // `AssetState` at a non-existent state_dir and asserting that
        // the DB file does not appear until a method is called.
        let tmp = TempDir::new().expect("temp dir");
        let state = AssetState::new(&tmp.path().join("not_yet"));
        assert!(
            !state.db_path().exists(),
            "construction must not create the on-disk DB"
        );
        state
            .with_db(|db| db.query(&aec_assets::AssetQuery::default()))
            .expect("first call opens DB");
        assert!(state.db_path().exists(), "first call must create the DB");
    }

    #[test]
    fn demo_seed_ids_match_in_process_fallback_seed() {
        // Pin the contract that the Rust seed list matches the
        // in-process TS fallback's `seedAssets()` so dev/prod
        // browsing shows identical cards. Drift here is a UX bug
        // (renderer's `bridge-catalogue.test.ts` covers method
        // disjointness but not seed-data parity).
        let expected = [
            "ikea.sofa_kivik_3s",
            "muuto.armchair_outline",
            "vendor.cafe_chair_thonet",
            "vendor.cafe_table_700",
        ];
        let actual: Vec<&str> = DEMO_ASSETS.iter().map(|t| t.0).collect();
        for id in expected {
            assert!(actual.contains(&id), "missing demo asset id: {id}");
        }
    }
}
