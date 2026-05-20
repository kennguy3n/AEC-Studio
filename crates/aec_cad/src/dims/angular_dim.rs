//! Angular dimensions (3-point and 4-point).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AngularDim {
    pub layer: String,
    pub style: String,
    /// Vertex.
    pub center: [f64; 2],
    /// First leg endpoint.
    pub a: [f64; 2],
    /// Second leg endpoint.
    pub b: [f64; 2],
    /// Dim arc placement point.
    pub arc_point: [f64; 2],
    /// Optional override text.
    pub override_text: Option<String>,
}

impl AngularDim {
    /// Returns the measured angle in degrees.
    pub fn measure_deg(&self) -> f64 {
        let v1x = self.a[0] - self.center[0];
        let v1y = self.a[1] - self.center[1];
        let v2x = self.b[0] - self.center[0];
        let v2y = self.b[1] - self.center[1];
        let dot = v1x * v2x + v1y * v2y;
        let cross = v1x * v2y - v1y * v2x;
        cross.atan2(dot).to_degrees().abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_angle_is_ninety() {
        let d = AngularDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            center: [0.0, 0.0],
            a: [10.0, 0.0],
            b: [0.0, 10.0],
            arc_point: [5.0, 5.0],
            override_text: None,
        };
        assert!((d.measure_deg() - 90.0).abs() < 1e-6);
    }

    #[test]
    fn obtuse_angle() {
        let d = AngularDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            center: [0.0, 0.0],
            a: [10.0, 0.0],
            b: [-10.0, 10.0],
            arc_point: [-1.0, 5.0],
            override_text: None,
        };
        assert!((d.measure_deg() - 135.0).abs() < 1e-6);
    }
}
