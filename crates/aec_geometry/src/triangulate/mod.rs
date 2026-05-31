//! Polygon triangulation algorithms used by the parametric geometry
//! crate. Two strategies coexist:
//!
//! 1. **Constrained Delaunay triangulation ([`cdt`])** is the default
//!    triangulator for arbitrary simple polygons, with first-class
//!    support for interior holes (openings in walls, courtyards in
//!    floors). It produces well-shaped triangles (large minimum
//!    angle), which downstream consumers (the path-traced renderer's
//!    BVH, the IFC tessellator, the GPU rasterizer) all prefer.
//!
//! 2. **Ear clipping** (still exported from [`crate::floor`] for
//!    backward compatibility, and re-imported here as
//!    [`ear_clip::triangulate_ear_clip`]) remains as the deterministic
//!    fallback. CDT can in principle fail on extremely degenerate
//!    input (multiple coincident points, collinear hole boundaries
//!    touching the outer ring); we always fall back to ear-clipping
//!    in that case so the geometry pipeline never emits an empty
//!    mesh.
//!
//! All routines work in plan-view 2D coordinates (`f64` millimetres)
//! and return triangle indices into a caller-managed vertex list.

pub mod cdt;

pub use cdt::{triangulate_cdt, triangulate_cdt_with_options, CdtOptions};
