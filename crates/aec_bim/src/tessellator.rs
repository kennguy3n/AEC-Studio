//! IFC geometry tessellator.
//!
//! Converts IFC's analytic geometry representations into triangle
//! meshes the renderer can consume directly. Phase 9 (Task 17)
//! covers the four geometry kinds that show up in 99%+ of the
//! `*.ifc` files exported by Revit, ArchiCAD, and IfcOpenShell:
//!
//! * [`IfcExtrudedAreaSolid`](ExtrudedAreaSolid): a closed 2D
//!   profile linearly extruded along a direction vector. By far
//!   the most common representation for walls, slabs, and beams.
//! * [`IfcFacetedBrep`](FacetedBrep): a polyhedron defined by
//!   planar faces, each a closed loop of 3D vertices. Used for
//!   curtain wall panels, complex furniture, and any geometry
//!   that's been baked from a parametric source.
//! * Profile types: [`RectangleProfile`], [`CircleProfile`],
//!   [`ArbitraryClosedProfile`]. These feed the extrusion and
//!   are also used in 2D drawings.
//! * Boolean clipping: [`IfcBooleanClippingResult`](BooleanClipping)
//!   subtracts a half-space (`IfcHalfSpaceSolid`) from a solid.
//!   This is how a sloped roof creates the slanted top edge on
//!   the wall under it.
//!
//! All math is in mm (IFC's default `IfcLengthMeasure` unit when
//! `IfcSIUnit(.LENGTHUNIT.,$,.METRE.)` is rescaled by AEC Studio's
//! [`crate::ifc`] reader). The output triangle mesh is right-handed
//! with CCW winding when viewed from the outside.
//!
//! The tessellator is deliberately *not* hooked into the parser yet
//! — the parser preserves unknown geometry entities verbatim
//! through [`PropertyValue::Other`](crate::properties::PropertyValue::Other),
//! and downstream code calls into this module on demand when it
//! needs to draw a face or compute a volume. This separation
//! keeps the parser memory-bounded for headless workflows that
//! don't need geometry.

use std::f64::consts::TAU;

/// Triangle mesh produced by the tessellator. Vertex positions are
/// `[x, y, z]` triples; indices are `[i, j, k]` triples referencing
/// `positions`. Winding is CCW when viewed from outside the solid.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f64; 3]>,
    pub indices: Vec<[u32; 3]>,
}

/// Failure modes the tessellator can report to callers.
///
/// The tessellator never silently produces a partial mesh for a
/// caller that expects a closed solid: any of these variants means
/// the input is malformed (self-intersecting profile, degenerate
/// face, ear-clipping bail-out, etc.) and downstream BIM code
/// should either skip the element or surface a Render Doctor
/// warning instead of emitting geometry with missing triangles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TessellatorError {
    /// Ear clipping bailed out before consuming the input polygon
    /// — typically because the polygon is self-intersecting or has
    /// duplicate vertices. Carries the number of vertices that
    /// remained un-consumed at bail-out, so the caller can decide
    /// whether the partial result is usable.
    PartialTriangulation {
        /// Number of triangles successfully emitted before bail-out.
        triangles_emitted: usize,
        /// Number of polygon vertices still in the active ring when
        /// the algorithm gave up.
        vertices_remaining: usize,
    },
    /// A `BrepFace` had a loop with fewer than 3 vertices, which
    /// cannot form a planar polygon. Includes the face index for
    /// debugging large breps.
    DegenerateFace {
        face_index: usize,
        vertex_count: usize,
    },
}

impl std::fmt::Display for TessellatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PartialTriangulation {
                triangles_emitted,
                vertices_remaining,
            } => write!(
                f,
                "ear clipping bailed out: {triangles_emitted} triangles emitted, \
                 {vertices_remaining} vertices remaining (input likely self-intersecting)"
            ),
            Self::DegenerateFace {
                face_index,
                vertex_count,
            } => write!(
                f,
                "BrepFace #{face_index} has {vertex_count} vertices; need at least 3"
            ),
        }
    }
}

impl std::error::Error for TessellatorError {}

/// Result alias for tessellation operations.
pub type TessellatorResult<T> = Result<T, TessellatorError>;

impl Mesh {
    /// Total surface area of the mesh (sum of triangle areas).
    pub fn surface_area(&self) -> f64 {
        let mut s = 0.0;
        for [a, b, c] in &self.indices {
            let pa = self.positions[*a as usize];
            let pb = self.positions[*b as usize];
            let pc = self.positions[*c as usize];
            let ab = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let ac = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
            let cross = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            s += 0.5 * (cross[0].powi(2) + cross[1].powi(2) + cross[2].powi(2)).sqrt();
        }
        s
    }

    /// Signed volume via the divergence theorem (sum of signed
    /// tetrahedra from the origin). For a closed solid with
    /// outward-facing normals this is positive and equals the
    /// IFC `IfcQuantityVolume`.
    pub fn signed_volume(&self) -> f64 {
        let mut v = 0.0;
        for [a, b, c] in &self.indices {
            let pa = self.positions[*a as usize];
            let pb = self.positions[*b as usize];
            let pc = self.positions[*c as usize];
            v += pa[0] * (pb[1] * pc[2] - pb[2] * pc[1])
                + pa[1] * (pb[2] * pc[0] - pb[0] * pc[2])
                + pa[2] * (pb[0] * pc[1] - pb[1] * pc[0]);
        }
        v / 6.0
    }

    fn push_tri(&mut self, a: u32, b: u32, c: u32) {
        self.indices.push([a, b, c]);
    }

    /// Produce a "welded" copy of this mesh in which positions that
    /// coincide within `epsilon` (mm) collapse to a single vertex.
    ///
    /// This is the right post-process for **non-rendering**
    /// consumers — quantity take-off, watertightness checks,
    /// volume reductions — where vertex count matters but
    /// shading does not. The rendering pipeline must NOT weld a
    /// `FacetedBrep` mesh because adjacent faces typically need
    /// split normals at sharp architectural edges; welding would
    /// smooth-shade across those seams and round off corners.
    ///
    /// `epsilon` is the L∞ (Chebyshev) tolerance per axis. A
    /// typical IFC tolerance is 0.1–1.0 mm.
    ///
    /// **Semantics (greedy, non-transitive)**: the welder walks the
    /// vertex list in input order. Each candidate is collapsed into
    /// the FIRST already-emitted vertex within `epsilon` it can find
    /// (3×3×3 neighbourhood lookup); once collapsed, the candidate's
    /// own position is no longer searchable, so a third vertex C that
    /// is within `epsilon` of B but NOT within `epsilon` of A (B's
    /// cluster representative) will start a fresh cluster instead of
    /// joining `{A, B}`. The guarantee is therefore the weaker
    /// **"every output vertex is within `epsilon` of its cluster
    /// representative"** — NOT the stronger transitive-closure
    /// guarantee "all pairwise-within-`epsilon` inputs share a
    /// cluster". For the use case this welder is designed for — IFC
    /// `FacetedBrep` quantity take-off, where adjacent faces share
    /// **exact** vertex positions (per-face tessellation of a closed
    /// solid) — greedy and union-find produce identical results, and
    /// greedy is `O(n)` while a true union-find merge would be
    /// `O(n α(n))` with materially higher constants. Callers using
    /// `welded()` for fuzzy deduplication with a tolerance larger
    /// than the gap between distinct features (e.g. `epsilon` >> the
    /// minimum inter-vertex distance you want to keep separate)
    /// should be aware of this — bump `epsilon` only as far as the
    /// numerical precision of the producer demands.
    pub fn welded(&self, epsilon: f64) -> Mesh {
        if self.positions.is_empty() {
            return self.clone();
        }
        // Bucket positions into an `epsilon`-sized grid, then for each
        // candidate vertex search the 3x3x3 neighbourhood of its cell
        // for an existing welded vertex within `epsilon` (L∞). Naive
        // single-cell lookup is broken at cell boundaries — two points
        // at `(0.4*eps, 0, 0)` and `(0.6*eps, 0, 0)` are within `eps`
        // L∞ but hash to keys `0` and `1` respectively. Checking the
        // 27-cell neighbourhood guarantees that any pair within
        // `epsilon` ends up in adjacent cells, so the lookup catches
        // them. Remains O(n · 27) = O(n) overall.
        //
        // Important: use `floor` rather than `round` for the bucket
        // key. Rust's `f64::round` rounds half-way cases away from
        // zero, so two points at `-0.5*eps` and `+0.5*eps` (L∞
        // distance exactly `eps` — within the welding tolerance)
        // would hash to keys `-1` and `+1` respectively, a gap of 2
        // along one axis. That falls OUTSIDE the 3x3x3 neighbourhood
        // search and the pair would be silently left un-collapsed.
        // `floor` is monotone non-decreasing with the property
        // `|x - y| <= eps ⇒ |floor(x/eps) - floor(y/eps)| <= 1`,
        // which is the invariant the 27-cell lookup relies on.
        use std::collections::HashMap;
        let inv_eps = if epsilon > 0.0 { 1.0 / epsilon } else { 1.0e9 };
        let key = |p: [f64; 3]| -> (i64, i64, i64) {
            (
                (p[0] * inv_eps).floor() as i64,
                (p[1] * inv_eps).floor() as i64,
                (p[2] * inv_eps).floor() as i64,
            )
        };
        let mut bucket: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let mut new_positions: Vec<[f64; 3]> = Vec::new();
        let mut remap = vec![0u32; self.positions.len()];
        for (old_i, &p) in self.positions.iter().enumerate() {
            let k = key(p);
            // Search 3x3x3 neighbouring cells for an existing welded
            // vertex within `epsilon` (L∞). First match wins, mirroring
            // the union-find semantics: order of input vertices defines
            // which collapsed-cluster representative survives.
            let mut found: Option<u32> = None;
            'search: for dx in -1..=1i64 {
                for dy in -1..=1i64 {
                    for dz in -1..=1i64 {
                        let nk = (k.0 + dx, k.1 + dy, k.2 + dz);
                        if let Some(&existing) = bucket.get(&nk) {
                            let q = new_positions[existing as usize];
                            if (p[0] - q[0]).abs() <= epsilon
                                && (p[1] - q[1]).abs() <= epsilon
                                && (p[2] - q[2]).abs() <= epsilon
                            {
                                found = Some(existing);
                                break 'search;
                            }
                        }
                    }
                }
            }
            let new_i = if let Some(idx) = found {
                idx
            } else {
                let i = new_positions.len() as u32;
                new_positions.push(p);
                bucket.insert(k, i);
                i
            };
            remap[old_i] = new_i;
        }
        let new_indices = self
            .indices
            .iter()
            .map(|&[a, b, c]| [remap[a as usize], remap[b as usize], remap[c as usize]])
            .collect();
        Mesh {
            positions: new_positions,
            indices: new_indices,
        }
    }
}

// ---------------------------------------------------------------------
// 2D profiles
// ---------------------------------------------------------------------

/// A closed 2D polyline in the XY-plane. The polygon is CCW; the
/// last vertex must NOT repeat the first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub points: Vec<[f64; 2]>,
}

impl Profile {
    /// Construct a profile from a list of points. The input is
    /// re-oriented to CCW if necessary so downstream code (the
    /// fan triangulator, the extruder, the boolean op) can assume
    /// one orientation.
    pub fn closed(points: Vec<[f64; 2]>) -> Self {
        let mut p = Self { points };
        if p.signed_area() < 0.0 {
            p.points.reverse();
        }
        p
    }

    /// Signed area via the shoelace formula. Positive when CCW.
    pub fn signed_area(&self) -> f64 {
        let n = self.points.len();
        let mut s = 0.0;
        for i in 0..n {
            let j = (i + 1) % n;
            s += self.points[i][0] * self.points[j][1] - self.points[j][0] * self.points[i][1];
        }
        s * 0.5
    }
}

/// `IfcRectangleProfileDef`: axis-aligned rectangle centred at the
/// origin with width `x_dim` and height `y_dim`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectangleProfile {
    pub x_dim: f64,
    pub y_dim: f64,
}

impl RectangleProfile {
    pub fn evaluate(&self) -> Profile {
        let hx = self.x_dim * 0.5;
        let hy = self.y_dim * 0.5;
        Profile::closed(vec![[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]])
    }
}

/// `IfcCircleProfileDef`: circle centred at the origin with radius
/// `radius`. Discretised into `segments` straight edges (default
/// 32 — matches Revit's "fine" detail level for IFC export).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleProfile {
    pub radius: f64,
    pub segments: u32,
}

impl CircleProfile {
    pub const DEFAULT_SEGMENTS: u32 = 32;

    pub fn evaluate(&self) -> Profile {
        let n = self.segments.max(3) as usize;
        let mut pts = Vec::with_capacity(n);
        for i in 0..n {
            let t = (i as f64) / (n as f64) * TAU;
            pts.push([self.radius * t.cos(), self.radius * t.sin()]);
        }
        Profile::closed(pts)
    }
}

/// `IfcArbitraryClosedProfileDef`: an arbitrary closed polygon in
/// XY. The reader is responsible for filtering out duplicate /
/// nearly-coincident points.
#[derive(Debug, Clone, PartialEq)]
pub struct ArbitraryClosedProfile {
    pub points: Vec<[f64; 2]>,
}

impl ArbitraryClosedProfile {
    pub fn evaluate(&self) -> Profile {
        Profile::closed(self.points.clone())
    }
}

// ---------------------------------------------------------------------
// Profile triangulation (ear clipping for arbitrary polygons,
// fan for convex)
// ---------------------------------------------------------------------

/// Triangulate a (CCW) simple polygon into a flat list of
/// (i0, i1, i2) triangles indexing into the polygon's vertex list.
///
/// Uses ear clipping (`O(n²)`), which handles arbitrary simple
/// polygons including non-convex ones — necessary for IFC
/// arbitrary closed profiles. For a convex polygon ear clipping
/// degenerates to a fan, so we don't special-case convex inputs.
///
/// **Error semantics**: returns
/// [`TessellatorError::PartialTriangulation`] if the ear-search
/// runs out of candidates before consuming the input polygon —
/// this means the input was self-intersecting, had duplicate
/// vertices, or otherwise violated the "simple polygon"
/// precondition. Callers receive the partially-built triangle
/// list inside the error so they can choose to surface a warning
/// to the user (Render Doctor) or fall back to a draft
/// representation, but the default expectation is that the caller
/// propagates the error rather than emitting geometry with
/// missing triangles — returning `Ok(tris)` always means the
/// triangulation is complete (`tris.len() == points.len() - 2`).
#[allow(clippy::many_single_char_names)]
fn triangulate_polygon_2d(points: &[[f64; 2]]) -> TessellatorResult<Vec<[u32; 3]>> {
    let n_total = points.len();
    if n_total < 3 {
        return Ok(Vec::new());
    }
    if n_total == 3 {
        return Ok(vec![[0, 1, 2]]);
    }
    let mut idx: Vec<usize> = (0..n_total).collect();
    let mut out = Vec::with_capacity(n_total - 2);
    let mut guard = 0usize;
    while idx.len() > 3 {
        guard += 1;
        // Safety net for degenerate / self-intersecting input: at
        // most n³ ear-search iterations should suffice; if we go
        // past that, surface the partial result through
        // `TessellatorError::PartialTriangulation` so the caller
        // can detect the bail-out instead of silently emitting a
        // mesh with missing triangles.
        if guard > n_total * n_total * n_total {
            return Err(TessellatorError::PartialTriangulation {
                triangles_emitted: out.len(),
                vertices_remaining: idx.len(),
            });
        }
        let active = idx.len();
        let mut found = false;
        for k in 0..active {
            let i_prev = idx[(k + active - 1) % active];
            let i_curr = idx[k];
            let i_next = idx[(k + 1) % active];
            let a = points[i_prev];
            let b = points[i_curr];
            let c = points[i_next];
            // Convex (CCW): cross product of (b-a)x(c-a) > 0.
            let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if cross <= 0.0 {
                continue;
            }
            // No other ACTIVE polygon vertex may lie inside the
            // triangle (a, b, c). Iterating over the original `points`
            // would also test vertices that have already been clipped
            // off in earlier iterations — once a previously-clipped
            // ear's tip lies inside the current candidate, the
            // candidate would be falsely rejected and the algorithm
            // could fail to find any valid ear, producing a partial
            // triangulation for legitimate concave inputs (very common
            // for IfcArbitraryClosedProfileDef cross-sections).
            // Iterate only over vertices still part of `idx`.
            let mut clean = true;
            for &j in &idx {
                if j == i_prev || j == i_curr || j == i_next {
                    continue;
                }
                if point_in_triangle(points[j], a, b, c) {
                    clean = false;
                    break;
                }
            }
            if clean {
                out.push([i_prev as u32, i_curr as u32, i_next as u32]);
                idx.remove(k);
                found = true;
                break;
            }
        }
        if !found {
            // No ear found — input is malformed. Surface the partial
            // triangulation so the caller can decide what to do.
            return Err(TessellatorError::PartialTriangulation {
                triangles_emitted: out.len(),
                vertices_remaining: idx.len(),
            });
        }
    }
    if idx.len() == 3 {
        out.push([idx[0] as u32, idx[1] as u32, idx[2] as u32]);
    }
    Ok(out)
}

fn point_in_triangle(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let s1 = sign(p, a, b);
    let s2 = sign(p, b, c);
    let s3 = sign(p, c, a);
    let has_neg = s1 < 0.0 || s2 < 0.0 || s3 < 0.0;
    let has_pos = s1 > 0.0 || s2 > 0.0 || s3 > 0.0;
    !(has_neg && has_pos)
}

fn sign(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (p[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (p[1] - b[1])
}

// ---------------------------------------------------------------------
// IfcExtrudedAreaSolid
// ---------------------------------------------------------------------

/// `IfcExtrudedAreaSolid`: a 2D `Profile` swept along a direction
/// vector by `depth`. The base profile sits in the local XY
/// plane; `direction` is a unit (or near-unit) vector AEC Studio
/// re-normalises before extrusion.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtrudedAreaSolid {
    pub profile: Profile,
    pub direction: [f64; 3],
    pub depth: f64,
}

impl ExtrudedAreaSolid {
    /// Tessellate to a closed triangle mesh. Produces:
    ///
    ///   * a bottom cap (the profile triangulated in XY, then
    ///     vertices placed at z = 0)
    ///   * a top cap (the profile triangulated, vertices offset
    ///     by `direction * depth`)
    ///   * a side wall ribbon (two triangles per profile edge)
    ///
    /// Returns [`TessellatorError::PartialTriangulation`] if the
    /// profile is self-intersecting or otherwise causes ear
    /// clipping to bail out before consuming every vertex — in
    /// that case the caller should skip the solid rather than
    /// emit a mesh with missing cap triangles.
    pub fn tessellate(&self) -> TessellatorResult<Mesh> {
        let mut mesh = Mesh::default();
        let n = self.profile.points.len();
        if n < 3 {
            return Ok(mesh);
        }

        // Normalise the extrusion direction.
        let d = self.direction;
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let dir = if len > 0.0 {
            [d[0] / len, d[1] / len, d[2] / len]
        } else {
            [0.0, 0.0, 1.0]
        };
        let dz = [
            dir[0] * self.depth,
            dir[1] * self.depth,
            dir[2] * self.depth,
        ];

        // Bottom vertices (indices 0..n), top vertices (indices n..2n).
        for p in &self.profile.points {
            mesh.positions.push([p[0], p[1], 0.0]);
        }
        for p in &self.profile.points {
            mesh.positions
                .push([p[0] + dz[0], p[1] + dz[1], 0.0 + dz[2]]);
        }

        // Caps. If the profile triangulator fails, propagate the
        // error so the caller knows the resulting solid would be
        // open at top + bottom.
        let tris = triangulate_polygon_2d(&self.profile.points)?;
        for [a, b, c] in &tris {
            // Bottom: invert winding so the normal points -dir.
            mesh.push_tri(*c, *b, *a);
            // Top: keep CCW so the normal points +dir.
            mesh.push_tri(*a + n as u32, *b + n as u32, *c + n as u32);
        }

        // Side ribbon. For edge i -> i+1, emit two triangles:
        //   bottom_i, bottom_{i+1}, top_{i+1}
        //   bottom_i, top_{i+1},    top_i
        for i in 0..n {
            let j = (i + 1) % n;
            let bi = i as u32;
            let bj = j as u32;
            let ti = (i + n) as u32;
            let tj = (j + n) as u32;
            mesh.push_tri(bi, bj, tj);
            mesh.push_tri(bi, tj, ti);
        }
        Ok(mesh)
    }
}

// ---------------------------------------------------------------------
// IfcFacetedBrep
// ---------------------------------------------------------------------

/// A single face of an `IfcFacetedBrep`. The vertex loop is a
/// closed 3D polygon (CCW from outside).
#[derive(Debug, Clone, PartialEq)]
pub struct BrepFace {
    pub loop_: Vec<[f64; 3]>,
}

/// `IfcFacetedBrep`: a polyhedron defined as a soup of planar
/// polygonal faces. The tessellator projects each face into 2D,
/// triangulates with ear clipping, then unprojects.
#[derive(Debug, Clone, PartialEq)]
pub struct FacetedBrep {
    pub faces: Vec<BrepFace>,
}

impl FacetedBrep {
    /// Tessellate every face independently and concatenate the
    /// results into a single triangle mesh.
    ///
    /// **Note on vertex sharing**: adjacent faces that meet at an
    /// edge intentionally produce *duplicate* vertices in the
    /// output. This is the correct representation for architectural
    /// geometry because sharp edges (a wall meeting a slab, a door
    /// frame meeting a wall) need split normals — welding the
    /// vertices would smooth-shade across the seam and visually
    /// round off corners that should appear crisp. A separate
    /// `welded()` post-process is exposed for callers (e.g. quantity
    /// take-off) that want unique vertex counts without changing
    /// the rendering geometry.
    ///
    /// Returns [`TessellatorError::DegenerateFace`] for any
    /// `BrepFace` with fewer than 3 vertices, or
    /// [`TessellatorError::PartialTriangulation`] if any face's
    /// ear-clip pass bails out.
    pub fn tessellate(&self) -> TessellatorResult<Mesh> {
        let mut mesh = Mesh::default();
        for (face_index, face) in self.faces.iter().enumerate() {
            if face.loop_.len() < 3 {
                return Err(TessellatorError::DegenerateFace {
                    face_index,
                    vertex_count: face.loop_.len(),
                });
            }
            let base = mesh.positions.len() as u32;
            // Compute the face normal from the first non-degenerate
            // triangle of the loop (Newell's method would be more
            // robust but adds cost for the 99% case where the loop
            // is planar by construction).
            let (n, u_axis, v_axis) = face_basis(&face.loop_);
            // Project to 2D.
            let pts2d: Vec<[f64; 2]> = face
                .loop_
                .iter()
                .map(|p| {
                    [
                        p[0] * u_axis[0] + p[1] * u_axis[1] + p[2] * u_axis[2],
                        p[0] * v_axis[0] + p[1] * v_axis[1] + p[2] * v_axis[2],
                    ]
                })
                .collect();
            // Ensure CCW projection (matching the face normal).
            let area_2d: f64 = {
                let mut s = 0.0;
                let m = pts2d.len();
                for i in 0..m {
                    let j = (i + 1) % m;
                    s += pts2d[i][0] * pts2d[j][1] - pts2d[j][0] * pts2d[i][1];
                }
                s * 0.5
            };
            let mut local_3d = face.loop_.clone();
            let pts2d = if area_2d < 0.0 {
                local_3d.reverse();
                let mut rev = pts2d;
                rev.reverse();
                rev
            } else {
                pts2d
            };
            for p in &local_3d {
                mesh.positions.push(*p);
            }
            for tri in triangulate_polygon_2d(&pts2d)? {
                mesh.push_tri(base + tri[0], base + tri[1], base + tri[2]);
            }
            // Discard the unused face normal; kept for future
            // smooth-shading work.
            let _ = n;
        }
        Ok(mesh)
    }
}

fn face_basis(loop_: &[[f64; 3]]) -> ([f64; 3], [f64; 3], [f64; 3]) {
    // Find a non-degenerate triangle.
    let n = loop_.len();
    let mut normal = [0.0; 3];
    for i in 0..n {
        let a = loop_[i];
        let b = loop_[(i + 1) % n];
        let c = loop_[(i + 2) % n];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let len = (cross[0].powi(2) + cross[1].powi(2) + cross[2].powi(2)).sqrt();
        if len > 1e-12 {
            normal = [cross[0] / len, cross[1] / len, cross[2] / len];
            break;
        }
    }
    // Build an orthonormal frame whose Z is `normal`.
    let ref_axis = if normal[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let mut u = [
        ref_axis[1] * normal[2] - ref_axis[2] * normal[1],
        ref_axis[2] * normal[0] - ref_axis[0] * normal[2],
        ref_axis[0] * normal[1] - ref_axis[1] * normal[0],
    ];
    let ul = (u[0].powi(2) + u[1].powi(2) + u[2].powi(2))
        .sqrt()
        .max(1e-12);
    u = [u[0] / ul, u[1] / ul, u[2] / ul];
    let v = [
        normal[1] * u[2] - normal[2] * u[1],
        normal[2] * u[0] - normal[0] * u[2],
        normal[0] * u[1] - normal[1] * u[0],
    ];
    (normal, u, v)
}

// ---------------------------------------------------------------------
// IfcBooleanClippingResult (half-space subtraction)
// ---------------------------------------------------------------------

/// `IfcHalfSpaceSolid`: a planar half-space defined by `point`
/// (any point on the plane) and `normal` (the half-space is the
/// side `normal` points away from). Subtracting a half-space from
/// a solid is the workhorse boolean used by IFC to model clipped
/// walls under sloped roofs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HalfSpace {
    pub point: [f64; 3],
    pub normal: [f64; 3],
}

impl HalfSpace {
    /// Returns `true` if `p` is inside the half-space (i.e. NOT
    /// being clipped away). The boundary plane is considered
    /// inside for numerical-stability reasons.
    pub fn contains(&self, p: [f64; 3]) -> bool {
        let dx = p[0] - self.point[0];
        let dy = p[1] - self.point[1];
        let dz = p[2] - self.point[2];
        dx * self.normal[0] + dy * self.normal[1] + dz * self.normal[2] <= 0.0
    }

    /// Linear-interpolate between `a` and `b` to find the point on
    /// this plane.
    pub fn intersect(&self, a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        let da = (a[0] - self.point[0]) * self.normal[0]
            + (a[1] - self.point[1]) * self.normal[1]
            + (a[2] - self.point[2]) * self.normal[2];
        let db = (b[0] - self.point[0]) * self.normal[0]
            + (b[1] - self.point[1]) * self.normal[1]
            + (b[2] - self.point[2]) * self.normal[2];
        let denom = da - db;
        let t = if denom.abs() < 1e-12 { 0.5 } else { da / denom };
        [
            a[0] + t * (b[0] - a[0]),
            a[1] + t * (b[1] - a[1]),
            a[2] + t * (b[2] - a[2]),
        ]
    }
}

/// `IfcBooleanClippingResult`: subtract a half-space from a solid.
/// Applied per-triangle by Sutherland-Hodgman polygon clipping for
/// the body geometry, **plus** a second pass that closes the newly
/// exposed cut face with a cap polygon so the output remains
/// watertight — a precondition for [`Mesh::signed_volume`] to match
/// the actual remaining solid's volume, which BIM quantity take-off
/// (`IfcQuantityVolume` on a clipped wall) relies on.
///
/// Per-triangle clipping alone is open at the cut, which is fine for
/// visual rendering but produces undefined `signed_volume()` results.
/// The cap pass collects the in→out / out→in intersection segment
/// from every partially-clipped triangle, chains the segments into
/// closed loops by directed-graph walk (the intersection of a plane
/// with a closed manifold is one or more closed curves), projects
/// each loop onto a 2D basis on the clip plane, and ear-clip
/// triangulates it with winding consistent with `+clip.normal` as
/// the outward normal of the cap face.
#[derive(Debug, Clone, PartialEq)]
pub struct BooleanClipping {
    pub operand: Mesh,
    pub clip: HalfSpace,
}

impl BooleanClipping {
    /// Tessellate the boolean subtraction. See the struct-level doc
    /// for the algorithm. Returns
    /// [`TessellatorError::PartialTriangulation`] only if a cap loop
    /// fails to triangulate (self-intersecting after projection —
    /// indicates malformed input). Open chains (operand not a closed
    /// manifold) emit body geometry without a cap rather than
    /// erroring, matching the pre-existing best-effort semantics.
    pub fn tessellate(&self) -> TessellatorResult<Mesh> {
        let mut out = Mesh::default();
        // Cache: for each vertex of the input mesh, where does it
        // end up in `out.positions`? Borderline triangles produce
        // new vertices on the clipping plane that don't dedupe
        // against input vertices.
        let mut remap: Vec<Option<u32>> = vec![None; self.operand.positions.len()];
        let push_vertex =
            |out: &mut Mesh, remap: &mut [Option<u32>], src: &[[f64; 3]], idx: u32| -> u32 {
                if let Some(mapped) = remap[idx as usize] {
                    return mapped;
                }
                let new_idx = out.positions.len() as u32;
                out.positions.push(src[idx as usize]);
                remap[idx as usize] = Some(new_idx);
                new_idx
            };
        // Cap-side edges collected from partial clips. Each pair is
        // `(in→out intersection, out→in intersection)` in the original
        // triangle's CCW order; chaining these in `close_cap` recovers
        // the cap polygon(s) on the clip plane.
        let mut cap_edges: Vec<([f64; 3], [f64; 3])> = Vec::new();
        for [a, b, c] in &self.operand.indices {
            let pa = self.operand.positions[*a as usize];
            let pb = self.operand.positions[*b as usize];
            let pc = self.operand.positions[*c as usize];
            let inside = [
                self.clip.contains(pa),
                self.clip.contains(pb),
                self.clip.contains(pc),
            ];
            let in_count = inside.iter().filter(|x| **x).count();
            match in_count {
                0 => continue,
                3 => {
                    let ia = push_vertex(&mut out, &mut remap, &self.operand.positions, *a);
                    let ib = push_vertex(&mut out, &mut remap, &self.operand.positions, *b);
                    let ic = push_vertex(&mut out, &mut remap, &self.operand.positions, *c);
                    out.push_tri(ia, ib, ic);
                }
                _ => {
                    // 1 or 2 vertices inside — produce a clipped
                    // polygon of 3 or 4 vertices.
                    let verts = [
                        (*a, pa, inside[0]),
                        (*b, pb, inside[1]),
                        (*c, pc, inside[2]),
                    ];
                    let mut clipped: Vec<[f64; 3]> = Vec::with_capacity(4);
                    let mut in_to_out: Option<[f64; 3]> = None;
                    let mut out_to_in: Option<[f64; 3]> = None;
                    for k in 0..3 {
                        let (_, p_curr, in_curr) = verts[k];
                        let (_, p_next, in_next) = verts[(k + 1) % 3];
                        if in_curr {
                            clipped.push(p_curr);
                        }
                        if in_curr != in_next {
                            // Edge crosses the plane.
                            let ip = self.clip.intersect(p_curr, p_next);
                            clipped.push(ip);
                            if in_curr && !in_next {
                                in_to_out = Some(ip);
                            } else {
                                out_to_in = Some(ip);
                            }
                        }
                    }
                    // Triangulate the resulting polygon as a fan.
                    if clipped.len() < 3 {
                        continue;
                    }
                    let base = out.positions.len() as u32;
                    for p in &clipped {
                        out.positions.push(*p);
                    }
                    for k in 1..(clipped.len() - 1) {
                        out.push_tri(base, base + k as u32, base + (k + 1) as u32);
                    }
                    if let (Some(from), Some(to)) = (in_to_out, out_to_in) {
                        cap_edges.push((from, to));
                    }
                }
            }
        }
        if !cap_edges.is_empty() {
            self.close_cap(&mut out, &cap_edges)?;
        }
        Ok(out)
    }

    /// Close the cut face by triangulating closed loops formed by the
    /// directed `cap_edges`. Open chains (which mean the operand was
    /// not a closed solid) are silently dropped — visible body
    /// geometry is still correct, just without the cap closure that
    /// watertightness would require.
    fn close_cap(
        &self,
        out: &mut Mesh,
        cap_edges: &[([f64; 3], [f64; 3])],
    ) -> TessellatorResult<()> {
        // 1. Intern cap-edge endpoints into a deduplicated position
        //    list via an `EPS`-bucketed lookup. Two endpoints that
        //    coincide within `EPS` mm collapse to one logical vertex.
        //    The 3×3×3-neighbourhood scan handles the cell-boundary
        //    case (same rationale as `Mesh::welded`).
        const EPS: f64 = 1.0e-6;
        let inv_eps = 1.0 / EPS;
        let key = |p: [f64; 3]| -> (i64, i64, i64) {
            (
                (p[0] * inv_eps).floor() as i64,
                (p[1] * inv_eps).floor() as i64,
                (p[2] * inv_eps).floor() as i64,
            )
        };
        use std::collections::HashMap;
        let mut bucket: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
        let mut positions: Vec<[f64; 3]> = Vec::new();
        let intern = |p: [f64; 3],
                      positions: &mut Vec<[f64; 3]>,
                      bucket: &mut HashMap<(i64, i64, i64), Vec<u32>>|
         -> u32 {
            let k = key(p);
            for dx in -1..=1i64 {
                for dy in -1..=1i64 {
                    for dz in -1..=1i64 {
                        let nk = (k.0 + dx, k.1 + dy, k.2 + dz);
                        if let Some(indices) = bucket.get(&nk) {
                            for &i in indices {
                                let q = positions[i as usize];
                                if (p[0] - q[0]).abs() <= EPS
                                    && (p[1] - q[1]).abs() <= EPS
                                    && (p[2] - q[2]).abs() <= EPS
                                {
                                    return i;
                                }
                            }
                        }
                    }
                }
            }
            let i = positions.len() as u32;
            positions.push(p);
            bucket.entry(k).or_default().push(i);
            i
        };
        let mut indexed_edges: Vec<(u32, u32)> = Vec::with_capacity(cap_edges.len());
        for &(from, to) in cap_edges {
            let i = intern(from, &mut positions, &mut bucket);
            let j = intern(to, &mut positions, &mut bucket);
            if i != j {
                indexed_edges.push((i, j));
            }
        }
        if indexed_edges.is_empty() {
            return Ok(());
        }
        // 2. Walk closed loops over the directed edges.
        let mut by_start: HashMap<u32, Vec<usize>> = HashMap::new();
        for (idx, &(i, _)) in indexed_edges.iter().enumerate() {
            by_start.entry(i).or_default().push(idx);
        }
        let mut visited = vec![false; indexed_edges.len()];
        let mut loops: Vec<Vec<u32>> = Vec::new();
        for start_edge in 0..indexed_edges.len() {
            if visited[start_edge] {
                continue;
            }
            let start_vertex = indexed_edges[start_edge].0;
            let mut cur = start_edge;
            let mut loop_verts: Vec<u32> = Vec::new();
            loop {
                if visited[cur] {
                    // Re-entered a previously-walked edge before
                    // closing — this segment forms an open chain;
                    // discard it.
                    break;
                }
                visited[cur] = true;
                loop_verts.push(indexed_edges[cur].0);
                let next_vertex = indexed_edges[cur].1;
                if next_vertex == start_vertex {
                    loops.push(loop_verts);
                    break;
                }
                let next = by_start
                    .get(&next_vertex)
                    .and_then(|edges| edges.iter().copied().find(|&e| !visited[e]));
                match next {
                    Some(n) => cur = n,
                    None => break, // open chain — operand not closed
                }
            }
        }
        if loops.is_empty() {
            return Ok(());
        }
        // 3. Triangulate each loop on the clip plane. Cap face outward
        //    normal = `+clip.normal`; loops are reoriented to CCW
        //    when viewed from `+clip.normal`.
        let n = cap_normalize(self.clip.normal);
        let (u_axis, v_axis) = plane_basis(n);
        for loop_verts in &loops {
            if loop_verts.len() < 3 {
                continue;
            }
            let pts2d: Vec<[f64; 2]> = loop_verts
                .iter()
                .map(|&i| {
                    let p = positions[i as usize];
                    [
                        p[0] * u_axis[0] + p[1] * u_axis[1] + p[2] * u_axis[2],
                        p[0] * v_axis[0] + p[1] * v_axis[1] + p[2] * v_axis[2],
                    ]
                })
                .collect();
            let area_2d: f64 = {
                let mut s = 0.0;
                let m = pts2d.len();
                for i in 0..m {
                    let j = (i + 1) % m;
                    s += pts2d[i][0] * pts2d[j][1] - pts2d[j][0] * pts2d[i][1];
                }
                s * 0.5
            };
            let (loop_verts_oriented, pts2d_oriented): (Vec<u32>, Vec<[f64; 2]>) = if area_2d < 0.0
            {
                let mut lv = loop_verts.clone();
                lv.reverse();
                let mut pp = pts2d;
                pp.reverse();
                (lv, pp)
            } else {
                (loop_verts.clone(), pts2d)
            };
            let tris = triangulate_polygon_2d(&pts2d_oriented)?;
            let base = out.positions.len() as u32;
            for &v in &loop_verts_oriented {
                out.positions.push(positions[v as usize]);
            }
            for [a, b, c] in tris {
                out.push_tri(base + a, base + b, base + c);
            }
        }
        Ok(())
    }
}

/// Normalise a 3-vector for use as a plane normal. Falls back to
/// `+Z` for the degenerate (near-zero) case so cap closure on a
/// malformed `HalfSpace` still produces *some* basis rather than
/// emitting NaN positions.
fn cap_normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-12 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// Build an orthonormal `(u, v)` basis on the plane perpendicular to
/// `n`. `n` is assumed unit-length. Used for projecting cap-loop 3D
/// points down to a 2D ear-clip input. Matches the convention used
/// by [`face_basis`] for `FacetedBrep` faces.
fn plane_basis(n: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let ref_axis = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let mut u = [
        ref_axis[1] * n[2] - ref_axis[2] * n[1],
        ref_axis[2] * n[0] - ref_axis[0] * n[2],
        ref_axis[0] * n[1] - ref_axis[1] * n[0],
    ];
    let ul = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt().max(1e-12);
    u = [u[0] / ul, u[1] / ul, u[2] / ul];
    let v = [
        n[1] * u[2] - n[2] * u[1],
        n[2] * u[0] - n[0] * u[2],
        n[0] * u[1] - n[1] * u[0],
    ];
    (u, v)
}

// ---------------------------------------------------------------------
// IfcRevolvedAreaSolid
// ---------------------------------------------------------------------

/// `IfcRevolvedAreaSolid`: a 2D `Profile` revolved around a 3D axis
/// through angle `angle_rad` (radians). Used for domes, half-arches,
/// spiralled stair newel posts, columns, finials, and curved
/// balustrades — geometry no `IfcExtrudedAreaSolid` can describe.
///
/// **Axis convention** (matches IFC4):
///
/// * The profile sits in the local XZ-plane (i.e. `profile.points`
///   are `(x_local, z_local)` coordinates, treated here as
///   `(x, 0, z)` in 3D).
/// * The axis is anchored at `axis_origin` and aligned along
///   `axis_direction` in the **local** (profile) frame. The
///   reader re-orients the profile so the axis becomes the
///   Z-axis prior to instantiating this struct, so the
///   tessellator assumes `axis_origin = [0, 0, 0]` and
///   `axis_direction = [0, 0, 1]` for the internal sweep math.
/// * The revolution sweeps CCW around the axis when viewed from
///   `+axis_direction`.
///
/// **Discretisation**: `segments` controls how many angular steps
/// span the full `angle_rad`. A full 2π revolution at 32 segments
/// is the default Revit "fine" detail level. The actual segment
/// count is clamped to `[3, 256]`.
///
/// **Caps**: when `angle_rad < 2π`, the start and end faces of the
/// swept ring are closed with triangulated copies of the original
/// profile so the resulting mesh is watertight. A full revolution
/// (`angle_rad ≈ 2π`) needs no caps — the ribbon meets itself.
#[derive(Debug, Clone, PartialEq)]
pub struct RevolvedAreaSolid {
    pub profile: Profile,
    /// Total revolution angle in radians. Clamped to `(0, 2π]`.
    pub angle_rad: f64,
    /// Number of angular subdivisions. Clamped to `[3, 256]`.
    pub segments: u32,
}

impl RevolvedAreaSolid {
    pub const DEFAULT_SEGMENTS: u32 = 32;

    /// Tessellate to a closed (or capped-open) triangle mesh.
    ///
    /// Returns [`TessellatorError::PartialTriangulation`] if the
    /// profile is self-intersecting and ear clipping bails out
    /// when triangulating the caps (a full revolution has no
    /// caps, so the error is impossible in that case).
    ///
    /// Returns `Ok(empty mesh)` when the profile has fewer than 3
    /// points, when `angle_rad <= 0`, or when both profile X
    /// coordinates straddle zero AND the axis is along Z — that
    /// last case would produce self-intersecting geometry on the
    /// axis crossing and is treated as malformed by the IFC4
    /// implementer agreement.
    #[allow(clippy::many_single_char_names)]
    pub fn tessellate(&self) -> TessellatorResult<Mesh> {
        let mut mesh = Mesh::default();
        let n_profile = self.profile.points.len();
        if n_profile < 3 || self.angle_rad <= 0.0 {
            return Ok(mesh);
        }
        let full_revolution = (self.angle_rad - TAU).abs() < 1e-9 || self.angle_rad >= TAU;
        let angle = self.angle_rad.min(TAU);
        let segs = self.segments.clamp(3, 256) as usize;

        // Profile must lie on one side of the revolution axis (X >= 0
        // when revolving around Z). If a vertex has X < 0 the
        // revolved sweep would self-intersect on the axis crossing —
        // surface an empty mesh per the IFC4 implementer agreement.
        if self.profile.points.iter().any(|p| p[0] < -1e-9) {
            return Ok(mesh);
        }

        // For each of `segs + 1` (or `segs` if full) angular steps,
        // rotate the 2D profile around the Z-axis. Build the
        // positions array layer-by-layer.
        //
        // For a full revolution we emit `segs` cross-sections; the
        // ribbon stitches segment i to segment (i+1) % segs.
        // For a partial revolution we emit `segs + 1` cross-sections;
        // the ribbon stitches i to i+1 with no wrap.
        let cross_section_count = if full_revolution { segs } else { segs + 1 };
        // `den` divides the section index to produce a 0..=1 sweep
        // parameter. Equal in both branches today (segs), kept as a
        // distinct binding so future per-branch adjustments (e.g.
        // half-step offsets) have a single point of edit.
        let den = segs as f64;
        for s in 0..cross_section_count {
            let t = (s as f64) / den;
            let theta = t * angle;
            let (sin_t, cos_t) = theta.sin_cos();
            for p in &self.profile.points {
                // Profile (x_local, z_local) → 3D (x cos θ, x sin θ, z)
                let x = p[0] * cos_t;
                let y = p[0] * sin_t;
                let z = p[1];
                mesh.positions.push([x, y, z]);
            }
        }
        // Side ribbon: edge i↔j in profile, layer s↔(s+1) in sweep.
        for s in 0..segs {
            let s_next = if full_revolution {
                (s + 1) % segs
            } else {
                s + 1
            };
            let base_a = (s * n_profile) as u32;
            let base_b = (s_next * n_profile) as u32;
            for i in 0..n_profile {
                let j = (i + 1) % n_profile;
                let a_i = base_a + i as u32;
                let a_j = base_a + j as u32;
                let b_i = base_b + i as u32;
                let b_j = base_b + j as u32;
                mesh.push_tri(a_i, a_j, b_j);
                mesh.push_tri(a_i, b_j, b_i);
            }
        }
        // Caps for partial revolutions only.
        if !full_revolution {
            let cap_tris = triangulate_polygon_2d(&self.profile.points)?;
            // Start cap (θ = 0): vertices already at indices 0..n_profile,
            // facing the −θ direction → flip winding.
            for [a, b, c] in &cap_tris {
                mesh.push_tri(*c, *b, *a);
            }
            // End cap (θ = angle): vertices at indices
            // `segs*n_profile..(segs+1)*n_profile`, facing +θ → keep CCW.
            let end_base = (segs * n_profile) as u32;
            for [a, b, c] in &cap_tris {
                mesh.push_tri(*a + end_base, *b + end_base, *c + end_base);
            }
        }
        Ok(mesh)
    }
}

// ---------------------------------------------------------------------
// IfcSweptDiskSolid
// ---------------------------------------------------------------------

/// `IfcSweptDiskSolid`: a disk (circular cross-section) swept along
/// a 3D polyline. The workhorse of MEP / structural modelling —
/// every pipe, raceway, rebar, conduit, and railing handrail in
/// IFC is exported as one of these.
///
/// The cross-section is a circle of radius `radius_m` (outer) with
/// optional inner radius `inner_radius_m` (hollow tube — IFC4's
/// `IfcSweptDiskSolid.InnerRadius`). The sweep direction at each
/// vertex is computed as the parallel-transported tangent so the
/// cross-section avoids the rolling-tangent twist that pure-Frenet
/// frames produce at S-curves.
///
/// **Endpoints**: capped with a triangulated disk facing the
/// segment direction. Hollow tubes are capped with an annulus
/// (outer disk minus inner disk).
///
/// **Validity**: requires at least 2 vertices on the path. A path
/// of 2 collinear vertices becomes a straight cylinder; a path of
/// N vertices produces N-1 cylindrical segments stitched into a
/// continuous tube.
#[derive(Debug, Clone, PartialEq)]
pub struct SweptDiskSolid {
    /// 3D polyline the disk is swept along.
    pub path: Vec<[f64; 3]>,
    /// Outer radius of the disk in metres.
    pub radius_m: f64,
    /// Optional inner radius for hollow tubes (pipes). Must be
    /// strictly less than `radius_m` if `Some`.
    pub inner_radius_m: Option<f64>,
    /// Number of circumferential samples around the disk. Clamped
    /// to `[3, 256]`. Default 16 (Revit's "fine" detail).
    pub segments: u32,
}

impl SweptDiskSolid {
    pub const DEFAULT_SEGMENTS: u32 = 16;

    #[allow(clippy::many_single_char_names)]
    pub fn tessellate(&self) -> TessellatorResult<Mesh> {
        let mut mesh = Mesh::default();
        if self.path.len() < 2 || self.radius_m <= 0.0 {
            return Ok(mesh);
        }
        if let Some(r_in) = self.inner_radius_m {
            if r_in <= 0.0 || r_in >= self.radius_m {
                return Ok(mesh);
            }
        }
        let segs = self.segments.clamp(3, 256) as usize;
        let path_len = self.path.len();

        // Parallel-transport frames along the polyline. Avoid the
        // Frenet-frame twist at S-curves by rotating the previous
        // frame's "up" vector into the plane perpendicular to the
        // current tangent (Hanrahan's parallel transport).
        let mut tangents: Vec<[f64; 3]> = Vec::with_capacity(path_len);
        for i in 0..path_len {
            let t = if i == 0 {
                sub3(self.path[1], self.path[0])
            } else if i == path_len - 1 {
                sub3(self.path[i], self.path[i - 1])
            } else {
                // Average of incoming + outgoing edges → tangent at the
                // joint. Smoother than picking one or the other.
                let incoming = normalize3(sub3(self.path[i], self.path[i - 1]));
                let outgoing = normalize3(sub3(self.path[i + 1], self.path[i]));
                [
                    incoming[0] + outgoing[0],
                    incoming[1] + outgoing[1],
                    incoming[2] + outgoing[2],
                ]
            };
            tangents.push(normalize3(t));
        }

        // Initial frame: pick any unit vector NOT parallel to the
        // first tangent.
        let mut up = pick_perpendicular(tangents[0]);
        let mut frames: Vec<([f64; 3], [f64; 3])> = Vec::with_capacity(path_len);
        frames.push((up, cross3(tangents[0], up)));
        for tangent in tangents.iter().skip(1) {
            // Rotate `up` from the previous tangent to the current
            // tangent. Geometrically this projects `up` into the plane
            // perpendicular to the current tangent and renormalises.
            up = project_perpendicular(up, *tangent);
            up = normalize3(up);
            let right = cross3(*tangent, up);
            frames.push((up, right));
        }

        let r_out = self.radius_m;
        let r_in = self.inner_radius_m;

        // Per-segment vertex layout: for each path vertex i, emit a
        // ring of `segs` outer-radius vertices. If hollow, append a
        // second ring of `segs` inner-radius vertices. We index them
        // as: outer[i, k] = (i * segs * 2) + k                if hollow
        //                  (i * segs) + k                     if solid
        //     inner[i, k] = (i * segs * 2) + segs + k         if hollow
        let stride = if r_in.is_some() { segs * 2 } else { segs };
        for (i, &(uvec, vvec)) in frames.iter().enumerate() {
            let center = self.path[i];
            for k in 0..segs {
                let t = (k as f64) / (segs as f64) * TAU;
                let (sin_t, cos_t) = t.sin_cos();
                let dir_u = [uvec[0] * cos_t, uvec[1] * cos_t, uvec[2] * cos_t];
                let dir_v = [vvec[0] * sin_t, vvec[1] * sin_t, vvec[2] * sin_t];
                let outer = [
                    center[0] + r_out * (dir_u[0] + dir_v[0]),
                    center[1] + r_out * (dir_u[1] + dir_v[1]),
                    center[2] + r_out * (dir_u[2] + dir_v[2]),
                ];
                mesh.positions.push(outer);
            }
            if let Some(r_in_v) = r_in {
                for k in 0..segs {
                    let t = (k as f64) / (segs as f64) * TAU;
                    let (sin_t, cos_t) = t.sin_cos();
                    let dir_u = [uvec[0] * cos_t, uvec[1] * cos_t, uvec[2] * cos_t];
                    let dir_v = [vvec[0] * sin_t, vvec[1] * sin_t, vvec[2] * sin_t];
                    let inner = [
                        center[0] + r_in_v * (dir_u[0] + dir_v[0]),
                        center[1] + r_in_v * (dir_u[1] + dir_v[1]),
                        center[2] + r_in_v * (dir_u[2] + dir_v[2]),
                    ];
                    mesh.positions.push(inner);
                }
            }
        }

        // Outer ribbon: for each segment i↔i+1, stitch the `segs`
        // outer-ring vertices into a tube.
        for i in 0..(path_len - 1) {
            let base_a = (i * stride) as u32;
            let base_b = ((i + 1) * stride) as u32;
            for k in 0..segs {
                let k_next = (k + 1) % segs;
                let a_k = base_a + k as u32;
                let a_n = base_a + k_next as u32;
                let b_k = base_b + k as u32;
                let b_n = base_b + k_next as u32;
                mesh.push_tri(a_k, a_n, b_n);
                mesh.push_tri(a_k, b_n, b_k);
            }
        }
        // Inner ribbon (flipped winding so the inside of the tube
        // faces outward toward the tube interior).
        if r_in.is_some() {
            for i in 0..(path_len - 1) {
                let base_a = (i * stride + segs) as u32;
                let base_b = ((i + 1) * stride + segs) as u32;
                for k in 0..segs {
                    let k_next = (k + 1) % segs;
                    let a_k = base_a + k as u32;
                    let a_n = base_a + k_next as u32;
                    let b_k = base_b + k as u32;
                    let b_n = base_b + k_next as u32;
                    mesh.push_tri(a_k, b_n, a_n);
                    mesh.push_tri(a_k, b_k, b_n);
                }
            }
        }
        // End caps: a disk at i=0 facing −tangent, a disk at
        // i=last facing +tangent.
        // Solid case: triangulate the disk as a fan from vertex k=0.
        // Hollow case: triangulate as a quad ribbon between outer
        // and inner rings.
        let last = path_len - 1;
        let base_start = 0u32;
        let base_end = (last * stride) as u32;
        if r_in.is_some() {
            // Hollow start cap: ribbon between outer (offset 0) and
            // inner (offset segs) rings, facing −tangent (flip
            // winding).
            for k in 0..segs {
                let k_next = (k + 1) % segs;
                let outer_a = base_start + k as u32;
                let outer_b = base_start + k_next as u32;
                let inner_a = base_start + (segs + k) as u32;
                let inner_b = base_start + (segs + k_next) as u32;
                mesh.push_tri(outer_a, inner_b, outer_b);
                mesh.push_tri(outer_a, inner_a, inner_b);
            }
            // Hollow end cap: same ribbon, normal winding.
            for k in 0..segs {
                let k_next = (k + 1) % segs;
                let outer_a = base_end + k as u32;
                let outer_b = base_end + k_next as u32;
                let inner_a = base_end + (segs + k) as u32;
                let inner_b = base_end + (segs + k_next) as u32;
                mesh.push_tri(outer_a, outer_b, inner_b);
                mesh.push_tri(outer_a, inner_b, inner_a);
            }
        } else {
            // Solid start cap: fan triangulation from k=0.
            for k in 1..(segs - 1) {
                let v0 = base_start;
                let v1 = base_start + k as u32;
                let v2 = base_start + (k + 1) as u32;
                // Reverse winding so the normal faces −tangent.
                mesh.push_tri(v0, v2, v1);
            }
            // Solid end cap: fan triangulation, normal winding.
            for k in 1..(segs - 1) {
                let v0 = base_end;
                let v1 = base_end + k as u32;
                let v2 = base_end + (k + 1) as u32;
                mesh.push_tri(v0, v1, v2);
            }
        }
        Ok(mesh)
    }
}

/// Subtract `b` from `a` component-wise.
fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Normalise a 3-vector; returns the zero vector if the input is
/// zero-length.
fn normalize3(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-12 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Cross product `a × b`.
fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Pick an arbitrary unit vector perpendicular to `t`. Used to
/// initialise the parallel-transport frame at the start of a
/// [`SweptDiskSolid`] path.
fn pick_perpendicular(t: [f64; 3]) -> [f64; 3] {
    // Choose the axis that's least aligned with `t`, then cross
    // with `t` to get a perpendicular vector. The "axis with the
    // smallest |dot| with `t`" gives the most numerically stable
    // result.
    let abs_x = t[0].abs();
    let abs_y = t[1].abs();
    let abs_z = t[2].abs();
    let helper = if abs_x <= abs_y && abs_x <= abs_z {
        [1.0, 0.0, 0.0]
    } else if abs_y <= abs_z {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    normalize3(cross3(t, helper))
}

/// Project `v` into the plane perpendicular to `n` (subtract the
/// `n`-component of `v`). Used to roll the parallel-transport
/// frame from one segment's tangent to the next without
/// introducing twist.
fn project_perpendicular(v: [f64; 3], n: [f64; 3]) -> [f64; 3] {
    let d = v[0] * n[0] + v[1] * n[1] + v[2] * n[2];
    [v[0] - d * n[0], v[1] - d * n[1], v[2] - d * n[2]]
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 rectangle profile evaluates to a CCW quad of area 1.
    #[test]
    fn rectangle_profile_is_unit_quad() {
        let p = RectangleProfile {
            x_dim: 1.0,
            y_dim: 1.0,
        }
        .evaluate();
        assert_eq!(p.points.len(), 4);
        assert!((p.signed_area() - 1.0).abs() < 1e-12);
    }

    /// A radius-1 circle with 32 segments has area very close to π.
    #[test]
    fn circle_profile_approximates_pi() {
        let p = CircleProfile {
            radius: 1.0,
            segments: 32,
        }
        .evaluate();
        let a = p.signed_area();
        // 32-segment regular polygon: area = (n/2) * r² * sin(2π/n).
        let expected = (32.0 / 2.0) * (std::f64::consts::TAU / 32.0).sin();
        assert!((a - expected).abs() < 1e-9, "got {a}, expected {expected}");
    }

    /// An L-shaped arbitrary profile (non-convex) triangulates
    /// into N-2 = 4 triangles and the total area equals the
    /// analytic area.
    #[test]
    fn arbitrary_l_profile_triangulates() {
        // Vertices of an L, area = 2 * 1 + 1 * 1 = 3.
        let l = ArbitraryClosedProfile {
            points: vec![
                [0.0, 0.0],
                [2.0, 0.0],
                [2.0, 1.0],
                [1.0, 1.0],
                [1.0, 2.0],
                [0.0, 2.0],
            ],
        }
        .evaluate();
        assert!((l.signed_area() - 3.0).abs() < 1e-12);
        let tris = triangulate_polygon_2d(&l.points).expect("L profile triangulates");
        assert_eq!(tris.len(), 4, "n-2 = 4 triangles for hexagonal L");
        // Sum of triangle areas equals polygon area.
        let mut sum = 0.0;
        for [a, b, c] in tris {
            let pa = l.points[a as usize];
            let pb = l.points[b as usize];
            let pc = l.points[c as usize];
            sum +=
                0.5 * ((pb[0] - pa[0]) * (pc[1] - pa[1]) - (pb[1] - pa[1]) * (pc[0] - pa[0])).abs();
        }
        assert!((sum - 3.0).abs() < 1e-12);
    }

    /// Star-shaped (highly concave) polygon — the classic worst case
    /// for ear clipping. Without the "iterate only over active
    /// vertices" fix, previously-clipped tips would falsely reject
    /// otherwise-valid ears and produce a partial triangulation.
    #[test]
    fn highly_concave_star_polygon_triangulates_completely() {
        // 5-pointed star: alternating outer (r=1) and inner (r=0.4)
        // vertices around the unit circle. 10 vertices total, so the
        // correct triangulation has n-2 = 8 triangles.
        let n_points = 10;
        let outer_r = 1.0;
        let inner_r = 0.4;
        let mut pts = Vec::with_capacity(n_points);
        for i in 0..n_points {
            let theta = (i as f64) / (n_points as f64) * std::f64::consts::TAU;
            let r = if i % 2 == 0 { outer_r } else { inner_r };
            pts.push([r * theta.cos(), r * theta.sin()]);
        }
        let prof = ArbitraryClosedProfile { points: pts }.evaluate();
        let polygon_area = prof.signed_area();
        let tris = triangulate_polygon_2d(&prof.points).expect("concave star triangulates");
        // Must produce exactly n-2 triangles (no partial output).
        assert_eq!(
            tris.len(),
            n_points - 2,
            "concave star triangulated incompletely: got {} tris (expected {})",
            tris.len(),
            n_points - 2
        );
        // Sum of triangle areas must equal the polygon area exactly
        // (modulo float epsilon) — proves we didn't bail out early.
        let mut sum = 0.0;
        for [a, b, c] in tris {
            let pa = prof.points[a as usize];
            let pb = prof.points[b as usize];
            let pc = prof.points[c as usize];
            sum +=
                0.5 * ((pb[0] - pa[0]) * (pc[1] - pa[1]) - (pb[1] - pa[1]) * (pc[0] - pa[0])).abs();
        }
        assert!(
            (sum - polygon_area.abs()).abs() < 1e-12,
            "triangulated area {sum} != polygon area {polygon_area}"
        );
    }

    /// A 200×100×3000 mm rectangular wall (extruded along +Z by 3 m)
    /// has the analytic volume 200·100·3000 = 60 000 000 mm³ and
    /// surface area 2·(200·100) + 2·(200·3000) + 2·(100·3000) =
    /// 40 000 + 1 200 000 + 600 000 = 1 840 000 mm².
    #[test]
    fn extruded_rectangle_has_correct_volume_and_area() {
        let solid = ExtrudedAreaSolid {
            profile: RectangleProfile {
                x_dim: 200.0,
                y_dim: 100.0,
            }
            .evaluate(),
            direction: [0.0, 0.0, 1.0],
            depth: 3000.0,
        };
        let mesh = solid.tessellate().expect("rectangle extrudes");
        let v = mesh.signed_volume();
        assert!(
            (v - 60_000_000.0).abs() < 1e-6,
            "extrusion volume mismatch: {v}"
        );
        let area = mesh.surface_area();
        assert!(
            (area - 1_840_000.0).abs() < 1e-6,
            "extrusion area mismatch: {area}"
        );
    }

    /// Extruding along a non-axis-aligned direction still
    /// produces the correct volume (volume is invariant under
    /// rotation of the extrusion axis when the profile is
    /// extruded along its own normal).
    #[test]
    fn extruded_oblique_direction_normalises() {
        // A non-unit direction; the tessellator normalises before
        // applying `depth`.
        let solid = ExtrudedAreaSolid {
            profile: RectangleProfile {
                x_dim: 100.0,
                y_dim: 100.0,
            }
            .evaluate(),
            direction: [0.0, 0.0, 4.0], // non-unit; gets normalised
            depth: 1000.0,
        };
        let mesh = solid.tessellate().expect("oblique extrusion succeeds");
        // V = 100 * 100 * 1000 = 10_000_000.
        assert!((mesh.signed_volume() - 10_000_000.0).abs() < 1e-6);
    }

    /// A unit cube modeled as an IfcFacetedBrep tessellates to a
    /// closed mesh with volume 1 and surface area 6.
    #[test]
    fn faceted_brep_unit_cube_round_trips() {
        let v = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        // CCW from outside: bottom (-Z), top (+Z), four sides.
        let faces = vec![
            BrepFace {
                loop_: vec![v[0], v[3], v[2], v[1]], // bottom, normal -Z
            },
            BrepFace {
                loop_: vec![v[4], v[5], v[6], v[7]], // top, normal +Z
            },
            BrepFace {
                loop_: vec![v[0], v[1], v[5], v[4]], // -Y side
            },
            BrepFace {
                loop_: vec![v[1], v[2], v[6], v[5]], // +X side
            },
            BrepFace {
                loop_: vec![v[2], v[3], v[7], v[6]], // +Y side
            },
            BrepFace {
                loop_: vec![v[3], v[0], v[4], v[7]], // -X side
            },
        ];
        let mesh = FacetedBrep { faces }
            .tessellate()
            .expect("unit cube brep tessellates");
        // The cube has 6 quad faces → 12 triangles.
        assert_eq!(mesh.indices.len(), 12);
        assert!(
            (mesh.signed_volume().abs() - 1.0).abs() < 1e-9,
            "cube volume = 1, got {}",
            mesh.signed_volume()
        );
        assert!(
            (mesh.surface_area() - 6.0).abs() < 1e-9,
            "cube area = 6, got {}",
            mesh.surface_area()
        );
    }

    /// Clipping a 2×2×2 cube with a horizontal half-space at
    /// z = 0 (clip everything above the plane) produces a closed,
    /// watertight mesh of the bottom half — every output vertex
    /// satisfies `z ≤ 0`, and (per the cap-closure pass) the mesh's
    /// signed volume matches the bottom half's actual volume
    /// (`2 × 2 × 1 = 4`). The pre-PR5-wave-7 implementation only
    /// performed per-triangle clipping and produced an open mesh; the
    /// volume invariant locked in here is the regression guard for
    /// the cap closure described in [`BooleanClipping`].
    #[test]
    fn boolean_clipping_produces_watertight_solid_with_correct_volume() {
        // Cube spanning [-1,1]³.
        let v = [
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let cube = FacetedBrep {
            faces: vec![
                BrepFace {
                    loop_: vec![v[0], v[3], v[2], v[1]],
                },
                BrepFace {
                    loop_: vec![v[4], v[5], v[6], v[7]],
                },
                BrepFace {
                    loop_: vec![v[0], v[1], v[5], v[4]],
                },
                BrepFace {
                    loop_: vec![v[1], v[2], v[6], v[5]],
                },
                BrepFace {
                    loop_: vec![v[2], v[3], v[7], v[6]],
                },
                BrepFace {
                    loop_: vec![v[3], v[0], v[4], v[7]],
                },
            ],
        }
        .tessellate()
        .expect("cube brep tessellates");
        let clip = HalfSpace {
            point: [0.0, 0.0, 0.0], // plane z = 0
            normal: [0.0, 0.0, 1.0],
        };
        let clipped = BooleanClipping {
            operand: cube,
            clip,
        }
        .tessellate()
        .expect("clipped cube tessellates");
        // Every output vertex must lie at or below z = 0.
        for p in &clipped.positions {
            assert!(
                p[2] <= 1e-9,
                "clipped vertex above plane: z = {} (full = {:?})",
                p[2],
                p
            );
        }
        // Body geometry survives clipping.
        assert!(!clipped.indices.is_empty());
        // Cap closure: the resulting solid must be watertight. A
        // 2×2×2 cube clipped at z = 0 keeps the bottom half whose
        // volume is 4 (2 × 2 × 1). The divergence-theorem
        // `signed_volume` is positive for CCW-from-outside winding
        // (and would be near zero / wrong for an open mesh).
        let vol = clipped.signed_volume();
        assert!(
            (vol.abs() - 4.0).abs() < 1e-6,
            "watertight half-cube must have |volume| = 4, got {vol}"
        );
    }

    /// Boolean clip that removes everything (plane far below the
    /// cube) produces an empty mesh.
    #[test]
    fn boolean_clipping_empty_when_plane_excludes_all() {
        let v = [
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let cube = FacetedBrep {
            faces: vec![BrepFace {
                loop_: vec![v[0], v[1], v[2], v[3]],
            }],
        }
        .tessellate()
        .expect("single-face brep tessellates");
        let clip = HalfSpace {
            point: [0.0, 0.0, 10.0], // plane z = 10
            // Inside half-space is z >= 10 (normal points -Z;
            // contains returns true when dot ≤ 0).
            normal: [0.0, 0.0, -1.0],
        };
        let out = BooleanClipping {
            operand: cube,
            clip,
        }
        .tessellate()
        .expect("empty boolean tessellates without error");
        assert!(out.indices.is_empty());
    }

    /// `BooleanClipping` must work on solids produced by
    /// `ExtrudedAreaSolid` too (shared vertex indices on edges).
    /// A 1×1×2 box extruded along +Z and clipped at z = 1 should
    /// retain a 1×1×1 cube of volume 1.
    #[test]
    fn boolean_clipping_on_extrusion_preserves_volume() {
        let solid = ExtrudedAreaSolid {
            profile: RectangleProfile {
                x_dim: 1.0,
                y_dim: 1.0,
            }
            .evaluate(),
            direction: [0.0, 0.0, 1.0],
            depth: 2.0,
        };
        let mesh = solid.tessellate().expect("extrusion tessellates");
        // Pre-clip volume = 1 × 1 × 2 = 2.
        assert!((mesh.signed_volume().abs() - 2.0).abs() < 1e-9);
        let clip = HalfSpace {
            point: [0.0, 0.0, 1.0],
            normal: [0.0, 0.0, 1.0],
        };
        let clipped = BooleanClipping {
            operand: mesh,
            clip,
        }
        .tessellate()
        .expect("clipped extrusion tessellates");
        // Post-clip volume = 1 × 1 × 1 = 1 (kept the z ∈ [0, 1]
        // bottom half of the 2-tall box).
        let vol = clipped.signed_volume();
        assert!(
            (vol.abs() - 1.0).abs() < 1e-6,
            "clipped extrusion volume must be 1, got {vol}"
        );
        // No vertex above the clip plane.
        for p in &clipped.positions {
            assert!(p[2] <= 1.0 + 1e-9);
        }
    }

    /// A degenerate polygon where every triangle is collinear
    /// (zero cross product) must surface
    /// `TessellatorError::PartialTriangulation` rather than
    /// silently emitting a partial cap. The old `-> Vec<...>`
    /// signature gave callers no way to distinguish a complete
    /// triangulation from a bail-out; this test pins the new
    /// `Result` contract.
    #[test]
    fn degenerate_collinear_polygon_reports_partial_triangulation() {
        // Four collinear points along y=0. Every candidate "ear"
        // has cross == 0 (degenerate), so the ear-search loop
        // exhausts its options without removing any vertex and
        // returns Err(PartialTriangulation).
        let pts = vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0]];
        let err = triangulate_polygon_2d(&pts).expect_err("collinear must bail");
        match err {
            TessellatorError::PartialTriangulation {
                triangles_emitted,
                vertices_remaining,
            } => {
                assert_eq!(triangles_emitted, 0);
                assert_eq!(vertices_remaining, 4);
            }
            other @ TessellatorError::DegenerateFace { .. } => {
                panic!("unexpected tessellator error variant: {other}")
            }
        }
    }

    /// Confirm that `ExtrudedAreaSolid::tessellate` propagates the
    /// `PartialTriangulation` error from its profile rather than
    /// silently producing an extrusion with missing caps.
    #[test]
    fn extruded_solid_propagates_triangulation_error() {
        let bad = ExtrudedAreaSolid {
            profile: Profile {
                points: vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0]],
            },
            direction: [0.0, 0.0, 1.0],
            depth: 10.0,
        };
        let err = bad.tessellate().expect_err("bad profile must error");
        assert!(matches!(err, TessellatorError::PartialTriangulation { .. }));
    }

    /// `BrepFace` with fewer than three vertices must surface
    /// `TessellatorError::DegenerateFace`, not silently skip the
    /// face. Skipping would let a malformed IFC pass validation
    /// and produce a hole in the rendered solid.
    #[test]
    fn brep_with_degenerate_face_reports_error() {
        let v = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let brep = FacetedBrep {
            faces: vec![BrepFace {
                loop_: vec![v[0], v[1]], // only 2 vertices
            }],
        };
        let err = brep.tessellate().expect_err("must report degenerate face");
        assert!(matches!(
            err,
            TessellatorError::DegenerateFace {
                face_index: 0,
                vertex_count: 2,
            }
        ));
    }

    /// `Mesh::welded` must collapse coincident vertices into a
    /// single canonical index while preserving the triangle list's
    /// topology. Quantity take-off relies on this — a unit cube
    /// emitted as six independent faces has 24 vertices, but the
    /// welded form must have exactly 8.
    #[test]
    fn welded_mesh_dedupes_coincident_brep_vertices() {
        let v = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let brep = FacetedBrep {
            faces: vec![
                BrepFace {
                    loop_: vec![v[0], v[3], v[2], v[1]],
                },
                BrepFace {
                    loop_: vec![v[4], v[5], v[6], v[7]],
                },
                BrepFace {
                    loop_: vec![v[0], v[1], v[5], v[4]],
                },
                BrepFace {
                    loop_: vec![v[1], v[2], v[6], v[5]],
                },
                BrepFace {
                    loop_: vec![v[2], v[3], v[7], v[6]],
                },
                BrepFace {
                    loop_: vec![v[3], v[0], v[4], v[7]],
                },
            ],
        };
        let mesh = brep.tessellate().expect("cube tessellates");
        // 6 faces × 4 vertices = 24 duplicated positions.
        assert_eq!(mesh.positions.len(), 24);
        let welded = mesh.welded(1e-9);
        // Cube has 8 unique vertices.
        assert_eq!(welded.positions.len(), 8);
        // Triangle count is preserved.
        assert_eq!(welded.indices.len(), mesh.indices.len());
        // Volume is preserved under welding (topology unchanged).
        assert!(
            (welded.signed_volume().abs() - mesh.signed_volume().abs()).abs() < 1e-9,
            "welding changed volume: before={}, after={}",
            mesh.signed_volume(),
            welded.signed_volume()
        );
    }

    /// Regression: the previous single-cell-lookup welder dropped
    /// vertex pairs that straddled an integer grid boundary even when
    /// they were within `epsilon` L∞ of each other. The 3x3x3
    /// neighbourhood search added alongside this test correctly
    /// collapses points that hash to adjacent cells. With
    /// `epsilon = 1.0` and `floor()`-based bucketing, a pair like
    /// `(0.9*eps, 0, 0)` and `(1.1*eps, 0, 0)` lands in cells `0`
    /// and `1` respectively — their L∞ distance is `0.2*eps`, well
    /// within the welding tolerance, and the cross-cell lookup must
    /// find them.
    #[test]
    fn welded_mesh_collapses_vertices_across_grid_cell_boundary() {
        let epsilon = 1.0; // 1 mm tolerance
        let mesh = Mesh {
            positions: vec![
                // Cluster A: cross-cell pair under floor() bucketing.
                [0.9, 0.0, 0.0], // floor(0.9) = 0, cell (0, 0, 0)
                [1.1, 0.0, 0.0], // floor(1.1) = 1, cell (1, 0, 0)
                // Cluster B: a cross-cell pair followed by a same-cell
                // straggler, exercising both the 3x3x3 lookup AND the
                // single-cell fast path off the same representative.
                [9.9, 0.0, 0.0],  // floor(9.9)  = 9,  cell (9,  0, 0)
                [10.1, 0.0, 0.0], // floor(10.1) = 10, cell (10, 0, 0)
                [10.4, 0.0, 0.0], // floor(10.4) = 10, cell (10, 0, 0)
            ],
            // Synthetic indices just to exercise the remap.
            indices: vec![[0, 1, 2], [2, 3, 4]],
        };
        let welded = mesh.welded(epsilon);
        // Original: 5 positions. After welding within eps=1.0:
        //   [0.9, 1.1] -> one cluster (L∞ = 0.2 < 1.0, crosses cell 0|1)
        //   [9.9, 10.1, 10.4] -> one cluster
        //       9.9 and 10.1 cross cell 9|10 (L∞ = 0.2 < 1.0);
        //       10.4 stays in cell 10 alongside 10.1 (single-cell merge).
        // Expected: 2 unique positions.
        assert_eq!(
            welded.positions.len(),
            2,
            "expected 2 welded clusters, got {} positions: {:?}",
            welded.positions.len(),
            welded.positions
        );
        // Triangle count preserved; remap collapses the 5 originals to 2.
        assert_eq!(welded.indices.len(), 2);
    }

    /// Regression: the previous `round()`-based bucket key dropped
    /// vertex pairs that landed on opposite sides of an integer
    /// half-boundary even though their L∞ distance was exactly the
    /// welding tolerance. With `epsilon = 1.0` the points
    /// `(-0.5, 0, 0)` and `(+0.5, 0, 0)` are exactly `eps` apart and
    /// MUST collapse, but `round(-0.5) = -1` and `round(+0.5) = +1`
    /// hashed them to keys two apart on the X axis — outside the
    /// 3x3x3 neighbourhood the welder searches. `floor()` guarantees
    /// any pair within `eps` lands in adjacent cells (max key diff
    /// of 1 along each axis), restoring the welding invariant.
    #[test]
    fn welded_mesh_collapses_vertices_straddling_half_integer_boundary() {
        let epsilon = 1.0;
        let mesh = Mesh {
            positions: vec![
                [-0.5, 0.0, 0.0],
                [0.5, 0.0, 0.0],
                // Second cluster, similarly straddling x = 9.5 — both
                // floor and round agree this cluster is within eps,
                // but the test still confirms the second-cluster path.
                [9.0, 0.0, 0.0],
                [9.5, 0.0, 0.0],
            ],
            indices: vec![[0, 1, 2], [1, 2, 3]],
        };
        let welded = mesh.welded(epsilon);
        assert_eq!(
            welded.positions.len(),
            2,
            "expected 2 welded clusters, got {} positions: {:?}",
            welded.positions.len(),
            welded.positions
        );
        assert_eq!(welded.indices.len(), 2);
    }

    // -----------------------------------------------------------------
    // IfcRevolvedAreaSolid
    // -----------------------------------------------------------------

    /// A square profile centred at `x=2, z=0` revolved through 2π
    /// around the Z axis should produce a torus-like ring.
    #[test]
    fn revolved_full_torus_is_watertight() {
        let profile = Profile::closed(vec![[1.0, -0.5], [2.0, -0.5], [2.0, 0.5], [1.0, 0.5]]);
        let solid = RevolvedAreaSolid {
            profile,
            angle_rad: TAU,
            segments: 32,
        };
        let mesh = solid.tessellate().expect("full revolution should not fail");
        // 32 sections × 4 vertices = 128 positions
        assert_eq!(mesh.positions.len(), 32 * 4);
        // Side ribbon only — no caps for full revolution.
        // 32 segments × 4 edges × 2 tris = 256 triangles.
        assert_eq!(mesh.indices.len(), 32 * 4 * 2);
        // Approximate volume via signed_volume (torus V ≈ 2π² R r²).
        let v = mesh.signed_volume().abs();
        assert!(v > 0.0);
    }

    /// A half-revolution (π rad) of the same profile should emit
    /// caps at θ=0 and θ=π so the result is watertight.
    #[test]
    fn revolved_half_revolution_emits_caps() {
        let profile = Profile::closed(vec![[1.0, -0.5], [2.0, -0.5], [2.0, 0.5], [1.0, 0.5]]);
        let solid = RevolvedAreaSolid {
            profile,
            angle_rad: std::f64::consts::PI,
            segments: 16,
        };
        let mesh = solid.tessellate().expect("half revolution");
        // 17 sections (16 + 1) × 4 vertices = 68 positions
        assert_eq!(mesh.positions.len(), 17 * 4);
        // Side: 16 × 4 × 2 = 128. Caps: 2 × (4-2) ear-clipped tris = 4.
        assert_eq!(mesh.indices.len(), 16 * 4 * 2 + 4);
    }

    /// A profile that crosses the axis must produce an empty mesh
    /// (self-intersecting sweep is treated as malformed per the
    /// IFC4 implementer agreement).
    #[test]
    fn revolved_axis_crossing_profile_yields_empty_mesh() {
        let profile = Profile::closed(vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]);
        let solid = RevolvedAreaSolid {
            profile,
            angle_rad: TAU,
            segments: 32,
        };
        let mesh = solid.tessellate().expect("axis crossing");
        assert!(mesh.positions.is_empty());
        assert!(mesh.indices.is_empty());
    }

    /// Segment count below 3 must be clamped up to 3 — the
    /// resulting mesh has ≥ 3 sections worth of positions.
    #[test]
    fn revolved_clamps_segments_to_minimum() {
        let profile = Profile::closed(vec![[1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0]]);
        let solid = RevolvedAreaSolid {
            profile,
            angle_rad: TAU,
            segments: 0,
        };
        let mesh = solid.tessellate().expect("clamped");
        assert_eq!(mesh.positions.len(), 3 * 4);
    }

    // -----------------------------------------------------------------
    // IfcSweptDiskSolid
    // -----------------------------------------------------------------

    /// A straight 2-vertex path with a solid disk swept along it
    /// produces a closed cylinder: outer ribbon + 2 fan caps.
    #[test]
    fn swept_disk_straight_cylinder_is_closed() {
        let solid = SweptDiskSolid {
            path: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            radius_m: 0.5,
            inner_radius_m: None,
            segments: 16,
        };
        let mesh = solid.tessellate().expect("straight cylinder");
        // 2 path vertices × 16 ring samples = 32 positions
        assert_eq!(mesh.positions.len(), 2 * 16);
        // Ribbon: 1 segment × 16 quads × 2 tris = 32 triangles.
        // Caps: 2 × (16 - 2) fan tris = 28 triangles.
        assert_eq!(mesh.indices.len(), 32 + 28);
        // Discretised cylinder volume at 16 segs ≈ 0.77; allow 15 %
        // slack against the analytic π r² h = 0.785.
        let v = mesh.signed_volume().abs();
        assert!(v > 0.6 && v < 0.9, "volume = {v}");
    }

    /// A hollow tube (inner_radius > 0) has 2 rings per section and
    /// ring-shaped caps.
    #[test]
    fn swept_disk_hollow_tube_layout() {
        let solid = SweptDiskSolid {
            path: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 2.0]],
            radius_m: 0.5,
            inner_radius_m: Some(0.4),
            segments: 12,
        };
        let mesh = solid.tessellate().expect("hollow tube");
        // 2 sections × 2 rings × 12 = 48 positions
        assert_eq!(mesh.positions.len(), 2 * 2 * 12);
        // Outer ribbon: 12 × 2 = 24. Inner ribbon: 24. Caps:
        // 12 × 2 = 24 each. 24 + 24 + 24 + 24 = 96 triangles.
        assert_eq!(mesh.indices.len(), 24 + 24 + 24 + 24);
    }

    /// An L-shaped path with 3 vertices produces 2 cylinder
    /// segments sharing the joint cross-section.
    #[test]
    fn swept_disk_polyline_shares_joint_section() {
        let solid = SweptDiskSolid {
            path: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            radius_m: 0.1,
            inner_radius_m: None,
            segments: 8,
        };
        let mesh = solid.tessellate().expect("L-shape");
        // 3 sections × 8 ring samples = 24 positions
        assert_eq!(mesh.positions.len(), 3 * 8);
        // 2 ribbon segments × 8 × 2 = 32 tris. Caps: 2 × (8 - 2) = 12.
        assert_eq!(mesh.indices.len(), 32 + 12);
    }

    /// A degenerate path (single point) returns an empty mesh.
    #[test]
    fn swept_disk_single_point_path_is_empty() {
        let solid = SweptDiskSolid {
            path: vec![[0.0, 0.0, 0.0]],
            radius_m: 0.5,
            inner_radius_m: None,
            segments: 16,
        };
        let mesh = solid.tessellate().expect("single point");
        assert!(mesh.positions.is_empty());
        assert!(mesh.indices.is_empty());
    }

    /// Inner radius ≥ outer radius is malformed; tessellator
    /// returns an empty mesh rather than producing self-intersecting
    /// geometry.
    #[test]
    fn swept_disk_invalid_inner_radius_is_empty() {
        let solid = SweptDiskSolid {
            path: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            radius_m: 0.5,
            inner_radius_m: Some(0.6),
            segments: 16,
        };
        let mesh = solid.tessellate().expect("invalid inner");
        assert!(mesh.positions.is_empty());
        assert!(mesh.indices.is_empty());
    }
}
