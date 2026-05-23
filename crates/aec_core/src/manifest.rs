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
pub const SCHEMA_VERSION: u32 = 2;

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

    pub fn validate(&self) -> Result<(), AecError> {
        if self.name.trim().is_empty() {
            return Err(AecError::InvalidManifest(
                "project name must not be empty".into(),
            ));
        }
        if self.schema_version != SCHEMA_VERSION {
            return Err(AecError::SchemaMismatch {
                found: self.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(())
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
    fn manifest_detects_schema_mismatch() {
        let mut m = ProjectManifest::new(ProjectId::new(), "p", ProjectSettings::default(), None);
        m.schema_version = 999;
        let err = m.validate().unwrap_err();
        matches!(err, AecError::SchemaMismatch { .. });
    }
}
