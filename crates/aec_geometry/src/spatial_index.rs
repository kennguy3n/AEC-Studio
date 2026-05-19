//! Tiny BVH spatial index over axis-aligned bounding boxes.
//!
//! The BVH is built once per session and re-used by the viewport (ray
//! picking, frustum culling, proximity queries). The implementation is a
//! median-split BVH; depth is bounded by `ceil(log2(n)) + 1` for `n` leaves.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BvhAabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl BvhAabb {
    pub fn new(min: [f64; 3], max: [f64; 3]) -> Self {
        Self { min, max }
    }

    pub fn from_points(pts: &[[f64; 3]]) -> Option<Self> {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for p in pts {
            for i in 0..3 {
                if p[i] < min[i] {
                    min[i] = p[i];
                }
                if p[i] > max[i] {
                    max[i] = p[i];
                }
            }
        }
        if min[0] == f64::INFINITY {
            None
        } else {
            Some(Self { min, max })
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        let mut min = [0.0; 3];
        let mut max = [0.0; 3];
        for i in 0..3 {
            min[i] = self.min[i].min(other.min[i]);
            max[i] = self.max[i].max(other.max[i]);
        }
        Self { min, max }
    }

    pub fn center(&self) -> [f64; 3] {
        [
            0.5 * (self.min[0] + self.max[0]),
            0.5 * (self.min[1] + self.max[1]),
            0.5 * (self.min[2] + self.max[2]),
        ]
    }

    pub fn longest_axis(&self) -> usize {
        let dx = self.max[0] - self.min[0];
        let dy = self.max[1] - self.min[1];
        let dz = self.max[2] - self.min[2];
        if dx >= dy && dx >= dz {
            0
        } else if dy >= dz {
            1
        } else {
            2
        }
    }

    /// Ray-AABB intersection (slab method). Returns `Some(t_enter)` if hit
    /// within `[t_min, t_max]`.
    ///
    /// Handles negative direction components (via slab swap), parallel rays
    /// (the resulting `±inf` propagates correctly), and degenerate slabs of
    /// zero thickness — the latter via a strict `<` miss test so that an
    /// infinitesimally thin AABB along one axis still registers a hit. This
    /// is the canonical ray–AABB implementation in the codebase; callers
    /// (BVH, viewport picker, etc.) all route through it.
    pub fn ray_intersect(
        &self,
        origin: [f64; 3],
        dir: [f64; 3],
        mut t_min: f64,
        mut t_max: f64,
    ) -> Option<f64> {
        for i in 0..3 {
            let inv_d = 1.0 / dir[i];
            let mut t0 = (self.min[i] - origin[i]) * inv_d;
            let mut t1 = (self.max[i] - origin[i]) * inv_d;
            if inv_d < 0.0 {
                std::mem::swap(&mut t0, &mut t1);
            }
            if t0 > t_min {
                t_min = t0;
            }
            if t1 < t_max {
                t_max = t1;
            }
            if t_max < t_min {
                return None;
            }
        }
        Some(t_min)
    }
}

#[derive(Debug, Clone)]
enum BvhNode {
    Leaf {
        primitive: usize,
        aabb: BvhAabb,
    },
    Internal {
        aabb: BvhAabb,
        left: Box<BvhNode>,
        right: Box<BvhNode>,
    },
}

#[derive(Debug, Clone)]
pub struct Bvh {
    root: Option<BvhNode>,
    primitive_count: usize,
}

impl Bvh {
    pub fn build(primitives: &[BvhAabb]) -> Self {
        let mut indexed: Vec<(usize, BvhAabb)> = primitives.iter().copied().enumerate().collect();
        let root = if indexed.is_empty() {
            None
        } else {
            Some(build_recursive(&mut indexed))
        };
        Self {
            root,
            primitive_count: primitives.len(),
        }
    }

    pub fn primitive_count(&self) -> usize {
        self.primitive_count
    }

    pub fn root_aabb(&self) -> Option<BvhAabb> {
        match self.root.as_ref()? {
            BvhNode::Leaf { aabb, .. } | BvhNode::Internal { aabb, .. } => Some(*aabb),
        }
    }

    /// Return all primitive indices whose AABB the ray passes through, sorted
    /// by hit-distance ascending. Phase 1 picking does the precise-triangle
    /// test client-side after this prune.
    pub fn ray_query(&self, origin: [f64; 3], dir: [f64; 3]) -> Vec<(usize, f64)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect(root, origin, dir, &mut out);
        }
        out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// Return all primitive indices whose AABB intersects `query`.
    pub fn aabb_query(&self, query: &BvhAabb) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect_aabb(root, query, &mut out);
        }
        out
    }
}

fn build_recursive(items: &mut [(usize, BvhAabb)]) -> BvhNode {
    if items.len() == 1 {
        let (idx, aabb) = items[0];
        return BvhNode::Leaf {
            primitive: idx,
            aabb,
        };
    }
    let mut bounds = items[0].1;
    for (_, a) in items.iter().skip(1) {
        bounds = bounds.union(a);
    }
    let axis = bounds.longest_axis();
    items.sort_by(|a, b| {
        a.1.center()[axis]
            .partial_cmp(&b.1.center()[axis])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mid = items.len() / 2;
    let (left_items, right_items) = items.split_at_mut(mid);
    let left = build_recursive(left_items);
    let right = build_recursive(right_items);
    let aabb = match (&left, &right) {
        (
            BvhNode::Leaf { aabb: la, .. } | BvhNode::Internal { aabb: la, .. },
            BvhNode::Leaf { aabb: ra, .. } | BvhNode::Internal { aabb: ra, .. },
        ) => la.union(ra),
    };
    BvhNode::Internal {
        aabb,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn collect(node: &BvhNode, origin: [f64; 3], dir: [f64; 3], out: &mut Vec<(usize, f64)>) {
    match node {
        BvhNode::Leaf { primitive, aabb } => {
            if let Some(t) = aabb.ray_intersect(origin, dir, 0.0, f64::INFINITY) {
                out.push((*primitive, t));
            }
        }
        BvhNode::Internal { aabb, left, right } => {
            if aabb
                .ray_intersect(origin, dir, 0.0, f64::INFINITY)
                .is_some()
            {
                collect(left, origin, dir, out);
                collect(right, origin, dir, out);
            }
        }
    }
}

fn collect_aabb(node: &BvhNode, query: &BvhAabb, out: &mut Vec<usize>) {
    let n_aabb = match node {
        BvhNode::Leaf { aabb, .. } | BvhNode::Internal { aabb, .. } => aabb,
    };
    if !aabbs_overlap(n_aabb, query) {
        return;
    }
    match node {
        BvhNode::Leaf { primitive, .. } => out.push(*primitive),
        BvhNode::Internal { left, right, .. } => {
            collect_aabb(left, query, out);
            collect_aabb(right, query, out);
        }
    }
}

fn aabbs_overlap(a: &BvhAabb, b: &BvhAabb) -> bool {
    for i in 0..3 {
        if a.max[i] < b.min[i] || a.min[i] > b.max[i] {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bvh_returns_none() {
        let b = Bvh::build(&[]);
        assert_eq!(b.primitive_count(), 0);
        assert!(b.root_aabb().is_none());
        assert!(b.ray_query([0.0; 3], [1.0, 0.0, 0.0]).is_empty());
    }

    #[test]
    fn ray_hits_one_box() {
        let a = BvhAabb::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = BvhAabb::new([5.0, 0.0, 0.0], [6.0, 1.0, 1.0]);
        let bvh = Bvh::build(&[a, b]);
        let hits = bvh.ray_query([-1.0, 0.5, 0.5], [1.0, 0.0, 0.0]);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 0); // nearer box hit first
    }

    #[test]
    fn aabb_query_returns_overlapping_primitives() {
        let a = BvhAabb::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b = BvhAabb::new([5.0, 0.0, 0.0], [6.0, 1.0, 1.0]);
        let c = BvhAabb::new([0.5, 0.5, 0.5], [2.0, 2.0, 2.0]);
        let bvh = Bvh::build(&[a, b, c]);
        let mut hits = bvh.aabb_query(&BvhAabb::new([0.0, 0.0, 0.0], [1.5, 1.5, 1.5]));
        hits.sort_unstable();
        assert_eq!(hits, vec![0, 2]);
    }
}
