//! Generate 2D CAD primitives (plan / elevation / section) from BIM
//! elements.
//!
//! Each input element is described by an [`ElementGeometry`] — a list of
//! polygons + extrusion height. The functions in this module project
//! that geometry onto a chosen plane and emit `aec_cad::primitives`
//! lines/polylines, tagged with the BIM `EntityId` they came from so
//! that subsequent BIM edits can find and re-emit the affected
//! geometry.

use serde::{Deserialize, Serialize};

use aec_cad::primitives::{Polyline, PolylineVertex, Primitive};
use aec_core::types::EntityId;

use crate::classification::IfcClass;

/// One BIM element's analytical geometry: a footprint polygon (z=0)
/// extruded vertically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementGeometry {
    pub element: EntityId,
    pub class: IfcClass,
    /// Outer footprint in world XY (one polygon, optionally closed).
    pub footprint: Vec<[f64; 2]>,
    /// Vertical extrusion in mm. `0.0` = no height (slabs, openings).
    pub height: f64,
    /// Z offset of the footprint base (mm).
    pub elevation: f64,
}

impl ElementGeometry {
    pub fn new(
        element: EntityId,
        class: IfcClass,
        footprint: Vec<[f64; 2]>,
        height: f64,
        elevation: f64,
    ) -> Self {
        Self {
            element,
            class,
            footprint,
            height,
            elevation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedPrimitive {
    pub source: EntityId,
    pub primitive: Primitive,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DrawingResult {
    pub primitives: Vec<LinkedPrimitive>,
}

impl DrawingResult {
    pub fn len(&self) -> usize {
        self.primitives.len()
    }

    pub fn is_empty(&self) -> bool {
        self.primitives.is_empty()
    }
}

/// Generate a plan view at `cut_elevation` mm above the project base.
/// Elements that intersect the cut plane render their footprint;
/// elements wholly below it render as background ("ghost") polylines
/// on a separate layer.
pub fn generate_plan(elements: &[ElementGeometry], cut_elevation: f64) -> DrawingResult {
    let mut out = Vec::new();
    for el in elements {
        if el.footprint.len() < 2 {
            continue;
        }
        let bottom = el.elevation;
        let top = el.elevation + el.height;
        let cut = el.height > 0.0 && cut_elevation >= bottom && cut_elevation <= top;
        let below = top < cut_elevation;
        let layer = if cut {
            layer_for_plan(&el.class)
        } else if below {
            "PLAN_BELOW".to_string()
        } else {
            // Above the cut plane — skip.
            continue;
        };
        out.push(LinkedPrimitive {
            source: el.element.clone(),
            primitive: Primitive::Polyline(polyline_from_footprint(&el.footprint, &layer)),
        });
    }
    DrawingResult { primitives: out }
}

/// Generate an elevation drawing projected onto the XZ plane (viewing
/// from +Y). Each element renders as a rectangle outline (footprint
/// width × height) at its absolute elevation.
pub fn generate_elevation(elements: &[ElementGeometry]) -> DrawingResult {
    let mut out = Vec::new();
    for el in elements {
        if el.footprint.is_empty() || el.height <= 0.0 {
            continue;
        }
        let (xmin, xmax) = footprint_x_range(&el.footprint);
        let z0 = el.elevation;
        let z1 = el.elevation + el.height;
        let layer = layer_for_elevation(&el.class);
        let pl = Polyline {
            layer,
            vertices: vec![
                PolylineVertex::new([xmin, z0]),
                PolylineVertex::new([xmax, z0]),
                PolylineVertex::new([xmax, z1]),
                PolylineVertex::new([xmin, z1]),
            ],
            closed: true,
            elevation: 0.0,
            color_override: None,
            lineweight_override: None,
        };
        out.push(LinkedPrimitive {
            source: el.element.clone(),
            primitive: Primitive::Polyline(pl),
        });
    }
    DrawingResult { primitives: out }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionPlane {
    pub origin: [f64; 2],
    /// Unit-length 2D direction (in plan XY). Section cut line passes
    /// through `origin` along this direction.
    pub direction: [f64; 2],
}

/// Generate a section view at the given plane. Elements whose
/// footprint crosses the plane render their cut polygon (currently a
/// rectangle from the intersection interval to the height extent).
pub fn generate_section(elements: &[ElementGeometry], plane: &SectionPlane) -> DrawingResult {
    let mut out = Vec::new();
    let nx = plane.direction[0];
    let ny = plane.direction[1];
    let len = (nx * nx + ny * ny).sqrt();
    if len < 1e-9 {
        return DrawingResult::default();
    }
    let nx = nx / len;
    let ny = ny / len;
    // Perpendicular normal of the section line.
    let perp = [-ny, nx];
    for el in elements {
        let projs: Vec<f64> = el
            .footprint
            .iter()
            .map(|p| (p[0] - plane.origin[0]) * perp[0] + (p[1] - plane.origin[1]) * perp[1])
            .collect();
        let min_p = projs.iter().copied().fold(f64::INFINITY, f64::min);
        let max_p = projs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if min_p * max_p > 0.0 {
            // Footprint sits entirely on one side of the plane.
            continue;
        }
        // Find the two intersection points of the footprint with the
        // section line.
        let intersections = section_intersections(&el.footprint, plane.origin, perp);
        if intersections.len() < 2 {
            continue;
        }
        let a = intersections[0];
        let b = intersections[intersections.len() - 1];
        let z0 = el.elevation;
        let z1 = el.elevation + el.height.max(1.0); // ensure non-zero rect
                                                    // Coordinates along the section line for the cut endpoints.
        let along = |p: [f64; 2]| (p[0] - plane.origin[0]) * nx + (p[1] - plane.origin[1]) * ny;
        let s0 = along(a);
        let s1 = along(b);
        let pl = Polyline {
            layer: layer_for_section(&el.class),
            vertices: vec![
                PolylineVertex::new([s0, z0]),
                PolylineVertex::new([s1, z0]),
                PolylineVertex::new([s1, z1]),
                PolylineVertex::new([s0, z1]),
            ],
            closed: true,
            elevation: 0.0,
            color_override: None,
            lineweight_override: None,
        };
        out.push(LinkedPrimitive {
            source: el.element.clone(),
            primitive: Primitive::Polyline(pl),
        });
    }
    DrawingResult { primitives: out }
}

fn polyline_from_footprint(footprint: &[[f64; 2]], layer: &str) -> Polyline {
    // Closed-polygon fixtures often duplicate the first vertex at the end of
    // the loop (GeoJSON / IFC convention). `Polyline::iter_segments` for a
    // closed polyline already wraps the last segment from `v[n-1]` to `v[0]`,
    // so we strip the trailing duplicate to avoid a zero-length closing
    // segment that would otherwise produce rendering artifacts.
    let closed = footprint.first() == footprint.last() && footprint.len() > 2;
    let trim_to = if closed {
        footprint.len() - 1
    } else {
        footprint.len()
    };
    let vertices = footprint[..trim_to]
        .iter()
        .copied()
        .map(PolylineVertex::new)
        .collect();
    Polyline {
        layer: layer.to_string(),
        vertices,
        closed,
        elevation: 0.0,
        color_override: None,
        lineweight_override: None,
    }
}

fn footprint_x_range(footprint: &[[f64; 2]]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for p in footprint {
        if p[0] < min {
            min = p[0];
        }
        if p[0] > max {
            max = p[0];
        }
    }
    (min, max)
}

fn section_intersections(
    footprint: &[[f64; 2]],
    origin: [f64; 2],
    perp: [f64; 2],
) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    if footprint.len() < 2 {
        return out;
    }
    for window in footprint.windows(2) {
        let p0 = window[0];
        let p1 = window[1];
        let d0 = (p0[0] - origin[0]) * perp[0] + (p0[1] - origin[1]) * perp[1];
        let d1 = (p1[0] - origin[0]) * perp[0] + (p1[1] - origin[1]) * perp[1];
        if d0 == 0.0 {
            out.push(p0);
        }
        if d0 * d1 < 0.0 {
            let t = d0 / (d0 - d1);
            out.push([p0[0] + t * (p1[0] - p0[0]), p0[1] + t * (p1[1] - p0[1])]);
        }
    }
    out
}

fn layer_for_plan(class: &IfcClass) -> String {
    match class {
        IfcClass::IfcWall | IfcClass::IfcWallStandardCase => "A-WALL".into(),
        IfcClass::IfcDoor => "A-DOOR".into(),
        IfcClass::IfcWindow => "A-GLAZ".into(),
        IfcClass::IfcSlab => "A-FLOR".into(),
        IfcClass::IfcColumn => "A-COLS".into(),
        IfcClass::IfcSpace => "A-AREA".into(),
        _ => "A-OTHR".into(),
    }
}

fn layer_for_elevation(class: &IfcClass) -> String {
    let base = layer_for_plan(class);
    format!("{}-ELEV", base)
}

fn layer_for_section(class: &IfcClass) -> String {
    let base = layer_for_plan(class);
    format!("{}-SECT", base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(elev: f64, height: f64) -> ElementGeometry {
        ElementGeometry::new(
            EntityId::new(),
            IfcClass::IfcWall,
            vec![
                [0.0, 0.0],
                [5000.0, 0.0],
                [5000.0, 200.0],
                [0.0, 200.0],
                [0.0, 0.0],
            ],
            height,
            elev,
        )
    }

    #[test]
    fn plan_cuts_walls_at_elevation() {
        let elements = vec![wall(0.0, 3000.0), wall(0.0, 100.0)];
        let drawing = generate_plan(&elements, 1500.0);
        // First wall (3 m tall) is cut; second wall (100 mm tall) is
        // wholly below.
        assert_eq!(drawing.primitives.len(), 2);
        let layers: Vec<&str> = drawing
            .primitives
            .iter()
            .map(|p| match &p.primitive {
                Primitive::Polyline(pl) => pl.layer.as_str(),
                _ => "",
            })
            .collect();
        assert!(layers.contains(&"A-WALL"));
        assert!(layers.contains(&"PLAN_BELOW"));
    }

    #[test]
    fn plan_skips_elements_above_cut() {
        let elements = vec![ElementGeometry::new(
            EntityId::new(),
            IfcClass::IfcWall,
            vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            500.0,
            10_000.0, // 10 m above ground
        )];
        let plan = generate_plan(&elements, 1500.0);
        assert!(plan.is_empty());
    }

    #[test]
    fn elevation_emits_rectangle_per_wall() {
        let elements = vec![wall(0.0, 3000.0)];
        let drawing = generate_elevation(&elements);
        assert_eq!(drawing.primitives.len(), 1);
        if let Primitive::Polyline(pl) = &drawing.primitives[0].primitive {
            assert!(pl.closed);
            assert_eq!(pl.vertices.len(), 4);
            assert_eq!(pl.vertices[0].at[0], 0.0);
            assert_eq!(pl.vertices[2].at[0], 5000.0);
            assert_eq!(pl.vertices[2].at[1], 3000.0);
        } else {
            panic!("expected polyline");
        }
    }

    #[test]
    fn section_cut_crosses_wall() {
        // Section plane along Y axis, passing through wall midpoint.
        let elements = vec![wall(0.0, 3000.0)];
        let plane = SectionPlane {
            origin: [2500.0, -100.0],
            direction: [0.0, 1.0],
        };
        let drawing = generate_section(&elements, &plane);
        assert_eq!(drawing.primitives.len(), 1);
    }

    #[test]
    fn section_skips_walls_off_plane() {
        let elements = vec![wall(0.0, 3000.0)];
        let plane = SectionPlane {
            origin: [10_000.0, 0.0], // far beyond wall
            direction: [0.0, 1.0],
        };
        let drawing = generate_section(&elements, &plane);
        assert!(drawing.is_empty());
    }

    #[test]
    fn primitive_carries_source_entity_for_associativity() {
        let elements = vec![wall(0.0, 3000.0)];
        let drawing = generate_elevation(&elements);
        assert_eq!(drawing.primitives[0].source, elements[0].element);
    }
}
