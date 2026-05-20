//! Continue dimensions — chained end-to-end on a single dim line.

use serde::{Deserialize, Serialize};

use crate::dims::linear_dim::{LinearDim, LinearDimKind};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinueDimChain {
    pub layer: String,
    pub style: String,
    pub kind: LinearDimKind,
    pub points: Vec<[f64; 2]>,
    /// Common dim-line y-coordinate (horizontal) or x-coordinate (vertical).
    pub dim_line_coord: f64,
    pub angle_deg: f64,
}

impl ContinueDimChain {
    pub fn expand(&self) -> Vec<LinearDim> {
        let mut out = Vec::with_capacity(self.points.len().saturating_sub(1));
        for win in self.points.windows(2) {
            let a = win[0];
            let b = win[1];
            let dim_line = match self.kind {
                LinearDimKind::Horizontal => [(a[0] + b[0]) * 0.5, self.dim_line_coord],
                LinearDimKind::Vertical => [self.dim_line_coord, (a[1] + b[1]) * 0.5],
                _ => [
                    (a[0] + b[0]) * 0.5,
                    (a[1] + b[1]) * 0.5 + self.dim_line_coord,
                ],
            };
            out.push(LinearDim {
                layer: self.layer.clone(),
                style: self.style.clone(),
                kind: self.kind,
                a,
                b,
                dim_line,
                angle_deg: self.angle_deg,
                override_text: None,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_continue_chain() {
        let chain = ContinueDimChain {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: LinearDimKind::Horizontal,
            points: vec![[0.0, 0.0], [3.0, 0.0], [8.0, 0.0]],
            dim_line_coord: 1.0,
            angle_deg: 0.0,
        };
        let dims = chain.expand();
        assert_eq!(dims.len(), 2);
        assert!((dims[1].measure() - 5.0).abs() < 1e-9);
    }
}
