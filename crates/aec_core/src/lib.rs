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
pub mod manifest;
pub mod package;
pub mod revision;
pub mod templates;
pub mod types;
pub mod version_diff;

pub use config::{HardwareProfile, ProjectConfig, ProjectSettings};
pub use error::{AecError, AecResult};
pub use manifest::{ProjectManifest, SCHEMA_VERSION};
pub use package::{ProjectPackage, ProjectSummary, RecentEntry, RecentsStore};
pub use revision::{Revision, RevisionDraft, RevisionEntity, RevisionStore};
pub use templates::{TemplateDefinition, TemplateLoader};
pub use types::{Actor, ActorKind, CommandId, DiffId, EntityId, ProjectId, Region, Scope, Units};
pub use version_diff::{
    compare_revisions, DiffCounts, EntityChange, EntityChangeKind, VersionDiff,
};
