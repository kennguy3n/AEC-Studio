//! Integration tests for the bridge service's concurrency contract.
//!
//! The napi layer wraps the singleton [`aec_bridge::BridgeService`] in a
//! `RwLock` so the read-only endpoints (`project_engine_status`,
//! `runtime_status`) can run concurrently with each other AND a write
//! endpoint can serialize against all of them. The N-API layer's
//! `with_service_ref_fallible` helper is napi-feature-gated, so this
//! test stands the same contract up locally using a plain `RwLock`
//! around a `BridgeService` and asserts:
//!
//!  1. The service is `Send + Sync` so it can live behind `RwLock` at
//!     all.
//!  2. Many threads can call `project_engine_status` concurrently and
//!     each one gets a correct [`aec_bridge::EngineStatusReport`].
//!  3. Repeated reads against the same path hit the connection cache
//!     (no per-call open).
//!  4. A write under the write lock excludes concurrent readers
//!     (proven by counting how many readers complete during a held
//!     write).
//!
//! These tests do not link against napi; they assert the architectural
//! invariants the napi layer relies on, so a future refactor of the
//! napi shim can't silently regress concurrency by accident.

use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use aec_bridge::{BridgeConfig, BridgeService};
use tempfile::TempDir;

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

fn make_service() -> (BridgeService, TempDir) {
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
    let s = BridgeService::new(cfg, [13u8; 32]).unwrap();
    (s, tmp)
}

/// Compile-time check: `BridgeService` must be `Send + Sync` so it can
/// live behind the `RwLock<Option<BridgeService>>` static in the napi
/// layer. If this trait-bound function compiles, the property holds.
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn bridge_service_is_send_and_sync() {
    assert_send_sync::<BridgeService>();
}

#[test]
fn concurrent_engine_status_reads_complete_correctly() {
    let (mut s, _g) = make_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Concurrent")
        .unwrap();
    let path = summary.path.clone();

    let lock = Arc::new(RwLock::new(s));

    // 8 reader threads, each calling project_engine_status 16 times.
    // All 128 calls must succeed and return the same report (the
    // project is otherwise idle).
    let mut handles = Vec::new();
    for _ in 0..8 {
        let lock = lock.clone();
        let path = path.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..16 {
                let svc = lock.read().expect("read-lock poisoned");
                let r = svc.project_engine_status(&path).expect("status read");
                assert_eq!(r.schema_version, aec_core::manifest::SCHEMA_VERSION);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // After the storm, the cache holds exactly one entry for the
    // single path that was polled. Idle eviction hasn't fired (TTL
    // is 30 s and the test runs in milliseconds), so the count is
    // exact.
    let svc = lock.read().unwrap();
    assert_eq!(svc.__engine_status_cache_len(), 1);
}

#[test]
fn write_lock_excludes_concurrent_reads() {
    // Hold a write lock for 100 ms and start 4 reader threads. None
    // of them should complete during the write window. After the
    // write is released they all complete promptly.
    let (mut s, _g) = make_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Excluded")
        .unwrap();
    let path = summary.path.clone();

    let lock = Arc::new(RwLock::new(s));

    let write_guard = lock.write().expect("acquire write lock");
    let write_held_until = Instant::now() + Duration::from_millis(100);

    let read_completion_times: Arc<std::sync::Mutex<Vec<Instant>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let lock = lock.clone();
        let path = path.clone();
        let times = read_completion_times.clone();
        handles.push(thread::spawn(move || {
            let svc = lock.read().expect("read-lock poisoned");
            svc.project_engine_status(&path).expect("status read");
            times.lock().unwrap().push(Instant::now());
        }));
    }

    // Wait past the planned release point, then drop the write guard.
    thread::sleep(Duration::from_millis(100));
    drop(write_guard);

    for h in handles {
        h.join().unwrap();
    }

    let times = read_completion_times.lock().unwrap();
    assert_eq!(times.len(), 4);
    for t in times.iter() {
        assert!(
            *t >= write_held_until,
            "reader completed at {:?} which is before the write release at {:?} — \
             RwLock did not exclude the reader",
            *t,
            write_held_until,
        );
    }
}

#[test]
fn save_under_write_lock_invalidates_cache_visible_to_subsequent_readers() {
    // Reader populates the cache, writer takes the write lock and
    // performs `project_save` (which invalidates the cache entry),
    // a later reader sees the cache repopulated by ITS open. The
    // assertion is on cache occupancy across the transitions.
    let (mut s, _g) = make_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Invalidated")
        .unwrap();
    let path = summary.path.clone();

    let lock = Arc::new(RwLock::new(s));

    // Reader 1: populate the cache.
    {
        let svc = lock.read().unwrap();
        svc.project_engine_status(&path).unwrap();
        assert_eq!(svc.__engine_status_cache_len(), 1);
    }

    // Writer: project_save invalidates the cache entry under &mut self.
    {
        let mut svc = lock.write().unwrap();
        svc.project_save(&path).unwrap();
        assert_eq!(svc.__engine_status_cache_len(), 0);
    }

    // Reader 2: re-populates the cache with a fresh connection.
    {
        let svc = lock.read().unwrap();
        svc.project_engine_status(&path).unwrap();
        assert_eq!(svc.__engine_status_cache_len(), 1);
    }
}
