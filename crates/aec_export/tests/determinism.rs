//! Determinism contract for client deliverables.
//!
//! Per the Phase 6 exit criteria ("All exports are deterministic"),
//! building the same `ProposalPack`, `ScheduleSheet`, or `ContractorPack`
//! from identical inputs must produce identical artefacts. Bit-for-bit
//! reproducibility is hard for PDF/ZIP because they embed creation
//! timestamps, so we treat *content* determinism as the contract:
//!
//! * `ProposalPack::to_pdf` → text streams identical across runs.
//! * `ScheduleSheet::to_xlsx` → workbook payload identical.
//! * `ContractorPack::to_zip` → manifest BLAKE3 hashes identical.
//!
//! All three roundtrips are exercised below.

use std::io::Read;

use aec_export::{
    ContractorPack, PackFile, ProposalAssets, ProposalBranding, ProposalPack, ScheduleSheet,
};

/// Strip the printpdf-generated XMP / creation-date / mod-date
/// segments from a PDF byte stream so two PDFs that differ only in
/// timestamps compare equal. Specifically we cut out:
///
/// * everything inside `<?xpacket ... ?>` markers (the XMP packet
///   carries `<xmp:CreateDate>` and `<xmp:ModifyDate>`),
/// * any PDF date literal of the form `(D:YYYYMMDDHHMMSS...)`,
/// * any `/ID [<hex> <hex>]` trailer entry (printpdf seeds its
///   document ID from the wall clock).
///
/// Whitespace and byte offsets in the cross-reference table also vary
/// between runs because the byte-offsets of objects shift when the
/// XMP and ID change, so we also strip the trailing `xref` section
/// (everything from `xref` to `%%EOF`). The remainder is the
/// deterministic content: object dictionaries, page streams, and the
/// font tables.
fn pdf_content_only(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Drop everything between `<?xpacket begin=...?>` and the matching
        // `<?xpacket end=...?>` (inclusive).
        if bytes[i..].starts_with(b"<?xpacket begin") {
            if let Some(close) = find_subseq(&bytes[i..], b"<?xpacket end") {
                // Advance past the closing `?>` of the end marker.
                let after_end = i + close;
                let from_end = &bytes[after_end..];
                if let Some(end_q) = find_subseq(from_end, b"?>") {
                    i = after_end + end_q + 2;
                    continue;
                }
            }
        }
        // Drop PDF date literals (`(D:YYYYMMDDHHMMSS-HH'00')`).
        if bytes[i..].starts_with(b"(D:") {
            if let Some(close) = bytes[i..].iter().position(|&b| b == b')') {
                i += close + 1;
                continue;
            }
        }
        // Drop the trailer ID array `/ID [<hex> <hex>]`.
        if bytes[i..].starts_with(b"/ID") {
            if let Some(open) = bytes[i..].iter().position(|&b| b == b'[') {
                if let Some(close) = bytes[i + open..].iter().position(|&b| b == b']') {
                    i += open + close + 1;
                    continue;
                }
            }
        }
        // Drop the xref + trailer block (everything after the last `xref`).
        if bytes[i..].starts_with(b"xref") && find_subseq(&bytes[i..], b"%%EOF").is_some() {
            break;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn sample_proposal() -> ProposalPack {
    let mut pack = ProposalPack::new("Apartment 12B", "Ms. K");
    pack.branding = ProposalBranding {
        studio_name: "Studio Forma".into(),
        studio_tagline: Some("interiors".into()),
        logo_path: None,
    };
    pack.assets = ProposalAssets {
        mood_board: vec![
            "Warm scandi palette with oak and wool.".into(),
            "Soft contrast accents in matte black.".into(),
        ],
        plan_overview: vec!["Open-plan kitchen/living.".into()],
        cover_paragraph: Some("Concept proposal for Apartment 12B.".into()),
        renders: Vec::new(),
        floor_plan_overview: vec!["See sheet A-100 for plan.".into()],
        next_steps: vec!["Materials sign-off".into(), "Schedule a fitting".into()],
    };
    pack
}

#[test]
fn proposal_pack_pdf_content_is_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    let pack_a = sample_proposal();
    let pack_b = sample_proposal();

    let path_a = pack_a.to_pdf(tmp.path().join("a.pdf")).unwrap();
    let path_b = pack_b.to_pdf(tmp.path().join("b.pdf")).unwrap();

    let mut bytes_a = Vec::new();
    let mut bytes_b = Vec::new();
    std::fs::File::open(&path_a)
        .unwrap()
        .read_to_end(&mut bytes_a)
        .unwrap();
    std::fs::File::open(&path_b)
        .unwrap()
        .read_to_end(&mut bytes_b)
        .unwrap();

    let stripped_a = pdf_content_only(&bytes_a);
    let stripped_b = pdf_content_only(&bytes_b);
    assert_eq!(
        stripped_a, stripped_b,
        "proposal PDFs differ in content (not just timestamps)"
    );
}

#[test]
fn schedule_sheet_xlsx_content_is_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    let sheet = ScheduleSheet::furniture_schedule_template();

    let p1 = sheet.to_xlsx(tmp.path().join("s1.xlsx")).unwrap();
    let p2 = sheet.to_xlsx(tmp.path().join("s2.xlsx")).unwrap();

    let bytes_1 = std::fs::read(&p1).unwrap();
    let bytes_2 = std::fs::read(&p2).unwrap();

    // XLSX is a ZIP — the cell-level content is what we care about.
    // Compare the file inventory and the BLAKE3 of every non-meta entry.
    let inventory_1 = xlsx_content_inventory(&bytes_1);
    let inventory_2 = xlsx_content_inventory(&bytes_2);
    assert_eq!(inventory_1, inventory_2);
}

fn xlsx_content_inventory(bytes: &[u8]) -> Vec<(String, String)> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        // Skip files that legitimately vary across runs (workbook
        // properties carry the build timestamp).
        let name = entry.name().to_string();
        if name == "docProps/core.xml" {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        let hash = blake3::hash(&buf).to_hex().to_string();
        entries.push((name, hash));
    }
    entries.sort();
    entries
}

#[test]
fn contractor_pack_manifest_hashes_are_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    let sheet = tmp.path().join("A100.pdf");
    let sched = tmp.path().join("materials.xlsx");
    let ifc = tmp.path().join("project.ifc");
    std::fs::write(&sheet, b"%PDF-1.4 deterministic").unwrap();
    std::fs::write(&sched, b"PK\x03\x04 deterministic").unwrap();
    std::fs::write(&ifc, b"ISO-10303-21; deterministic").unwrap();

    let pack = ContractorPack {
        project_name: "Det Project".into(),
        app_version: "0.1.0".into(),
        sheets: vec![PackFile {
            archive_name: "sheets/A100.pdf".into(),
            source_path: sheet,
        }],
        schedules: vec![PackFile {
            archive_name: "schedules/materials.xlsx".into(),
            source_path: sched,
        }],
        ifc: Some(PackFile {
            archive_name: "model/project.ifc".into(),
            source_path: ifc,
        }),
        boq: None,
        proposal: None,
    };

    let (_, manifest_a) = pack.to_zip(tmp.path().join("a.zip")).unwrap();
    let (_, manifest_b) = pack.to_zip(tmp.path().join("b.zip")).unwrap();

    // The manifest carries a `created_at` field that legitimately
    // moves between runs, but the entry hashes must not.
    let hashes_a: Vec<_> = manifest_a
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.blake3.clone(), e.bytes))
        .collect();
    let hashes_b: Vec<_> = manifest_b
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.blake3.clone(), e.bytes))
        .collect();
    assert_eq!(hashes_a, hashes_b);
}
