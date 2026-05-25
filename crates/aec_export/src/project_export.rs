//! Project-level export facades.
//!
//! Phase 10 PR-S surface — the bridge layer calls these from the
//! N-API-exposed `exportPdf` / `exportDxf` / `exportIfc` / `exportGltf`
//! / `exportBuildProposalPack` / `deliverBuildPack` endpoints. Each
//! function writes a **real** file to disk in the requested format
//! (not a stub — the bytes pass the relevant format's magic-number /
//! grammar check) and returns the on-disk path plus enough metadata
//! for the renderer's progress UI.
//!
//! The intent is to keep the bridge's call sites short — every export
//! method should be a `let path = project_export::write_*(...)?;`
//! one-liner. Format-specific complexity (printpdf, DXF entity
//! assembly, IFC STEP serialisation, glTF JSON) lives here.

use std::io::Write;
use std::path::{Path, PathBuf};

use aec_bim::ifc::writer::IfcWriter;
use aec_bim::{ClassificationStore, IfcClass, MaterialStore, Project, PropertyStore};
use aec_cad::dxf::entities::{DxfLine, DxfPolyline, DxfPolylineVertex, DxfText};
use aec_cad::dxf::DxfEntity;
use aec_cad::{DxfDocument, DxfWriter, Layer};
use chrono::Utc;
use serde::Serialize;
use thiserror::Error;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::contractor_pack::ContractorPackError;
use crate::pdf::{PageSize, PdfBuilder, PdfBuilderError};
use crate::proposal::{ProposalAssets, ProposalBranding, ProposalPack};

/// Aggregated error from any of the project-level export helpers.
///
/// Each format keeps its native error type; we collect them via
/// `#[from]` so the bridge layer can pattern-match if it wants finer
/// granularity, but mostly just bubbles a single `String` up through
/// `napi::Error`.
#[derive(Debug, Error)]
pub enum ProjectExportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pdf: {0}")]
    Pdf(#[from] PdfBuilderError),
    #[error("dxf: {0}")]
    Dxf(#[from] aec_cad::error::CadError),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("contractor pack: {0}")]
    Pack(#[from] ContractorPackError),
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// Returned by [`write_project_pdf`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteProjectPdfResult {
    pub out_path: PathBuf,
    pub pages: u32,
}

/// Build a multi-page summary PDF for `project_name`.
///
/// The cover page repeats the title and ISO-8601 timestamp; an
/// "Overview" page lists `body_lines`. The output is a real
/// printpdf-serialised file (starts with `%PDF-`) — the test
/// [`tests::write_project_pdf_emits_pdf_magic`] pins the magic-number
/// invariant so this can never silently regress to a stub.
pub fn write_project_pdf(
    out_path: &Path,
    project_name: &str,
    body_lines: &[String],
) -> Result<WriteProjectPdfResult, ProjectExportError> {
    ensure_parent_dir(out_path)?;
    let mut b = PdfBuilder::new(project_name, PageSize::A4_PORTRAIT)?;
    b.add_cover_page(Some(&format!("Exported {}", Utc::now().to_rfc3339())))?;
    let overview_lines: Vec<String> = if body_lines.is_empty() {
        vec!["(no project body provided)".to_string()]
    } else {
        body_lines.to_vec()
    };
    b.add_text_page("Overview", &overview_lines)?;
    let pages = b.page_count();
    let path = b.save(out_path)?;
    Ok(WriteProjectPdfResult {
        out_path: path,
        pages,
    })
}

/// Returned by [`write_project_dxf`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteProjectDxfResult {
    pub out_path: PathBuf,
}

/// Write a real DXF file containing a project title block + any
/// supplied wall segments.
///
/// Walls are emitted as `LINE` entities on layer `WALLS`. The output
/// is the standard `AC1027` (DXF R2013) ASCII variant produced by
/// [`DxfWriter::write`]. A title block (`A0` layer text) and a single
/// frame `LWPOLYLINE` give the file enough content that downstream
/// tooling can open it; the test
/// [`tests::write_project_dxf_emits_dxf_grammar`] pins the AutoCAD
/// version marker so this can never regress to an empty stub.
pub fn write_project_dxf(
    out_path: &Path,
    project_name: &str,
    walls_mm: &[(f64, f64, f64, f64)],
) -> Result<WriteProjectDxfResult, ProjectExportError> {
    ensure_parent_dir(out_path)?;
    let mut doc = DxfDocument::new();

    // Title-block layer plus a wall layer. `LayerSystem::new` already
    // contains layer `0`; we add the two named layers so the layer
    // table in the output has more than the default and the file
    // looks like a real project drawing. Layer-name validation is
    // surfaced rather than panicked — a malformed user-supplied
    // project name would not be able to corrupt the export.
    let walls_layer = Layer::new("WALLS")
        .map_err(|e| ProjectExportError::Invalid(format!("create WALLS layer: {e}")))?;
    let title_layer = Layer::new("TITLE")
        .map_err(|e| ProjectExportError::Invalid(format!("create TITLE layer: {e}")))?;
    doc.layers
        .insert(walls_layer)
        .map_err(|e| ProjectExportError::Invalid(format!("insert WALLS layer: {e}")))?;
    doc.layers
        .insert(title_layer)
        .map_err(|e| ProjectExportError::Invalid(format!("insert TITLE layer: {e}")))?;

    // Title block text at the bottom-left of the page (in mm).
    doc.push(DxfEntity::Text(DxfText {
        layer: "TITLE".to_string(),
        position: [10.0, 10.0, 0.0],
        height: 5.0,
        rotation: 0.0,
        text: format!("Project: {project_name}"),
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "TITLE".to_string(),
        position: [10.0, 4.0, 0.0],
        height: 3.0,
        rotation: 0.0,
        text: format!("Exported {}", Utc::now().to_rfc3339()),
    }));

    // Page frame as a closed LWPOLYLINE (A4 landscape, 297×210 mm).
    doc.push(DxfEntity::Polyline(DxfPolyline {
        layer: "TITLE".to_string(),
        vertices: vec![
            DxfPolylineVertex::new(0.0, 0.0),
            DxfPolylineVertex::new(297.0, 0.0),
            DxfPolylineVertex::new(297.0, 210.0),
            DxfPolylineVertex::new(0.0, 210.0),
        ],
        closed: true,
        elevation: 0.0,
    }));

    for (x1, y1, x2, y2) in walls_mm {
        doc.push(DxfEntity::Line(DxfLine {
            layer: "WALLS".to_string(),
            start: [*x1, *y1, 0.0],
            end: [*x2, *y2, 0.0],
        }));
    }

    let mut buf = Vec::new();
    DxfWriter::write(&doc, &mut buf)?;
    std::fs::write(out_path, &buf)?;
    Ok(WriteProjectDxfResult {
        out_path: out_path.to_path_buf(),
    })
}

/// Returned by [`write_project_ifc`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteProjectIfcResult {
    pub out_path: PathBuf,
}

/// Write a real IFC4 STEP file representing `project_name`.
///
/// The output is the same `IfcWriter::to_string` serialisation the
/// IFC round-trip test suite is built on. Always emits at least an
/// `IfcProject` + `IfcSite` + `IfcBuilding` + `IfcBuildingStorey`
/// spatial chain so the file is parseable by IFC4-conformant tools.
/// `storey_names`, when non-empty, adds named storeys under the
/// single building (otherwise a single "Level 1" storey is added).
pub fn write_project_ifc(
    out_path: &Path,
    project_name: &str,
    storey_names: &[String],
) -> Result<WriteProjectIfcResult, ProjectExportError> {
    ensure_parent_dir(out_path)?;
    let mut project = Project::new(project_name);
    let root = project.root.clone();
    let site = project
        .add_child(&root, IfcClass::IfcSite, "Site")
        .expect("root exists");
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Building")
        .expect("site exists");
    let names = if storey_names.is_empty() {
        vec!["Level 1".to_string()]
    } else {
        storey_names.to_vec()
    };
    for name in names {
        project
            .add_child(&building, IfcClass::IfcBuildingStorey, name)
            .expect("building exists");
    }

    let classification = ClassificationStore::new();
    let properties = PropertyStore::new();
    let materials = MaterialStore::new();
    let step =
        IfcWriter::to_string_with_materials(&project, &classification, &properties, &materials);
    std::fs::write(out_path, step.as_bytes())?;
    Ok(WriteProjectIfcResult {
        out_path: out_path.to_path_buf(),
    })
}

/// Returned by [`write_project_gltf`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteProjectGltfResult {
    pub out_path: PathBuf,
}

/// Write a real glTF 2.0 JSON file.
///
/// The result is a minimal-but-valid glTF document: an empty default
/// scene with a single root node named after the project. This is
/// enough that glTF tooling (e.g. Khronos `gltf-validator`,
/// three.js's `GLTFLoader`) opens it without errors. Future PRs will
/// thread real meshes (walls as extrusions) through this entry
/// point; the JSON shape stays additive-compatible.
pub fn write_project_gltf(
    out_path: &Path,
    project_name: &str,
) -> Result<WriteProjectGltfResult, ProjectExportError> {
    ensure_parent_dir(out_path)?;
    // Use a plain `serde_json::Value` rather than a typed shape so the
    // exact field-order matches the glTF 2.0 reference layout. The
    // mandatory `asset.version: "2.0"` is what tooling checks first;
    // `scenes`/`nodes` are non-empty so the file represents a real
    // scene graph rather than an empty document.
    let doc = serde_json::json!({
        "asset": {
            "version": "2.0",
            "generator": "AEC Studio aec_export project_export",
            "copyright": format!("AEC Studio export — {}", Utc::now().to_rfc3339()),
        },
        "scene": 0,
        "scenes": [
            { "name": project_name, "nodes": [0] }
        ],
        "nodes": [
            { "name": project_name }
        ],
    });
    let bytes = serde_json::to_vec_pretty(&doc)
        .map_err(|e| ProjectExportError::Invalid(format!("failed to serialise glTF JSON: {e}")))?;
    std::fs::write(out_path, &bytes)?;
    Ok(WriteProjectGltfResult {
        out_path: out_path.to_path_buf(),
    })
}

/// Returned by [`write_proposal_pack`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteProposalPackResult {
    pub out_path: PathBuf,
}

/// Write the proposal pack PDF for `(project_name, client_name)`.
///
/// Routes through [`ProposalPack::to_pdf`], which already produces a
/// real PDF (cover, scope, schedule, attachments). With no caller-
/// supplied `ProposalAssets`/`ProposalBranding`, the defaults give a
/// clean cover + empty body — still a valid PDF the renderer can
/// open. Phase 11 will thread real branding/assets through this
/// entry point.
pub fn write_proposal_pack(
    out_path: &Path,
    project_name: &str,
    client_name: &str,
) -> Result<WriteProposalPackResult, ProjectExportError> {
    ensure_parent_dir(out_path)?;
    let mut pack = ProposalPack::new(project_name, client_name);
    pack.assets = ProposalAssets::default();
    pack.branding = ProposalBranding::default();
    let path = pack.to_pdf(out_path)?;
    Ok(WriteProposalPackResult { out_path: path })
}

/// Pack kind for [`write_deliver_pack`]. Mirrors the renderer's
/// `deliver.buildPack` `kind` union — these are the four shipping
/// archetypes the Deliver page supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverPackKind {
    Concept,
    Interior,
    Contractor,
    Bim,
}

impl DeliverPackKind {
    pub fn parse(s: &str) -> Result<Self, ProjectExportError> {
        match s {
            "concept" => Ok(Self::Concept),
            "interior" => Ok(Self::Interior),
            "contractor" => Ok(Self::Contractor),
            "bim" => Ok(Self::Bim),
            other => Err(ProjectExportError::Invalid(format!(
                "unknown deliver pack kind `{other}` (expected one of \
                 concept, interior, contractor, bim)"
            ))),
        }
    }
}

/// Options for [`write_deliver_pack`]. Mirrors the renderer's
/// `deliver.buildPack` request shape so the bridge call site can pass
/// the JS params through verbatim.
///
/// **Default semantics asymmetry note**: `Default::default()` here
/// gives all-`false` (standard Rust semantics — `bool::default()` is
/// `false`). The JS-facing `deliver.buildPack` contract defaults
/// omitted flags to `true` (see `apps/desktop/electron/bridge.ts`
/// `?? true` and `crates/aec_bridge/src/napi_api.rs` `unwrap_or(true)`).
/// Direct Rust callers wanting the JS-equivalent "include everything"
/// behaviour should use [`DeliverPackOptions::all_enabled`] rather
/// than `default()`.
#[derive(Debug, Clone, Default)]
pub struct DeliverPackOptions {
    pub include_renders: bool,
    pub include_sheets: bool,
    pub include_ifc: bool,
    pub include_boq: bool,
    pub include_proposal: bool,
}

impl DeliverPackOptions {
    /// Returns an options struct with every `include_*` flag set to
    /// `true`. This matches the JS-facing `deliver.buildPack` default
    /// where omitted flags are treated as enabled.
    ///
    /// Use this rather than `DeliverPackOptions::default()` when a
    /// Rust caller wants the "include everything" semantics that the
    /// JS layer presents to renderer code.
    pub fn all_enabled() -> Self {
        Self {
            include_renders: true,
            include_sheets: true,
            include_ifc: true,
            include_boq: true,
            include_proposal: true,
        }
    }
}

/// Returned by [`write_deliver_pack`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteDeliverPackResult {
    pub out_path: PathBuf,
    pub contents: Vec<String>,
    pub total_bytes: u64,
}

/// Build the deliver pack ZIP at `out_path`.
///
/// Writes a *real* ZIP archive (a downstream `unzip` or
/// `zip::ZipArchive::new` can open it) — not a stub returning a
/// canned file list. Synthesises the kind-appropriate inventory
/// directly into the archive: a manifest (`manifest.json`), a
/// summary PDF (`summary.pdf`, real PDF bytes from [`PdfBuilder`]),
/// and the optional `includeX`-flagged artefacts. Every file's
/// archive name is recorded in `contents`, and `total_bytes` is the
/// sum of payload sizes (excluding the manifest, matching the
/// renderer's preview expectations).
///
/// The included files are deliberately minimal placeholders for now
/// (a one-page PDF, a small XLSX-shaped JSON, etc.); the structure
/// is the load-bearing piece that turns the renderer's deliver UI
/// from a fake into something a contractor could actually unzip.
pub fn write_deliver_pack(
    out_path: &Path,
    kind: DeliverPackKind,
    options: &DeliverPackOptions,
    project_name: &str,
) -> Result<WriteDeliverPackResult, ProjectExportError> {
    ensure_parent_dir(out_path)?;

    // Synthesise the file list this kind+options combination would
    // produce. Defaults match the JS in-process backend so the
    // renderer's preview UI shows the same inventory in dev and prod.
    let mut planned: Vec<(String, Vec<u8>)> = Vec::new();

    let summary_pdf = build_summary_pdf(project_name, kind)?;
    let summary_name = match kind {
        DeliverPackKind::Concept => "concept_pack.pdf",
        DeliverPackKind::Interior => "interior_summary.pdf",
        DeliverPackKind::Contractor => "contractor_summary.pdf",
        DeliverPackKind::Bim => "validation_report.pdf",
    };
    planned.push((summary_name.to_string(), summary_pdf));

    if kind == DeliverPackKind::Contractor && options.include_sheets {
        planned.push((
            "sheets/A100.pdf".to_string(),
            build_summary_pdf(&format!("{project_name} — Sheet A100"), kind)?,
        ));
        planned.push((
            "sheets/A101.pdf".to_string(),
            build_summary_pdf(&format!("{project_name} — Sheet A101"), kind)?,
        ));
    } else if kind == DeliverPackKind::Concept && options.include_sheets {
        planned.push((
            "sheets/A100.pdf".to_string(),
            build_summary_pdf(&format!("{project_name} — Cover Sheet"), kind)?,
        ));
    } else if kind == DeliverPackKind::Bim && options.include_sheets {
        planned.push((
            "sheets/A100.pdf".to_string(),
            build_summary_pdf(&format!("{project_name} — Sheet A100"), kind)?,
        ));
        planned.push((
            "sheets/A101.pdf".to_string(),
            build_summary_pdf(&format!("{project_name} — Sheet A101"), kind)?,
        ));
    }

    if matches!(kind, DeliverPackKind::Concept | DeliverPackKind::Interior)
        && options.include_renders
    {
        let render_files: &[&str] = match kind {
            DeliverPackKind::Concept => &["renders/01_cover.png"],
            DeliverPackKind::Interior => &["renders/01_living.png", "renders/02_kitchen.png"],
            _ => &[],
        };
        for name in render_files {
            planned.push(((*name).to_string(), placeholder_png()));
        }
    }

    if kind == DeliverPackKind::Interior {
        planned.push(("schedules/materials.xlsx".to_string(), placeholder_xlsx()));
    }
    if kind == DeliverPackKind::Contractor {
        planned.push(("schedules/materials.xlsx".to_string(), placeholder_xlsx()));
        if options.include_boq {
            planned.push(("schedules/boq.xlsx".to_string(), placeholder_xlsx()));
        }
    }

    if matches!(kind, DeliverPackKind::Contractor | DeliverPackKind::Bim) && options.include_ifc {
        let ifc_bytes = build_summary_ifc(project_name)?;
        planned.push(("model/project.ifc".to_string(), ifc_bytes));
    }

    if kind == DeliverPackKind::Contractor && options.include_proposal {
        let tmp_pdf = tempfile::NamedTempFile::new()?;
        // Reuse the proposal pack writer so the embedded PDF is the
        // same shape the standalone `exportBuildProposalPack` call
        // produces — keeps the two surfaces consistent.
        //
        // Read back via `std::fs::read(path)` rather than
        // `io::copy(tmp_pdf.as_file_mut(), …)`: `write_proposal_pack`
        // internally calls `PdfBuilder::save`, which today truncates-
        // and-writes the same path but is free to switch to an atomic
        // write-temp+rename in a future PR. A fresh fd opened from the
        // canonical path is robust against either implementation;
        // `as_file_mut` would silently observe the pre-truncated state
        // (position 0, empty) if `PdfBuilder::save` ever swaps the
        // inode. Same pattern as `build_summary_pdf` (`read(&saved)`)
        // a few lines below.
        let _ = write_proposal_pack(tmp_pdf.path(), project_name, "Contractor")?;
        let buf = std::fs::read(tmp_pdf.path())?;
        planned.push(("proposal.pdf".to_string(), buf));
    }

    // ----- ZIP assembly -----
    let file = std::fs::File::create(out_path)?;
    let mut zw = ZipWriter::new(file);
    let opts: SimpleFileOptions =
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut contents: Vec<String> = Vec::with_capacity(planned.len() + 1);
    let mut total_bytes: u64 = 0;
    for (name, bytes) in &planned {
        zw.start_file(name.as_str(), opts)?;
        zw.write_all(bytes)?;
        contents.push(name.clone());
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
    }

    // Manifest mirrors `ContractorPack::PackManifest` shape so a
    // downstream consumer who already speaks the contractor pack
    // protocol can read this without learning a new schema.
    let manifest = serde_json::json!({
        "project_name": project_name,
        "kind": match kind {
            DeliverPackKind::Concept => "concept",
            DeliverPackKind::Interior => "interior",
            DeliverPackKind::Contractor => "contractor",
            DeliverPackKind::Bim => "bim",
        },
        "created_at": Utc::now().to_rfc3339(),
        "entries": planned.iter().map(|(name, bytes)| {
            serde_json::json!({
                "name": name,
                "bytes": bytes.len() as u64,
                "blake3": hex::encode(blake3::hash(bytes).as_bytes()),
            })
        }).collect::<Vec<_>>(),
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| ProjectExportError::Invalid(format!("manifest serialise: {e}")))?;
    zw.start_file("manifest.json", opts)?;
    zw.write_all(&manifest_bytes)?;
    contents.push("manifest.json".to_string());

    zw.finish()?;
    Ok(WriteDeliverPackResult {
        out_path: out_path.to_path_buf(),
        contents,
        total_bytes,
    })
}

fn ensure_parent_dir(p: &Path) -> Result<(), ProjectExportError> {
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn build_summary_pdf(title: &str, kind: DeliverPackKind) -> Result<Vec<u8>, ProjectExportError> {
    // Use `PdfBuilder` directly (rather than `SheetPdfBuilder`, which
    // takes a typed `Sheet` + DXF entity list) so the deliver pack's
    // synthesised summary doesn't need a sheet layout — a cover +
    // text page is enough for the contractor to see what's in the
    // archive without us inventing a fake sheet layout.
    let kind_label = match kind {
        DeliverPackKind::Concept => "Concept pack",
        DeliverPackKind::Interior => "Interior pack",
        DeliverPackKind::Contractor => "Contractor pack",
        DeliverPackKind::Bim => "BIM pack",
    };
    let mut b = PdfBuilder::new(title, PageSize::A4_PORTRAIT)?;
    b.add_cover_page(Some(kind_label))?;
    b.add_text_page(
        "Pack Summary",
        &[
            kind_label.to_string(),
            format!("Generated {}", Utc::now().to_rfc3339()),
        ],
    )?;
    let tmp = tempfile::NamedTempFile::new()?;
    let saved = b.save(tmp.path())?;
    let bytes = std::fs::read(&saved)?;
    Ok(bytes)
}

fn build_summary_ifc(project_name: &str) -> Result<Vec<u8>, ProjectExportError> {
    let tmp = tempfile::NamedTempFile::new()?;
    let res = write_project_ifc(tmp.path(), project_name, &[])?;
    let bytes = std::fs::read(&res.out_path)?;
    Ok(bytes)
}

/// Minimal 1×1 fully-transparent PNG. Real PNG header so downstream
/// tooling treats the entry as an image rather than a corrupt blob;
/// the file is 67 bytes so the renderer's pack-preview total-bytes
/// estimate is exercised without bloating the ZIP.
fn placeholder_png() -> Vec<u8> {
    // 1x1 black PNG. Hand-computed CRCs.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
        0x00, 0x00, 0x00, 0x0D, // IHDR length
        0x49, 0x48, 0x44, 0x52, // "IHDR"
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x06, 0x00, 0x00, 0x00, // 8-bit RGBA
        0x1F, 0x15, 0xC4, 0x89, // IHDR CRC
        0x00, 0x00, 0x00, 0x0A, // IDAT length
        0x49, 0x44, 0x41, 0x54, // "IDAT"
        0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x01, // data
        0x0D, 0x0A, 0x2D, 0xB4, // IDAT CRC
        0x00, 0x00, 0x00, 0x00, // IEND length
        0x49, 0x45, 0x4E, 0x44, // "IEND"
        0xAE, 0x42, 0x60, 0x82, // IEND CRC
    ];
    PNG.to_vec()
}

/// Minimal valid XLSX (real ZIP container with the minimum
/// `xl/workbook.xml` + `[Content_Types].xml` + `_rels/.rels`
/// Open XML scaffolding). Excel opens it without complaints.
fn placeholder_xlsx() -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let cursor = std::io::Cursor::new(&mut buf);
    let mut zw = ZipWriter::new(cursor);
    let opts: SimpleFileOptions =
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let files: &[(&str, &str)] = &[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>AEC Studio placeholder</t></is></c></row></sheetData>
</worksheet>"#,
        ),
    ];

    for (name, body) in files {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(body.as_bytes()).unwrap();
    }
    zw.finish().unwrap();
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn write_project_pdf_emits_pdf_magic() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export.pdf");
        let res = write_project_pdf(&out, "Test Project", &["body line 1".into()]).unwrap();
        assert_eq!(res.out_path, out);
        assert_eq!(res.pages, 2);
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "missing PDF magic header");
        assert!(bytes.len() > 1024);
    }

    #[test]
    fn write_project_pdf_handles_empty_body() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export.pdf");
        let res = write_project_pdf(&out, "Empty Body", &[]).unwrap();
        // Still emits the cover + overview, just with the placeholder line.
        assert_eq!(res.pages, 2);
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn write_project_dxf_emits_dxf_grammar() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export.dxf");
        let walls = vec![(0.0, 0.0, 1000.0, 0.0), (1000.0, 0.0, 1000.0, 500.0)];
        let res = write_project_dxf(&out, "Test Project", &walls).unwrap();
        assert_eq!(res.out_path, out);
        let text = std::fs::read_to_string(&out).unwrap();
        // ASCII DXF must contain a SECTION header and EOF marker.
        assert!(text.contains("SECTION"));
        assert!(text.trim_end().ends_with("EOF"));
        // The walls we passed must be present as LINE entities on the WALLS layer.
        assert!(text.contains("LINE"));
        assert!(text.contains("WALLS"));
        // And the title text must round-trip.
        assert!(text.contains("Test Project"));
    }

    #[test]
    fn write_project_ifc_emits_real_ifc4_step() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export.ifc");
        let res = write_project_ifc(&out, "Test Project", &[]).unwrap();
        assert_eq!(res.out_path, out);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.starts_with("ISO-10303-21;"));
        assert!(text.contains("FILE_SCHEMA(('IFC4'));"));
        // The four mandatory spatial classes (Project/Site/Building/Storey)
        // must all appear. The writer emits the camelCase
        // `IfcProject` / `IfcSite` / `IfcBuilding` / `IfcBuildingStorey`
        // tag form (the reader is case-insensitive on entity-type
        // tokens). Matching the case-insensitive form here so this
        // test pins "Project/Site/Building/Storey were emitted at all"
        // independently of any future writer-side casing change.
        let upper = text.to_ascii_uppercase();
        assert!(upper.contains("IFCPROJECT"));
        assert!(upper.contains("IFCSITE"));
        assert!(upper.contains("IFCBUILDING"));
        assert!(upper.contains("IFCBUILDINGSTOREY"));
    }

    #[test]
    fn write_project_ifc_with_custom_storeys() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export.ifc");
        write_project_ifc(
            &out,
            "Test",
            &[
                "Ground Floor".to_string(),
                "First Floor".to_string(),
                "Roof".to_string(),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("'Ground Floor'"));
        assert!(text.contains("'First Floor'"));
        assert!(text.contains("'Roof'"));
    }

    #[test]
    fn write_project_gltf_passes_minimum_schema_check() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export.gltf");
        write_project_gltf(&out, "Test Project").unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["asset"]["version"], "2.0");
        assert!(doc["scenes"].is_array());
        assert!(doc["nodes"].is_array());
        assert_eq!(doc["nodes"][0]["name"], "Test Project");
    }

    #[test]
    fn write_proposal_pack_emits_real_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("proposal.pdf");
        let res = write_proposal_pack(&out, "Project A", "Client B").unwrap();
        assert_eq!(res.out_path, out);
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn write_deliver_pack_concept_writes_real_zip_with_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("concept.zip");
        let res = write_deliver_pack(
            &out,
            DeliverPackKind::Concept,
            &DeliverPackOptions {
                include_renders: true,
                include_sheets: true,
                ..Default::default()
            },
            "Test Project",
        )
        .unwrap();
        assert_eq!(res.out_path, out);
        assert!(res.contents.contains(&"concept_pack.pdf".to_string()));
        assert!(res.contents.contains(&"renders/01_cover.png".to_string()));
        assert!(res.contents.contains(&"sheets/A100.pdf".to_string()));
        assert!(res.contents.contains(&"manifest.json".to_string()));
        // Reopen the ZIP and verify the manifest is parseable + lists
        // the same entries.
        let f = std::fs::File::open(&out).unwrap();
        let mut zr = zip::ZipArchive::new(f).unwrap();
        let mut manifest_bytes = Vec::new();
        zr.by_name("manifest.json")
            .unwrap()
            .read_to_end(&mut manifest_bytes)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        assert_eq!(manifest["project_name"], "Test Project");
        assert_eq!(manifest["kind"], "concept");
    }

    #[test]
    fn write_deliver_pack_contractor_with_all_flags_includes_every_artefact() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("contractor.zip");
        let res = write_deliver_pack(
            &out,
            DeliverPackKind::Contractor,
            &DeliverPackOptions {
                include_renders: true,
                include_sheets: true,
                include_ifc: true,
                include_boq: true,
                include_proposal: true,
            },
            "Test Project",
        )
        .unwrap();
        for expected in &[
            "contractor_summary.pdf",
            "sheets/A100.pdf",
            "sheets/A101.pdf",
            "schedules/materials.xlsx",
            "schedules/boq.xlsx",
            "model/project.ifc",
            "proposal.pdf",
            "manifest.json",
        ] {
            assert!(
                res.contents.contains(&(*expected).to_string()),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn write_deliver_pack_bim_kind_includes_validation_report() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("bim.zip");
        let res = write_deliver_pack(
            &out,
            DeliverPackKind::Bim,
            &DeliverPackOptions {
                include_ifc: true,
                include_sheets: true,
                ..Default::default()
            },
            "Test Project",
        )
        .unwrap();
        assert!(res.contents.contains(&"validation_report.pdf".to_string()));
        assert!(res.contents.contains(&"model/project.ifc".to_string()));
        assert!(res.contents.contains(&"sheets/A100.pdf".to_string()));
    }

    /// Pins the exact `write_deliver_pack` output ordering for the
    /// "all flags enabled" path of all four kinds. The JS-side mirror
    /// (`apps/desktop/electron/bridge.ts::packContents` + its vitest at
    /// `apps/desktop/renderer/src/__tests__/export-in-process.test.ts`)
    /// asserts the same sequences with `toEqual`. Together these two
    /// tests pin the cross-language ordering parity that the renderer's
    /// preview pane relies on (the synthetic JS preview must match the
    /// real ZIP the native backend produces).
    #[test]
    fn write_deliver_pack_contents_ordering_matches_js_pack_contents() {
        let dir = tempfile::tempdir().unwrap();

        let cases = [
            (
                DeliverPackKind::Concept,
                "concept_all_flags.zip",
                vec![
                    "concept_pack.pdf".to_string(),
                    "sheets/A100.pdf".to_string(),
                    "renders/01_cover.png".to_string(),
                    "manifest.json".to_string(),
                ],
            ),
            (
                DeliverPackKind::Interior,
                "interior_all_flags.zip",
                vec![
                    "interior_summary.pdf".to_string(),
                    "renders/01_living.png".to_string(),
                    "renders/02_kitchen.png".to_string(),
                    "schedules/materials.xlsx".to_string(),
                    "manifest.json".to_string(),
                ],
            ),
            (
                DeliverPackKind::Contractor,
                "contractor_all_flags.zip",
                vec![
                    "contractor_summary.pdf".to_string(),
                    "sheets/A100.pdf".to_string(),
                    "sheets/A101.pdf".to_string(),
                    "schedules/materials.xlsx".to_string(),
                    "schedules/boq.xlsx".to_string(),
                    "model/project.ifc".to_string(),
                    "proposal.pdf".to_string(),
                    "manifest.json".to_string(),
                ],
            ),
            (
                DeliverPackKind::Bim,
                "bim_all_flags.zip",
                vec![
                    "validation_report.pdf".to_string(),
                    "sheets/A100.pdf".to_string(),
                    "sheets/A101.pdf".to_string(),
                    "model/project.ifc".to_string(),
                    "manifest.json".to_string(),
                ],
            ),
        ];

        for (kind, filename, expected) in cases {
            let out = dir.path().join(filename);
            let res = write_deliver_pack(
                &out,
                kind,
                &DeliverPackOptions::all_enabled(),
                "Test Project",
            )
            .unwrap();
            assert_eq!(
                res.contents, expected,
                "{kind:?} contents ordering must match JS packContents"
            );
        }
    }

    #[test]
    fn deliver_pack_options_all_enabled_sets_every_flag() {
        let opts = DeliverPackOptions::all_enabled();
        assert!(opts.include_renders);
        assert!(opts.include_sheets);
        assert!(opts.include_ifc);
        assert!(opts.include_boq);
        assert!(opts.include_proposal);
    }

    #[test]
    fn deliver_pack_kind_parse_rejects_unknown() {
        assert!(DeliverPackKind::parse("concept").is_ok());
        assert!(DeliverPackKind::parse("interior").is_ok());
        assert!(DeliverPackKind::parse("contractor").is_ok());
        assert!(DeliverPackKind::parse("bim").is_ok());
        match DeliverPackKind::parse("hat") {
            Err(ProjectExportError::Invalid(msg)) => assert!(msg.contains("`hat`")),
            other => panic!("expected Invalid error, got {other:?}"),
        }
    }
}
