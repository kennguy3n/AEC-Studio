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
    /// Run `f` with mutable access to the cached connection. Stamps
    /// `last_used` to `Instant::now()` **after** `f` returns so the
    /// idle-eviction sweep observes the time the access *completed*,
    /// not the time it started.
    ///
    /// The inner mutex on `conn` is held for the duration of `f`,
    /// matching `rusqlite`'s requirement that a `Connection` only
    /// serve one query at a time.
    ///
    /// **Why stamp after, not before?** Semantically "last used"
    /// should mean "last completed use". A query that runs near the
    /// TTL boundary (e.g. a slow large `audit_chain` mirror read)
    /// must not be evicted by a concurrent `evict_stale` sweep that
    /// only sees a stale `last_used` from when the query *started*.
    /// The cost is one extra `last_used` lock acquire after the
    /// connection is released (~a handful of nanoseconds), which is
    /// negligible against any plausible SQL query latency.
    ///
    /// **Panic semantics.** If `f` panics, `last_used` is not
    /// updated; the entry retains its prior `last_used` and will be
    /// evicted on the normal TTL schedule. The connection lock
    /// poisons but is held only inside this method, so it doesn't
    /// leak (the `Arc<CachedConn>` will be dropped by the caller
    /// once it unwinds).
    pub(crate) fn with_conn<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Connection) -> R,
    {
        let result = {
            let mut conn = self.conn.lock().expect("cached connection mutex poisoned");
            f(&mut conn)
        };
        // Release `conn` *before* taking `last_used` so another
        // thread can be parked on `conn` while we briefly stamp.
        *self
            .last_used
            .lock()
            .expect("cached connection last_used mutex poisoned") = Instant::now();
        result
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
    /// Uses a **double-checked locking** pattern so the cache mutex is
    /// **not held during `open()`**. This matters on cold start: two
    /// concurrent first-polls for *different* projects each take the
    /// cache lock briefly, miss, release the lock, run their open
    /// concurrently, then briefly re-acquire to insert. Without DCL
    /// the second project's open would serialize behind the first
    /// project's ~0.8–1.2 ms key derive + PRAGMA cipher + migration
    /// walk.
    ///
    /// **Race semantics on the same key.** If two threads both miss
    /// the cache for the same path, both will run `open()` (one of
    /// the resulting connections is wasted). On the second insert,
    /// the loser observes the winner's already-inserted entry and
    /// returns that one; its own freshly-opened connection drops
    /// cleanly via `Arc`. This is a deliberate trade-off: the
    /// alternative is a per-key lock (overkill for an O(few) cache),
    /// and a wasted ~1 ms open on a rare race is cheaper than the
    /// global serialization the previous design imposed.
    ///
    /// The returned [`Arc`] keeps the entry alive across the cache
    /// lock release — the caller can run queries against it without
    /// holding the cache lock, so concurrent calls for *other* paths
    /// are not blocked.
    pub(crate) fn get_or_open<F, E>(&self, key: &Path, open: F) -> Result<Arc<CachedConn>, E>
    where
        F: FnOnce() -> Result<Connection, E>,
    {
        // Phase 1: hot-path lookup under the cache lock.
        //
        // If the cached entry's `conn` mutex is poisoned (a previous
        // `with_conn` callback panicked while holding it), evict the
        // entry and fall through to the cold-open path. Without this
        // step, every subsequent `get_or_open` for the same key would
        // hand out the poisoned `Arc<CachedConn>` and the next
        // `with_conn` call would re-panic on the poison check,
        // creating a cascading panic chain that only stopped when
        // the 30-s TTL eviction removed the entry. The fix is O(1)
        // (`Mutex::is_poisoned()` is a non-blocking atomic load) and
        // self-heals on the very next access instead of waiting out
        // the TTL.
        {
            let mut cache = self.entries.lock().expect("cache mutex poisoned");
            self.evict_stale(&mut cache);
            if let Some(entry) = cache.get(key) {
                if entry.conn.is_poisoned() {
                    cache.remove(key);
                } else {
                    *entry
                        .last_used
                        .lock()
                        .expect("cached connection last_used mutex poisoned") = Instant::now();
                    return Ok(entry.clone());
                }
            }
        }

        // Phase 2: open WITHOUT holding the cache lock. This is the
        // expensive operation (BLAKE3 key derive + PRAGMA cipher +
        // migration registry walk) and we deliberately let it run
        // concurrently with other readers / other-key cold opens.
        let conn = open()?;
        let new_entry = Arc::new(CachedConn {
            conn: Mutex::new(conn),
            last_used: Mutex::new(Instant::now()),
        });

        // Phase 3: re-acquire the cache lock and check again. If
        // another thread inserted while we were opening, their entry
        // wins (our connection drops with the `new_entry` Arc when
        // it goes out of scope on the return path).
        let mut cache = self.entries.lock().expect("cache mutex poisoned");
        if let Some(entry) = cache.get(key) {
            *entry
                .last_used
                .lock()
                .expect("cached connection last_used mutex poisoned") = Instant::now();
            return Ok(entry.clone());
        }
        cache.insert(key.to_path_buf(), new_entry.clone());
        Ok(new_entry)
    }

    /// Drop the cache entry for `key`, if any. Called by mutating
    /// endpoints so the next status read opens a fresh connection
    /// reflecting their changes.
    pub(crate) fn invalidate(&self, key: &Path) {
        let mut cache = self.entries.lock().expect("cache mutex poisoned");
        cache.remove(key);
    }

    /// Drop **every** cached entry. Used as a safe fallback by the
    /// mutating bridge endpoints when canonicalising the project
    /// path fails (e.g. a transient `std::fs::canonicalize` error
    /// between a successful open and the post-write invalidation):
    /// rather than silently leaving a potentially-stale entry alive
    /// until idle TTL eviction, blow the cache away so the next
    /// status read for *any* project is guaranteed to observe a
    /// fresh database state.
    ///
    /// In practice this path is essentially unreachable — the
    /// canonicalisation only runs *after* a successful
    /// [`aec_core::package::ProjectPackage::open_with_master_key`]
    /// which proves the path exists — but we still want to be
    /// correct in the face of NFS blips and similar weirdness rather
    /// than serve stale data.
    pub(crate) fn invalidate_all(&self) {
        let mut cache = self.entries.lock().expect("cache mutex poisoned");
        cache.clear();
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
            // `saturating_duration_since` (not `duration_since`):
            // `with_conn` stamps `last_used = Instant::now()` *without*
            // holding the cache mutex, so a concurrent `with_conn` on
            // a different cache entry can finish *between* this
            // function's `now = Instant::now()` capture (line 254) and
            // the per-entry `last_used.lock()` here. That makes it
            // possible to observe `last_used > now`. In current Rust
            // `Instant::duration_since` saturates to `Duration::ZERO`
            // in that case, but the std-lib docs explicitly warn
            // "Future versions may reintroduce the panic in some
            // circumstances", so using the explicit
            // `saturating_*` variant pins the safe behaviour
            // independently of stdlib evolution. A `last_used` later
            // than `now` semantically means "freshly used right now"
            // — `Duration::ZERO < ttl` correctly keeps the entry.
            now.saturating_duration_since(last_used) < ttl
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

    #[test]
    fn invalidate_all_clears_every_entry() {
        let cache = EngineStatusCache::new();
        let _a = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/inv-a"), || Ok(in_memory_conn()))
            .unwrap();
        let _b = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/inv-b"), || Ok(in_memory_conn()))
            .unwrap();
        let _c = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/inv-c"), || Ok(in_memory_conn()))
            .unwrap();
        assert_eq!(cache.len(), 3);

        cache.invalidate_all();
        assert_eq!(cache.len(), 0);

        // After wiping, subsequent get_or_open must re-open from
        // scratch — the cache has no memory of the previous entries.
        let calls = Cell::new(0u32);
        let _re = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/inv-a"), || {
                calls.set(calls.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        assert_eq!(
            calls.get(),
            1,
            "invalidate_all must force re-open on next access"
        );
    }

    #[test]
    fn get_or_open_does_not_hold_cache_lock_during_open() {
        // The double-checked-locking refactor moved `open()` out of
        // the cache-lock critical section so a slow cold open for
        // project A no longer serializes a concurrent cold open for
        // project B. Prove it: thread 1 holds an `open` callback open
        // for ~50 ms; meanwhile thread 2 must be able to call
        // `get_or_open` for a *different* key and complete promptly.
        use std::sync::mpsc;

        let cache = Arc::new(EngineStatusCache::new());
        let (start_slow_tx, start_slow_rx) = mpsc::channel::<()>();
        let (slow_inside_tx, slow_inside_rx) = mpsc::channel::<()>();
        let (release_slow_tx, release_slow_rx) = mpsc::channel::<()>();

        let cache_slow = cache.clone();
        let slow = thread::spawn(move || {
            start_slow_rx.recv().unwrap();
            let _e = cache_slow
                .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/dcl-slow"), || {
                    // Signal that we're now INSIDE the open callback.
                    slow_inside_tx.send(()).unwrap();
                    // Block until the test harness releases us.
                    release_slow_rx.recv().unwrap();
                    Ok(in_memory_conn())
                })
                .unwrap();
        });

        // Kick the slow thread off and wait until it's stuck inside
        // its open callback (i.e. past phase 1's lock release).
        start_slow_tx.send(()).unwrap();
        slow_inside_rx.recv().unwrap();

        // Now race a second cold open on a different key. If the
        // cache lock were still held by the slow thread this call
        // would block until release_slow_tx is signalled. With DCL
        // it returns immediately.
        let start = Instant::now();
        let _fast = cache
            .get_or_open::<_, rusqlite::Error>(Path::new("/tmp/dcl-fast"), || Ok(in_memory_conn()))
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(40),
            "second cold open should not block on first cold open's `open()`; \
             actually took {elapsed:?}"
        );

        // Release the slow thread so it can finish and join cleanly.
        release_slow_tx.send(()).unwrap();
        slow.join().unwrap();

        assert_eq!(cache.len(), 2, "both cold opens must end up cached");
    }

    #[test]
    fn same_key_concurrent_miss_settles_on_one_entry() {
        // Per the documented race semantics: when two threads both
        // miss the cache for the same key, both run `open()`, and the
        // second insert observes the winner's entry and discards its
        // own connection via Arc drop. Either way the cache ends up
        // with exactly one entry for that key and the two returned
        // Arcs point to the same `CachedConn`.
        let cache = Arc::new(EngineStatusCache::new());
        let key = Path::new("/tmp/dcl-race");

        let cache_a = cache.clone();
        let h_a = thread::spawn(move || {
            cache_a
                .get_or_open::<_, rusqlite::Error>(key, || {
                    // Sleep so the other thread reliably races.
                    thread::sleep(Duration::from_millis(10));
                    Ok(in_memory_conn())
                })
                .unwrap()
        });
        let cache_b = cache.clone();
        let h_b = thread::spawn(move || {
            cache_b
                .get_or_open::<_, rusqlite::Error>(key, || {
                    thread::sleep(Duration::from_millis(10));
                    Ok(in_memory_conn())
                })
                .unwrap()
        });

        let entry_a = h_a.join().unwrap();
        let entry_b = h_b.join().unwrap();

        // Critical invariant: both threads return Arcs pointing at
        // the SAME cached CachedConn — the loser of the second-check
        // race re-fetched the winner's entry.
        assert!(
            Arc::ptr_eq(&entry_a, &entry_b),
            "same-key concurrent miss must settle on one cached entry, not two"
        );
        assert_eq!(
            cache.len(),
            1,
            "cache must hold exactly one entry for the key"
        );
    }

    #[test]
    fn with_conn_stamps_last_used_after_f_completes() {
        // `with_conn` must update `last_used` *after* `f` returns so
        // a long-running query near the TTL boundary doesn't get
        // evicted by a concurrent sweep that only sees a stale
        // start-time `last_used`. Verify the post-f stamp is strictly
        // later than a marker captured during `f`.
        let cache = Arc::new(EngineStatusCache::new());
        let key = PathBuf::from("/tmp/with-conn-stamp-after");
        let entry = cache
            .get_or_open::<_, rusqlite::Error>(&key, || Ok(in_memory_conn()))
            .unwrap();

        let mid_f_marker = entry.with_conn(|_c| {
            let marker = Instant::now();
            // Simulate a query that takes noticeable wall-clock time.
            thread::sleep(Duration::from_millis(15));
            marker
        });

        let last_used_after = *entry
            .last_used
            .lock()
            .expect("cached connection last_used mutex poisoned");

        assert!(
            last_used_after > mid_f_marker,
            "last_used must be stamped after f completes, not before \
             (last_used={last_used_after:?}, mid-f marker={mid_f_marker:?})"
        );
    }

    #[test]
    fn with_conn_does_not_advance_last_used_on_panic() {
        // Documented panic semantics: if `f` panics, `last_used` is
        // not updated. Verify by capturing the pre-panic stamp,
        // catching a panic from `f`, then asserting `last_used` has
        // not advanced.
        let cache = Arc::new(EngineStatusCache::new());
        let key = PathBuf::from("/tmp/with-conn-panic");
        let entry = cache
            .get_or_open::<_, rusqlite::Error>(&key, || Ok(in_memory_conn()))
            .unwrap();

        let before = *entry
            .last_used
            .lock()
            .expect("cached connection last_used mutex poisoned");

        // Sleep just enough that any post-f stamp would observably
        // advance the timestamp.
        thread::sleep(Duration::from_millis(10));

        let entry_for_panic = entry.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            entry_for_panic.with_conn(|_c| -> () { panic!("simulated query panic") });
        }));
        assert!(result.is_err(), "test fixture: f must panic");

        let after = *entry
            .last_used
            .lock()
            .expect("cached connection last_used mutex poisoned");

        assert_eq!(
            before, after,
            "panic in f must leave last_used unchanged \
             (was {before:?}, became {after:?})"
        );
    }

    #[test]
    fn get_or_open_evicts_poisoned_entry_and_reopens() {
        // After a `with_conn` callback panics the inner `conn` Mutex
        // is poisoned. A naive cache would keep handing out the
        // poisoned `Arc<CachedConn>` until the 30-s TTL evicted it,
        // and every interim `with_conn` would re-panic. Verify the
        // hot-path lookup detects poison via `Mutex::is_poisoned()`,
        // removes the entry, and falls through to a fresh open.
        let cache = Arc::new(EngineStatusCache::new());
        let key = PathBuf::from("/tmp/poison-recover");

        // First open seeds the cache.
        let first = cache
            .get_or_open::<_, rusqlite::Error>(&key, || Ok(in_memory_conn()))
            .unwrap();
        assert_eq!(cache.len(), 1);

        // Poison the entry's conn mutex by panicking inside with_conn.
        let first_for_panic = first.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            first_for_panic.with_conn(|_c| -> () { panic!("simulated query panic") });
        }));
        assert!(
            first.conn.is_poisoned(),
            "fixture: with_conn panic must poison the inner conn mutex"
        );

        // Drop our reference to the poisoned entry so the cache holds
        // the only Arc — verifying the cache really does remove and
        // free it on the next lookup (the count goes 1 → 0 → 1, with
        // the second 1 being the fresh entry).
        drop(first);

        // Next get_or_open must detect poison, evict, and re-open.
        // Track whether `open` ran to prove the cold-open path was
        // taken.
        let opens = Cell::new(0u32);
        let fresh = cache
            .get_or_open::<_, rusqlite::Error>(&key, || {
                opens.set(opens.get() + 1);
                Ok(in_memory_conn())
            })
            .unwrap();
        assert_eq!(
            opens.get(),
            1,
            "poisoned cache entry must force a cold open on next access"
        );
        assert!(
            !fresh.conn.is_poisoned(),
            "fresh entry must have a non-poisoned conn mutex"
        );

        // The fresh entry must actually work: calling `with_conn`
        // on it should not panic.
        fresh.with_conn(|conn| {
            conn.execute_batch("SELECT 1;").expect("fresh conn works");
        });

        assert_eq!(cache.len(), 1, "cache must hold exactly the fresh entry");
    }
}
