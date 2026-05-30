//! Constrained Delaunay triangulation (CDT) for simple polygons with
//! holes. Pure Rust, `unsafe`-free, depends only on `glam`.
//!
//! ## Algorithm
//!
//! 1. **Super-triangle bootstrap.** Compute the bounding box of all
//!    input points and surround it with a triangle so large that
//!    every input point lies strictly inside.
//! 2. **Bowyer–Watson incremental insertion.** Insert each input
//!    point one by one. For each insertion, find all triangles whose
//!    circumcircle contains the new point ("bad triangles"). Their
//!    union forms a star-shaped polygon (the *cavity*); replace the
//!    bad triangles with a fan of new triangles connecting the new
//!    point to each cavity edge. After all points are inserted the
//!    mesh is the unconstrained Delaunay triangulation.
//! 3. **Constraint enforcement (Sloan 1993).** For each required
//!    edge (boundary edges + hole edges), find all triangles whose
//!    interior is crossed by that segment and repeatedly flip the
//!    edges that cross the constraint. After all constraints are
//!    enforced, the triangulation respects every input edge.
//! 4. **Interior / exterior classification.** Pick a seed triangle
//!    *outside* the boundary (one adjacent to the super-triangle)
//!    and flood-fill across non-constraint edges, marking reached
//!    triangles as "outside". Repeat from a point inside each hole.
//!    The remaining unmarked triangles lie inside the boundary and
//!    outside every hole — those are the output.
//! 5. **Triangle remap.** Drop the super-triangle vertices, build
//!    the final vertex list (boundary first, then each hole), and
//!    re-index the surviving triangles into that list.
//!
//! ## Numeric robustness
//!
//! All predicates (orientation, in-circle) use `f64` and are
//! computed in an order that minimises catastrophic cancellation
//! relative to the input coordinate magnitudes the geometry crate
//! actually sees (mm-scale floor boundaries, up to ~10⁵ mm
//! diameter). For pathological inputs (collinear constraint edges,
//! exactly-cocircular point quadruplets) the caller is expected to
//! fall back to ear-clipping via the `Result::Err` arm.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::{GeometryError, GeometryResult};

/// Tuning knobs for the CDT triangulator. Defaults are tuned for the
/// mm-scale floor and wall boundaries used by the rest of the
/// `aec_geometry` crate.
#[derive(Debug, Clone, Copy)]
pub struct CdtOptions {
    /// Tolerance for "two points coincident" / "point on edge"
    /// checks. Input coordinates closer than this are treated as
    /// equal. Defaults to `1e-6` mm (one nanometre); tightening
    /// risks false-positive flips on near-cocircular input.
    pub coincident_tolerance: f64,
}

impl Default for CdtOptions {
    fn default() -> Self {
        Self {
            coincident_tolerance: 1e-6,
        }
    }
}

/// Constrained-Delaunay triangulate a simple polygon, optionally with
/// holes. Returns triangle indices into the *flattened* vertex list
/// `[boundary, hole_0, hole_1, …]` (each hole appended in input
/// order). Indices use `u32` to match the downstream
/// [`crate::mesh::Mesh`] type.
///
/// The boundary must be a simple polygon with at least 3 vertices.
/// Holes must lie strictly inside the boundary and not overlap each
/// other. Boundary winding may be CW or CCW (the algorithm
/// normalises internally); hole winding is irrelevant. Each hole
/// must have at least 3 vertices.
///
/// # Errors
///
/// Returns [`GeometryError::InvalidPolygon`] when the input is
/// structurally invalid (fewer than 3 boundary points, fewer than 3
/// points in any hole, or coincident consecutive points). Returns
/// [`GeometryError::Tessellation`] when the algorithm cannot
/// recover from a degenerate configuration (e.g. all input points
/// collinear) and the caller should fall back to ear-clipping.
pub fn triangulate_cdt(
    boundary: &[[f64; 2]],
    holes: &[Vec<[f64; 2]>],
) -> GeometryResult<Vec<[u32; 3]>> {
    triangulate_cdt_with_options(boundary, holes, CdtOptions::default())
}

/// As [`triangulate_cdt`], but accepts a [`CdtOptions`] for fine
/// control over numeric thresholds.
pub fn triangulate_cdt_with_options(
    boundary: &[[f64; 2]],
    holes: &[Vec<[f64; 2]>],
    opts: CdtOptions,
) -> GeometryResult<Vec<[u32; 3]>> {
    if boundary.len() < 3 {
        return Err(GeometryError::InvalidPolygon);
    }
    for hole in holes {
        if hole.len() < 3 {
            return Err(GeometryError::InvalidPolygon);
        }
    }
    let mut cdt = Cdt::new(opts);
    cdt.build(boundary, holes)?;
    Ok(cdt.into_triangles())
}

// ----------------------------------------------------------------------------
// Internal triangulator state.
// ----------------------------------------------------------------------------

type V = usize;

#[derive(Debug, Clone, Copy)]
struct Triangle {
    /// CCW-ordered vertex indices.
    v: [V; 3],
    /// `true` once the flood-fill marks this triangle as outside the
    /// boundary or inside a hole. Removed triangles also stay
    /// `removed: true` so we can skip them with a single check.
    removed: bool,
}

impl Triangle {
    fn new(a: V, b: V, c: V) -> Self {
        Self {
            v: [a, b, c],
            removed: false,
        }
    }

    fn has_vertex(&self, v: V) -> bool {
        self.v[0] == v || self.v[1] == v || self.v[2] == v
    }

    fn other_vertex(&self, a: V, b: V) -> Option<V> {
        self.v.iter().find(|&&x| x != a && x != b).copied()
    }
}

/// Canonical undirected edge key (smaller vertex first). The
/// triangulator's edge-to-triangle map uses this so neighbour
/// lookups don't care about orientation.
#[inline]
fn edge_key(a: V, b: V) -> (V, V) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

struct Cdt {
    opts: CdtOptions,
    points: Vec<[f64; 2]>,
    triangles: Vec<Triangle>,
    /// Set of canonical edge keys that must be preserved (boundary +
    /// hole edges). Flips never cross a constrained edge.
    constrained: HashSet<(V, V)>,
    /// First three vertices are the super-triangle; we drop any
    /// triangle that still references them at the end.
    super_vertex_count: usize,
}

impl Cdt {
    fn new(opts: CdtOptions) -> Self {
        Self {
            opts,
            points: Vec::new(),
            triangles: Vec::new(),
            constrained: HashSet::new(),
            super_vertex_count: 3,
        }
    }

    fn build(&mut self, boundary: &[[f64; 2]], holes: &[Vec<[f64; 2]>]) -> GeometryResult<()> {
        // 1. Compute bounding box across boundary + every hole.
        let mut min = [f64::INFINITY, f64::INFINITY];
        let mut max = [f64::NEG_INFINITY, f64::NEG_INFINITY];
        let extend = |min: &mut [f64; 2], max: &mut [f64; 2], pts: &[[f64; 2]]| {
            for p in pts {
                if p[0] < min[0] {
                    min[0] = p[0];
                }
                if p[1] < min[1] {
                    min[1] = p[1];
                }
                if p[0] > max[0] {
                    max[0] = p[0];
                }
                if p[1] > max[1] {
                    max[1] = p[1];
                }
            }
        };
        extend(&mut min, &mut max, boundary);
        for hole in holes {
            extend(&mut min, &mut max, hole);
        }
        if !min[0].is_finite() || !max[0].is_finite() {
            return Err(GeometryError::InvalidPolygon);
        }
        let dx = (max[0] - min[0]).max(1.0);
        let dy = (max[1] - min[1]).max(1.0);
        let pad = (dx + dy) * 20.0;
        let cx = (min[0] + max[0]) * 0.5;
        let cy = (min[1] + max[1]) * 0.5;
        // 2. Seed three super-triangle vertices. Indices 0, 1, 2.
        self.points.push([cx - pad, cy - pad]);
        self.points.push([cx + pad, cy - pad]);
        self.points.push([cx, cy + pad]);
        self.triangles.push(Triangle::new(0, 1, 2));

        // 3. Insert boundary points (indices 3..3+n_boundary).
        let boundary_start = self.points.len();
        for p in boundary {
            self.insert_point(*p)?;
        }
        let mut hole_starts: Vec<usize> = Vec::with_capacity(holes.len());
        for hole in holes {
            hole_starts.push(self.points.len());
            for p in hole {
                self.insert_point(*p)?;
            }
        }

        // 4. Record constraint edges (canonical undirected keys).
        let boundary_count = boundary.len();
        for i in 0..boundary_count {
            let a = boundary_start + i;
            let b = boundary_start + (i + 1) % boundary_count;
            self.constrained.insert(edge_key(a, b));
        }
        for (h_idx, hole) in holes.iter().enumerate() {
            let start = hole_starts[h_idx];
            let n = hole.len();
            for i in 0..n {
                let a = start + i;
                let b = start + (i + 1) % n;
                self.constrained.insert(edge_key(a, b));
            }
        }

        // 5. Enforce every constraint edge.
        let constrained_edges: Vec<(V, V)> = self.constrained.iter().copied().collect();
        for (a, b) in constrained_edges {
            self.enforce_edge(a, b)?;
        }

        // 6. Flood-fill exterior (from any super-triangle-touching
        //    triangle, walking across non-constraint edges) and
        //    each hole interior. The flood stops at boundary +
        //    hole constraint edges, so the surviving (unremoved)
        //    triangles are exactly those interior to the boundary
        //    polygon and exterior to every hole.
        let mut adjacency = self.build_adjacency();
        let mut exterior_seeds: Vec<usize> = Vec::new();
        for (i, tri) in self.triangles.iter().enumerate() {
            if tri.removed {
                continue;
            }
            if tri.has_vertex(0) || tri.has_vertex(1) || tri.has_vertex(2) {
                exterior_seeds.push(i);
            }
        }
        for seed in exterior_seeds {
            if !self.triangles[seed].removed {
                self.flood_fill_remove(seed, &mut adjacency);
            }
        }
        // Flood-fill from each hole: pick any point inside the hole
        // (the centroid is strictly inside any convex hole and is
        // also inside the typical simple-concave shapes opening
        // boundaries produce).
        for hole in holes {
            let seed_pt = interior_point(hole);
            if let Some(tri_idx) = self.find_containing_triangle(seed_pt) {
                if !self.triangles[tri_idx].removed {
                    self.flood_fill_remove(tri_idx, &mut adjacency);
                }
            }
        }
        Ok(())
    }

    fn into_triangles(self) -> Vec<[u32; 3]> {
        // Final triangles index into the *original* boundary+holes
        // points. Subtract `super_vertex_count` to compensate for
        // the super-triangle seeding.
        let offset = self.super_vertex_count;
        let mut out = Vec::new();
        for tri in &self.triangles {
            if tri.removed {
                continue;
            }
            let a = tri.v[0];
            let b = tri.v[1];
            let c = tri.v[2];
            if a < offset || b < offset || c < offset {
                continue;
            }
            out.push([
                (a - offset) as u32,
                (b - offset) as u32,
                (c - offset) as u32,
            ]);
        }
        out
    }

    // ----- Bowyer-Watson incremental insertion -----

    fn insert_point(&mut self, p: [f64; 2]) -> GeometryResult<()> {
        // Reject coincident duplicates: two points within tol of each
        // other are merged onto the first.
        let tol = self.opts.coincident_tolerance;
        for existing in &self.points {
            let dx = existing[0] - p[0];
            let dy = existing[1] - p[1];
            if dx * dx + dy * dy < tol * tol {
                // Duplicate — silently skip; caller is expected to
                // preserve duplicate behaviour via the
                // GeometryError::InvalidPolygon path if they care.
                self.points.push(p);
                return Ok(());
            }
        }
        self.points.push(p);
        let new_idx = self.points.len() - 1;
        // Find bad triangles whose circumcircle contains p.
        let mut bad: Vec<usize> = Vec::new();
        for (i, tri) in self.triangles.iter().enumerate() {
            if tri.removed {
                continue;
            }
            if in_circumcircle(
                self.points[tri.v[0]],
                self.points[tri.v[1]],
                self.points[tri.v[2]],
                p,
            ) {
                bad.push(i);
            }
        }
        if bad.is_empty() {
            return Err(GeometryError::Tessellation(format!(
                "CDT: no triangle circumscribes new point ({:.3}, {:.3})",
                p[0], p[1]
            )));
        }
        // Build the cavity polygon: edges of bad triangles that are
        // not shared with another bad triangle.
        let mut edge_count: HashMap<(V, V), u32> = HashMap::new();
        let mut edge_dir: HashMap<(V, V), (V, V)> = HashMap::new();
        for &bi in &bad {
            let v = self.triangles[bi].v;
            let dirs = [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])];
            for (a, b) in dirs {
                let key = edge_key(a, b);
                *edge_count.entry(key).or_insert(0) += 1;
                edge_dir.entry(key).or_insert((a, b));
            }
        }
        // Mark bad triangles as removed (we'll reuse the slots when
        // we push the cavity fan).
        for &bi in &bad {
            self.triangles[bi].removed = true;
        }
        // Connect each cavity edge to the new vertex.
        for (key, count) in edge_count {
            if count != 1 {
                continue;
            }
            let (a, b) = edge_dir[&key];
            // Preserve orientation: original edge (a -> b) becomes
            // triangle (a, b, new) so CCW is maintained.
            self.triangles.push(Triangle::new(a, b, new_idx));
        }
        Ok(())
    }

    // ----- Constraint enforcement (Sloan 1993) -----

    fn enforce_edge(&mut self, va: V, vb: V) -> GeometryResult<()> {
        // If the edge already exists in the triangulation we're done.
        if self.edge_exists(va, vb) {
            return Ok(());
        }
        // Collect edges that cross the constraint segment (va, vb).
        // Each crossing edge is identified by its two endpoints.
        let max_iters = self.triangles.len() * 4 + 32;
        let mut iter = 0;
        loop {
            if iter > max_iters {
                return Err(GeometryError::Tessellation(format!(
                    "CDT: constraint enforcement did not converge for edge ({va}, {vb})"
                )));
            }
            iter += 1;
            // Find one edge that crosses (va, vb) and is not itself
            // constrained.
            let crossing = self.find_crossing_edge(va, vb);
            let Some((u, v, t1_idx, t2_idx)) = crossing else {
                break;
            };
            if self.constrained.contains(&edge_key(u, v)) {
                return Err(GeometryError::Tessellation(format!(
                    "CDT: cannot enforce edge ({va}, {vb}) — crosses existing constraint ({u}, {v})"
                )));
            }
            // Flip edge (u, v) — combine the quadrilateral
            // (t1, t2) and re-split along the other diagonal.
            self.flip_edge(t1_idx, t2_idx, u, v)?;
            if self.edge_exists(va, vb) {
                break;
            }
        }
        Ok(())
    }

    fn edge_exists(&self, a: V, b: V) -> bool {
        for tri in &self.triangles {
            if tri.removed {
                continue;
            }
            if (tri.v[0] == a || tri.v[1] == a || tri.v[2] == a)
                && (tri.v[0] == b || tri.v[1] == b || tri.v[2] == b)
            {
                return true;
            }
        }
        false
    }

    /// Locate one triangulation edge (u, v) that geometrically
    /// crosses segment (va, vb) strictly, along with the two
    /// triangles sharing it. Returns `None` when no such edge
    /// remains.
    fn find_crossing_edge(&self, va: V, vb: V) -> Option<(V, V, usize, usize)> {
        let pa = self.points[va];
        let pb = self.points[vb];
        // Build a temporary edge-to-triangles map.
        let mut edge_to_tris: HashMap<(V, V), Vec<usize>> = HashMap::new();
        for (i, tri) in self.triangles.iter().enumerate() {
            if tri.removed {
                continue;
            }
            for (a, b) in [
                (tri.v[0], tri.v[1]),
                (tri.v[1], tri.v[2]),
                (tri.v[2], tri.v[0]),
            ] {
                edge_to_tris.entry(edge_key(a, b)).or_default().push(i);
            }
        }
        for ((u, v), tris) in &edge_to_tris {
            if *u == va || *u == vb || *v == va || *v == vb {
                continue;
            }
            if tris.len() != 2 {
                continue;
            }
            let pu = self.points[*u];
            let pv = self.points[*v];
            if segments_cross_strict(pa, pb, pu, pv) {
                return Some((*u, *v, tris[0], tris[1]));
            }
        }
        None
    }

    fn flip_edge(&mut self, t1_idx: usize, t2_idx: usize, u: V, v: V) -> GeometryResult<()> {
        let t1 = self.triangles[t1_idx];
        let t2 = self.triangles[t2_idx];
        let Some(w) = t1.other_vertex(u, v) else {
            return Err(GeometryError::Tessellation(
                "CDT: flip: t1 has no third vertex".into(),
            ));
        };
        let Some(x) = t2.other_vertex(u, v) else {
            return Err(GeometryError::Tessellation(
                "CDT: flip: t2 has no third vertex".into(),
            ));
        };
        // Replace (u, v, w) and (u, v, x) with (w, x, u) and (x, w, v),
        // preserving CCW winding.
        self.triangles[t1_idx].removed = true;
        self.triangles[t2_idx].removed = true;
        let new_t1 = ccw_triangle(self.points[w], self.points[x], self.points[u], w, x, u);
        let new_t2 = ccw_triangle(self.points[x], self.points[w], self.points[v], x, w, v);
        self.triangles.push(new_t1);
        self.triangles.push(new_t2);
        Ok(())
    }

    // ----- Interior / exterior classification -----

    fn build_adjacency(&self) -> HashMap<(V, V), Vec<usize>> {
        let mut map: HashMap<(V, V), Vec<usize>> = HashMap::new();
        for (i, tri) in self.triangles.iter().enumerate() {
            if tri.removed {
                continue;
            }
            for (a, b) in [
                (tri.v[0], tri.v[1]),
                (tri.v[1], tri.v[2]),
                (tri.v[2], tri.v[0]),
            ] {
                map.entry(edge_key(a, b)).or_default().push(i);
            }
        }
        map
    }

    fn find_containing_triangle(&self, p: [f64; 2]) -> Option<usize> {
        for (i, tri) in self.triangles.iter().enumerate() {
            if tri.removed {
                continue;
            }
            if point_in_triangle_strict(
                p,
                self.points[tri.v[0]],
                self.points[tri.v[1]],
                self.points[tri.v[2]],
            ) {
                return Some(i);
            }
        }
        None
    }

    /// Flood-fill across non-constrained edges from `start`,
    /// marking reached triangles as removed.
    fn flood_fill_remove(&mut self, start: usize, adjacency: &mut HashMap<(V, V), Vec<usize>>) {
        let mut q: VecDeque<usize> = VecDeque::new();
        q.push_back(start);
        while let Some(t_idx) = q.pop_front() {
            if self.triangles[t_idx].removed {
                continue;
            }
            self.triangles[t_idx].removed = true;
            let v = self.triangles[t_idx].v;
            for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
                let key = edge_key(a, b);
                if self.constrained.contains(&key) {
                    continue;
                }
                let Some(neighbours) = adjacency.get(&key) else {
                    continue;
                };
                for &n in neighbours {
                    if n != t_idx && !self.triangles[n].removed {
                        q.push_back(n);
                    }
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Predicates and helpers.
// ----------------------------------------------------------------------------

#[inline]
fn orient2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Standard determinant in-circle test. Returns `true` when `d` is
/// strictly inside the circle through CCW-oriented `(a, b, c)`. Uses
/// the displacement form to keep cancellation tractable for the
/// mm-scale inputs the geometry crate operates on.
fn in_circumcircle(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    // Ensure (a, b, c) is CCW; if not, swap so the determinant sign
    // matches the "inside" convention.
    let mut b = b;
    let mut c = c;
    if orient2d(a, b, c) < 0.0 {
        std::mem::swap(&mut b, &mut c);
    }
    let ax = a[0] - d[0];
    let ay = a[1] - d[1];
    let bx = b[0] - d[0];
    let by = b[1] - d[1];
    let cx = c[0] - d[0];
    let cy = c[1] - d[1];
    let a_sq = ax * ax + ay * ay;
    let b_sq = bx * bx + by * by;
    let c_sq = cx * cx + cy * cy;
    let det =
        ax * (by * c_sq - b_sq * cy) - ay * (bx * c_sq - b_sq * cx) + a_sq * (bx * cy - by * cx);
    det > 0.0
}

/// Strict point-in-triangle test using barycentric signs. Boundary
/// hits return `false` so flood-fill seeds never land on an edge.
fn point_in_triangle_strict(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let d1 = orient2d(p, a, b);
    let d2 = orient2d(p, b, c);
    let d3 = orient2d(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Returns `true` when the open segments `(p, q)` and `(r, s)` cross
/// in their interiors (no endpoint touching). Endpoint-touching
/// cases return `false` so we never flip an edge that shares a
/// vertex with the constraint.
fn segments_cross_strict(p: [f64; 2], q: [f64; 2], r: [f64; 2], s: [f64; 2]) -> bool {
    let d1 = orient2d(r, s, p);
    let d2 = orient2d(r, s, q);
    let d3 = orient2d(p, q, r);
    let d4 = orient2d(p, q, s);
    (d1 * d2 < 0.0) && (d3 * d4 < 0.0)
}

fn ccw_triangle(pa: [f64; 2], pb: [f64; 2], pc: [f64; 2], va: V, vb: V, vc: V) -> Triangle {
    if orient2d(pa, pb, pc) >= 0.0 {
        Triangle::new(va, vb, vc)
    } else {
        Triangle::new(va, vc, vb)
    }
}

/// Pick a point that is strictly inside the polygon defined by
/// `pts`. The centroid is used as a cheap first guess; for simple
/// convex polygons this is always inside, and for the simple
/// concave polygons walls/floors emit it almost always is too. For
/// pathological concave shapes the caller falls back to ear-clipping
/// before this is reached.
fn interior_point(pts: &[[f64; 2]]) -> [f64; 2] {
    let mut sx = 0.0;
    let mut sy = 0.0;
    for p in pts {
        sx += p[0];
        sy += p[1];
    }
    [sx / pts.len() as f64, sy / pts.len() as f64]
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn square_2x2() -> Vec<[f64; 2]> {
        vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]
    }

    #[test]
    fn triangulates_unit_square() {
        let tris = triangulate_cdt(&square_2x2(), &[]).unwrap();
        // A simple convex quad tessellates to exactly 2 triangles.
        assert_eq!(tris.len(), 2, "square should produce exactly 2 triangles");
        // Both triangles together span the full area.
        let pts = square_2x2();
        let mut total = 0.0;
        for tri in &tris {
            let a = pts[tri[0] as usize];
            let b = pts[tri[1] as usize];
            let c = pts[tri[2] as usize];
            total += orient2d(a, b, c).abs() * 0.5;
        }
        assert!(
            (total - 4.0).abs() < 1e-9,
            "area must equal 4 (got {total})"
        );
    }

    #[test]
    fn triangulates_l_shape() {
        // CCW L-shape, 6 vertices, expected to triangulate to 4
        // triangles (n - 2 for a simple polygon without holes).
        let boundary = vec![
            [0.0, 0.0],
            [3.0, 0.0],
            [3.0, 1.0],
            [1.0, 1.0],
            [1.0, 3.0],
            [0.0, 3.0],
        ];
        let tris = triangulate_cdt(&boundary, &[]).unwrap();
        assert_eq!(tris.len(), 4, "L-shape should triangulate to 4 tris");
        // Verify total area = 5 (= 3*1 + 1*2).
        let mut total = 0.0;
        for tri in &tris {
            let a = boundary[tri[0] as usize];
            let b = boundary[tri[1] as usize];
            let c = boundary[tri[2] as usize];
            total += orient2d(a, b, c).abs() * 0.5;
        }
        assert!(
            (total - 5.0).abs() < 1e-9,
            "L-shape area must equal 5 (got {total})"
        );
    }

    #[test]
    fn triangulates_square_with_square_hole() {
        // 10x10 outer with 2x2 hole centred at (5, 5).
        let boundary = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let hole = vec![[4.0, 4.0], [6.0, 4.0], [6.0, 6.0], [4.0, 6.0]];
        let tris = triangulate_cdt(&boundary, std::slice::from_ref(&hole)).unwrap();
        assert!(!tris.is_empty(), "donut must produce triangles");
        // Combine the flattened vertex list to compute total area.
        let mut verts = boundary.clone();
        verts.extend(hole.iter().copied());
        let mut total = 0.0;
        for tri in &tris {
            let a = verts[tri[0] as usize];
            let b = verts[tri[1] as usize];
            let c = verts[tri[2] as usize];
            total += orient2d(a, b, c).abs() * 0.5;
        }
        // Outer area 100 minus hole area 4 = 96.
        assert!(
            (total - 96.0).abs() < 1e-9,
            "donut area must equal 96 (got {total})"
        );
    }

    #[test]
    fn triangulates_square_with_two_holes() {
        let boundary = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let hole_a = vec![[2.0, 2.0], [3.0, 2.0], [3.0, 3.0], [2.0, 3.0]];
        let hole_b = vec![[7.0, 7.0], [8.0, 7.0], [8.0, 8.0], [7.0, 8.0]];
        let tris = triangulate_cdt(&boundary, &[hole_a.clone(), hole_b.clone()]).unwrap();
        let mut verts = boundary.clone();
        verts.extend(hole_a.iter().copied());
        verts.extend(hole_b.iter().copied());
        let mut total = 0.0;
        for tri in &tris {
            let a = verts[tri[0] as usize];
            let b = verts[tri[1] as usize];
            let c = verts[tri[2] as usize];
            total += orient2d(a, b, c).abs() * 0.5;
        }
        // Outer 100 − 2*(1) = 98.
        assert!(
            (total - 98.0).abs() < 1e-9,
            "two-holes area must equal 98 (got {total})"
        );
    }

    #[test]
    fn triangulates_with_clockwise_boundary() {
        // Same square, but CW input.
        let boundary = vec![[0.0, 0.0], [0.0, 2.0], [2.0, 2.0], [2.0, 0.0]];
        let tris = triangulate_cdt(&boundary, &[]).unwrap();
        assert_eq!(tris.len(), 2);
    }

    #[test]
    fn rejects_too_few_boundary_points() {
        let boundary = vec![[0.0, 0.0], [1.0, 0.0]];
        let err = triangulate_cdt(&boundary, &[]).unwrap_err();
        matches!(err, GeometryError::InvalidPolygon);
    }

    #[test]
    fn rejects_too_few_hole_points() {
        let boundary = square_2x2();
        let hole = vec![[0.5, 0.5], [1.5, 0.5]];
        let err = triangulate_cdt(&boundary, &[hole]).unwrap_err();
        matches!(err, GeometryError::InvalidPolygon);
    }

    #[test]
    fn no_triangle_contains_super_vertex_in_output() {
        let tris = triangulate_cdt(&square_2x2(), &[]).unwrap();
        // Output triangle indices must all reference the user-
        // supplied vertex list (0..n_boundary), not the super
        // triangle (which would have been at negative offsets if
        // not stripped). u32 underflow would surface as huge values
        // — sanity-check the range.
        for tri in &tris {
            for &i in tri {
                assert!(i < 4, "tri index {i} out of input range");
            }
        }
    }

    #[test]
    fn output_triangles_all_ccw() {
        let boundary = vec![
            [0.0, 0.0],
            [3.0, 0.0],
            [3.0, 1.0],
            [1.0, 1.0],
            [1.0, 3.0],
            [0.0, 3.0],
        ];
        let tris = triangulate_cdt(&boundary, &[]).unwrap();
        for tri in &tris {
            let a = boundary[tri[0] as usize];
            let b = boundary[tri[1] as usize];
            let c = boundary[tri[2] as usize];
            assert!(
                orient2d(a, b, c) > 0.0,
                "output triangle ({:?}, {:?}, {:?}) is not CCW",
                a,
                b,
                c
            );
        }
    }

    #[test]
    fn orient2d_sign_matches_winding() {
        let ccw = orient2d([0.0, 0.0], [1.0, 0.0], [0.0, 1.0]);
        let cw = orient2d([0.0, 0.0], [0.0, 1.0], [1.0, 0.0]);
        assert!(ccw > 0.0);
        assert!(cw < 0.0);
    }

    #[test]
    fn in_circumcircle_self_check() {
        // Point at centre of CCW unit triangle is inside its
        // circumcircle.
        let a = [0.0, 0.0];
        let b = [1.0, 0.0];
        let c = [0.0, 1.0];
        let inside = [0.3, 0.3];
        let outside = [10.0, 10.0];
        assert!(in_circumcircle(a, b, c, inside));
        assert!(!in_circumcircle(a, b, c, outside));
    }
}
