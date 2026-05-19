//! Selection state and hit-testing against the scene graph.

use std::collections::HashSet;

#[cfg(test)]
use glam::Vec3;
use glam::Vec3A;
use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::camera::Ray;
use crate::scene::{SceneGraph, SceneNodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    Replace,
    AddOrToggle,
    Subtract,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    items: HashSet<EntityId>,
    hover: Option<EntityId>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &HashSet<EntityId> {
        &self.items
    }

    pub fn hover(&self) -> Option<&EntityId> {
        self.hover.as_ref()
    }

    pub fn set_hover(&mut self, id: Option<EntityId>) {
        self.hover = id;
    }

    pub fn apply(&mut self, mode: SelectionMode, id: EntityId) {
        match mode {
            SelectionMode::Replace => {
                self.items.clear();
                self.items.insert(id);
            }
            SelectionMode::AddOrToggle => {
                if !self.items.insert(id.clone()) {
                    self.items.remove(&id);
                }
            }
            SelectionMode::Subtract => {
                self.items.remove(&id);
            }
        }
    }

    pub fn contains(&self, id: &EntityId) -> bool {
        self.items.contains(id)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// Hit-test the given ray against every visible mesh node in the graph.
/// Uses bounding-box test as the broad phase — accurate enough for click
/// selection and *deterministic*, which matters more than micro-precision
/// for the editor.
pub fn pick(graph: &SceneGraph, ray: &Ray) -> Option<EntityId> {
    let mut best: Option<(EntityId, f32)> = None;
    for (id, node) in &graph.nodes {
        if !node.visible {
            continue;
        }
        let SceneNodeKind::Mesh(mesh) = &node.kind else {
            continue;
        };
        let Some((min, max)) = mesh.bounding_box() else {
            continue;
        };
        if let Some(t) = ray_aabb_intersection(ray, min.into(), max.into()) {
            match &best {
                Some((_, best_t)) if t >= *best_t => {}
                _ => best = Some((id.clone(), t)),
            }
        }
    }
    best.map(|(id, _)| id)
}

/// Slab-method ray–AABB intersection. Returns the nearest positive `t`, or
/// `None` if the ray misses or hits behind the camera.
pub fn ray_aabb_intersection(ray: &Ray, min: Vec3A, max: Vec3A) -> Option<f32> {
    let inv = Vec3A::new(
        1.0 / ray.direction.x.max(1e-9_f32.copysign(ray.direction.x)),
        1.0 / ray.direction.y.max(1e-9_f32.copysign(ray.direction.y)),
        1.0 / ray.direction.z.max(1e-9_f32.copysign(ray.direction.z)),
    );
    let t0 = (min - Vec3A::from(ray.origin)) * inv;
    let t1 = (max - Vec3A::from(ray.origin)) * inv;
    let tmin = Vec3A::min(t0, t1);
    let tmax = Vec3A::max(t0, t1);
    let near = tmin.x.max(tmin.y).max(tmin.z);
    let far = tmax.x.min(tmax.y).min(tmax.z);
    if near > far || far < 0.0 {
        None
    } else {
        Some(near.max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{SceneMesh, SceneNode};

    fn unit_cube() -> SceneMesh {
        SceneMesh {
            indices: vec![0, 1, 2],
            positions: vec![
                [-500.0, 0.0, -500.0],
                [500.0, 0.0, -500.0],
                [500.0, 1000.0, -500.0],
            ],
            normals: vec![[0.0, 1.0, 0.0]; 3],
            uvs: vec![[0.0, 0.0]; 3],
        }
    }

    #[test]
    fn select_replace_then_toggle() {
        let mut sel = Selection::new();
        let a = EntityId::new();
        let b = EntityId::new();
        sel.apply(SelectionMode::Replace, a.clone());
        sel.apply(SelectionMode::AddOrToggle, b.clone());
        assert!(sel.contains(&a) && sel.contains(&b));
        sel.apply(SelectionMode::AddOrToggle, a.clone());
        assert!(!sel.contains(&a));
        assert!(sel.contains(&b));
    }

    #[test]
    fn pick_finds_mesh_in_path_of_ray() {
        let mut g = SceneGraph::new();
        let root = g.ensure_root();
        let id = g.add_node(&root, SceneNode::new_mesh(unit_cube())).unwrap();
        let ray = Ray {
            origin: Vec3::new(0.0, 500.0, -3000.0),
            direction: Vec3::new(0.0, 0.0, 1.0).normalize(),
        };
        let hit = pick(&g, &ray).unwrap();
        assert_eq!(hit, id);
    }

    #[test]
    fn pick_returns_none_when_ray_misses() {
        let mut g = SceneGraph::new();
        let root = g.ensure_root();
        g.add_node(&root, SceneNode::new_mesh(unit_cube())).unwrap();
        let ray = Ray {
            origin: Vec3::new(5000.0, 500.0, -3000.0),
            direction: Vec3::new(1.0, 0.0, 0.0).normalize(),
        };
        assert!(pick(&g, &ray).is_none());
    }
}
