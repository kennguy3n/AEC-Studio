//! Interior package deliverable.
//!
//! Bundles a summary PDF, render images, and a material schedule into
//! a single ZIP archive that the studio can hand to an interior client.
//! The ZIP contents map 1:1 to a manifest written alongside.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::schedule::ScheduleSheet;
use crate::xlsx::XlsxExportError;

#[derive(Debug, Error)]
pub enum InteriorPackError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("render image not found: {0}")]
    MissingRender(PathBuf),
    #[error("xlsx error: {0}")]
    Xlsx(#[from] XlsxExportError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteriorRender {
    pub label: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteriorPack {
    pub project_name: String,
    pub summary_pdf_bytes: Vec<u8>,
    pub renders: Vec<InteriorRender>,
    pub material_schedule: ScheduleSheet,
}

impl InteriorPack {
    /// Build the pack and write it to disk as a ZIP archive.
    /// Returns the path to the created archive and a manifest of its
    /// contents (also embedded in the zip).
    pub fn to_zip(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(PathBuf, InteriorPackManifest), InteriorPackError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let file = std::fs::File::create(path)?;
        let mut zw = ZipWriter::new(file);
        let opts: SimpleFileOptions =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        let mut manifest = InteriorPackManifest {
            project_name: self.project_name.clone(),
            entries: Vec::new(),
        };

        // Summary PDF.
        zw.start_file("interior_summary.pdf", opts)?;
        zw.write_all(&self.summary_pdf_bytes)?;
        manifest.entries.push(ManifestEntry {
            name: "interior_summary.pdf".into(),
            bytes: self.summary_pdf_bytes.len() as u64,
        });

        // Renders.
        for render in &self.renders {
            if !render.source_path.exists() {
                return Err(InteriorPackError::MissingRender(render.source_path.clone()));
            }
            let mut buf = Vec::new();
            std::fs::File::open(&render.source_path)?.read_to_end(&mut buf)?;
            let ext = render
                .source_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("png");
            let name = format!("renders/{}.{ext}", sanitize_path_component(&render.label));
            zw.start_file(&name, opts)?;
            zw.write_all(&buf)?;
            manifest.entries.push(ManifestEntry {
                name,
                bytes: buf.len() as u64,
            });
        }

        // Material schedule XLSX — write it to a tempfile so we can
        // route the rust_xlsxwriter output back into the archive.
        let tmp = tempfile::NamedTempFile::new()?;
        self.material_schedule.to_xlsx(tmp.path())?;
        let mut sched_bytes = Vec::new();
        std::fs::File::open(tmp.path())?.read_to_end(&mut sched_bytes)?;
        zw.start_file("schedules/materials.xlsx", opts)?;
        zw.write_all(&sched_bytes)?;
        manifest.entries.push(ManifestEntry {
            name: "schedules/materials.xlsx".into(),
            bytes: sched_bytes.len() as u64,
        });

        // Manifest JSON.
        let manifest_json = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        zw.start_file("manifest.json", opts)?;
        zw.write_all(&manifest_json)?;

        zw.finish()?;
        Ok((path.to_path_buf(), manifest))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteriorPackManifest {
    pub project_name: String,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub name: String,
    pub bytes: u64,
}

fn sanitize_path_component(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '-',
            c if c.is_whitespace() => '_',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn write_fake_render(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n....").unwrap();
        path
    }

    fn sample_pack(tmp: &Path) -> InteriorPack {
        let mut sched = ScheduleSheet::material_schedule_template();
        sched.push_row(["mat-001", "Oak", "Living", "12 m²", "Atelier"]);
        InteriorPack {
            project_name: "Apartment 12B".into(),
            summary_pdf_bytes: b"%PDF-1.4\nfake pdf bytes\n".to_vec(),
            renders: vec![
                InteriorRender {
                    label: "Living room".into(),
                    source_path: write_fake_render(tmp, "living.png"),
                },
                InteriorRender {
                    label: "Kitchen".into(),
                    source_path: write_fake_render(tmp, "kitchen.png"),
                },
            ],
            material_schedule: sched,
        }
    }

    #[test]
    fn zip_contains_summary_renders_schedule_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = sample_pack(tmp.path());
        let out = tmp.path().join("interior_pack.zip");
        let (created, manifest) = pack.to_zip(&out).unwrap();
        assert_eq!(created, out);

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();

        assert!(names.iter().any(|n| n == "interior_summary.pdf"));
        assert!(names.iter().any(|n| n.starts_with("renders/")));
        assert!(names.iter().any(|n| n == "schedules/materials.xlsx"));
        assert!(names.iter().any(|n| n == "manifest.json"));

        // Manifest mirrors the zip entries (minus the manifest itself).
        assert_eq!(manifest.entries.len(), 4); // summary + 2 renders + xlsx
        assert!(manifest
            .entries
            .iter()
            .any(|e| e.name == "interior_summary.pdf"));
    }

    #[test]
    fn manifest_json_is_in_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = sample_pack(tmp.path());
        let out = tmp.path().join("interior_pack.zip");
        pack.to_zip(&out).unwrap();

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name("manifest.json").unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        let parsed: InteriorPackManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.project_name, "Apartment 12B");
        assert!(parsed
            .entries
            .iter()
            .any(|e| e.name == "schedules/materials.xlsx"));
    }

    #[test]
    fn missing_render_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut pack = sample_pack(tmp.path());
        pack.renders.push(InteriorRender {
            label: "Bedroom".into(),
            source_path: tmp.path().join("missing.png"),
        });
        let out = tmp.path().join("err.zip");
        let err = pack.to_zip(&out).unwrap_err();
        assert!(matches!(err, InteriorPackError::MissingRender(_)));
    }

    #[test]
    fn sanitize_strips_slashes() {
        assert_eq!(sanitize_path_component("Hello/World"), "Hello-World");
        assert_eq!(sanitize_path_component("Hello World"), "Hello_World");
    }
}
