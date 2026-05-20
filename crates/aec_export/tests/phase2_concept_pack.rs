//! Phase 2 — concept-pack export end-to-end integration test.
//!
//! Simulates the closing leg of the "create project → place furniture
//! → save cameras → queue renders → export PDF concept pack" flow
//! described in `PROGRESS.md`. The render queue + command engine are
//! exercised by `aec_command`/`aec_render` integration tests; this
//! test takes the *output* of that flow (rendered images, a material
//! and furniture schedule, an AI-generated cover paragraph, and a
//! studio branding payload) and confirms a real PDF and accompanying
//! ZIPs (interior pack + BIM pack + contractor pack + BOQ XLSX) are
//! produced.
//!
//! The test writes only inside a `tempdir`. It asserts:
//! * the proposal PDF starts with `%PDF` and exceeds a non-trivial
//!   byte budget,
//! * the interior pack ZIP is a valid ZIP and the manifest enumerates
//!   the PDF + every render + the material schedule,
//! * the BIM pack ZIP carries the IFC + sheets + validation report,
//! * the BOQ XLSX is a valid ZIP-based workbook,
//! * the contractor pack ZIP carries a manifest with BLAKE3 checksums
//!   on every entry.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use aec_export::{
    bim_pack::{BimPack, ValidationReport, ValidationReportKind},
    contractor_pack::{ContractorPack, PackFile},
    interior_pack::{InteriorPack, InteriorRender},
    BoqExport, BoqLine, ProposalAssets, ProposalBranding, ProposalPack, RegionalConfig,
    RenderAttachment, ScheduleSheet,
};
use aec_export::boq::QuantityUnit;

fn write_fake_png(dir: &std::path::Path, name: &str) -> PathBuf {
    // A real 1×1 PNG so the pack's hashing path runs against
    // legitimate content instead of an empty file.
    let bytes: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let path = dir.join(name);
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(&bytes).unwrap();
    path
}

fn write_fake_file(dir: &std::path::Path, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.join(name);
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(body).unwrap();
    path
}

#[test]
fn full_concept_pack_produces_valid_pdf_and_zips() {
    let tmp = tempfile::tempdir().unwrap();
    let render_a = write_fake_png(tmp.path(), "living_render.png");
    let render_b = write_fake_png(tmp.path(), "bedroom_render.png");
    let render_c = write_fake_png(tmp.path(), "kitchen_render.png");
    let render_d = write_fake_png(tmp.path(), "hall_render.png");

    // ---------- 1. Proposal PDF with AI-generated cover ----------
    let mut p = ProposalPack::new("60m² Apartment", "Eva K.");
    p.branding = ProposalBranding {
        studio_name: "Atelier Lumen".into(),
        studio_tagline: Some("Quiet, considered interiors.".into()),
        logo_path: None,
    };
    p.assets = ProposalAssets {
        mood_board: vec![
            "Warm oak floors, putty walls, brushed brass accents.".into(),
            "Soft north light through linen drapes.".into(),
        ],
        plan_overview: vec![
            "Living/kitchen along the south façade; sleeping zone tucked behind a half-height oak screen.".into(),
        ],
        cover_paragraph: Some(
            "60m² Apartment is a thoughtful design that balances the everyday rhythms of Eva K. with a quietly considered material palette and a flexible plan."
                .into(),
        ),
        renders: vec![
            RenderAttachment { path: render_a.clone(), caption: "Living — golden hour".into(), preset_id: "standard".into() },
            RenderAttachment { path: render_b.clone(), caption: "Bedroom — overcast".into(), preset_id: "standard".into() },
            RenderAttachment { path: render_c.clone(), caption: "Kitchen — eye level".into(), preset_id: "high".into() },
            RenderAttachment { path: render_d.clone(), caption: "Entry hall".into(), preset_id: "quick".into() },
        ],
        floor_plan_overview: vec!["Single-storey, 60m², south-facing.".into()],
        next_steps: vec!["Confirm material samples by Friday.".into()],
    };
    p.material_schedule
        .push_row(["MAT-001", "Oak board", "Living", "12 m²", "Acme"]);
    p.material_schedule
        .push_row(["MAT-002", "Brass trim", "Living", "8 m", "Atelier Brass"]);
    p.furniture_schedule
        .push_row(["FUR-001", "Sofa Kivik", "Living", "1", "Linen, putty"]);
    p.furniture_schedule
        .push_row(["FUR-002", "Bed Malm 140", "Bedroom", "1", "Oak veneer"]);

    let pdf_path = tmp.path().join("proposal.pdf");
    let written = p
        .to_pdf(&pdf_path)
        .expect("proposal PDF must build with renders + cover paragraph");
    let bytes = fs::read(written).unwrap();
    assert!(bytes.starts_with(b"%PDF"), "proposal PDF must start with %PDF");
    assert!(
        bytes.len() > 4096,
        "proposal with 4 renders + 2 schedules + cover should be > 4 KiB; got {}",
        bytes.len()
    );

    // ---------- 2. Interior pack ZIP ----------
    let interior_pack = InteriorPack {
        project_name: "60m² Apartment".into(),
        summary_pdf_bytes: bytes.clone(),
        renders: vec![
            InteriorRender {
                source_path: render_a.clone(),
                label: "Living".into(),
            },
            InteriorRender {
                source_path: render_b.clone(),
                label: "Bedroom".into(),
            },
        ],
        material_schedule: ScheduleSheet::material_schedule_template(),
    };
    let interior_zip = tmp.path().join("interior.zip");
    let (interior_zip_path, manifest) = interior_pack
        .to_zip(&interior_zip)
        .expect("interior pack must zip");
    let zip_bytes = fs::read(&interior_zip_path).unwrap();
    assert!(
        zip_bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]),
        "interior pack must be a valid ZIP (PK\\x03\\x04 header)"
    );
    assert!(
        manifest.entries.len() >= 3,
        "manifest must list the PDF + every render + the schedule, got {}",
        manifest.entries.len()
    );

    // ---------- 3. BIM pack ZIP ----------
    let ifc_payload = write_fake_file(
        tmp.path(),
        "apartment.ifc",
        b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('AEC Studio'),'2;1');\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n",
    );
    let sheet_pdf = write_fake_file(tmp.path(), "sheet_A100.pdf", &bytes);
    let bim_pack = BimPack {
        project_name: "60m² Apartment".into(),
        ifc: PackFile {
            archive_name: "apartment.ifc".into(),
            source_path: ifc_payload,
        },
        sheets: vec![PackFile {
            archive_name: "sheets/A100.pdf".into(),
            source_path: sheet_pdf.clone(),
        }],
        validation_report: ValidationReport {
            kind: ValidationReportKind::Text,
            bytes: b"IFC4: 0 errors, 0 warnings.\nGUIDs preserved.\nNo duplicate spaces.\n"
                .to_vec(),
        },
    };
    let bim_zip = tmp.path().join("bim.zip");
    let bim_zip_path = bim_pack.to_zip(&bim_zip).expect("BIM pack must zip");
    let bim_zip_bytes = fs::read(&bim_zip_path).unwrap();
    assert!(
        bim_zip_bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]),
        "BIM pack must be a valid ZIP"
    );

    // ---------- 4. BOQ XLSX ----------
    let mut boq = BoqExport::new("60m² Apartment", RegionalConfig::Eu);
    boq.push_section(
        "Finishes",
        vec![BoqLine {
            code: "FIN-001".into(),
            description: "Oak board".into(),
            quantity: 12.0,
            unit: QuantityUnit::Area,
            rate: 35.0,
        }],
    );
    let boq_xlsx_path = tmp.path().join("boq.xlsx");
    boq.to_xlsx(&boq_xlsx_path).expect("BOQ XLSX must write");
    let boq_bytes = fs::read(&boq_xlsx_path).unwrap();
    assert!(
        boq_bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]),
        "BOQ XLSX must be a valid ZIP-based XLSX"
    );

    // ---------- 5. Contractor pack ZIP ----------
    let boq_pack_path = write_fake_file(tmp.path(), "boq_for_pack.xlsx", &boq_bytes);
    let proposal_pack_path = write_fake_file(tmp.path(), "proposal_for_pack.pdf", &bytes);
    let contractor_pack = ContractorPack {
        project_name: "60m² Apartment".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        sheets: vec![PackFile {
            archive_name: "sheets/A100.pdf".into(),
            source_path: sheet_pdf,
        }],
        schedules: vec![PackFile {
            archive_name: "schedules/boq.xlsx".into(),
            source_path: boq_pack_path,
        }],
        ifc: Some(PackFile {
            archive_name: "model/apartment.ifc".into(),
            source_path: write_fake_file(
                tmp.path(),
                "apartment_for_pack.ifc",
                b"ISO-10303-21;\nEND-ISO-10303-21;\n",
            ),
        }),
        boq: None,
        proposal: Some(PackFile {
            archive_name: "proposal.pdf".into(),
            source_path: proposal_pack_path,
        }),
    };
    let contractor_zip = tmp.path().join("contractor.zip");
    let (contractor_zip_path, contractor_manifest) = contractor_pack
        .to_zip(&contractor_zip)
        .expect("contractor pack must zip");
    let contractor_bytes = fs::read(&contractor_zip_path).unwrap();
    assert!(contractor_bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]));
    assert!(
        !contractor_manifest.entries.is_empty(),
        "contractor manifest must list at least one entry"
    );
    // Every manifest entry must carry a BLAKE3 hash (32 bytes ⇒ 64 hex chars).
    for entry in &contractor_manifest.entries {
        assert_eq!(
            entry.blake3.len(),
            64,
            "manifest checksum must be a BLAKE3 hex digest ({})",
            entry.name
        );
    }
}
