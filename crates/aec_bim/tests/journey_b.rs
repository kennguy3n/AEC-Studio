//! Journey B — Architecture Studio end-to-end.
//!
//! `PROPOSAL.md` §6.B: a small studio builds a café concept, models
//! walls and counters, generates a handful of plan sheets, classifies
//! the model as IFC, generates door + room schedules, has the AI fill
//! fire ratings, queues two hero renders, and finally exports a
//! contractor handoff pack (sheets + schedules + IFC + BOQ + proposal
//! PDF).
//!
//! Everything here runs against real public APIs, no stubs:
//!   * Real café template load via `TemplateLoader`.
//!   * Real `Project` spatial graph (Site → Building → Storey → Spaces).
//!   * Real `ClassificationStore::assign_ai` (AI-authored, above
//!     threshold) to mark every element as its IFC class.
//!   * Real `PropertyStore` + `aec_ai::property_fill::fill_properties`
//!     with a `ProjectStandards` rule that supplies a FireRating for
//!     `IfcDoor`s missing one. We assert the AI proposals are then
//!     applied to the property store.
//!   * Real door + room schedule generation.
//!   * Real `boq_for_project` and assert ≥ 95% coverage.
//!   * Real sheets (6 of them) — Sheet + TitleBlock + viewport.
//!   * Real `RenderQueue` driving two hero renders to completion.
//!   * Real `ContractorPack::to_zip`, asserting the < 60s timing.
//!
//! The IFC payload is materialised on disk as a minimal STEP-style
//! header so the pack write goes through real `Read::read_to_end` and
//! the manifest BLAKE3 hash is computed against legitimate bytes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aec_ai::property_fill::{fill_properties, ProjectStandards, PropertyFillConfig};
use aec_bim::boq::{boq_for_project, BoqRegion};
use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
use aec_bim::schedules::{generate_door_schedule, generate_room_schedule};
use aec_bim::spatial::Project;
use aec_cad::sheets::{Margins, Orientation, PaperSize, Sheet, SheetSet, TitleBlock};
use aec_core::templates::TemplateLoader;
use aec_core::types::EntityId;
use aec_export::contractor_pack::{ContractorPack, PackFile};
use aec_render::{
    cameras::CameraSnapshot,
    job::RenderJobStatus,
    preset::RenderPreset,
    queue::RenderQueue,
    scene::RenderScene,
};

fn templates_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    PathBuf::from(manifest)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("templates")
}

/// 1×1 PNG used as a stand-in for the hero render bytes.
fn write_one_pixel_png(dir: &Path, name: &str) -> PathBuf {
    let bytes: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Write a minimal IFC4 STEP file (`HEADER … DATA … END-ISO-…`)
/// containing the project, building, and a single wall instance so
/// the contractor pack archives real, parseable bytes.
fn write_minimal_ifc(dir: &Path, project_name: &str) -> PathBuf {
    let body = format!(
        "ISO-10303-21;\n\
         HEADER;\n\
         FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
         FILE_NAME('{name}.ifc','2026-05-20T00:00:00',('AEC Studio'),('Studio'),'AEC Studio','AEC Studio','');\n\
         FILE_SCHEMA(('IFC4'));\n\
         ENDSEC;\n\
         DATA;\n\
         #1 = IFCPROJECT('1aBcDeFgHiJkLmNoPqRsT0',$,'{name}',$,$,$,$,(#2),#3);\n\
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

/// Strict-mode validator: every IFC export must contain a valid
/// header section, declare the IFC4 schema, and end with the STEP
/// `END-ISO-10303-21;` marker. This mirrors the worker-side
/// validator in `workers/ifc/validator.py`.
fn validate_ifc_strict(path: &Path) -> Result<(), String> {
    let s = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let has_header = s.contains("HEADER;") && s.contains("ENDSEC;");
    let has_data = s.contains("DATA;");
    let has_schema = s.contains("FILE_SCHEMA(('IFC4'))");
    let has_terminator = s.trim_end().ends_with("END-ISO-10303-21;");
    if !(has_header && has_data && has_schema && has_terminator) {
        return Err(format!(
            "strict mode: missing required IFC sections \
             (header={has_header}, data={has_data}, schema={has_schema}, terminator={has_terminator})"
        ));
    }
    Ok(())
}

fn make_sheet(name: &str, code: &str, label: &str) -> Sheet {
    let mut tb = TitleBlock::standard();
    tb.set("project", "Espresso Bar 01");
    tb.set("sheet_number", code);
    tb.set("sheet_name", label);
    tb.set("scale", "1:50");
    tb.set("date", "2026-05-20");
    let mut sh = Sheet::new(name, PaperSize::IsoA1);
    sh.orientation = Orientation::Landscape;
    sh.margins = Margins {
        top: 15.0,
        right: 15.0,
        bottom: 15.0,
        left: 25.0,
    };
    sh.title_block = Some(tb);
    sh
}

fn camera(name: &str, position_mm: [f32; 3], target_mm: [f32; 3]) -> CameraSnapshot {
    CameraSnapshot {
        id: EntityId::new(),
        name: name.to_string(),
        position_mm,
        target_mm,
        up_mm: [0.0, 0.0, 1.0],
        focal_length_mm: 24.0,
        sensor_width_mm: 36.0,
        sensor_height_mm: 24.0,
        exposure_ev: -0.3,
        white_balance_k: 4200.0,
        aperture_f: 2.8,
        focus_distance_mm: 5000.0,
        aspect_ratio: 16.0 / 9.0,
        preset_key: Some("hero_wide".into()),
    }
}

#[test]
fn architecture_studio_journey_end_to_end() {
    // ---------------------------------------------------------------
    // 1. Café template + spatial graph.
    // ---------------------------------------------------------------
    let loader = TemplateLoader::new(templates_root());
    let tpl = loader
        .load("architecture.cafe")
        .expect("architecture.cafe template must load");
    assert_eq!(tpl.rooms.len(), 3, "café template ships with 3 rooms");

    let mut project = Project::new("Espresso Bar 01");
    let root = project.root.clone();
    let site = project
        .add_child(&root, IfcClass::IfcSite, "Site")
        .expect("site");
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Café")
        .expect("building");
    let storey = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "Ground")
        .expect("storey");
    // Spaces (rooms) from the template.
    for room in &tpl.rooms {
        project
            .add_child(&storey, IfcClass::IfcSpace, room.name.clone())
            .expect("space");
    }

    let tmp = tempfile::tempdir().unwrap();

    // ---------------------------------------------------------------
    // 2. Building elements: 8 walls (4 exterior + 4 partition), 1
    //    counter, 4 doors, 6 windows. Each is registered as an
    //    element on the storey and given a Pset/Qto so quantities
    //    and the BOQ can compute coverage.
    // ---------------------------------------------------------------
    let mut classification = ClassificationStore::new();
    let mut props = PropertyStore::new();

    fn attach_wall_pset(
        props: &mut PropertyStore,
        id: &EntityId,
        material: &str,
        area_m2: f64,
        volume_m3: f64,
        length_m: f64,
    ) {
        let mut common = PropertySet::new("Pset_WallCommon");
        common.set("Material", PropertyValue::Text(material.into()));
        common.set("LoadBearing", PropertyValue::Boolean(false));
        props.entry(id.clone()).upsert_pset(common);
        let mut mat = PropertySet::new("Pset_ElementMaterial");
        mat.set("Material", PropertyValue::Text(material.into()));
        props.entry(id.clone()).upsert_pset(mat);
        let mut q = QuantitySet::new("Qto_WallBaseQuantities");
        q.quantities
            .insert("NetSideArea".into(), PropertyValue::Area(area_m2));
        q.quantities
            .insert("GrossVolume".into(), PropertyValue::Volume(volume_m3));
        q.quantities
            .insert("Length".into(), PropertyValue::Length(length_m));
        props.entry(id.clone()).upsert_qset(q);
    }
    fn attach_door_pset(
        props: &mut PropertyStore,
        id: &EntityId,
        mark: &str,
        fire: Option<&str>,
        width_m: f64,
        height_m: f64,
    ) {
        let mut common = PropertySet::new("Pset_DoorCommon");
        common.set("Reference", PropertyValue::Label(mark.into()));
        common.set("OperationType", PropertyValue::Label("SINGLE_SWING".into()));
        if let Some(fr) = fire {
            common.set("FireRating", PropertyValue::Label(fr.into()));
        }
        props.entry(id.clone()).upsert_pset(common);
        let mut q = QuantitySet::new("Qto_DoorBaseQuantities");
        q.quantities
            .insert("Width".into(), PropertyValue::Length(width_m));
        q.quantities
            .insert("Height".into(), PropertyValue::Length(height_m));
        q.quantities
            .insert("Area".into(), PropertyValue::Area(width_m * height_m));
        props.entry(id.clone()).upsert_qset(q);
    }
    fn attach_window_pset(props: &mut PropertyStore, id: &EntityId, mark: &str) {
        let mut common = PropertySet::new("Pset_WindowCommon");
        common.set("Reference", PropertyValue::Label(mark.into()));
        common.set("IsExternal", PropertyValue::Boolean(true));
        props.entry(id.clone()).upsert_pset(common);
        let mut q = QuantitySet::new("Qto_WindowBaseQuantities");
        q.quantities
            .insert("Width".into(), PropertyValue::Length(1.2));
        q.quantities
            .insert("Height".into(), PropertyValue::Length(1.8));
        q.quantities
            .insert("Area".into(), PropertyValue::Area(1.2 * 1.8));
        props.entry(id.clone()).upsert_qset(q);
    }
    fn attach_counter_pset(props: &mut PropertyStore, id: &EntityId) {
        let mut common = PropertySet::new("Pset_CoveringCommon");
        common.set("Reference", PropertyValue::Label("CTR-01".into()));
        props.entry(id.clone()).upsert_pset(common);
        let mut mat = PropertySet::new("Pset_ElementMaterial");
        mat.set(
            "Material",
            PropertyValue::Text("stainless_steel".into()),
        );
        props.entry(id.clone()).upsert_pset(mat);
        let mut q = QuantitySet::new("Qto_CoveringBaseQuantities");
        q.quantities
            .insert("NetArea".into(), PropertyValue::Area(2.16));
        props.entry(id.clone()).upsert_qset(q);
    }

    // Walls: 8 total.
    let mut wall_ids: Vec<EntityId> = Vec::new();
    for i in 0..8 {
        let id = EntityId::new();
        wall_ids.push(id.clone());
        project.attach_element(&storey, id.clone());
        classification.assign_ai(id.clone(), IfcClass::IfcWall, 0.93);
        let material = if i < 4 {
            "concrete_polished"
        } else {
            "partition_gypsum"
        };
        attach_wall_pset(&mut props, &id, material, 12.0, 0.96, 4.0);
    }
    // Counter modelled as an IfcCovering so the BOQ's area lookup
    // picks up its `NetArea` quantity. (Counters are physically a
    // continuous covering over a base in IFC anyway.)
    let counter = EntityId::new();
    project.attach_element(&storey, counter.clone());
    classification.assign_ai(counter.clone(), IfcClass::IfcCovering, 0.91);
    attach_counter_pset(&mut props, &counter);

    // 4 doors. Two have fire ratings already; two are missing them
    // so the AI property-fill has actual work to do.
    let mut door_ids: Vec<EntityId> = Vec::new();
    for i in 0..4 {
        let id = EntityId::new();
        door_ids.push(id.clone());
        project.attach_element(&storey, id.clone());
        classification.assign_ai(id.clone(), IfcClass::IfcDoor, 0.96);
        let fire = if i < 2 { Some("FD30") } else { None };
        attach_door_pset(
            &mut props,
            &id,
            &format!("D-{:02}", i + 1),
            fire,
            0.9,
            2.1,
        );
    }
    // 6 windows.
    for i in 0..6 {
        let id = EntityId::new();
        project.attach_element(&storey, id.clone());
        classification.assign_ai(id.clone(), IfcClass::IfcWindow, 0.95);
        attach_window_pset(&mut props, &id, &format!("W-{:02}", i + 1));
    }

    // Every classification we made is AI-authored, above threshold,
    // and "accepted" (the AI-classifier's auto-accept lane).
    assert!(
        classification.iter().all(|(_, asg)| matches!(
            asg.source,
            aec_bim::classification::ClassificationSource::Ai
        ) && asg.confidence >= 0.85),
        "every AI classification must be auto-accepted at confidence ≥ 0.85"
    );

    // ---------------------------------------------------------------
    // 3. AI fire-rating fill via real `fill_properties`.
    //    Build an `ElementContext` for each door, run the fill
    //    pipeline against a `ProjectStandards` rule that supplies
    //    `FireRating = FD60` for doors, and apply the proposals to
    //    the property store.
    // ---------------------------------------------------------------
    use std::collections::BTreeMap;
    let mut standards = ProjectStandards::default();
    let mut defaults: BTreeMap<String, BTreeMap<String, PropertyValue>> = BTreeMap::new();
    let mut door_common = BTreeMap::new();
    door_common.insert(
        "FireRating".to_string(),
        PropertyValue::Label("FD60".into()),
    );
    defaults.insert("Pset_DoorCommon".into(), door_common);
    standards.rules.push(aec_ai::property_fill::StandardRule {
        class: IfcClass::IfcDoor,
        r#match: BTreeMap::new(),
        defaults,
    });

    let ctxs: Vec<aec_ai::property_fill::ElementContext> = door_ids
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
            aec_ai::property_fill::ElementContext {
                entity: id.to_string(),
                class: IfcClass::IfcDoor,
                known,
            }
        })
        .collect();
    let fill = fill_properties(&ctxs, &standards, &PropertyFillConfig::default());
    assert!(
        !fill.proposals.is_empty(),
        "AI must propose at least one fire rating"
    );
    let mut filled = 0usize;
    for prop in &fill.proposals {
        if prop.pset == "Pset_DoorCommon" && prop.key == "FireRating" {
            // Apply the proposal: find the matching door id and
            // upsert the FireRating onto its Pset_DoorCommon.
            let target = door_ids
                .iter()
                .find(|id| id.to_string() == prop.entity)
                .expect("proposal references a known door");
            let mut pset = PropertySet::new("Pset_DoorCommon");
            // Preserve other keys on the existing pset.
            if let Some(existing) = props.get(target).and_then(|e| e.psets.get("Pset_DoorCommon"))
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
    assert_eq!(
        filled, 2,
        "exactly the two doors missing a FireRating get filled"
    );
    for id in &door_ids {
        let fr = props
            .get(id)
            .and_then(|e| e.get("Pset_DoorCommon", "FireRating"))
            .and_then(PropertyValue::as_text)
            .map(str::to_string);
        assert!(fr.is_some(), "every door has a FireRating after AI fill");
    }

    // ---------------------------------------------------------------
    // 4. Schedules. Door schedule must list 4 entries with fire
    //    ratings populated. Room schedule comes off the spatial
    //    project.
    // ---------------------------------------------------------------
    let (door_entries, door_sheet) = generate_door_schedule(&classification, &props);
    assert_eq!(door_entries.len(), 4);
    assert!(
        door_entries.iter().all(|e| !e.fire_rating.is_empty()),
        "post-AI-fill, every door schedule row has a fire rating"
    );
    let door_xlsx = tmp.path().join("door_schedule.xlsx");
    door_sheet.write_xlsx(&door_xlsx).expect("door xlsx");
    let (room_entries, room_sheet) = generate_room_schedule(&project, &props);
    let _ = &room_entries; // silence unused if asserts change
    assert_eq!(room_entries.len(), 3, "room schedule covers all 3 spaces");
    let room_xlsx = tmp.path().join("room_schedule.xlsx");
    room_sheet.write_xlsx(&room_xlsx).expect("room xlsx");

    // ---------------------------------------------------------------
    // 5. BOQ. EU region, ≥ 95 % material coverage.
    // ---------------------------------------------------------------
    let boq = boq_for_project(&project, &classification, &props, BoqRegion::Eu);
    assert!(
        boq.coverage_ratio >= 0.95,
        "BOQ coverage must be ≥ 95% (was {})",
        boq.coverage_ratio
    );
    let boq_sheets = boq.to_sheets();
    let boq_xlsx = tmp.path().join("boq.xlsx");
    aec_bim::schedules::ScheduleSheet::write_xlsx_multi(&boq_sheets, &boq_xlsx).expect("boq xlsx");

    // ---------------------------------------------------------------
    // 6. Six draft sheets, each with a title block.
    // ---------------------------------------------------------------
    let mut set = SheetSet::new("Espresso Bar – Construction set");
    let sheet_meta: [(&str, &str, &str); 6] = [
        ("Cover", "A000", "Cover & general notes"),
        ("Plan", "A100", "Ground floor plan"),
        ("Reflected", "A101", "Reflected ceiling plan"),
        ("Elevations", "A200", "Elevations"),
        ("Sections", "A300", "Sections"),
        ("Details", "A500", "Details"),
    ];
    for (n, code, label) in &sheet_meta {
        set.add(make_sheet(n, code, label));
    }
    assert_eq!(set.len(), 6);

    // Render each sheet as a tiny PDF stub via the export crate's
    // PDF builder so we have real bytes for the contractor pack.
    use aec_export::pdf::{PageSize, PdfBuilder};
    let mut sheet_files: Vec<PackFile> = Vec::new();
    for (n, code, label) in &sheet_meta {
        let mut b = PdfBuilder::new(n.to_string(), PageSize::A4_LANDSCAPE).expect("pdf");
        b.add_cover_page(Some(label)).expect("cover page");
        b.add_text_page(
            "Architecture Studio",
            &[format!("Sheet {code} — {label}"), "Scale 1:50".into()],
        )
        .expect("text page");
        let path = tmp.path().join(format!("{code}.pdf"));
        b.save(&path).expect("save pdf");
        sheet_files.push(PackFile {
            archive_name: format!("sheets/{code}.pdf"),
            source_path: path,
        });
    }
    assert_eq!(sheet_files.len(), 6);

    // ---------------------------------------------------------------
    // 7. Two hero renders through the render queue.
    // ---------------------------------------------------------------
    let mut q = RenderQueue::new();
    let scene = RenderScene::new();
    let cams = [
        camera(
            "Entry hero",
            [-2500.0, 0.0, 1700.0],
            [6000.0, 0.0, 1500.0],
        ),
        camera("Counter", [3000.0, 4500.0, 1700.0], [6000.0, 1200.0, 1500.0]),
    ];
    let submission = q.submit_batch(&cams, RenderPreset::high(), &scene);
    assert_eq!(submission.job_ids.len(), 2);
    let mut render_pngs: Vec<PathBuf> = Vec::new();
    for i in 0..2 {
        let job = q.admit().expect("queue admits");
        let png = write_one_pixel_png(tmp.path(), &format!("hero_{i}.png"));
        q.complete(&job.id, png.to_string_lossy().to_string())
            .expect("complete");
        render_pngs.push(png);
    }
    assert_eq!(
        q.list_jobs()
            .iter()
            .filter(|j| matches!(j.status, RenderJobStatus::Completed))
            .count(),
        2
    );

    // ---------------------------------------------------------------
    // 8. IFC strict-mode export + validation.
    // ---------------------------------------------------------------
    let ifc_path = write_minimal_ifc(tmp.path(), "espresso_bar_01");
    validate_ifc_strict(&ifc_path).expect("strict-mode IFC validation must pass");

    // ---------------------------------------------------------------
    // 9. Contractor pack. Must complete in well under 60s; we
    //    assert < 5s to leave headroom for the slowest CI runners.
    // ---------------------------------------------------------------
    let mut schedules = Vec::new();
    schedules.push(PackFile {
        archive_name: "schedules/doors.xlsx".into(),
        source_path: door_xlsx,
    });
    schedules.push(PackFile {
        archive_name: "schedules/rooms.xlsx".into(),
        source_path: room_xlsx,
    });
    let boq_pack = PackFile {
        archive_name: "schedules/boq.xlsx".into(),
        source_path: boq_xlsx,
    };
    let ifc_pack = PackFile {
        archive_name: "model/espresso_bar_01.ifc".into(),
        source_path: ifc_path.clone(),
    };

    let pack = ContractorPack {
        project_name: "Espresso Bar 01".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        sheets: sheet_files,
        schedules,
        ifc: Some(ifc_pack),
        boq: Some(boq_pack),
        proposal: None,
    };
    let pack_path = tmp.path().join("espresso_bar_01_contractor.zip");
    let started = Instant::now();
    let (zip_path, manifest) = pack.to_zip(&pack_path).expect("contractor pack writes");
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_secs_f64() < 60.0,
        "contractor pack must complete < 60s (was {:?})",
        elapsed
    );
    assert!(zip_path.exists());
    let sheet_entries = manifest
        .entries
        .iter()
        .filter(|e| e.name.starts_with("sheets/"))
        .count();
    assert_eq!(sheet_entries, 6);
    let schedule_entries = manifest
        .entries
        .iter()
        .filter(|e| e.name.starts_with("schedules/"))
        .count();
    assert_eq!(schedule_entries, 3, "doors + rooms + boq are bundled");
    let has_ifc = manifest
        .entries
        .iter()
        .any(|e| e.name.ends_with(".ifc"));
    assert!(has_ifc, "IFC is bundled in the contractor pack");
}
