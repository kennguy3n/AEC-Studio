//! Process-wide KChat state held on [`crate::service::BridgeService`].
//!
//! ## Phase 15 transport replatform
//!
//! Phase 12 shipped a UNIX-socket / Windows-named-pipe transport
//! between AEC Studio and KChat Desktop. KChat Desktop's Extension
//! Platform turned out to be a JS-only sandbox with no socket
//! listener — the same conclusion KCreate and Tessera reached — so
//! Phase 15 replaced the socket transport with a loopback HTTP API
//! served from the Electron main process (see
//! `apps/desktop/electron/kchat/kchatLocalApi.ts`). The `.kcz`
//! companion extension installed inside KChat Desktop reads
//! `{userData}/aec-kchat-port.json` and posts artefact cards back
//! to the Electron loopback API; the renderer's `kchat:*` IPC
//! channels also go straight to the Electron loopback state
//! (`apps/desktop/electron/kchat/kchatAppState.ts`).
//!
//! That move makes the Rust-side `KChatState` *headless*: it is no
//! longer the transport, it is only the in-process accounting
//! object the bridge keeps for tests, journey tests, and the
//! `aec_bridge` consumers that don't run inside Electron (CLI
//! tools, integration tests). The publisher it owns is always an
//! [`InMemoryPublisher`] — the loopback HTTP queue lives outside
//! this crate.
//!
//! ## Surface kept stable
//!
//! The public methods (`status`, `reload`, `publish`,
//! `ingest_reviews`, `apply_project_config`, `clear_project_config`,
//! `set_enabled`, `is_enabled`, `default_thread_id`) all keep
//! their Phase 12 shapes so the bridge service, the napi exports,
//! and the renderer fallback path don't need to change. What did
//! change is the *semantics*:
//!
//! - `KChatStatusReport::publisher_kind` is now either
//!   `"loopback_http"` (set when the bridge boots inside the
//!   Electron host that hosts the loopback API — currently signalled
//!   by [`KChatState::mark_loopback_active`]) or `"in_memory"`
//!   (every other context). The `"local_ipc"` variant is gone.
//! - `KChatStatusReport::instance` is `None`. The Electron-side
//!   loopback snapshot (port, port-file path, extension heartbeat,
//!   queue depth) is surfaced via the `kchat:status` IPC channel
//!   directly from the Electron main process; the Rust state does
//!   not have, and does not need, visibility into it.
//! - The probe-cooldown / discovery-throttle machinery is gone —
//!   there is nothing to probe.

use std::sync::{Arc, RwLock};

use aec_core::kchat::{
    ArtifactCard, InMemoryPublisher, KChatError, KChatPublisher, PublishResult, ReviewCard,
    ReviewComment,
};
use aec_core::kchat_config::KChatConfig;
use serde::{Deserialize, Serialize};

/// Snapshot returned by [`KChatState::status`]. The renderer-side
/// `KChatStatusIndicator` mirrors this shape over the
/// `kchat:status` IPC channel; adding a field here is a breaking
/// change at the IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KChatStatusReport {
    /// One of `connected` (the Electron host has signalled the
    /// loopback API is live via
    /// [`KChatState::mark_loopback_active`] and the integration is
    /// enabled), or `disconnected` (no loopback signal and / or
    /// disabled). The intermediate `reconnecting` state from Phase
    /// 12 is gone — there is no transport to reconnect.
    pub state: String,
    /// Publisher kind currently in use — `loopback_http` when the
    /// Electron host has surfaced the loopback API to this
    /// process, or `in_memory` for tests / CLI runs / journey
    /// tests. Surfaced so the Settings page can show
    /// "Loopback HTTP API on 127.0.0.1:N" vs "Local-only fallback".
    pub publisher_kind: String,
    /// Phase 12 carried discovery info here (socket path, version,
    /// health). In Phase 15 the loopback-API snapshot lives in the
    /// Electron main process, so this field is always `None` on the
    /// Rust side. The renderer reads the loopback snapshot via the
    /// `kchat:status` IPC channel directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<serde_json::Value>,
    /// Per-project KChat thread the active project routes publishes
    /// and review-comment ingestion to. Mirrors
    /// [`KChatConfig::default_thread_id`] from the open project's
    /// manifest, surfaced through the status payload so the renderer
    /// can wire its review panel without an extra IPC roundtrip.
    ///
    /// `None` either means no project is open yet or the open
    /// project hasn't picked a thread — callers fall back to
    /// [`aec_core::DEFAULT_THREAD_ID`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thread_id: Option<String>,
    /// Master enable switch mirroring
    /// [`aec_core::kchat_config::KChatConfig::enabled`]. The
    /// Electron `kchat:publish` IPC handler reads this through
    /// `BridgeService::kchat_is_enabled` *before* enqueueing into
    /// the loopback HTTP queue and refuses the publish with a
    /// structured `disabled` error when `false`. Surfaced through
    /// the status payload so the renderer's Settings card and
    /// status chip stay in sync with the bridge without an extra
    /// IPC roundtrip. The Rust-side default is `true` (matches
    /// `KChatConfig::default`); per-project manifests overwrite it
    /// via [`KChatState::apply_project_config`] and the Settings
    /// toggle drives it via [`KChatState::set_enabled`].
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

pub struct KChatState {
    inner: RwLock<Inner>,
}

impl std::fmt::Debug for KChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The trait-object publisher inside `Inner` is not `Debug`,
        // so we surface a hand-rolled summary instead of deriving.
        let inner = self.inner.read().expect("kchat state not poisoned");
        f.debug_struct("KChatState")
            .field("publisher_kind", &inner.publisher_kind)
            .field("state", &inner.last_status.state)
            .finish_non_exhaustive()
    }
}

struct Inner {
    /// Always an `InMemoryPublisher` in Phase 15 — the loopback API
    /// publisher lives in the Electron main process and is not
    /// reachable from `aec_bridge`. The trait object is kept so the
    /// `publish` method stays generic in case a future phase wires a
    /// Rust-side HTTP client to the loopback API.
    publisher: Arc<dyn KChatPublisher + Send + Sync>,
    publisher_kind: PublisherKind,
    last_status: KChatStatusReport,
    /// Master enable switch mirroring
    /// [`aec_core::kchat_config::KChatConfig::enabled`]. When
    /// `false` every publish / ingest call returns
    /// [`KChatError::Disabled`] *before* touching the publisher.
    enabled: bool,
    /// Cached per-project thread id mirroring
    /// [`KChatConfig::default_thread_id`] from the currently open
    /// project's manifest. Refreshed by
    /// [`KChatState::apply_project_config`] on each `project_open`
    /// / `project_save` so the renderer's status poll surfaces the
    /// right thread the moment a project is active. `None` outside
    /// of an open project (or when the project chose to leave
    /// `default_thread_id` unset).
    default_thread_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublisherKind {
    InMemory,
    LoopbackHttp,
}

impl PublisherKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::LoopbackHttp => "loopback_http",
        }
    }
}

impl Default for KChatState {
    fn default() -> Self {
        Self::new()
    }
}

impl KChatState {
    /// Build a fresh state object. The Rust-side publisher is
    /// always an [`InMemoryPublisher`] in Phase 15 — the Electron
    /// host promotes the kind to `loopback_http` via
    /// [`Self::mark_loopback_active`] after the loopback API has
    /// bound on `127.0.0.1`.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::initial()),
        }
    }

    /// Snapshot the current connection status. Cheap read-only
    /// access — no I/O, no socket probes (there is no socket in
    /// Phase 15). The renderer's 5 s status poll therefore costs
    /// only a read-lock acquire.
    pub fn status(&self) -> KChatStatusReport {
        self.inner
            .read()
            .expect("kchat state not poisoned")
            .last_status
            .clone()
    }

    /// Re-emit the cached status report. Phase 12 used this to
    /// re-run socket discovery and rebuild the publisher; Phase 15
    /// has no transport to rebuild, so this returns the current
    /// snapshot unchanged. The Settings page's "Reload KChat
    /// connection" button still calls through to this method so the
    /// IPC contract stays stable; on the Electron side, the
    /// `kchat:reload` IPC handler also re-emits the loopback
    /// snapshot rather than restarting the HTTP server.
    pub fn reload(&self) -> KChatStatusReport {
        self.status()
    }

    /// Promote the publisher kind to `loopback_http`. Called by the
    /// Electron host once `kchatLocalApi` has bound on `127.0.0.1`
    /// and written the port file. Demoting back to `in_memory` is
    /// done via [`Self::mark_loopback_inactive`] (e.g. on Electron
    /// shutdown).
    ///
    /// Idempotent — repeated calls with the same state are no-ops.
    pub fn mark_loopback_active(&self) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        if inner.publisher_kind == PublisherKind::LoopbackHttp {
            return;
        }
        inner.publisher_kind = PublisherKind::LoopbackHttp;
        inner.last_status.publisher_kind = PublisherKind::LoopbackHttp.as_str().into();
        inner.last_status.state = if inner.enabled {
            "connected".into()
        } else {
            "disconnected".into()
        };
    }

    /// Demote the publisher kind back to `in_memory`. Called by the
    /// Electron host on shutdown so the next status snapshot
    /// surfaces the headless state honestly.
    pub fn mark_loopback_inactive(&self) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        if inner.publisher_kind == PublisherKind::InMemory {
            return;
        }
        inner.publisher_kind = PublisherKind::InMemory;
        inner.last_status.publisher_kind = PublisherKind::InMemory.as_str().into();
        inner.last_status.state = "disconnected".into();
    }

    /// Publish an artifact card through the in-process publisher.
    /// Returns [`KChatError::Disabled`] *before* touching the
    /// publisher when the integration has been disabled via
    /// [`Self::set_enabled`] or via the project manifest.
    pub fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError> {
        let publisher = {
            let inner = self.inner.read().expect("kchat state not poisoned");
            if !inner.enabled {
                return Err(KChatError::Disabled);
            }
            Arc::clone(&inner.publisher)
        };
        publisher.publish(card)
    }

    /// Poll new review comments. In Phase 15 the Rust-side
    /// publisher is always in-memory; review-comment ingestion is
    /// driven by the Electron loopback API (which the `.kcz`
    /// extension feeds from KChat Desktop). This method therefore
    /// always returns `(vec![], vec![])` once the disabled-state
    /// check has cleared.
    pub fn ingest_reviews(
        &self,
        _thread_id: &str,
        _since_iso: Option<String>,
    ) -> Result<(Vec<ReviewComment>, Vec<ReviewCard>), KChatError> {
        let inner = self.inner.read().expect("kchat state not poisoned");
        if !inner.enabled {
            return Err(KChatError::Disabled);
        }
        Ok((Vec::new(), Vec::new()))
    }

    /// Master enable / disable switch. Setting this to `false`
    /// causes every subsequent [`Self::publish`] and
    /// [`Self::ingest_reviews`] call to return
    /// [`KChatError::Disabled`] without touching the publisher.
    /// Idempotent — flipping back to `true` resumes through the
    /// already-installed publisher.
    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.enabled = enabled;
        inner.last_status.enabled = enabled;
        // The cached state string follows the enabled flag so the
        // renderer's chip flips to "disconnected" the moment a
        // project disables KChat, without waiting for the next
        // status poll to re-derive it.
        inner.last_status.state = match (inner.enabled, inner.publisher_kind) {
            (true, PublisherKind::LoopbackHttp) => "connected".into(),
            _ => "disconnected".into(),
        };
    }

    /// Mirror of [`Self::set_enabled`] — useful for the Settings
    /// page to reflect the current toggle state without re-reading
    /// from the underlying project config.
    pub fn is_enabled(&self) -> bool {
        self.inner.read().expect("kchat state not poisoned").enabled
    }

    /// Adopt the per-project [`KChatConfig`] for the currently
    /// active project. Called by [`crate::service::BridgeService`]
    /// after every `project_open` / `project_save` so the bridge's
    /// shared state mirrors the just-opened manifest.
    ///
    /// Applies two fields:
    ///
    /// - `enabled` — flipped onto [`Inner::enabled`] so subsequent
    ///   publishes respect the per-project toggle. This is the same
    ///   bit [`Self::set_enabled`] flips; we route through both so
    ///   the Settings toggle and project-open both converge on the
    ///   same observable state.
    /// - `default_thread_id` — cached so [`Self::status`] surfaces
    ///   it to the renderer. The Deliver page's review panel reads
    ///   this field and falls back to the publisher-side default
    ///   only when it's `None`.
    ///
    /// Idempotent — repeated calls with the same config produce
    /// the same `last_status`.
    pub fn apply_project_config(&self, config: &KChatConfig) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.enabled = config.enabled;
        inner.last_status.enabled = config.enabled;
        inner
            .default_thread_id
            .clone_from(&config.default_thread_id);
        inner
            .last_status
            .default_thread_id
            .clone_from(&config.default_thread_id);
        inner.last_status.state = match (inner.enabled, inner.publisher_kind) {
            (true, PublisherKind::LoopbackHttp) => "connected".into(),
            _ => "disconnected".into(),
        };
    }

    /// Drop any per-project state cached from a prior
    /// [`Self::apply_project_config`] call. Used when closing a
    /// project (or opening one with no `KChatConfig` set in the
    /// manifest) so the renderer's review panel falls back to the
    /// publisher-side default thread instead of pointing at a
    /// stale project's thread.
    pub fn clear_project_config(&self) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.default_thread_id = None;
        inner.last_status.default_thread_id = None;
    }

    /// Mirror of [`KChatConfig::default_thread_id`] for the
    /// currently open project. Returned by [`Self::status`] as
    /// `default_thread_id`; exposed separately so tests can assert
    /// without re-running discovery.
    pub fn default_thread_id(&self) -> Option<String> {
        self.inner
            .read()
            .expect("kchat state not poisoned")
            .default_thread_id
            .clone()
    }

    /// Test helper — force the state into an in-memory publisher
    /// pointed at the supplied origin tag. Kept under `__test_`
    /// so it's clearly out of the public API surface.
    #[doc(hidden)]
    pub fn __test_force_in_memory(&self, origin: &str) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.publisher = Arc::new(InMemoryPublisher::new(origin));
        inner.publisher_kind = PublisherKind::InMemory;
        let enabled = inner.enabled;
        inner.last_status = KChatStatusReport {
            state: "disconnected".into(),
            publisher_kind: PublisherKind::InMemory.as_str().into(),
            instance: None,
            default_thread_id: inner.default_thread_id.clone(),
            enabled,
        };
    }
}

impl Inner {
    fn initial() -> Self {
        let publisher: Arc<dyn KChatPublisher + Send + Sync> =
            Arc::new(InMemoryPublisher::new("aecstudio-fallback"));
        Self {
            publisher,
            publisher_kind: PublisherKind::InMemory,
            last_status: KChatStatusReport {
                state: "disconnected".into(),
                publisher_kind: PublisherKind::InMemory.as_str().into(),
                instance: None,
                default_thread_id: None,
                enabled: true,
            },
            enabled: true,
            default_thread_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::kchat::KChatArtifact;

    fn card() -> ArtifactCard {
        ArtifactCard {
            artifact: KChatArtifact::ConceptRender,
            caption: "kchat-state test card".into(),
            project_link: "aecstudio://project/x/r/y".into(),
            thumbnail_blake3: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    /// A fresh state must report the in-memory fallback (no
    /// Electron host has signalled the loopback API yet).
    #[test]
    fn default_starts_in_memory_until_loopback_marks_active() {
        let st = KChatState::new();
        let s = st.status();
        assert_eq!(s.publisher_kind, "in_memory");
        assert_eq!(s.state, "disconnected");
        assert!(s.instance.is_none());
        // Default starts enabled; the Settings toggle / per-project
        // config flip this off explicitly. The Electron
        // `kchat:publish` handler reads this field through the
        // status payload to gate enqueue decisions, so an
        // unintentional default of `false` would break publishing
        // on every fresh install.
        assert!(s.enabled);
    }

    /// The `enabled` field on the status payload must follow
    /// [`KChatState::set_enabled`] so the Electron `kchat:publish`
    /// gate observes the same flag the Settings toggle just
    /// flipped without a per-call bridge round-trip.
    #[test]
    fn status_enabled_field_follows_set_enabled() {
        let st = KChatState::new();
        assert!(st.status().enabled);
        st.set_enabled(false);
        assert!(!st.status().enabled);
        st.set_enabled(true);
        assert!(st.status().enabled);
    }

    /// `apply_project_config` adopts the manifest's `enabled` flag
    /// *and* mirrors it onto the status payload so a fresh
    /// `project_open` flips the renderer's chip without waiting
    /// for the next 5 s status poll.
    #[test]
    fn apply_project_config_mirrors_enabled_onto_status() {
        let st = KChatState::new();
        st.mark_loopback_active();
        let mut cfg = KChatConfig {
            enabled: false,
            ..Default::default()
        };
        st.apply_project_config(&cfg);
        let s = st.status();
        assert!(!s.enabled);
        assert_eq!(s.state, "disconnected");

        cfg.enabled = true;
        st.apply_project_config(&cfg);
        let s = st.status();
        assert!(s.enabled);
        assert_eq!(s.state, "connected");
    }

    /// `mark_loopback_active` promotes the kind to `loopback_http`
    /// and flips the connection state to `connected` when the
    /// integration is enabled.
    #[test]
    fn mark_loopback_active_promotes_status_when_enabled() {
        let st = KChatState::new();
        st.mark_loopback_active();
        let s = st.status();
        assert_eq!(s.publisher_kind, "loopback_http");
        assert_eq!(s.state, "connected");
    }

    /// When the integration is disabled, even an active loopback
    /// API must still report `disconnected` so the renderer's chip
    /// honestly reflects "publishes are refused" rather than
    /// "online".
    #[test]
    fn disabled_overrides_loopback_active_state() {
        let st = KChatState::new();
        st.mark_loopback_active();
        st.set_enabled(false);
        let s = st.status();
        assert_eq!(s.publisher_kind, "loopback_http");
        assert_eq!(s.state, "disconnected");
    }

    /// Re-emitting status without a loopback signal must continue
    /// to report `in_memory` — `reload` is a no-op now that there
    /// is no socket transport to rebuild.
    #[test]
    fn reload_is_a_noop_in_phase_15() {
        let st = KChatState::new();
        let before = st.status();
        let after = st.reload();
        assert_eq!(before, after);
    }

    /// In-memory publish always succeeds when enabled.
    #[test]
    fn in_memory_publisher_round_trips() {
        let st = KChatState::new();
        let r = st.publish(card()).expect("in-memory publish never fails");
        assert!(!r.message_id.is_empty());
    }

    /// Disabled state refuses publishes before touching the
    /// publisher.
    #[test]
    fn disabled_state_refuses_publish_without_touching_publisher() {
        let st = KChatState::new();
        st.set_enabled(false);
        let err = st
            .publish(card())
            .expect_err("disabled publisher must refuse");
        assert!(matches!(err, KChatError::Disabled));
    }

    /// Ingest always returns empty in Phase 15 — review comments
    /// flow through the Electron loopback API, not the Rust state.
    #[test]
    fn ingest_returns_empty_in_phase_15() {
        let st = KChatState::new();
        let (c, k) = st.ingest_reviews("any-thread", None).unwrap();
        assert!(c.is_empty());
        assert!(k.is_empty());
    }

    /// `apply_project_config` propagates the thread id and the
    /// enabled flag to the next status snapshot.
    #[test]
    fn apply_project_config_threads_through_status_payload() {
        let st = KChatState::new();
        assert!(st.default_thread_id().is_none());
        assert!(st.status().default_thread_id.is_none());

        st.apply_project_config(&KChatConfig::enabled_with_thread("project-thread-xyz"));
        assert_eq!(
            st.default_thread_id(),
            Some("project-thread-xyz".to_string()),
        );
        let s = st.status();
        assert_eq!(s.default_thread_id, Some("project-thread-xyz".to_string()));
        assert!(st.is_enabled());
    }

    /// Opening a project that omitted `default_thread_id` must
    /// clear the previously cached value so the renderer falls
    /// back to its `kchat-default` constant instead of continuing
    /// to read from the prior project's thread.
    #[test]
    fn apply_project_config_clears_stale_thread_when_new_config_omits_it() {
        let st = KChatState::new();
        st.apply_project_config(&KChatConfig::enabled_with_thread("first-thread"));
        assert_eq!(st.default_thread_id(), Some("first-thread".to_string()));

        st.apply_project_config(&KChatConfig::enabled());
        assert_eq!(st.default_thread_id(), None);
        assert!(st.status().default_thread_id.is_none());
    }

    /// `clear_project_config` drops the cached thread regardless
    /// of whether a project was previously opened.
    #[test]
    fn clear_project_config_drops_cached_thread() {
        let st = KChatState::new();
        st.apply_project_config(&KChatConfig::enabled_with_thread("to-be-cleared"));
        assert!(st.default_thread_id().is_some());
        st.clear_project_config();
        assert!(st.default_thread_id().is_none());
        assert!(st.status().default_thread_id.is_none());
    }

    /// `mark_loopback_inactive` demotes the kind back to in-memory
    /// on Electron shutdown.
    #[test]
    fn mark_loopback_inactive_demotes_to_in_memory() {
        let st = KChatState::new();
        st.mark_loopback_active();
        assert_eq!(st.status().publisher_kind, "loopback_http");
        st.mark_loopback_inactive();
        let s = st.status();
        assert_eq!(s.publisher_kind, "in_memory");
        assert_eq!(s.state, "disconnected");
    }
}
