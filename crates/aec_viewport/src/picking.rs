//! GPU id-buffer picking + selection registration.
//!
//! Selection in a CAD/BIM editor needs to be sub-pixel-accurate — a
//! click on a thin window mullion should pick the mullion and not the
//! wall behind it. A CPU ray-AABB broad phase (which is what
//! [`crate::selection::pick`] does today) only matches the visible
//! envelope and misses these cases.
//!
//! GPU picking: render the scene into an `R32Uint` colour target,
//! writing the per-instance `PickingId` (a stable u32 the registry
//! assigns) instead of shaded RGB. To resolve a pointer, copy a 1×1
//! pixel rect at the cursor into a staging buffer, map it, read the
//! `PickingId`, look it up in the registry, get back the [`EntityId`].
//!
//! The actual wgpu pass + readback lives in
//! [`crate::viewport_pipeline`]; this module owns the **registry** and
//! the small wire types.
//!
//! Why a u32 id instead of writing the [`EntityId`] (a String UUID)
//! directly? Because R32Uint is the widest single-channel integer
//! format wgpu guarantees as a renderable colour attachment, and
//! cramming a 128-bit UUID into a u32 would clobber it. The registry
//! holds the lookup; reassigning a stable monotonic counter keeps the
//! mapping fast and deterministic across frames.

use std::collections::HashMap;

use aec_core::types::EntityId;
use serde::{Deserialize, Serialize};

/// 32-bit identifier written into the picking colour target. 0 is
/// reserved for "no hit" (the cleared value) so the smallest assigned
/// id is `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PickingId(pub u32);

impl PickingId {
    pub const NONE: PickingId = PickingId(0);

    pub fn is_some(self) -> bool {
        self.0 != 0
    }
}

/// What was picked — usually an entity, but could be a gizmo handle, a
/// reference-image quad, or a special "background" hit. Keeping it as a
/// typed enum lets the host distinguish these without re-deriving from
/// state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PickingTarget {
    Entity(EntityId),
    GizmoHandle(String),
    ReferenceImage,
}

/// Resolved hit. `pixel` is the screen-space pixel where the hit was
/// recorded (for debug overlays); `target` is what was picked.
#[derive(Debug, Clone, PartialEq)]
pub struct PickedHit {
    pub pixel: [u32; 2],
    pub target: PickingTarget,
}

/// Registry that assigns stable [`PickingId`]s to [`PickingTarget`]s.
/// Caller order: each frame, the navigable pipeline rebuilds the
/// registry from the current visible set, assigns IDs in deterministic
/// order, writes them into the per-instance buffer, and `resolve()`s
/// them after a click.
#[derive(Debug, Default, Clone)]
pub struct PickRegistry {
    next: u32,
    by_id: HashMap<PickingId, PickingTarget>,
    by_target: HashMap<PickingTarget, PickingId>,
}

impl PickRegistry {
    pub fn new() -> Self {
        Self {
            next: 1,
            by_id: HashMap::new(),
            by_target: HashMap::new(),
        }
    }

    /// Forget every prior assignment. Call once per frame before
    /// re-registering visible instances. The next id is reset to `1`
    /// so the ID stream is deterministic.
    pub fn clear(&mut self) {
        self.next = 1;
        self.by_id.clear();
        self.by_target.clear();
    }

    /// Assign (or recall) a [`PickingId`] for a target. Idempotent —
    /// re-registering the same target returns the same id until
    /// `clear()`.
    pub fn register(&mut self, target: PickingTarget) -> PickingId {
        if let Some(id) = self.by_target.get(&target).copied() {
            return id;
        }
        let id = PickingId(self.next);
        self.next = self.next.checked_add(1).expect(
            "picking id overflow: scenes with more than 4 billion visible instances are unsupported",
        );
        self.by_id.insert(id, target.clone());
        self.by_target.insert(target, id);
        id
    }

    /// Look up the target a [`PickingId`] points at. Returns `None`
    /// for unassigned ids and for the reserved `PickingId::NONE`.
    pub fn resolve(&self, id: PickingId) -> Option<&PickingTarget> {
        if id == PickingId::NONE {
            None
        } else {
            self.by_id.get(&id)
        }
    }

    /// Number of assigned ids. Useful for the HUD overlay and for
    /// asserting registry invariants in tests.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

// Hashable PickingTarget: derive impls rely on EntityId / String being
// hashable, which they are.
impl std::hash::Hash for PickingTarget {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Variant discriminator first so two variants with the same
        // string payload can't collide.
        match self {
            PickingTarget::Entity(e) => {
                0_u8.hash(state);
                e.as_str().hash(state);
            }
            PickingTarget::GizmoHandle(h) => {
                1_u8.hash(state);
                h.hash(state);
            }
            PickingTarget::ReferenceImage => {
                2_u8.hash(state);
            }
        }
    }
}

impl Eq for PickingTarget {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_id_zero_is_none() {
        assert_eq!(PickingId::NONE.0, 0);
        assert!(!PickingId::NONE.is_some());
        assert!(PickingId(1).is_some());
    }

    #[test]
    fn registry_assigns_distinct_ids_to_distinct_targets() {
        let mut r = PickRegistry::new();
        let a = r.register(PickingTarget::Entity(EntityId::new()));
        let b = r.register(PickingTarget::Entity(EntityId::new()));
        assert_ne!(a, b);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn registry_returns_same_id_for_same_target() {
        let mut r = PickRegistry::new();
        let target = PickingTarget::Entity(EntityId::new());
        let a = r.register(target.clone());
        let b = r.register(target);
        assert_eq!(a, b);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn registry_resolve_returns_the_registered_target() {
        let mut r = PickRegistry::new();
        let entity = EntityId::new();
        let id = r.register(PickingTarget::Entity(entity.clone()));
        let resolved = r.resolve(id).unwrap();
        match resolved {
            PickingTarget::Entity(e) => assert_eq!(e, &entity),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn registry_resolve_none_for_unassigned_and_zero() {
        let r = PickRegistry::new();
        assert!(r.resolve(PickingId::NONE).is_none());
        assert!(r.resolve(PickingId(99)).is_none());
    }

    #[test]
    fn clear_resets_id_stream() {
        let mut r = PickRegistry::new();
        r.register(PickingTarget::ReferenceImage);
        r.register(PickingTarget::GizmoHandle("rotate_x".into()));
        assert_eq!(r.len(), 2);
        r.clear();
        assert_eq!(r.len(), 0);
        let a = r.register(PickingTarget::ReferenceImage);
        assert_eq!(a, PickingId(1), "id stream restarts at 1 after clear");
    }

    #[test]
    fn distinct_variants_with_same_string_dont_collide() {
        // GizmoHandle("ent_xyz") and Entity("ent_xyz") must produce
        // different IDs — the variant discriminator in the hash impl
        // is exactly to guarantee this.
        let mut r = PickRegistry::new();
        let entity = EntityId::from_string("ent_abc123").unwrap();
        let a = r.register(PickingTarget::Entity(entity));
        let b = r.register(PickingTarget::GizmoHandle("ent_abc123".into()));
        assert_ne!(a, b);
    }
}
