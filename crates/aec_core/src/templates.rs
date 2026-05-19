//! Template loader for `templates/<category>/<id>.json` files.
//!
//! Template authors declare rooms, default walls, lighting presets, and
//! asset shelves. The loader resolves a `<category>.<id>` key (e.g.
//! `interior.apartment`) to a parsed [`TemplateDefinition`] and is used by
//! [`crate::ProjectPackage::create`] to seed a new project.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AecError, AecResult};
use crate::types::{Region, Units};

/// Templates use millimeters for all dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateDefinition {
    pub template_id: String,
    pub name: String,
    pub description: String,
    pub region_defaults: Region,
    pub units: Units,
    pub rooms: Vec<TemplateRoom>,
    pub default_walls: Vec<TemplateWall>,
    pub lighting_preset: String,
    pub asset_shelf: Vec<String>,
    pub camera_presets: Vec<TemplateCamera>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateRoom {
    pub name: String,
    pub width_mm: f64,
    pub depth_mm: f64,
    pub height_mm: f64,
    /// Origin of the room's south-west corner in project mm.
    pub origin_mm: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateWall {
    pub start_mm: [f64; 2],
    pub end_mm: [f64; 2],
    pub height_mm: f64,
    pub thickness_mm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateCamera {
    pub name: String,
    pub position_mm: [f64; 3],
    pub target_mm: [f64; 3],
    pub focal_length_mm: f64,
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

    fn sample_template_json(id: &str) -> String {
        format!(
            r#"{{
                "template_id": "interior.{id}",
                "name": "Sample {id}",
                "description": "test",
                "region_defaults": "eu",
                "units": "mm",
                "rooms": [
                    {{ "name": "Living", "width_mm": 4500, "depth_mm": 3500, "height_mm": 2700, "origin_mm": [0,0,0] }}
                ],
                "default_walls": [],
                "lighting_preset": "warm_evening",
                "asset_shelf": ["asset_sofa_modern_3seat_v2"],
                "camera_presets": [
                    {{ "name": "Hero", "position_mm": [3000, -2000, 1500], "target_mm": [0,0,1200], "focal_length_mm": 35 }}
                ]
            }}"#,
        )
    }

    #[test]
    fn loader_parses_and_resolves_key() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(
            cat_dir.join("apartment.json"),
            sample_template_json("apartment"),
        )
        .unwrap();
        let loader = TemplateLoader::new(td.path());
        let tpl = loader.load("interior.apartment").unwrap();
        assert_eq!(tpl.name, "Sample apartment");
        assert_eq!(tpl.rooms.len(), 1);
    }

    #[test]
    fn loader_rejects_mismatched_template_id() {
        let td = tempfile::tempdir().unwrap();
        let cat_dir = td.path().join("interior");
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(
            cat_dir.join("apartment.json"),
            sample_template_json("kitchen"),
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
            sample_template_json("apartment"),
        )
        .unwrap();
        std::fs::write(
            cat_dir.join("kitchen.json"),
            sample_template_json("kitchen"),
        )
        .unwrap();
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
}
