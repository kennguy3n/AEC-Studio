//! Polyline primitive — straight or bulged segments.
//!
//! Bulge is the DXF convention: `tan(angle/4)` of the included arc angle,
//! positive = counter-clockwise. Zero = straight segment. The polyline
//! exposes both raw vertex snaps and per-segment midpoint snaps for the
//! precision engine.

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolylineVertex {
    pub at: [f64; 2],
    /// DXF-style bulge for the segment that *starts* at this vertex.
    #[serde(default)]
    pub bulge: f64,
}

impl PolylineVertex {
    pub fn new(at: [f64; 2]) -> Self {
        Self { at, bulge: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Polyline {
    pub layer: String,
    pub vertices: Vec<PolylineVertex>,
    pub closed: bool,
    #[serde(default)]
    pub elevation: f64,
    #[serde(default)]
    pub color_override: Option<i16>,
    #[serde(default)]
    pub lineweight_override: Option<i16>,
}

impl Polyline {
    pub fn new(layer: impl Into<String>, vertices: Vec<PolylineVertex>) -> Self {
        Self {
            layer: layer.into(),
            vertices,
            closed: false,
            elevation: 0.0,
            color_override: None,
            lineweight_override: None,
        }
    }

    pub fn closed(mut self, closed: bool) -> Self {
        self.closed = closed;
        self
    }

    pub fn segment_count(&self) -> usize {
        if self.vertices.len() < 2 {
            0
        } else if self.closed {
            self.vertices.len()
        } else {
            self.vertices.len() - 1
        }
    }

    pub fn iter_segments(&self) -> impl Iterator<Item = ([f64; 2], [f64; 2], f64)> + '_ {
        let count = self.segment_count();
        let vs = &self.vertices;
        (0..count).map(move |i| {
            let a = vs[i].at;
            let b = vs[(i + 1) % vs.len()].at;
            let bulge = vs[i].bulge;
            (a, b, bulge)
        })
    }

    pub fn length(&self) -> f64 {
        self.iter_segments()
            .map(|(a, b, bulge)| segment_length(a, b, bulge))
            .sum()
    }

    pub fn closest_point(&self, p: [f64; 2]) -> [f64; 2] {
        let mut best = [self.vertices.first().map_or(0.0, |v| v.at[0]), 0.0];
        let mut best_d2 = f64::INFINITY;
        for (a, b, _bulge) in self.iter_segments() {
            let c = closest_on_segment(a, b, p);
            let dx = p[0] - c[0];
            let dy = p[1] - c[1];
            let d2 = dx * dx + dy * dy;
            if d2 < best_d2 {
                best_d2 = d2;
                best = c;
            }
        }
        best
    }
}

fn segment_length(a: [f64; 2], b: [f64; 2], bulge: f64) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let chord = (dx * dx + dy * dy).sqrt();
    if bulge.abs() < f64::EPSILON {
        return chord;
    }
    // Arc length from chord + bulge:
    // sagitta = chord/2 * bulge; included angle = 4 * atan(bulge);
    // radius = chord/(2 sin(angle/2)); arc = radius * angle.
    let included = 4.0 * bulge.atan();
    let radius = chord / (2.0 * (included / 2.0).sin().abs());
    radius * included.abs()
}

fn closest_on_segment(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> [f64; 2] {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len2 = dx * dx + dy * dy;
    if len2 < f64::EPSILON {
        return a;
    }
    let t = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2;
    let t = t.clamp(0.0, 1.0);
    [a[0] + t * dx, a[1] + t * dy]
}

impl Drawable for Polyline {
    fn bbox(&self) -> Bbox {
        let mut b = Bbox::empty();
        for v in &self.vertices {
            b.extend(v.at);
        }
        b
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Polyline {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let mut best = f64::INFINITY;
        for (a, b, _bulge) in self.iter_segments() {
            let c = closest_on_segment(a, b, point);
            let dx = point[0] - c[0];
            let dy = point[1] - c[1];
            best = best.min(dx * dx + dy * dy);
        }
        best
    }

    fn inside(&self, window: &Bbox) -> bool {
        self.vertices.iter().all(|v| window.contains(v.at))
    }
}

impl Snappable for Polyline {
    fn snap_points(&self) -> Vec<SnapPoint> {
        let mut points = Vec::with_capacity(self.vertices.len() * 2);
        for v in &self.vertices {
            points.push(SnapPoint {
                kind: SnapKind::Endpoint,
                at: v.at,
            });
        }
        for (a, b, _bulge) in self.iter_segments() {
            points.push(SnapPoint {
                kind: SnapKind::Midpoint,
                at: [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])],
            });
        }
        points
    }
}

impl Transformable for Polyline {
    fn transformed(&self, t: &Affine2) -> Self {
        Self {
            layer: self.layer.clone(),
            vertices: self
                .vertices
                .iter()
                .map(|v| PolylineVertex {
                    at: t.apply(v.at),
                    bulge: v.bulge,
                })
                .collect(),
            closed: self.closed,
            elevation: self.elevation,
            color_override: self.color_override,
            lineweight_override: self.lineweight_override,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pv(x: f64, y: f64) -> PolylineVertex {
        PolylineVertex::new([x, y])
    }

    #[test]
    fn open_polyline_segments() {
        let pl = Polyline::new("0", vec![pv(0.0, 0.0), pv(1.0, 0.0), pv(1.0, 1.0)]);
        assert_eq!(pl.segment_count(), 2);
        let segs: Vec<_> = pl.iter_segments().collect();
        assert_eq!(segs[0].0, [0.0, 0.0]);
        assert_eq!(segs[0].1, [1.0, 0.0]);
        assert_eq!(segs[1].1, [1.0, 1.0]);
        assert!((pl.length() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn closed_polyline_includes_wraparound() {
        let mut pl = Polyline::new("0", vec![pv(0.0, 0.0), pv(1.0, 0.0), pv(0.0, 1.0)]);
        pl.closed = true;
        assert_eq!(pl.segment_count(), 3);
    }

    #[test]
    fn distance_to_polyline_segment() {
        let pl = Polyline::new("0", vec![pv(0.0, 0.0), pv(10.0, 0.0), pv(10.0, 10.0)]);
        let d2 = pl.distance2([5.0, 1.0]);
        assert!((d2 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn arc_segment_length_via_bulge() {
        // bulge=1 means 180° arc (semi-circle); chord=10 → r=5; arc=π·5.
        let pl = Polyline::new(
            "0",
            vec![
                PolylineVertex {
                    at: [0.0, 0.0],
                    bulge: 1.0,
                },
                pv(10.0, 0.0),
            ],
        );
        assert!((pl.length() - std::f64::consts::PI * 5.0).abs() < 1e-6);
    }
}
