//! BIM cache: per-element geometry hash + Pset hash + classification.
//!
//! Used to skip work on re-import: if an element's geometry+psets+class
//! fingerprint matches the cached entry, we keep the existing geometry/
//! materials/asset bindings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::classification::IfcClass;
use crate::properties::PropertySet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedElement {
    pub entity: EntityId,
    pub class: IfcClass,
    pub geometry_hash: String,
    pub pset_hash: String,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimCache {
    entries: BTreeMap<String, CachedElement>,
}

impl BimCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&mut self, key: impl Into<String>, element: CachedElement) {
        self.entries.insert(key.into(), element);
    }

    pub fn get(&self, key: &str) -> Option<&CachedElement> {
        self.entries.get(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compute the cache key for an `(ifc_guid, geometry_hash, pset_hash)`
    /// tuple. Stable across re-imports.
    pub fn key(ifc_guid: &str, geometry_hash: &str, pset_hash: &str) -> String {
        let mut h = blake3::Hasher::new();
        h.update(ifc_guid.as_bytes());
        h.update(geometry_hash.as_bytes());
        h.update(pset_hash.as_bytes());
        h.finalize().to_hex().to_string()
    }

    /// Combine a list of [`PropertySet`] fingerprints into a single hex hash.
    pub fn hash_psets(psets: &[PropertySet]) -> String {
        let mut h = blake3::Hasher::new();
        for ps in psets {
            h.update(&ps.fingerprint());
        }
        h.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::properties::PropertyValue;

    #[test]
    fn cache_roundtrip_preserves_entry() {
        let mut cache = BimCache::new();
        let key = BimCache::key("01ABC", "geom-x", "ps-y");
        let elem = CachedElement {
            entity: EntityId::new(),
            class: IfcClass::IfcWallStandardCase,
            geometry_hash: "geom-x".into(),
            pset_hash: "ps-y".into(),
        };
        cache.upsert(key.clone(), elem.clone());
        assert_eq!(
            cache.get(&key).unwrap().class.ifc_tag(),
            "IfcWallStandardCase"
        );
    }

    #[test]
    fn pset_hash_changes_with_content() {
        let mut a = PropertySet::new("Pset_WallCommon");
        a.set("LoadBearing", PropertyValue::Boolean(false));
        let h1 = BimCache::hash_psets(&[a.clone()]);
        a.set("LoadBearing", PropertyValue::Boolean(true));
        let h2 = BimCache::hash_psets(&[a]);
        assert_ne!(h1, h2);
    }
}
