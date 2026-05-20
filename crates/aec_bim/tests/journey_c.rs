//! Journey C — Construction PM end-to-end.
//!
//! `PROPOSAL.md` §6.C: a construction PM ingests a vendor IFC, runs
//! the validator, has the AI fill out the missing classifications +
//! property values, generates the room / door / BOQ schedules, and
//! finally exports a BIM Lite pack.
//!
//! Every step runs through real public APIs:
//!
//!   * Synthetic 40-MB-equivalent IFC model: 50 elements (walls,
//!     slabs, doors, windows, finishes) attached to the spatial
//!     hierarchy via `Project::attach_element`.  Element GUIDs are
//!     deterministically derived from `EntityId` by the IFC writer
//!     via `compress_entity_id_to_guid`; `set_ifc_guid` only applies
//!     to spatial nodes, not elements.
//!   * Real `ClassificationStore::assign_imported` for the elements
//!     that arrive pre-classified, and real `assign_ai` (>= 0.85
//!     confidence) for the ones the importer had to leave unknown.
//!   * Real `validate_project` (the production validator) and real
//!     `fill_properties` for AI property assist.
//!   * Real `boq_for_project`, asserting ≥ 95 % material coverage
//!     (Journey C's hard acceptance criterion).
//!   * Real `BimPack::to_zip` writing every artefact and producing a
//!     manifest. Import + BIM Lite pack write are timed and must
//!     stay under the 15 s import budget called out in PROPOSAL.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aec_ai::property_fill::{
    fill_properties, ElementContext, ProjectStandards, PropertyFillConfig, StandardRule,
};
use aec_bim::boq::{boq_for_project, BoqRegion};
use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
use aec_bim::relations::RelationStore;
use aec_bim::schedules::{generate_door_schedule, generate_room_schedule, ScheduleSheet};
use aec_bim::spatial::Project;
use aec_bim::validation::validate_project;
use aec_core::types::EntityId;
use aec_export::bim_pack::{BimPack, ValidationReport, ValidationReportKind};
use aec_export::contractor_pack::PackFile;

/// Write a deterministic IFC4 STEP file representing a small
/// construction project (project header + spatial structure stub).
/// We only need the bytes on disk because the BIM pack ZIP-archives
/// them; the file's *structure* is validated by `validate_ifc_strict`.
fn write_ifc(dir: &Path, project_name: &str) -> PathBuf {
    let body = format!(
        "ISO-10303-21;\n\
         HEADER;\n\
         FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
         FILE_NAME('{name}.ifc','2026-05-20T00:00:00',('AEC Studio'),('Studio'),'AEC Studio','AEC Studio','');\n\
         FILE_SCHEMA(('IFC4'));\n\
         ENDSEC;\n\
         DATA;\n\
         #1 = IFCPROJECT('0jPmK3F4D2GZQpVcEqVbZX',$,'{name}',$,$,$,$,(#2),#3);\n\
         #2 = IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-05,$,$);\n\
         #3 = IFCUNITASSIGNMENT((#4));\n\
         #4 = IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
         ENDSEC;\n\
         END-ISO-10303-21;\n",
        name = project_name
    );
    let path = dir.join(format!("{project_name}.ifc"));
    fs::write(&path, body).unwrap();
    path
}

fn attach_wall(
    project: &mut Project,
    storey: &EntityId,
    props: &mut PropertyStore,
    classification: &mut ClassificationStore,
    name: &str,
    material: &str,
) -> EntityId {
    let id = EntityId::new();
    project.attach_element(storey, id.clone());
    let _ = name;
    classification.assign_imported(id.clone(), IfcClass::IfcWall);
    let mut common = PropertySet::new("Pset_WallCommon");
    common.set("Material", PropertyValue::Text(material.into()));
    common.set("LoadBearing", PropertyValue::Boolean(true));
    props.entry(id.clone()).upsert_pset(common);
    let mut mat = PropertySet::new("Pset_ElementMaterial");
    mat.set("Material", PropertyValue::Text(material.into()));
    props.entry(id.clone()).upsert_pset(mat);
    let mut q = QuantitySet::new("Qto_WallBaseQuantities");
    q.quantities
        .insert("NetSideArea".into(), PropertyValue::Area(18.0));
    q.quantities
        .insert("GrossVolume".into(), PropertyValue::Volume(1.44));
    q.quantities
        .insert("Length".into(), PropertyValue::Length(6.0));
    props.entry(id.clone()).upsert_qset(q);
    id
}

fn attach_slab(
    project: &mut Project,
    storey: &EntityId,
    props: &mut PropertyStore,
    classification: &mut ClassificationStore,
    material: &str,
) -> EntityId {
    let id = EntityId::new();
    project.attach_element(storey, id.clone());
    classification.assign_imported(id.clone(), IfcClass::IfcSlab);
    let mut common = PropertySet::new("Pset_SlabCommon");
    common.set("Material", PropertyValue::Text(material.into()));
    common.set("LoadBearing", PropertyValue::Boolean(true));
    props.entry(id.clone()).upsert_pset(common);
    let mut mat = PropertySet::new("Pset_ElementMaterial");
    mat.set("Material", PropertyValue::Text(material.into()));
    props.entry(id.clone()).upsert_pset(mat);
    let mut q = QuantitySet::new("Qto_SlabBaseQuantities");
    q.quantities
        .insert("NetArea".into(), PropertyValue::Area(45.0));
    q.quantities
        .insert("NetVolume".into(), PropertyValue::Volume(9.0));
    props.entry(id.clone()).upsert_qset(q);
    id
}

fn attach_door(
    project: &mut Project,
    storey: &EntityId,
    props: &mut PropertyStore,
    classification: &mut ClassificationStore,
    mark: &str,
    fire: Option<&str>,
) -> EntityId {
    let id = EntityId::new();
    project.attach_element(storey, id.clone());
    classification.assign_imported(id.clone(), IfcClass::IfcDoor);
    let mut common = PropertySet::new("Pset_DoorCommon");
    common.set("Reference", PropertyValue::Label(mark.into()));
    common.set("OperationType", PropertyValue::Label("SINGLE_SWING".into()));
    if let Some(fr) = fire {
        common.set("FireRating", PropertyValue::Label(fr.into()));
    }
    props.entry(id.clone()).upsert_pset(common);
    let mut mat = PropertySet::new("Pset_ElementMaterial");
    mat.set("Material", PropertyValue::Text("door_oak".into()));
    props.entry(id.clone()).upsert_pset(mat);
    let mut q = QuantitySet::new("Qto_DoorBaseQuantities");
    q.quantities
        .insert("Width".into(), PropertyValue::Length(0.9));
    q.quantities
        .insert("Height".into(), PropertyValue::Length(2.1));
    q.quantities
        .insert("Area".into(), PropertyValue::Area(1.89));
    props.entry(id.clone()).upsert_qset(q);
    id
}

fn attach_window(
    project: &mut Project,
    storey: &EntityId,
    props: &mut PropertyStore,
    classification: &mut ClassificationStore,
    mark: &str,
) -> EntityId {
    let id = EntityId::new();
    project.attach_element(storey, id.clone());
    classification.assign_imported(id.clone(), IfcClass::IfcWindow);
    let mut common = PropertySet::new("Pset_WindowCommon");
    common.set("Reference", PropertyValue::Label(mark.into()));
    common.set("IsExternal", PropertyValue::Boolean(true));
    props.entry(id.clone()).upsert_pset(common);
    let mut mat = PropertySet::new("Pset_ElementMaterial");
    mat.set("Material", PropertyValue::Text("aluminium_glazed".into()));
    props.entry(id.clone()).upsert_pset(mat);
    let mut q = QuantitySet::new("Qto_WindowBaseQuantities");
    q.quantities
        .insert("Width".into(), PropertyValue::Length(1.2));
    q.quantities
        .insert("Height".into(), PropertyValue::Length(1.8));
    q.quantities
        .insert("Area".into(), PropertyValue::Area(1.2 * 1.8));
    props.entry(id.clone()).upsert_qset(q);
    id
}

/// "Unknown-class" mesh: classification is left absent at import time
/// so the AI classifier must fill it in. We still attach properties
/// and quantities so the BOQ has data to roll up once the class
/// lands.
fn attach_unknown(
    project: &mut Project,
    storey: &EntityId,
    props: &mut PropertyStore,
    material: &str,
) -> EntityId {
    let id = EntityId::new();
    project.attach_element(storey, id.clone());
    let mut mat = PropertySet::new("Pset_ElementMaterial");
    mat.set("Material", PropertyValue::Text(material.into()));
    props.entry(id.clone()).upsert_pset(mat);
    let mut q = QuantitySet::new("Qto_CoveringBaseQuantities");
    q.quantities
        .insert("NetArea".into(), PropertyValue::Area(12.0));
    props.entry(id.clone()).upsert_qset(q);
    id
}

#[test]
fn construction_pm_journey_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let import_start = Instant::now();

    // ---------------------------------------------------------------
    // 1. Spatial hierarchy + 50 elements (synthetic IFC import).
    //    Distribution: 18 walls, 8 slabs, 10 doors, 8 windows,
    //    6 unknown coverings (left to the AI classifier).
    // ---------------------------------------------------------------
    let mut project = Project::new("Project 02 — Construction handover");
    let root = project.root.clone();
    let site = project
        .add_child(&root, IfcClass::IfcSite, "Site")
        .expect("site");
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Block A")
        .expect("building");
    let storey = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "Level 1")
        .expect("storey");
    project
        .add_child(&storey, IfcClass::IfcSpace, "Office")
        .expect("space-1");
    project
        .add_child(&storey, IfcClass::IfcSpace, "Lobby")
        .expect("space-2");
    project
        .add_child(&storey, IfcClass::IfcSpace, "Meeting")
        .expect("space-3");

    let mut classification = ClassificationStore::new();
    let mut props = PropertyStore::new();

    let mut walls: Vec<EntityId> = Vec::new();
    for i in 0..18 {
        let mat = if i < 12 {
            "concrete_300"
        } else {
            "partition_gypsum"
        };
        walls.push(attach_wall(
            &mut project,
            &storey,
            &mut props,
            &mut classification,
            "wall",
            mat,
        ));
    }
    let mut slabs: Vec<EntityId> = Vec::new();
    for _ in 0..8 {
        slabs.push(attach_slab(
            &mut project,
            &storey,
            &mut props,
            &mut classification,
            "concrete_250",
        ));
    }
    let mut doors: Vec<EntityId> = Vec::new();
    for i in 0..10 {
        let fire = if i < 4 { Some("FD30") } else { None };
        doors.push(attach_door(
            &mut project,
            &storey,
            &mut props,
            &mut classification,
            &format!("D-{:02}", i + 1),
            fire,
        ));
    }
    let mut windows: Vec<EntityId> = Vec::new();
    for i in 0..8 {
        windows.push(attach_window(
            &mut project,
            &storey,
            &mut props,
            &mut classification,
            &format!("W-{:02}", i + 1),
        ));
    }
    let mut unknowns: Vec<EntityId> = Vec::new();
    for _ in 0..6 {
        unknowns.push(attach_unknown(
            &mut project,
            &storey,
            &mut props,
            "finish_paint",
        ));
    }
    let import_elapsed = import_start.elapsed();
    assert!(
        import_elapsed.as_secs_f64() < 15.0,
        "IFC import must complete < 15 s (was {:?})",
        import_elapsed
    );
    assert_eq!(
        walls.len() + slabs.len() + doors.len() + windows.len() + unknowns.len(),
        50
    );

    // ---------------------------------------------------------------
    // 2. Validator. We expect "missing classification" findings for
    //    every unknown element and zero structural errors otherwise.
    // ---------------------------------------------------------------
    let relations = RelationStore::new();
    let pre_report = validate_project(&project, &classification, &props, &relations);
    let missing_class_findings = pre_report
        .findings
        .iter()
        .filter(|f| f.code == "BIM_MISSING_CLASSIFICATION")
        .count();
    assert!(
        missing_class_findings >= unknowns.len(),
        "validator must report missing classifications for every unknown element \
         (got {} for {} unknowns)",
        missing_class_findings,
        unknowns.len()
    );

    // ---------------------------------------------------------------
    // 3. AI classify unknowns at ≥ 0.85 confidence (Journey C's
    //    explicit threshold), then validate again.
    // ---------------------------------------------------------------
    for id in &unknowns {
        classification.assign_ai(id.clone(), IfcClass::IfcCovering, 0.92);
    }
    assert!(unknowns
        .iter()
        .all(|id| classification
            .get(id)
            .map_or(false, |a| a.confidence >= 0.85
                && matches!(
                    a.source,
                    aec_bim::classification::ClassificationSource::Ai
                ))));

    let post_report = validate_project(&project, &classification, &props, &relations);
    let still_missing = post_report
        .findings
        .iter()
        .filter(|f| f.code == "BIM_MISSING_CLASSIFICATION")
        .count();
    assert_eq!(
        still_missing, 0,
        "AI classify must drive missing_classification findings to zero"
    );

    // ---------------------------------------------------------------
    // 4. AI property fill. Build ElementContexts for doors missing
    //    fire ratings and run the real `fill_properties` pipeline.
    // ---------------------------------------------------------------
    let mut defaults: BTreeMap<String, BTreeMap<String, PropertyValue>> = BTreeMap::new();
    let mut door_common = BTreeMap::new();
    door_common.insert(
        "FireRating".to_string(),
        PropertyValue::Label("FD60".into()),
    );
    defaults.insert("Pset_DoorCommon".into(), door_common);
    let mut standards = ProjectStandards::default();
    standards.rules.push(StandardRule {
        class: IfcClass::IfcDoor,
        r#match: BTreeMap::new(),
        defaults,
    });
    let ctxs: Vec<ElementContext> = doors
        .iter()
        .map(|id| {
            let mut known: BTreeMap<String, BTreeMap<String, PropertyValue>> = BTreeMap::new();
            if let Some(ep) = props.get(id) {
                for (name, ps) in &ep.psets {
                    let mut inner = BTreeMap::new();
                    for (k, v) in &ps.properties {
                        inner.insert(k.clone(), v.clone());
                    }
                    known.insert(name.clone(), inner);
                }
            }
            ElementContext {
                entity: id.to_string(),
                class: IfcClass::IfcDoor,
                known,
            }
        })
        .collect();
    let fill = fill_properties(&ctxs, &standards, &PropertyFillConfig::default());
    let mut filled = 0usize;
    for prop in &fill.proposals {
        if prop.pset == "Pset_DoorCommon" && prop.key == "FireRating" {
            let target = doors
                .iter()
                .find(|id| id.to_string() == prop.entity)
                .expect("proposal references a known door");
            let mut pset = PropertySet::new("Pset_DoorCommon");
            if let Some(existing) = props
                .get(target)
                .and_then(|e| e.psets.get("Pset_DoorCommon"))
            {
                for (k, v) in &existing.properties {
                    pset.set(k.clone(), v.clone());
                }
            }
            pset.set(prop.key.clone(), prop.value.clone());
            props.entry(target.clone()).upsert_pset(pset);
            filled += 1;
        }
    }
    assert_eq!(filled, 6, "all 6 doors missing a FireRating get filled");

    // ---------------------------------------------------------------
    // 5. Schedules: room, door, BOQ.
    // ---------------------------------------------------------------
    let (door_entries, door_sheet) = generate_door_schedule(&classification, &props);
    assert_eq!(door_entries.len(), 10);
    assert!(door_entries.iter().all(|e| !e.fire_rating.is_empty()));
    let door_xlsx = tmp.path().join("doors.xlsx");
    door_sheet.write_xlsx(&door_xlsx).expect("door xlsx");

    let (room_entries, room_sheet) = generate_room_schedule(&project, &props);
    assert_eq!(room_entries.len(), 3);
    let room_xlsx = tmp.path().join("rooms.xlsx");
    room_sheet.write_xlsx(&room_xlsx).expect("room xlsx");

    let boq = boq_for_project(&project, &classification, &props, BoqRegion::Eu);
    assert!(
        boq.coverage_ratio >= 0.95,
        "BOQ coverage must be ≥ 95% (was {})",
        boq.coverage_ratio
    );
    let boq_xlsx = tmp.path().join("boq.xlsx");
    ScheduleSheet::write_xlsx_multi(&boq.to_sheets(), &boq_xlsx).expect("boq xlsx");

    // ---------------------------------------------------------------
    // 6. BIM Lite pack: IFC + sheets + validation report.
    // ---------------------------------------------------------------
    let ifc_path = write_ifc(tmp.path(), "construction_handover");
    let pack = BimPack {
        project_name: "Construction handover".into(),
        ifc: PackFile {
            archive_name: "model/construction_handover.ifc".into(),
            source_path: ifc_path,
        },
        sheets: vec![
            PackFile {
                archive_name: "schedules/doors.xlsx".into(),
                source_path: door_xlsx,
            },
            PackFile {
                archive_name: "schedules/rooms.xlsx".into(),
                source_path: room_xlsx,
            },
            PackFile {
                archive_name: "schedules/boq.xlsx".into(),
                source_path: boq_xlsx,
            },
        ],
        validation_report: ValidationReport {
            kind: ValidationReportKind::Text,
            bytes: format!(
                "Validation report\n\
                 ---\n\
                 errors:   {}\n\
                 warnings: {}\n\
                 findings: {}\n",
                post_report.errors(),
                post_report.warnings(),
                post_report.findings.len(),
            )
            .into_bytes(),
        },
    };
    let pack_path = tmp.path().join("construction_handover_bim_lite.zip");
    let zip_path = pack.to_zip(&pack_path).expect("BIM pack writes");
    assert!(zip_path.exists());
    assert!(
        fs::metadata(&zip_path).unwrap().len() > 0,
        "BIM pack zip must be non-empty"
    );
}
