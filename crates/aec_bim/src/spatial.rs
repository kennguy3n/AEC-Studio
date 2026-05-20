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

    /// Remove a node and its subtree. Returns the list of node IDs that
    /// were removed. Elements attached to removed nodes are also
    /// reported (but their EntityId can still be referenced from the
    /// scene graph — callers are expected to do that cleanup).
    pub fn remove_subtree(&mut self, id: &EntityId) -> RemovedSubtree {
        let descs = self.descendants(id);
        let mut removed_elements = Vec::new();
        for node in &descs {
            if let Some(n) = self.nodes.remove(node) {
                removed_elements.extend(n.elements);
            }
        }
        // Detach from parent.
        for (_pid, node) in self.nodes.iter_mut() {
            node.children.retain(|c| c != id);
        }
        RemovedSubtree {
            nodes: descs,
            elements: removed_elements,
        }
    }

    /// Iterate the nodes of a given class in deterministic depth-first
    /// order.
    pub fn nodes_of_class(&self, class: &IfcClass) -> Vec<&SpatialNode> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.nodes.get(&id) {
                if &n.class == class {
                    out.push(n);
                }
                for c in n.children.iter().rev() {
                    stack.push(c.clone());
                }
            }
        }
        out
    }

    pub fn set_ifc_guid(&mut self, id: &EntityId, guid: impl Into<String>) -> bool {
        if let Some(n) = self.nodes.get_mut(id) {
            n.ifc_guid = Some(guid.into());
            true
        } else {
            false
        }
    }

    pub fn rename(&mut self, id: &EntityId, name: impl Into<String>) -> bool {
        if let Some(n) = self.nodes.get_mut(id) {
            n.name = name.into();
            true
        } else {
            false
        }
    }

    /// Direct children of `id`, in insertion order.
    pub fn children_of(&self, id: &EntityId) -> Vec<&SpatialNode> {
        if let Some(n) = self.nodes.get(id) {
            n.children
                .iter()
                .filter_map(|c| self.nodes.get(c))
                .collect()
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemovedSubtree {
    pub nodes: Vec<EntityId>,
    pub elements: Vec<EntityId>,
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

    #[test]
    fn remove_subtree_drops_nodes_and_returns_elements() {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let bldg = p
            .add_child(&site, IfcClass::IfcBuilding, "Building A")
            .unwrap();
        let elem = EntityId::new();
        p.attach_element(&bldg, elem.clone());
        let removed = p.remove_subtree(&site);
        assert!(removed.nodes.contains(&site));
        assert!(removed.nodes.contains(&bldg));
        assert!(removed.elements.contains(&elem));
        assert!(p.get(&site).is_none());
        assert_eq!(p.children_of(&root).len(), 0);
    }

    #[test]
    fn nodes_of_class_returns_matching_classes() {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let _b1 = p.add_child(&site, IfcClass::IfcBuilding, "A");
        let _b2 = p.add_child(&site, IfcClass::IfcBuilding, "B");
        let buildings = p.nodes_of_class(&IfcClass::IfcBuilding);
        assert_eq!(buildings.len(), 2);
    }

    #[test]
    fn rename_and_set_guid() {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        assert!(p.rename(&root, "Renamed"));
        assert!(p.set_ifc_guid(&root, "GUID-123"));
        assert_eq!(p.get(&root).unwrap().name, "Renamed");
        assert_eq!(p.get(&root).unwrap().ifc_guid.as_deref(), Some("GUID-123"));
    }
}
