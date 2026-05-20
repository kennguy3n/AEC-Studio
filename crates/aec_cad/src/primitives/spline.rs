//! Spline primitive — non-uniform B-spline with de Boor evaluation.
//!
//! We support degrees 1–5 (degrees ≥ 6 are exotic in CAD) and a uniform
//! knot vector if the caller doesn't supply one. The selection / snap
//! engine treats the spline as a polyline of evaluated samples — fine for
//! pick proximity at typical screen scales.

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spline {
    pub layer: String,
    pub degree: u32,
    pub control_points: Vec<[f64; 2]>,
    pub knots: Vec<f64>,
    #[serde(default)]
    pub closed: bool,
}

impl Spline {
    /// Build a uniform B-spline of the given degree from a control polygon.
    pub fn uniform(layer: impl Into<String>, degree: u32, control_points: Vec<[f64; 2]>) -> Self {
        let n = control_points.len();
        let k = degree as usize;
        let knots = if n > k {
            uniform_knots(n, k)
        } else {
            Vec::new()
        };
        Self {
            layer: layer.into(),
            degree,
            control_points,
            knots,
            closed: false,
        }
    }

    pub fn sample(&self, samples: usize) -> Vec<[f64; 2]> {
        let n = self.control_points.len();
        let k = self.degree as usize;
        if n <= k || samples < 2 {
            return self.control_points.clone();
        }
        let (lo, hi) = if self.knots.len() > n + k {
            (self.knots[k], self.knots[n])
        } else {
            (0.0, 1.0)
        };
        let mut out = Vec::with_capacity(samples);
        for i in 0..samples {
            let t = lo + (hi - lo) * (i as f64 / (samples - 1) as f64);
            out.push(self.evaluate(t));
        }
        out
    }

    fn evaluate(&self, param: f64) -> [f64; 2] {
        let n_cp = self.control_points.len();
        let deg = self.degree as usize;
        if self.knots.len() < n_cp + deg + 1 {
            return self.control_points[0];
        }
        let span = find_span(param, &self.knots, n_cp, deg);
        let basis = basis_funs(param, span, deg, &self.knots);
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        for (jj, basis_val) in basis.iter().enumerate() {
            let cp = self.control_points[span - deg + jj];
            sum_x += basis_val * cp[0];
            sum_y += basis_val * cp[1];
        }
        [sum_x, sum_y]
    }
}

fn uniform_knots(n: usize, k: usize) -> Vec<f64> {
    // Clamped uniform knot vector: k+1 zeros, n-k interior knots,
    // k+1 ones (where m = n+k+1).
    let m = n + k + 1;
    let interior = n.saturating_sub(k);
    let mut knots = vec![0.0_f64; k + 1];
    knots.reserve(m - knots.len());
    for i in 1..interior {
        knots.push(i as f64 / interior as f64);
    }
    knots.resize(m, 1.0);
    knots
}

fn find_span(t: f64, knots: &[f64], n: usize, k: usize) -> usize {
    let high = n; // index of the last basis we can return
    if t >= knots[high] {
        return high - 1;
    }
    if t <= knots[k] {
        return k;
    }
    // Binary search for the first knot > t.
    let mut lo = k;
    let mut hi = high;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if t < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

fn basis_funs(t: f64, span: usize, degree: usize, knots: &[f64]) -> Vec<f64> {
    let mut n = vec![0.0; degree + 1];
    let mut left = vec![0.0; degree + 1];
    let mut right = vec![0.0; degree + 1];
    n[0] = 1.0;
    for j in 1..=degree {
        left[j] = t - knots[span + 1 - j];
        right[j] = knots[span + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let denom = right[r + 1] + left[j - r];
            let temp = if denom.abs() < f64::EPSILON {
                0.0
            } else {
                n[r] / denom
            };
            n[r] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        n[j] = saved;
    }
    n
}

impl Drawable for Spline {
    fn bbox(&self) -> Bbox {
        let mut b = Bbox::empty();
        for &p in &self.control_points {
            b.extend(p);
        }
        b
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Spline {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let samples = self.sample(64);
        let mut best = f64::INFINITY;
        for i in 0..samples.len().saturating_sub(1) {
            let a = samples[i];
            let b = samples[i + 1];
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
            let dx = point[0] - c[0];
            let dy = point[1] - c[1];
            best = best.min(dx * dx + dy * dy);
        }
        best
    }

    fn inside(&self, window: &Bbox) -> bool {
        self.control_points.iter().all(|&p| window.contains(p))
    }
}

impl Snappable for Spline {
    fn snap_points(&self) -> Vec<SnapPoint> {
        let mut pts = Vec::new();
        for &p in &self.control_points {
            pts.push(SnapPoint {
                kind: SnapKind::Node,
                at: p,
            });
        }
        if let Some(&first) = self.control_points.first() {
            pts.push(SnapPoint {
                kind: SnapKind::Endpoint,
                at: first,
            });
        }
        if let Some(&last) = self.control_points.last() {
            pts.push(SnapPoint {
                kind: SnapKind::Endpoint,
                at: last,
            });
        }
        pts
    }
}

impl Transformable for Spline {
    fn transformed(&self, t: &Affine2) -> Self {
        Self {
            layer: self.layer.clone(),
            degree: self.degree,
            control_points: self.control_points.iter().map(|&p| t.apply(p)).collect(),
            knots: self.knots.clone(),
            closed: self.closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_spline_matches_polyline() {
        let s = Spline::uniform("0", 1, vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]);
        let samples = s.sample(5);
        assert_eq!(samples[0], [0.0, 0.0]);
        // Last sample is the last control point for a clamped knot vector.
        let last = samples.last().unwrap();
        assert!((last[0] - 2.0).abs() < 1e-9);
        assert!((last[1] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn cubic_spline_passes_through_endpoints() {
        let s = Spline::uniform("0", 3, vec![[0.0, 0.0], [1.0, 5.0], [2.0, 0.0], [3.0, 5.0]]);
        let samples = s.sample(20);
        // Clamped end is the last control point.
        let last = samples.last().unwrap();
        assert!((last[0] - 3.0).abs() < 1e-6);
        assert!((last[1] - 5.0).abs() < 1e-6);
    }

    #[test]
    fn spline_bbox_includes_control_polygon() {
        let s = Spline::uniform("0", 2, vec![[0.0, 0.0], [5.0, 5.0], [10.0, 0.0]]);
        let b = s.bbox();
        assert_eq!(b.min, [0.0, 0.0]);
        assert_eq!(b.max, [10.0, 5.0]);
    }
}
