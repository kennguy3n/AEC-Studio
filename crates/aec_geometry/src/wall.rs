//! Wall primitive. A wall is a straight segment with a height and a
//! thickness, hosting any number of openings (doors / windows).
//!
//! Mesh tessellation supports openings: each opening becomes a hole in the
//! wall by emitting two side rectangles + a header lintel + a sill (for
//! windows). The implementation walks the wall along the X axis (after
//! re-framing into the wall's local coordinate system) and emits quads for
//! each segment between openings.

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::error::{GeometryError, GeometryResult};
use crate::mesh::Mesh;
use crate::opening::Opening;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wall {
    pub id: EntityId,
    pub start_mm: [f64; 2],
    pub end_mm: [f64; 2],
    pub height_mm: f64,
    pub thickness_mm: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub openings: Vec<Opening>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
}

impl Wall {
    pub fn length_mm(&self) -> f64 {
        let dx = self.end_mm[0] - self.start_mm[0];
        let dy = self.end_mm[1] - self.start_mm[1];
        (dx * dx + dy * dy).sqrt()
    }

    pub fn direction(&self) -> DVec2 {
        let dx = self.end_mm[0] - self.start_mm[0];
        let dy = self.end_mm[1] - self.start_mm[1];
        let len = (dx * dx + dy * dy).sqrt().max(f64::EPSILON);
        DVec2::new(dx / len, dy / len)
    }

    /// Right-hand normal in 2D plan view (turns left of `direction`).
    pub fn normal(&self) -> DVec2 {
        let d = self.direction();
        DVec2::new(-d.y, d.x)
    }

    /// Verify all openings fit within the wall.
    pub fn validate_openings(&self) -> GeometryResult<()> {
        let len = self.length_mm();
        for o in &self.openings {
            let end = o.position_along_wall_mm + o.width_mm;
            if o.position_along_wall_mm < 0.0 || end > len + 1e-6 {
                return Err(GeometryError::OpeningOutOfBounds {
                    opening_id: o.id.to_string(),
                    wall_id: self.id.to_string(),
                    wall_len_mm: len,
                    end_mm: end,
                });
            }
        }
        Ok(())
    }

    /// Sort openings by position along the wall.
    fn sorted_openings(&self) -> Vec<Opening> {
        let mut o = self.openings.clone();
        o.sort_by(|a, b| {
            a.position_along_wall_mm
                .partial_cmp(&b.position_along_wall_mm)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        o
    }

    /// Tessellate the wall into a Mesh. Emits the interior and exterior face
    /// (two parallel rectangles offset by `thickness_mm/2` along the normal),
    /// the top cap, side caps at each opening, and the header lintel above
    /// each opening (and sill below for windows).
    pub fn tessellate(&self) -> GeometryResult<Mesh> {
        self.validate_openings()?;
        let mut mesh = Mesh::new();
        let len = self.length_mm();
        if len < 1e-6 {
            return Err(GeometryError::Degenerate("zero-length wall".into()));
        }
        let dir = self.direction();
        let nrm = self.normal();
        let half_t = self.thickness_mm / 2.0;
        let height = self.height_mm;

        let start = DVec2::new(self.start_mm[0], self.start_mm[1]);
        // Helper to evaluate a wall-local (x_along, z_height) on either face.
        let point = |x: f64, z: f64, side: f64| -> [f32; 3] {
            let p = start + dir * x + nrm * (half_t * side);
            [p.x as f32, p.y as f32, z as f32]
        };

        let emit_panel = |mesh: &mut Mesh, x0: f64, x1: f64, z0: f64, z1: f64| {
            if x1 <= x0 + 1e-9 || z1 <= z0 + 1e-9 {
                return;
            }
            // Exterior face (normal points outward = +nrm direction).
            let n_ext = [nrm.x as f32, nrm.y as f32, 0.0];
            mesh.push_quad(
                point(x0, z0, 1.0),
                point(x1, z0, 1.0),
                point(x1, z1, 1.0),
                point(x0, z1, 1.0),
                n_ext,
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            );
            // Interior face (normal points inward = -nrm direction).
            let n_int = [-nrm.x as f32, -nrm.y as f32, 0.0];
            mesh.push_quad(
                point(x0, z0, -1.0),
                point(x0, z1, -1.0),
                point(x1, z1, -1.0),
                point(x1, z0, -1.0),
                n_int,
                [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
            );
        };

        // Walk segments between openings.
        let openings = self.sorted_openings();
        let mut cursor = 0.0_f64;
        for o in &openings {
            let x0 = cursor;
            let x1 = o.position_along_wall_mm;
            // Solid panel before opening.
            emit_panel(&mut mesh, x0, x1, 0.0, height);
            // Sill (only for windows: panel below opening).
            if o.sill_height_mm > 1e-6 {
                emit_panel(&mut mesh, x1, x1 + o.width_mm, 0.0, o.sill_height_mm);
            }
            // Header (panel above opening).
            let head_top = (o.sill_height_mm + o.height_mm).min(height);
            if head_top < height - 1e-6 {
                emit_panel(&mut mesh, x1, x1 + o.width_mm, head_top, height);
            }
            cursor = x1 + o.width_mm;
        }
        // Remainder after last opening.
        emit_panel(&mut mesh, cursor, len, 0.0, height);

        // Top cap (single quad across full length and thickness, normal +Z).
        let top_z = height;
        mesh.push_quad(
            point(0.0, top_z, -1.0),
            point(len, top_z, -1.0),
            point(len, top_z, 1.0),
            point(0.0, top_z, 1.0),
            [0.0, 0.0, 1.0],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        Ok(mesh)
    }

    /// 2D AABB in plan view (useful for the BVH).
    pub fn aabb_2d(&self) -> ([f64; 2], [f64; 2]) {
        let nrm = self.normal() * (self.thickness_mm / 2.0);
        let pts: [DVec2; 4] = [
            DVec2::new(self.start_mm[0], self.start_mm[1]) + nrm,
            DVec2::new(self.start_mm[0], self.start_mm[1]) - nrm,
            DVec2::new(self.end_mm[0], self.end_mm[1]) + nrm,
            DVec2::new(self.end_mm[0], self.end_mm[1]) - nrm,
        ];
        let mut min = DVec2::splat(f64::INFINITY);
        let mut max = DVec2::splat(f64::NEG_INFINITY);
        for p in &pts {
            min = min.min(*p);
            max = max.max(*p);
        }
        ([min.x, min.y], [max.x, max.y])
    }

    /// World-space center point at base.
    pub fn center(&self) -> DVec3 {
        DVec3::new(
            (self.start_mm[0] + self.end_mm[0]) * 0.5,
            (self.start_mm[1] + self.end_mm[1]) * 0.5,
            0.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opening::Opening;

    fn wall(len_mm: f64) -> Wall {
        Wall {
            id: EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [len_mm, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            openings: vec![],
            material_id: None,
        }
    }

    #[test]
    fn length_and_direction_are_correct() {
        let w = wall(5000.0);
        assert!((w.length_mm() - 5000.0).abs() < 1e-9);
        assert_eq!(w.direction(), DVec2::new(1.0, 0.0));
        assert_eq!(w.normal(), DVec2::new(0.0, 1.0));
    }

    #[test]
    fn no_opening_emits_two_faces_and_a_cap() {
        let m = wall(5000.0).tessellate().unwrap();
        // One panel = 2 quads (interior + exterior) = 4 tris. Cap = 2 tris.
        // Total: 4 + 2 = 6 triangles.
        assert_eq!(m.triangle_count(), 6);
    }

    #[test]
    fn one_door_inserts_a_header_only() {
        let mut w = wall(5000.0);
        w.openings.push(Opening {
            id: EntityId::new(),
            position_along_wall_mm: 1000.0,
            width_mm: 900.0,
            height_mm: 2100.0,
            sill_height_mm: 0.0,
            kind: crate::opening::OpeningKind::Door,
            sub_kind: "single_swing".into(),
        });
        let m = w.tessellate().unwrap();
        // Segments: [0..1000] left panel, [1000..1900] header (above opening),
        // [1900..5000] right panel, + top cap. Each panel = 2 quads (interior +
        // exterior) = 4 tris; cap = 1 quad = 2 tris. Total 3*4 + 2 = 14 tris.
        assert_eq!(m.triangle_count(), 14);
    }

    #[test]
    fn one_window_inserts_sill_and_header() {
        let mut w = wall(5000.0);
        w.openings.push(Opening {
            id: EntityId::new(),
            position_along_wall_mm: 1500.0,
            width_mm: 1200.0,
            height_mm: 1200.0,
            sill_height_mm: 900.0,
            kind: crate::opening::OpeningKind::Window,
            sub_kind: "casement".into(),
        });
        let m = w.tessellate().unwrap();
        // 4 panels (left, sill, header, right) + cap.
        // 4 panels * 4 tris + cap 2 tris = 18 tris.
        assert_eq!(m.triangle_count(), 18);
    }

    #[test]
    fn opening_outside_bounds_returns_error() {
        let mut w = wall(2000.0);
        w.openings.push(Opening {
            id: EntityId::new(),
            position_along_wall_mm: 1500.0,
            width_mm: 1000.0,
            height_mm: 2100.0,
            sill_height_mm: 0.0,
            kind: crate::opening::OpeningKind::Door,
            sub_kind: "single_swing".into(),
        });
        let err = w.tessellate().unwrap_err();
        match err {
            GeometryError::OpeningOutOfBounds { .. } => {}
            other => panic!("expected OpeningOutOfBounds, got {other:?}"),
        }
    }
}
