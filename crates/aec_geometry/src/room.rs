//! Room primitive: an enclosed space composed of walls, a floor, and an
//! (optional) ceiling.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::floor::polygon_area_signed;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub id: EntityId,
    pub name: String,
    pub wall_ids: Vec<EntityId>,
    pub floor_id: Option<EntityId>,
    pub ceiling_id: Option<EntityId>,
    /// Cached boundary loop (closed polygon, mm). Recomputed from walls when
    /// they change.
    #[serde(default)]
    pub boundary_mm: Vec<[f64; 2]>,
}

impl Room {
    pub fn area_mm2(&self) -> f64 {
        polygon_area_signed(&self.boundary_mm).abs()
    }

    pub fn perimeter_mm(&self) -> f64 {
        let n = self.boundary_mm.len();
        if n < 2 {
            return 0.0;
        }
        let mut acc = 0.0;
        for i in 0..n {
            let a = self.boundary_mm[i];
            let b = self.boundary_mm[(i + 1) % n];
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            acc += (dx * dx + dy * dy).sqrt();
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_room_area_and_perimeter() {
        let r = Room {
            id: EntityId::new(),
            name: "Living".into(),
            wall_ids: vec![],
            floor_id: None,
            ceiling_id: None,
            boundary_mm: vec![[0.0, 0.0], [4000.0, 0.0], [4000.0, 3000.0], [0.0, 3000.0]],
        };
        assert!((r.area_mm2() - 12_000_000.0).abs() < 1e-6);
        assert!((r.perimeter_mm() - 14000.0).abs() < 1e-6);
    }
}
