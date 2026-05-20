//! Contractor handoff pack export must complete in under 60 seconds.
//!
//! Phase 6 promises the contractor pack ships in under a minute on a
//! reasonable workstation. We exercise the worst-case shape we expect
//! in practice — 12 sheet PDFs, 3 schedule XLSX files, a 1 MB IFC, a
//! BOQ XLSX, and a proposal PDF — then assert the wall-clock time of
//! [`ContractorPack::to_zip`] is well under 60 s.
//!
//! The intent here is *headroom*: 60 s is a customer-visible budget,
//! so we additionally assert against 30 s in CI to catch perf
//! regressions early.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use aec_export::{ContractorPack, PackFile};

/// Hard ceiling from the Phase 6 exit criterion.
const HARD_BUDGET_SECS: f64 = 60.0;
/// CI guardrail — well under the customer budget so a regression
/// shows up before users feel it.
const SOFT_BUDGET_SECS: f64 = 30.0;

fn write_payload(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn contractor_pack_zips_realistic_payload_under_60s() {
    let tmp = tempfile::tempdir().unwrap();

    // 12 sheet PDFs at ~1 KB each. Use a printable PDF magic so the
    // result still looks like a PDF on inspection.
    let mut sheets = Vec::with_capacity(12);
    for i in 0..12 {
        let body = format!("%PDF-1.4 sheet {i}\n{}", "x".repeat(1024));
        let p = write_payload(tmp.path(), &format!("A{i:03}.pdf"), body.as_bytes());
        sheets.push(PackFile {
            archive_name: format!("sheets/A{i:03}.pdf"),
            source_path: p,
        });
    }

    // 3 schedule XLSXs (synthesised — just ZIP magic + filler).
    let mut schedules = Vec::with_capacity(3);
    for name in ["materials", "furniture", "doors"] {
        let body = {
            let mut b = b"PK\x03\x04".to_vec();
            b.extend_from_slice(&vec![b'x'; 32 * 1024]);
            b
        };
        let p = write_payload(tmp.path(), &format!("{name}.xlsx"), &body);
        schedules.push(PackFile {
            archive_name: format!("schedules/{name}.xlsx"),
            source_path: p,
        });
    }

    // 1 MB synthetic IFC payload.
    let mut ifc_body = b"ISO-10303-21;\n".to_vec();
    ifc_body.extend_from_slice(&vec![b'I'; 1024 * 1024]);
    ifc_body.extend_from_slice(b"END-ISO-10303-21;\n");
    let ifc_path = write_payload(tmp.path(), "project.ifc", &ifc_body);

    // BOQ XLSX.
    let mut boq_body = b"PK\x03\x04".to_vec();
    boq_body.extend_from_slice(&vec![b'b'; 16 * 1024]);
    let boq_path = write_payload(tmp.path(), "boq.xlsx", &boq_body);

    // Proposal PDF.
    let proposal_path = write_payload(
        tmp.path(),
        "proposal.pdf",
        &[b"%PDF-1.4 proposal\n".as_ref(), &vec![b'P'; 4 * 1024]].concat(),
    );

    let pack = ContractorPack {
        project_name: "Perf Project".into(),
        app_version: "0.1.0".into(),
        sheets,
        schedules,
        ifc: Some(PackFile {
            archive_name: "model/project.ifc".into(),
            source_path: ifc_path,
        }),
        boq: Some(PackFile {
            archive_name: "schedules/boq.xlsx".into(),
            source_path: boq_path,
        }),
        proposal: Some(PackFile {
            archive_name: "proposal.pdf".into(),
            source_path: proposal_path,
        }),
    };

    let out = tmp.path().join("contractor_pack.zip");
    let start = Instant::now();
    let (created, manifest) = pack.to_zip(&out).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    eprintln!("contractor_pack export: {elapsed:.3}s");

    assert_eq!(created, out, "to_zip should return the requested path");
    assert!(out.exists(), "archive should exist on disk");
    assert!(out.metadata().unwrap().len() > 1024, "archive too small");

    // Manifest should account for every payload: 12 sheets + 3
    // schedules + ifc + boq + proposal = 18.
    assert_eq!(manifest.entries.len(), 12 + 3 + 1 + 1 + 1);

    assert!(
        elapsed < HARD_BUDGET_SECS,
        "contractor pack export exceeded hard 60s budget: {elapsed:.3}s"
    );
    assert!(
        elapsed < SOFT_BUDGET_SECS,
        "contractor pack export exceeded soft CI budget of {SOFT_BUDGET_SECS}s: {elapsed:.3}s"
    );
}
