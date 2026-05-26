//! Phase 11 Task 22 acceptance test:
//!
//! Build a project with real rooms (with footprint geometry), doors,
//! windows, and walls (with materials and quantities); generate all
//! four schedule types end-to-end; verify both **row counts** and
//! **computed values**.
//!
//! This is the only test in the workspace that exercises all four
//! schedule generators against a single coherent project, with room
//! area sourced from real geometry rather than a pre-baked qto.

use std::collections::HashMap;

use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
use aec_bim::schedules::{
    backfill_space_quantities, generate_door_schedule, generate_material_schedule,
    generate_room_schedule, generate_window_schedule, ScheduleSheet,
};
use aec_bim::spatial::Project;
use aec_core::types::EntityId;

/// A small but representative project: 1 site, 1 building, 1 storey,
/// 2 rooms (Living 5x4 m, Bedroom 4x3 m), 2 doors, 2 windows, 2 walls
/// (concrete + brick).
struct Fixture {
    project: Project,
    classification: ClassificationStore,
    props: PropertyStore,
    space_ids: Vec<EntityId>,
}

fn build_fixture() -> Fixture {
    let mut project = Project::new("Phase 11 fixture");
    let root = project.root.clone();
    let site = project.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
    let bldg = project
        .add_child(&site, IfcClass::IfcBuilding, "Building A")
        .unwrap();
    let storey = project
        .add_child(&bldg, IfcClass::IfcBuildingStorey, "Level 1")
        .unwrap();

    let living = project
        .add_child(&storey, IfcClass::IfcSpace, "Living")
        .unwrap();
    let bedroom = project
        .add_child(&storey, IfcClass::IfcSpace, "Bedroom")
        .unwrap();

    let mut classification = ClassificationStore::default();
    let mut props = PropertyStore::new();

    // Room metadata (non-geometric — number / category / finishes).
    for (id, num, cat, ff, wf, cf) in [
        (
            living.clone(),
            "101",
            "Living room",
            "Engineered oak",
            "Painted plaster",
            "Skim coat",
        ),
        (
            bedroom.clone(),
            "102",
            "Bedroom",
            "Wool carpet",
            "Painted plaster",
            "Skim coat",
        ),
    ] {
        let mut pset = PropertySet::new("Pset_SpaceCommon");
        pset.set("Reference", PropertyValue::Label(num.into()));
        pset.set("Category", PropertyValue::Text(cat.into()));
        let mut finishes = PropertySet::new("Pset_SpaceFinishes");
        finishes.set("FloorFinish", PropertyValue::Label(ff.into()));
        finishes.set("WallFinish", PropertyValue::Label(wf.into()));
        finishes.set("CeilingFinish", PropertyValue::Label(cf.into()));
        // Height shipped in qto so we exercise the merge-with-existing path.
        let mut qto = QuantitySet::new("Qto_SpaceBaseQuantities");
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(2.7));
        let ep = props.entry(id);
        ep.upsert_pset(pset);
        ep.upsert_pset(finishes);
        ep.upsert_qset(qto);
    }

    // Real footprints (in metres) — area lands in qto via backfill.
    let mut footprints = HashMap::new();
    footprints.insert(
        living.clone(),
        vec![[0.0, 0.0], [5.0, 0.0], [5.0, 4.0], [0.0, 4.0]], // 20 m²
    );
    footprints.insert(
        bedroom.clone(),
        vec![[5.0, 0.0], [9.0, 0.0], [9.0, 3.0], [5.0, 3.0]], // 12 m²
    );
    let n_updated = backfill_space_quantities(&project, &mut props, &footprints);
    assert_eq!(n_updated, 2, "both rooms should be backfilled");

    // Doors: D01 (1000mm exterior, EI60) + D02 (900mm interior, EI30).
    for (mark, w, h, fire, op) in [
        ("D01", 1.0, 2.1, "EI60", "SingleSwingLeft"),
        ("D02", 0.9, 2.1, "EI30", "SingleSwingRight"),
    ] {
        let id = EntityId::new();
        classification.assign_manual(id.clone(), IfcClass::IfcDoor);
        let mut pdc = PropertySet::new("Pset_DoorCommon");
        pdc.set("Reference", PropertyValue::Label(mark.into()));
        pdc.set("FireRating", PropertyValue::Label(fire.into()));
        pdc.set("OperationType", PropertyValue::Label(op.into()));
        let mut hw = PropertySet::new("Pset_DoorHardware");
        hw.set("HardwareSet", PropertyValue::Label("HS-01".into()));
        let mut qto = QuantitySet::new("Qto_DoorBaseQuantities");
        qto.quantities
            .insert("Width".into(), PropertyValue::Length(w));
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(h));
        let ep = props.entry(id);
        ep.upsert_pset(pdc);
        ep.upsert_pset(hw);
        ep.upsert_qset(qto);
    }

    // Windows: W01 (double-glazed, external) + W02 (single-glazed, internal).
    for (mark, w, h, glass, u, ext) in [
        ("W01", 1.20, 1.40, "Double-glazed", 1.10, true),
        ("W02", 0.80, 1.20, "Single-glazed", 5.50, false),
    ] {
        let id = EntityId::new();
        classification.assign_manual(id.clone(), IfcClass::IfcWindow);
        let mut pwc = PropertySet::new("Pset_WindowCommon");
        pwc.set("Reference", PropertyValue::Label(mark.into()));
        pwc.set("ThermalTransmittance", PropertyValue::Real(u));
        pwc.set("IsExternal", PropertyValue::Boolean(ext));
        let mut glaze = PropertySet::new("Pset_DoorWindowGlazingType");
        glaze.set("GlazingType", PropertyValue::Label(glass.into()));
        let mut qto = QuantitySet::new("Qto_WindowBaseQuantities");
        qto.quantities
            .insert("Width".into(), PropertyValue::Length(w));
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(h));
        let ep = props.entry(id);
        ep.upsert_pset(pwc);
        ep.upsert_pset(glaze);
        ep.upsert_qset(qto);
    }

    // Walls: 1 concrete (8 m², supplier ACME) + 1 brick (6 m², supplier Bricky).
    for (mat, area, supplier) in [
        ("Concrete C25/30", 8.0, "ACME Concrete"),
        ("Brick — clay common", 6.0, "Bricky Ltd"),
    ] {
        let id = EntityId::new();
        classification.assign_manual(id.clone(), IfcClass::IfcWall);
        let mut mp = PropertySet::new("Pset_ElementMaterial");
        mp.set("Material", PropertyValue::Label(mat.into()));
        mp.set("Supplier", PropertyValue::Label(supplier.into()));
        let mut qto = QuantitySet::new("Qto_WallBaseQuantities");
        qto.quantities
            .insert("NetSideArea".into(), PropertyValue::Area(area));
        let ep = props.entry(id);
        ep.upsert_pset(mp);
        ep.upsert_qset(qto);
    }

    Fixture {
        project,
        classification,
        props,
        space_ids: vec![living, bedroom],
    }
}

#[test]
fn room_schedule_pulls_area_from_real_geometry_via_backfill() {
    let f = build_fixture();
    let (entries, sheet) = generate_room_schedule(&f.project, &f.props);
    assert_eq!(entries.len(), 2, "two rooms");
    // Sorted by reference number: 101 then 102.
    assert_eq!(entries[0].number, "101");
    assert_eq!(entries[0].name, "Living");
    assert!(
        (entries[0].area_m2.unwrap() - 20.0).abs() < 1e-9,
        "Living should be 5x4=20 m², got {:?}",
        entries[0].area_m2
    );
    assert!(
        (entries[0].perimeter_m.unwrap() - 18.0).abs() < 1e-9,
        "Living perimeter should be 18 m, got {:?}",
        entries[0].perimeter_m
    );
    assert!((entries[0].height_m.unwrap() - 2.7).abs() < 1e-9);
    assert_eq!(entries[0].floor_finish, "Engineered oak");
    assert_eq!(entries[0].wall_finish, "Painted plaster");

    assert_eq!(entries[1].number, "102");
    assert_eq!(entries[1].name, "Bedroom");
    assert!((entries[1].area_m2.unwrap() - 12.0).abs() < 1e-9);
    assert!((entries[1].perimeter_m.unwrap() - 14.0).abs() < 1e-9);
    assert_eq!(entries[1].floor_finish, "Wool carpet");

    assert_eq!(sheet.title, "Room schedule");
    assert_eq!(sheet.rows.len(), 2);
}

#[test]
fn door_schedule_has_two_entries_with_correct_widths() {
    let f = build_fixture();
    let (entries, sheet) = generate_door_schedule(&f.classification, &f.props);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].mark, "D01");
    assert!((entries[0].width_mm.unwrap() - 1000.0).abs() < 1e-6);
    assert!((entries[0].height_mm.unwrap() - 2100.0).abs() < 1e-6);
    assert_eq!(entries[0].fire_rating, "EI60");
    assert_eq!(entries[0].hardware, "HS-01");
    assert_eq!(entries[1].mark, "D02");
    assert!((entries[1].width_mm.unwrap() - 900.0).abs() < 1e-6);
    assert_eq!(entries[1].fire_rating, "EI30");
    assert_eq!(sheet.rows.len(), 2);
}

#[test]
fn window_schedule_has_two_entries_with_glass_and_uvalue() {
    let f = build_fixture();
    let (entries, sheet) = generate_window_schedule(&f.classification, &f.props);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].mark, "W01");
    assert!((entries[0].width_mm.unwrap() - 1200.0).abs() < 1e-6);
    assert!((entries[0].u_value.unwrap() - 1.10).abs() < 1e-9);
    assert_eq!(entries[0].glass_type, "Double-glazed");
    assert_eq!(entries[0].is_external, Some(true));
    assert_eq!(entries[1].mark, "W02");
    assert!((entries[1].u_value.unwrap() - 5.50).abs() < 1e-9);
    assert_eq!(entries[1].is_external, Some(false));
    assert_eq!(sheet.rows.len(), 2);
}

#[test]
fn material_schedule_aggregates_walls_by_material() {
    let f = build_fixture();
    let (entries, sheet) = generate_material_schedule(&f.classification, &f.props);
    // Two distinct materials (Brick + Concrete) → two rows.
    assert_eq!(entries.len(), 2);
    let brick = entries
        .iter()
        .find(|e| e.material.starts_with("Brick"))
        .unwrap();
    assert_eq!(brick.element_count, 1);
    assert!((brick.total_area_m2.unwrap() - 6.0).abs() < 1e-9);
    assert_eq!(brick.supplier, "Bricky Ltd");
    let concrete = entries
        .iter()
        .find(|e| e.material.starts_with("Concrete"))
        .unwrap();
    assert!((concrete.total_area_m2.unwrap() - 8.0).abs() < 1e-9);
    assert_eq!(concrete.supplier, "ACME Concrete");
    assert_eq!(sheet.rows.len(), 2);
}

#[test]
fn all_schedules_can_be_written_to_a_single_xlsx_workbook() {
    let f = build_fixture();
    let (_, room_sheet) = generate_room_schedule(&f.project, &f.props);
    let (_, door_sheet) = generate_door_schedule(&f.classification, &f.props);
    let (_, window_sheet) = generate_window_schedule(&f.classification, &f.props);
    let (_, material_sheet) = generate_material_schedule(&f.classification, &f.props);
    let sheets = vec![room_sheet, door_sheet, window_sheet, material_sheet];
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("schedules.xlsx");
    ScheduleSheet::write_xlsx_multi(&sheets, &p).unwrap();
    let meta = std::fs::metadata(&p).unwrap();
    assert!(meta.len() > 2000, "xlsx workbook should be non-trivial");
}

#[test]
fn backfill_is_idempotent_when_called_twice_with_same_footprints() {
    // First call backfills; second call leaves the qto alone.
    let mut f = build_fixture();
    let mut fps = HashMap::new();
    fps.insert(
        f.space_ids[0].clone(),
        vec![[0.0, 0.0], [5.0, 0.0], [5.0, 4.0], [0.0, 4.0]],
    );
    // The fixture already backfilled both — calling again should not
    // overwrite.
    let n2 = backfill_space_quantities(&f.project, &mut f.props, &fps);
    assert_eq!(n2, 0, "non-forced backfill is idempotent");
}
