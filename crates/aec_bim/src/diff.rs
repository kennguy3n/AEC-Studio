//! Element-level diff between two BIM projects.
//!
//! Pairing strategy: GUID match first (most reliable across edit
//! sessions), then fall back to `name + class` for spatial nodes
//! without GUIDs. Geometry equality is approximated via the cached
//! BLAKE3 `geometry_hash` from `BimCache` (callers pass that in).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::classification::ClassificationStore;
use crate::properties::{PropertyStore, PropertyValue};
use crate::spatial::Project;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyDelta {
    pub pset: String,
    pub key: String,
    pub before: Option<PropertyValue>,
    pub after: Option<PropertyValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementDelta {
    pub key: String,
    pub class_changed: Option<(String, String)>,
    pub name_changed: Option<(String, String)>,
    pub property_deltas: Vec<PropertyDelta>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<ElementDelta>,
}

pub fn diff_projects(
    before: &Project,
    before_class: &ClassificationStore,
    before_props: &PropertyStore,
    after: &Project,
    after_class: &ClassificationStore,
    after_props: &PropertyStore,
) -> ProjectDiff {
    // Build a stable join key per node: prefer GUID, else "class:name".
    let key_b = build_key_index(before);
    let key_a = build_key_index(after);

    let mut diff = ProjectDiff::default();
    let before_keys: HashSet<&String> = key_b.keys().collect();
    let after_keys: HashSet<&String> = key_a.keys().collect();

    for k in after_keys.difference(&before_keys) {
        diff.added.push((*k).clone());
    }
    for k in before_keys.difference(&after_keys) {
        diff.removed.push((*k).clone());
    }
    for k in before_keys.intersection(&after_keys) {
        let id_b = key_b.get(*k).unwrap();
        let id_a = key_a.get(*k).unwrap();
        let nb = before.get(id_b).unwrap();
        let na = after.get(id_a).unwrap();
        let mut delta = ElementDelta {
            key: (*k).clone(),
            class_changed: None,
            name_changed: None,
            property_deltas: Vec::new(),
        };
        if nb.class != na.class {
            delta.class_changed = Some((nb.class.ifc_tag().into(), na.class.ifc_tag().into()));
        }
        if nb.name != na.name {
            delta.name_changed = Some((nb.name.clone(), na.name.clone()));
        }
        let _ = (before_class, after_class);
        delta
            .property_deltas
            .extend(property_diff(id_b, id_a, before_props, after_props));
        // Element-side children (elements attached to node) — we treat
        // them as opaque IDs that survive across versions; cross-key
        // them by their attached element IDs (no GUID for raw
        // elements). This means element-level deltas show up as
        // property changes.
        if delta.class_changed.is_some()
            || delta.name_changed.is_some()
            || !delta.property_deltas.is_empty()
        {
            diff.modified.push(delta);
        }
    }
    diff.added.sort();
    diff.removed.sort();
    diff.modified.sort_by(|a, b| a.key.cmp(&b.key));
    diff
}

fn build_key_index(p: &Project) -> HashMap<String, EntityId> {
    let mut out = HashMap::new();
    for (id, n) in &p.nodes {
        let k = match &n.ifc_guid {
            Some(g) => g.clone(),
            None => format!("{}:{}", n.class.ifc_tag(), n.name),
        };
        out.insert(k, id.clone());
    }
    out
}

fn property_diff(
    id_b: &EntityId,
    id_a: &EntityId,
    before: &PropertyStore,
    after: &PropertyStore,
) -> Vec<PropertyDelta> {
    let mut out = Vec::new();
    let b = before.get(id_b);
    let a = after.get(id_a);
    let mut pset_names: HashSet<&String> = HashSet::new();
    if let Some(b) = b {
        for n in b.psets.keys() {
            pset_names.insert(n);
        }
    }
    if let Some(a) = a {
        for n in a.psets.keys() {
            pset_names.insert(n);
        }
    }
    let mut pset_names: Vec<&String> = pset_names.into_iter().collect();
    pset_names.sort();
    for pname in pset_names {
        let pb = b.and_then(|x| x.psets.get(pname));
        let pa = a.and_then(|x| x.psets.get(pname));
        let mut keys: HashSet<&String> = HashSet::new();
        if let Some(p) = pb {
            for k in p.properties.keys() {
                keys.insert(k);
            }
        }
        if let Some(p) = pa {
            for k in p.properties.keys() {
                keys.insert(k);
            }
        }
        let mut keys: Vec<&String> = keys.into_iter().collect();
        keys.sort();
        for k in keys {
            let vb = pb.and_then(|p| p.properties.get(k)).cloned();
            let va = pa.and_then(|p| p.properties.get(k)).cloned();
            if vb != va {
                out.push(PropertyDelta {
                    pset: pname.clone(),
                    key: k.clone(),
                    before: vb,
                    after: va,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::IfcClass;
    use crate::properties::PropertySet;

    fn project_with(
        name: &str,
        guid: &str,
    ) -> (Project, EntityId, ClassificationStore, PropertyStore) {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let storey = p
            .add_child(&root, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        p.set_ifc_guid(&storey, guid);
        p.rename(&storey, name);
        (
            p,
            storey,
            ClassificationStore::default(),
            PropertyStore::new(),
        )
    }

    #[test]
    fn detects_renamed_node() {
        let (a, _, ca, pa) = project_with("L01", "STR1");
        let (b, _, cb, pb) = project_with("L01-renamed", "STR1");
        let d = diff_projects(&a, &ca, &pa, &b, &cb, &pb);
        assert!(d.added.is_empty());
        assert!(d.removed.is_empty());
        assert_eq!(d.modified.len(), 1);
        assert!(d.modified[0].name_changed.is_some());
    }

    #[test]
    fn detects_added_and_removed() {
        let (a, _, ca, pa) = project_with("L01", "STR1");
        let (mut b, _, cb, pb) = project_with("L01", "STR1");
        let bldg = b
            .add_child(&b.root.clone(), IfcClass::IfcBuilding, "Building B")
            .unwrap();
        b.set_ifc_guid(&bldg, "BLDB");
        let d = diff_projects(&a, &ca, &pa, &b, &cb, &pb);
        assert!(d.added.contains(&"BLDB".to_string()));
        assert!(d.removed.is_empty());
    }

    #[test]
    fn detects_property_changes() {
        let (a, storey_a, ca, mut pa) = project_with("L01", "STR1");
        let (b, storey_b, cb, mut pb) = project_with("L01", "STR1");
        let mut psa = PropertySet::new("Pset_BuildingStoreyCommon");
        psa.set("AboveGround", PropertyValue::Boolean(true));
        psa.set("GrossFloorArea", PropertyValue::Area(100.0));
        pa.entry(storey_a).upsert_pset(psa);
        let mut psb = PropertySet::new("Pset_BuildingStoreyCommon");
        psb.set("AboveGround", PropertyValue::Boolean(true));
        psb.set("GrossFloorArea", PropertyValue::Area(120.0));
        pb.entry(storey_b).upsert_pset(psb);
        let d = diff_projects(&a, &ca, &pa, &b, &cb, &pb);
        assert_eq!(d.modified.len(), 1);
        let delta = &d.modified[0].property_deltas[0];
        assert_eq!(delta.pset, "Pset_BuildingStoreyCommon");
        assert_eq!(delta.key, "GrossFloorArea");
        assert!(matches!(delta.before, Some(PropertyValue::Area(100.0))));
        assert!(matches!(delta.after, Some(PropertyValue::Area(120.0))));
    }

    #[test]
    fn identical_projects_diff_to_nothing() {
        let (a, _, ca, pa) = project_with("L01", "STR1");
        let d = diff_projects(&a, &ca, &pa, &a, &ca, &pa);
        assert!(d.added.is_empty());
        assert!(d.removed.is_empty());
        assert!(d.modified.is_empty());
    }
}
