//! Linear dimensions — horizontal, vertical, aligned, rotated.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearDimKind {
    Horizontal,
    Vertical,
    Aligned,
    /// Rotated by `angle_deg` from the X axis.
    Rotated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearDim {
    pub layer: String,
    pub style: String,
    pub kind: LinearDimKind,
    pub a: [f64; 2],
    pub b: [f64; 2],
    /// Dim-line offset point (where the user clicked to place the line).
    pub dim_line: [f64; 2],
    /// Used only for `Rotated`.
    pub angle_deg: f64,
    /// Optional override text (otherwise the computed measurement).
    pub override_text: Option<String>,
}

impl LinearDim {
    /// Measured value (in drawing units).
    pub fn measure(&self) -> f64 {
        match self.kind {
            LinearDimKind::Horizontal => (self.b[0] - self.a[0]).abs(),
            LinearDimKind::Vertical => (self.b[1] - self.a[1]).abs(),
            LinearDimKind::Aligned => {
                let dx = self.b[0] - self.a[0];
                let dy = self.b[1] - self.a[1];
                (dx * dx + dy * dy).sqrt()
            }
            LinearDimKind::Rotated => {
                let theta = self.angle_deg.to_radians();
                let dx = self.b[0] - self.a[0];
                let dy = self.b[1] - self.a[1];
                (dx * theta.cos() + dy * theta.sin()).abs()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_dim_measures_dx() {
        let d = LinearDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: LinearDimKind::Horizontal,
            a: [0.0, 0.0],
            b: [10.0, 5.0],
            dim_line: [5.0, -2.0],
            angle_deg: 0.0,
            override_text: None,
        };
        assert!((d.measure() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn aligned_dim_measures_distance() {
        let d = LinearDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: LinearDimKind::Aligned,
            a: [0.0, 0.0],
            b: [3.0, 4.0],
            dim_line: [0.0, 5.0],
            angle_deg: 0.0,
            override_text: None,
        };
        assert!((d.measure() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn rotated_dim_projects_onto_axis() {
        let d = LinearDim {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: LinearDimKind::Rotated,
            a: [0.0, 0.0],
            b: [10.0, 10.0],
            dim_line: [5.0, 0.0],
            angle_deg: 45.0,
            override_text: None,
        };
        let m = d.measure();
        assert!((m - (200.0_f64).sqrt()).abs() < 1e-9);
    }
}
