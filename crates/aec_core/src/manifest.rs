//! The on-disk `manifest.json` schema for a `.aecstudio` package.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::ProjectSettings;
use crate::error::AecError;
use crate::types::ProjectId;

/// Latest project schema version. Bumped in lockstep with the
/// migration registry — every integer from 2 to this value must have
/// a corresponding [`crate::migrations::Migration`] in
/// [`crate::migrations::Migration::all`]. New projects are created
/// at this version; older projects are upgraded by
/// [`crate::migrations::run_pending`] when opened.
pub const SCHEMA_VERSION: u32 = 5;

/// The `manifest.json` at the root of a `.aecstudio` directory package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub project_id: ProjectId,
    pub name: String,
    pub schema_version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub settings: ProjectSettings,
    /// Optional template id this project was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Application version that created or last opened the project. Useful
    /// for forward-migration heuristics.
    pub app_version: String,
}

impl ProjectManifest {
    pub fn new(
        project_id: ProjectId,
        name: impl Into<String>,
        settings: ProjectSettings,
        template_id: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            project_id,
            name: name.into(),
            schema_version: SCHEMA_VERSION,
            created_at: now,
            updated_at: now,
            settings,
            template_id,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Validate a manifest read off disk.
    ///
    /// Backward-compatible: any `schema_version` between 1 and the
    /// current [`SCHEMA_VERSION`] inclusive is accepted, so a project
    /// written by an older app version still opens. Forward versions
    /// (`schema_version > SCHEMA_VERSION`) are still rejected loudly
    /// because we can't predict how a future schema's manifest
    /// extensions interact with this binary's reader.
    ///
    /// The DB-side counterpart is
    /// [`crate::migrations::run_pending`], which advances the SQL
    /// schema during [`crate::db::open_encrypted`]. The manifest's
    /// `schema_version` field is then bumped to [`SCHEMA_VERSION`]
    /// by [`upgrade_schema_version`] when the package is reopened
    /// with a key (so a subsequent strict reader sees the new
    /// value).
    pub fn validate(&self) -> Result<(), AecError> {
        if self.name.trim().is_empty() {
            return Err(AecError::InvalidManifest(
                "project name must not be empty".into(),
            ));
        }
        if self.schema_version == 0 || self.schema_version > SCHEMA_VERSION {
            return Err(AecError::SchemaMismatch {
                found: self.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// True when the on-disk manifest is older than [`SCHEMA_VERSION`].
    /// Callers that have the master key (e.g. the bridge) should run
    /// the DB migrations *and then* call [`Self::upgrade_schema_version`]
    /// to update the field.
    pub fn needs_upgrade(&self) -> bool {
        self.schema_version < SCHEMA_VERSION
    }

    /// Set `schema_version` to the current [`SCHEMA_VERSION`].
    /// Idempotent: a no-op when the field is already current.
    pub fn upgrade_schema_version(&mut self) {
        self.schema_version = SCHEMA_VERSION;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Region;

    #[test]
    fn manifest_roundtrip_through_json() {
        let m = ProjectManifest::new(
            ProjectId::new(),
            "Apartment renovation",
            ProjectSettings::from_region(Region::Eu),
            Some("interior.apartment".into()),
        );
        let j = serde_json::to_string_pretty(&m).unwrap();
        let back: ProjectManifest = serde_json::from_str(&j).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_rejects_empty_name() {
        let mut m = ProjectManifest::new(ProjectId::new(), "ok", ProjectSettings::default(), None);
        m.name = "   ".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn manifest_detects_future_schema_version() {
        let mut m = ProjectManifest::new(ProjectId::new(), "p", ProjectSettings::default(), None);
        m.schema_version = 999;
        let err = m.validate().unwrap_err();
        assert!(matches!(err, AecError::SchemaMismatch { .. }));
    }

    #[test]
    fn manifest_accepts_older_schema_version() {
        // A v1 manifest is opened by this binary (current SCHEMA_VERSION=2)
        // and is expected to validate cleanly so the migration runner can
        // do its work. The actual schema_version field bump happens via
        // `upgrade_schema_version` after migrations complete.
        let mut m = ProjectManifest::new(ProjectId::new(), "p", ProjectSettings::default(), None);
        m.schema_version = 1;
        m.validate().expect("older manifests must validate");
        assert!(m.needs_upgrade());
        m.upgrade_schema_version();
        assert_eq!(m.schema_version, SCHEMA_VERSION);
        assert!(!m.needs_upgrade());
    }

    #[test]
    fn manifest_rejects_zero_schema_version() {
        // Defensively, schema_version=0 isn't a real version we've ever
        // shipped, so treat it as a corrupted manifest. This guards
        // against a deserialiser default sneaking through.
        let mut m = ProjectManifest::new(ProjectId::new(), "p", ProjectSettings::default(), None);
        m.schema_version = 0;
        let err = m.validate().unwrap_err();
        assert!(matches!(err, AecError::SchemaMismatch { .. }));
    }
}
