//! Commands for the **Deliver** scope.
//!
//! Today the only deliver-scope command is [`CreateRevision`], used to
//! mark the audit chain with a "snapshot taken here" entry. The actual
//! revision file is written by
//! `aec_bridge::service::BridgeService::deliver_create_revision` —
//! this command exists so the gesture lands in the journal and audit
//! log (same auditability surface as a `design.*` mutation) but does
//! NOT alter the project graph. The command returns an empty delta
//! vector and the engine simply records the journal entry.
//!
//! Listing and comparing revisions are read-only and therefore do not
//! flow through the command engine; they live as plain service
//! methods on `BridgeService`.
use serde::{Deserialize, Serialize};

use crate::error::{CommandError, CommandResult};

/// Capture a revision snapshot of the project.
///
/// `tag` is the user-supplied unique identifier for the revision
/// (must be unique within a project — the service layer enforces this
/// via `aec_core::revision::RevisionStore::create`). `description` is
/// a free-form human-readable note shown in the Deliver UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRevision {
    /// Unique tag (e.g. `"client-review-2026-05-21"`). The service
    /// layer rejects duplicates with `AecError::AlreadyExists`.
    pub tag: String,
    /// Free-form note describing why the revision was taken.
    #[serde(default)]
    pub description: String,
    /// Optional pre-computed revision id (for deterministic replay
    /// in tests). When `None` the service layer generates a fresh id
    /// via `RevisionStore::create`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
}

impl CreateRevision {
    pub fn validate(&self) -> CommandResult<()> {
        if self.tag.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "deliver.create_revision".into(),
                reason: "tag must not be empty".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_tag() {
        let cmd = CreateRevision {
            tag: "   ".into(),
            description: "x".into(),
            revision_id: None,
        };
        assert!(cmd.validate().is_err());
    }

    #[test]
    fn accepts_normal_tag() {
        let cmd = CreateRevision {
            tag: "client-review-2026-05-21".into(),
            description: "First milestone".into(),
            revision_id: None,
        };
        cmd.validate().unwrap();
    }
}
