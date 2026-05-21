//! BIM tree GPU instancing — group [`aec_geometry::mesh::Mesh`] outputs
//! from the IFC tessellator (walls, slabs, doors, windows, beams,
//! columns, ...) into draw batches keyed by `(mesh_hash, material_id,
//! kind)`.
//!
//! Two identical doors with the same material live in the same batch
//! and render as a single instanced draw. A door with a different
//! material lives in its own batch even if the geometry is identical;
//! we don't pack material indices into per-instance data here because
//! the navigable viewport pipeline draws per-(material, batch) pairs.
//!
//! Why per-kind instead of one global batch? Because BIM authors edit
//! kinds in clusters — moving a wall doesn't invalidate any door — so
//! we want to be able to rebuild a single kind without re-hashing the
//! whole project. The cache exposes `invalidate_kind` for this.
//!
//! Pure CPU; no wgpu calls. The output [`BimBatch`] feeds the navigable
//! viewport pipeline which uploads the per-instance transforms into a
//! single GPU buffer.

use std::collections::{BTreeMap, HashMap};

use aec_core::types::EntityId;
use aec_geometry::mesh::Mesh;
use serde::{Deserialize, Serialize};

/// Coarse IFC kind label used as a primary batch axis. Keeping this on
/// the viewport side (rather than reaching into `aec_bim::Kind` enums)
/// keeps the dependency graph clean — the viewport doesn't need the
/// full IFC type universe, only a handful of buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BimKind {
    Wall,
    Slab,
    Roof,
    Door,
    Window,
    Beam,
    Column,
    Stair,
    Railing,
    Furniture,
    Other,
}

impl BimKind {
    /// Stable string form for logs / debug. Matches the serde repr.
    pub fn as_str(self) -> &'static str {
        match self {
            BimKind::Wall => "wall",
            BimKind::Slab => "slab",
            BimKind::Roof => "roof",
            BimKind::Door => "door",
            BimKind::Window => "window",
            BimKind::Beam => "beam",
            BimKind::Column => "column",
            BimKind::Stair => "stair",
            BimKind::Railing => "railing",
            BimKind::Furniture => "furniture",
            BimKind::Other => "other",
        }
    }
}

/// 32-byte BLAKE3 digest of a canonical (welded, sorted) mesh. Used as
/// the primary deduplication key — two meshes producing the same hash
/// share a single vertex/index buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MeshHash(pub [u8; 32]);

impl MeshHash {
    /// Hex prefix for logs. Full digest is overkill in messages.
    pub fn short(&self) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(16);
        for byte in &self.0[..8] {
            // Cannot fail: writing to a `String` is infallible. We
            // discard the `Result` rather than `unwrap`ing it to avoid
            // pulling in a panic site for an unreachable branch.
            let _ = write!(s, "{byte:02x}");
        }
        s
    }

    /// Hash a mesh's positions and indices. Normals/UVs are derived
    /// from positions in the tessellator so we deliberately exclude
    /// them — a re-tessellation with the same positions+indices hits
    /// the same cache key.
    pub fn of_mesh(mesh: &Mesh) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(mesh.positions.len() as u32).to_le_bytes());
        for p in &mesh.positions {
            hasher.update(&p[0].to_le_bytes());
            hasher.update(&p[1].to_le_bytes());
            hasher.update(&p[2].to_le_bytes());
        }
        hasher.update(&(mesh.indices.len() as u32).to_le_bytes());
        for i in &mesh.indices {
            hasher.update(&i.to_le_bytes());
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(hasher.finalize().as_bytes());
        Self(out)
    }
}

/// Composite key uniquely identifying a draw batch. Two entities share
/// a batch iff all three fields match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BatchKey {
    pub kind: BimKind,
    pub mesh: MeshHash,
    /// Material reference. `None` means "use kind default material".
    pub material_id: Option<String>,
}

/// A single entity contributing one instance to a batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BimEntity {
    pub entity: EntityId,
    pub kind: BimKind,
    pub mesh: Mesh,
    pub mesh_hash: MeshHash,
    /// Row-major 4x4 world transform in millimetres (project units).
    pub transform: [[f32; 4]; 4],
    pub material_id: Option<String>,
}

impl BimEntity {
    pub fn new(
        entity: EntityId,
        kind: BimKind,
        mesh: Mesh,
        transform: [[f32; 4]; 4],
        material_id: Option<String>,
    ) -> Self {
        let mesh_hash = MeshHash::of_mesh(&mesh);
        Self {
            entity,
            kind,
            mesh,
            mesh_hash,
            transform,
            material_id,
        }
    }

    pub fn batch_key(&self) -> BatchKey {
        BatchKey {
            kind: self.kind,
            mesh: self.mesh_hash,
            material_id: self.material_id.clone(),
        }
    }
}

/// One batch's worth of GPU data, ready to be uploaded into a single
/// vertex/index buffer plus an instance buffer of length
/// `entities.len()`. The mesh is *cloned* in (rather than borrowed) so
/// the navigable pipeline can keep a long-lived reference to it; the
/// cache holds an `Arc` to avoid second-copy overhead.
#[derive(Debug, Clone, PartialEq)]
pub struct BimBatch {
    pub key: BatchKey,
    pub mesh: Mesh,
    pub entities: Vec<EntityId>,
    pub transforms: Vec<[[f32; 4]; 4]>,
}

impl BimBatch {
    pub fn instance_count(&self) -> usize {
        self.entities.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count() * self.entities.len()
    }
}

/// In-memory cache of [`BimBatch`]es. Update with [`insert`](Self::insert)
/// when entities are added/modified; [`remove`](Self::remove) when they're
/// deleted; [`invalidate_kind`](Self::invalidate_kind) when a bulk
/// per-kind rebuild is cheaper than tracking individual entities (e.g.
/// after a wall-thickness global edit).
#[derive(Debug, Default, Clone)]
pub struct BimBatchCache {
    /// `EntityId → BatchKey` so we can find an entity's batch in O(1)
    /// for removal.
    entity_lookup: HashMap<EntityId, BatchKey>,
    /// Sorted by key so iteration order is stable across runs.
    batches: BTreeMap<BatchKey, BimBatch>,
}

impl BimBatchCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    pub fn entity_count(&self) -> usize {
        self.entity_lookup.len()
    }

    /// Total triangle count across all batches × instances. Useful for
    /// HUD overlays and culling-effectiveness metrics.
    pub fn total_triangles(&self) -> usize {
        self.batches.values().map(BimBatch::triangle_count).sum()
    }

    /// Insert or update an entity. If the entity already exists with a
    /// different [`BatchKey`] (e.g. a material change), the old batch
    /// loses one instance and the new batch gains one.
    pub fn insert(&mut self, e: BimEntity) {
        let new_key = e.batch_key();
        // Remove from old batch if present and the key changed.
        if let Some(prev_key) = self.entity_lookup.get(&e.entity).cloned() {
            if prev_key == new_key {
                // Same batch — just refresh the transform.
                if let Some(batch) = self.batches.get_mut(&new_key) {
                    if let Some(idx) = batch.entities.iter().position(|x| x == &e.entity) {
                        batch.transforms[idx] = e.transform;
                        return;
                    }
                }
            } else {
                self.detach_entity(&e.entity, &prev_key);
            }
        }
        let batch = self
            .batches
            .entry(new_key.clone())
            .or_insert_with(|| BimBatch {
                key: new_key.clone(),
                mesh: e.mesh.clone(),
                entities: Vec::new(),
                transforms: Vec::new(),
            });
        batch.entities.push(e.entity.clone());
        batch.transforms.push(e.transform);
        self.entity_lookup.insert(e.entity, new_key);
    }

    /// Remove an entity from whatever batch holds it. Returns `true`
    /// if it was present.
    pub fn remove(&mut self, entity: &EntityId) -> bool {
        if let Some(key) = self.entity_lookup.remove(entity) {
            self.detach_entity(entity, &key);
            true
        } else {
            false
        }
    }

    /// Drop every batch and entity matching `kind`. Used on bulk
    /// per-kind rebuilds (rare but cheap when needed).
    pub fn invalidate_kind(&mut self, kind: BimKind) {
        let stale_keys: Vec<_> = self
            .batches
            .keys()
            .filter(|k| k.kind == kind)
            .cloned()
            .collect();
        for key in stale_keys {
            if let Some(batch) = self.batches.remove(&key) {
                for e in batch.entities {
                    self.entity_lookup.remove(&e);
                }
            }
        }
    }

    /// Iterator over batches, in stable key order. Render pipelines
    /// consume this directly.
    pub fn batches(&self) -> impl Iterator<Item = &BimBatch> {
        self.batches.values()
    }

    /// Look up a batch by key. Used by tests and by per-batch metric
    /// inspectors.
    pub fn get(&self, key: &BatchKey) -> Option<&BimBatch> {
        self.batches.get(key)
    }

    fn detach_entity(&mut self, entity: &EntityId, key: &BatchKey) {
        // Remove the entity + transform from the batch. If the batch
        // becomes empty, drop it entirely so subsequent iterations don't
        // hand the renderer a zero-instance batch (which would still
        // submit a draw call with `instance_count == 0`).
        let drop_batch = if let Some(batch) = self.batches.get_mut(key) {
            if let Some(idx) = batch.entities.iter().position(|x| x == entity) {
                batch.entities.swap_remove(idx);
                batch.transforms.swap_remove(idx);
            }
            batch.entities.is_empty()
        } else {
            false
        };
        if drop_batch {
            self.batches.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube_mesh() -> Mesh {
        let mut m = Mesh::new();
        m.push_quad(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        m
    }

    fn identity_transform() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    #[test]
    fn identical_meshes_hash_identical() {
        let a = unit_cube_mesh();
        let b = unit_cube_mesh();
        assert_eq!(MeshHash::of_mesh(&a), MeshHash::of_mesh(&b));
    }

    #[test]
    fn differing_meshes_hash_distinct() {
        let mut a = unit_cube_mesh();
        let b = unit_cube_mesh();
        a.positions[0][0] += 0.001;
        assert_ne!(MeshHash::of_mesh(&a), MeshHash::of_mesh(&b));
    }

    #[test]
    fn batch_groups_two_identical_walls() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        let e1 = EntityId::new();
        let e2 = EntityId::new();
        cache.insert(BimEntity::new(
            e1.clone(),
            BimKind::Wall,
            mesh.clone(),
            identity_transform(),
            Some("mat:wall_default".into()),
        ));
        cache.insert(BimEntity::new(
            e2.clone(),
            BimKind::Wall,
            mesh,
            identity_transform(),
            Some("mat:wall_default".into()),
        ));
        assert_eq!(cache.batch_count(), 1);
        assert_eq!(cache.entity_count(), 2);
        let batch = cache.batches().next().unwrap();
        assert_eq!(batch.instance_count(), 2);
        assert_eq!(batch.entities.len(), 2);
    }

    #[test]
    fn different_materials_split_into_distinct_batches() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        cache.insert(BimEntity::new(
            EntityId::new(),
            BimKind::Wall,
            mesh.clone(),
            identity_transform(),
            Some("mat:wall_default".into()),
        ));
        cache.insert(BimEntity::new(
            EntityId::new(),
            BimKind::Wall,
            mesh,
            identity_transform(),
            Some("mat:wall_painted_red".into()),
        ));
        assert_eq!(cache.batch_count(), 2);
    }

    #[test]
    fn different_kinds_split_into_distinct_batches() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        cache.insert(BimEntity::new(
            EntityId::new(),
            BimKind::Wall,
            mesh.clone(),
            identity_transform(),
            None,
        ));
        cache.insert(BimEntity::new(
            EntityId::new(),
            BimKind::Door,
            mesh,
            identity_transform(),
            None,
        ));
        assert_eq!(cache.batch_count(), 2);
    }

    #[test]
    fn re_inserting_same_entity_with_same_key_updates_transform() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        let e = EntityId::new();
        cache.insert(BimEntity::new(
            e.clone(),
            BimKind::Wall,
            mesh.clone(),
            identity_transform(),
            None,
        ));
        let mut translated = identity_transform();
        translated[3][0] = 1000.0; // 1 m in mm
        cache.insert(BimEntity::new(
            e.clone(),
            BimKind::Wall,
            mesh,
            translated,
            None,
        ));
        assert_eq!(cache.entity_count(), 1);
        assert_eq!(cache.batch_count(), 1);
        let batch = cache.batches().next().unwrap();
        assert_eq!(batch.transforms[0][3][0], 1000.0);
    }

    #[test]
    fn re_inserting_entity_with_new_material_moves_to_new_batch() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        let e = EntityId::new();
        cache.insert(BimEntity::new(
            e.clone(),
            BimKind::Wall,
            mesh.clone(),
            identity_transform(),
            Some("mat:a".into()),
        ));
        cache.insert(BimEntity::new(
            e.clone(),
            BimKind::Wall,
            mesh,
            identity_transform(),
            Some("mat:b".into()),
        ));
        assert_eq!(cache.entity_count(), 1);
        // Old batch should be gone (now empty), new batch has the entity.
        assert_eq!(cache.batch_count(), 1);
        let batch = cache.batches().next().unwrap();
        assert_eq!(batch.key.material_id.as_deref(), Some("mat:b"));
    }

    #[test]
    fn removing_last_entity_drops_the_batch() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        let e = EntityId::new();
        cache.insert(BimEntity::new(
            e.clone(),
            BimKind::Wall,
            mesh,
            identity_transform(),
            None,
        ));
        assert_eq!(cache.batch_count(), 1);
        assert!(cache.remove(&e));
        assert_eq!(cache.batch_count(), 0);
        assert_eq!(cache.entity_count(), 0);
    }

    #[test]
    fn invalidate_kind_drops_only_matching_kind() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        cache.insert(BimEntity::new(
            EntityId::new(),
            BimKind::Wall,
            mesh.clone(),
            identity_transform(),
            None,
        ));
        cache.insert(BimEntity::new(
            EntityId::new(),
            BimKind::Door,
            mesh,
            identity_transform(),
            None,
        ));
        assert_eq!(cache.batch_count(), 2);
        cache.invalidate_kind(BimKind::Wall);
        assert_eq!(cache.batch_count(), 1);
        let remaining = cache.batches().next().unwrap();
        assert_eq!(remaining.key.kind, BimKind::Door);
    }

    #[test]
    fn total_triangles_scales_with_instance_count() {
        let mut cache = BimBatchCache::new();
        let mesh = unit_cube_mesh();
        let per_instance_tris = mesh.triangle_count();
        for _ in 0..4 {
            cache.insert(BimEntity::new(
                EntityId::new(),
                BimKind::Wall,
                mesh.clone(),
                identity_transform(),
                None,
            ));
        }
        assert_eq!(cache.total_triangles(), per_instance_tris * 4);
    }

    #[test]
    fn mesh_hash_short_returns_16_hex_chars() {
        let h = MeshHash::of_mesh(&unit_cube_mesh());
        assert_eq!(h.short().len(), 16);
        assert!(h.short().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
