//! Spatial hierarchy: Project → Site → Building → Level → Space → elements.
//!
//! Holds the project-side BIM graph. Geometry attaches to leaf elements
//! (walls, slabs, doors, etc.), but those types live in `aec_geometry` —
//! this module just stores `EntityId` references.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::classification::IfcClass;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialNode {
    pub id: EntityId,
    /// IFC GUID (22-char compressed). Stored for round-trip stability.
    pub ifc_guid: Option<String>,
    pub class: IfcClass,
    pub name: String,
    pub children: Vec<EntityId>,
    pub elements: Vec<EntityId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub root: EntityId,
    pub nodes: HashMap<EntityId, SpatialNode>,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let root = EntityId::new();
        let mut nodes = HashMap::new();
        nodes.insert(
            root.clone(),
            SpatialNode {
                id: root.clone(),
                ifc_guid: None,
                class: IfcClass::IfcProject,
                name: name.into(),
                children: Vec::new(),
                elements: Vec::new(),
            },
        );
        Self { root, nodes }
    }

    pub fn add_child(
        &mut self,
        parent: &EntityId,
        class: IfcClass,
        name: impl Into<String>,
    ) -> Option<EntityId> {
        if !self.nodes.contains_key(parent) {
            return None;
        }
        let id = EntityId::new();
        self.nodes.insert(
            id.clone(),
            SpatialNode {
                id: id.clone(),
                ifc_guid: None,
                class,
                name: name.into(),
                children: Vec::new(),
                elements: Vec::new(),
            },
        );
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(id.clone());
        }
        Some(id)
    }

    pub fn attach_element(&mut self, parent: &EntityId, element: EntityId) -> bool {
        if let Some(p) = self.nodes.get_mut(parent) {
            p.elements.push(element);
            true
        } else {
            false
        }
    }

    pub fn get(&self, id: &EntityId) -> Option<&SpatialNode> {
        self.nodes.get(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn descendants(&self, root: &EntityId) -> Vec<EntityId> {
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.nodes.get(&id) {
                out.push(id.clone());
                for child in &n.children {
                    stack.push(child.clone());
                }
            }
        }
        out
    }
}

// Convenience type aliases.
pub type Site = SpatialNode;
pub type Building = SpatialNode;
pub type Level = SpatialNode;
pub type Space = SpatialNode;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_project_hierarchy() {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let bldg = p
            .add_child(&site, IfcClass::IfcBuilding, "Building A")
            .unwrap();
        let level = p
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let space = p.add_child(&level, IfcClass::IfcSpace, "Living").unwrap();
        let elem = EntityId::new();
        assert!(p.attach_element(&space, elem.clone()));
        assert_eq!(p.descendants(&root).len(), 5);
        assert_eq!(p.get(&space).unwrap().elements, vec![elem]);
    }

    #[test]
    fn unknown_parent_rejected() {
        let mut p = Project::new("Demo");
        let stranger = EntityId::new();
        assert!(p.add_child(&stranger, IfcClass::IfcSite, "x").is_none());
        assert!(!p.attach_element(&stranger, EntityId::new()));
    }
}
