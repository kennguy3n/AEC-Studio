//! Phase 7 end-to-end user journey — "KChat collaboration"
//! (Phase 12 Task 30). Driven through the `BridgeService` public API
//! against a mock UNIX-domain-socket KChat Desktop server.
//!
//! Steps:
//!
//!   1. Create a project from the apartment template.
//!   2. Enqueue a render so we have a job id to anchor the artifact
//!      card on (the journey doesn't need the render to *complete* —
//!      the card just needs to point to a job that exists).
//!   3. Spin up a UNIX-domain-socket mock server speaking the KChat
//!      Desktop IPC envelope (`ping` / `publish` / `ingest_reviews`).
//!      Install it on the bridge via `__test_force_local_ipc` so the
//!      `kchat_publish` / `kchat_ingest_reviews` calls go over the
//!      real `LocalIpcPublisher` → `LocalIpcTransport` path.
//!   4. Publish a `concept_render` artifact card. Assert the
//!      `PublishResult` carries the server-issued `message_id` and
//!      thread id — that's the audit-trail anchor.
//!   5. Ingest 3 review comments from the mock server. Pass them
//!      through `aec_core::kchat_sync::CommentSync` to produce
//!      `ProjectAuditEntry` rows with `actor.kind = ActorKind::KChat`
//!      — exactly what the bridge would persist to the project audit
//!      log.
//!   6. Disable KChat via `kchat_set_enabled(false)` and verify every
//!      publish / ingest call returns `KChatError::Disabled` *without*
//!      contacting the server.
//!   7. Re-enable KChat, publish a `revision_pack` card, and verify
//!      it goes through. Re-ingest the same 3 comments and verify
//!      `CommentSync` dedupes them so the audit log doesn't grow.
//!
//! Mock-server justification: the journey can't talk to a real KChat
//! Desktop on CI (the binary isn't installed and the platform-specific
//! discovery paths point at user-only locations). The mock speaks the
//! same line-oriented JSON envelope as the real server, so the bridge
//! exercises the production transport (line framing, request
//! discrimination, error envelope handling) exactly as it would in
//! production — the only thing mocked is the server's response body.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use aec_bridge::{BridgeConfig, BridgeService};
use aec_core::kchat::{ArtifactCard, KChatArtifact, KChatError, ReviewCard, ReviewComment};
use aec_core::kchat_sync::CommentSync;
use aec_core::types::ActorKind;
use chrono::TimeZone;

fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

fn copy_template(category: &str, id: &str, dest: &Path) {
    let src = workspace_templates_dir()
        .join(category)
        .join(format!("{id}.json"));
    let dest_dir = dest.join(category);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest_file = dest_dir.join(format!("{id}.json"));
    std::fs::copy(&src, &dest_file).unwrap_or_else(|e| {
        panic!(
            "failed to copy template {} -> {}: {e}",
            src.display(),
            dest_file.display()
        )
    });
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_template("interior", "apartment", &templates);
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let svc = BridgeService::new(cfg, [0x77u8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

/// Track of every request line the mock server has received, in
/// the order it received them. Shared with the test so assertions
/// can inspect what the bridge actually sent.
type RequestLog = Arc<Mutex<Vec<String>>>;

struct MockKChatServer {
    socket_path: PathBuf,
    requests: RequestLog,
    shutdown_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
    _tmp: tempfile::TempDir,
}

impl MockKChatServer {
    /// Spawn a mock server listening on a freshly-created socket
    /// path. Handles one connection at a time; each request line is
    /// parsed as JSON, the `kind` tag chosen, and a canned reply
    /// emitted.
    fn spawn() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
        let log = Arc::clone(&requests);
        let handle = std::thread::spawn(move || serve(listener, log, shutdown_rx));
        Self {
            socket_path: sock,
            requests,
            shutdown_tx,
            handle: Some(handle),
            _tmp: tmp,
        }
    }
}

impl Drop for MockKChatServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve(listener: UnixListener, requests: RequestLog, shutdown: mpsc::Receiver<()>) {
    loop {
        if shutdown.try_recv().is_ok() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                handle_client(stream, &requests);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("mock kchat accept error: {e}");
                return;
            }
        }
    }
}

fn handle_client(stream: UnixStream, requests: &RequestLog) {
    let mut writer = stream.try_clone().unwrap();
    writer
        .set_write_timeout(Some(std::time::Duration::from_secs(2)))
        .ok();
    writer
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .ok();
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        requests.lock().unwrap().push(line.clone());
        let parsed: serde_json::Value =
            serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
        let kind = parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let reply = match kind {
            "ping" => serde_json::json!({"kind": "pong", "version": "9.9.9-mock"}),
            "publish" => serde_json::json!({
                "kind": "publish_ok",
                "result": {
                    "message_id": format!("msg-{}", requests.lock().unwrap().len()),
                    "thread_id": "phase7-thread",
                    "published_at": "2026-05-27T00:00:00Z",
                },
            }),
            "ingest_reviews" => serde_json::json!({
                "kind": "ingest_ok",
                "comments": mock_review_comments(),
                "cards": Vec::<serde_json::Value>::new(),
            }),
            _ => serde_json::json!({"kind": "error", "message": format!("unknown kind `{kind}`")}),
        };
        let line_bytes = format!("{reply}\n");
        if writer.write_all(line_bytes.as_bytes()).is_err() {
            break;
        }
    }
}

fn mock_review_comments() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "thread_id": "phase7-thread",
            "commenter": "@alice",
            "text": "looks great — approve",
            "timestamp": "2026-05-27T10:00:00Z",
        }),
        serde_json::json!({
            "thread_id": "phase7-thread",
            "commenter": "@bob",
            "text": "can you tweak the lighting on the kitchen render?",
            "timestamp": "2026-05-27T10:05:00Z",
        }),
        serde_json::json!({
            "thread_id": "phase7-thread",
            "commenter": "@carol",
            "text": "please re-render at high quality",
            "timestamp": "2026-05-27T10:10:00Z",
        }),
    ]
}

fn artifact_card(artifact: KChatArtifact, caption: &str) -> ArtifactCard {
    ArtifactCard {
        artifact,
        caption: caption.into(),
        project_link: "aecstudio://project/phase7/r/0".into(),
        thumbnail_blake3: None,
        metadata: std::collections::HashMap::new(),
    }
}

#[test]
fn phase7_journey_kchat_publish_ingest_disable_reenable() {
    // ── Step 1: create project ──
    let (mut svc, _g) = boot_service();
    let _summary = svc
        .project_create_from_template("interior.apartment", "Phase 7 KChat Journey")
        .expect("create project");

    // ── Step 2: enqueue a render so we have something to publish ──
    let enqueue = svc
        .render_enqueue("cam_phase7", "standard", 0, None)
        .expect("render_enqueue");
    assert!(
        !enqueue.job_id.is_empty(),
        "render_enqueue must produce a job id"
    );

    // ── Step 3: install the mock IPC server on the kchat state ──
    let mock = MockKChatServer::spawn();
    svc.__kchat_state()
        .__test_force_local_ipc(mock.socket_path.clone());

    // The status indicator must now report the local_ipc publisher
    // kind. The `state` field depends on whether the well-known
    // discovery path resolves — we deliberately don't pin it here
    // because the mock socket lives in a temp dir, not at the
    // discovery probe's canonical location, so a fresh `status()`
    // call may legitimately report `reconnecting`. The renderer
    // surfaces both kind+state separately; pinning kind is what
    // matters for the journey.
    let status = svc.kchat_status();
    assert_eq!(status.publisher_kind, "local_ipc");
    assert!(
        matches!(status.state.as_str(), "connected" | "reconnecting"),
        "kchat_status must report either connected (probe found a socket) or reconnecting (probe couldn't, but the publisher is still installed); got `{}`",
        status.state
    );

    // ── Step 4: publish a concept_render artifact card ──
    let card = artifact_card(KChatArtifact::ConceptRender, "Living room — concept");
    let result = svc.kchat_publish(card.clone()).expect("kchat_publish");
    assert_eq!(
        result.thread_id, "phase7-thread",
        "publisher must echo the server-assigned thread id"
    );
    assert!(
        result.message_id.starts_with("msg-"),
        "server-issued message id must round-trip back to the caller (got `{}`)",
        result.message_id
    );

    // ── Step 5: ingest 3 review comments ──
    let ingest = svc
        .kchat_ingest_reviews("phase7-thread", None)
        .expect("kchat_ingest_reviews");
    assert_eq!(
        ingest.comments.len(),
        3,
        "mock server returns exactly 3 comments"
    );

    // Pipe the comments through CommentSync to produce audit
    // entries — this is what the bridge does to convert ingested
    // comments into rows for the project audit log.
    let mut sync = CommentSync::new();
    let audit_entries = sync.ingest_batch(ingest.comments.clone());
    assert_eq!(
        audit_entries.len(),
        3,
        "first ingest of 3 comments must produce 3 audit entries"
    );
    for entry in &audit_entries {
        assert_eq!(
            entry.actor.kind,
            ActorKind::KChat,
            "audit entries from the kchat publisher must carry actor.kind = KChat"
        );
    }

    // ── Step 6: disable KChat → publish / ingest must refuse ──
    svc.kchat_set_enabled(false);
    assert!(!svc.kchat_is_enabled(), "set_enabled(false) must stick");
    let pub_err = svc.kchat_publish(card.clone()).expect_err(
        "disabled kchat must refuse publish — silently succeeding would mask misconfiguration",
    );
    match pub_err {
        aec_bridge::BridgeServiceError::Core(msg) => {
            assert!(
                msg.contains("disabled"),
                "publish error while disabled must surface the KChatError::Disabled label (got `{msg}`)"
            );
        }
        e => panic!("unexpected error variant on disabled publish: {e:?}"),
    }
    let ingest_err = svc
        .kchat_ingest_reviews("phase7-thread", None)
        .expect_err("disabled kchat must refuse ingest");
    match ingest_err {
        aec_bridge::BridgeServiceError::Core(msg) => {
            assert!(
                msg.contains("disabled"),
                "ingest error while disabled must surface the KChatError::Disabled label (got `{msg}`)"
            );
        }
        e => panic!("unexpected error variant on disabled ingest: {e:?}"),
    }

    // Confirm via the underlying KChatError surface too — the
    // bridge wraps it in Core(...), but the underlying call returns
    // the typed variant.
    let direct_err = svc.__kchat_state().publish(card.clone()).unwrap_err();
    assert!(matches!(direct_err, KChatError::Disabled));

    // ── Step 7: re-enable, publish a revision pack, re-ingest comments ──
    svc.kchat_set_enabled(true);
    assert!(svc.kchat_is_enabled());

    let revision_card = artifact_card(KChatArtifact::RevisionPack, "Rev pack v2");
    let result2 = svc
        .kchat_publish(revision_card)
        .expect("re-enabled kchat_publish");
    assert_eq!(result2.thread_id, "phase7-thread");

    // Re-ingest the same 3 comments — the existing CommentSync must
    // dedup them so the audit log doesn't double up.
    let second_pass = sync.ingest_batch(ingest.comments.clone());
    assert!(
        second_pass.is_empty(),
        "re-ingest of identical comments must dedup (got {} new entries)",
        second_pass.len()
    );
    assert_eq!(
        sync.seen_count(),
        3,
        "dedup state must still contain exactly the 3 original keys"
    );

    // CommentSync rebuilt from audit entries also dedupes — i.e.
    // a fresh session that loads the audit log will not re-record
    // any of the existing comments.
    let rebuilt = CommentSync::from_existing_entries(audit_entries.iter());
    assert_eq!(rebuilt.seen_count(), 3);
    let mut rebuilt = rebuilt;
    let after_restart = rebuilt.ingest_batch(ingest.comments.clone());
    assert!(
        after_restart.is_empty(),
        "rehydrated CommentSync must also dedup the same comments after restart"
    );

    // Inspect what the mock server actually received. We expect
    // (in order):
    //   1) ping (from kchat_status discovery refresh)
    //   2) publish (concept render)
    //   3) ingest_reviews
    //   4) publish (revision pack, after re-enable)
    // The disabled-state calls must NOT have hit the wire.
    let log = mock.requests.lock().unwrap();
    let kinds: Vec<&str> = log
        .iter()
        .map(|s| {
            serde_json::from_str::<serde_json::Value>(s)
                .ok()
                .and_then(|v| {
                    v.get("kind")
                        .and_then(|k| k.as_str())
                        .map(|s| Box::leak(s.to_string().into_boxed_str()) as &str)
                })
                .unwrap_or("(unparsable)")
        })
        .collect();
    let publish_count = kinds.iter().filter(|k| **k == "publish").count();
    let ingest_count = kinds.iter().filter(|k| **k == "ingest_reviews").count();
    assert_eq!(
        publish_count, 2,
        "expected 2 publishes (concept + revision); disabled publish must not have hit the wire (got {kinds:?})"
    );
    assert_eq!(
        ingest_count, 1,
        "expected exactly 1 ingest_reviews; disabled ingest must not have hit the wire (got {kinds:?})"
    );
}

#[test]
fn phase7_journey_disabled_blocks_publish_without_mock() {
    // A leaner counterpart to the main journey: a bridge with no
    // mock server installed (in-memory fallback) still refuses
    // publish / ingest when disabled. This shows the disable gate
    // sits *above* the publisher selection, so it cannot be
    // bypassed by toggling the publisher kind.
    let (svc, _g) = boot_service();
    svc.kchat_set_enabled(false);

    let card = ArtifactCard {
        artifact: KChatArtifact::ConceptRender,
        caption: "leaner journey".into(),
        project_link: "aecstudio://project/x/r/y".into(),
        thumbnail_blake3: None,
        metadata: std::collections::HashMap::new(),
    };
    let err = svc.__kchat_state().publish(card).unwrap_err();
    assert!(matches!(err, KChatError::Disabled));
    let err2 = svc
        .__kchat_state()
        .ingest_reviews("any-thread", None)
        .unwrap_err();
    assert!(matches!(err2, KChatError::Disabled));

    // Sanity — the mock-less in-memory fallback IS responsive
    // after re-enabling.
    svc.kchat_set_enabled(true);
    let card2 = ArtifactCard {
        artifact: KChatArtifact::ConceptRender,
        caption: "after re-enable".into(),
        project_link: "aecstudio://project/x/r/y".into(),
        thumbnail_blake3: None,
        metadata: std::collections::HashMap::new(),
    };
    let ok = svc.__kchat_state().publish(card2).unwrap();
    assert!(!ok.message_id.is_empty());
    // Ingest on the in-memory fallback is a no-op (the LocalIpc
    // path isn't installed), but it must succeed when enabled.
    let (comments, cards) = svc.__kchat_state().ingest_reviews("any", None).unwrap();
    assert!(comments.is_empty());
    assert!(cards.is_empty());
    let _ = (
        ReviewComment {
            // ensure the symbol is exercised
            thread_id: "x".into(),
            commenter: "y".into(),
            text: "z".into(),
            timestamp: chrono::Utc.with_ymd_and_hms(2026, 5, 27, 0, 0, 0).unwrap(),
            artifact_ref: None,
        },
        ReviewCard {
            comment: ReviewComment {
                thread_id: "x".into(),
                commenter: "y".into(),
                text: "z".into(),
                timestamp: chrono::Utc.with_ymd_and_hms(2026, 5, 27, 0, 0, 0).unwrap(),
                artifact_ref: None,
            },
            status: aec_core::kchat::ApprovalStatus::Approved,
        },
    );
}
