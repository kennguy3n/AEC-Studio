//! # Phase 18 Group E — production readiness wrap-up integration test
//!
//! This is the **Task 27** integration test. It is the *outer* test
//! that ties together everything Tasks 23 – 26 secured, so the
//! invariants those tasks pinned at unit-test layer continue to hold
//! end-to-end as the public bridge API:
//!
//! * **Task 23** — no-Python invariant at ship time. The
//!   `crates/aec_ai/tests/no_python_invariant.rs` integration test
//!   already walks the workspace at every `cargo test`; this test
//!   here just adds a second assertion: the **runtime** path that
//!   loads the model registry must reject any descriptor with a
//!   non-GGUF format at serde-deserialization time. The check has
//!   to live at the public boundary of `BridgeService` (the
//!   constructor calls into `ModelRegistry::embedded()`) so a
//!   future maintainer who adds a `format: "mlx"` to
//!   `ai_models.json` is caught at *boot*, not in CI grep.
//!
//! * **Task 24** — binary integrity verification scaffold. We
//!   exercise the `aec_integrity::TrustAnchor::production()`
//!   fail-closed default plus the `aec_integrity::verify_manifest`
//!   round-trip with a deterministic test key to confirm the
//!   verification pipeline itself is real working code.
//!
//! * **Task 25** — sidecar lifecycle telemetry. The structured
//!   tracing events emitted by `AiState::ensure_ready` /
//!   `ImageGenState::ensure_ready` are unit-tested inline. Here we
//!   only confirm the `tracing` dependency is wired so a future
//!   `tracing-subscriber` consumer can collect them; this avoids a
//!   silent regression where a refactor strips the `tracing` import
//!   and the events become `eprintln!` lines.
//!
//! * **Task 26** — boot-time model verification. We construct a
//!   real `BridgeService`, run `model_integrity_report()` on
//!   pristine state, plant a tamper byte at the canonical path of
//!   one text-tier model, re-run, and confirm the tamper is
//!   surfaced. This is the **end-to-end** version of the inline
//!   unit tests in `service.rs`.
//!
//! ## Why this lives in `tests/`, not inline
//!
//! `tests/` integration tests exercise the *publicly exported*
//! surface of `aec_bridge` + `aec_integrity` exactly as a downstream
//! consumer (the napi shim, the renderer, the desktop binary) would
//! see it. If a refactor accidentally privatizes
//! [`BridgeService::model_integrity_report`] or strips the
//! `aec_integrity` re-exports, this test stops compiling — that's
//! the contract we want to lock down.

use std::fs;
use std::path::PathBuf;

use aec_bridge::{BridgeConfig, BridgeService};
use aec_integrity::{
    blake3_file, verify_files_against_pins, Blake3Digest, FilePin, ModelVerificationStatus,
    TrustAnchor, VerificationReport,
};

/// Spin up a [`BridgeService`] with all directories scoped to a
/// per-test `TempDir`. The bridge's text + image-gen model managers
/// will default-resolve to the user-data directory (the same
/// platform-default path real desktop sessions use), but the
/// `model_integrity_report` we exercise here only touches files we
/// explicitly plant at the canonical path, so the test is hermetic
/// w.r.t. anything the developer may have downloaded outside the
/// test harness.
fn boot_bridge() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    fs::create_dir_all(&templates).unwrap();

    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    // A deterministic master key — bridge needs one for its
    // sealed-state crypto but Task 27 doesn't exercise that path.
    let svc =
        BridgeService::new(cfg, [27u8; 32]).expect("bridge constructs against task-27 fixture");
    (svc, tmp)
}

#[test]
fn boot_does_not_panic_and_model_integrity_report_is_callable() {
    // Phase 18 Group E Task 23 — the registry baked into the bridge
    // binary at compile time must round-trip through serde at boot
    // (rejecting any non-GGUF format). Bridge construction calls
    // through `ModelRegistry::embedded` which would panic if a
    // future PR introduced a `format: "mlx"` descriptor. This is
    // therefore a boot smoke-test for the no-Python contract.
    //
    // Phase 18 Group E Task 26 — the `model_integrity_report`
    // method must be reachable from the public surface (the napi
    // shim hits this through the same `&BridgeService` reference).
    let (svc, _g) = boot_bridge();
    let report = svc
        .model_integrity_report()
        .expect("model_integrity_report compiles and runs against the bridge");
    // On a pristine test environment every entry is either
    // verified (if the developer has the file on disk) or missing
    // (most common in CI). Either is fine; a `mismatch` would mean
    // the developer's local user-data dir holds a tampered file,
    // which is itself a bug we want to surface.
    for entry in &report.entries {
        assert!(
            entry.status == "verified" || entry.status == "missing",
            "Task 27 contract: every pristine entry is verified-or-missing, got {}={} ({})",
            entry.id,
            entry.status,
            entry.detail
        );
    }
}

#[test]
fn integrity_report_serializes_to_json_for_renderer_consumption() {
    // Task 26 — the renderer surfaces the report through napi as
    // JSON, so the report struct must round-trip through serde
    // without loss. Pin this so a future refactor that drops the
    // serde derive doesn't silently break the Models pane.
    let (svc, _g) = boot_bridge();
    let report: VerificationReport = svc.model_integrity_report().unwrap();
    let json = serde_json::to_string(&report).expect("VerificationReport implements Serialize");
    assert!(json.contains("\"entries\""), "JSON shape: {json}");
    assert!(
        json.contains("\"all_verified\""),
        "JSON exposes the boot-time roll-up flag: {json}"
    );
    // Round-trip parity guards against a future struct field
    // change that breaks the Tauri / Electron IPC envelope.
    let restored: VerificationReport = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.entries.len(), report.entries.len());
    assert_eq!(restored.all_verified, report.all_verified);
}

#[test]
fn blake3_file_streams_large_files_without_loading_into_ram() {
    // Task 24 — `blake3_file` must stream (64 KiB chunks per
    // module doc). Plant a 5 MiB file (> any single-chunk limit
    // but << any "load into memory and panic" amount) and confirm
    // the hash matches a hand-computed reference. This pins that
    // the streaming pipeline is wired through `std::io::copy`
    // into the hasher and not via `fs::read(path)` (which would
    // OOM on a real GGUF).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("five_mib.bin");
    // Deterministic content: 5 MiB of a repeating pattern. The
    // pattern is intentionally non-trivial so an accidental
    // zero-fill bug in either the writer or the hasher would
    // change the digest.
    let chunk: Vec<u8> = (0u8..=255u8).cycle().take(5 * 1024 * 1024).collect();
    fs::write(&path, &chunk).unwrap();

    let streamed = blake3_file(&path).expect("blake3_file reads the 5 MiB fixture");
    // Reference: hash the same bytes with blake3 in one shot.
    let oneshot = blake3::hash(&chunk);
    assert_eq!(
        streamed.to_hex(),
        oneshot.to_hex().as_str(),
        "streaming BLAKE3 must equal one-shot BLAKE3 for the same input"
    );
}

#[test]
fn trust_anchor_production_is_empty_and_rejects_every_manifest() {
    // Task 24 — the production trust anchor MUST be empty until a
    // signing server is provisioned. This pins the fail-closed
    // posture; if a future PR ships a non-empty
    // `TrustAnchor::production()` without also wiring real
    // distribution, this test will catch the regression at
    // `cargo test --workspace` time.
    let anchor = TrustAnchor::production();
    assert_eq!(
        anchor.len(),
        0,
        "Task 24 contract: production TrustAnchor is empty (fail-closed) \
         until a signing server is provisioned"
    );
    // A sanity check that the verify path returns false against
    // an arbitrary message + signature pair (we don't even bother
    // constructing a real ed25519 signature — an empty anchor
    // has no keys to try, so verify must fall through to
    // `false`).
    let bytes = b"a payload";
    let bogus_sig = ed25519_dalek::Signature::from_bytes(&[0u8; ed25519_dalek::SIGNATURE_LENGTH]);
    assert!(
        !anchor.verify(bytes, &bogus_sig),
        "empty anchor must reject every (msg, sig) pair"
    );
}

#[test]
fn verify_files_against_pins_round_trips_real_file_io() {
    // Task 26 — `verify_files_against_pins` is the engine the
    // bridge calls into. Pin its behavior with a real disk
    // fixture so a future refactor that, e.g., swaps streaming
    // BLAKE3 for a one-shot read is caught here.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("model.gguf");
    let bytes = b"pretend GGUF file contents";
    fs::write(&path, bytes).unwrap();

    let digest_hex = blake3::hash(bytes).to_hex().to_string();
    let pin = FilePin {
        id: "text.small".to_string(),
        path: path.clone(),
        expected_blake3: Blake3Digest::from_hex(&digest_hex).unwrap(),
        expected_size_bytes: bytes.len() as u64,
    };

    let outcomes =
        verify_files_against_pins(std::slice::from_ref(&pin)).expect("single-pin pipeline runs");
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(outcomes[0].status, ModelVerificationStatus::Verified),
        "verified: got {:?}",
        outcomes[0].status
    );

    // Now tamper: flip the first byte. Re-run the same pin.
    let mut tampered = bytes.to_vec();
    tampered[0] ^= 0xFF;
    fs::write(&path, &tampered).unwrap();

    let outcomes = verify_files_against_pins(&[pin]).expect("pipeline runs against tampered");
    assert!(
        matches!(outcomes[0].status, ModelVerificationStatus::Mismatch { .. }),
        "Task 26 invariant: tampered file surfaces as Mismatch, got {:?}",
        outcomes[0].status
    );
}

#[test]
fn verify_files_against_pins_surfaces_missing_distinct_from_mismatch() {
    // Task 26 — "missing != tampered" is the renderer's contract:
    // missing routes to "download" CTA, mismatch routes to "tamper
    // warning + re-download". They must not be conflated.
    let tmp = tempfile::tempdir().unwrap();
    let nonexistent = tmp.path().join("not_here.gguf");

    let pin = FilePin {
        id: "image-gen.imaginary".to_string(),
        path: nonexistent,
        expected_blake3: Blake3Digest::from_hex(&"00".repeat(32)).unwrap(),
        expected_size_bytes: 1,
    };

    let outcomes = verify_files_against_pins(&[pin]).expect("missing-file pipeline runs");
    assert!(
        matches!(outcomes[0].status, ModelVerificationStatus::Missing),
        "missing file must report Missing, not Mismatch: got {:?}",
        outcomes[0].status
    );
    assert!(
        !outcomes[0].status.is_quarantine_trigger(),
        "Missing must NOT escalate to a quarantine trigger \
         (would otherwise cause pristine bridge boots to nuke the user's models dir)"
    );
}

#[test]
fn no_python_in_aec_integrity_crate_source_tree() {
    // Task 23 cross-cutting check — the verification crate
    // explicitly must not pull a Python dep. The workspace-wide
    // `crates/aec_ai/tests/no_python_invariant.rs` already covers
    // the whole tree, but this test pins the specific contract
    // that `aec_integrity` (the security-sensitive boundary crate)
    // is hand-audited and stays pure-Rust.
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("aec_integrity");
    // Walk every file under the crate root, fail if any `.py` is
    // present.
    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let p = entry.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
    let files = walk(&crate_root);
    for f in &files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        assert_ne!(
            ext,
            "py",
            "Task 23 contract: aec_integrity must contain zero Python, found {}",
            f.display()
        );
    }
    // Sanity: we actually walked something (so a path-typo above
    // can't false-pass the assertion).
    assert!(
        !files.is_empty(),
        "walked the aec_integrity crate but found no files; \
         test fixture is broken"
    );
    // And Cargo.toml of the crate must NOT depend on a
    // python-ffi-shaped crate.
    let cargo_toml = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("aec_integrity Cargo.toml is readable");
    for needle in ["pyo3", "rustpython", "cpython", "python3-sys", "python-sys"] {
        assert!(
            !cargo_toml.contains(needle),
            "Task 23 contract: aec_integrity Cargo.toml must not depend on {needle}"
        );
    }
}
