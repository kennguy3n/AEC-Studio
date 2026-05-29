//! `aec_core` — the foundational types, configuration, errors, project
//! package format, and crypto primitives for AEC Studio.
//!
//! This crate is dependency-free of the rest of the workspace and is
//! consumed by every other AEC crate. It defines:
//!
//! - Strongly-typed IDs ([`ProjectId`], [`EntityId`], [`CommandId`], [`DiffId`])
//! - Workflow [`Scope`]s and [`Actor`]s
//! - Unit and region settings
//! - The [`ProjectConfig`] and [`HardwareProfile`] structs
//! - The [`AecError`] enum
//! - The on-disk [`ProjectPackage`] format (`*.aecstudio`)
//! - SQLCipher-encrypted [`db`] initialization and schema
//! - BLAKE3 hashing and per-project key derivation utilities ([`crypto`])
//! - JSON [`templates`] loader
//!
//! See `ARCHITECTURE.md` for the full architectural background.

pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod extension_permissions;
pub mod extensions;
#[cfg(feature = "kchat")]
pub mod kchat;
#[cfg(feature = "kchat")]
pub mod kchat_config;
#[cfg(feature = "kchat")]
pub mod kchat_sync;
pub mod manifest;
pub mod migrations;
pub mod package;
pub mod revision;
pub mod templates;
pub mod types;
pub mod version_diff;

pub use config::{HardwareProfile, ProjectConfig, ProjectSettings};
pub use error::{AecError, AecResult};
pub use extension_permissions::{
    validate_manifest, verify_signature_against, verify_signature_self_consistent, ManifestError,
    Operation, PermissionCheck, PermissionEnforcer, SignatureError, TrustStore,
};
pub use extensions::{
    canonical_payload_bytes, AiToolBody, AssetEntry, AssetEntryKind, AssetPackBody, ExportFormat,
    ExportTargetBody, ExtensionId, ExtensionLoader, ExtensionManifest, ExtensionRegistry,
    ExtensionSignature, ExtensionType, ImporterBody, LoadError, LoadOptions, LoadedExtension,
    Permission, ScheduleBody, ScheduleColumnDef, ScheduleFormulaDef, ScheduleValueType,
    TemplateBody,
};
#[cfg(feature = "kchat")]
pub use kchat::DEFAULT_THREAD_ID;
#[cfg(feature = "kchat")]
pub use kchat::{
    ingest_review, ingest_review_card, ApprovalStatus, ArtifactCard, AssetPackEntry,
    AssetPackEntryKind, AssetPackManifest, AssetPackReference, InMemoryPublisher, KChatArtifact,
    KChatError, KChatPublisher, ProjectAuditEntry, PublishResult, ReviewCard, ReviewComment,
};
#[cfg(feature = "kchat")]
pub use kchat_config::{AssetPackPublishOutcome, KChatConfig, KChatIntegration};
#[cfg(feature = "kchat")]
pub use kchat_sync::CommentSync;
pub use manifest::{ProjectManifest, SCHEMA_VERSION};
pub use package::{ProjectPackage, ProjectSummary, RecentEntry, RecentsStore};
pub use revision::{
    Revision, RevisionDraft, RevisionEntity, RevisionSnapshot, RevisionStore, SnapshotVerification,
};
pub use templates::{TemplateDefinition, TemplateLoader};
pub use types::{Actor, ActorKind, CommandId, DiffId, EntityId, ProjectId, Region, Scope, Units};
pub use version_diff::{
    classify_entity_kind, compare_revision_snapshots, compare_revisions, snapshot_entities,
    DiffCounts, EntityChange, EntityChangeKind, VersionDiff,
};
