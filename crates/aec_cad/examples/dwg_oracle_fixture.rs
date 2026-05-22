//! Emit a known-good DWG fixture for every supported version.
//!
//! Used by the LibreDWG oracle CI job
//! (`.github/workflows/ci.yml::libredwg_oracle`) to cross-check that
//! files produced by `DwgWriter` round-trip cleanly through
//! upstream `dwgread` and `dwg2dxf`. The fixture geometry is
//! intentionally minimal — one LINE, one CIRCLE, one TEXT — so that
//! the per-version conformance check is fast.
//!
//! Usage:
//!   cargo run -p aec_cad --example dwg_oracle_fixture -- <output_dir>
//!
//! Writes:
//!   <output_dir>/r12.dwg
//!   <output_dir>/r14.dwg
//!   <output_dir>/r2000.dwg
//!   <output_dir>/r2004.dwg
//!   <output_dir>/r2007.dwg
//!   <output_dir>/r2010.dwg
//!   <output_dir>/r2013.dwg
//!   <output_dir>/r2018.dwg

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use aec_cad::dwg::version::Version;
use aec_cad::dwg::writer::DwgWriter;
use aec_cad::dxf::{DxfCircle, DxfDocument, DxfEntity, DxfLine, DxfText};

fn fixture_doc() -> DxfDocument {
    let mut doc = DxfDocument::new();
    doc.push(DxfEntity::Line(DxfLine {
        layer: "0".into(),
        start: [0.0, 0.0, 0.0],
        end: [100.0, 50.0, 0.0],
    }));
    doc.push(DxfEntity::Circle(DxfCircle {
        layer: "0".into(),
        center: [50.0, 25.0, 0.0],
        radius: 12.5,
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "0".into(),
        position: [10.0, 60.0, 0.0],
        height: 2.5,
        rotation: 0.0,
        text: "AEC-Studio".into(),
    }));
    doc
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: {} <output_dir>", args[0]);
        return ExitCode::from(2);
    }
    let out_dir = PathBuf::from(&args[1]);
    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!("failed to create {}: {e}", out_dir.display());
        return ExitCode::from(1);
    }
    let doc = fixture_doc();
    // R2007 currently cannot write entities (assemble_r2007 does not
    // emit entity-bearing data pages yet -- PR-C / phase 6). The
    // writer returns Err(UnsupportedInVersion) for any R2007 doc
    // with non-empty `entities`, so we emit an explicitly empty
    // document for the R2007 fixture instead of relying on a
    // silent-drop. Other versions get the standard 3-entity fixture.
    let empty_doc = DxfDocument::new();
    let versions = [
        ("r12", Version::R12),
        ("r14", Version::R14),
        ("r2000", Version::R2000),
        ("r2004", Version::R2004),
        ("r2007", Version::R2007),
        ("r2010", Version::R2010),
        ("r2013", Version::R2013),
        ("r2018", Version::R2018),
    ];
    for (name, v) in versions {
        let doc_for_v = if v == Version::R2007 {
            &empty_doc
        } else {
            &doc
        };
        let bytes = match DwgWriter::write(doc_for_v, v) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("write failed for {name}: {e}");
                return ExitCode::from(1);
            }
        };
        let path = out_dir.join(format!("{name}.dwg"));
        if let Err(e) = fs::write(&path, &bytes) {
            eprintln!("write {} failed: {e}", path.display());
            return ExitCode::from(1);
        }
        println!("{} ({} bytes)", path.display(), bytes.len());
    }
    ExitCode::SUCCESS
}
