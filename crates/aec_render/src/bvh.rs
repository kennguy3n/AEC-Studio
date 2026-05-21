//! SAH (Surface Area Heuristic) BVH2 builder for triangle meshes.
//!
//! Native Rust replacement for the BVH that lived inside Blender / Cycles.
//! Two-level layout:
//!
//! * **Bottom-level acceleration structure (BLAS)** — one per static mesh,
//!   indexes its triangles directly.
//! * **Top-level acceleration structure (TLAS)** — one per scene, indexes
//!   instances of BLASes with per-instance affine transforms.
//!
//! Both levels share the same packed [`BvhNode`] layout so the traversal
//! kernel in `intersect.rs` is identical.
//!
//! Inspired by the architecture of Cycles' `src/bvh/` builder and
//! `src/kernel/bvh/traversal.h`, but built natively in Rust against
//! `glam` types and Rayon for parallel construction of large meshes.

use glam::{Mat4, Vec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    /// Empty AABB — `min` set to +infinity, `max` to -infinity so the first
    /// `expand` is unconditional.
    pub fn empty() -> Self {
        Self {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
        }
    }

    pub fn point(p: Vec3) -> Self {
        Self { min: p, max: p }
    }

    pub fn from_points<I: IntoIterator<Item = Vec3>>(points: I) -> Self {
        let mut b = Self::empty();
        for p in points {
            b.expand_point(p);
        }
        b
    }

    pub fn expand_point(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn merge(&mut self, other: &Self) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    pub fn union(a: &Self, b: &Self) -> Self {
        Self {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        }
    }

    pub fn extent(&self) -> Vec3 {
        self.max - self.min
    }

    pub fn centroid(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half-surface-area times two (i.e. full surface area) of the box.
    /// Returns 0.0 for empty boxes.
    pub fn surface_area(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        let e = self.extent();
        2.0 * e.x.mul_add(e.y, e.x.mul_add(e.z, e.y * e.z))
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    /// Index (0=x, 1=y, 2=z) of the longest axis.
    pub fn longest_axis(&self) -> usize {
        let e = self.extent();
        if e.x > e.y && e.x > e.z {
            0
        } else if e.y > e.z {
            1
        } else {
            2
        }
    }

    /// Tightest axis-aligned wrap of this AABB after `m` is applied.
    /// Uses the 8-corner method, which is conservative but cheap and
    /// matches Cycles' instance BVH approach.
    pub fn transformed(&self, m: &Mat4) -> Self {
        if self.is_empty() {
            return *self;
        }
        let corners = [
            Vec3::new(self.min.x, self.min.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.max.z),
        ];
        let mut out = Aabb::empty();
        for c in corners {
            out.expand_point(m.transform_point3(c));
        }
        out
    }
}

impl Default for Aabb {
    fn default() -> Self {
        Self::empty()
    }
}

/// Packed BVH node.
///
/// For internal nodes: `left_or_start` is the index of the left child in
/// the `nodes` vector. The right child is implicit at `left_or_start + 1`
/// by construction (children are emitted contiguously).
///
/// For leaf nodes: `left_or_start` is the start index into `prim_indices`
/// and `prim_count` is the number of triangles in the leaf.
///
/// A node is a leaf iff `prim_count > 0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BvhNode {
    pub bounds: Aabb,
    pub left_or_start: u32,
    pub prim_count: u32,
}

impl BvhNode {
    pub fn is_leaf(&self) -> bool {
        self.prim_count > 0
    }
}

/// Triangle in builder-format: three vertex positions and the original
/// triangle index (so the path tracer can look up shading data — normals,
/// UVs, material id — in the source mesh).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuilderTriangle {
    pub v0: Vec3,
    pub v1: Vec3,
    pub v2: Vec3,
    pub prim_id: u32,
}

impl BuilderTriangle {
    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::point(self.v0);
        b.expand_point(self.v1);
        b.expand_point(self.v2);
        b
    }

    pub fn centroid(&self) -> Vec3 {
        (self.v0 + self.v1 + self.v2) * (1.0 / 3.0)
    }
}

/// SAH BVH over a set of triangles or instances.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bvh {
    /// Flat node array, root at index 0.
    pub nodes: Vec<BvhNode>,
    /// Permutation of primitive ids that leaves slice into.
    pub prim_indices: Vec<u32>,
}

const SAH_NUM_BUCKETS: usize = 12;
const MAX_LEAF_PRIMS: usize = 4;
const PARALLEL_BUILD_THRESHOLD: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct PrimInfo {
    index: u32,
    bounds: Aabb,
    centroid: Vec3,
}

#[derive(Debug, Clone, Copy, Default)]
struct SahBucket {
    count: u32,
    bounds: Aabb,
}

impl Bvh {
    /// Build a BVH from triangles. Parallel for large inputs.
    pub fn build(triangles: &[BuilderTriangle]) -> Self {
        if triangles.is_empty() {
            return Self::default();
        }
        let mut prims: Vec<PrimInfo> = if triangles.len() >= PARALLEL_BUILD_THRESHOLD {
            triangles
                .par_iter()
                .enumerate()
                .map(|(i, t)| PrimInfo {
                    index: i as u32,
                    bounds: t.bounds(),
                    centroid: t.centroid(),
                })
                .collect()
        } else {
            triangles
                .iter()
                .enumerate()
                .map(|(i, t)| PrimInfo {
                    index: i as u32,
                    bounds: t.bounds(),
                    centroid: t.centroid(),
                })
                .collect()
        };
        let mut nodes = Vec::with_capacity(triangles.len() * 2);
        nodes.push(BvhNode {
            bounds: Aabb::empty(),
            left_or_start: 0,
            prim_count: 0,
        });
        build_recursive(&mut prims[..], 0, &mut nodes, 0);
        let prim_indices = prims.into_iter().map(|p| p.index).collect();
        Self {
            nodes,
            prim_indices,
        }
    }

    /// Build a TLAS over instance AABBs. Each entry is `(prim_id,
    /// world_space_aabb, world_space_centroid)`.
    pub fn build_from_aabbs(items: &[(u32, Aabb)]) -> Self {
        if items.is_empty() {
            return Self::default();
        }
        let mut prims: Vec<PrimInfo> = items
            .iter()
            .map(|(id, b)| PrimInfo {
                index: *id,
                bounds: *b,
                centroid: b.centroid(),
            })
            .collect();
        let mut nodes = Vec::with_capacity(items.len() * 2);
        nodes.push(BvhNode {
            bounds: Aabb::empty(),
            left_or_start: 0,
            prim_count: 0,
        });
        build_recursive(&mut prims[..], 0, &mut nodes, 0);
        let prim_indices = prims.into_iter().map(|p| p.index).collect();
        Self {
            nodes,
            prim_indices,
        }
    }

    pub fn root_bounds(&self) -> Aabb {
        self.nodes.first().map_or_else(Aabb::empty, |n| n.bounds)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn leaf_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_leaf()).count()
    }

    /// Sum of `surface_area(node) * (children_or_prims)` over the tree —
    /// the SAH cost (lower is better). Used in tests to verify that the
    /// builder is producing reasonable trees.
    pub fn sah_cost(&self) -> f32 {
        if self.nodes.is_empty() {
            return 0.0;
        }
        let root_sa = self.nodes[0].bounds.surface_area().max(1e-20);
        let mut sum = 0.0_f32;
        for n in &self.nodes {
            let sa = n.bounds.surface_area();
            if n.is_leaf() {
                sum += sa * n.prim_count as f32;
            } else {
                sum += sa * 1.5; // typical SAH internal node traversal cost weighting
            }
        }
        sum / root_sa
    }
}

fn compute_bounds(prims: &[PrimInfo]) -> (Aabb, Aabb) {
    let mut total = Aabb::empty();
    let mut centroid = Aabb::empty();
    for p in prims {
        total.merge(&p.bounds);
        centroid.expand_point(p.centroid);
    }
    (total, centroid)
}

fn build_recursive(prims: &mut [PrimInfo], offset: u32, nodes: &mut Vec<BvhNode>, node_idx: u32) {
    let n = prims.len();
    let (total, centroid_bounds) = compute_bounds(prims);
    nodes[node_idx as usize].bounds = total;

    let make_leaf = |nodes: &mut Vec<BvhNode>| {
        nodes[node_idx as usize].left_or_start = offset;
        nodes[node_idx as usize].prim_count = n as u32;
    };

    if n <= MAX_LEAF_PRIMS || centroid_bounds.is_empty() {
        make_leaf(nodes);
        return;
    }

    let axis = centroid_bounds.longest_axis();
    let cb_min = centroid_bounds.min[axis];
    let cb_max = centroid_bounds.max[axis];
    let cb_ext = (cb_max - cb_min).max(1e-20);

    // SAH bucketing.
    let mut buckets = [SahBucket::default(); SAH_NUM_BUCKETS];
    for p in prims.iter() {
        let t = (p.centroid[axis] - cb_min) / cb_ext;
        let b = ((t * SAH_NUM_BUCKETS as f32) as usize).min(SAH_NUM_BUCKETS - 1);
        buckets[b].count += 1;
        buckets[b].bounds.merge(&p.bounds);
    }

    // Walk every split position, find minimum SAH cost.
    let parent_sa = total.surface_area().max(1e-20);
    let mut min_cost = f32::INFINITY;
    let mut best_split = 0usize;
    for i in 0..(SAH_NUM_BUCKETS - 1) {
        let mut left_b = Aabb::empty();
        let mut right_b = Aabb::empty();
        let mut left_c = 0u32;
        let mut right_c = 0u32;
        for (j, bucket) in buckets.iter().enumerate() {
            if j <= i {
                left_b.merge(&bucket.bounds);
                left_c += bucket.count;
            } else {
                right_b.merge(&bucket.bounds);
                right_c += bucket.count;
            }
        }
        if left_c == 0 || right_c == 0 {
            continue;
        }
        let cost = 1.0
            + (left_b.surface_area() * left_c as f32 + right_b.surface_area() * right_c as f32)
                / parent_sa;
        if cost < min_cost {
            min_cost = cost;
            best_split = i;
        }
    }

    let leaf_cost = n as f32;
    if min_cost >= leaf_cost && n <= 16 {
        make_leaf(nodes);
        return;
    }

    // Partition prims by bucket index.
    let split_bucket = best_split;
    let mid = partition_by_bucket(prims, axis, cb_min, cb_ext, split_bucket);
    if mid == 0 || mid == n {
        // Degenerate split, force a leaf.
        make_leaf(nodes);
        return;
    }

    let left_idx = nodes.len() as u32;
    nodes.push(BvhNode {
        bounds: Aabb::empty(),
        left_or_start: 0,
        prim_count: 0,
    });
    let right_idx = nodes.len() as u32;
    nodes.push(BvhNode {
        bounds: Aabb::empty(),
        left_or_start: 0,
        prim_count: 0,
    });
    nodes[node_idx as usize].left_or_start = left_idx;
    nodes[node_idx as usize].prim_count = 0;

    let (left_prims, right_prims) = prims.split_at_mut(mid);
    build_recursive(left_prims, offset, nodes, left_idx);
    build_recursive(right_prims, offset + mid as u32, nodes, right_idx);
}

fn partition_by_bucket(
    prims: &mut [PrimInfo],
    axis: usize,
    cb_min: f32,
    cb_ext: f32,
    split_bucket: usize,
) -> usize {
    let mut left = 0usize;
    let mut right = prims.len();
    while left < right {
        let t = (prims[left].centroid[axis] - cb_min) / cb_ext;
        let b = ((t * SAH_NUM_BUCKETS as f32) as usize).min(SAH_NUM_BUCKETS - 1);
        if b <= split_bucket {
            left += 1;
        } else {
            right -= 1;
            prims.swap(left, right);
        }
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_triangle(prim_id: u32, origin: Vec3) -> BuilderTriangle {
        BuilderTriangle {
            v0: origin,
            v1: origin + Vec3::X,
            v2: origin + Vec3::Y,
            prim_id,
        }
    }

    #[test]
    fn aabb_surface_area_unit_cube_is_six() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        };
        assert!((b.surface_area() - 6.0).abs() < 1e-6);
    }

    #[test]
    fn aabb_empty_surface_area_is_zero() {
        let b = Aabb::empty();
        assert_eq!(b.surface_area(), 0.0);
    }

    #[test]
    fn aabb_longest_axis_picks_largest_extent() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::new(2.0, 5.0, 3.0),
        };
        assert_eq!(b.longest_axis(), 1);
    }

    #[test]
    fn aabb_transformed_translates_correctly() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        };
        let m = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
        let t = b.transformed(&m);
        assert!((t.min.x - 10.0).abs() < 1e-5);
        assert!((t.max.x - 11.0).abs() < 1e-5);
    }

    #[test]
    fn bvh_empty_is_empty() {
        let bvh = Bvh::build(&[]);
        assert!(bvh.nodes.is_empty());
        assert!(bvh.prim_indices.is_empty());
    }

    #[test]
    fn bvh_single_triangle_makes_leaf() {
        let tri = unit_triangle(0, Vec3::ZERO);
        let bvh = Bvh::build(&[tri]);
        assert_eq!(bvh.nodes.len(), 1);
        assert!(bvh.nodes[0].is_leaf());
        assert_eq!(bvh.nodes[0].prim_count, 1);
        assert_eq!(bvh.prim_indices, vec![0]);
    }

    #[test]
    fn bvh_root_bounds_cover_all_triangles() {
        let tris: Vec<_> = (0..16)
            .map(|i| unit_triangle(i, Vec3::new(i as f32 * 10.0, 0.0, 0.0)))
            .collect();
        let bvh = Bvh::build(&tris);
        let b = bvh.root_bounds();
        // Triangle i spans [i*10 .. i*10 + 1] on X axis.
        assert!(b.min.x <= 0.0 + 1e-5);
        assert!(b.max.x >= 15.0 * 10.0 + 1.0 - 1e-5);
    }

    #[test]
    fn bvh_prim_indices_permutation_is_complete() {
        let tris: Vec<_> = (0..32)
            .map(|i| {
                unit_triangle(
                    i,
                    Vec3::new(
                        (i as f32 * 1.7).sin() * 5.0,
                        (i as f32 * 0.9).cos() * 5.0,
                        i as f32,
                    ),
                )
            })
            .collect();
        let bvh = Bvh::build(&tris);
        let mut sorted = bvh.prim_indices.clone();
        sorted.sort_unstable();
        let expected: Vec<u32> = (0..32).collect();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn bvh_internal_node_bounds_cover_children() {
        let tris: Vec<_> = (0..64)
            .map(|i| {
                unit_triangle(
                    i,
                    Vec3::new(
                        (i % 8) as f32 * 5.0,
                        ((i / 8) % 8) as f32 * 5.0,
                        (i / 64) as f32 * 5.0,
                    ),
                )
            })
            .collect();
        let bvh = Bvh::build(&tris);
        for n in &bvh.nodes {
            if !n.is_leaf() {
                let l = &bvh.nodes[n.left_or_start as usize];
                let r = &bvh.nodes[n.left_or_start as usize + 1];
                let union = Aabb::union(&l.bounds, &r.bounds);
                let parent = n.bounds;
                // Parent must contain union (within float epsilon).
                assert!(parent.min.x <= union.min.x + 1e-4);
                assert!(parent.min.y <= union.min.y + 1e-4);
                assert!(parent.min.z <= union.min.z + 1e-4);
                assert!(parent.max.x >= union.max.x - 1e-4);
                assert!(parent.max.y >= union.max.y - 1e-4);
                assert!(parent.max.z >= union.max.z - 1e-4);
            }
        }
    }

    #[test]
    fn bvh_sah_cost_is_finite_and_positive() {
        let tris: Vec<_> = (0..128)
            .map(|i| {
                unit_triangle(
                    i,
                    Vec3::new(
                        (i as f32 * 0.3).sin() * 10.0,
                        (i as f32 * 0.5).cos() * 10.0,
                        i as f32 * 0.1,
                    ),
                )
            })
            .collect();
        let bvh = Bvh::build(&tris);
        let cost = bvh.sah_cost();
        assert!(cost.is_finite());
        assert!(cost > 0.0);
    }

    #[test]
    fn bvh_two_well_separated_clusters_split_first() {
        // Two well-separated groups of 8 triangles each. SAH should split
        // them at the root.
        let mut tris = Vec::new();
        for i in 0..8 {
            tris.push(unit_triangle(i, Vec3::new(i as f32, 0.0, 0.0)));
        }
        for i in 0..8 {
            tris.push(unit_triangle(8 + i, Vec3::new(1000.0 + i as f32, 0.0, 0.0)));
        }
        let bvh = Bvh::build(&tris);
        assert!(!bvh.nodes[0].is_leaf());
        let l = &bvh.nodes[bvh.nodes[0].left_or_start as usize];
        let r = &bvh.nodes[bvh.nodes[0].left_or_start as usize + 1];
        // One side max < other side min (with slack).
        assert!(l.bounds.max.x < r.bounds.min.x || r.bounds.max.x < l.bounds.min.x);
    }

    #[test]
    fn bvh_from_aabbs_builds_tlas() {
        let items: Vec<_> = (0..16)
            .map(|i| {
                (
                    i,
                    Aabb {
                        min: Vec3::new(i as f32 * 2.0, 0.0, 0.0),
                        max: Vec3::new(i as f32 * 2.0 + 1.0, 1.0, 1.0),
                    },
                )
            })
            .collect();
        let bvh = Bvh::build_from_aabbs(&items);
        assert!(!bvh.nodes.is_empty());
        let mut sorted = bvh.prim_indices.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..16u32).collect::<Vec<_>>());
    }

    #[test]
    fn bvh_large_mesh_uses_parallel_path() {
        let tris: Vec<_> = (0..5000u32)
            .map(|i| {
                unit_triangle(
                    i,
                    Vec3::new(
                        (i as f32 * 0.13).sin() * 100.0,
                        (i as f32 * 0.27).cos() * 100.0,
                        i as f32 * 0.05,
                    ),
                )
            })
            .collect();
        let bvh = Bvh::build(&tris);
        assert_eq!(bvh.prim_indices.len(), 5000);
        assert!(bvh.leaf_count() > 0);
    }
}
