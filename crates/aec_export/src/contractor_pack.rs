//! Contractor handoff pack.
//!
//! Bundles the artefacts a contractor needs to actually build the
//! project: sheet PDFs, schedule XLSX files, the IFC model, a BOQ
//! XLSX, and an optional proposal PDF. A `manifest.json` accompanies
//! the archive and records BLAKE3 checksums for every payload so the
//! contractor can verify they received an unmodified pack.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[derive(Debug, Error)]
pub enum ContractorPackError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("file not found: {0}")]
    MissingFile(PathBuf),
    #[error("pack has no payloads — must include at least one sheet, schedule, or IFC")]
    EmptyPack,
}

/// Pack manifest written alongside the contractor archive. Records
/// the file inventory plus BLAKE3 hashes so the recipient can
/// verify integrity without trusting the transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackManifest {
    pub project_name: String,
    pub app_version: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub name: String,
    pub bytes: u64,
    /// BLAKE3 hash of the file contents, hex-encoded.
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContractorPack {
    pub project_name: String,
    pub app_version: String,
    pub sheets: Vec<PackFile>,
    pub schedules: Vec<PackFile>,
    pub ifc: Option<PackFile>,
    pub boq: Option<PackFile>,
    pub proposal: Option<PackFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackFile {
    /// Display name inside the archive (e.g. `sheets/A100.pdf`).
    pub archive_name: String,
    /// Path on disk to read from.
    pub source_path: PathBuf,
}

impl ContractorPack {
    pub fn to_zip(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(PathBuf, PackManifest), ContractorPackError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        if self.sheets.is_empty() && self.schedules.is_empty() && self.ifc.is_none() {
            return Err(ContractorPackError::EmptyPack);
        }

        let file = std::fs::File::create(path)?;
        let mut zw = ZipWriter::new(file);
        let opts: SimpleFileOptions =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        let mut entries: Vec<ManifestEntry> = Vec::new();
        let add = |zw: &mut ZipWriter<_>,
                   pf: &PackFile,
                   entries: &mut Vec<ManifestEntry>|
         -> Result<(), ContractorPackError> {
            if !pf.source_path.exists() {
                return Err(ContractorPackError::MissingFile(pf.source_path.clone()));
            }
            let mut buf = Vec::new();
            std::fs::File::open(&pf.source_path)?.read_to_end(&mut buf)?;
            zw.start_file(&pf.archive_name, opts)?;
            zw.write_all(&buf)?;
            let hash = blake3::hash(&buf);
            entries.push(ManifestEntry {
                name: pf.archive_name.clone(),
                bytes: buf.len() as u64,
                blake3: hex::encode(hash.as_bytes()),
            });
            Ok(())
        };

        for sheet in &self.sheets {
            add(&mut zw, sheet, &mut entries)?;
        }
        for schedule in &self.schedules {
            add(&mut zw, schedule, &mut entries)?;
        }
        if let Some(ifc) = &self.ifc {
            add(&mut zw, ifc, &mut entries)?;
        }
        if let Some(boq) = &self.boq {
            add(&mut zw, boq, &mut entries)?;
        }
        if let Some(prop) = &self.proposal {
            add(&mut zw, prop, &mut entries)?;
        }

        let manifest = PackManifest {
            project_name: self.project_name.clone(),
            app_version: self.app_version.clone(),
            created_at: chrono::Utc::now(),
            entries,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        zw.start_file("manifest.json", opts)?;
        zw.write_all(&manifest_json)?;
        zw.finish()?;
        Ok((path.to_path_buf(), manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn pack_contains_all_artefacts_with_manifest_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let sheet = write_temp(tmp.path(), "A100.pdf", b"%PDF-1.4 sheet bytes");
        let sched = write_temp(tmp.path(), "schedule.xlsx", b"PK\x03\x04 sched");
        let ifc = write_temp(tmp.path(), "project.ifc", b"ISO-10303-21;");
        let boq = write_temp(tmp.path(), "boq.xlsx", b"PK\x03\x04 boq");

        let pack = ContractorPack {
            project_name: "Apartment 12B".into(),
            app_version: "0.1.0".into(),
            sheets: vec![PackFile {
                archive_name: "sheets/A100.pdf".into(),
                source_path: sheet.clone(),
            }],
            schedules: vec![PackFile {
                archive_name: "schedules/materials.xlsx".into(),
                source_path: sched.clone(),
            }],
            ifc: Some(PackFile {
                archive_name: "model/project.ifc".into(),
                source_path: ifc.clone(),
            }),
            boq: Some(PackFile {
                archive_name: "schedules/boq.xlsx".into(),
                source_path: boq.clone(),
            }),
            proposal: None,
        };

        let out = tmp.path().join("contractor_pack.zip");
        let (created, manifest) = pack.to_zip(&out).unwrap();
        assert_eq!(created, out);

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        for expected in [
            "sheets/A100.pdf",
            "schedules/materials.xlsx",
            "model/project.ifc",
            "schedules/boq.xlsx",
            "manifest.json",
        ] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
        assert_eq!(manifest.entries.len(), 4);
        // Verify the manifest blake3 actually matches the file contents.
        let mut sheet_bytes = Vec::new();
        std::fs::File::open(&sheet)
            .unwrap()
            .read_to_end(&mut sheet_bytes)
            .unwrap();
        let expected_hash = hex::encode(blake3::hash(&sheet_bytes).as_bytes());
        let sheet_entry = manifest
            .entries
            .iter()
            .find(|e| e.name == "sheets/A100.pdf")
            .unwrap();
        assert_eq!(sheet_entry.blake3, expected_hash);
    }

    #[test]
    fn empty_pack_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = ContractorPack {
            project_name: "Empty".into(),
            app_version: "0.1.0".into(),
            ..Default::default()
        };
        let out = tmp.path().join("empty.zip");
        let err = pack.to_zip(&out).unwrap_err();
        assert!(matches!(err, ContractorPackError::EmptyPack));
    }

    #[test]
    fn missing_source_file_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = ContractorPack {
            project_name: "Test".into(),
            app_version: "0.1.0".into(),
            sheets: vec![PackFile {
                archive_name: "sheets/A100.pdf".into(),
                source_path: tmp.path().join("nope.pdf"),
            }],
            ..Default::default()
        };
        let out = tmp.path().join("err.zip");
        let err = pack.to_zip(&out).unwrap_err();
        assert!(matches!(err, ContractorPackError::MissingFile(_)));
    }
}
