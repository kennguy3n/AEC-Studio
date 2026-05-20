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
    pub fn publish_asset_pack(
        &self,
        manifest: &AssetPackManifest,
        thread_id: Option<&str>,
    ) -> Result<AssetPackReference, KChatError> {
        if !self.config.enabled {
            return Err(KChatError::Disabled);
        }
        let target = thread_id
            .map(str::to_string)
            .or_else(|| self.config.default_thread_id.clone())
            .ok_or_else(|| {
                KChatError::Transport("no thread_id provided and no default configured".into())
            })?;
        manifest.reference(target)
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
            InMemoryPublisher::new("ignored"),
        );
        let r = integ.publish_asset_pack(&manifest(), None).unwrap();
        assert_eq!(r.thread_id, "default-thread");
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
