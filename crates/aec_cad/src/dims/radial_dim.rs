//! Radial dimensions — radius and diameter.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadialKind {
    Radius,
    Diameter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadialDim {
    pub layer: String,
    pub style: String,
    pub kind: RadialKind,
    pub center: [f64; 2],
    pub radius: f64,
    /// Point on the circumference where the leader meets the arc.
    pub leader_point: [f64; 2],
    pub override_text: Option<String>,
}

impl RadialDim {
    pub fn measure(&self) -> f64 {
        match self.kind {
            RadialKind::Radius => self.radius,
            RadialKind::Diameter => self.radius * 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_is_radius() {
        let d = RadialDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: RadialKind::Radius,
            center: [0.0, 0.0],
            radius: 5.0,
            leader_point: [5.0, 0.0],
            override_text: None,
        };
        assert_eq!(d.measure(), 5.0);
    }

    #[test]
    fn diameter_is_twice_radius() {
        let d = RadialDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: RadialKind::Diameter,
            center: [0.0, 0.0],
            radius: 5.0,
            leader_point: [5.0, 0.0],
            override_text: None,
        };
        assert_eq!(d.measure(), 10.0);
    }
}
