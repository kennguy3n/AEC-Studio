//! Baseline dimensions — chained from a common baseline.

use serde::{Deserialize, Serialize};

use crate::dims::linear_dim::{LinearDim, LinearDimKind};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineDimChain {
    pub layer: String,
    pub style: String,
    pub kind: LinearDimKind,
    pub baseline: [f64; 2],
    pub points: Vec<[f64; 2]>,
    pub dim_line_offset: f64,
    pub angle_deg: f64,
}

impl BaselineDimChain {
    /// Expand to individual [`LinearDim`] entries. Each one shares the
    /// baseline as the first point and uses an incrementing y-offset for
    /// the dim line.
    pub fn expand(&self) -> Vec<LinearDim> {
        let mut out = Vec::with_capacity(self.points.len());
        for (i, p) in self.points.iter().enumerate() {
            let offset = self.dim_line_offset * ((i + 1) as f64);
            let dim_line = match self.kind {
                LinearDimKind::Horizontal => {
                    [(self.baseline[0] + p[0]) * 0.5, self.baseline[1] + offset]
                }
                LinearDimKind::Vertical => {
                    [self.baseline[0] + offset, (self.baseline[1] + p[1]) * 0.5]
                }
                _ => [
                    (self.baseline[0] + p[0]) * 0.5,
                    (self.baseline[1] + p[1]) * 0.5 + offset,
                ],
            };
            out.push(LinearDim {
                layer: self.layer.clone(),
                style: self.style.clone(),
                kind: self.kind,
                a: self.baseline,
                b: *p,
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
    fn expand_baseline_chain() {
        let chain = BaselineDimChain {
            layer: "Dim".into(),
            style: "STANDARD".into(),
            kind: LinearDimKind::Horizontal,
            baseline: [0.0, 0.0],
            points: vec![[3.0, 0.0], [6.0, 0.0]],
            dim_line_offset: 1.0,
            angle_deg: 0.0,
        };
        let dims = chain.expand();
        assert_eq!(dims.len(), 2);
        assert_eq!(dims[0].dim_line[1], 1.0);
        assert_eq!(dims[1].dim_line[1], 2.0);
    }
}
