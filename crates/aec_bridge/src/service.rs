//! Plain-Rust service layer for the bridge.
//!
//! Everything the Electron renderer can do ultimately calls into one of
//! these methods. Keeping the napi wrappers thin and the logic here makes
//! this layer trivially testable.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_core::config::ProjectSettings;
use aec_core::package::{ProjectPackage, ProjectSummary as CoreProjectSummary};
use aec_core::templates::TemplateLoader;
use aec_core::types::ProjectId;
use aec_governor::profiler::{CpuProfile, GpuProfile, HardwareProfiler};
use aec_governor::tier::HardwareTier;

use crate::recents::{RecentsStore, RecentsStoreError};

#[derive(Debug, Error)]
pub enum BridgeServiceError {
    #[error("core: {0}")]
    Core(String),
    #[error("recents: {0}")]
    Recents(#[from] RecentsStoreError),
    #[error("template: {0}")]
    Template(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<aec_core::error::AecError> for BridgeServiceError {
    fn from(e: aec_core::error::AecError) -> Self {
        Self::Core(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateChoice {
    pub key: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub name: String,
    pub path: String,
    /// Last-write timestamp from the manifest. The N-API layer renders
    /// this as ISO-8601 and the TypeScript `ProjectSummary.modifiedAt`
    /// field consumes it directly. Carrying it through the service layer
    /// (instead of resynthesising it in the bridge) is what lets the
    /// recents list show a real timestamp on Home/dashboard tiles.
    pub updated_at: DateTime<Utc>,
    pub template_id: Option<String>,
}

impl From<CoreProjectSummary> for ProjectSummary {
    fn from(s: CoreProjectSummary) -> Self {
        Self {
            project_id: s.project_id,
            name: s.name,
            path: s.path,
            updated_at: s.updated_at,
            template_id: s.template_id,
        }
    }
}

/// Hardware-status snapshot. The shape mirrors the TypeScript
/// `RuntimeStatus` interface in `apps/desktop/electron/bridge.ts` so that
/// the JS bridge can hand the value to React components without a runtime
/// transformation. The N-API layer turns this struct into a JS object with
/// nested `cpu`/`gpu` fields and a PascalCase `tier` string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatusReport {
    pub tier: HardwareTier,
    pub cpu: CpuProfile,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    /// `None` when the bridge runs outside Electron (e.g. CLI tools, the
    /// Vitest fallback path). The Electron main process owns the wgpu
    /// instance and supplies the real GPU descriptor via
    /// [`BridgeService::runtime_status_with_gpu`].
    pub gpu: Option<GpuProfile>,
    pub os: String,
}

/// Configuration for [`BridgeService`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Directory that holds `recents.json`.
    pub state_dir: PathBuf,
    /// Directory the bridge will create new project packages under.
    pub projects_dir: PathBuf,
    /// Where bundled template JSON files live.
    pub templates_dir: PathBuf,
    /// Maximum number of entries to retain in the recents store.
    pub max_recents: usize,
}

pub struct BridgeService {
    config: BridgeConfig,
    recents: RecentsStore,
    /// Master key for SQLCipher derivation. In production the Electron
    /// main process keeps this in the OS keychain; in tests we pass a
    /// well-known value.
    master_key: [u8; 32],
}

impl BridgeService {
    pub fn new(config: BridgeConfig, master_key: [u8; 32]) -> Result<Self, BridgeServiceError> {
        std::fs::create_dir_all(&config.state_dir)?;
        std::fs::create_dir_all(&config.projects_dir)?;
        let recents =
            RecentsStore::open(config.state_dir.join("recents.json"), config.max_recents)?;
        Ok(Self {
            config,
            recents,
            master_key,
        })
    }

    /// List bundled templates available for the New Project flow.
    pub fn list_templates(&self) -> Result<Vec<TemplateChoice>, BridgeServiceError> {
        let loader = TemplateLoader::new(self.config.templates_dir.clone());
        let mut out = Vec::new();
        for key in loader
            .discover()
            .map_err(|e| BridgeServiceError::Template(e.to_string()))?
        {
            match loader.load(&key) {
                Ok(def) => out.push(TemplateChoice {
                    key,
                    name: def.name,
                    description: def.description,
                }),
                Err(e) => return Err(BridgeServiceError::Template(e.to_string())),
            }
        }
        Ok(out)
    }

    /// Create a new project on disk from a template.
    pub fn project_create_from_template(
        &mut self,
        template_key: &str,
        project_name: &str,
    ) -> Result<ProjectSummary, BridgeServiceError> {
        let loader = TemplateLoader::new(self.config.templates_dir.clone());
        let template = loader
            .load(template_key)
            .map_err(|e| BridgeServiceError::Template(e.to_string()))?;

        let slug = slugify(project_name);
        let root = self.config.projects_dir.join(format!("{slug}.aecstudio"));
        let settings = ProjectSettings {
            units: template.units,
            region: template.primary_region(),
            ..ProjectSettings::default()
        };
        let pkg = ProjectPackage::create(
            &root,
            project_name,
            settings,
            Some(template.template_id.clone()),
            &self.master_key,
        )?;
        let summary: ProjectSummary = pkg.summary().into();
        let core_summary = pkg.summary();
        self.recents.record(&core_summary)?;
        Ok(summary)
    }

    /// Open an existing project package and update the recents store.
    pub fn project_open(&mut self, path: &str) -> Result<ProjectSummary, BridgeServiceError> {
        let pkg = ProjectPackage::open(path)?;
        let core_summary = pkg.summary();
        let summary: ProjectSummary = core_summary.clone().into();
        self.recents.record(&core_summary)?;
        Ok(summary)
    }

    /// Persist the manifest of an open project.
    pub fn project_save(&mut self, path: &str) -> Result<ProjectSummary, BridgeServiceError> {
        let mut pkg = ProjectPackage::open(path)?;
        pkg.save()?;
        Ok(pkg.summary().into())
    }

    /// Return the recents list (most-recent first), in the API shape.
    pub fn project_list_recents(&self) -> Result<Vec<ProjectSummary>, BridgeServiceError> {
        Ok(self
            .recents
            .entries()
            .iter()
            .map(|e| ProjectSummary {
                project_id: e.project_id.clone(),
                name: e.name.clone(),
                path: e.path.clone(),
                // Recents tracks the last-opened moment rather than the
                // manifest's `updated_at`. For the Home page "recent
                // projects" tiles the user-perceived freshness is when
                // they last touched it, so use that here. If/when the
                // recents store grows a separate `updated_at` field we
                // can prefer it.
                updated_at: e.last_opened_at,
                template_id: None,
            })
            .collect())
    }

    /// Snapshot of the host hardware. Read fresh each call so the status
    /// bar reflects current OS-level state.
    ///
    /// GPU detection lives in the Electron main process (which owns the
    /// wgpu instance). When the bridge runs outside Electron — e.g. from
    /// CLI tools, unit tests, or the Vitest fallback — we report `None`
    /// for `gpu` rather than fabricating a "software" placeholder.
    /// Callers that DO have a GPU descriptor should use
    /// [`Self::runtime_status_with_gpu`] instead.
    pub fn runtime_status(&self) -> RuntimeStatusReport {
        Self::runtime_status_impl(None)
    }

    /// Same as [`Self::runtime_status`] but with a caller-supplied GPU
    /// descriptor. The Electron main process calls this with the wgpu
    /// adapter info it already has, avoiding a second probe.
    pub fn runtime_status_with_gpu(&self, gpu: GpuProfile) -> RuntimeStatusReport {
        Self::runtime_status_impl(Some(gpu))
    }

    fn runtime_status_impl(gpu_hint: Option<GpuProfile>) -> RuntimeStatusReport {
        let mut p = HardwareProfiler::new();
        // The profiler insists on receiving a GPU descriptor so that the
        // classifier can read it back. When the bridge has none we feed
        // a `vram_mb: 0` software placeholder for tier classification
        // ONLY and then drop it from the report so the JS side sees a
        // proper `null` gpu (not a misleading software fake).
        let gpu_for_profile = gpu_hint.clone().unwrap_or_else(|| GpuProfile {
            vendor: "software".into(),
            model: "software".into(),
            vram_mb: 0,
            backend: "software".into(),
            accelerators: Vec::new(),
        });
        let profile = p.profile(gpu_for_profile);
        let tier = HardwareTier::classify(&profile);
        RuntimeStatusReport {
            tier,
            cpu: profile.cpu.clone(),
            total_ram_mb: profile.total_ram_mb,
            available_ram_mb: profile.available_ram_mb,
            gpu: gpu_hint,
            os: profile.os.clone(),
        }
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("project");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_template(root: &std::path::Path, category: &str, id: &str) {
        let category_dir = root.join(category);
        std::fs::create_dir_all(&category_dir).unwrap();
        let key = format!("{category}.{id}");
        let json = serde_json::json!({
            "template_id": key,
            "name": format!("Test {id}"),
            "description": "test fixture",
            "units": "mm",
            "region_defaults": {
                "EU": {"units": "mm", "standards": ["IFC4"]}
            },
            "rooms": [],
            "default_walls": {
                "exterior_thickness_mm": 250,
                "interior_thickness_mm": 100,
                "material": "wall_white"
            },
            "lighting_preset": "daylight",
            "asset_shelf": [],
            "camera_presets": []
        });
        std::fs::write(category_dir.join(format!("{id}.json")), json.to_string()).unwrap();
    }

    fn service() -> (BridgeService, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let projects = tmp.path().join("projects");
        let templates = tmp.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        write_template(&templates, "interior", "apartment");
        let cfg = BridgeConfig {
            state_dir: state,
            projects_dir: projects,
            templates_dir: templates,
            max_recents: 10,
        };
        let s = BridgeService::new(cfg, [42u8; 32]).unwrap();
        (s, tmp)
    }

    #[test]
    fn create_open_save_roundtrip() {
        let (mut s, _g) = service();
        let summary = s
            .project_create_from_template("interior.apartment", "Test Apartment")
            .unwrap();
        assert_eq!(summary.name, "Test Apartment");
        let opened = s.project_open(&summary.path).unwrap();
        assert_eq!(opened.project_id, summary.project_id);
        let saved = s.project_save(&summary.path).unwrap();
        assert_eq!(saved.project_id, summary.project_id);
    }

    #[test]
    fn list_templates_returns_discovered_entries() {
        let (s, _g) = service();
        let entries = s.list_templates().unwrap();
        assert!(entries.iter().any(|t| t.key == "interior.apartment"));
    }

    #[test]
    fn recents_records_created_project() {
        let (mut s, _g) = service();
        s.project_create_from_template("interior.apartment", "P1")
            .unwrap();
        let recents = s.project_list_recents().unwrap();
        assert!(recents.iter().any(|r| r.name == "P1"));
    }

    #[test]
    fn runtime_status_returns_finite_values() {
        let (s, _g) = service();
        let r = s.runtime_status();
        assert!(r.total_ram_mb > 0);
        assert!(r.cpu.physical_cores > 0);
        // Outside Electron the bridge has no wgpu adapter; gpu must be
        // None rather than a misleading "software" placeholder.
        assert!(r.gpu.is_none());
    }

    #[test]
    fn runtime_status_with_gpu_carries_descriptor_through() {
        let (s, _g) = service();
        let gpu = GpuProfile {
            vendor: "TestCorp".into(),
            model: "TestGPU".into(),
            vram_mb: 8192,
            backend: "vulkan".into(),
            accelerators: vec!["vulkan".into()],
        };
        let r = s.runtime_status_with_gpu(gpu.clone());
        assert_eq!(r.gpu.as_ref().unwrap().vram_mb, 8192);
        assert_eq!(r.gpu.as_ref().unwrap().vendor, "TestCorp");
    }

    #[test]
    fn slugify_handles_unicode_and_punctuation() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("   "), "project");
        assert_eq!(slugify("Loft 12B"), "loft-12b");
    }
}
