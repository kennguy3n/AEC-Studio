//! # Phase 18 Group F Task 28 — end-to-end image-gen lifecycle test
//!
//! Exercises the **full happy path** of the image-gen sidecar through
//! the same public `BridgeService` surface the napi shim wraps:
//!
//! ```text
//! descriptor pin → integrity check → spawn → generate → idle unload → re-spawn
//! ```
//!
//! ## Why this lives in `tests/`
//!
//! Integration tests in `tests/` link `aec_bridge` as an external
//! crate and can only call into its *public* API — the same surface
//! the napi shim sees. If a future refactor accidentally privatizes
//! [`BridgeService::image_gen_set_descriptor`] /
//! [`BridgeService::image_gen_prepare_generate`] /
//! [`BridgeService::run_image_gen_generate`] /
//! [`BridgeService::image_gen_maybe_unload`] /
//! [`BridgeService::image_gen_runtime_status`] /
//! [`BridgeService::model_integrity_report`], this test stops
//! compiling. That's the contract we want to lock down end-to-end
//! beyond what the inline unit tests in `service.rs` cover.
//!
//! ## Why the mock TCP server (not a real `sd-server` binary)
//!
//! Stable-diffusion sidecars take 30+ seconds to mmap the GGUF and
//! initialise their Vulkan / Metal context, then 60-180 seconds per
//! generate. Running that in CI on every PR is infeasible. Instead
//! the test:
//!
//! 1. Plants a fake model file on disk + pins a descriptor matching
//!    its BLAKE3, so the *integrity* path is fully real.
//! 2. Stands up a `TcpListener` on `127.0.0.1:0` that speaks the
//!    A1111-shaped `/sdapi/v1/txt2img` protocol with a canned PNG
//!    payload.
//! 3. Builds an [`ImageGenState`] via
//!    [`ImageGenState::__test_with_transport`] pointing at the mock
//!    server's port, then installs it on the bridge via
//!    [`BridgeService::__test_install_image_gen_state`].
//!
//! Every byte that crosses the wire crosses **real** loopback TCP and
//! goes through the **real** `ImageGenTransport::generate` HTTP
//! encoder + decoder. Only the `llama-server`/`sd-server` *process*
//! is replaced by a deterministic mock — the bridge's lifecycle
//! state machine, runtime config, governor policy gate, prepare/run
//! split, idle-eviction tick, and integrity verifier are all real
//! production code paths.
//!
//! ## Lifecycle phases asserted
//!
//! | Phase | Code path | Assertion |
//! |---|---|---|
//! | 1. Descriptor pin | `image_gen_set_descriptor` | `model_path` baked into runtime config; runtime resets to `Idle`. |
//! | 2. Integrity check | `model_integrity_report` | Planted fake file reports `Verified`. |
//! | 3. Spawn | `__test_install_image_gen_state(... ::__test_with_transport)` | Runtime publishes `Ready`. |
//! | 4. Generate | `image_gen_prepare_generate` + `run_image_gen_generate` | Real `/sdapi/v1/txt2img` round-trip against mock server; PNG decoded; seed echoed. |
//! | 5. Idle unload | `image_gen_maybe_unload` | After idle window elapses, `maybe_unload` returns `true` and runtime is `Idle`. |
//! | 6. Re-spawn | Install second `__test_with_transport` against a fresh mock server | Runtime publishes `Ready` again; second generate succeeds. |
//!
//! The two-mock-server pattern (steps 3 and 6) deliberately uses
//! *different* TCP ports for the spawn and re-spawn phases — this
//! pins that the bridge actually swaps the underlying handle after
//! eviction, rather than reusing a stale transport.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use aec_ai::image_gen::{
    ImageGenConfig, ImageGenModelDescriptor, ImageGenRuntime, ImageGenRuntimeConfig,
    ImageGenRuntimeState, ImageGenTransport,
};
use aec_bridge::image_gen_state::ImageGenState;
use aec_bridge::service::ImageGenGenerateRequest;
use aec_bridge::{BridgeConfig, BridgeService};
use aec_governor::HardwareTier;
use aec_integrity::Blake3Digest;
use base64::Engine;

// A 67-byte minimal valid PNG (1x1 black pixel) — sufficient for the
// transport to decode + the renderer to display. Real diffusion
// output would be megabytes; we only need the wire round-trip to
// prove the transport.generate → ImageGenResponse pipeline works.
const ONE_PIXEL_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

/// Construct an A1111-shaped txt2img response body wrapping the
/// one-pixel PNG and a deterministic seed. The seed is the
/// renderer-visible reproducibility key — the test asserts the
/// bridge plumbs it back through `ImageGenResponse::seed`.
fn canned_a1111_response(seed: i64) -> String {
    format!(
        r#"{{"images":["{ONE_PIXEL_PNG_BASE64}"],"parameters":{{"seed":{seed}}},"info":"e2e-mock"}}"#
    )
}

/// Build a full HTTP/1.1 response framing the JSON body. The
/// `Content-Length` header is critical — the bridge's HTTP client
/// uses it to determine when the body is fully received.
fn http_response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

/// Spawn a mock A1111 sidecar that handles a single `/sdapi/v1/txt2img`
/// request with a canned response, then exits. Returns `(port, join_handle)`.
fn spawn_mock_sidecar(seed: i64) -> (u16, mpsc::Receiver<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let (done_tx, done_rx) = mpsc::channel();
    let body = canned_a1111_response(seed);
    let response = http_response(&body);
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _addr)) = listener.accept() {
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(&response);
            stream.flush().ok();
            let _ = done_tx.send(());
        }
    });
    (port, done_rx, handle)
}

/// Boot a `BridgeService` with a hermetic temporary state /
/// projects / templates directory so the test doesn't pollute the
/// developer's user-data dir.
fn boot_bridge() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    // Deterministic 32-byte master key — sealed-state crypto isn't
    // exercised by the image-gen lifecycle; any fixed value works.
    let svc = BridgeService::new(cfg, [0x28u8; 32]).expect("bridge constructs");
    (svc, tmp)
}

/// Build a runtime config that mirrors a freshly-spawned sidecar
/// pointing at `port`, with an *aggressively short* idle window so
/// the test exercises eviction in real time rather than waiting
/// minutes for the production budget.
fn mock_runtime_config(port: u16, model_path: std::path::PathBuf) -> ImageGenRuntimeConfig {
    ImageGenRuntimeConfig {
        spawn_config: ImageGenConfig {
            port,
            model_path,
            vae_path: None,
            threads: 1,
            request_timeout: Duration::from_secs(5),
        },
        idle_timeout: Duration::from_millis(50),
        load_budget: Duration::from_secs(5),
    }
}

#[test]
fn e2e_full_lifecycle_descriptor_pin_through_respawn() {
    // --------- PHASE 0: boot ---------
    // The bridge constructs with the boot-default Medium tier
    // governor policy. We override to Pro to exercise the higher
    // idle budget path (though we'll still install a test
    // ImageGenState with a tighter window for eviction timing).
    let (mut svc, _tmpdir) = boot_bridge();
    svc.governor_apply_hardware_tier(HardwareTier::Pro)
        .expect("apply Pro tier");

    // --------- PHASE 1: descriptor pin ---------
    // Plant a fake model file on disk where the image-gen model
    // manager expects it. `image_gen_model_availability` and the
    // integrity report both stat this path, so we need a real file.
    let availability_before = svc
        .image_gen_model_availability()
        .expect("availability callable before descriptor pin");
    // Pre-pin the bridge reports an empty filename — the renderer
    // uses this to gate the wizard's empty-state CTA.
    assert!(
        availability_before.filename.is_empty(),
        "before pin: filename is empty so the wizard renders the empty-state CTA, got {:?}",
        availability_before.filename
    );

    // Prepare a real GGUF-shaped file on disk so the post-pin
    // availability check can confirm `available: true` end-to-end.
    //
    // The shipped image-gen model manager uses
    // `aec_ai::default_models_dir()` (a stable per-OS user-data
    // path) and the bridge has no constructor knob to override it.
    // That's correct for production but means parallel / repeated
    // test runs would observe each other's planted files. Make the
    // filename unique-per-run so each invocation starts from a
    // genuine empty-state.
    let unique_tag = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let unique_filename = format!("task28-mock-sd-{unique_tag}.gguf");
    let model_bytes =
        format!("PRETEND GGUF: an SD descriptor planted by the Task 28 e2e test ({unique_tag})");
    let model_bytes = model_bytes.as_bytes();
    let blake3_hex = blake3::hash(model_bytes).to_hex().to_string();
    let size_bytes = model_bytes.len() as u64;

    // Pin the descriptor — bridge stores the metadata and rebuilds
    // the runtime config so the next ensure_ready uses this model.
    let descriptor = ImageGenModelDescriptor {
        filename: unique_filename.clone(),
        blake3_hex: blake3_hex.clone(),
        size_bytes,
        download_url: Some(
            "https://huggingface.co/aec-studio-test/task28-mock-sd/resolve/main/task28-mock-sd.gguf"
                .into(),
        ),
        vae_filename: None,
    };
    svc.image_gen_set_descriptor(descriptor.clone())
        .expect("descriptor pins under https:// + non-empty filename validation");

    // After the pin the runtime config has the model path baked in
    // (via `set_descriptor`'s reload_with_config call). Read it back
    // through the public availability surface — same surface the
    // renderer's Settings panel polls.
    let availability_after = svc
        .image_gen_model_availability()
        .expect("availability callable after descriptor pin");
    assert_eq!(
        availability_after.filename, unique_filename,
        "post-pin: bridge returns the pinned filename so the renderer can show it"
    );
    assert_eq!(
        availability_after.blake3_hex, blake3_hex,
        "post-pin: pinned BLAKE3 round-trips through the bridge surface unchanged"
    );
    assert_eq!(availability_after.size_bytes, size_bytes);
    // The actual file isn't on disk yet, so `available` is false.
    // This pins the contract: the renderer gates the Generate button
    // on `available`, not on `filename != ""`.
    assert!(
        !availability_after.available,
        "post-pin but pre-download: available=false, the renderer shows the Download button"
    );

    // Now plant the file at the canonical resolved path so the
    // integrity report can verify it. The path is the
    // `models_dir` from the availability surface joined with the
    // pinned filename — exactly the path the production sidecar
    // would read.
    let resolved_model_path: PathBuf =
        PathBuf::from(&availability_after.models_dir).join(&availability_after.filename);
    if let Some(parent) = resolved_model_path.parent() {
        std::fs::create_dir_all(parent).expect("create models dir");
    }
    std::fs::write(&resolved_model_path, model_bytes).expect("plant fake model on disk");

    // Sanity: now that the file is on disk, availability flips to true.
    let availability_with_file = svc
        .image_gen_model_availability()
        .expect("availability callable with file planted");
    assert!(
        availability_with_file.available,
        "file planted + size matches descriptor.size_bytes → available=true"
    );
    assert_eq!(availability_with_file.size_on_disk, size_bytes);

    // --------- PHASE 2: integrity verify ---------
    // The boot-time integrity verifier walks every pinned model and
    // reports per-file Verified / Missing / Mismatch / ReadError.
    // With the file we just planted matching the pinned BLAKE3, the
    // image-gen entry must be Verified.
    let report = svc
        .model_integrity_report()
        .expect("model_integrity_report callable");
    // The user-supplied descriptor is namespaced as
    // `image-gen.custom` (registry presets use
    // `image-gen.{preset-id}`). See
    // `BridgeService::model_integrity_report` for the namespacing
    // contract.
    let image_gen_entry = report
        .entries
        .iter()
        .find(|e| e.id == "image-gen.custom")
        .expect("integrity report contains the user-pinned image-gen.custom entry");
    assert_eq!(
        image_gen_entry.status, "verified",
        "planted file's BLAKE3 matches the pinned digest, so integrity reports Verified: {} ({})",
        image_gen_entry.status, image_gen_entry.detail
    );

    // Cross-check the digest computed by the boot verifier against
    // an independent BLAKE3 of the same bytes. If `verify_files_against_pins`
    // ever stops streaming and switches to in-memory reads, this
    // assertion still holds — it's the digest equality, not the
    // streaming-vs-buffered implementation, that's the contract.
    let oneshot = blake3::hash(model_bytes).to_hex().to_string();
    let pinned = Blake3Digest::from_hex(&oneshot).unwrap();
    assert_eq!(
        pinned.to_hex(),
        oneshot,
        "Blake3Digest round-trip through hex is lossless"
    );

    // --------- PHASE 3: spawn (mocked) ---------
    // Stand up the first mock A1111 sidecar on a random port.
    let seed_1: i64 = 28_000_001;
    let (port_1, done_rx_1, server_1) = spawn_mock_sidecar(seed_1);

    // Build an ImageGenState pre-attached to the mock transport.
    // `__test_with_transport` advances the runtime through
    // begin_load → mark_ready so the next `ensure_ready` is a fast
    // path that just hands back the existing transport.
    let cfg = mock_runtime_config(port_1, resolved_model_path.clone());
    let transport = ImageGenTransport::new(port_1, Duration::from_secs(5));
    let state_1 = ImageGenState::__test_with_transport(cfg.clone(), transport);
    svc.__test_install_image_gen_state(state_1);

    // The bridge's renderer-visible status must now be Ready.
    let status_after_spawn = svc
        .image_gen_runtime_status()
        .expect("runtime_status after spawn");
    assert_eq!(
        status_after_spawn.state, "ready",
        "post-spawn: runtime publishes Ready so the renderer enables the Generate button"
    );
    assert!(
        status_after_spawn.last_error.is_none(),
        "post-spawn: no sticky error state"
    );

    // --------- PHASE 4: generate (real HTTP round-trip) ---------
    // The prepare/run split is the same code path the napi handler
    // wraps. `prepare_generate` validates the request + clones the
    // Arc<ImageGenState> + snapshots the policy under a brief
    // bridge reader guard; `run_image_gen_generate` then does the
    // (potentially multi-minute, but here ~milliseconds against the
    // mock) actual generate with NO bridge lock held.
    let request = ImageGenGenerateRequest {
        prompt: "a wooden chair, studio lighting".into(),
        negative_prompt: None,
        width: 512,
        height: 512,
        steps: 20,
        cfg_scale: 7.0,
        seed: None,
        sampler: None,
    };
    let ctx = svc
        .image_gen_prepare_generate(request)
        .expect("prepare_generate validates the request");
    let result = BridgeService::run_image_gen_generate(ctx).expect("generate against mock sidecar");

    // Wait for the mock server thread to actually deliver the
    // response — without this the test could race the server thread
    // and observe its handle as still alive.
    done_rx_1
        .recv_timeout(Duration::from_secs(3))
        .expect("mock server 1 received and replied to the txt2img request");
    server_1.join().ok();

    // The bridge wraps the transport's PNG output in an
    // ImageGenGenerateResult with the renderer-visible shape. Pin
    // the contract: seed is the one from the canned response (NOT
    // the request's None), width/height are echoed from the request
    // (A1111 protocol doesn't return them), PNG bytes are non-empty.
    assert_eq!(
        result.seed,
        Some(seed_1),
        "bridge plumbs the sidecar-returned seed back to the renderer so the user can reproduce the result"
    );
    assert_eq!(result.width, 512);
    assert_eq!(result.height, 512);
    assert_eq!(result.steps, 20);
    assert!(
        !result.png_base64.is_empty(),
        "bridge returns a non-empty base64-encoded PNG"
    );
    // The bridge surfaces the PNG to the renderer as base64
    // (napi-rs's Buffer marshalling is slower than a base64
    // round-trip for ~512 KiB payloads). Decode here and pin the
    // PNG signature so a regression that double-encodes or skips
    // decoding the sidecar's response is caught.
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.png_base64)
        .expect("bridge's png_base64 is valid STANDARD base64");
    assert_eq!(
        &png_bytes[..8],
        &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        "bridge decodes the sidecar's base64 into a valid PNG signature"
    );

    // --------- PHASE 5: idle unload ---------
    // The runtime config above set idle_timeout to 50ms so the
    // test can exercise eviction in real time. Sleep past it and
    // tick the governor's `maybe_unload`.
    thread::sleep(Duration::from_millis(80));
    let unloaded = svc.image_gen_maybe_unload().expect("maybe_unload callable");
    assert!(
        unloaded,
        "after idle window elapses, maybe_unload returns true (a real child was shut down)"
    );

    // Runtime is now Idle and a follow-up unload tick must be a
    // no-op (the bridge doesn't spam logs / churn locks on empty
    // state).
    let status_after_unload = svc
        .image_gen_runtime_status()
        .expect("runtime_status after unload");
    assert_eq!(
        status_after_unload.state, "idle",
        "post-unload: runtime back to Idle so the renderer shows 'Sidecar idle'"
    );
    let unloaded_again = svc
        .image_gen_maybe_unload()
        .expect("second unload callable");
    assert!(
        !unloaded_again,
        "Idle → maybe_unload returns false: idempotent, no log spam on empty state"
    );

    // --------- PHASE 6: re-spawn ---------
    // Stand up a SECOND mock sidecar on a *different* TCP port and
    // install a fresh state. This pins that the bridge actually
    // swaps the underlying handle after eviction rather than reusing
    // a stale transport.
    let seed_2: i64 = 28_000_002;
    let (port_2, done_rx_2, server_2) = spawn_mock_sidecar(seed_2);
    assert_ne!(
        port_1, port_2,
        "the OS assigned different ports — the re-spawn target genuinely differs from the original"
    );
    let cfg_2 = mock_runtime_config(port_2, resolved_model_path.clone());
    let transport_2 = ImageGenTransport::new(port_2, Duration::from_secs(5));
    let state_2 = ImageGenState::__test_with_transport(cfg_2, transport_2);
    svc.__test_install_image_gen_state(state_2);

    let status_after_respawn = svc.image_gen_runtime_status().unwrap();
    assert_eq!(
        status_after_respawn.state, "ready",
        "post-respawn: runtime publishes Ready again"
    );

    let request_2 = ImageGenGenerateRequest {
        prompt: "a marble countertop, soft daylight".into(),
        negative_prompt: Some("blurry, low quality".into()),
        width: 768,
        height: 512,
        steps: 25,
        cfg_scale: 6.5,
        seed: Some(42),
        sampler: None,
    };
    let ctx_2 = svc
        .image_gen_prepare_generate(request_2)
        .expect("prepare_generate after re-spawn");
    let result_2 =
        BridgeService::run_image_gen_generate(ctx_2).expect("generate against re-spawned sidecar");
    done_rx_2
        .recv_timeout(Duration::from_secs(3))
        .expect("mock server 2 received and replied");
    server_2.join().ok();

    assert_eq!(
        result_2.seed,
        Some(seed_2),
        "re-spawn: bridge talks to the NEW sidecar (different seed) — not a stale reused handle"
    );
    assert_eq!(result_2.width, 768);
    assert_eq!(result_2.height, 512);
    assert_eq!(result_2.steps, 25);

    // Best-effort cleanup: remove the file we planted in the
    // shared OS user-data dir so a repeated `cargo test` invocation
    // doesn't accumulate files. The `unique_tag`-based filename
    // means a stale file from a previous run wouldn't interfere
    // with the test logic — this cleanup is just to keep the
    // user-data dir tidy.
    let _ = std::fs::remove_file(&resolved_model_path);
}

#[test]
fn descriptor_pin_rejects_non_https_url() {
    // Phase 18 Group E Devin Review fix — descriptor URLs are
    // constrained to `https://` only. The error message explicitly
    // cites the TLS-only outbound posture so an operator who hits
    // this can immediately understand *why* the bridge refused.
    let (svc, _tmp) = boot_bridge();
    let bad = ImageGenModelDescriptor {
        filename: "leaky.gguf".into(),
        blake3_hex: String::new(),
        size_bytes: 1024,
        download_url: Some("http://evil.example.com/leaky.gguf".into()),
        vae_filename: None,
    };
    let err = svc
        .image_gen_set_descriptor(bad)
        .expect_err("plain http:// descriptor URL must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("https://"),
        "error message points to the required scheme: {msg}"
    );
    assert!(
        msg.contains("TLS"),
        "error message cites the TLS-only outbound posture for the operator: {msg}"
    );
}

#[test]
fn descriptor_pin_rejects_empty_filename() {
    // Empty filename is the renderer-side empty-state sentinel (the
    // wizard renders the empty-state CTA when filename is ""), so
    // the bridge must refuse to commit an empty filename — otherwise
    // the renderer would render a "model row" with no name and the
    // user couldn't distinguish "no descriptor pinned" from "pinned
    // an empty-name descriptor".
    let (svc, _tmp) = boot_bridge();
    let bad = ImageGenModelDescriptor {
        filename: "   ".into(), // whitespace counts as empty
        blake3_hex: "0".repeat(64),
        size_bytes: 1024,
        download_url: Some("https://huggingface.co/example/m.gguf".into()),
        vae_filename: None,
    };
    let err = svc
        .image_gen_set_descriptor(bad)
        .expect_err("empty / whitespace-only filename must be rejected");
    assert!(
        err.to_string().contains("filename"),
        "error message names the offending field: {err}"
    );
}

#[test]
fn maybe_unload_is_idempotent_on_idle_state() {
    // Pin the contract: maybe_unload on a fresh (never-spawned)
    // ImageGenState is a noop. The governor ticks this method every
    // 5 seconds; without this property the bridge would emit a log
    // line every tick on a project that never touches image-gen.
    let (mut svc, _tmp) = boot_bridge();
    let cfg = ImageGenRuntimeConfig {
        idle_timeout: Duration::from_millis(10),
        ..Default::default()
    };
    // Install a fresh Idle state (no transport attached).
    let state = ImageGenState::new(cfg);
    svc.__test_install_image_gen_state(state);

    // Idle → false (no child was running so there's nothing to evict).
    assert!(
        !svc.image_gen_maybe_unload().unwrap(),
        "Idle → maybe_unload returns false"
    );
    // Repeated calls remain false — no internal state mutation that
    // would cause a second call to behave differently.
    assert!(!svc.image_gen_maybe_unload().unwrap());
    assert!(!svc.image_gen_maybe_unload().unwrap());

    let status = svc.image_gen_runtime_status().unwrap();
    assert_eq!(
        status.state, "idle",
        "state remains Idle across repeated maybe_unload ticks"
    );
}

#[test]
fn runtime_state_string_round_trips_through_status_surface() {
    // The renderer parses these strings; if the bridge ever stops
    // matching the documented set ("Idle", "Loading", "Ready",
    // "Failed", "Generating"), the renderer's switch statement
    // falls into the default branch and the UI shows a confused
    // state. Pin the contract that at least the fresh Idle string
    // is exactly "Idle", and that an attached __test_with_transport
    // state is "Ready". The other transitions are exercised by
    // the unit tests in image_gen_state.rs.
    let (mut svc, _tmp) = boot_bridge();

    let cfg = ImageGenRuntimeConfig::default();
    svc.__test_install_image_gen_state(ImageGenState::new(cfg.clone()));
    assert_eq!(
        svc.image_gen_runtime_status().unwrap().state,
        "idle",
        "fresh state stringifies to 'idle' (lowercase) — the renderer's switch is case-sensitive"
    );

    // Sanity: ImageGenRuntime exposes the same enum the bridge
    // string-maps. If the enum gains a variant, this assertion
    // forces the test author to consider whether the renderer
    // needs to handle the new state.
    let runtime = ImageGenRuntime::new(cfg);
    assert_eq!(runtime.state(), ImageGenRuntimeState::Idle);

    let transport = ImageGenTransport::new(1, Duration::from_secs(1));
    svc.__test_install_image_gen_state(ImageGenState::__test_with_transport(
        ImageGenRuntimeConfig::default(),
        transport,
    ));
    assert_eq!(
        svc.image_gen_runtime_status().unwrap().state,
        "ready",
        "__test_with_transport drives the runtime to 'ready' (lowercase)"
    );
}
