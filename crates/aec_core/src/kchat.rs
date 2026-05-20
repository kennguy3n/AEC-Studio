//! KChat integration (Phase 7).
//!
//! AEC Studio remains a local-first tool: the KChat integration only
//! provides a **one-way publish** of artefact cards (renders, sheets,
//! revision packs, BOQ snapshots) and a **one-way ingest** of review
//! comments back into the project audit trail. There is no two-way
//! sync of project data — KChat receives presentational snapshots
//! and the project file is the canonical source of truth.
//!
//! Architectural contract:
//!
//! * [`KChatPublisher`] is the trait every concrete transport (Slack,
//!   KChat REST API, a self-hosted server, …) implements.
//! * [`InMemoryPublisher`] is provided for tests and for the
//!   in-process bridge fallback used by the desktop app in development.
//! * [`ArtifactCard`] is the on-wire payload. It is intentionally
//!   thin — captions, project link, thumbnail hash, and a small
//!   metadata bag. We do **not** ship raw geometry or BIM data over
//!   KChat; teams that want to share that use [`AssetPackReference`]
//!   and their own transport (Dropbox / OneDrive / USB).
//! * Review comments produced on KChat threads can be ingested via
//!   [`ingest_review`] which produces an [`crate::audit::ProjectAuditEntry`]
//!   that the project audit log can append.
//!
//! All operations are gated by [`crate::kchat_config::KChatIntegration`]
//! — when the integration is disabled, every publisher / sync method
//! returns [`KChatError::Disabled`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{Actor, EntityId};

/// Errors returned by KChat operations.
#[derive(Debug, Error)]
pub enum KChatError {
    #[error("KChat integration is disabled in the project config")]
    Disabled,
    #[error("KChat transport error: {0}")]
    Transport(String),
    #[error("invalid artefact card: {0}")]
    InvalidCard(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Category of artefact a card represents. Keeps the renderer in the
/// desktop app from having to inspect free-form metadata fields to
/// decide layout (a sheet card lays out differently to a render card).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KChatArtifact {
    ConceptRender,
    Sheet,
    RevisionPack,
    BoqSnapshot,
}

impl KChatArtifact {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConceptRender => "concept_render",
            Self::Sheet => "sheet",
            Self::RevisionPack => "revision_pack",
            Self::BoqSnapshot => "boq_snapshot",
        }
    }

    /// Human-readable label used in the publish modal.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ConceptRender => "Concept render",
            Self::Sheet => "Sheet",
            Self::RevisionPack => "Revision pack",
            Self::BoqSnapshot => "BOQ snapshot",
        }
    }
}

/// The serialised payload a publisher pushes to a KChat thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCard {
    /// Category of the artefact.
    pub artifact: KChatArtifact,
    /// One- or two-line caption shown above the thumbnail.
    pub caption: String,
    /// `aecstudio://project/<id>/...` link the desktop app handles
    /// when clicked. Kept opaque to KChat.
    pub project_link: String,
    /// BLAKE3 hex of the thumbnail bytes. We hash rather than embed
    /// the image so the publisher transport can decide whether to
    /// upload, link, or skip thumbnails depending on policy. Optional
    /// because revision packs and BOQ snapshots don't have a natural
    /// thumbnail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_blake3: Option<String>,
    /// Free-form key/value tags (e.g. `preset=warm_evening`,
    /// `sheet=A100`). Bounded to small values by the publish modal.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

impl ArtifactCard {
    /// Validate the card before publishing — catches obviously
    /// malformed payloads early so the transport never sees them.
    pub fn validate(&self) -> Result<(), KChatError> {
        if self.caption.trim().is_empty() {
            return Err(KChatError::InvalidCard("caption must not be empty".into()));
        }
        if !self.project_link.starts_with("aecstudio://") {
            return Err(KChatError::InvalidCard(format!(
                "project_link must start with aecstudio://, got `{}`",
                self.project_link
            )));
        }
        if let Some(hash) = &self.thumbnail_blake3 {
            if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(KChatError::InvalidCard(
                    "thumbnail_blake3 must be 64 hex chars".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Result of a successful publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishResult {
    /// Transport-specific message id the publisher returns. Opaque to
    /// AEC Studio — recorded so the project package can show the
    /// "published to thread X" history.
    pub message_id: String,
    /// Thread id the card landed in.
    pub thread_id: String,
    /// ISO-8601 timestamp the publisher reported.
    pub published_at: chrono::DateTime<chrono::Utc>,
}

/// Transport-agnostic publisher trait. Concrete implementations live
/// outside this crate (Slack adapter, KChat REST adapter, …).
pub trait KChatPublisher {
    /// Publish a single artefact card to the configured default
    /// thread (or `card`'s thread when the publisher supports
    /// per-card routing).
    fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError>;
}

/// In-memory publisher used by tests and the in-process desktop
/// fallback. Records every published card so tests can assert against
/// the published history.
#[derive(Debug, Default)]
pub struct InMemoryPublisher {
    published: std::sync::Mutex<Vec<(ArtifactCard, PublishResult)>>,
    thread_id: String,
    next_seq: std::sync::Mutex<u64>,
}

impl InMemoryPublisher {
    pub fn new(thread_id: impl Into<String>) -> Self {
        Self {
            published: std::sync::Mutex::new(Vec::new()),
            thread_id: thread_id.into(),
            next_seq: std::sync::Mutex::new(0),
        }
    }

    /// Borrow the published history.
    pub fn history(&self) -> Vec<(ArtifactCard, PublishResult)> {
        self.published.lock().expect("not poisoned").clone()
    }
}

impl KChatPublisher for InMemoryPublisher {
    fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError> {
        card.validate()?;
        let mut seq = self.next_seq.lock().expect("not poisoned");
        *seq += 1;
        let result = PublishResult {
            message_id: format!("inmem-{}", *seq),
            thread_id: self.thread_id.clone(),
            published_at: chrono::Utc::now(),
        };
        self.published
            .lock()
            .expect("not poisoned")
            .push((card, result.clone()));
        Ok(result)
    }
}

/// Approval status carried on a review card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Approved,
    ChangesRequested,
    Commented,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
            Self::Commented => "commented",
        }
    }
}

/// Inline review comment from a KChat thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    pub thread_id: String,
    pub commenter: String,
    pub text: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Optional reference to the AEC Studio entity this comment is
    /// about (e.g. a render id, a sheet id). Kept optional so threads
    /// can carry general project comments without forcing a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<EntityId>,
}

/// BLAKE3 hex digest of a comment body, used as part of the dedup
/// key so an *edit* to a previously-ingested comment shows up as a
/// new audit entry rather than being silently dropped.
fn text_fingerprint(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Stable identity tuple for a KChat review comment in the audit
/// trail. See [`ReviewComment::dedup_key`] for why `text` is folded
/// into the key via a BLAKE3 digest.
pub type ReviewCommentKey = (String, chrono::DateTime<chrono::Utc>, String, String);

impl ReviewComment {
    /// Dedup key — two comments with the same
    /// `(thread_id, timestamp, commenter, blake3(text))` are treated
    /// as a re-import of the same logical event by
    /// [`crate::kchat_sync::CommentSync`].
    ///
    /// **Why the text fingerprint is part of the key.** The audit
    /// trail is the source of truth for what reviewers said about a
    /// project. If a commenter edits their KChat comment after the
    /// first sync — even keeping the same author and timestamp — the
    /// edit must land in the audit trail as its own entry, otherwise
    /// later readers see the original text and never learn the
    /// reviewer changed their mind. Hashing the text gives idempotent
    /// re-imports for unchanged comments and turns edits into
    /// independent audit rows that share `(thread_id, commenter)` and
    /// can be ordered by timestamp to reconstruct the edit history.
    pub fn dedup_key(&self) -> ReviewCommentKey {
        (
            self.thread_id.clone(),
            self.timestamp,
            self.commenter.clone(),
            text_fingerprint(&self.text),
        )
    }
}

/// Rebuild a [`ReviewCommentKey`] from a persisted audit entry. Used
/// by [`crate::kchat_sync::CommentSync::from_existing_entries`] so
/// resumed sessions keep their dedup set consistent with the on-disk
/// audit log.
pub fn audit_entry_dedup_key(entry: &ProjectAuditEntry) -> ReviewCommentKey {
    (
        entry.thread_id.clone(),
        entry.timestamp,
        entry.actor.tool.clone().unwrap_or_default(),
        text_fingerprint(&entry.text),
    )
}

/// Full review card — a comment plus an explicit approval status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCard {
    pub comment: ReviewComment,
    pub status: ApprovalStatus,
}

/// Render an audit-trail entry from a review comment.
///
/// The returned [`ProjectAuditEntry`] is a thin record the project
/// audit log appends with `actor.kind = ActorKind::KChat` and
/// `tool = "review_comment"`. Persisting it is the caller's job — we
/// keep this function pure so it composes with [`crate::kchat_sync::CommentSync`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectAuditEntry {
    pub actor: Actor,
    pub source: String,
    pub thread_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub text: String,
    pub artifact_ref: Option<EntityId>,
    pub status: Option<ApprovalStatus>,
}

pub fn ingest_review(comment: ReviewComment) -> ProjectAuditEntry {
    ProjectAuditEntry {
        actor: Actor::kchat(&comment.commenter),
        source: "review_comment".into(),
        thread_id: comment.thread_id,
        timestamp: comment.timestamp,
        text: comment.text,
        artifact_ref: comment.artifact_ref,
        status: None,
    }
}

/// Same as [`ingest_review`] but for review cards that carry an
/// explicit approval state (Approve / Request changes).
pub fn ingest_review_card(card: ReviewCard) -> ProjectAuditEntry {
    let mut entry = ingest_review(card.comment);
    entry.status = Some(card.status);
    entry
}

/// Reference to a published asset pack. The reference is what
/// crosses KChat — the actual blobs (geometry, textures) are out of
/// scope and ship via user-chosen transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetPackReference {
    pub pack_id: String,
    pub version: String,
    /// BLAKE3 of the manifest JSON so subscribers can verify they
    /// pulled the same logical pack from their transport.
    pub manifest_blake3: String,
    pub thread_id: String,
}

/// On-disk manifest of an asset pack. Holds the *names* and hashes
/// of the assets only — actual asset blobs are stored separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetPackManifest {
    pub pack_id: String,
    pub version: String,
    pub display_name: String,
    pub entries: Vec<AssetPackEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetPackEntry {
    pub name: String,
    pub kind: AssetPackEntryKind,
    pub blake3: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetPackEntryKind {
    Geometry,
    Material,
    Texture,
    Preset,
    Other,
}

impl AssetPackManifest {
    /// Compute the manifest hash deterministically. Used by
    /// [`Self::reference`] and by subscribers verifying integrity.
    pub fn manifest_hash(&self) -> Result<String, KChatError> {
        let canonical = serde_json::to_vec(self)?;
        Ok(blake3::hash(&canonical).to_hex().to_string())
    }

    /// Build a publishable [`AssetPackReference`] for this manifest,
    /// targeted at `thread_id`.
    pub fn reference(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<AssetPackReference, KChatError> {
        Ok(AssetPackReference {
            pack_id: self.pack_id.clone(),
            version: self.version.clone(),
            manifest_blake3: self.manifest_hash()?,
            thread_id: thread_id.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_card() -> ArtifactCard {
        ArtifactCard {
            artifact: KChatArtifact::ConceptRender,
            caption: "Living room — warm evening".into(),
            project_link: "aecstudio://project/proj_42/renders/r1".into(),
            thumbnail_blake3: Some("a".repeat(64)),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn artifact_card_roundtrips_via_json() {
        let card = valid_card();
        let s = serde_json::to_string(&card).unwrap();
        let back: ArtifactCard = serde_json::from_str(&s).unwrap();
        assert_eq!(card, back);
    }

    #[test]
    fn empty_caption_is_rejected() {
        let mut card = valid_card();
        card.caption = "  ".into();
        assert!(matches!(card.validate(), Err(KChatError::InvalidCard(_))));
    }

    #[test]
    fn malformed_project_link_is_rejected() {
        let mut card = valid_card();
        card.project_link = "https://example.com/oops".into();
        assert!(matches!(card.validate(), Err(KChatError::InvalidCard(_))));
    }

    #[test]
    fn malformed_thumbnail_hash_is_rejected() {
        let mut card = valid_card();
        card.thumbnail_blake3 = Some("not-hex".into());
        assert!(matches!(card.validate(), Err(KChatError::InvalidCard(_))));
    }

    #[test]
    fn in_memory_publisher_records_publishes() {
        let pub_ = InMemoryPublisher::new("thread-42");
        let result = pub_.publish(valid_card()).unwrap();
        assert_eq!(result.thread_id, "thread-42");
        assert_eq!(pub_.history().len(), 1);
    }

    #[test]
    fn in_memory_publisher_emits_unique_message_ids() {
        let pub_ = InMemoryPublisher::new("t");
        let a = pub_.publish(valid_card()).unwrap();
        let b = pub_.publish(valid_card()).unwrap();
        assert_ne!(a.message_id, b.message_id);
    }

    #[test]
    fn ingest_review_yields_kchat_actor() {
        let comment = ReviewComment {
            thread_id: "t".into(),
            commenter: "@alice".into(),
            text: "lighting feels cold".into(),
            timestamp: chrono::Utc::now(),
            artifact_ref: None,
        };
        let entry = ingest_review(comment);
        assert_eq!(entry.actor.kind, crate::types::ActorKind::KChat);
        assert_eq!(entry.actor.tool.as_deref(), Some("@alice"));
        assert_eq!(entry.source, "review_comment");
        assert!(entry.status.is_none());
    }

    #[test]
    fn ingest_review_card_propagates_status() {
        let comment = ReviewComment {
            thread_id: "t".into(),
            commenter: "@bob".into(),
            text: "looks great".into(),
            timestamp: chrono::Utc::now(),
            artifact_ref: None,
        };
        let card = ReviewCard {
            comment,
            status: ApprovalStatus::Approved,
        };
        let entry = ingest_review_card(card);
        assert_eq!(entry.status, Some(ApprovalStatus::Approved));
    }

    #[test]
    fn asset_pack_manifest_hash_is_deterministic_and_reference_carries_it() {
        let manifest = AssetPackManifest {
            pack_id: "studio-furniture-pack".into(),
            version: "1.0.0".into(),
            display_name: "Studio Furniture".into(),
            entries: vec![AssetPackEntry {
                name: "chair_modern.glb".into(),
                kind: AssetPackEntryKind::Geometry,
                blake3: "f".repeat(64),
                size_bytes: 12_345,
            }],
        };
        let h1 = manifest.manifest_hash().unwrap();
        let h2 = manifest.manifest_hash().unwrap();
        assert_eq!(h1, h2);
        let r = manifest.reference("thread-7").unwrap();
        assert_eq!(r.manifest_blake3, h1);
        assert_eq!(r.thread_id, "thread-7");
    }

    #[test]
    fn approval_status_string_form_is_stable() {
        assert_eq!(ApprovalStatus::Approved.as_str(), "approved");
        assert_eq!(
            ApprovalStatus::ChangesRequested.as_str(),
            "changes_requested"
        );
        assert_eq!(ApprovalStatus::Commented.as_str(), "commented");
    }

    #[test]
    fn artifact_kind_string_form_is_stable() {
        assert_eq!(KChatArtifact::ConceptRender.as_str(), "concept_render");
        assert_eq!(KChatArtifact::Sheet.as_str(), "sheet");
        assert_eq!(KChatArtifact::RevisionPack.as_str(), "revision_pack");
        assert_eq!(KChatArtifact::BoqSnapshot.as_str(), "boq_snapshot");
    }
}
