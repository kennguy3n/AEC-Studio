//! Stretch tool — move only the vertices of a primitive that fall inside
//! a selection window.

use crate::primitives::{Bbox, Line, Polyline, PolylineVertex, Primitive};

pub struct StretchTool;

fn translate(p: [f64; 2], d: [f64; 2]) -> [f64; 2] {
    [p[0] + d[0], p[1] + d[1]]
}

impl StretchTool {
    pub fn stretch_line(line: &Line, window: &Bbox, delta: [f64; 2]) -> Line {
        Line {
            start: if window.contains(line.start) {
                translate(line.start, delta)
            } else {
                line.start
            },
            end: if window.contains(line.end) {
                translate(line.end, delta)
            } else {
                line.end
            },
            ..line.clone()
        }
    }

    pub fn stretch_polyline(pl: &Polyline, window: &Bbox, delta: [f64; 2]) -> Polyline {
        Polyline {
            vertices: pl
                .vertices
                .iter()
                .map(|v| {
                    if window.contains(v.at) {
                        PolylineVertex {
                            at: translate(v.at, delta),
                            bulge: v.bulge,
                        }
                    } else {
                        v.clone()
                    }
                })
                .collect(),
            ..pl.clone()
        }
    }

    pub fn apply(primitive: &Primitive, window: &Bbox, delta: [f64; 2]) -> Primitive {
        match primitive {
            Primitive::Line(l) => Primitive::Line(Self::stretch_line(l, window, delta)),
            Primitive::Polyline(p) => Primitive::Polyline(Self::stretch_polyline(p, window, delta)),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_only_selected_endpoint() {
        let l = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let win = Bbox {
            min: [-1.0, -1.0],
            max: [5.0, 1.0],
        };
        let r = StretchTool::stretch_line(&l, &win, [0.0, 5.0]);
        assert_eq!(r.start, [0.0, 5.0]); // start was inside the window
        assert_eq!(r.end, [10.0, 0.0]); // end was outside
    }

    #[test]
    fn stretch_polyline_picks_grip_vertices() {
        let pl = Polyline::new(
            "0",
            vec![
                PolylineVertex::new([0.0, 0.0]),
                PolylineVertex::new([5.0, 0.0]),
                PolylineVertex::new([10.0, 0.0]),
            ],
        );
        let win = Bbox {
            min: [4.0, -1.0],
            max: [6.0, 1.0],
        };
        let r = StretchTool::stretch_polyline(&pl, &win, [0.0, 5.0]);
        assert_eq!(r.vertices[0].at, [0.0, 0.0]);
        assert_eq!(r.vertices[1].at, [5.0, 5.0]);
        assert_eq!(r.vertices[2].at, [10.0, 0.0]);
    }
}
