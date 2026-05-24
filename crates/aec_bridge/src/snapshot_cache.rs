//! LRU snapshot cache for `bim_import_ifc` → `bim_attach_ifc`.
//!
//! ## Why a cache?
//!
//! The renderer-side BIM flow is two-phase: the user first
//! `bim_import_ifc`s a file to see a preview (counts, schema banner,
//! material/Pset summary), then optionally `bim_attach_ifc`s the same
//! file to fold its spatial graph + materials + Psets into the active
//! project's authoring database. Without a cache, the file would be
//! parsed twice on the happy path — once for the preview and again for
//! the attach. Real-world federated IFCs run 50–500 MB (MEP, structural,
//! Revit-room separators baked into the architectural model), and the
//! parser at [`aec_bim::ifc::IfcReader::from_string`] is intentionally
//! linear in token count, so the second parse would block the renderer
//! thread for the same several seconds the first parse already cost.
//!
//! Caching the parsed [`aec_bim::ifc::IfcSnapshot`] across the preview
//! → attach handoff means the second call is a single `HashMap` lookup
//! plus an `Arc` clone — microseconds rather than seconds.
//!
//! ## Cache identity (key)
//!
//! The cache key is a triple of `(canonical_path, mtime, size)`:
//!
//! * **Canonical path** — the same form produced by
//!   [`std::fs::canonicalize`] and stored in
//!   [`crate::service::BimImportSummary::path`]. This means two distinct
//!   user-supplied paths that resolve to the same file (relative vs
//!   absolute, `..`-segments, symlinks) share a cache entry.
//! * **Modification time** — guards against the file being overwritten
//!   between import and attach. If the user re-exports their IFC from
//!   Revit between the preview and the attach, `mtime` will have moved
//!   and the next [`SnapshotCache::get`] correctly misses, forcing a
//!   re-parse.
//! * **Size** — defence-in-depth against the rare filesystem that
//!   doesn't bump `mtime` on every write (some network shares, some
//!   atomic-write semantics on macOS APFS). A file that changed size
//!   definitely changed content.
//!
//! All three values are captured once at cache-write time and pinned
//! into the key — the cache does not silently re-stat on read, so a hit
//! always reflects the file state at the moment of the import that
//! populated the entry.
//!
//! ## Capacity + TTL invalidation
//!
//! * **Capacity** ([`SNAPSHOT_CACHE_CAPACITY`] = 4). Each cached snapshot
//!   holds the parsed IFC in memory (peak ~5–10× the wire size for a
//!   structural model — `Vec<EntityRecord>` indirection, `BTreeMap`
//!   children, `HashMap` step-id indexes inside each Pset / Qto). 4 is
//!   the working-set size for the typical AEC user who toggles between
//!   the architectural model, an MEP federation, a structural model,
//!   and one reference file from a consultant. Above 4, LRU eviction
//!   reclaims the least-recently-used entry.
//! * **TTL** ([`SNAPSHOT_CACHE_TTL`] = 60 s). A two-minute Slack window
//!   between preview and attach is well above the usual interaction
//!   pattern (the user clicks Attach a few seconds after the preview
//!   pane shows the counts). 60 s gives them a generous read of the
//!   preview without retaining stale parse-state once they walk away.
//!   Idle eviction happens on every [`SnapshotCache::get`] /
//!   [`SnapshotCache::insert`] call so an idle process eventually drops
//!   memory back without needing a sweeper thread.
//!
//! ## Concurrency model
//!
//! Mirrors [`crate::engine_status_cache::EngineStatusCache`]:
//!
//! * Single [`Mutex`] on the entries map. Lookups are O(1) average and
//!   the lock is held only for the lookup itself — never while parsing
//!   (which would hold the lock for seconds and block every other
//!   bridge call). Parses run *outside* the cache lock at the
//!   `BridgeService` layer, and only the resulting `Arc<IfcSnapshot>`
//!   is moved back into the cache.
//! * Each cache entry stores an [`Arc<IfcSnapshot>`] so a hit just
//!   bumps the reference count — no clone of the underlying graph.
//!   Multiple readers can hold their own `Arc` clone concurrently while
//!   the cache lock is dropped.
//!
//! ## Invalidation contract
//!
//! * `bim_import_ifc` calls [`SnapshotCache::insert`] on the canonical
//!   path of the just-parsed file. This populates the cache so the
//!   matching `bim_attach_ifc` is a hit.
//! * `bim_attach_ifc` calls [`SnapshotCache::get`] for the same
//!   `(canonical_path, mtime, size)` key. On hit it consumes the
//!   `Arc<IfcSnapshot>` directly; on miss it re-parses and re-populates.
//! * No "write" endpoint mutates the source IFC file — the cache is
//!   read-only against external state. There is therefore no
//!   "post-write invalidation" path analogous to the engine-status
//!   cache: a file changed under the cache will simply produce a new
//!   `(mtime, size)` triple, miss, and re-populate at the new key. The
//!   old key sits idle until TTL eviction.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use aec_bim::ifc::IfcSnapshot;

/// Default time-to-live for a cached snapshot. Production callers use
/// [`SnapshotCache::new`] which wires this; tests can use
/// [`SnapshotCache::with_config`] for short TTLs.
pub(crate) const SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(60);

/// Maximum number of cached snapshots before LRU eviction kicks in.
/// See module docs for the sizing rationale.
pub(crate) const SNAPSHOT_CACHE_CAPACITY: usize = 4;

/// Composite cache key: canonical path + filesystem identity.
///
/// `mtime`/`size` are captured at insert time and pinned in the key,
/// so the cache will only hit when the next caller computes the *same*
/// triple. A file replaced on disk between import and attach produces
/// a different `mtime` and naturally misses without any extra
/// invalidation logic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SnapshotKey {
    /// Result of [`std::fs::canonicalize`] on the user-supplied path.
    /// Matches [`crate::service::BimImportSummary::path`] so the two
    /// endpoints index the same value.
    pub canonical_path: PathBuf,
    /// Filesystem modification time. `SystemTime` is `Hash` on every
    /// stdlib platform we support; we never compare it across machines
    /// so the lack of monotonicity is irrelevant.
    pub mtime: SystemTime,
    /// File size in bytes — defence-in-depth against filesystems that
    /// don't reliably bump `mtime` on overwrite.
    pub size: u64,
}

impl SnapshotKey {
    /// Compute the cache key for an already-canonicalised path. Reads
    /// the file's metadata via [`std::fs::metadata`] to capture
    /// `mtime`/`size`.
    ///
    /// Callers should pass a path that's already gone through
    /// [`std::fs::canonicalize`] — the [`crate::service::BridgeService`]
    /// canonicalises in `bim_import_ifc` and reuses that string for the
    /// cache key, ensuring the import and the matching attach agree on
    /// the canonical form even when the user passed differently-shaped
    /// strings.
    pub(crate) fn from_canonical_path(canonical_path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(canonical_path)?;
        let mtime = meta.modified()?;
        let size = meta.len();
        Ok(Self {
            canonical_path: canonical_path.to_path_buf(),
            mtime,
            size,
        })
    }
}

/// One cached snapshot plus its last-used timestamp.
///
/// `last_used` lives in its own `Mutex` so the LRU evict-on-insert path
/// can read it without taking the outer cache lock recursively. The
/// inner mutex is uncontended in practice — only the LRU scan touches
/// it and only the entries-map lock holder can run that scan.
struct CachedSnapshot {
    snapshot: Arc<IfcSnapshot>,
    last_used: Mutex<Instant>,
}

/// Process-wide LRU + TTL cache for parsed [`IfcSnapshot`]s.
///
/// Owned by [`crate::service::BridgeService`] (one instance per
/// service singleton). The cache survives across bridge endpoint calls
/// but is dropped when the service shuts down.
pub(crate) struct SnapshotCache {
    entries: Mutex<HashMap<SnapshotKey, CachedSnapshot>>,
    ttl: Duration,
    capacity: usize,
}

impl SnapshotCache {
    /// Construct a cache with production defaults
    /// ([`SNAPSHOT_CACHE_TTL`] + [`SNAPSHOT_CACHE_CAPACITY`]).
    pub(crate) fn new() -> Self {
        Self::with_config(SNAPSHOT_CACHE_TTL, SNAPSHOT_CACHE_CAPACITY)
    }

    /// Construct with explicit TTL + capacity. Used by tests to assert
    /// LRU eviction and TTL expiry without sleeping minutes.
    pub(crate) fn with_config(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            capacity,
        }
    }

    /// Return a cached snapshot for `key` if present and unexpired.
    /// Returns `None` if the entry is absent, has aged past `ttl`, or
    /// the file the key references has been replaced (the caller's
    /// freshly-computed `(mtime, size)` won't match the stored ones).
    ///
    /// Side effects: refreshes the entry's `last_used` on a hit so the
    /// LRU scan does not evict it; sweeps stale entries before
    /// answering.
    pub(crate) fn get(&self, key: &SnapshotKey) -> Option<Arc<IfcSnapshot>> {
        let mut cache = self.entries.lock().expect("snapshot cache mutex poisoned");
        self.evict_stale(&mut cache);
        let entry = cache.get(key)?;
        *entry
            .last_used
            .lock()
            .expect("snapshot cache last_used mutex poisoned") = Instant::now();
        Some(Arc::clone(&entry.snapshot))
    }

    /// Insert a parsed snapshot under `key`. If the cache is at
    /// capacity and `key` is not already present, evicts the entry
    /// with the oldest `last_used` to make room (LRU). Re-inserting an
    /// existing key replaces the entry (cheap — same memory footprint;
    /// the stored snapshot is dropped via `Arc` when no readers remain).
    pub(crate) fn insert(&self, key: SnapshotKey, snapshot: Arc<IfcSnapshot>) {
        let mut cache = self.entries.lock().expect("snapshot cache mutex poisoned");
        self.evict_stale(&mut cache);
        if !cache.contains_key(&key) && cache.len() >= self.capacity {
            // LRU eviction: drop the entry with the oldest `last_used`.
            // `min_by_key` over `Instant` is correct since `Instant`'s
            // `Ord` is monotonic. We compute the victim key while
            // holding the cache lock so no concurrent insert can race
            // us into a >capacity state.
            let victim_key: Option<SnapshotKey> = cache
                .iter()
                .min_by_key(|(_, v)| {
                    *v.last_used
                        .lock()
                        .expect("snapshot cache last_used mutex poisoned")
                })
                .map(|(k, _)| k.clone());
            if let Some(victim) = victim_key {
                cache.remove(&victim);
            }
        }
        cache.insert(
            key,
            CachedSnapshot {
                snapshot,
                last_used: Mutex::new(Instant::now()),
            },
        );
    }

    /// Drop every cached entry whose canonical path matches
    /// `canonical_path`, regardless of `mtime`/`size`. Used by the
    /// (not-yet-wired) `bim_detach_*` flow and reserved for tests; the
    /// happy path relies on TTL eviction.
    #[allow(dead_code)]
    pub(crate) fn invalidate_path(&self, canonical_path: &Path) {
        let mut cache = self.entries.lock().expect("snapshot cache mutex poisoned");
        cache.retain(|k, _| k.canonical_path != canonical_path);
    }

    /// Number of entries currently held. Exposed for the
    /// `BridgeService::__snapshot_cache_len` test accessor so
    /// integration tests can assert LRU and TTL behaviour through the
    /// public service surface.
    pub(crate) fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("snapshot cache mutex poisoned")
            .len()
    }

    /// Drop entries whose `last_used` is older than `ttl`. Called on
    /// every read/write so an idle service eventually returns to a
    /// zero-memory footprint without needing a background sweeper.
    fn evict_stale(&self, cache: &mut HashMap<SnapshotKey, CachedSnapshot>) {
        let now = Instant::now();
        let ttl = self.ttl;
        cache.retain(|_, entry| {
            let last_used = *entry
                .last_used
                .lock()
                .expect("snapshot cache last_used mutex poisoned");
            // `saturating_duration_since` mirrors the rationale in
            // `EngineStatusCache::evict_stale`: a concurrent `get` may
            // bump `last_used` to a value greater than the `now`
            // captured above; treat that as "freshly used" and keep
            // the entry rather than panicking on a hypothetical future
            // stdlib that re-introduces the panic.
            now.saturating_duration_since(last_used) < ttl
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub `IfcSnapshot` for cache tests. Builds the minimal valid
    /// snapshot via `IfcReader::from_string` against a 3-entity IFC
    /// (project + site + building) — that's the cheapest way to
    /// produce a real `IfcSnapshot` without re-implementing one in
    /// test scaffolding.
    fn stub_snapshot() -> Arc<IfcSnapshot> {
        // Mirror the IFC body the existing
        // `bim_import_ifc_returns_canonical_path` regression test
        // builds — that proves the parser accepts this exact shape
        // (3-entity minimal IFC4 with owner-history pointer + project
        // name `'P'`), so the snapshot-cache tests don't drift from
        // the parser's actual contract.
        let ifc = "ISO-10303-21;\n\
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
        Arc::new(aec_bim::ifc::IfcReader::from_string(ifc).expect("stub snapshot must parse"))
    }

    fn temp_file_with(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::tempdir().expect("tempdir");
        let p = td.path().join("model.ifc");
        std::fs::write(&p, bytes).expect("write tempfile");
        // Canonicalise so the key matches what the service would
        // produce — `tempfile::TempDir` on macOS lives under
        // `/private/var/...` once resolved.
        let canon = std::fs::canonicalize(&p).expect("canonicalize");
        (td, canon)
    }

    #[test]
    fn insert_then_get_returns_same_arc() {
        let (_td, p) = temp_file_with(b"placeholder");
        let cache = SnapshotCache::new();
        let key = SnapshotKey::from_canonical_path(&p).unwrap();
        let snap = stub_snapshot();
        cache.insert(key.clone(), Arc::clone(&snap));
        let got = cache.get(&key).expect("must hit");
        // `Arc::ptr_eq` confirms the cache hands back the *same*
        // allocation, not a deep clone.
        assert!(Arc::ptr_eq(&got, &snap));
    }

    #[test]
    fn get_miss_on_unknown_key() {
        let (_td, p) = temp_file_with(b"placeholder");
        let cache = SnapshotCache::new();
        let key = SnapshotKey::from_canonical_path(&p).unwrap();
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn key_changes_when_file_size_changes() {
        // The key is the cache's freshness oracle — if the file is
        // overwritten between import and attach with different
        // content, the new computed key MUST differ so the cache
        // misses and re-parses rather than serving stale data.
        let (td, p) = temp_file_with(b"first");
        let k1 = SnapshotKey::from_canonical_path(&p).unwrap();
        std::fs::write(&p, b"second-with-more-bytes").expect("rewrite");
        let k2 = SnapshotKey::from_canonical_path(&p).unwrap();
        assert_ne!(k1, k2, "size delta must produce a different key");
        drop(td);
    }

    #[test]
    fn ttl_evicts_expired_entry_on_next_access() {
        let (_td, p) = temp_file_with(b"placeholder");
        let cache = SnapshotCache::with_config(Duration::from_millis(1), 4);
        let key = SnapshotKey::from_canonical_path(&p).unwrap();
        cache.insert(key.clone(), stub_snapshot());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(10));
        assert!(cache.get(&key).is_none(), "1ms TTL must have expired");
        assert_eq!(cache.len(), 0, "expired entry must be evicted");
    }

    #[test]
    fn lru_evicts_oldest_on_capacity_overflow() {
        // Capacity = 2, insert three distinct keys, verify the
        // least-recently-touched one is the victim.
        let cache = SnapshotCache::with_config(Duration::from_secs(60), 2);
        let (_td_a, pa) = temp_file_with(b"a");
        let (_td_b, pb) = temp_file_with(b"b");
        let (_td_c, pc) = temp_file_with(b"c");
        let ka = SnapshotKey::from_canonical_path(&pa).unwrap();
        let kb = SnapshotKey::from_canonical_path(&pb).unwrap();
        let kc = SnapshotKey::from_canonical_path(&pc).unwrap();
        cache.insert(ka.clone(), stub_snapshot());
        std::thread::sleep(Duration::from_millis(2));
        cache.insert(kb.clone(), stub_snapshot());
        // Touch A so B becomes the LRU victim, not A.
        std::thread::sleep(Duration::from_millis(2));
        let _ = cache.get(&ka);
        std::thread::sleep(Duration::from_millis(2));
        cache.insert(kc.clone(), stub_snapshot());
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&ka).is_some(), "A was just-touched, must survive");
        assert!(cache.get(&kc).is_some(), "C is the latest, must survive");
        assert!(cache.get(&kb).is_none(), "B was LRU and must be evicted");
    }

    #[test]
    fn reinsert_same_key_replaces_entry() {
        // Re-inserting an already-present key MUST overwrite the prior
        // value but MUST NOT trigger LRU eviction (it's a hit-shaped
        // path, not a new entry). This matters when the same file is
        // imported twice in quick succession at the same `mtime`: the
        // second insert should just refresh `last_used`, not evict any
        // other unrelated entry.
        let cache = SnapshotCache::with_config(Duration::from_secs(60), 2);
        let (_td_a, pa) = temp_file_with(b"a");
        let (_td_b, pb) = temp_file_with(b"b");
        let ka = SnapshotKey::from_canonical_path(&pa).unwrap();
        let kb = SnapshotKey::from_canonical_path(&pb).unwrap();
        cache.insert(ka.clone(), stub_snapshot());
        cache.insert(kb.clone(), stub_snapshot());
        assert_eq!(cache.len(), 2);
        cache.insert(ka.clone(), stub_snapshot());
        assert_eq!(cache.len(), 2, "re-insert must NOT evict the other entry");
        assert!(cache.get(&ka).is_some());
        assert!(cache.get(&kb).is_some());
    }

    #[test]
    fn invalidate_path_drops_all_entries_for_path() {
        // Two distinct (mtime, size) pairs for the same canonical
        // path can co-exist if the file was overwritten between
        // imports. `invalidate_path` should drop both regardless of
        // the secondary key fields. We simulate this by inserting
        // manually with hand-constructed keys (the size differs).
        let cache = SnapshotCache::with_config(Duration::from_secs(60), 4);
        let (_td, p) = temp_file_with(b"placeholder");
        let k1 = SnapshotKey {
            canonical_path: p.clone(),
            mtime: SystemTime::UNIX_EPOCH,
            size: 1,
        };
        let k2 = SnapshotKey {
            canonical_path: p.clone(),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            size: 2,
        };
        cache.insert(k1.clone(), stub_snapshot());
        cache.insert(k2.clone(), stub_snapshot());
        assert_eq!(cache.len(), 2);
        cache.invalidate_path(&p);
        assert_eq!(cache.len(), 0);
    }
}
