//! Phase 6 end-to-end exit criteria.
//!
//! 1. A single `.aecstudio` project produces all four delivery types.
//! 2. Revisions can be diffed at project / sheet / element level.
//! 3. Contractor handoff pack export completes in under 60 s.
//! 4. All exports are deterministic.
//!
//! (2) is covered in `aec_core::version_diff::tests` and (3)/(4) have
//! dedicated test files (`contractor_perf.rs`, `determinism.rs`). This
//! file covers (1) — building a Concept pack, an Interior pack, a
//! Contractor pack, and a BIM pack from one set of project inputs.

use std::fs;
use std::path::PathBuf;

use aec_export::{
    BimPack, ContractorPack, InteriorPack, InteriorRender, PackFile, ProposalAssets, ProposalPack,
    ScheduleSheet, ValidationReport, ValidationReportKind,
};

fn write_payload(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn single_project_produces_concept_interior_contractor_bim_packs() {
    let tmp = tempfile::tempdir().unwrap();

    // ── Shared project payloads (the .aecstudio project source) ──
    let sheet = write_payload(tmp.path(), "A100.pdf", b"%PDF-1.4 sheet bytes");
    let ifc = write_payload(
        tmp.path(),
        "project.ifc",
        b"ISO-10303-21;\nproject\nEND-ISO-10303-21;",
    );
    let render = write_payload(tmp.path(), "render-1.png", b"\x89PNG\r\n\x1a\n synthetic");
    let materials = ScheduleSheet::material_schedule_template();

    // ── 1. Concept pack (ProposalPack PDF) ──
    let mut concept = ProposalPack::new("Apartment 12B", "Ms. K");
    concept.assets = ProposalAssets {
        mood_board: vec!["Warm scandi palette".into()],
        plan_overview: vec!["Open-plan kitchen/living".into()],
        ..Default::default()
    };
    let concept_path = concept.to_pdf(tmp.path().join("concept.pdf")).unwrap();
    assert!(concept_path.exists());
    assert!(concept_path.metadata().unwrap().len() > 1024);

    // ── 2. Interior pack (summary + renders + material sched) ──
    let interior = InteriorPack {
        project_name: "Apartment 12B".into(),
        summary_pdf_bytes: fs::read(&concept_path).unwrap(),
        renders: vec![InteriorRender {
            label: "Hero camera".into(),
            source_path: render.clone(),
        }],
        material_schedule: materials.clone(),
    };
    let interior_out = tmp.path().join("interior.zip");
    let (interior_path, _interior_manifest) = interior.to_zip(&interior_out).unwrap();
    assert!(interior_path.exists());

    // ── 3. Contractor pack (sheets + schedules + ifc) ──
    let mut schedule_path = tmp.path().join("materials.xlsx");
    materials.to_xlsx(&schedule_path).unwrap();
    schedule_path = schedule_path.canonicalize().unwrap();

    let contractor = ContractorPack {
        project_name: "Apartment 12B".into(),
        app_version: "0.1.0".into(),
        sheets: vec![PackFile {
            archive_name: "sheets/A100.pdf".into(),
            source_path: sheet.clone(),
        }],
        schedules: vec![PackFile {
            archive_name: "schedules/materials.xlsx".into(),
            source_path: schedule_path,
        }],
        ifc: Some(PackFile {
            archive_name: "model/project.ifc".into(),
            source_path: ifc.clone(),
        }),
        boq: None,
        proposal: Some(PackFile {
            archive_name: "proposal.pdf".into(),
            source_path: concept_path.clone(),
        }),
    };
    let contractor_out = tmp.path().join("contractor.zip");
    let (contractor_path, contractor_manifest) = contractor.to_zip(&contractor_out).unwrap();
    assert!(contractor_path.exists());
    assert!(contractor_manifest.entries.len() >= 4);

    // ── 4. BIM pack (IFC + sheets + validation report) ──
    let bim = BimPack {
        project_name: "Apartment 12B".into(),
        ifc: PackFile {
            archive_name: "project.ifc".into(),
            source_path: ifc,
        },
        sheets: vec![PackFile {
            archive_name: "sheets/A100.pdf".into(),
            source_path: sheet,
        }],
        validation_report: ValidationReport {
            kind: ValidationReportKind::Text,
            bytes: b"PASS - no issues found\n".to_vec(),
        },
    };
    let bim_out = tmp.path().join("bim.zip");
    let bim_path = bim.to_zip(&bim_out).unwrap();
    assert!(bim_path.exists());

    // All four artefacts must coexist on disk.
    for p in [&concept_path, &interior_path, &contractor_path, &bim_path] {
        let meta = fs::metadata(p).expect("artefact must exist");
        assert!(meta.len() > 0, "artefact at {} is empty", p.display());
    }
}
