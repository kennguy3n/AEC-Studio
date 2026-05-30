//! Floor primitive. The boundary is a closed polygon in plan-view
//! coordinates (mm). Triangulation prefers
//! [`crate::triangulate::triangulate_cdt`] (constrained Delaunay)
//! and falls back to ear-clipping on the rare cases where CDT
//! cannot converge (extremely degenerate input).

use glam::DVec2;
use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::error::{GeometryError, GeometryResult};
use crate::mesh::Mesh;
use crate::triangulate::triangulate_cdt;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Floor {
    pub id: EntityId,
    pub boundary_mm: Vec<[f64; 2]>,
    pub thickness_mm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
    /// World-space Z (mm) of the floor top surface. Defaults to 0 (ground).
    #[serde(default)]
    pub elevation_mm: f64,
}

impl Floor {
    pub fn area_mm2(&self) -> f64 {
        polygon_area_signed(&self.boundary_mm).abs()
    }

    pub fn tessellate(&self) -> GeometryResult<Mesh> {
        if self.boundary_mm.len() < 3 {
            return Err(GeometryError::InvalidPolygon);
        }
        let z_top = self.elevation_mm;
        let z_bot = self.elevation_mm - self.thickness_mm;
        let mut mesh = Mesh::new();
        let triangles = triangulate_floor(&self.boundary_mm)?;
        let normal_up = [0.0, 0.0, 1.0];
        let normal_dn = [0.0, 0.0, -1.0];
        for tri in &triangles {
            let a = [tri[0][0] as f32, tri[0][1] as f32, z_top as f32];
            let b = [tri[1][0] as f32, tri[1][1] as f32, z_top as f32];
            let c = [tri[2][0] as f32, tri[2][1] as f32, z_top as f32];
            mesh.push_triangle(a, b, c, normal_up, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
            let a_d = [tri[0][0] as f32, tri[0][1] as f32, z_bot as f32];
            let b_d = [tri[1][0] as f32, tri[1][1] as f32, z_bot as f32];
            let c_d = [tri[2][0] as f32, tri[2][1] as f32, z_bot as f32];
            // Bottom face wound the other way.
            mesh.push_triangle(
                a_d,
                c_d,
                b_d,
                normal_dn,
                [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0]],
            );
        }
        // Side walls (skirts) connecting top to bottom.
        let n = self.boundary_mm.len();
        for i in 0..n {
            let p0 = self.boundary_mm[i];
            let p1 = self.boundary_mm[(i + 1) % n];
            let edge = DVec2::new(p1[0] - p0[0], p1[1] - p0[1]);
            let len = edge.length().max(f64::EPSILON);
            let nrm = DVec2::new(edge.y / len, -edge.x / len);
            mesh.push_quad(
                [p0[0] as f32, p0[1] as f32, z_bot as f32],
                [p1[0] as f32, p1[1] as f32, z_bot as f32],
                [p1[0] as f32, p1[1] as f32, z_top as f32],
                [p0[0] as f32, p0[1] as f32, z_top as f32],
                [nrm.x as f32, nrm.y as f32, 0.0],
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            );
        }
        Ok(mesh)
    }
}

/// Shoelace formula. Positive ⇒ CCW, negative ⇒ CW.
pub fn polygon_area_signed(points: &[[f64; 2]]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut acc = 0.0;
    let n = points.len();
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        acc += a[0] * b[1] - b[0] * a[1];
    }
    acc * 0.5
}

/// Triangulate a simple floor boundary, preferring CDT (which
/// produces well-shaped triangles and handles concave shapes
/// cleanly) and falling back to ear-clipping if CDT cannot
/// converge.
///
/// Returns a flat list of triangles `[[ax, ay], [bx, by], [cx, cy]]`
/// in CCW winding for top-face consumption.
pub fn triangulate_floor(points: &[[f64; 2]]) -> GeometryResult<Vec<[[f64; 2]; 3]>> {
    match triangulate_cdt(points, &[]) {
        Ok(idx_tris) => {
            let mut out = Vec::with_capacity(idx_tris.len());
            for tri in idx_tris {
                let a = points[tri[0] as usize];
                let b = points[tri[1] as usize];
                let c = points[tri[2] as usize];
                out.push([a, b, c]);
            }
            // Normalise winding to CCW so callers see deterministic
            // output regardless of input boundary direction.
            let mut verts = points.to_vec();
            if polygon_area_signed(&verts) < 0.0 {
                verts.reverse();
            }
            for tri in &mut out {
                if (tri[1][0] - tri[0][0]) * (tri[2][1] - tri[0][1])
                    - (tri[1][1] - tri[0][1]) * (tri[2][0] - tri[0][0])
                    < 0.0
                {
                    tri.swap(1, 2);
                }
            }
            Ok(out)
        }
        Err(_) => triangulate_ear_clip(points),
    }
}

/// Ear-clipping triangulation. Returns a flat list of triangles
/// `[[ax, ay], [bx, by], [cx, cy]]`. The output is always CCW.
///
/// Kept as the fallback path for inputs where the CDT cannot
/// converge (extremely degenerate / self-intersecting / collinear
/// boundaries). Prefer [`triangulate_floor`] for new call sites.
pub fn triangulate_ear_clip(points: &[[f64; 2]]) -> GeometryResult<Vec<[[f64; 2]; 3]>> {
    if points.len() < 3 {
        return Err(GeometryError::InvalidPolygon);
    }
    let mut verts: Vec<[f64; 2]> = points.to_vec();
    // Force CCW orientation.
    if polygon_area_signed(&verts) < 0.0 {
        verts.reverse();
    }
    let mut tris = Vec::new();
    while verts.len() > 3 {
        let n = verts.len();
        let mut found = false;
        for i in 0..n {
            let prev = verts[(i + n - 1) % n];
            let curr = verts[i];
            let next = verts[(i + 1) % n];
            if is_convex(prev, curr, next) && !any_point_inside(&verts, prev, curr, next, i) {
                tris.push([prev, curr, next]);
                verts.remove(i);
                found = true;
                break;
            }
        }
        if !found {
            // Degenerate polygon (self-intersecting). Fall back to fan from v0.
            tris.clear();
            for i in 1..points.len() - 1 {
                tris.push([points[0], points[i], points[i + 1]]);
            }
            return Ok(tris);
        }
    }
    tris.push([verts[0], verts[1], verts[2]]);
    Ok(tris)
}

fn is_convex(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let abx = b[0] - a[0];
    let aby = b[1] - a[1];
    let bcx = c[0] - b[0];
    let bcy = c[1] - b[1];
    (abx * bcy - aby * bcx) > 0.0
}

fn point_in_tri(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let edge0 = (c[0] - a[0], c[1] - a[1]);
    let edge1 = (b[0] - a[0], b[1] - a[1]);
    let to_p = (p[0] - a[0], p[1] - a[1]);
    let dot00 = edge0.0 * edge0.0 + edge0.1 * edge0.1;
    let dot01 = edge0.0 * edge1.0 + edge0.1 * edge1.1;
    let dot02 = edge0.0 * to_p.0 + edge0.1 * to_p.1;
    let dot11 = edge1.0 * edge1.0 + edge1.1 * edge1.1;
    let dot12 = edge1.0 * to_p.0 + edge1.1 * to_p.1;
    let denom = dot00 * dot11 - dot01 * dot01;
    if denom.abs() < 1e-12 {
        return false;
    }
    let inv = 1.0 / denom;
    let bary_u = (dot11 * dot02 - dot01 * dot12) * inv;
    let bary_v = (dot00 * dot12 - dot01 * dot02) * inv;
    bary_u >= 0.0 && bary_v >= 0.0 && (bary_u + bary_v) < 1.0
}

fn any_point_inside(
    verts: &[[f64; 2]],
    a: [f64; 2],
    b: [f64; 2],
    c: [f64; 2],
    skip_i: usize,
) -> bool {
    let n = verts.len();
    let prev = (skip_i + n - 1) % n;
    let next = (skip_i + 1) % n;
    for (i, p) in verts.iter().enumerate() {
        if i == skip_i || i == prev || i == next {
            continue;
        }
        if point_in_tri(*p, a, b, c) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangular_floor_area_matches_w_x_h() {
        let f = Floor {
            id: EntityId::new(),
            boundary_mm: vec![[0.0, 0.0], [4000.0, 0.0], [4000.0, 3000.0], [0.0, 3000.0]],
            thickness_mm: 200.0,
            material_id: None,
            elevation_mm: 0.0,
        };
        let area = f.area_mm2();
        assert!((area - 12_000_000.0).abs() < 1e-6);
    }

    #[test]
    fn floor_tessellates_with_top_bottom_and_sides() {
        let f = Floor {
            id: EntityId::new(),
            boundary_mm: vec![[0.0, 0.0], [4000.0, 0.0], [4000.0, 3000.0], [0.0, 3000.0]],
            thickness_mm: 200.0,
            material_id: None,
            elevation_mm: 0.0,
        };
        let m = f.tessellate().unwrap();
        // 2 quads top + 2 quads bottom = 4 quads from triangulation (2 tris top + 2 tris bottom)
        // Each quad in polygon = 2 tris top + 2 tris bottom = 4 tris.
        // Plus 4 side quads = 8 tris.
        // Total: 4 + 8 = 12 triangles.
        assert_eq!(m.triangle_count(), 12);
    }

    #[test]
    fn signed_area_detects_winding() {
        let ccw = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let cw = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        assert!(polygon_area_signed(&ccw) > 0.0);
        assert!(polygon_area_signed(&cw) < 0.0);
    }
}
