//! Quadric error metric (QEM) mesh simplification.
//!
//! Short single-character bindings (`a`, `b`, `c`, `d`, `p0`, `p1`, …)
//! are deliberate throughout this module — they mirror the linear-
//! algebra notation used in Garland & Heckbert's 1997 paper so the code
//! reads as a literal translation of the math. Forcing descriptive
//! names like `plane_coefficient_a` would obscure the algorithm rather
//! than clarify it.
//!
//! Implements Garland & Heckbert's 1997 paper *Surface Simplification Using
//! Quadric Error Metrics*. We compute, for each vertex, a 4×4 symmetric
//! "quadric" matrix `Q` whose value `vᵀ Q v` measures the squared distance
//! from `v` to the incident face planes. The cost of collapsing an edge
//! `(i, j)` is then `(v_new)ᵀ (Q_i + Q_j) (v_new)`, minimised over `v_new`
//! by solving a 3×3 linear system (with a midpoint fallback when the
//! system is singular).
//!
//! A min-heap drives the contraction order. Each collapse:
//!   1. Picks the lowest-cost edge whose endpoint versions still match
//!      (lazy invalidation; stale entries are discarded on pop).
//!   2. Refuses collapses that would invert any incident face normal
//!      (topology safety).
//!   3. Refuses collapses on boundary or non-manifold edges to keep the
//!      mesh closed.
//!   4. Merges the two vertex adjacency lists, re-evaluates costs for
//!      every neighbour edge, pushes the new entries onto the heap.
//!   5. Bumps the kept vertex's version so the stale neighbours pop and
//!      get discarded.
//!
//! Repeats until the live triangle count reaches the target. The
//! decimated mesh is rebuilt from the surviving triangles with the
//! contracted positions, with UVs and normals reseated barycentrically
//! at each `v_new`.
//!
//! The implementation is pure Rust (no `meshopt` C++ binding) and is
//! Phase-9 compliant: no external runtime deps beyond the workspace's
//! existing crates.

#![allow(clippy::many_single_char_names)]

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use aec_geometry::Mesh;

/// Symmetric 4×4 matrix stored as 10 unique components in row-major
/// upper-triangle order: `[m00, m01, m02, m03, m11, m12, m13, m22, m23, m33]`.
///
/// All arithmetic is `f64` to keep cumulative quadric error well above
/// the floating-point noise floor for meshes with millions of faces. The
/// memory cost (80 bytes per vertex) is negligible next to the position
/// array.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Quadric {
    m: [f64; 10],
}

impl Quadric {
    /// Identity-zero quadric.
    #[must_use]
    pub const fn zero() -> Self {
        Self { m: [0.0; 10] }
    }

    /// Build the outer product of plane coefficients `(a, b, c, d)`,
    /// i.e. the quadric for the half-space `ax + by + cz + d = 0`.
    /// `(a, b, c)` is assumed unit-length.
    #[must_use]
    pub fn from_plane(a: f64, b: f64, c: f64, d: f64) -> Self {
        Self {
            m: [
                a * a,
                a * b,
                a * c,
                a * d,
                b * b,
                b * c,
                b * d,
                c * c,
                c * d,
                d * d,
            ],
        }
    }

    /// Build the quadric of the plane spanned by three points (CCW from
    /// outside the surface). Returns `Quadric::zero()` for degenerate
    /// (zero-area) triangles so they contribute nothing.
    #[must_use]
    pub fn from_triangle(p0: [f64; 3], p1: [f64; 3], p2: [f64; 3]) -> Self {
        let e1 = sub(p1, p0);
        let e2 = sub(p2, p0);
        let n = cross(e1, e2);
        let len = norm(n);
        if len < 1e-30 {
            return Self::zero();
        }
        let inv = 1.0 / len;
        let (a, b, c) = (n[0] * inv, n[1] * inv, n[2] * inv);
        let d = -(a * p0[0] + b * p0[1] + c * p0[2]);
        Self::from_plane(a, b, c, d)
    }

    /// In-place sum: `self += other`.
    pub fn add_assign(&mut self, other: &Self) {
        for i in 0..10 {
            self.m[i] += other.m[i];
        }
    }

    /// Sum of two quadrics.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        let mut out = *self;
        out.add_assign(other);
        out
    }

    /// Evaluate `vᵀ Q v` for the homogeneous point `(x, y, z, 1)`. This is
    /// the squared distance from `v` to the planes contributing to `Q`.
    #[must_use]
    pub fn evaluate(&self, v: [f64; 3]) -> f64 {
        let (x, y, z) = (v[0], v[1], v[2]);
        let m = &self.m;
        m[0] * x * x
            + 2.0 * m[1] * x * y
            + 2.0 * m[2] * x * z
            + 2.0 * m[3] * x
            + m[4] * y * y
            + 2.0 * m[5] * y * z
            + 2.0 * m[6] * y
            + m[7] * z * z
            + 2.0 * m[8] * z
            + m[9]
    }

    /// Solve the 3×3 system `A v = -b` for the optimal position `v` that
    /// minimises the quadric. Returns `None` if the linear system is
    /// singular (caller falls back to midpoint).
    ///
    /// `A` is the upper-left 3×3 of the symmetric matrix; `b` is the
    /// top-right 3×1 column.
    #[must_use]
    pub fn optimal_position(&self) -> Option<[f64; 3]> {
        let m = &self.m;
        let a = [[m[0], m[1], m[2]], [m[1], m[4], m[5]], [m[2], m[5], m[7]]];
        let b = [-m[3], -m[6], -m[8]];
        solve_3x3(a, b)
    }
}

/// Decimation options.
#[derive(Debug, Clone, Copy)]
pub struct DecimateOptions {
    /// Target triangle count. Decimation stops as soon as the live face
    /// count drops to or below this number.
    pub target_triangle_count: u32,
    /// Maximum allowed cost for any single collapse. Higher values
    /// permit larger geometric error but more aggressive reduction.
    /// `f64::INFINITY` disables the cap.
    pub max_cost: f64,
    /// Prevent collapses on edges that lie on a topological boundary
    /// (only one incident triangle). Default `true`.
    pub preserve_boundary: bool,
}

impl Default for DecimateOptions {
    fn default() -> Self {
        Self {
            target_triangle_count: 0,
            max_cost: f64::INFINITY,
            preserve_boundary: true,
        }
    }
}

/// Errors returned by [`decimate`].
#[derive(Debug, thiserror::Error)]
pub enum DecimateError {
    #[error("mesh has no triangles to decimate")]
    Empty,
    #[error("mesh has positions/normals/uvs arrays of mismatched length")]
    ArrayMismatch,
    #[error("triangle indices reference vertex {index} but mesh has {len} positions")]
    IndexOutOfRange { index: u32, len: u32 },
    #[error("target triangle count ({target}) is greater than input ({input})")]
    TargetTooLarge { target: u32, input: u32 },
}

/// One collapsable edge, ordered so [`std::collections::BinaryHeap`] (a
/// max-heap) pops the lowest-cost edge first. We invert the cost via
/// [`Ord::cmp`] reversal.
#[derive(Debug, Clone, Copy)]
struct HeapEntry {
    cost: f64,
    v0: u32,
    v1: u32,
    v_new: [f64; 3],
    v0_version: u32,
    v1_version: u32,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lower cost should sort GREATER so the max-heap pops it first.
        // NaN handling: we never construct NaN costs in practice
        // (`Quadric::evaluate` is a sum of squares; `optimal_position`
        // is gated by a finite determinant check), but to keep `Ord`
        // strictly total even if a future code path introduces one,
        // we sort NaN as the LEAST-preferred edge (Ordering::Less in
        // the max-heap = pops last) by treating any NaN as "greater
        // cost than any finite cost". Using `unwrap_or(Equal)` would
        // make the ordering non-transitive (NaN == every finite cost
        // but two finite costs ≠ each other), which violates BinaryHeap's
        // invariant.
        match (self.cost.is_nan(), other.cost.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => other
                .cost
                .partial_cmp(&self.cost)
                .unwrap_or(Ordering::Equal),
        }
    }
}

/// Decimate a mesh down to (at most) `opts.target_triangle_count` faces.
///
/// Returns a new [`Mesh`] holding only the surviving vertices and
/// triangles with positions contracted, and UVs/normals reseated
/// barycentrically at each contracted point.
pub fn decimate(input: &Mesh, opts: &DecimateOptions) -> Result<Mesh, DecimateError> {
    if input.indices.is_empty() {
        return Err(DecimateError::Empty);
    }
    if input.normals.len() != input.positions.len() || input.uvs.len() != input.positions.len() {
        return Err(DecimateError::ArrayMismatch);
    }
    if input.indices.len() % 3 != 0 {
        return Err(DecimateError::ArrayMismatch);
    }
    let pos_len = u32::try_from(input.positions.len()).unwrap_or(u32::MAX);
    for &idx in &input.indices {
        if idx >= pos_len {
            return Err(DecimateError::IndexOutOfRange {
                index: idx,
                len: pos_len,
            });
        }
    }

    let input_tri_count = u32::try_from(input.indices.len() / 3).unwrap_or(u32::MAX);
    if opts.target_triangle_count > input_tri_count {
        return Err(DecimateError::TargetTooLarge {
            target: opts.target_triangle_count,
            input: input_tri_count,
        });
    }
    if opts.target_triangle_count == input_tri_count {
        return Ok(input.clone());
    }

    let mut state = DecimateState::new(input);
    state.run(opts);
    Ok(state.export())
}

/// Internal mutable working set.
struct DecimateState {
    positions: Vec<[f64; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    quadrics: Vec<Quadric>,
    /// Triangles as triplets of vertex indices. A triangle is "dead" when
    /// any of its indices equals `u32::MAX`.
    triangles: Vec<[u32; 3]>,
    /// Per-vertex adjacency list of triangle indices. Vertices removed by
    /// collapse end up with an empty list.
    vertex_tris: Vec<Vec<u32>>,
    /// Per-vertex monotonic version counter; bumped on each collapse so
    /// stale heap entries can be lazily filtered out.
    vertex_versions: Vec<u32>,
    /// Set of "dead" (collapsed-away) vertex indices; we skip them on
    /// export and exclude them from new heap entries.
    vertex_alive: Vec<bool>,
    /// `true` if the vertex sits on a topological boundary loop (touches
    /// an edge that has only one incident triangle). Computed once at
    /// init and updated on collapse.
    vertex_on_boundary: Vec<bool>,
    live_triangles: u32,
    heap: BinaryHeap<HeapEntry>,
}

impl DecimateState {
    fn new(mesh: &Mesh) -> Self {
        let n_vert = mesh.positions.len();
        let positions: Vec<[f64; 3]> = mesh
            .positions
            .iter()
            .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
            .collect();
        let normals = mesh.normals.clone();
        let uvs = mesh.uvs.clone();
        let triangles: Vec<[u32; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();

        // Quadrics: sum of plane quadrics of incident faces.
        let mut quadrics = vec![Quadric::zero(); n_vert];
        let mut vertex_tris: Vec<Vec<u32>> = vec![Vec::new(); n_vert];
        for (ti, tri) in triangles.iter().enumerate() {
            let (a, b, c) = (
                positions[tri[0] as usize],
                positions[tri[1] as usize],
                positions[tri[2] as usize],
            );
            let q = Quadric::from_triangle(a, b, c);
            quadrics[tri[0] as usize].add_assign(&q);
            quadrics[tri[1] as usize].add_assign(&q);
            quadrics[tri[2] as usize].add_assign(&q);
            for &v in tri {
                vertex_tris[v as usize].push(u32::try_from(ti).unwrap_or(u32::MAX));
            }
        }

        // Mark boundary vertices: a vertex is on the boundary if any of
        // its incident edges has exactly one adjacent triangle.
        let mut edge_face_count: HashMap<(u32, u32), u32> = HashMap::new();
        for tri in &triangles {
            for k in 0..3 {
                let a = tri[k];
                let b = tri[(k + 1) % 3];
                *edge_face_count.entry(order_pair(a, b)).or_insert(0) += 1;
            }
        }
        let mut vertex_on_boundary = vec![false; n_vert];
        for ((a, b), c) in &edge_face_count {
            if *c == 1 {
                vertex_on_boundary[*a as usize] = true;
                vertex_on_boundary[*b as usize] = true;
            }
        }

        Self {
            positions,
            normals,
            uvs,
            quadrics,
            triangles,
            vertex_tris,
            vertex_versions: vec![0; n_vert],
            vertex_alive: vec![true; n_vert],
            vertex_on_boundary,
            live_triangles: u32::try_from(mesh.indices.len() / 3).unwrap_or(u32::MAX),
            heap: BinaryHeap::new(),
        }
    }

    fn run(&mut self, opts: &DecimateOptions) {
        self.seed_heap(opts);
        while self.live_triangles > opts.target_triangle_count {
            let Some(entry) = self.heap.pop() else { break };
            if !self.entry_is_fresh(&entry) {
                continue;
            }
            if entry.cost > opts.max_cost {
                break;
            }
            if !self.can_collapse(entry.v0, entry.v1, entry.v_new, opts.preserve_boundary) {
                continue;
            }
            self.collapse(entry.v0, entry.v1, entry.v_new);
            self.push_neighbour_edges(entry.v0, opts);
        }
    }

    fn seed_heap(&mut self, opts: &DecimateOptions) {
        // Collect undirected edges as `(min, max)` pairs to avoid duplicates.
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        for tri in &self.triangles {
            for k in 0..3 {
                let a = tri[k];
                let b = tri[(k + 1) % 3];
                let key = order_pair(a, b);
                seen.insert(key);
            }
        }
        for (v0, v1) in seen {
            self.push_edge(v0, v1, opts);
        }
    }

    fn push_neighbour_edges(&mut self, v: u32, opts: &DecimateOptions) {
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        for &ti in &self.vertex_tris[v as usize] {
            let tri = self.triangles[ti as usize];
            if tri[0] == u32::MAX {
                continue;
            }
            for k in 0..3 {
                let a = tri[k];
                let b = tri[(k + 1) % 3];
                seen.insert(order_pair(a, b));
            }
        }
        for (a, b) in seen {
            self.push_edge(a, b, opts);
        }
    }

    fn push_edge(&mut self, v0: u32, v1: u32, opts: &DecimateOptions) {
        if !self.vertex_alive[v0 as usize] || !self.vertex_alive[v1 as usize] {
            return;
        }
        let qsum = self.quadrics[v0 as usize].add(&self.quadrics[v1 as usize]);
        let p0 = self.positions[v0 as usize];
        let p1 = self.positions[v1 as usize];
        let v_new = optimal_or_midpoint(&qsum, p0, p1);
        let cost = qsum.evaluate(v_new).max(0.0);
        if cost > opts.max_cost {
            return;
        }
        self.heap.push(HeapEntry {
            cost,
            v0,
            v1,
            v_new,
            v0_version: self.vertex_versions[v0 as usize],
            v1_version: self.vertex_versions[v1 as usize],
        });
    }

    fn entry_is_fresh(&self, entry: &HeapEntry) -> bool {
        self.vertex_alive[entry.v0 as usize]
            && self.vertex_alive[entry.v1 as usize]
            && self.vertex_versions[entry.v0 as usize] == entry.v0_version
            && self.vertex_versions[entry.v1 as usize] == entry.v1_version
    }

    fn can_collapse(&self, v0: u32, v1: u32, v_new: [f64; 3], preserve_boundary: bool) -> bool {
        if v0 == v1 {
            return false;
        }
        // Boundary preservation: when the flag is set, refuse to collapse
        // any edge that has a boundary-vertex endpoint. This is stricter
        // than "the edge itself is a boundary edge" because an interior
        // edge connecting a boundary vertex to an interior vertex still
        // moves the boundary vertex inward, shrinking the silhouette.
        if preserve_boundary
            && (self.vertex_on_boundary[v0 as usize] || self.vertex_on_boundary[v1 as usize])
        {
            return false;
        }
        // Collect incident triangles touching either endpoint. A
        // triangle containing BOTH endpoints appears in both adjacency
        // lists, so we must deduplicate the triangle indices before
        // counting — otherwise every shared face is counted twice and
        // manifold interior edges (truly shared_count == 2) end up
        // measured as 4, tripping the >2 rejection below and refusing
        // every non-boundary collapse.
        let mut seen: HashSet<u32> = HashSet::new();
        let mut shared_count = 0_u32;
        for &ti in self.vertex_tris[v0 as usize]
            .iter()
            .chain(self.vertex_tris[v1 as usize].iter())
        {
            if !seen.insert(ti) {
                continue;
            }
            let tri = self.triangles[ti as usize];
            if tri[0] == u32::MAX {
                continue;
            }
            let has_v0 = tri.contains(&v0);
            let has_v1 = tri.contains(&v1);
            if has_v0 && has_v1 {
                shared_count += 1;
            }
        }
        // A manifold interior edge has exactly two shared faces; a
        // boundary edge has one. Non-manifold (>2) we always refuse.
        if shared_count == 0 {
            return false;
        }
        if shared_count > 2 {
            return false;
        }
        // Flip check: every kept triangle (touches v0 or v1 but not both)
        // must keep the same normal sign with v_new substituted for the
        // collapsed vertex.
        for &endpoint in &[v0, v1] {
            for &ti in &self.vertex_tris[endpoint as usize] {
                let tri = self.triangles[ti as usize];
                if tri[0] == u32::MAX {
                    continue;
                }
                let has_v0 = tri.contains(&v0);
                let has_v1 = tri.contains(&v1);
                if has_v0 && has_v1 {
                    continue; // removed by the collapse
                }
                let new_tri = [
                    if tri[0] == endpoint { u32::MAX } else { tri[0] },
                    if tri[1] == endpoint { u32::MAX } else { tri[1] },
                    if tri[2] == endpoint { u32::MAX } else { tri[2] },
                ];
                // Compute both normals and reject sign flips.
                let original_normal = triangle_normal(
                    self.positions[tri[0] as usize],
                    self.positions[tri[1] as usize],
                    self.positions[tri[2] as usize],
                );
                let mapped = [
                    if new_tri[0] == u32::MAX {
                        v_new
                    } else {
                        self.positions[new_tri[0] as usize]
                    },
                    if new_tri[1] == u32::MAX {
                        v_new
                    } else {
                        self.positions[new_tri[1] as usize]
                    },
                    if new_tri[2] == u32::MAX {
                        v_new
                    } else {
                        self.positions[new_tri[2] as usize]
                    },
                ];
                let new_normal = triangle_normal(mapped[0], mapped[1], mapped[2]);
                if dot(original_normal, new_normal) < 0.0 {
                    return false;
                }
            }
        }
        true
    }

    fn collapse(&mut self, v0: u32, v1: u32, v_new: [f64; 3]) {
        // Convention: keep v0, discard v1. Merge v1's adjacency and
        // quadric into v0; replace v1 indices in surviving triangles.
        let qsum = self.quadrics[v0 as usize].add(&self.quadrics[v1 as usize]);
        self.quadrics[v0 as usize] = qsum;

        // Compute the barycentric alpha BEFORE overwriting v0's
        // position. Otherwise `barycentric_alpha(v_new, p1, v_new)`
        // always returns 0, freezing v0's original normal/uv on every
        // collapse regardless of where `v_new` actually landed.
        let p0_old = self.positions[v0 as usize];
        let p1_old = self.positions[v1 as usize];
        self.positions[v0 as usize] = v_new;

        // Reseat normal/uv barycentrically along the edge for v0.
        let alpha = barycentric_alpha(p0_old, p1_old, v_new);
        let new_normal = lerp3(self.normals[v0 as usize], self.normals[v1 as usize], alpha);
        let new_uv = lerp2(self.uvs[v0 as usize], self.uvs[v1 as usize], alpha);
        self.normals[v0 as usize] = normalize_or_keep(new_normal, self.normals[v0 as usize]);
        self.uvs[v0 as usize] = new_uv;

        // Walk v1's triangles. Triangles using both endpoints become
        // degenerate -- mark them dead. Others get their v1 -> v0.
        let v1_tris = std::mem::take(&mut self.vertex_tris[v1 as usize]);
        for ti in v1_tris {
            let tri = &mut self.triangles[ti as usize];
            if tri[0] == u32::MAX {
                continue;
            }
            let has_v0 = tri.contains(&v0);
            let has_v1 = tri.contains(&v1);
            if has_v0 && has_v1 {
                *tri = [u32::MAX; 3];
                self.live_triangles = self.live_triangles.saturating_sub(1);
            } else if has_v1 {
                for slot in tri.iter_mut() {
                    if *slot == v1 {
                        *slot = v0;
                    }
                }
                self.vertex_tris[v0 as usize].push(ti);
            }
        }
        // Drop v1.
        self.vertex_alive[v1 as usize] = false;
        self.vertex_versions[v1 as usize] = self.vertex_versions[v1 as usize].wrapping_add(1);
        self.vertex_versions[v0 as usize] = self.vertex_versions[v0 as usize].wrapping_add(1);
        // Also clear any duplicate triangle refs in v0's list (defensive).
        self.vertex_tris[v0 as usize].sort_unstable();
        self.vertex_tris[v0 as usize].dedup();
        self.vertex_tris[v0 as usize].retain(|&ti| self.triangles[ti as usize][0] != u32::MAX);
    }

    fn export(self) -> Mesh {
        // Compact vertex array: only keep alive vertices.
        let n = self.positions.len();
        let mut remap = vec![u32::MAX; n];
        let mut out_positions: Vec<[f32; 3]> = Vec::new();
        let mut out_normals: Vec<[f32; 3]> = Vec::new();
        let mut out_uvs: Vec<[f32; 2]> = Vec::new();
        for (i, alive) in self.vertex_alive.iter().enumerate() {
            if *alive {
                remap[i] = u32::try_from(out_positions.len()).unwrap_or(u32::MAX);
                let p = self.positions[i];
                out_positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
                out_normals.push(self.normals[i]);
                out_uvs.push(self.uvs[i]);
            }
        }
        let mut out_indices: Vec<u32> = Vec::new();
        for tri in &self.triangles {
            if tri[0] == u32::MAX {
                continue;
            }
            let a = remap[tri[0] as usize];
            let b = remap[tri[1] as usize];
            let c = remap[tri[2] as usize];
            if a == u32::MAX || b == u32::MAX || c == u32::MAX || a == b || b == c || a == c {
                continue;
            }
            out_indices.extend_from_slice(&[a, b, c]);
        }
        Mesh {
            positions: out_positions,
            normals: out_normals,
            uvs: out_uvs,
            indices: out_indices,
            attributes: Vec::new(),
        }
    }
}

fn order_pair(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn optimal_or_midpoint(q: &Quadric, p0: [f64; 3], p1: [f64; 3]) -> [f64; 3] {
    if let Some(v) = q.optimal_position() {
        // Sanity: reject points that wander outside a generous bounding
        // sphere around the segment -- the linear solve can be ill-
        // conditioned for nearly-coplanar incident faces.
        let mid = midpoint(p0, p1);
        let r = norm(sub(p0, p1)) * 4.0;
        if norm(sub(v, mid)) <= r.max(1.0) {
            return v;
        }
    }
    midpoint(p0, p1)
}

fn midpoint(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        0.5 * (a[0] + b[0]),
        0.5 * (a[1] + b[1]),
        0.5 * (a[2] + b[2]),
    ]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

fn triangle_normal(p0: [f64; 3], p1: [f64; 3], p2: [f64; 3]) -> [f64; 3] {
    cross(sub(p1, p0), sub(p2, p0))
}

/// Barycentric parameter on the edge `[p0, p1]` that yields `target`,
/// clamped to `[0, 1]` (so degenerate near-collinear cases never blow up).
fn barycentric_alpha(p0: [f64; 3], p1: [f64; 3], target: [f64; 3]) -> f32 {
    let edge = sub(p1, p0);
    let to = sub(target, p0);
    let denom = dot(edge, edge);
    if denom <= 1e-30 {
        return 0.0;
    }
    let t = (dot(edge, to) / denom).clamp(0.0, 1.0);
    t as f32
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn normalize_or_keep(v: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= 1e-10 {
        fallback
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

/// Solve a 3×3 linear system `A x = b` via Cramer's rule. Returns `None`
/// if the matrix is singular.
fn solve_3x3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = det3(a);
    if det.abs() < 1e-20 {
        return None;
    }
    let inv = 1.0 / det;
    let m0 = [
        [b[0], a[0][1], a[0][2]],
        [b[1], a[1][1], a[1][2]],
        [b[2], a[2][1], a[2][2]],
    ];
    let m1 = [
        [a[0][0], b[0], a[0][2]],
        [a[1][0], b[1], a[1][2]],
        [a[2][0], b[2], a[2][2]],
    ];
    let m2 = [
        [a[0][0], a[0][1], b[0]],
        [a[1][0], a[1][1], b[1]],
        [a[2][0], a[2][1], b[2]],
    ];
    Some([det3(m0) * inv, det3(m1) * inv, det3(m2) * inv])
}

fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Mesh {
        // 24 verts (one per face corner) so we can hold flat normals,
        // 12 triangles. Side length = 1.
        let mut mesh = Mesh::new();
        let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
            (
                [0.0, 0.0, 1.0],
                [
                    [-0.5, -0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, 0.5, 0.5],
                    [-0.5, 0.5, 0.5],
                ],
            ),
            (
                [0.0, 0.0, -1.0],
                [
                    [0.5, -0.5, -0.5],
                    [-0.5, -0.5, -0.5],
                    [-0.5, 0.5, -0.5],
                    [0.5, 0.5, -0.5],
                ],
            ),
            (
                [0.0, 1.0, 0.0],
                [
                    [-0.5, 0.5, 0.5],
                    [0.5, 0.5, 0.5],
                    [0.5, 0.5, -0.5],
                    [-0.5, 0.5, -0.5],
                ],
            ),
            (
                [0.0, -1.0, 0.0],
                [
                    [-0.5, -0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, -0.5, 0.5],
                    [-0.5, -0.5, 0.5],
                ],
            ),
            (
                [1.0, 0.0, 0.0],
                [
                    [0.5, -0.5, 0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, 0.5, -0.5],
                    [0.5, 0.5, 0.5],
                ],
            ),
            (
                [-1.0, 0.0, 0.0],
                [
                    [-0.5, -0.5, -0.5],
                    [-0.5, -0.5, 0.5],
                    [-0.5, 0.5, 0.5],
                    [-0.5, 0.5, -0.5],
                ],
            ),
        ];
        for (n, verts) in faces {
            mesh.push_quad(
                verts[0],
                verts[1],
                verts[2],
                verts[3],
                n,
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            );
        }
        mesh
    }

    /// Build a high-poly disc (one triangle fan) for boundary tests.
    fn disc(segments: u32) -> Mesh {
        let mut mesh = Mesh::new();
        let n = [0.0, 0.0, 1.0];
        mesh.positions.push([0.0, 0.0, 0.0]);
        mesh.normals.push(n);
        mesh.uvs.push([0.5, 0.5]);
        for i in 0..segments {
            let theta = std::f32::consts::TAU * (i as f32) / (segments as f32);
            mesh.positions.push([theta.cos(), theta.sin(), 0.0]);
            mesh.normals.push(n);
            mesh.uvs
                .push([0.5 + 0.5 * theta.cos(), 0.5 + 0.5 * theta.sin()]);
        }
        for i in 0..segments {
            let a = 0;
            let b = 1 + i;
            let c = 1 + ((i + 1) % segments);
            mesh.indices.extend_from_slice(&[a, b, c]);
        }
        mesh
    }

    #[test]
    fn quadric_from_plane_evaluates_to_zero_on_plane() {
        let q = Quadric::from_plane(0.0, 0.0, 1.0, -2.0); // z = 2
        for p in [[1.0, 1.0, 2.0], [-3.0, 5.0, 2.0], [0.0, 0.0, 2.0]] {
            assert!(q.evaluate(p).abs() < 1e-10);
        }
        assert!((q.evaluate([0.0, 0.0, 3.0]) - 1.0).abs() < 1e-10);
        assert!((q.evaluate([0.0, 0.0, 0.0]) - 4.0).abs() < 1e-10);
    }

    #[test]
    fn quadric_add_is_commutative_and_associative() {
        let a = Quadric::from_plane(1.0, 0.0, 0.0, -1.0);
        let b = Quadric::from_plane(0.0, 1.0, 0.0, -2.0);
        let c = Quadric::from_plane(0.0, 0.0, 1.0, -3.0);
        let s1 = a.add(&b).add(&c);
        let s2 = c.add(&b).add(&a);
        for i in 0..10 {
            assert!((s1.m[i] - s2.m[i]).abs() < 1e-10);
        }
    }

    #[test]
    fn empty_mesh_rejected() {
        let mesh = Mesh::new();
        let err = decimate(&mesh, &DecimateOptions::default()).unwrap_err();
        assert!(matches!(err, DecimateError::Empty));
    }

    #[test]
    fn target_larger_than_input_rejected() {
        let mesh = cube();
        let err = decimate(
            &mesh,
            &DecimateOptions {
                target_triangle_count: 100,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, DecimateError::TargetTooLarge { .. }));
    }

    #[test]
    fn target_equal_to_input_returns_clone() {
        let mesh = cube();
        let out = decimate(
            &mesh,
            &DecimateOptions {
                target_triangle_count: 12,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.triangle_count(), 12);
    }

    #[test]
    fn cube_decimates_to_target_face_count() {
        let mesh = cube();
        let input_tri = mesh.indices.len() / 3;
        let out = decimate(
            &mesh,
            &DecimateOptions {
                target_triangle_count: 6,
                preserve_boundary: false,
                ..Default::default()
            },
        )
        .unwrap();
        // Must actually reduce — historically a `<= input` assertion
        // here hid a `shared_count` double-counting bug that caused
        // every interior edge collapse to be rejected.
        assert!(
            out.triangle_count() < input_tri,
            "decimation produced no reduction: input {input_tri}, out {}",
            out.triangle_count()
        );
        // Should hit the requested budget within a small tolerance —
        // a closed cube has 6 quad-pairs that can each collapse cleanly.
        assert!(
            out.triangle_count() <= 8,
            "decimation overshot target: requested 6, got {}",
            out.triangle_count()
        );
    }

    #[test]
    fn output_contains_no_degenerate_triangles() {
        let mesh = disc(32);
        let out = decimate(
            &mesh,
            &DecimateOptions {
                target_triangle_count: 16,
                preserve_boundary: false,
                ..Default::default()
            },
        )
        .unwrap();
        for c in out.indices.chunks_exact(3) {
            assert_ne!(c[0], c[1]);
            assert_ne!(c[1], c[2]);
            assert_ne!(c[0], c[2]);
        }
    }

    #[test]
    fn boundary_preserved_when_flag_set() {
        let mesh = disc(16); // outer ring is a boundary loop
        let outer_perimeter = perimeter(&mesh);
        let out = decimate(
            &mesh,
            &DecimateOptions {
                target_triangle_count: 8,
                preserve_boundary: true,
                ..Default::default()
            },
        )
        .unwrap();
        let new_perimeter = perimeter(&out);
        // Perimeter shrinkage > 5% means a boundary edge collapsed.
        assert!(
            (new_perimeter - outer_perimeter).abs() / outer_perimeter < 0.05,
            "boundary perimeter changed from {outer_perimeter} to {new_perimeter}",
        );
    }

    fn perimeter(mesh: &Mesh) -> f32 {
        // Sum the lengths of edges that appear in exactly one triangle.
        let mut counts: HashMap<(u32, u32), u32> = HashMap::new();
        for c in mesh.indices.chunks_exact(3) {
            for k in 0..3 {
                let a = c[k];
                let b = c[(k + 1) % 3];
                let key = (a.min(b), a.max(b));
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        let mut total = 0.0_f32;
        for ((a, b), n) in counts {
            if n == 1 {
                let p0 = mesh.positions[a as usize];
                let p1 = mesh.positions[b as usize];
                let dx = p1[0] - p0[0];
                let dy = p1[1] - p0[1];
                let dz = p1[2] - p0[2];
                total += (dx * dx + dy * dy + dz * dz).sqrt();
            }
        }
        total
    }

    #[test]
    fn solve_3x3_recovers_known_vector() {
        // x + y = 3, x - y = 1, z = 5 -> (2, 1, 5)
        let m = [[1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 0.0, 1.0]];
        let b = [3.0, 1.0, 5.0];
        let s = solve_3x3(m, b).unwrap();
        assert!((s[0] - 2.0).abs() < 1e-10);
        assert!((s[1] - 1.0).abs() < 1e-10);
        assert!((s[2] - 5.0).abs() < 1e-10);
    }

    #[test]
    fn solve_3x3_returns_none_when_singular() {
        let m = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [4.0, 8.0, 12.0]];
        assert!(solve_3x3(m, [1.0, 2.0, 4.0]).is_none());
    }
}
