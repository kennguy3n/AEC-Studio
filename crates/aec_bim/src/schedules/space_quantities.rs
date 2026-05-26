//! Compute Qto_SpaceBaseQuantities from real footprint geometry.
//!
//! IFC schedules nominally read room area from
//! `Qto_SpaceBaseQuantities.NetFloorArea`, but that quantity is the
//! *result* of a geometric computation on the room's footprint
//! polygon. When a project carries the footprint but hasn't been
//! through a full tessellation pass yet (e.g. immediately after a
//! template instantiation, or after the user edited a wall), the
//! qto is stale or missing. This module backfills the qto in place
//! from the supplied footprint map.
//!
//! Area is computed via the shoelace formula. Perimeter is the sum
//! of Euclidean edge lengths.
//!
//! Units convention: footprint points are in **metres** (the
//! project canonical unit), so `NetFloorArea` lands in m² and
//! `GrossPerimeter` in m — matching the IFC `Pset_SpaceBaseQuantities`
//! schema (`IfcAreaMeasure` / `IfcLengthMeasure`).

use std::collections::HashMap;

use aec_core::types::EntityId;

use crate::classification::IfcClass;
use crate::properties::{PropertyStore, PropertyValue, QuantitySet};
use crate::spatial::Project;

/// Footprint polygon as a closed loop of `[x, y]` points in metres.
/// The last point need not duplicate the first — both forms are
/// handled. Polygon orientation does not matter (we take the absolute
/// value of the shoelace sum).
pub type FootprintPolygon = Vec<[f64; 2]>;

/// Backfill `Qto_SpaceBaseQuantities` (`NetFloorArea`, `GrossPerimeter`)
/// from real footprint geometry for every `IfcSpace` in `project`
/// that has an entry in `footprints`. Returns the number of spaces
/// that were updated.
///
/// If a space already has a non-zero `NetFloorArea` in the qto, this
/// function **does not overwrite it** — that lets a richer
/// tessellator (which would account for holes, curved walls, voids,
/// etc.) take precedence over the shoelace fallback. To force an
/// overwrite, call [`backfill_space_quantities_forced`].
pub fn backfill_space_quantities<S: std::hash::BuildHasher>(
    project: &Project,
    props: &mut PropertyStore,
    footprints: &HashMap<EntityId, FootprintPolygon, S>,
) -> usize {
    backfill_inner(project, props, footprints, false)
}

/// Same as [`backfill_space_quantities`] but unconditionally
/// overwrites existing qto entries. Use this when the footprint has
/// just been edited and the previous qto is known to be stale.
pub fn backfill_space_quantities_forced<S: std::hash::BuildHasher>(
    project: &Project,
    props: &mut PropertyStore,
    footprints: &HashMap<EntityId, FootprintPolygon, S>,
) -> usize {
    backfill_inner(project, props, footprints, true)
}

fn backfill_inner<S: std::hash::BuildHasher>(
    project: &Project,
    props: &mut PropertyStore,
    footprints: &HashMap<EntityId, FootprintPolygon, S>,
    force: bool,
) -> usize {
    let spaces: Vec<EntityId> = project
        .nodes_of_class(&IfcClass::IfcSpace)
        .into_iter()
        .map(|n| n.id.clone())
        .collect();

    let mut updated = 0usize;
    for id in spaces {
        let Some(footprint) = footprints.get(&id) else {
            continue;
        };
        // Use `effective_len` so a closed polygon expressed with a
        // trailing duplicate point (e.g. `[a, b, c, a]`) is recognised
        // as a degenerate-vs-real ring consistently with
        // `polygon_area_m2` / `polygon_perimeter_m`. Raw `len() < 3`
        // would have let a 3-point closed polygon (effective len 2)
        // through and then written a zero-area qto entry that adds
        // nothing but does dirty the property store.
        if effective_len(footprint) < 3 {
            continue;
        }

        let area = polygon_area_m2(footprint);
        let perimeter = polygon_perimeter_m(footprint);

        // Skip when the computed area is degenerate. A polygon
        // with `effective_len >= 3` but all vertices collinear
        // (e.g. `[[0,0],[1,0],[2,0],[3,0]]`) still produces
        // `area == 0.0`. Without this guard we would write
        // `NetFloorArea = 0.0`, and on every subsequent
        // non-forced call `existing_area > 0.0` would be false
        // (because `0.0 > 0.0` is false), so we would re-walk
        // the same write — making the function non-idempotent
        // and dirtying the property store with no information.
        // The `1e-9` m² ≈ 1 µm² threshold sits well below any
        // meaningful BIM precision while still tolerating the
        // floating-point residue that shoelace can produce on
        // a near-collinear-but-not-quite ring.
        const MIN_AREA_M2: f64 = 1e-9;
        if area < MIN_AREA_M2 {
            continue;
        }

        let existing_area = props
            .get(&id)
            .and_then(|e| e.get("Qto_SpaceBaseQuantities", "NetFloorArea"))
            .and_then(PropertyValue::as_real)
            .unwrap_or(0.0);

        if !force && existing_area > 0.0 {
            continue;
        }

        let ep = props.entry(id);
        // Merge: don't blow away other quantities (Height, etc.) that
        // a previous tessellation might have populated.
        let mut qto = ep
            .qsets
            .get("Qto_SpaceBaseQuantities")
            .cloned()
            .unwrap_or_else(|| QuantitySet::new("Qto_SpaceBaseQuantities"));
        qto.quantities
            .insert("NetFloorArea".into(), PropertyValue::Area(area));
        qto.quantities
            .insert("GrossPerimeter".into(), PropertyValue::Length(perimeter));
        ep.upsert_qset(qto);
        updated += 1;
    }
    updated
}

/// Absolute polygon area via the shoelace formula. Robust to
/// trailing duplicate-point closing (last point == first point).
pub fn polygon_area_m2(points: &[[f64; 2]]) -> f64 {
    let n = effective_len(points);
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        sum += points[i][0] * points[j][1] - points[j][0] * points[i][1];
    }
    (sum * 0.5).abs()
}

/// Closed-polygon perimeter (m). Trailing duplicate-point closing
/// is tolerated.
pub fn polygon_perimeter_m(points: &[[f64; 2]]) -> f64 {
    let n = effective_len(points);
    if n < 2 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        let dx = points[j][0] - points[i][0];
        let dy = points[j][1] - points[i][1];
        s += (dx * dx + dy * dy).sqrt();
    }
    s
}

fn effective_len(points: &[[f64; 2]]) -> usize {
    if points.len() >= 2 {
        let first = points[0];
        let last = points[points.len() - 1];
        let dx = (first[0] - last[0]).abs();
        let dy = (first[1] - last[1]).abs();
        if dx < 1e-9 && dy < 1e-9 {
            return points.len() - 1;
        }
    }
    points.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::classification::IfcClass;
    use crate::properties::{PropertyStore, PropertyValue, QuantitySet};

    fn square_5x4_open() -> FootprintPolygon {
        vec![[0.0, 0.0], [5.0, 0.0], [5.0, 4.0], [0.0, 4.0]]
    }

    fn square_5x4_closed() -> FootprintPolygon {
        vec![[0.0, 0.0], [5.0, 0.0], [5.0, 4.0], [0.0, 4.0], [0.0, 0.0]]
    }

    #[test]
    fn area_shoelace_matches_geometric_truth() {
        let a = polygon_area_m2(&square_5x4_open());
        assert!((a - 20.0).abs() < 1e-9, "got {a}");
    }

    #[test]
    fn area_handles_trailing_closing_point() {
        let a = polygon_area_m2(&square_5x4_closed());
        assert!((a - 20.0).abs() < 1e-9);
    }

    #[test]
    fn area_independent_of_winding_order() {
        let mut cw = square_5x4_open();
        cw.reverse();
        let a = polygon_area_m2(&cw);
        assert!((a - 20.0).abs() < 1e-9);
    }

    #[test]
    fn perimeter_of_rectangle_is_2w_plus_2h() {
        let p = polygon_perimeter_m(&square_5x4_open());
        assert!((p - 18.0).abs() < 1e-9, "got {p}");
        let pc = polygon_perimeter_m(&square_5x4_closed());
        assert!((pc - 18.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_polygon_returns_zero() {
        assert_eq!(polygon_area_m2(&[]), 0.0);
        assert_eq!(polygon_area_m2(&[[0.0, 0.0]]), 0.0);
        assert_eq!(polygon_area_m2(&[[0.0, 0.0], [1.0, 0.0]]), 0.0);
    }

    fn project_with_one_space() -> (Project, EntityId) {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let bldg = p.add_child(&site, IfcClass::IfcBuilding, "B").unwrap();
        let st = p
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L1")
            .unwrap();
        let s = p.add_child(&st, IfcClass::IfcSpace, "Living").unwrap();
        (p, s)
    }

    #[test]
    fn backfill_writes_area_and_perimeter_into_qto() {
        let (p, s) = project_with_one_space();
        let mut props = PropertyStore::new();
        let mut fps = std::collections::HashMap::new();
        fps.insert(s.clone(), square_5x4_open());
        let n = backfill_space_quantities(&p, &mut props, &fps);
        assert_eq!(n, 1);

        let ep = props.get(&s).unwrap();
        let a = ep
            .get("Qto_SpaceBaseQuantities", "NetFloorArea")
            .and_then(PropertyValue::as_real)
            .unwrap();
        assert!((a - 20.0).abs() < 1e-9);
        let per = ep
            .get("Qto_SpaceBaseQuantities", "GrossPerimeter")
            .and_then(PropertyValue::as_real)
            .unwrap();
        assert!((per - 18.0).abs() < 1e-9);
    }

    #[test]
    fn non_forced_backfill_preserves_existing_area() {
        let (p, s) = project_with_one_space();
        let mut props = PropertyStore::new();
        // Pre-populate with a value the tessellator would have written.
        let mut qto = QuantitySet::new("Qto_SpaceBaseQuantities");
        qto.quantities
            .insert("NetFloorArea".into(), PropertyValue::Area(42.0));
        props.entry(s.clone()).upsert_qset(qto);

        let mut fps = std::collections::HashMap::new();
        fps.insert(s.clone(), square_5x4_open());
        let n = backfill_space_quantities(&p, &mut props, &fps);
        assert_eq!(
            n, 0,
            "non-forced backfill must not overwrite an existing area"
        );

        let a = props
            .get(&s)
            .unwrap()
            .get("Qto_SpaceBaseQuantities", "NetFloorArea")
            .and_then(PropertyValue::as_real)
            .unwrap();
        assert!((a - 42.0).abs() < 1e-9);
    }

    #[test]
    fn forced_backfill_overwrites_existing_area() {
        let (p, s) = project_with_one_space();
        let mut props = PropertyStore::new();
        let mut qto = QuantitySet::new("Qto_SpaceBaseQuantities");
        qto.quantities
            .insert("NetFloorArea".into(), PropertyValue::Area(42.0));
        props.entry(s.clone()).upsert_qset(qto);

        let mut fps = std::collections::HashMap::new();
        fps.insert(s.clone(), square_5x4_open());
        let n = backfill_space_quantities_forced(&p, &mut props, &fps);
        assert_eq!(n, 1);

        let a = props
            .get(&s)
            .unwrap()
            .get("Qto_SpaceBaseQuantities", "NetFloorArea")
            .and_then(PropertyValue::as_real)
            .unwrap();
        assert!((a - 20.0).abs() < 1e-9);
    }

    #[test]
    fn backfill_merges_with_existing_quantities() {
        let (p, s) = project_with_one_space();
        let mut props = PropertyStore::new();
        let mut qto = QuantitySet::new("Qto_SpaceBaseQuantities");
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(2.7));
        props.entry(s.clone()).upsert_qset(qto);

        let mut fps = std::collections::HashMap::new();
        fps.insert(s.clone(), square_5x4_open());
        backfill_space_quantities(&p, &mut props, &fps);

        let ep = props.get(&s).unwrap();
        let h = ep
            .get("Qto_SpaceBaseQuantities", "Height")
            .and_then(PropertyValue::as_real)
            .unwrap();
        assert!((h - 2.7).abs() < 1e-9, "existing Height must survive");
        let a = ep
            .get("Qto_SpaceBaseQuantities", "NetFloorArea")
            .and_then(PropertyValue::as_real)
            .unwrap();
        assert!((a - 20.0).abs() < 1e-9);
    }

    #[test]
    fn space_without_footprint_is_skipped() {
        let (p, _s) = project_with_one_space();
        let mut props = PropertyStore::new();
        let fps = std::collections::HashMap::<EntityId, FootprintPolygon>::new();
        let n = backfill_space_quantities(&p, &mut props, &fps);
        assert_eq!(n, 0);
    }

    #[test]
    fn non_space_classes_are_ignored() {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let _w = p.add_child(&site, IfcClass::IfcWall, "W").unwrap();
        let mut props = PropertyStore::new();
        let fps = std::collections::HashMap::<EntityId, FootprintPolygon>::new();
        assert_eq!(backfill_space_quantities(&p, &mut props, &fps), 0);
    }

    #[test]
    fn degenerate_closed_polygon_does_not_write_zero_qto() {
        // Regression guard for the `effective_len` guard: a polygon
        // whose *raw* length is ≥3 but whose *effective* length
        // (after stripping the trailing closing point) is <3 must be
        // skipped, not written into the qto as a zero-area entry.
        let (p, s) = project_with_one_space();
        let mut props = PropertyStore::new();
        let mut fps = std::collections::HashMap::new();
        // Three raw points, two effective (last == first).
        fps.insert(s.clone(), vec![[0.0, 0.0], [1.0, 0.0], [0.0, 0.0]]);
        let n = backfill_space_quantities(&p, &mut props, &fps);
        assert_eq!(n, 0, "degenerate closed polygon must be skipped");
        assert!(
            props.get(&s).is_none()
                || props
                    .get(&s)
                    .and_then(|e| e.get("Qto_SpaceBaseQuantities", "NetFloorArea"))
                    .is_none(),
            "no qto entry should be written for a degenerate polygon"
        );
    }

    #[test]
    fn collinear_polygon_is_skipped_and_backfill_is_idempotent() {
        // Regression guard for the zero-area collinear-vertex case:
        // a polygon with `effective_len >= 3` but all vertices on a
        // single line (here: four points on the x-axis) yields
        // `polygon_area_m2 == 0.0` and must be skipped, not written
        // as a zero-area qto. Crucially, calling `backfill` twice
        // must return the same `0` both times — proving the
        // function is strictly idempotent for this case, not
        // re-walking the same zero-write on every call.
        let (p, s) = project_with_one_space();
        let mut props = PropertyStore::new();
        let mut fps = std::collections::HashMap::new();
        // Four collinear points → effective_len 4, area 0.0.
        fps.insert(
            s.clone(),
            vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0]],
        );

        let n1 = backfill_space_quantities(&p, &mut props, &fps);
        assert_eq!(n1, 0, "collinear polygon must be skipped on first call");
        assert!(
            props
                .get(&s)
                .and_then(|e| e.get("Qto_SpaceBaseQuantities", "NetFloorArea"))
                .is_none(),
            "no qto entry should be written for a collinear polygon",
        );

        let n2 = backfill_space_quantities(&p, &mut props, &fps);
        assert_eq!(
            n2, 0,
            "second non-forced call must not re-walk the same zero-area write",
        );
        assert!(
            props
                .get(&s)
                .and_then(|e| e.get("Qto_SpaceBaseQuantities", "NetFloorArea"))
                .is_none(),
            "second call must still leave no NetFloorArea on the entry",
        );
    }
}
