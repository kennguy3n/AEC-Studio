//! BIM Lite export pack.
//!
//! Bundles an IFC model, sheet PDFs, and a validation report into a
//! single ZIP. The validation report is rendered inline as bytes so
//! callers can either supply pre-rendered PDF bytes or a plain-text
//! report; both are written as `validation_report.{ext}` based on
//! the supplied content type.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::contractor_pack::PackFile;

#[derive(Debug, Error)]
pub enum BimPackError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("file not found: {0}")]
    MissingFile(PathBuf),
    #[error("BIM pack requires an IFC payload")]
    MissingIfc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationReportKind {
    Text,
    Pdf,
}

impl ValidationReportKind {
    fn extension(self) -> &'static str {
        match self {
            ValidationReportKind::Text => "txt",
            ValidationReportKind::Pdf => "pdf",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub kind: ValidationReportKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimPack {
    pub project_name: String,
    pub ifc: PackFile,
    pub sheets: Vec<PackFile>,
    pub validation_report: ValidationReport,
}

impl BimPack {
    pub fn to_zip(&self, path: impl AsRef<Path>) -> Result<PathBuf, BimPackError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        if !self.ifc.source_path.exists() {
            return Err(BimPackError::MissingIfc);
        }

        let file = std::fs::File::create(path)?;
        let mut zw = ZipWriter::new(file);
        let opts: SimpleFileOptions =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        // IFC.
        let mut ifc_bytes = Vec::new();
        std::fs::File::open(&self.ifc.source_path)?.read_to_end(&mut ifc_bytes)?;
        zw.start_file(&self.ifc.archive_name, opts)?;
        zw.write_all(&ifc_bytes)?;

        // Sheets.
        for sheet in &self.sheets {
            if !sheet.source_path.exists() {
                return Err(BimPackError::MissingFile(sheet.source_path.clone()));
            }
            let mut buf = Vec::new();
            std::fs::File::open(&sheet.source_path)?.read_to_end(&mut buf)?;
            zw.start_file(&sheet.archive_name, opts)?;
            zw.write_all(&buf)?;
        }

        // Validation report.
        let report_name = format!(
            "validation_report.{}",
            self.validation_report.kind.extension()
        );
        zw.start_file(&report_name, opts)?;
        zw.write_all(&self.validation_report.bytes)?;

        // Manifest.
        let manifest = serde_json::json!({
            "project_name": self.project_name,
            "ifc": self.ifc.archive_name,
            "sheets": self.sheets.iter().map(|s| s.archive_name.clone()).collect::<Vec<_>>(),
            "validation_report": report_name,
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        zw.start_file("manifest.json", opts)?;
        zw.write_all(&manifest_bytes)?;

        zw.finish()?;
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn zip_contains_ifc_sheets_and_validation_report() {
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = write_temp(tmp.path(), "model.ifc", b"ISO-10303-21;");
        let sheet_path = write_temp(tmp.path(), "A100.pdf", b"%PDF sheet");

        let pack = BimPack {
            project_name: "Apartment 12B".into(),
            ifc: PackFile {
                archive_name: "model/project.ifc".into(),
                source_path: ifc_path,
            },
            sheets: vec![PackFile {
                archive_name: "sheets/A100.pdf".into(),
                source_path: sheet_path,
            }],
            validation_report: ValidationReport {
                kind: ValidationReportKind::Text,
                bytes: b"OK: 0 errors\nWARN: unclassified spaces=1\n".to_vec(),
            },
        };

        let out = tmp.path().join("bim_pack.zip");
        pack.to_zip(&out).unwrap();

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        for expected in [
            "model/project.ifc",
            "sheets/A100.pdf",
            "validation_report.txt",
            "manifest.json",
        ] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn pdf_report_lands_with_pdf_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let ifc_path = write_temp(tmp.path(), "model.ifc", b"ISO-10303-21;");

        let pack = BimPack {
            project_name: "Test".into(),
            ifc: PackFile {
                archive_name: "model/project.ifc".into(),
                source_path: ifc_path,
            },
            sheets: vec![],
            validation_report: ValidationReport {
                kind: ValidationReportKind::Pdf,
                bytes: b"%PDF-1.4 validation".to_vec(),
            },
        };
        let out = tmp.path().join("bim.zip");
        pack.to_zip(&out).unwrap();

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "validation_report.pdf"));
    }

    #[test]
    fn missing_ifc_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = BimPack {
            project_name: "Test".into(),
            ifc: PackFile {
                archive_name: "model/project.ifc".into(),
                source_path: tmp.path().join("nope.ifc"),
            },
            sheets: vec![],
            validation_report: ValidationReport {
                kind: ValidationReportKind::Text,
                bytes: b"ok".to_vec(),
            },
        };
        let out = tmp.path().join("err.zip");
        let err = pack.to_zip(&out).unwrap_err();
        assert!(matches!(err, BimPackError::MissingIfc));
    }
}
