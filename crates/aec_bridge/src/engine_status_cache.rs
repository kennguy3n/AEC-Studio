//! Connection cache for `BridgeService::project_engine_status`.
//!
//! ## Why a cache?
//!
//! `project_engine_status` is the renderer's status-pane refresh
//! endpoint. The Electron renderer polls it periodically (typical
//! cadence 1–5 s) to keep the schema version, audit-chain head, and
//! per-scope command counts live in the UI. Every call goes through
//! [`aec_core::package::ProjectPackage::open_database`], which:
//!
//!  1. Reads `manifest.json` to derive the per-project SQLCipher key
//!     via BLAKE3 (the manifest read itself is cheap, but the BLAKE3
//!     derive is intentionally tunable so it's not free).
//!  2. Opens the SQLCipher file and runs every `PRAGMA cipher_*`
//!     setup statement before any data can be read.
//!  3. Walks the migration registry (no-op when up to date) to make
//!     legacy v1 projects safe to query.
//!
//! In aggregate that's a measured ~0.8–1.2 ms per call on modern
//! desktop hardware — small per individual poll but compounding when
//! the status pane is visible alongside the user's actual workflow.
//! Caching the open [`rusqlite::Connection`] across consecutive polls
//! drops the per-poll cost to the actual SQL query latency (~50 µs).
//!
//! ## Concurrency model
//!
//!  * The cache itself is a [`Mutex`] guarding a `HashMap`. Lookups
//!    are O(1) average and the lock is held for the lookup only — not
//!    while the cached connection is actually executing SQL.
//!  * Each cache entry is an [`Arc<CachedConn>`] containing an inner
//!    [`Mutex<Connection>`]. Two concurrent status reads for the *same*
//!    project serialize on that inner mutex, matching the hard
//!    serialization that `rusqlite` already imposes on a single
//!    `Connection`. Two concurrent reads for *different* projects use
//!    different cache entries and run fully in parallel.
//!  * The `napi_api::with_service_ref_fallible` reader-lock on the
//!    `BridgeService` singleton sits *above* this cache, so any
//!    mutating endpoint (`project_open`, `project_save`,
//!    `project_audit_sync`) excludes all status reads for its duration
//!    via the outer `RwLock`. That guarantees the cache only ever sees
//!    a single writer touching the underlying file at any given moment.
//!
//! ## Invalidation
//!
//!  * **Idle eviction.** Entries unused for [`CACHE_TTL`] are dropped
//!    on the next [`EngineStatusCache::get_or_open`] call. 30 s is a
//!    comfortable margin above a typical 1–5 s poll cadence while
//!    still letting `rusqlite` close the SQLCipher file handle when
//!    the user pauses interaction.
//!  * **Explicit invalidation.** The mutating bridge endpoints call
//!    [`EngineStatusCache::invalidate`] after their write so the next
//!    status read picks up the post-migration / post-write state on
//!    a freshly-opened connection. This matters in particular for any
//!    future v3+ schema migration: a cached read connection's
//!    statement cache would otherwise hold prepared statements
//!    referencing the pre-migration schema.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

/// How long a cached open SQLCipher connection stays valid between
/// `project_engine_status` calls before being lazily evicted on the
/// next access.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(30);

/// One cached `Connection` plus its last-used timestamp.
///
/// The fields are deliberately private: callers must go through
/// [`CachedConn::with_conn`] so `last_used` cannot drift from actual
/// connection use.
pub(crate) struct CachedConn {
    conn: Mutex<Connection>,
    last_used: Mutex<Instant>,
}

impl CachedConn {
    /// Run `f` with mutable access to the cached connection. Updates
    /// `last_used` atomically with the access so the idle-eviction
    /// pass observes the call.
    ///
    /// The inner mutex is held for the duration of `f`, matching
    /// `rusqlite`'s requirement that a `Connection` only serve one
    /// query at a time.
    pub(crate) fn with_conn<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Connection) -> R,
    {
        let mut conn = self.conn.lock().expect("cached connection mutex poisoned");
        *self
            .last_used
            .lock()
            .expect("cached connection last_used mutex poisoned") = Instant::now();
        f(&mut conn)
    }
}

/// LRU-by-access connection cache for `project_engine_status`.
pub(crate) struct EngineStatusCache {
    entries: Mutex<HashMap<PathBuf, Arc<CachedConn>>>,
    /// Per-instance idle TTL, threaded as a field so tests can construct
    /// caches with a 1 ms TTL to assert eviction without sleeping.
    /// Production callers always use [`CACHE_TTL`].
    ttl: Duration,
}

impl EngineStatusCache {
    pub(crate) fn new() -> Self {
        Self::with_ttl(CACHE_TTL)
    }

    pub(crate) fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Return a cached connection for `key` if one exists and is not
    /// stale; otherwise call `open` to produce a fresh one, insert it,
    /// and return it.
    ///
    /// The returned [`Arc`] keeps the entry alive across the cache
    /// lock release — the caller can run queries against it without
    /// holding the cache lock, so concurrent calls for *other* paths
    /// are not blocked.
    pub(crate) fn get_or_open<F, E>(&self, key: &Path, open: F) -> Result<Arc<CachedConn>, E>
    where
        F: FnOnce() -> Result<Connection, E>,
    {
        let mut cache = self.entries.lock().expect("cache mutex poisoned");
        self.evict_stale(&mut cache);
        if let Some(entry) = cache.get(key) {
            // Hot path: refresh last_used so we don't churn the entry
            // out on the next eviction sweep.
            *entry
                .last_used
                .lock()
                .expect("cached connection last_used mutex poisoned") = Instant::now();
            return Ok(entry.clone());
        }
        let conn = open()?;
        let entry = Arc::new(CachedConn {
            conn: Mutex::new(conn),
            last_used: Mutex::new(Instant::now()),
        });
        cache.insert(key.to_path_buf(), entry.clone());
        Ok(entry)
    }

    /// Drop the cache entry for `key`, if any. Called by mutating
    /// endpoints so the next status read opens a fresh connection
    /// reflecting their changes.
    pub(crate) fn invalidate(&self, key: &Path) {
        let mut cache = self.entries.lock().expect("cache mutex poisoned");
        cache.remove(key);
    }

    /// How many connections are currently cached. Used by the
    /// service-layer test accessor `BridgeService::__engine_status_cache_len`
    /// (always-available so integration tests in `tests/` can observe
    /// it). Not `cfg(test)`-gated because integration tests build the
    /// lib without `cfg(test)` active; the cost in production is one
    /// dead-code function. The function is `pub(crate)` so the
    /// observability surface stays inside the crate.
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().expect("cache mutex poisoned").len()
    }

    fn evict_stale(&self, cache: &mut HashMap<PathBuf, Arc<CachedConn>>) {
        let now = Instant::now();
        let ttl = self.ttl;
        cache.retain(|_, entry| {
            let last_used = *entry
                .last_used
                .lock()
                .expect("cached connection last_used mutex poisoned");
            now.duration_since(last_used) < ttl
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::thread;

    fn in_memory_conn() -> Connection {
        Connection::open_in_memory().expect("open in-memory sqlite for test")
    }

    #[test]
    fn get_or_open_inserts_on_miss() {
        let cache = EngineStatusCache::new();
        let key = PathBuf::from("/tmp/project-a");
        let calls = Cell::new(0u32);
        let _entry = cache
            .get_or_open::<_, rusqlite::Error>(&key, || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_or_open_reuses_on_hit() {
        let cache = EngineStatusCache::new();
        let key = PathBuf::from("/tmp/project-b");
        let calls = Cell::new(0u32);

        let entry1 = cache
            .get_or_open::<_, rusqlite::Error>(&key, || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        let entry2 = cache
            .get_or_open::<_, rusqlite::Error>(&key, || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();

        assert_eq!(calls.get(), 1, "second call must not re-open");
        assert!(
            Arc::ptr_eq(&entry1, &entry2),
            "cached Arc must be reused, not re-derived"
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn invalidate_drops_entry() {
        let cache = EngineStatusCache::new();
        let key = PathBuf::from("/tmp/project-c");

        let _e = cache
            .get_or_open::<_, rusqlite::Error>(&key, || Ok(in_memory_conn()))
            .unwrap();
        assert_eq!(cache.len(), 1);

        cache.invalidate(&key);
        assert_eq!(cache.len(), 0);

        // Next get_or_open must re-open.
        let calls = Cell::new(0u32);
        let _e = cache
            .get_or_open::<_, rusqlite::Error>(&key, || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn idle_entries_evicted_on_next_access() {
        let cache = EngineStatusCache::with_ttl(Duration::from_millis(10));
        let key_a = PathBuf::from("/tmp/project-d");
        let key_b = PathBuf::from("/tmp/project-e");

        let _e = cache
            .get_or_open::<_, rusqlite::Error>(&key_a, || Ok(in_memory_conn()))
            .unwrap();
        assert_eq!(cache.len(), 1);

        // Wait past TTL.
        thread::sleep(Duration::from_millis(20));

        // Any subsequent access triggers the eviction sweep — even one
        // for a different key.
        let _e = cache
            .get_or_open::<_, rusqlite::Error>(&key_b, || Ok(in_memory_conn()))
            .unwrap();
        assert_eq!(
            cache.len(),
            1,
            "stale key_a entry must be evicted; only key_b survives"
        );
    }

    #[test]
    fn different_keys_open_separate_connections() {
        let cache = EngineStatusCache::new();
        let calls = Cell::new(0u32);
        let _a = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/a"), || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        let _b = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/b"), || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn with_conn_serializes_concurrent_access_to_same_key() {
        // Two threads call `with_conn` on the same cached entry; the
        // inner mutex must serialize them so rusqlite never sees
        // concurrent statements on one Connection.
        let cache = Arc::new(EngineStatusCache::new());
        let key = PathBuf::from("/tmp/project-shared");
        let entry = cache
            .get_or_open::<_, rusqlite::Error>(&key, || Ok(in_memory_conn()))
            .unwrap();
        entry
            .with_conn(|c| c.execute_batch("CREATE TABLE t (x INTEGER)"))
            .unwrap();

        let entry_clone = entry.clone();
        let h = thread::spawn(move || {
            entry_clone
                .with_conn(|c| -> rusqlite::Result<()> {
                    c.execute("INSERT INTO t VALUES (1)", [])?;
                    thread::sleep(Duration::from_millis(20));
                    c.execute("INSERT INTO t VALUES (2)", [])?;
                    Ok(())
                })
                .unwrap();
        });

        thread::sleep(Duration::from_millis(5));
        // While the spawned thread holds the inner mutex this call
        // blocks rather than racing into the same Connection.
        let n: i64 = entry
            .with_conn(|c| c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)))
            .unwrap();

        h.join().unwrap();

        // The query saw a consistent state — either before both
        // inserts (n=0) or after both (n=2), never the intermediate
        // n=1.
        assert!(
            n == 0 || n == 2,
            "with_conn must serialize: saw n={n} (1 means torn read)"
        );
    }

    #[test]
    fn open_failure_does_not_populate_cache() {
        let cache = EngineStatusCache::new();
        let key = PathBuf::from("/tmp/project-fails-to-open");

        let result = cache
            .get_or_open::<_, rusqlite::Error>(&key, || Err(rusqlite::Error::QueryReturnedNoRows));
        assert!(result.is_err());
        assert_eq!(cache.len(), 0, "failed open must not leave a tombstone");
    }
}
