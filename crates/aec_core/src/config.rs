//! Project configuration and hardware-profile descriptions.

use serde::{Deserialize, Serialize};

use crate::types::{ProjectId, Region, Units};

/// Top-level project configuration recorded in the project manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project_id: ProjectId,
    pub name: String,
    pub settings: ProjectSettings,
}

impl ProjectConfig {
    pub fn new(name: impl Into<String>, settings: ProjectSettings) -> Self {
        Self {
            project_id: ProjectId::new(),
            name: name.into(),
            settings,
        }
    }
}

/// Project-level settings persisted in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub units: Units,
    pub region: Region,
    /// Optional drawing-standards identifier (e.g. `iso128`, `ansi_y14`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standards: Option<String>,
}

impl ProjectSettings {
    pub fn from_region(region: Region) -> Self {
        Self {
            units: region.default_units(),
            region,
            standards: None,
        }
    }
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self::from_region(Region::Eu)
    }
}

/// Snapshot of the detected hardware at project-open time. Mirrors
/// `aec_governor::profiler::HardwareProfile` but is kept in `aec_core` so
/// every crate (and the bridge) can use the type without depending on the
/// governor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub cpu_brand: String,
    pub cpu_cores: u32,
    pub cpu_logical: u32,
    pub ram_total_mb: u64,
    pub ram_available_mb: u64,
    pub gpu_vendor: Option<String>,
    pub gpu_model: Option<String>,
    pub gpu_vram_mb: Option<u64>,
    pub os_name: String,
    pub os_version: String,
    pub accelerators: Vec<String>,
}

impl HardwareProfile {
    pub fn placeholder() -> Self {
        Self {
            cpu_brand: "unknown".to_string(),
            cpu_cores: 0,
            cpu_logical: 0,
            ram_total_mb: 0,
            ram_available_mb: 0,
            gpu_vendor: None,
            gpu_model: None,
            gpu_vram_mb: None,
            os_name: std::env::consts::OS.to_string(),
            os_version: String::new(),
            accelerators: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_roundtrip() {
        let s = ProjectSettings {
            units: Units::Mm,
            region: Region::Eu,
            standards: None,
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: ProjectSettings = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn region_defaults_apply() {
        let s = ProjectSettings::from_region(Region::Na);
        assert!(matches!(s.units, Units::Inches));
    }

    #[test]
    fn project_config_has_fresh_id() {
        let cfg = ProjectConfig::new("Test", ProjectSettings::default());
        assert!(cfg.project_id.as_str().starts_with("proj_"));
        assert_eq!(cfg.name, "Test");
    }

    #[test]
    fn hardware_profile_serializes_optional_fields() {
        let hp = HardwareProfile::placeholder();
        let value: serde_json::Value = serde_json::to_value(&hp).unwrap();
        // os_name should always be present; gpu_* may be null
        assert!(value.get("os_name").is_some());
    }
}
