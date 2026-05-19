//! Template loader for `templates/<category>/<id>.json` files.
//!
//! Template authors declare rooms, default walls, lighting presets, and
//! asset shelves. The loader resolves a `<category>.<id>` key (e.g.
//! `interior.apartment`) to a parsed [`TemplateDefinition`] and is used by
//! [`crate::ProjectPackage::create`] to seed a new project.
//!
//! The on-disk schema is intentionally richer than a flat-room model:
//! * `region_defaults` is a per-region map (EU/NA/APAC) so each region can
//!   carry its own units and drafting standards.
//! * `default_walls` is a config object describing the project's default
//!   exterior/interior wall thicknesses, not a list of authored walls.
//! * Single-storey templates (apartment, kitchen, ...) populate the flat
//!   `rooms` list. Multi-storey templates (`architecture/villa.json`) use
//!   `storeys` instead.
//! * Drafting templates (`drafting/2d_drafting.json`) carry `sheet_presets`
//!   and `dim_styles` and have an empty `rooms` list.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AecError, AecResult};
use crate::types::{Region, Units};

/// Templates use millimeters for all dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateDefinition {
    pub template_id: String,
    /// Filesystem category (`interior`, `architecture`, `drafting`).
    /// Present in the shipped JSONs but optional so older fixtures still
    /// parse.
    #[serde(default)]
    pub category: Option<String>,
    pub name: String,
    pub description: String,
    /// Per-region defaults bundle (units + standards). Keyed by [`Region`].
    pub region_defaults: BTreeMap<Region, RegionDefaults>,
    pub units: Units,
    /// Flat rooms list. Single-storey templates populate this directly;
    /// multi-storey templates leave it empty and use `storeys` instead.
    #[serde(default)]
    pub rooms: Vec<TemplateRoom>,
    /// Multi-storey hierarchy. Set for villa-like templates; empty for
    /// flat (single-level) templates.
    #[serde(default)]
    pub storeys: Vec<TemplateStorey>,
    #[serde(default)]
    pub default_walls: WallDefaults,
    /// Lighting preset to seed the project with. `None` for drafting-only
    /// templates.
    #[serde(default)]
    pub lighting_preset: Option<String>,
    #[serde(default)]
    pub asset_shelf: Vec<String>,
    #[serde(default)]
    pub camera_presets: Vec<TemplateCamera>,
    /// Drafting-only: pre-canned sheet sizes / title blocks.
    #[serde(default)]
    pub sheet_presets: Vec<SheetPreset>,
    /// Drafting-only: dimension styles to include in the project.
    #[serde(default)]
    pub dim_styles: Vec<String>,
}

impl TemplateDefinition {
    /// Pick a canonical region for project creation when the user hasn't
    /// explicitly chosen one. Templates typically list EU/NA/APAC; we
    /// prefer EU, then NA, then APAC, then fall back to [`Region::Eu`].
    pub fn primary_region(&self) -> Region {
        for r in [Region::Eu, Region::Na, Region::Apac] {
            if self.region_defaults.contains_key(&r) {
                return r;
            }
        }
        Region::Eu
    }

    /// Yield every room across the template, walking through any storey
    /// hierarchy. Single-storey templates yield from the flat list, while
    /// multi-storey templates yield from each storey in order.
    pub fn iter_rooms(&self) -> impl Iterator<Item = &TemplateRoom> {
        self.rooms
            .iter()
            .chain(self.storeys.iter().flat_map(|s| s.rooms.iter()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionDefaults {
    pub units: Units,
    /// Drafting / BIM standards to apply when this region is selected
    /// (e.g. `["EN ISO 5457", "IFC4"]`).
    #[serde(default)]
    pub standards: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateRoom {
    pub name: String,
    pub width_mm: f64,
    pub depth_mm: f64,
    pub height_mm: f64,
    /// Origin of the room's south-west corner in project mm. Defaults to
    /// `[0, 0, 0]` when the template author omits it.
    #[serde(default)]
    pub origin_mm: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateStorey {
    pub name: String,
    pub elevation_mm: f64,
    #[serde(default)]
    pub rooms: Vec<TemplateRoom>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallDefaults {
    pub exterior_thickness_mm: f64,
    pub interior_thickness_mm: f64,
    /// Default wall finish material. `None` for templates that don't
    /// preset a material (e.g. drafting).
    #[serde(default)]
    pub material: Option<String>,
}

impl Default for WallDefaults {
    fn default() -> Self {
        Self {
            exterior_thickness_mm: 250.0,
            interior_thickness_mm: 100.0,
            material: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateCamera {
    pub name: String,
    /// 3D location of the camera (mm). Shipped template JSON uses
    /// `location_mm`; the legacy `position_mm` key is accepted as an
    /// alias so older fixtures keep deserialising.
    #[serde(alias = "position_mm")]
    pub location_mm: [f64; 3],
    pub target_mm: [f64; 3],
    pub focal_length_mm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetPreset {
    pub name: String,
    /// Sheet size in mm `[width, height]`.
    pub size_mm: [f64; 2],
    pub title_block: String,
}

#[derive(Debug)]
pub struct TemplateLoader {
    root: PathBuf,
}

impl TemplateLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a `<category>.<id>` key to a parsed template.
    pub fn load(&self, key: &str) -> AecResult<TemplateDefinition> {
        let (category, id) = key
            .split_once('.')
            .ok_or_else(|| AecError::TemplateNotFound(key.to_string()))?;
        let path = self.root.join(category).join(format!("{id}.json"));
        if !path.is_file() {
            return Err(AecError::TemplateNotFound(key.to_string()));
        }
        let raw = fs::read_to_string(&path)?;
        let tpl: TemplateDefinition =
            serde_json::from_str(&raw).map_err(|e| AecError::InvalidTemplate(e.to_string()))?;
        if tpl.template_id != key {
            return Err(AecError::InvalidTemplate(format!(
                "template_id in file is '{}' but path key is '{}'",
                tpl.template_id, key
            )));
        }
        Ok(tpl)
    }

    /// Enumerate every category/file under `root`.
    pub fn discover(&self) -> AecResult<Vec<String>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for cat in fs::read_dir(&self.root)? {
            let cat = cat?;
            if !cat.file_type()?.is_dir() {
                continue;
            }
            let category = cat.file_name().to_string_lossy().to_string();
            for file in fs::read_dir(cat.path())? {
                let file = file?;
                if let Some(name) = file.file_name().to_str() {
                    if let Some(stem) = name.strip_suffix(".json") {
                        out.push(format!("{category}.{stem}"));
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }
}

/// Validate a template path on disk by attempting to load it. Used by tests
/// to ensure shipped templates parse cleanly.
pub fn validate_template_dir(path: &Path) -> AecResult<usize> {
    let loader = TemplateLoader::new(path);
    let keys = loader.discover()?;
    for k in &keys {
        loader.load(k)?;
    }
    Ok(keys.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_template_json(id: &str) -> String {
        format!(
            r#"{{
                "template_id": "interior.{id}",
                "category": "interior",
                "name": "Sample {id}",
                "description": "test",
                "region_defaults": {{
                    "EU": {{"units": "mm", "standards": ["IFC4"]}},
                    "NA": {{"units": "inches", "standards": ["IFC4"]}}
                }},
                "units": "mm",
                "rooms": [
                    {{ "name": "Living", "width_mm": 4500, "depth_mm": 3500, "height_mm": 2700 }}
                ],
                "default_walls": {{
                    "exterior_thickness_mm": 250,
                    "interior_thickness_mm": 100,
                    "material": "wall_white"
                }},
                "lighting_preset": "warm_evening",
                "asset_shelf": ["asset_sofa_modern_3seat_v2"],
                "camera_presets": [
                    {{ "name": "Hero", "location_mm": [3000, -2000, 1500], "target_mm": [0,0,1200], "focal_length_mm": 35 }}
                ]
            }}"#,
        )
    }

    #[test]
    fn loader_parses_real_shipped_template() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(
            cat_dir.join("apartment.json"),
            flat_template_json("apartment"),
        )
        .unwrap();
        let loader = TemplateLoader::new(td.path());
        let tpl = loader.load("interior.apartment").unwrap();
        assert_eq!(tpl.name, "Sample apartment");
        assert_eq!(tpl.rooms.len(), 1);
        // Origin defaults to [0,0,0] when omitted.
        assert_eq!(tpl.rooms[0].origin_mm, [0.0, 0.0, 0.0]);
        assert_eq!(tpl.region_defaults.len(), 2);
        assert_eq!(tpl.region_defaults[&Region::Na].units, Units::Inches);
        assert_eq!(tpl.default_walls.exterior_thickness_mm, 250.0);
        assert_eq!(tpl.lighting_preset.as_deref(), Some("warm_evening"));
        assert_eq!(tpl.camera_presets[0].location_mm, [3000.0, -2000.0, 1500.0]);
    }

    #[test]
    fn loader_rejects_mismatched_template_id() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(
            cat_dir.join("apartment.json"),
            flat_template_json("kitchen"),
        )
        .unwrap();
        let loader = TemplateLoader::new(td.path());
        let err = loader.load("interior.apartment").unwrap_err();
        match err {
            AecError::InvalidTemplate(_) => {}
            other => panic!("expected InvalidTemplate, got {other:?}"),
        }
    }

    #[test]
    fn loader_discovers_all_files() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(
            cat_dir.join("apartment.json"),
            flat_template_json("apartment"),
        )
        .unwrap();
        std::fs::write(cat_dir.join("kitchen.json"), flat_template_json("kitchen")).unwrap();
        let loader = TemplateLoader::new(td.path());
        let mut keys = loader.discover().unwrap();
        keys.sort();
        assert_eq!(keys, vec!["interior.apartment", "interior.kitchen"]);
    }

    #[test]
    fn missing_template_reports_not_found() {
        let td = tempfile::tempdir().unwrap();
        let loader = TemplateLoader::new(td.path());
        let err = loader.load("interior.nope").unwrap_err();
        matches!(err, AecError::TemplateNotFound(_));
    }

    #[test]
    fn primary_region_prefers_eu_then_na() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(cat_dir.join("a.json"), flat_template_json("a")).unwrap();
        let loader = TemplateLoader::new(td.path());
        let tpl = loader.load("interior.a").unwrap();
        assert_eq!(tpl.primary_region(), Region::Eu);
    }

    #[test]
    fn multi_storey_template_flattens_via_iter_rooms() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("architecture");
        std::fs::create_dir_all(&cat_dir).unwrap();
        let json = r#"{
            "template_id": "architecture.villa",
            "category": "architecture",
            "name": "Villa",
            "description": "Multi-storey",
            "region_defaults": {"EU": {"units": "mm", "standards": ["IFC4"]}},
            "units": "mm",
            "storeys": [
                {"name": "Ground", "elevation_mm": 0, "rooms": [
                    {"name": "Foyer", "width_mm": 3000, "depth_mm": 4000, "height_mm": 2700},
                    {"name": "Living", "width_mm": 5500, "depth_mm": 4500, "height_mm": 2700}
                ]},
                {"name": "Upper", "elevation_mm": 3200, "rooms": [
                    {"name": "Master", "width_mm": 4500, "depth_mm": 4000, "height_mm": 2700}
                ]}
            ],
            "default_walls": {"exterior_thickness_mm": 350, "interior_thickness_mm": 120, "material": null},
            "lighting_preset": "daylight",
            "asset_shelf": [],
            "camera_presets": []
        }"#;
        std::fs::write(cat_dir.join("villa.json"), json).unwrap();
        let tpl = TemplateLoader::new(td.path())
            .load("architecture.villa")
            .unwrap();
        assert_eq!(tpl.rooms.len(), 0);
        assert_eq!(tpl.storeys.len(), 2);
        let all: Vec<&str> = tpl.iter_rooms().map(|r| r.name.as_str()).collect();
        assert_eq!(all, vec!["Foyer", "Living", "Master"]);
    }

    #[test]
    fn drafting_template_with_null_lighting_and_sheet_presets_parses() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("drafting");
        std::fs::create_dir_all(&cat_dir).unwrap();
        let json = r#"{
            "template_id": "drafting.2d",
            "category": "drafting",
            "name": "2D",
            "description": "drafting only",
            "region_defaults": {"EU": {"units": "mm", "standards": ["ISO 7200"]}},
            "units": "mm",
            "rooms": [],
            "default_walls": {"exterior_thickness_mm": 250, "interior_thickness_mm": 100, "material": null},
            "lighting_preset": null,
            "sheet_presets": [
                {"name": "A1", "size_mm": [841, 594], "title_block": "iso_7200"}
            ],
            "dim_styles": ["arch_metric"]
        }"#;
        std::fs::write(cat_dir.join("2d.json"), json).unwrap();
        let tpl = TemplateLoader::new(td.path()).load("drafting.2d").unwrap();
        assert!(tpl.lighting_preset.is_none());
        assert_eq!(tpl.sheet_presets.len(), 1);
        assert_eq!(tpl.sheet_presets[0].size_mm, [841.0, 594.0]);
        assert_eq!(tpl.dim_styles, vec!["arch_metric"]);
    }

    #[test]
    fn position_mm_alias_still_accepted_for_camera_presets() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        let json = r#"{
            "template_id": "interior.legacy",
            "name": "Legacy",
            "description": "uses position_mm not location_mm",
            "region_defaults": {"EU": {"units": "mm", "standards": []}},
            "units": "mm",
            "rooms": [],
            "default_walls": {"exterior_thickness_mm": 250, "interior_thickness_mm": 100},
            "lighting_preset": "daylight",
            "asset_shelf": [],
            "camera_presets": [
                {"name": "Hero", "position_mm": [1, 2, 3], "target_mm": [0,0,0], "focal_length_mm": 35}
            ]
        }"#;
        std::fs::write(cat_dir.join("legacy.json"), json).unwrap();
        let tpl = TemplateLoader::new(td.path())
            .load("interior.legacy")
            .unwrap();
        assert_eq!(tpl.camera_presets[0].location_mm, [1.0, 2.0, 3.0]);
    }
}
