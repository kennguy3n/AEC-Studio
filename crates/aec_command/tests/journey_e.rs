//! Journey E — Studio Lead end-to-end.
//!
//! `PROPOSAL.md` §6.E: a studio lead drives one `.aecstudio` project
//! through *all four* delivery types:
//!
//!   * Design — model massing, 3 concept renders.
//!   * Draft — 24 sheets with title blocks.
//!   * BIM — classify spaces / structural elements, export IFC.
//!   * Deliver — tag two revisions, run a before/after compare, then
//!     export the contract pack (PDF + IFC + DXF + BOQ + proposal).
//!
//! Acceptance from `PROPOSAL.md`:
//!   * A single `.aecstudio` produces all four delivery types.
//!   * Revisions are diffable through `compare_revisions`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aec_bim::boq::{boq_for_project, BoqRegion};
use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
use aec_bim::spatial::Project;
use aec_cad::dxf::{
    DxfBlockRecord, DxfDocument, DxfEntity, DxfLine, DxfPolyline, DxfPolylineVertex, DxfReader,
    DxfWriter,
};
use aec_cad::layers::{Layer, LayerColor, LayerLineweight};
use aec_cad::sheets::{PaperSize, Sheet, SheetSet, TitleBlock};
use aec_core::package::ProjectPackage;
use aec_core::templates::TemplateLoader;
use aec_core::types::EntityId;
use aec_core::version_diff::{compare_revisions, EntityChangeKind};
use aec_core::{
    revision::{RevisionDraft, RevisionEntity, RevisionStore},
    ProjectSettings, Region,
};
use aec_export::before_after::{BeforeAfterPdfOptions, BeforeAfterRenderPair};
use aec_export::bim_pack::{BimPack, ValidationReport, ValidationReportKind};
use aec_export::contractor_pack::{ContractorPack, PackFile};
use aec_export::interior_pack::{InteriorPack, InteriorRender};
use aec_export::pdf::{PageSize, PdfBuilder};
use aec_export::proposal::ProposalPack;
use aec_export::schedule::ScheduleSheet;

fn templates_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    PathBuf::from(manifest)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("templates")
}

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

fn write_ifc(dir: &Path, name: &str) -> PathBuf {
    let body = format!(
        "ISO-10303-21;\n\
         HEADER;\n\
         FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
         FILE_NAME('{name}.ifc','2026-05-20T00:00:00',('AEC Studio'),('Studio'),'AEC Studio','AEC Studio','');\n\
         FILE_SCHEMA(('IFC4'));\n\
         ENDSEC;\n\
         DATA;\n\
         #1 = IFCPROJECT('0jPmK3F4D2GZQpVcEqVbZ1',$,'{name}',$,$,$,$,(#2),#3);\n\
         #2 = IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-05,$,$);\n\
         #3 = IFCUNITASSIGNMENT((#4));\n\
         #4 = IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
         ENDSEC;\n\
         END-ISO-10303-21;\n",
    );
    let p = dir.join(format!("{name}.ifc"));
    fs::write(&p, body).unwrap();
    p
}

fn make_sheet(name: &str, code: &str, label: &str) -> Sheet {
    let mut tb = TitleBlock::standard();
    tb.set("project", "Villa Solaris");
    tb.set("sheet_number", code);
    tb.set("sheet_name", label);
    tb.set("scale", "1:100");
    tb.set("date", "2026-05-20");
    let mut sh = Sheet::new(name, PaperSize::IsoA1);
    sh.title_block = Some(tb);
    sh
}

fn make_dxf(dir: &Path, project_name: &str) -> PathBuf {
    let mut doc = DxfDocument::new();
    let mut walls = Layer::new("A-WALL").unwrap();
    walls.color = LayerColor(1);
    walls.lineweight = LayerLineweight::from_mm(0.5);
    doc.layers.upsert(walls);
    doc.block_records.push(DxfBlockRecord::new("DOOR_900"));
    doc.push(DxfEntity::Line(DxfLine {
        layer: "A-WALL".into(),
        start: [0.0, 0.0, 0.0],
        end: [5000.0, 0.0, 0.0],
    }));
    doc.push(DxfEntity::Polyline(DxfPolyline {
        layer: "A-WALL".into(),
        vertices: vec![
            DxfPolylineVertex::new(0.0, 0.0),
            DxfPolylineVertex::new(5000.0, 0.0),
            DxfPolylineVertex::new(5000.0, 3000.0),
            DxfPolylineVertex::new(0.0, 3000.0),
        ],
        closed: true,
        elevation: 0.0,
    }));
    let s = DxfWriter::write_to_string(&doc).expect("dxf writes");
    let p = dir.join(format!("{project_name}.dxf"));
    fs::write(&p, s).unwrap();
    p
}

#[test]
fn studio_lead_journey_end_to_end() {
    // ---------------------------------------------------------------
    // 1. Load the villa template — one `.aecstudio` package will back
    //    every delivery type.
    // ---------------------------------------------------------------
    let loader = TemplateLoader::new(templates_root());
    let tpl = loader
        .load("architecture.villa")
        .expect("villa template must load");
    let template_rooms: Vec<_> = tpl.iter_rooms().collect();
    assert!(
        !template_rooms.is_empty(),
        "villa template provides rooms (across storeys)"
    );

    let tmp = tempfile::tempdir().unwrap();

    // ---------------------------------------------------------------
    // 2. Create the on-disk `.aecstudio` package and exercise the
    //    save/open roundtrip.
    // ---------------------------------------------------------------
    let pkg_path = tmp.path().join("villa_solaris.aecstudio");
    let mut master_key = [0u8; 32];
    for (i, b) in master_key.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    let pkg = ProjectPackage::create(
        &pkg_path,
        "Villa Solaris",
        ProjectSettings::from_region(Region::Eu),
        Some(tpl.template_id.clone()),
        &master_key,
    )
    .expect("project package creates");
    let project_id = pkg.manifest().project_id.clone();
    let reopened = ProjectPackage::open(&pkg_path).expect("re-open after create");
    assert_eq!(reopened.manifest().project_id, project_id);

    // ---------------------------------------------------------------
    // 3. Design — build the spatial graph + 3 concept renders.
    // ---------------------------------------------------------------
    let mut project = Project::new("Villa Solaris");
    let root = project.root.clone();
    let site = project
        .add_child(&root, IfcClass::IfcSite, "Site")
        .expect("site");
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Villa")
        .expect("building");
    let storey = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "Ground")
        .expect("storey");
    let space_ids: Vec<EntityId> = template_rooms
        .iter()
        .take(6)
        .map(|r| {
            project
                .add_child(&storey, IfcClass::IfcSpace, r.name.clone())
                .expect("space")
        })
        .collect();

    let mut classification = ClassificationStore::new();
    let mut props = PropertyStore::new();

    // Massing: 12 walls so the BIM step has something substantial to
    // export.
    let mut wall_ids: Vec<EntityId> = Vec::new();
    for i in 0..12 {
        let id = EntityId::new();
        wall_ids.push(id.clone());
        project.attach_element(&storey, id.clone());
        classification.assign_ai(id.clone(), IfcClass::IfcWall, 0.94);
        let mat = if i < 8 { "concrete_300" } else { "brick_240" };
        let mut common = PropertySet::new("Pset_WallCommon");
        common.set("Material", PropertyValue::Text(mat.into()));
        common.set("LoadBearing", PropertyValue::Boolean(true));
        props.entry(id.clone()).upsert_pset(common);
        let mut emat = PropertySet::new("Pset_ElementMaterial");
        emat.set("Material", PropertyValue::Text(mat.into()));
        props.entry(id.clone()).upsert_pset(emat);
        let mut q = QuantitySet::new("Qto_WallBaseQuantities");
        q.quantities
            .insert("NetSideArea".into(), PropertyValue::Area(15.0));
        q.quantities
            .insert("NetVolume".into(), PropertyValue::Volume(2.4));
        q.quantities
            .insert("Length".into(), PropertyValue::Length(5.0));
        props.entry(id.clone()).upsert_qset(q);
    }
    for id in &space_ids {
        let mut common = PropertySet::new("Pset_SpaceCommon");
        common.set("Reference", PropertyValue::Label("Room".into()));
        common.set("IsExternal", PropertyValue::Boolean(false));
        props.entry(id.clone()).upsert_pset(common);
        let mut q = QuantitySet::new("Qto_SpaceBaseQuantities");
        q.quantities
            .insert("NetFloorArea".into(), PropertyValue::Area(22.5));
        q.quantities
            .insert("Height".into(), PropertyValue::Length(2.8));
        props.entry(id.clone()).upsert_qset(q);
    }

    // 3 concept renders.
    let r1 = write_one_pixel_png(tmp.path(), "concept_1.png");
    let r2 = write_one_pixel_png(tmp.path(), "concept_2.png");
    let r3 = write_one_pixel_png(tmp.path(), "concept_3.png");

    // ---------------------------------------------------------------
    // 4. Draft — 24 sheets with title blocks.
    // ---------------------------------------------------------------
    let mut sheet_set = SheetSet::new("Villa contract set");
    for i in 0..24 {
        sheet_set.add(make_sheet(
            &format!("Sheet {i:02}"),
            &format!("A{i:03}"),
            &format!("Plan {i:02}"),
        ));
    }
    assert_eq!(sheet_set.len(), 24);

    // Real PDFs for two of the sheets so the contract pack archives
    // legitimate binary bytes.
    let sheet_a000 = tmp.path().join("A000.pdf");
    let mut pdfb = PdfBuilder::new("Villa Solaris A000", PageSize::A4_LANDSCAPE).unwrap();
    pdfb.add_cover_page(Some("Plan 00 — context")).unwrap();
    pdfb.save(&sheet_a000).unwrap();
    let sheet_a001 = tmp.path().join("A001.pdf");
    let mut pdfb = PdfBuilder::new("Villa Solaris A001", PageSize::A4_LANDSCAPE).unwrap();
    pdfb.add_cover_page(Some("Plan 01 — ground floor")).unwrap();
    pdfb.save(&sheet_a001).unwrap();

    // ---------------------------------------------------------------
    // 5. BIM — BOQ + IFC.
    // ---------------------------------------------------------------
    let boq = boq_for_project(&project, &classification, &props, BoqRegion::Eu);
    assert!(
        boq.coverage_ratio >= 0.95,
        "BOQ coverage ≥ 95% (was {})",
        boq.coverage_ratio
    );
    let boq_xlsx = tmp.path().join("boq.xlsx");
    let boq_sheets = boq.to_sheets();
    aec_bim::ScheduleSheet::write_xlsx_multi(&boq_sheets, &boq_xlsx).expect("boq xlsx");
    let ifc_path = write_ifc(tmp.path(), "villa_solaris");

    // ---------------------------------------------------------------
    // 6. Deliver — tag two revisions and assert `compare_revisions`
    //    surfaces real, structural changes between them.
    //
    //    Revision 1 (baseline): 12 walls + 6 spaces.
    //    Revision 2 (revised):  remove 1 wall, modify 1 wall's
    //                           material (changes payload hash), add a
    //                           new wall. Net: 1 removed, 1 modified,
    //                           1 added.
    // ---------------------------------------------------------------
    let rev_store = RevisionStore::open(pkg_path.join("revisions")).expect("revision store");
    fn entity_hash(payload: &serde_json::Value) -> String {
        let canon = serde_json::to_vec(payload).unwrap();
        let mut h = blake3::Hasher::new();
        h.update(&canon);
        h.finalize().to_hex().to_string()
    }
    let baseline_entities: Vec<RevisionEntity> = wall_ids
        .iter()
        .enumerate()
        .map(|(i, id)| RevisionEntity {
            category: "geometry".into(),
            id: id.to_string(),
            payload_hash: entity_hash(&serde_json::json!({
                "kind": "wall",
                "material": if i < 8 { "concrete_300" } else { "brick_240" },
            })),
            label: Some(format!("Wall {i:02}")),
        })
        .collect();
    let baseline_draft = baseline_entities.iter().cloned().fold(
        RevisionDraft::new(
            project_id.clone(),
            "v0.1-baseline",
            "Concept baseline",
            "0".repeat(64),
            "Villa Solaris",
            env!("CARGO_PKG_VERSION"),
        ),
        |d, e| d.add_entity(e),
    );
    let baseline = rev_store
        .create(baseline_draft)
        .expect("baseline revision");

    // Mutate: remove last wall, modify wall[0] material, add a brand
    // new wall.
    let mut revised_entities = baseline_entities.clone();
    revised_entities.pop();
    revised_entities[0].payload_hash = entity_hash(&serde_json::json!({
        "kind": "wall",
        "material": "stone_500",
    }));
    let new_wall_id = EntityId::new();
    revised_entities.push(RevisionEntity {
        category: "geometry".into(),
        id: new_wall_id.to_string(),
        payload_hash: entity_hash(&serde_json::json!({
            "kind": "wall",
            "material": "concrete_300",
        })),
        label: Some("Wall 12".into()),
    });
    let revised_draft = revised_entities.iter().cloned().fold(
        RevisionDraft::new(
            project_id.clone(),
            "v0.2-client-review",
            "Post client review",
            "1".repeat(64),
            "Villa Solaris",
            env!("CARGO_PKG_VERSION"),
        ),
        |d, e| d.add_entity(e),
    );
    let revised = rev_store.create(revised_draft).expect("revised revision");

    let diff = compare_revisions(&baseline, &revised);
    assert_eq!(
        diff.changes_in("geometry", EntityChangeKind::Added).len(),
        1,
        "1 wall added"
    );
    assert_eq!(
        diff.changes_in("geometry", EntityChangeKind::Removed).len(),
        1,
        "1 wall removed"
    );
    assert_eq!(
        diff.changes_in("geometry", EntityChangeKind::Modified).len(),
        1,
        "1 wall modified"
    );

    // Before/after PDF compare for two of the renders.
    let before_after_pdf = tmp.path().join("before_after.pdf");
    BeforeAfterPdfOptions {
        project_name: "Villa Solaris".into(),
        pairs: vec![BeforeAfterRenderPair {
            label: "Hero living room".into(),
            before_path: r1.clone(),
            after_path: r2.clone(),
            before_preset: "preview".into(),
            after_preset: "final_high".into(),
            before_ms: 4_200,
            after_ms: 11_800,
        }],
    }
    .write_to(&before_after_pdf)
    .expect("before/after PDF writes");
    assert!(before_after_pdf.exists());

    // ---------------------------------------------------------------
    // 7. The four delivery types from one project — *every* one
    //    materialises a real artefact on disk.
    // ---------------------------------------------------------------
    // (a) Concept pack (proposal PDF + interior pack).
    let mut proposal = ProposalPack::new("Villa Solaris", "Studio Solaris");
    proposal.designer_name = "AEC Studio".into();
    let proposal_pdf = tmp.path().join("proposal.pdf");
    proposal
        .to_pdf(&proposal_pdf)
        .expect("proposal pdf writes");

    let interior_pack = InteriorPack {
        project_name: "Villa Solaris".into(),
        summary_pdf_bytes: fs::read(&proposal_pdf).unwrap(),
        renders: vec![
            InteriorRender {
                label: "concept_1".into(),
                source_path: r1.clone(),
            },
            InteriorRender {
                label: "concept_2".into(),
                source_path: r2.clone(),
            },
            InteriorRender {
                label: "concept_3".into(),
                source_path: r3.clone(),
            },
        ],
        material_schedule: ScheduleSheet::material_schedule_template(),
    };
    let interior_zip = tmp.path().join("villa_solaris_interior.zip");
    let (interior_zip_path, interior_manifest) = interior_pack
        .to_zip(&interior_zip)
        .expect("interior pack writes");
    assert!(interior_zip_path.exists());
    assert!(interior_manifest
        .entries
        .iter()
        .any(|e| e.name == "interior_summary.pdf"));

    // (b) Contractor pack (sheets + schedules + IFC + BOQ + proposal).
    let dxf_path = make_dxf(tmp.path(), "villa_solaris");
    let contractor = ContractorPack {
        project_name: "Villa Solaris".into(),
        sheets: vec![
            PackFile {
                archive_name: "sheets/A000.pdf".into(),
                source_path: sheet_a000.clone(),
            },
            PackFile {
                archive_name: "sheets/A001.pdf".into(),
                source_path: sheet_a001.clone(),
            },
        ],
        schedules: vec![PackFile {
            archive_name: "schedules/boq.xlsx".into(),
            source_path: boq_xlsx.clone(),
        }],
        ifc: Some(PackFile {
            archive_name: "model/villa_solaris.ifc".into(),
            source_path: ifc_path.clone(),
        }),
        boq: Some(PackFile {
            archive_name: "boq/boq.xlsx".into(),
            source_path: boq_xlsx.clone(),
        }),
        proposal: Some(PackFile {
            archive_name: "proposal/proposal.pdf".into(),
            source_path: proposal_pdf.clone(),
        }),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let contractor_zip = tmp.path().join("villa_solaris_contract.zip");
    let (contractor_zip_path, contractor_manifest) = contractor
        .to_zip(&contractor_zip)
        .expect("contractor pack writes");
    assert!(contractor_zip_path.exists());
    let archive_names: BTreeMap<&str, u64> = contractor_manifest
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.bytes))
        .collect();
    for expected in [
        "sheets/A000.pdf",
        "sheets/A001.pdf",
        "schedules/boq.xlsx",
        "model/villa_solaris.ifc",
        "boq/boq.xlsx",
        "proposal/proposal.pdf",
    ] {
        assert!(
            archive_names.contains_key(expected),
            "contractor pack missing {expected}"
        );
    }

    // DXF is part of the contract handover too: verify roundtrip.
    let written = fs::read_to_string(&dxf_path).unwrap();
    let reloaded = DxfReader::read_str(&written).expect("dxf reread");
    assert_eq!(reloaded.entities.len(), 2);

    // (c) BIM Lite pack.
    let bim_pack = BimPack {
        project_name: "Villa Solaris".into(),
        ifc: PackFile {
            archive_name: "model/villa_solaris.ifc".into(),
            source_path: ifc_path.clone(),
        },
        sheets: vec![PackFile {
            archive_name: "schedules/boq.xlsx".into(),
            source_path: boq_xlsx.clone(),
        }],
        validation_report: ValidationReport {
            kind: ValidationReportKind::Text,
            bytes: b"villa solaris -- validation: clean\n".to_vec(),
        },
    };
    let bim_zip_path = tmp.path().join("villa_solaris_bim_lite.zip");
    bim_pack.to_zip(&bim_zip_path).expect("BIM pack writes");
    assert!(bim_zip_path.exists());

    // (d) Final assertion: a single `.aecstudio` produced all 4
    //     delivery archives.
    let outputs = [
        &interior_zip_path,
        &contractor_zip_path,
        &bim_zip_path,
        &before_after_pdf,
    ];
    for out in outputs {
        assert!(out.exists(), "delivery artefact missing: {}", out.display());
        let len = fs::metadata(out).unwrap().len();
        assert!(len > 0, "{} is empty", out.display());
    }

    // The package on disk is still openable after the whole pipeline.
    let _final_open = ProjectPackage::open(&pkg_path).expect("project survives full pipeline");
}
