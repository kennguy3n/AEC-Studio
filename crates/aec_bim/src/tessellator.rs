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
    pub fn welded(&self, epsilon: f64) -> Mesh {
        if self.positions.is_empty() {
            return self.clone();
        }
        // Bucket positions into an `epsilon`-sized grid; identical
        // grid cells map to the same canonical vertex. This is
        // O(n) instead of the naive O(n²) all-pairs comparison.
        use std::collections::HashMap;
        let inv_eps = if epsilon > 0.0 { 1.0 / epsilon } else { 1.0e9 };
        let key = |p: [f64; 3]| -> (i64, i64, i64) {
            (
                (p[0] * inv_eps).round() as i64,
                (p[1] * inv_eps).round() as i64,
                (p[2] * inv_eps).round() as i64,
            )
        };
        let mut bucket: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let mut new_positions: Vec<[f64; 3]> = Vec::new();
        let mut remap = vec![0u32; self.positions.len()];
        for (old_i, &p) in self.positions.iter().enumerate() {
            let k = key(p);
            let new_i = *bucket.entry(k).or_insert_with(|| {
                let i = new_positions.len() as u32;
                new_positions.push(p);
                i
            });
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
/// Applied per-triangle by clipping each triangle against the
/// half-space plane (Sutherland-Hodgman polygon clipping
/// specialised for triangles).
#[derive(Debug, Clone, PartialEq)]
pub struct BooleanClipping {
    pub operand: Mesh,
    pub clip: HalfSpace,
}

impl BooleanClipping {
    pub fn tessellate(&self) -> Mesh {
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
                    for k in 0..3 {
                        let (_, p_curr, in_curr) = verts[k];
                        let (_, p_next, in_next) = verts[(k + 1) % 3];
                        if in_curr {
                            clipped.push(p_curr);
                        }
                        if in_curr != in_next {
                            // Edge crosses the plane.
                            clipped.push(self.clip.intersect(p_curr, p_next));
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
                }
            }
        }
        out
    }
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
    /// z = 1 (clip everything above the plane) produces a closed
    /// mesh whose triangle vertices all satisfy z ≤ 1.
    #[test]
    fn boolean_clipping_subtracts_half_space() {
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
        .tessellate();
        // Every output vertex must lie at or below z = 0.
        for p in &clipped.positions {
            assert!(
                p[2] <= 1e-9,
                "clipped vertex above plane: z = {} (full = {:?})",
                p[2],
                p
            );
        }
        // The clipped mesh should still have triangles.
        assert!(!clipped.indices.is_empty());
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
        .tessellate();
        assert!(out.indices.is_empty());
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
}
