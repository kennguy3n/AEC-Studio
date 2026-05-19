//! Material library: load bundled packs, query by tag/style/vendor.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::material::PbrMaterial;

#[derive(Debug, Error)]
pub enum MaterialLibraryError {
    #[error("duplicate material id `{0}`")]
    DuplicateId(String),
    #[error("material `{0}` not found")]
    NotFound(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialQuery {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub style_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Clone)]
pub struct MaterialLibrary {
    materials: BTreeMap<String, PbrMaterial>,
}

impl MaterialLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, m: PbrMaterial) -> Result<(), MaterialLibraryError> {
        if self.materials.contains_key(&m.id) {
            return Err(MaterialLibraryError::DuplicateId(m.id));
        }
        self.materials.insert(m.id.clone(), m);
        Ok(())
    }

    pub fn upsert(&mut self, m: PbrMaterial) {
        self.materials.insert(m.id.clone(), m);
    }

    pub fn get(&self, id: &str) -> Option<&PbrMaterial> {
        self.materials.get(id)
    }

    pub fn len(&self) -> usize {
        self.materials.len()
    }

    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PbrMaterial> {
        self.materials.values()
    }

    pub fn query(&self, q: &MaterialQuery) -> Vec<&PbrMaterial> {
        let mut out: Vec<&PbrMaterial> = self
            .materials
            .values()
            .filter(|m| {
                q.tags.iter().all(|tag| m.tags.iter().any(|t| t == tag))
                    && q.style_tags
                        .iter()
                        .all(|tag| m.style_tags.iter().any(|t| t == tag))
                    && q.vendor_id
                        .as_ref()
                        .map_or(true, |v| m.vendor_id.as_deref() == Some(v.as_str()))
                    && q.name_contains.as_ref().map_or(true, |needle| {
                        m.name.to_lowercase().contains(&needle.to_lowercase())
                    })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(limit) = q.limit {
            out.truncate(limit);
        }
        out
    }

    /// Load a JSON list of [`PbrMaterial`] objects.
    pub fn load_pack(&mut self, json: &str) -> Result<usize, MaterialLibraryError> {
        let mats: Vec<PbrMaterial> = serde_json::from_str(json)?;
        let mut added = 0;
        for m in mats {
            self.upsert(m);
            added += 1;
        }
        Ok(added)
    }

    /// Bundled default pack used for the starter UI experience. Real
    /// production packs ship as separate JSON files and are loaded via
    /// [`load_pack`].
    pub fn with_default_pack() -> Self {
        let mut lib = Self::new();
        let materials = [
            PbrMaterial::new("mat:oak_light", "Light Oak")
                .with_albedo([0.78, 0.66, 0.5])
                .with_style_tags(["scandinavian".into(), "warm".into()]),
            PbrMaterial::new("mat:walnut", "Walnut")
                .with_albedo([0.34, 0.21, 0.14])
                .with_style_tags(["industrial".into(), "warm".into()]),
            PbrMaterial::new("mat:concrete_polished", "Polished Concrete")
                .with_albedo([0.55, 0.55, 0.56])
                .with_style_tags(["industrial".into(), "minimal".into()]),
            PbrMaterial::new("mat:linen_oat", "Oat Linen")
                .with_albedo([0.85, 0.78, 0.66])
                .with_style_tags(["japandi".into(), "warm".into()]),
            PbrMaterial::new("mat:matte_white", "Matte White Paint")
                .with_albedo([0.92, 0.92, 0.91])
                .with_style_tags(["minimal".into()]),
            PbrMaterial::new("mat:terracotta", "Terracotta Tile")
                .with_albedo([0.78, 0.42, 0.32])
                .with_style_tags(["mediterranean".into(), "warm".into()]),
            PbrMaterial::new("mat:brushed_brass", "Brushed Brass")
                .with_albedo([0.78, 0.68, 0.42])
                .with_style_tags(["art_deco".into(), "warm".into()]),
            PbrMaterial::new("mat:marble_carrara", "Carrara Marble")
                .with_albedo([0.92, 0.92, 0.93])
                .with_style_tags(["classical".into(), "minimal".into()]),
        ];
        for mut m in materials {
            // Roughness/metallic tuning per material type.
            if m.id.contains("concrete") || m.id.contains("marble") {
                m.roughness = 0.45;
            }
            if m.id.contains("brass") {
                m.metallic = 0.9;
                m.roughness = 0.3;
            }
            if m.id.contains("linen") {
                m.roughness = 0.85;
            }
            lib.upsert(m);
        }
        lib
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pack_has_eight_materials() {
        let lib = MaterialLibrary::with_default_pack();
        assert_eq!(lib.len(), 8);
    }

    #[test]
    fn query_by_style_tag_filters_results() {
        let lib = MaterialLibrary::with_default_pack();
        let q = MaterialQuery {
            style_tags: vec!["warm".into()],
            ..MaterialQuery::default()
        };
        let hits = lib.query(&q);
        assert!(!hits.is_empty());
        for m in hits {
            assert!(m.style_tags.iter().any(|s| s == "warm"));
        }
    }

    #[test]
    fn duplicate_add_errors() {
        let mut lib = MaterialLibrary::new();
        lib.add(PbrMaterial::new("a", "A")).unwrap();
        assert!(lib.add(PbrMaterial::new("a", "A2")).is_err());
    }

    #[test]
    fn load_pack_inserts_materials() {
        let mut lib = MaterialLibrary::new();
        let json = serde_json::to_string(&vec![PbrMaterial::new("b", "B")]).unwrap();
        let n = lib.load_pack(&json).unwrap();
        assert_eq!(n, 1);
        assert!(lib.get("b").is_some());
    }
}
