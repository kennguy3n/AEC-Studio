//! Hatch primitive — a closed-boundary loop pattern fill.

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

/// Standard hatch pattern names (subset matching DXF/AutoCAD conventions).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HatchPattern {
    Solid,
    Ansi31,
    Ansi32,
    Ansi33,
    Ansi34,
    Ansi35,
    Ansi36,
    Ansi37,
    Ansi38,
    ArB816,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HatchBoundary {
    pub vertices: Vec<[f64; 2]>,
}

impl HatchBoundary {
    pub fn area_signed(&self) -> f64 {
        // Shoelace.
        let n = self.vertices.len();
        if n < 3 {
            return 0.0;
        }
        let mut s = 0.0;
        for i in 0..n {
            let (x1, y1) = (self.vertices[i][0], self.vertices[i][1]);
            let (x2, y2) = (self.vertices[(i + 1) % n][0], self.vertices[(i + 1) % n][1]);
            s += x1 * y2 - x2 * y1;
        }
        0.5 * s
    }

    pub fn area(&self) -> f64 {
        self.area_signed().abs()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hatch {
    pub layer: String,
    pub boundary_loops: Vec<HatchBoundary>,
    pub pattern: HatchPattern,
    pub scale: f64,
    /// Pattern angle in degrees.
    pub angle: f64,
}

impl Hatch {
    pub fn solid(layer: impl Into<String>, boundary: HatchBoundary) -> Self {
        Self {
            layer: layer.into(),
            boundary_loops: vec![boundary],
            pattern: HatchPattern::Solid,
            scale: 1.0,
            angle: 0.0,
        }
    }

    pub fn total_area(&self) -> f64 {
        // Outer loop area − inner loop areas (islands).
        let mut total = 0.0;
        for (i, b) in self.boundary_loops.iter().enumerate() {
            let signed = b.area_signed().abs();
            if i == 0 {
                total += signed;
            } else {
                total -= signed;
            }
        }
        total.max(0.0)
    }
}

impl Drawable for Hatch {
    fn bbox(&self) -> Bbox {
        let mut b = Bbox::empty();
        for loop_ in &self.boundary_loops {
            for &v in &loop_.vertices {
                b.extend(v);
            }
        }
        b
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Hatch {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let bb = self.bbox();
        if bb.contains(point) {
            // Treat the interior as a hit (zero distance).
            return 0.0;
        }
        // Distance to closest boundary edge.
        let mut best = f64::INFINITY;
        for loop_ in &self.boundary_loops {
            let n = loop_.vertices.len();
            for i in 0..n {
                let a = loop_.vertices[i];
                let b = loop_.vertices[(i + 1) % n];
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                let len2 = dx * dx + dy * dy;
                let c = if len2 < f64::EPSILON {
                    a
                } else {
                    let t = ((point[0] - a[0]) * dx + (point[1] - a[1]) * dy) / len2;
                    let t = t.clamp(0.0, 1.0);
                    [a[0] + t * dx, a[1] + t * dy]
                };
                let dx2 = point[0] - c[0];
                let dy2 = point[1] - c[1];
                best = best.min(dx2 * dx2 + dy2 * dy2);
            }
        }
        best
    }

    fn inside(&self, window: &Bbox) -> bool {
        let bb = self.bbox();
        window.contains(bb.min) && window.contains(bb.max)
    }
}

impl Snappable for Hatch {
    fn snap_points(&self) -> Vec<SnapPoint> {
        let mut pts = Vec::new();
        for loop_ in &self.boundary_loops {
            for &v in &loop_.vertices {
                pts.push(SnapPoint {
                    kind: SnapKind::Node,
                    at: v,
                });
            }
        }
        pts
    }
}

impl Transformable for Hatch {
    fn transformed(&self, t: &Affine2) -> Self {
        Self {
            layer: self.layer.clone(),
            boundary_loops: self
                .boundary_loops
                .iter()
                .map(|b| HatchBoundary {
                    vertices: b.vertices.iter().map(|&v| t.apply(v)).collect(),
                })
                .collect(),
            pattern: self.pattern.clone(),
            scale: self.scale,
            angle: self.angle + t.rotation_deg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f64, h: f64) -> HatchBoundary {
        HatchBoundary {
            vertices: vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]],
        }
    }

    #[test]
    fn solid_hatch_area() {
        let h = Hatch::solid("0", rect(10.0, 5.0));
        assert!((h.total_area() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn hatch_with_island_subtracts_inner() {
        let outer = rect(10.0, 10.0);
        let mut hole = rect(2.0, 2.0);
        for v in &mut hole.vertices {
            v[0] += 4.0;
            v[1] += 4.0;
        }
        let h = Hatch {
            layer: "0".into(),
            boundary_loops: vec![outer, hole],
            pattern: HatchPattern::Ansi31,
            scale: 1.0,
            angle: 0.0,
        };
        assert!((h.total_area() - 96.0).abs() < 1e-9);
    }

    #[test]
    fn hatch_inside_window() {
        let h = Hatch::solid("0", rect(2.0, 2.0));
        let win = Bbox {
            min: [-1.0, -1.0],
            max: [10.0, 10.0],
        };
        assert!(h.inside(&win));
    }
}
