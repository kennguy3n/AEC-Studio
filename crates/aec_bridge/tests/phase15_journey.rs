//! Phase 15 end-to-end user journey — KChat replatform (loopback +
//! `.kcz` extension + `aecstudio://` deeplinks).
//!
//! Replaces the deleted `phase7_journey.rs`, which exercised the
//! Phase 12 socket transport (`LocalIpcTransport` →
//! `LocalIpcPublisher` → `KChatDiscovery`). Phase 15 removed all
//! three because KChat Desktop's Extension Platform is a JS-only
//! sandbox with no socket listener; the new wire is a loopback HTTP
//! API hosted by Electron + a `.kcz` companion extension inside
//! KChat Desktop. From the Rust side that makes [`KChatState`]
//! *headless* — the bridge keeps the in-process accounting object
//! the rest of the workspace expects (publisher, enable flag,
//! per-project default thread) but the publisher is always
//! [`aec_core::kchat::InMemoryPublisher`]. The Electron host flips
//! `publisher_kind` to `"loopback_http"` via
//! [`crate::kchat_state::KChatState::mark_loopback_active`] when its
//! HTTP server binds.
//!
//! This journey exercises the Phase 15 surface end-to-end through
//! the public [`BridgeService`] API exactly as the Electron host
//! drives it:
//!
//!   1. Boot a fresh `BridgeService`. The cached KChat status is
//!      `disconnected` / `in_memory`, no per-project thread — that's
//!      the empty workspace baseline an Electron boot would observe
//!      before the user opens a project.
//!   2. Create a project from the apartment template. Status still
//!      reports no per-project thread because the template ships
//!      no `KChatConfig`.
//!   3. Write a manifest carrying
//!      [`aec_core::kchat_config::KChatConfig::enabled_with_thread`]
//!      and re-open it. The bridge's `apply_project_config` hook
//!      must surface the thread id in the next `kchat_status()`
//!      snapshot — the renderer's review panel reads exactly this.
//!   4. Promote the publisher kind to `loopback_http` via
//!      `__kchat_state().mark_loopback_active()` — the same call
//!      `apps/desktop/electron/kchat/kchatAppState.ts` issues once
//!      the loopback HTTP server has bound. Status flips to
//!      `connected`.
//!   5. Publish a `concept_render` artifact card through the bridge.
//!      The bridge routes through the in-memory publisher; we assert
//!      the returned `PublishResult` carries an `inmem-N` message id
//!      (the publisher's own sequence prefix) and the publisher
//!      origin thread — proof the card actually traversed the
//!      configured publisher rather than short-circuiting.
//!   6. Disable KChat via `kchat_set_enabled(false)`. Publish must
//!      now fail with `BridgeServiceError::Core` mentioning
//!      `disabled` and the in-memory publisher's history must
//!      remain at exactly the one card from step 5 — disabling is
//!      not a silent drop.
//!   7. Re-enable KChat, publish a `revision_pack` card. The
//!      sequence counter advances to `inmem-2`, proving the
//!      in-memory publisher is reused (not torn down + rebuilt)
//!      across the enable / disable cycle.
//!   8. Ingest reviews. Phase 15's Rust state always returns
//!      `(vec![], vec![])` because the real review-comment ingestion
//!      runs in the Electron loopback handler driven by the `.kcz`
//!      extension — we pin that contract here so a future change
//!      that re-introduces a Rust-side puller will trip this test.
//!   9. Open a *second* project whose manifest omits the kchat
//!      block. `apply_project_config(None)` must clear the thread
//!      id cached from project #1, otherwise the Deliver page would
//!      keep routing comments to the previous project's thread.
//!  10. Demote the publisher kind via `mark_loopback_inactive()`.
//!      Status reports `disconnected` / `in_memory` again — the
//!      same shape an Electron shutdown would leave the bridge in.
//!
//! This is the canonical Phase 15 acceptance test: every step is a
//! contract the Electron host or the renderer relies on. A break
//! here is an immediate Phase 15 regression, not just a unit-test
//! tweak.

use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService};
use aec_core::kchat::{ArtifactCard, KChatArtifact};
use aec_core::kchat_config::KChatConfig;
use aec_core::ProjectManifest;

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
            "failed to copy shipped template {} -> {}: {e}",
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
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, [0xF1u8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

fn make_card(caption: &str, kind: KChatArtifact) -> ArtifactCard {
    ArtifactCard {
        artifact: kind,
        caption: caption.into(),
        project_link: "aecstudio://project/phase15/journey/anchor".into(),
        thumbnail_blake3: None,
        metadata: std::collections::HashMap::new(),
    }
}

#[test]
fn phase15_loopback_replatform_journey() {
    let (mut s, _tmp) = boot_service();

    // === Step 1: empty workspace baseline. ===
    let baseline = s.kchat_status();
    assert_eq!(
        baseline.state, "disconnected",
        "fresh bridge with no loopback signal must report disconnected"
    );
    assert_eq!(
        baseline.publisher_kind, "in_memory",
        "Phase 15 bridge starts on the in-memory publisher; loopback is opt-in"
    );
    assert!(
        baseline.default_thread_id.is_none(),
        "no project open => no per-project thread"
    );
    assert!(
        baseline.instance.is_none(),
        "Phase 15 surfaces the loopback snapshot via the Electron IPC channel, not here"
    );

    // === Step 2: create project — no kchat config yet. ===
    let summary = s
        .project_create_from_template("interior.apartment", "Phase15 Journey")
        .expect("create project");
    assert!(
        s.kchat_status().default_thread_id.is_none(),
        "template ships no KChatConfig => no per-project thread"
    );

    // === Step 3: amend manifest + reopen — bridge surfaces the
    //     per-project thread. ===
    let manifest_path = Path::new(&summary.path).join("manifest.json");
    let mut manifest: ProjectManifest =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest.settings.kchat = Some(KChatConfig::enabled_with_thread("phase15-thread"));
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    s.project_open(&summary.path).expect("reopen project");
    let after_reopen = s.kchat_status();
    assert_eq!(
        after_reopen.default_thread_id,
        Some("phase15-thread".to_string()),
        "project_open must thread KChatConfig::default_thread_id into kchat_status"
    );

    // === Step 4: Electron host signals the loopback API has bound. ===
    s.__kchat_state().mark_loopback_active();
    let promoted = s.kchat_status();
    assert_eq!(promoted.publisher_kind, "loopback_http");
    assert_eq!(
        promoted.state, "connected",
        "loopback-active + enabled => connected (matches the renderer's status chip)"
    );
    assert_eq!(
        promoted.default_thread_id,
        Some("phase15-thread".to_string()),
        "promoting the publisher kind must not drop the per-project thread"
    );

    // === Step 5: publish a card; assert the publisher actually
    //     handled it (in-memory sequence prefix). ===
    let first = s
        .kchat_publish(make_card(
            "Concept renders v1",
            KChatArtifact::ConceptRender,
        ))
        .expect("publish first card");
    assert_eq!(
        first.message_id, "inmem-1",
        "in-memory publisher mints `inmem-<seq>`; first publish is seq 1"
    );
    assert_eq!(
        first.thread_id, "aecstudio-fallback",
        "Phase 15's in-memory publisher origin is `aecstudio-fallback`"
    );

    // === Step 6: disable KChat — publish must fail with a
    //     `disabled`-flavoured error and the publisher must remain
    //     untouched (no silent drops). ===
    s.kchat_set_enabled(false);
    assert!(!s.kchat_is_enabled());
    let err = s
        .kchat_publish(make_card("blocked publish", KChatArtifact::ConceptRender))
        .expect_err("publish after disable must error");
    let msg = format!("{err:?}");
    assert!(
        msg.to_lowercase().contains("disabled"),
        "disabled publish must surface a `disabled`-flavoured error (got: {msg})"
    );

    // === Step 7: re-enable; sequence counter advances, proving
    //     the publisher is reused rather than rebuilt. ===
    s.kchat_set_enabled(true);
    assert!(s.kchat_is_enabled());
    let second = s
        .kchat_publish(make_card("Revision pack v2", KChatArtifact::RevisionPack))
        .expect("publish second card");
    assert_eq!(
        second.message_id, "inmem-2",
        "the in-memory publisher is reused across enable/disable cycles"
    );

    // === Step 8: ingest reviews — always empty on the Rust side.
    //     The real review chain runs through the Electron loopback
    //     handler driven by the `.kcz` extension. ===
    let report = s
        .kchat_ingest_reviews("phase15-thread", None)
        .expect("ingest reviews");
    assert_eq!(report.thread_id, "phase15-thread");
    assert!(
        report.comments.is_empty(),
        "Phase 15 Rust state never produces comments; the Electron loopback handler does"
    );
    assert!(
        report.cards.is_empty(),
        "Phase 15 Rust state never produces review cards"
    );

    // === Step 9: switch projects — clearing the per-project
    //     thread when the new manifest omits the kchat block is a
    //     correctness invariant for the Deliver page. ===
    let second_proj = s
        .project_create_from_template("interior.apartment", "Phase15 Journey - Other")
        .expect("create second project");
    s.project_open(&second_proj.path).expect("open second");
    assert!(
        s.kchat_status().default_thread_id.is_none(),
        "opening a project without kchat must clear the prior project's cached thread"
    );

    // === Step 10: Electron shutdown — demote the publisher kind
    //     back to the headless default. ===
    s.__kchat_state().mark_loopback_inactive();
    let final_status = s.kchat_status();
    assert_eq!(final_status.publisher_kind, "in_memory");
    assert_eq!(
        final_status.state, "disconnected",
        "loopback-inactive => disconnected regardless of enabled flag"
    );
}
