// Single-letter math identifiers (a, f, h, q, s, t, u, v) match the
// Möller-Trumbore intersection paper directly; long names make the math
// harder to verify against the reference.
#![allow(clippy::many_single_char_names)]

//! Ray-AABB / ray-triangle intersection and BVH traversal kernels.
//!
//! Native Rust replacement for the intersection code in Cycles
//! (`src/kernel/bvh/traversal.h` and `src/kernel/geom/triangle_intersect.h`).
//! The traversal is stack-based and matches the structure of the Cycles
//! kernel: walk internal nodes, fetch leaves, intersect primitives, push
//! children ordered by t-min.

use glam::Vec3;

use crate::bvh::{Aabb, Bvh, BvhNode};

/// A ray in world space. `dir` is expected to be normalised; the traversal
/// pre-computes `1/dir` for the slab test.
#[derive(Debug, Clone, Copy)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
    pub t_min: f32,
    pub t_max: f32,
}

impl Ray {
    pub fn new(origin: Vec3, dir: Vec3) -> Self {
        Self {
            origin,
            dir,
            t_min: 1e-4,
            t_max: f32::INFINITY,
        }
    }

    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }
}

/// Result of a successful triangle intersection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intersection {
    pub t: f32,
    pub u: f32,
    pub v: f32,
    pub prim_id: u32,
    pub object_id: u32,
}

/// One triangle, in the form that the intersection kernel consumes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadingTriangle {
    pub v0: Vec3,
    pub v1: Vec3,
    pub v2: Vec3,
    pub prim_id: u32,
    pub object_id: u32,
}

impl ShadingTriangle {
    /// Möller-Trumbore ray-triangle intersection.
    ///
    /// Returns `Some(intersection)` if the ray hits the triangle within
    /// `(ray.t_min, ray.t_max)`, otherwise `None`.
    #[inline]
    pub fn intersect(&self, ray: &Ray) -> Option<Intersection> {
        let edge1 = self.v1 - self.v0;
        let edge2 = self.v2 - self.v0;
        let h = ray.dir.cross(edge2);
        let a = edge1.dot(h);
        if a.abs() < 1e-12 {
            return None;
        }
        let f = 1.0 / a;
        let s = ray.origin - self.v0;
        let u = f * s.dot(h);
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = s.cross(edge1);
        let v = f * ray.dir.dot(q);
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = f * edge2.dot(q);
        if t <= ray.t_min || t >= ray.t_max {
            return None;
        }
        Some(Intersection {
            t,
            u,
            v,
            prim_id: self.prim_id,
            object_id: self.object_id,
        })
    }
}

/// Compute the geometric normal of the triangle (right-hand rule).
pub fn geom_normal(t: &ShadingTriangle) -> Vec3 {
    (t.v1 - t.v0).cross(t.v2 - t.v0).normalize()
}

/// Slab test for ray-AABB. Returns `Some((t_near, t_far))` if the ray hits
/// the box within the ray's `[t_min, t_max]` range, otherwise `None`.
///
/// `inv_dir` is `1.0 / ray.dir` (pre-computed once per traversal).
#[inline]
pub fn ray_aabb_t(ray: &Ray, inv_dir: Vec3, b: &Aabb) -> Option<(f32, f32)> {
    let t0 = (b.min - ray.origin) * inv_dir;
    let t1 = (b.max - ray.origin) * inv_dir;
    let tmin_v = t0.min(t1);
    let tmax_v = t0.max(t1);
    let tmin = tmin_v.x.max(tmin_v.y).max(tmin_v.z).max(ray.t_min);
    let tmax = tmax_v.x.min(tmax_v.y).min(tmax_v.z).min(ray.t_max);
    if tmin <= tmax {
        Some((tmin, tmax))
    } else {
        None
    }
}

const TRAVERSAL_STACK_SIZE: usize = 64;

/// Stack-based BVH traversal. Returns the closest intersection with any
/// triangle in `triangles` indexed via `bvh.prim_indices`. Mirrors the
/// Cycles kernel BVH traversal pattern: walk internal nodes ordered by
/// near-t, swap if left's t is greater than right's, push the far one,
/// descend into the near one.
pub fn closest_hit(bvh: &Bvh, triangles: &[ShadingTriangle], ray: &Ray) -> Option<Intersection> {
    if bvh.nodes.is_empty() {
        return None;
    }
    let inv_dir = Vec3::new(
        safe_inverse(ray.dir.x),
        safe_inverse(ray.dir.y),
        safe_inverse(ray.dir.z),
    );

    let mut stack: [u32; TRAVERSAL_STACK_SIZE] = [0; TRAVERSAL_STACK_SIZE];
    let mut sp = 0usize;
    let mut closest: Option<Intersection> = None;
    let mut active_ray = *ray;

    let mut node_idx: u32 = 0;
    loop {
        let node: &BvhNode = &bvh.nodes[node_idx as usize];

        if node.is_leaf() {
            let start = node.left_or_start as usize;
            let count = node.prim_count as usize;
            for slot in start..(start + count) {
                let tri_idx = bvh.prim_indices[slot] as usize;
                if let Some(hit) = triangles[tri_idx].intersect(&active_ray) {
                    if closest.map_or(true, |c| hit.t < c.t) {
                        active_ray.t_max = hit.t;
                        closest = Some(hit);
                    }
                }
            }
        } else {
            let left = node.left_or_start;
            let right = left + 1;
            let l_node = &bvh.nodes[left as usize];
            let r_node = &bvh.nodes[right as usize];
            let l_t = ray_aabb_t(&active_ray, inv_dir, &l_node.bounds);
            let r_t = ray_aabb_t(&active_ray, inv_dir, &r_node.bounds);
            match (l_t, r_t) {
                (Some((l_near, _)), Some((r_near, _))) => {
                    if l_near <= r_near {
                        if sp < TRAVERSAL_STACK_SIZE {
                            stack[sp] = right;
                            sp += 1;
                        }
                        node_idx = left;
                        continue;
                    }
                    if sp < TRAVERSAL_STACK_SIZE {
                        stack[sp] = left;
                        sp += 1;
                    }
                    node_idx = right;
                    continue;
                }
                (Some(_), None) => {
                    node_idx = left;
                    continue;
                }
                (None, Some(_)) => {
                    node_idx = right;
                    continue;
                }
                (None, None) => { /* fall through to stack pop */ }
            }
        }

        if sp == 0 {
            break;
        }
        sp -= 1;
        node_idx = stack[sp];
    }

    closest
}

/// Visibility test for shadow rays.
///
/// Returns `true` if the ray hits anything before `ray.t_max` — i.e. the
/// path from origin to `origin + t_max*dir` is occluded. Uses early-out
/// on the first hit (no need to find the closest), matching Cycles'
/// `PATH_RAY_SHADOW_OPAQUE` behaviour.
pub fn any_hit(bvh: &Bvh, triangles: &[ShadingTriangle], ray: &Ray) -> bool {
    if bvh.nodes.is_empty() {
        return false;
    }
    let inv_dir = Vec3::new(
        safe_inverse(ray.dir.x),
        safe_inverse(ray.dir.y),
        safe_inverse(ray.dir.z),
    );

    let mut stack: [u32; TRAVERSAL_STACK_SIZE] = [0; TRAVERSAL_STACK_SIZE];
    let mut sp = 0usize;
    let mut node_idx: u32 = 0;
    loop {
        let node = &bvh.nodes[node_idx as usize];
        if node.is_leaf() {
            let start = node.left_or_start as usize;
            let count = node.prim_count as usize;
            for slot in start..(start + count) {
                let tri_idx = bvh.prim_indices[slot] as usize;
                if triangles[tri_idx].intersect(ray).is_some() {
                    return true;
                }
            }
        } else {
            let left = node.left_or_start;
            let right = left + 1;
            let l_hit = ray_aabb_t(ray, inv_dir, &bvh.nodes[left as usize].bounds).is_some();
            let r_hit = ray_aabb_t(ray, inv_dir, &bvh.nodes[right as usize].bounds).is_some();
            if l_hit && r_hit {
                if sp < TRAVERSAL_STACK_SIZE {
                    stack[sp] = right;
                    sp += 1;
                }
                node_idx = left;
                continue;
            }
            if l_hit {
                node_idx = left;
                continue;
            }
            if r_hit {
                node_idx = right;
                continue;
            }
        }
        if sp == 0 {
            break;
        }
        sp -= 1;
        node_idx = stack[sp];
    }
    false
}

#[inline]
fn safe_inverse(x: f32) -> f32 {
    // Use a large finite number rather than +/-Inf to avoid NaNs in the
    // subsequent multiplication where `(b.min - origin) == 0`.
    if x.abs() < 1e-20 {
        if x >= 0.0 {
            1e20
        } else {
            -1e20
        }
    } else {
        1.0 / x
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bvh::BuilderTriangle;

    fn build_bvh_and_triangles(tris: Vec<(Vec3, Vec3, Vec3)>) -> (Bvh, Vec<ShadingTriangle>) {
        let builder_tris: Vec<_> = tris
            .iter()
            .enumerate()
            .map(|(i, (a, b, c))| BuilderTriangle {
                v0: *a,
                v1: *b,
                v2: *c,
                prim_id: i as u32,
            })
            .collect();
        let bvh = Bvh::build(&builder_tris);
        let shading: Vec<_> = tris
            .into_iter()
            .enumerate()
            .map(|(i, (a, b, c))| ShadingTriangle {
                v0: a,
                v1: b,
                v2: c,
                prim_id: i as u32,
                object_id: 0,
            })
            .collect();
        (bvh, shading)
    }

    #[test]
    fn ray_triangle_basic_hit() {
        let tri = ShadingTriangle {
            v0: Vec3::new(0.0, 0.0, 0.0),
            v1: Vec3::new(1.0, 0.0, 0.0),
            v2: Vec3::new(0.0, 1.0, 0.0),
            prim_id: 0,
            object_id: 0,
        };
        let ray = Ray::new(Vec3::new(0.25, 0.25, 1.0), Vec3::new(0.0, 0.0, -1.0));
        let h = tri.intersect(&ray).expect("hit");
        assert!((h.t - 1.0).abs() < 1e-5);
        assert!((h.u - 0.25).abs() < 1e-5);
        assert!((h.v - 0.25).abs() < 1e-5);
    }

    #[test]
    fn ray_triangle_miss_outside_uv() {
        let tri = ShadingTriangle {
            v0: Vec3::new(0.0, 0.0, 0.0),
            v1: Vec3::new(1.0, 0.0, 0.0),
            v2: Vec3::new(0.0, 1.0, 0.0),
            prim_id: 0,
            object_id: 0,
        };
        let ray = Ray::new(Vec3::new(2.0, 2.0, 1.0), Vec3::new(0.0, 0.0, -1.0));
        assert!(tri.intersect(&ray).is_none());
    }

    #[test]
    fn ray_triangle_miss_behind() {
        let tri = ShadingTriangle {
            v0: Vec3::new(0.0, 0.0, 0.0),
            v1: Vec3::new(1.0, 0.0, 0.0),
            v2: Vec3::new(0.0, 1.0, 0.0),
            prim_id: 0,
            object_id: 0,
        };
        let ray = Ray::new(Vec3::new(0.1, 0.1, -1.0), Vec3::new(0.0, 0.0, -1.0));
        assert!(tri.intersect(&ray).is_none());
    }

    #[test]
    fn ray_triangle_parallel() {
        let tri = ShadingTriangle {
            v0: Vec3::new(0.0, 0.0, 0.0),
            v1: Vec3::new(1.0, 0.0, 0.0),
            v2: Vec3::new(0.0, 1.0, 0.0),
            prim_id: 0,
            object_id: 0,
        };
        let ray = Ray::new(Vec3::new(0.1, 0.1, 1.0), Vec3::new(1.0, 0.0, 0.0));
        assert!(tri.intersect(&ray).is_none());
    }

    #[test]
    fn ray_aabb_inside_box_returns_hit() {
        let b = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        };
        let ray = Ray::new(Vec3::ZERO, Vec3::Z);
        let inv = Vec3::ONE / ray.dir;
        let hit = ray_aabb_t(&ray, inv, &b).unwrap();
        assert!(hit.0 <= 1.0 && hit.1 >= 0.0);
    }

    #[test]
    fn ray_aabb_axis_aligned_miss() {
        let b = Aabb {
            min: Vec3::new(10.0, 0.0, 0.0),
            max: Vec3::new(11.0, 1.0, 1.0),
        };
        let ray = Ray::new(Vec3::ZERO, Vec3::Y);
        let inv = Vec3::new(safe_inverse(0.0), safe_inverse(1.0), safe_inverse(0.0));
        assert!(ray_aabb_t(&ray, inv, &b).is_none());
    }

    #[test]
    fn bvh_traversal_finds_closest_hit() {
        let (bvh, tris) = build_bvh_and_triangles(vec![
            // Far quad at z=10
            (
                Vec3::new(-5.0, -5.0, 10.0),
                Vec3::new(5.0, -5.0, 10.0),
                Vec3::new(-5.0, 5.0, 10.0),
            ),
            // Near quad at z=2
            (
                Vec3::new(-5.0, -5.0, 2.0),
                Vec3::new(5.0, -5.0, 2.0),
                Vec3::new(-5.0, 5.0, 2.0),
            ),
        ]);
        let ray = Ray::new(Vec3::ZERO, Vec3::Z);
        let hit = closest_hit(&bvh, &tris, &ray).expect("hit");
        assert!((hit.t - 2.0).abs() < 1e-4, "got t={}", hit.t);
    }

    #[test]
    fn bvh_traversal_misses_empty_space() {
        let (bvh, tris) = build_bvh_and_triangles(vec![(
            Vec3::new(10.0, 10.0, 10.0),
            Vec3::new(11.0, 10.0, 10.0),
            Vec3::new(10.0, 11.0, 10.0),
        )]);
        let ray = Ray::new(Vec3::ZERO, Vec3::X);
        assert!(closest_hit(&bvh, &tris, &ray).is_none());
    }

    #[test]
    fn shadow_ray_finds_occlusion_early() {
        let (bvh, tris) = build_bvh_and_triangles(vec![(
            Vec3::new(-1.0, -1.0, 5.0),
            Vec3::new(1.0, -1.0, 5.0),
            Vec3::new(0.0, 1.0, 5.0),
        )]);
        let mut ray = Ray::new(Vec3::ZERO, Vec3::Z);
        ray.t_max = 10.0;
        assert!(any_hit(&bvh, &tris, &ray));
    }

    #[test]
    fn shadow_ray_no_occlusion_when_target_closer_than_geometry() {
        let (bvh, tris) = build_bvh_and_triangles(vec![(
            Vec3::new(-1.0, -1.0, 5.0),
            Vec3::new(1.0, -1.0, 5.0),
            Vec3::new(0.0, 1.0, 5.0),
        )]);
        let mut ray = Ray::new(Vec3::ZERO, Vec3::Z);
        ray.t_max = 3.0;
        assert!(!any_hit(&bvh, &tris, &ray));
    }

    #[test]
    fn bvh_traversal_finds_one_of_many() {
        // Build a 10x10 grid of small triangles and shoot a ray at a known one.
        let mut tris = Vec::new();
        for i in 0..10 {
            for j in 0..10 {
                let x = i as f32 * 2.0;
                let y = j as f32 * 2.0;
                tris.push((
                    Vec3::new(x, y, 5.0),
                    Vec3::new(x + 1.0, y, 5.0),
                    Vec3::new(x, y + 1.0, 5.0),
                ));
            }
        }
        let (bvh, tris) = build_bvh_and_triangles(tris);
        let ray = Ray::new(Vec3::new(6.25, 6.25, 0.0), Vec3::Z);
        let hit = closest_hit(&bvh, &tris, &ray).expect("hit");
        assert!((hit.t - 5.0).abs() < 1e-3);
    }
}
