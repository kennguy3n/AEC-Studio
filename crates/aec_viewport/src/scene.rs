//! In-memory scene graph the viewport renders.
//!
//! Domain code populates `SceneGraph` (typically via the command engine)
//! and the renderer reads from it each frame.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneMesh {
    pub indices: Vec<u32>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
}

impl SceneMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn bounding_box(&self) -> Option<([f32; 3], [f32; 3])> {
        if self.positions.is_empty() {
            return None;
        }
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for p in &self.positions {
            for i in 0..3 {
                if p[i] < min[i] {
                    min[i] = p[i];
                }
                if p[i] > max[i] {
                    max[i] = p[i];
                }
            }
        }
        Some((min, max))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SceneNodeKind {
    /// Renderable geometry (walls, floors, furniture).
    Mesh(SceneMesh),
    /// Polyline overlay (snap guides, dimensions).
    Polyline {
        points: Vec<[f32; 3]>,
        color_rgba: [f32; 4],
    },
    /// Group node — children only, no geometry of its own.
    Group,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneNode {
    pub id: EntityId,
    pub kind: SceneNodeKind,
    pub transform: [[f32; 4]; 4],
    pub material_id: Option<String>,
    pub visible: bool,
    pub layer: String,
    pub children: Vec<EntityId>,
}

impl SceneNode {
    pub fn new_group() -> Self {
        Self {
            id: EntityId::new(),
            kind: SceneNodeKind::Group,
            transform: identity(),
            material_id: None,
            visible: true,
            layer: "default".into(),
            children: Vec::new(),
        }
    }

    pub fn new_mesh(mesh: SceneMesh) -> Self {
        Self {
            id: EntityId::new(),
            kind: SceneNodeKind::Mesh(mesh),
            transform: identity(),
            material_id: None,
            visible: true,
            layer: "default".into(),
            children: Vec::new(),
        }
    }
}

pub fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneGraph {
    pub root: Option<EntityId>,
    pub nodes: HashMap<EntityId, SceneNode>,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ensure_root(&mut self) -> EntityId {
        if let Some(root) = self.root.clone() {
            return root;
        }
        let root = SceneNode::new_group();
        let id = root.id.clone();
        self.nodes.insert(id.clone(), root);
        self.root = Some(id.clone());
        id
    }

    pub fn add_node(&mut self, parent: &EntityId, node: SceneNode) -> Option<EntityId> {
        if !self.nodes.contains_key(parent) {
            return None;
        }
        let id = node.id.clone();
        self.nodes.insert(id.clone(), node);
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(id.clone());
        }
        Some(id)
    }

    pub fn remove_node(&mut self, id: &EntityId) -> bool {
        let removed = self.nodes.remove(id).is_some();
        if removed {
            for n in self.nodes.values_mut() {
                n.children.retain(|c| c != id);
            }
        }
        removed
    }

    pub fn get(&self, id: &EntityId) -> Option<&SceneNode> {
        self.nodes.get(id)
    }

    pub fn get_mut(&mut self, id: &EntityId) -> Option<&mut SceneNode> {
        self.nodes.get_mut(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Total triangles across all visible mesh nodes.
    pub fn total_triangles(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.visible)
            .filter_map(|n| match &n.kind {
                SceneNodeKind::Mesh(m) => Some(m.triangle_count()),
                _ => None,
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_roundtrip() {
        let mut g = SceneGraph::new();
        let root = g.ensure_root();
        let mesh = SceneMesh {
            indices: vec![0, 1, 2],
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.0, 0.0]; 3],
        };
        let n = g
            .add_node(&root, SceneNode::new_mesh(mesh.clone()))
            .unwrap();
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.total_triangles(), 1);
        assert!(g.remove_node(&n));
        assert_eq!(g.total_triangles(), 0);
    }

    #[test]
    fn bounding_box_handles_empty() {
        let m = SceneMesh {
            indices: vec![],
            positions: vec![],
            normals: vec![],
            uvs: vec![],
        };
        assert!(m.bounding_box().is_none());
    }
}
