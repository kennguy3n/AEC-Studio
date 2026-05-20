//! KChat integration configuration.
//!
//! AEC Studio is local-first; the KChat integration is strictly
//! opt-in. This module owns the toggle and acts as the gate every
//! KChat-aware operation must pass through.
//!
//! When the integration is disabled (the default), every publish /
//! sync / subscribe operation returns [`crate::kchat::KChatError::Disabled`]
//! immediately. Disabled is a hard refusal — there is no "fall back to
//! noop" mode that could mask a misconfiguration.

use serde::{Deserialize, Serialize};

use crate::kchat::{
    ArtifactCard, AssetPackManifest, AssetPackReference, KChatError, KChatPublisher,
    ProjectAuditEntry, PublishResult, ReviewCard, ReviewComment,
};

/// Outcome of [`KChatIntegration::publish_asset_pack`]. Carries both
/// the shareable reference *and* the publisher's response so the
/// project audit can record the transport message id alongside the
/// pack metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetPackPublishOutcome {
    pub reference: AssetPackReference,
    pub publish: PublishResult,
}

/// Configuration knob persisted in the project package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KChatConfig {
    /// Master enable switch. Defaults to `false`.
    pub enabled: bool,
    /// Thread id every publish routes to when the card does not
    /// override it. `None` means publishers must always be passed an
    /// explicit thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thread_id: Option<String>,
}

impl KChatConfig {
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            default_thread_id: None,
        }
    }

    pub fn enabled_with_thread(thread_id: impl Into<String>) -> Self {
        Self {
            enabled: true,
            default_thread_id: Some(thread_id.into()),
        }
    }
}

/// Integration façade. Holds the config plus the concrete publisher
/// and gates every operation on `config.enabled`.
///
/// Generic over the publisher so production code can wire a Slack /
/// KChat transport while tests can wire the in-memory publisher
/// without dynamic dispatch.
pub struct KChatIntegration<P>
where
    P: KChatPublisher,
{
    pub config: KChatConfig,
    publisher: P,
}

impl<P> KChatIntegration<P>
where
    P: KChatPublisher,
{
    pub fn new(config: KChatConfig, publisher: P) -> Self {
        Self { config, publisher }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Borrow the underlying publisher. Primarily useful for tests
    /// that need to assert on the publisher's recorded history.
    pub fn publisher(&self) -> &P {
        &self.publisher
    }

    /// Publish an artefact card. Returns [`KChatError::Disabled`]
    /// when the integration is disabled — *before* the card is
    /// validated, so callers can rely on disabled being a no-op.
    pub fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError> {
        if !self.config.enabled {
            return Err(KChatError::Disabled);
        }
        self.publisher.publish(card)
    }

    /// Ingest a single review comment as a project-audit entry.
    pub fn ingest_review(&self, comment: ReviewComment) -> Result<ProjectAuditEntry, KChatError> {
        if !self.config.enabled {
            return Err(KChatError::Disabled);
        }
        Ok(crate::kchat::ingest_review(comment))
    }

    /// Ingest a review card (comment + status) as a project-audit entry.
    pub fn ingest_review_card(&self, card: ReviewCard) -> Result<ProjectAuditEntry, KChatError> {
        if !self.config.enabled {
            return Err(KChatError::Disabled);
        }
        Ok(crate::kchat::ingest_review_card(card))
    }

    /// Publish an asset-pack reference to the configured thread (or
    /// `thread_id` when the caller wants to override). The blobs are
    /// out of scope — the user transports them through Dropbox /
    /// OneDrive / USB / wherever.
    ///
    /// This method now actually **publishes through the configured
    /// transport** in addition to building the reference: it renders
    /// an [`ArtifactCard`] from the manifest and calls
    /// [`KChatPublisher::publish`] on it. Returns both the reference
    /// (shared with subscribers so they can verify the manifest they
    /// pulled from their out-of-band transport) and the
    /// [`PublishResult`] reported by the transport (so the project
    /// audit can record "published to thread X at time Y").
    ///
    /// Earlier revisions only constructed the reference and never
    /// touched the publisher, which made the method name actively
    /// misleading. Callers that just want the reference without
    /// publishing should use [`AssetPackManifest::reference`]
    /// directly.
    pub fn publish_asset_pack(
        &self,
        manifest: &AssetPackManifest,
        thread_id: Option<&str>,
    ) -> Result<AssetPackPublishOutcome, KChatError> {
        if !self.config.enabled {
            return Err(KChatError::Disabled);
        }
        let target = thread_id
            .map(str::to_string)
            .or_else(|| self.config.default_thread_id.clone())
            .ok_or_else(|| {
                KChatError::Transport("no thread_id provided and no default configured".into())
            })?;
        let reference = manifest.reference(target)?;
        let card = manifest.artifact_card(&reference);
        let publish = self.publisher.publish(card)?;
        Ok(AssetPackPublishOutcome { reference, publish })
    }

    /// Subscribe to an asset pack — verify the manifest matches the
    /// reference (so the receiver knows they pulled the same logical
    /// pack from their transport).
    pub fn subscribe_asset_pack<'a>(
        &self,
        reference: &AssetPackReference,
        manifest: &'a AssetPackManifest,
    ) -> Result<&'a AssetPackManifest, KChatError> {
        if !self.config.enabled {
            return Err(KChatError::Disabled);
        }
        let computed = manifest.manifest_hash()?;
        if computed != reference.manifest_blake3 {
            return Err(KChatError::Transport(format!(
                "manifest hash mismatch: reference={}, computed={}",
                reference.manifest_blake3, computed
            )));
        }
        if manifest.pack_id != reference.pack_id || manifest.version != reference.version {
            return Err(KChatError::Transport(
                "manifest pack_id/version does not match reference".into(),
            ));
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kchat::{
        ApprovalStatus, AssetPackEntry, AssetPackEntryKind, AssetPackManifest, InMemoryPublisher,
        KChatArtifact, ReviewComment,
    };
    use std::collections::HashMap;

    fn card() -> ArtifactCard {
        ArtifactCard {
            artifact: KChatArtifact::Sheet,
            caption: "A100 — Plan".into(),
            project_link: "aecstudio://project/p/sheets/A100".into(),
            thumbnail_blake3: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn disabled_config_rejects_publish() {
        let integ = KChatIntegration::new(KChatConfig::default(), InMemoryPublisher::new("t"));
        assert!(matches!(integ.publish(card()), Err(KChatError::Disabled)));
    }

    #[test]
    fn enabled_config_publishes_through() {
        let integ = KChatIntegration::new(KChatConfig::enabled(), InMemoryPublisher::new("t"));
        let r = integ.publish(card()).unwrap();
        assert_eq!(r.thread_id, "t");
    }

    #[test]
    fn disabled_config_rejects_review_ingest() {
        let integ = KChatIntegration::new(KChatConfig::default(), InMemoryPublisher::new("t"));
        let comment = ReviewComment {
            thread_id: "t".into(),
            commenter: "@a".into(),
            text: "hi".into(),
            timestamp: chrono::Utc::now(),
            artifact_ref: None,
        };
        assert!(matches!(
            integ.ingest_review(comment),
            Err(KChatError::Disabled)
        ));
    }

    #[test]
    fn enabled_config_ingests_review() {
        let integ = KChatIntegration::new(KChatConfig::enabled(), InMemoryPublisher::new("t"));
        let comment = ReviewComment {
            thread_id: "t".into(),
            commenter: "@a".into(),
            text: "hi".into(),
            timestamp: chrono::Utc::now(),
            artifact_ref: None,
        };
        let entry = integ.ingest_review(comment).unwrap();
        assert_eq!(entry.source, "review_comment");
    }

    #[test]
    fn ingest_review_card_carries_status_when_enabled() {
        let integ = KChatIntegration::new(KChatConfig::enabled(), InMemoryPublisher::new("t"));
        let card_ = ReviewCard {
            comment: ReviewComment {
                thread_id: "t".into(),
                commenter: "@a".into(),
                text: "lgtm".into(),
                timestamp: chrono::Utc::now(),
                artifact_ref: None,
            },
            status: ApprovalStatus::Approved,
        };
        let entry = integ.ingest_review_card(card_).unwrap();
        assert_eq!(entry.status, Some(ApprovalStatus::Approved));
    }

    fn manifest() -> AssetPackManifest {
        AssetPackManifest {
            pack_id: "pack".into(),
            version: "0.1.0".into(),
            display_name: "Pack".into(),
            entries: vec![AssetPackEntry {
                name: "x.glb".into(),
                kind: AssetPackEntryKind::Geometry,
                blake3: "0".repeat(64),
                size_bytes: 1,
            }],
        }
    }

    #[test]
    fn publish_asset_pack_uses_default_thread_when_provided() {
        let integ = KChatIntegration::new(
            KChatConfig::enabled_with_thread("default-thread"),
            InMemoryPublisher::new("default-thread"),
        );
        let outcome = integ.publish_asset_pack(&manifest(), None).unwrap();
        // The reference carries the *target* thread the manifest is
        // published into. Subscribers verify against this.
        assert_eq!(outcome.reference.thread_id, "default-thread");
        // The publish result carries the *transport's* bound thread —
        // they agree because the integration was wired correctly.
        assert_eq!(outcome.publish.thread_id, "default-thread");
    }

    #[test]
    fn publish_asset_pack_actually_pushes_through_transport() {
        // Regression: earlier revisions only constructed the
        // reference and returned it, leaving the configured publisher
        // untouched. Verify the publisher actually receives the card.
        let publisher = InMemoryPublisher::new("t");
        let integ = KChatIntegration::new(KChatConfig::enabled(), publisher);
        let outcome = integ.publish_asset_pack(&manifest(), Some("t")).unwrap();
        // Grab the history off the integration's publisher.
        let history = integ.publisher().history();
        assert_eq!(history.len(), 1);
        let (card, result) = &history[0];
        assert_eq!(card.artifact, KChatArtifact::AssetPack);
        assert_eq!(
            card.metadata.get("pack_id").map(String::as_str),
            Some("pack")
        );
        assert_eq!(
            card.metadata.get("version").map(String::as_str),
            Some("0.1.0")
        );
        assert_eq!(
            card.metadata.get("manifest_blake3").map(String::as_str),
            Some(outcome.reference.manifest_blake3.as_str())
        );
        // The PublishResult returned through the outcome matches the
        // history entry — same message_id, same thread.
        assert_eq!(result.message_id, outcome.publish.message_id);
        assert_eq!(result.thread_id, "t");
    }

    #[test]
    fn publish_asset_pack_errors_without_thread() {
        let integ = KChatIntegration::new(KChatConfig::enabled(), InMemoryPublisher::new("t"));
        assert!(matches!(
            integ.publish_asset_pack(&manifest(), None),
            Err(KChatError::Transport(_))
        ));
    }

    #[test]
    fn subscribe_validates_manifest_hash() {
        let integ = KChatIntegration::new(KChatConfig::enabled(), InMemoryPublisher::new("t"));
        let m = manifest();
        let reference = m.reference("t").unwrap();
        let got = integ.subscribe_asset_pack(&reference, &m).unwrap();
        assert_eq!(got, &m);
    }

    #[test]
    fn subscribe_rejects_mutated_manifest() {
        let integ = KChatIntegration::new(KChatConfig::enabled(), InMemoryPublisher::new("t"));
        let m = manifest();
        let reference = m.reference("t").unwrap();
        let mut tampered = m.clone();
        tampered.entries[0].size_bytes += 1;
        assert!(matches!(
            integ.subscribe_asset_pack(&reference, &tampered),
            Err(KChatError::Transport(_))
        ));
    }
}
