//! Process-wide KChat state held on [`crate::service::BridgeService`].
//!
//! The renderer talks to KChat Desktop through three IPC endpoints
//! (`kchat:status`, `kchat:publish`, `kchat:ingestReviews`). All three
//! delegate to this state object, which owns the chosen
//! [`KChatPublisher`] for the running process.
//!
//! ## Publisher selection
//!
//! [`KChatState::new`] resolves a publisher at construction time:
//!
//! 1. If [`aec_core::KChatDiscovery::probe`] returns
//!    `Some(KChatInstanceInfo)`, the state holds a
//!    [`LocalIpcPublisher`] and surfaces the discovered instance via
//!    [`KChatState::status`].
//! 2. Otherwise the state falls back to an [`InMemoryPublisher`] so
//!    callers can still exercise the publish path (the bridge tests
//!    exercise this; CI never has a real KChat Desktop on the box).
//!
//! The publisher choice is sticky for the lifetime of the bridge
//! process. A `kchat:status` poll re-runs discovery but only updates
//! the cached `KChatInstanceInfo`; the publisher itself is replaced
//! only when the renderer hits "Reload KChat connection" in the
//! Settings page (which calls [`KChatState::reload`]).

use std::sync::{Arc, RwLock};
use std::time::Duration;

use aec_core::kchat::{
    ArtifactCard, InMemoryPublisher, KChatError, KChatPublisher, PublishResult, ReviewCard,
    ReviewComment,
};
use aec_core::kchat_config::KChatConfig;
use aec_core::kchat_discovery::{KChatDiscovery, KChatInstanceInfo};
use aec_core::kchat_transport::DEFAULT_IO_TIMEOUT;
use aec_core::local_ipc_publisher::LocalIpcPublisher;
use serde::{Deserialize, Serialize};

/// Snapshot returned by [`KChatState::status`]. Mirrored 1:1 by the
/// renderer-side `KChatStatusIndicator` so adding fields here is a
/// breaking change at the IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KChatStatusReport {
    /// One of `connected` (local IPC publisher with a healthy
    /// heartbeat), `disconnected` (in-memory fallback, no instance
    /// discovered), or `reconnecting` (we have a publisher but the
    /// last heartbeat failed).
    pub state: String,
    /// Concrete publisher kind currently in use — `local_ipc` or
    /// `in_memory`. Surfaced so the Settings page can show "Connected
    /// to KChat Desktop X.Y.Z" vs "Local-only fallback".
    pub publisher_kind: String,
    /// Discovered instance info, if any.
    pub instance: Option<KChatInstanceInfo>,
    /// Per-project KChat thread the active project routes publishes
    /// and review-comment ingestion to. Mirrors
    /// [`KChatConfig::default_thread_id`] from the open project's
    /// manifest, surfaced through the status payload so the renderer
    /// can wire its review panel without an extra IPC roundtrip.
    ///
    /// `None` either means no project is open yet or the open
    /// project hasn't picked a thread — callers fall back to
    /// [`aec_core::DEFAULT_THREAD_ID`] (the publisher-side default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thread_id: Option<String>,
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
            .field("instance_present", &inner.discovered.is_some())
            .field("state", &inner.last_status.state)
            .finish_non_exhaustive()
    }
}

struct Inner {
    /// Trait-object handle used by [`KChatState::publish`]. Always
    /// present; the concrete type is either `LocalIpcPublisher` (when
    /// `local_ipc` is Some) or `InMemoryPublisher`.
    publisher: Arc<dyn KChatPublisher + Send + Sync>,
    /// Concrete LocalIpcPublisher reference, present iff the
    /// `publisher_kind` is `LocalIpc`. Kept alongside the trait
    /// object so `ingest_reviews` (which isn't on the trait) can
    /// reach the concrete type without an unsafe downcast.
    local_ipc: Option<Arc<LocalIpcPublisher>>,
    publisher_kind: PublisherKind,
    last_status: KChatStatusReport,
    /// Cached discovery info — refreshed by `status()` calls so the
    /// UI can poll cheaply without re-probing every tick.
    discovered: Option<KChatInstanceInfo>,
    /// Phase 12 Task 30 — master enable switch mirroring
    /// [`aec_core::kchat_config::KChatConfig::enabled`]. When `false`
    /// every publish / ingest call returns
    /// [`KChatError::Disabled`] *before* touching the transport, so
    /// "disable KChat" in Settings is a hard refusal and not a silent
    /// noop.
    enabled: bool,
    /// Cached per-project thread id mirroring
    /// [`KChatConfig::default_thread_id`] from the currently open
    /// project's manifest. Refreshed by [`KChatState::apply_project_config`]
    /// on each `project_open` / `project_save` so the renderer's
    /// status poll surfaces the right thread the moment a project is
    /// active. `None` outside of an open project (or when the
    /// project chose to leave `default_thread_id` unset).
    default_thread_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublisherKind {
    InMemory,
    LocalIpc,
}

impl PublisherKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::LocalIpc => "local_ipc",
        }
    }
}

impl Default for KChatState {
    fn default() -> Self {
        Self::new()
    }
}

impl KChatState {
    /// Build a fresh state object. Runs discovery once; if it
    /// succeeds the state holds a [`LocalIpcPublisher`], otherwise
    /// it falls back to an [`InMemoryPublisher`].
    pub fn new() -> Self {
        let inner = Inner::detect_and_build();
        Self {
            inner: RwLock::new(inner),
        }
    }

    /// Snapshot the current connection status.
    ///
    /// When the state is already "connected" (a live
    /// [`LocalIpcPublisher`] is installed and the last discovery
    /// returned `Some(_)`), this takes only a read-lock — it does
    /// NOT re-probe the IPC socket. Crash detection for an
    /// already-connected instance flows through the publisher
    /// itself: [`Self::publish`] downgrades to the in-memory
    /// fallback on a [`KChatError::Transport`] result, which flips
    /// the cached state to "disconnected" and re-enables probing.
    ///
    /// When the state is "disconnected" (or the cached publisher is
    /// in-memory because boot-time discovery returned `None`), this
    /// re-runs [`KChatDiscovery::probe_with_timeout`] so the
    /// renderer's poll picks up newly started KChat Desktop
    /// instances without restarting AEC Studio. The probe is cheap
    /// (200 ms socket connect + ping) and gated on the
    /// not-yet-connected path so the steady-state renderer poll
    /// (every 5 s while connected) costs only a read-lock acquire.
    pub fn status(&self) -> KChatStatusReport {
        // Fast path: read-lock only. We re-probe iff we're currently
        // disconnected — see the doc comment for why connected
        // sessions don't need a periodic probe.
        let needs_refresh = {
            let inner = self.inner.read().expect("kchat state not poisoned");
            inner.last_status.state == "disconnected"
        };
        if needs_refresh {
            self.refresh_status();
        }
        self.inner
            .read()
            .expect("kchat state not poisoned")
            .last_status
            .clone()
    }

    /// Re-run discovery and rebuild the publisher accordingly. Used
    /// by Settings "Reload KChat connection".
    pub fn reload(&self) -> KChatStatusReport {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        *inner = Inner::detect_and_build();
        inner.last_status.clone()
    }

    /// Publish an artifact card through whichever publisher is
    /// currently active. Returns [`KChatError::Disabled`] *before*
    /// touching the transport when the integration has been disabled
    /// via [`Self::set_enabled`] (Phase 12 Task 30).
    pub fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError> {
        let publisher = {
            let inner = self.inner.read().expect("kchat state not poisoned");
            if !inner.enabled {
                return Err(KChatError::Disabled);
            }
            Arc::clone(&inner.publisher)
        };
        let result = publisher.publish(card);
        // A transport failure on the local IPC path likely means
        // KChat Desktop crashed; downgrade to in-memory fallback so
        // the renderer's status indicator shows "disconnected"
        // immediately rather than waiting for the next status poll.
        if matches!(&result, Err(KChatError::Transport(_))) {
            self.downgrade_to_fallback();
        }
        result
    }

    /// Poll new review comments. Only meaningful when the local IPC
    /// publisher is active; the in-memory fallback returns
    /// `(vec![], vec![])` so the renderer's poll loop is a no-op.
    pub fn ingest_reviews(
        &self,
        thread_id: &str,
        since_iso: Option<String>,
    ) -> Result<(Vec<ReviewComment>, Vec<ReviewCard>), KChatError> {
        let local = {
            let inner = self.inner.read().expect("kchat state not poisoned");
            if !inner.enabled {
                return Err(KChatError::Disabled);
            }
            inner.local_ipc.as_ref().map(Arc::clone)
        };
        match local {
            Some(p) => p.ingest_reviews(thread_id.to_string(), since_iso),
            None => Ok((Vec::new(), Vec::new())),
        }
    }

    fn refresh_status(&self) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        // Re-run discovery cheaply (200 ms timeout).
        let probed = KChatDiscovery::probe_with_timeout(Duration::from_millis(200));
        inner.discovered.clone_from(&probed);
        let state_str = match (inner.publisher_kind, &probed) {
            (PublisherKind::LocalIpc, Some(_)) => "connected",
            (PublisherKind::LocalIpc, None) => "reconnecting",
            (PublisherKind::InMemory, Some(_)) => "reconnecting",
            (PublisherKind::InMemory, None) => "disconnected",
        };
        inner.last_status = KChatStatusReport {
            state: state_str.into(),
            publisher_kind: inner.publisher_kind.as_str().into(),
            instance: probed,
            default_thread_id: inner.default_thread_id.clone(),
        };
    }

    fn downgrade_to_fallback(&self) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        if inner.publisher_kind == PublisherKind::InMemory {
            return;
        }
        inner.publisher = Arc::new(InMemoryPublisher::new("aecstudio-fallback"));
        inner.local_ipc = None;
        inner.publisher_kind = PublisherKind::InMemory;
        inner.last_status = KChatStatusReport {
            state: "disconnected".into(),
            publisher_kind: PublisherKind::InMemory.as_str().into(),
            instance: inner.discovered.clone(),
            default_thread_id: inner.default_thread_id.clone(),
        };
    }

    /// Phase 12 Task 30 — master enable / disable switch. Setting
    /// this to `false` causes every subsequent [`Self::publish`] and
    /// [`Self::ingest_reviews`] call to return
    /// [`KChatError::Disabled`] without touching the transport.
    /// Idempotent — flipping back to `true` resumes through the
    /// already-installed publisher.
    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.enabled = enabled;
    }

    /// Mirror of [`Self::set_enabled`] — useful for the Settings page
    /// to reflect the current toggle state without re-reading from
    /// the underlying project config.
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
    /// - `default_thread_id` — cached so [`Self::status`] surfaces it
    ///   to the renderer. The Deliver page's review panel reads
    ///   this field and falls back to the publisher-side default
    ///   only when it's `None`.
    ///
    /// Idempotent — repeated calls with the same config produce
    /// the same `last_status`.
    pub fn apply_project_config(&self, config: &KChatConfig) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.enabled = config.enabled;
        inner
            .default_thread_id
            .clone_from(&config.default_thread_id);
        inner
            .last_status
            .default_thread_id
            .clone_from(&config.default_thread_id);
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
    /// without re-running the discovery probe in `status()`.
    pub fn default_thread_id(&self) -> Option<String> {
        self.inner
            .read()
            .expect("kchat state not poisoned")
            .default_thread_id
            .clone()
    }

    /// Test helper — force the state into an in-memory publisher
    /// pointed at the supplied origin tag.
    #[doc(hidden)]
    pub fn __test_force_in_memory(&self, origin: &str) {
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.publisher = Arc::new(InMemoryPublisher::new(origin));
        inner.local_ipc = None;
        inner.publisher_kind = PublisherKind::InMemory;
        inner.last_status = KChatStatusReport {
            state: "disconnected".into(),
            publisher_kind: PublisherKind::InMemory.as_str().into(),
            instance: None,
            default_thread_id: inner.default_thread_id.clone(),
        };
        inner.discovered = None;
    }

    /// Test helper — install a `LocalIpcPublisher` targeted at a
    /// caller-supplied socket path so phase-7 e2e tests can drive
    /// the full IPC path against a mock socket server.
    #[doc(hidden)]
    pub fn __test_force_local_ipc(&self, socket_path: std::path::PathBuf) {
        let publisher_local = Arc::new(LocalIpcPublisher::with_socket_timeout(
            socket_path.clone(),
            DEFAULT_IO_TIMEOUT,
        ));
        let mut inner = self.inner.write().expect("kchat state not poisoned");
        inner.publisher = Arc::clone(&publisher_local) as Arc<dyn KChatPublisher + Send + Sync>;
        inner.local_ipc = Some(publisher_local);
        inner.publisher_kind = PublisherKind::LocalIpc;
        let info = KChatInstanceInfo {
            socket_path,
            version: "test-fixture".into(),
            health: "connected".into(),
        };
        inner.discovered = Some(info.clone());
        inner.last_status = KChatStatusReport {
            state: "connected".into(),
            publisher_kind: PublisherKind::LocalIpc.as_str().into(),
            instance: Some(info),
            default_thread_id: inner.default_thread_id.clone(),
        };
    }
}

impl Inner {
    fn detect_and_build() -> Self {
        if let Some(info) = KChatDiscovery::probe_with_timeout(Duration::from_millis(200)) {
            let publisher_local = Arc::new(LocalIpcPublisher::with_socket_timeout(
                info.socket_path.clone(),
                DEFAULT_IO_TIMEOUT,
            ));
            let publisher: Arc<dyn KChatPublisher + Send + Sync> =
                Arc::clone(&publisher_local) as Arc<dyn KChatPublisher + Send + Sync>;
            return Self {
                publisher,
                local_ipc: Some(publisher_local),
                publisher_kind: PublisherKind::LocalIpc,
                last_status: KChatStatusReport {
                    state: "connected".into(),
                    publisher_kind: PublisherKind::LocalIpc.as_str().into(),
                    instance: Some(info.clone()),
                    default_thread_id: None,
                },
                discovered: Some(info),
                enabled: true,
                default_thread_id: None,
            };
        }
        let publisher: Arc<dyn KChatPublisher + Send + Sync> =
            Arc::new(InMemoryPublisher::new("aecstudio-fallback"));
        Self {
            publisher,
            local_ipc: None,
            publisher_kind: PublisherKind::InMemory,
            last_status: KChatStatusReport {
                state: "disconnected".into(),
                publisher_kind: PublisherKind::InMemory.as_str().into(),
                instance: None,
                default_thread_id: None,
            },
            discovered: None,
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

    #[test]
    fn default_falls_back_to_in_memory_publisher_when_no_kchat_running() {
        // Make sure the test env has no real socket pointed at it.
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        let s = st.status();
        // On a CI box there is no KChat Desktop, so the publisher
        // must be in-memory.
        assert_eq!(s.publisher_kind, "in_memory");
        assert_eq!(s.state, "disconnected");
    }

    #[test]
    fn in_memory_publisher_round_trips() {
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        let r = st.publish(card()).expect("in-memory publish never fails");
        assert!(!r.message_id.is_empty());
    }

    #[test]
    fn ingest_returns_empty_for_in_memory_kind() {
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        let (c, k) = st.ingest_reviews("any-thread", None).unwrap();
        assert!(c.is_empty());
        assert!(k.is_empty());
    }

    /// `apply_project_config` must propagate
    /// [`KChatConfig::default_thread_id`] to both the cached
    /// `default_thread_id` and the next `status()` snapshot so the
    /// renderer's poll surfaces the per-project thread on the very
    /// next tick.
    #[test]
    fn apply_project_config_threads_through_status_payload() {
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        assert!(
            st.default_thread_id().is_none(),
            "fresh state must have no per-project thread"
        );
        assert!(
            st.status().default_thread_id.is_none(),
            "status payload must mirror cached state"
        );

        st.apply_project_config(&KChatConfig::enabled_with_thread("project-thread-xyz"));
        assert_eq!(
            st.default_thread_id(),
            Some("project-thread-xyz".to_string()),
        );
        let s = st.status();
        assert_eq!(s.default_thread_id, Some("project-thread-xyz".to_string()));
        assert!(
            st.is_enabled(),
            "applying an enabled config must enable publishes"
        );
    }

    /// Reopening a project that omitted `default_thread_id` must
    /// clear the previously cached value so the renderer falls
    /// back to its `kchat-default` constant instead of continuing
    /// to read from the prior project's thread.
    #[test]
    fn apply_project_config_clears_stale_thread_when_new_config_omits_it() {
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        st.apply_project_config(&KChatConfig::enabled_with_thread("first-thread"));
        assert_eq!(st.default_thread_id(), Some("first-thread".to_string()));

        st.apply_project_config(&KChatConfig::enabled());
        assert_eq!(st.default_thread_id(), None);
        assert!(st.status().default_thread_id.is_none());
    }

    /// `clear_project_config` is the explicit "close project" hook;
    /// it must drop the cached thread regardless of whether a
    /// project was previously opened.
    #[test]
    fn clear_project_config_drops_cached_thread() {
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        st.apply_project_config(&KChatConfig::enabled_with_thread("to-be-cleared"));
        assert!(st.default_thread_id().is_some());
        st.clear_project_config();
        assert!(st.default_thread_id().is_none());
        assert!(st.status().default_thread_id.is_none());
    }

    /// Even after the publisher transparently downgrades to the
    /// in-memory fallback, the per-project thread id must survive
    /// because it's a property of the open project, not the
    /// transport.
    #[test]
    fn downgrade_to_fallback_preserves_default_thread_id() {
        std::env::remove_var(aec_core::kchat_discovery::KCHAT_SOCKET_PATH_ENV);
        let st = KChatState::new();
        st.apply_project_config(&KChatConfig::enabled_with_thread("survive-downgrade"));
        // The `__test_force_in_memory` helper rewrites
        // `last_status` directly, mirroring the downgrade path;
        // we still expect the cached `default_thread_id` to win.
        st.__test_force_in_memory("forced-origin");
        assert_eq!(
            st.status().default_thread_id,
            Some("survive-downgrade".to_string())
        );
    }
}
