//! IFC relationships between spatial nodes and elements.
//!
//! `IfcRelAggregates` connects a parent spatial node to its child
//! spatial nodes (e.g. `IfcSite` → `IfcBuilding`). `IfcRelContainedIn`
//! connects a spatial node to the building elements physically inside
//! it (e.g. `IfcBuildingStorey` → walls/doors/windows on that floor).
//!
//! All relations have a stable IFC GUID so they survive an
//! import/export round-trip.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregateRelation {
    pub guid: String,
    pub relating: EntityId,
    pub related: Vec<EntityId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainmentRelation {
    pub guid: String,
    pub spatial_node: EntityId,
    pub elements: Vec<EntityId>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelationStore {
    aggregates: BTreeMap<String, AggregateRelation>,
    contains: BTreeMap<String, ContainmentRelation>,
}

impl RelationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_aggregate(&mut self, rel: AggregateRelation) {
        self.aggregates.insert(rel.guid.clone(), rel);
    }

    pub fn upsert_containment(&mut self, rel: ContainmentRelation) {
        self.contains.insert(rel.guid.clone(), rel);
    }

    pub fn aggregates(&self) -> impl Iterator<Item = &AggregateRelation> {
        self.aggregates.values()
    }

    pub fn containments(&self) -> impl Iterator<Item = &ContainmentRelation> {
        self.contains.values()
    }

    /// Find the parent (relating) node of a given child node.
    pub fn parent_of(&self, child: &EntityId) -> Option<&EntityId> {
        for rel in self.aggregates.values() {
            if rel.related.iter().any(|c| c == child) {
                return Some(&rel.relating);
            }
        }
        None
    }

    /// Find the spatial node that an element is contained in.
    pub fn container_of(&self, element: &EntityId) -> Option<&EntityId> {
        for rel in self.contains.values() {
            if rel.elements.iter().any(|e| e == element) {
                return Some(&rel.spatial_node);
            }
        }
        None
    }

    /// Remove all references to the given entity (deleted element).
    /// Returns the number of relations actually mutated.
    pub fn purge(&mut self, entity: &EntityId) -> usize {
        let mut hits = 0;
        for rel in self.aggregates.values_mut() {
            let before = rel.related.len();
            rel.related.retain(|c| c != entity);
            if rel.related.len() != before {
                hits += 1;
            }
        }
        for rel in self.contains.values_mut() {
            let before = rel.elements.len();
            rel.elements.retain(|e| e != entity);
            if rel.elements.len() != before {
                hits += 1;
            }
        }
        self.aggregates.retain(|_, r| &r.relating != entity);
        self.contains.retain(|_, r| &r.spatial_node != entity);
        hits
    }

    pub fn len(&self) -> usize {
        self.aggregates.len() + self.contains.len()
    }

    pub fn is_empty(&self) -> bool {
        self.aggregates.is_empty() && self.contains.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_parent_lookup() {
        let mut store = RelationStore::new();
        let parent = EntityId::new();
        let child = EntityId::new();
        store.upsert_aggregate(AggregateRelation {
            guid: "rel-1".into(),
            relating: parent.clone(),
            related: vec![child.clone()],
        });
        assert_eq!(store.parent_of(&child), Some(&parent));
    }

    #[test]
    fn containment_lookup() {
        let mut store = RelationStore::new();
        let storey = EntityId::new();
        let wall = EntityId::new();
        store.upsert_containment(ContainmentRelation {
            guid: "rel-2".into(),
            spatial_node: storey.clone(),
            elements: vec![wall.clone()],
        });
        assert_eq!(store.container_of(&wall), Some(&storey));
    }

    #[test]
    fn purge_drops_entity_from_all_relations() {
        let mut store = RelationStore::new();
        let parent = EntityId::new();
        let child = EntityId::new();
        let element = EntityId::new();
        store.upsert_aggregate(AggregateRelation {
            guid: "agg".into(),
            relating: parent.clone(),
            related: vec![child.clone()],
        });
        store.upsert_containment(ContainmentRelation {
            guid: "con".into(),
            spatial_node: parent.clone(),
            elements: vec![element.clone(), child.clone()],
        });
        let hits = store.purge(&child);
        assert!(hits >= 2);
        assert!(store.parent_of(&child).is_none());
        // Containment still exists for `element`.
        assert_eq!(store.container_of(&element), Some(&parent));
    }

    #[test]
    fn upsert_replaces_existing_guid() {
        let mut store = RelationStore::new();
        let parent = EntityId::new();
        let child_a = EntityId::new();
        let child_b = EntityId::new();
        store.upsert_aggregate(AggregateRelation {
            guid: "rel-1".into(),
            relating: parent.clone(),
            related: vec![child_a.clone()],
        });
        store.upsert_aggregate(AggregateRelation {
            guid: "rel-1".into(),
            relating: parent.clone(),
            related: vec![child_b.clone()],
        });
        assert_eq!(store.aggregates.len(), 1);
        assert_eq!(store.parent_of(&child_b), Some(&parent));
        assert!(store.parent_of(&child_a).is_none());
    }
}
