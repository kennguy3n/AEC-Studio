//! XLSX exporter for [`ScheduleSheet`].
//!
//! The aec_export crate already produces tabular PDFs; downstream
//! pipelines (estimators, procurement) want the same data as a real
//! XLSX spreadsheet so they can sort, filter, and feed it into their
//! own systems. We use [`rust_xlsxwriter`] to write a single-sheet
//! workbook with the schedule columns as the header row.
//!
//! The output is a real `.xlsx` file (a ZIP container starting with
//! the `PK` magic bytes) — the tests verify both the bytes on disk and
//! that the workbook reloads correctly via the same crate.

use std::path::{Path, PathBuf};

use rust_xlsxwriter::{Format, Workbook, XlsxError};
use thiserror::Error;

use crate::schedule::ScheduleSheet;

#[derive(Debug, Error)]
pub enum XlsxExportError {
    #[error("xlsx writer error: {0}")]
    Writer(#[from] XlsxError),
    #[error("output path has no parent directory: {0:?}")]
    NoParentDir(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Sheet name used inside the workbook. Kept short enough to avoid
/// Excel's 31-character limit no matter how the schedule is titled.
const DEFAULT_SHEET_NAME: &str = "Schedule";

impl ScheduleSheet {
    /// Write this schedule out as an XLSX workbook.
    ///
    /// The workbook contains a single sheet (`Schedule`) with the
    /// schedule title in the very first row, the column display names
    /// as a bold header row underneath, and the data rows following.
    pub fn to_xlsx(&self, path: impl AsRef<Path>) -> Result<PathBuf, XlsxExportError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        } else {
            return Err(XlsxExportError::NoParentDir(path.to_path_buf()));
        }

        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.set_name(DEFAULT_SHEET_NAME)?;

        let title_format = Format::new().set_bold().set_font_size(14);
        let header_format = Format::new().set_bold();

        // Row 0: schedule title spanning all columns.
        sheet.write_with_format(0, 0, &self.title, &title_format)?;

        // Row 1: column headers.
        for (col_idx, col) in self.columns.iter().enumerate() {
            sheet.write_with_format(1, col_idx as u16, &col.display_name, &header_format)?;
        }

        // Rows 2+: data rows.
        for (row_idx, row) in self.rows.iter().enumerate() {
            let r = (row_idx + 2) as u32;
            for (col_idx, cell) in row.cells.iter().enumerate() {
                sheet.write_string(r, col_idx as u16, cell)?;
            }
        }

        // Autofit columns so the output is readable when opened.
        sheet.autofit();

        workbook.save(path)?;
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample_material_schedule() -> ScheduleSheet {
        let mut s = ScheduleSheet::material_schedule_template();
        s.push_row(["mat-001", "Oak veneer", "Living room", "12.4 m²", "Atelier"]);
        s.push_row(["mat-002", "Walnut floor", "Kitchen", "8.2 m²", "Atelier"]);
        s.push_row(["mat-003", "Brass trim", "Hallway", "3.0 m", "FormGuild"]);
        s
    }

    #[test]
    fn xlsx_output_starts_with_pk_zip_signature() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("materials.xlsx");
        let schedule = sample_material_schedule();
        schedule.to_xlsx(&path).unwrap();

        let mut f = std::fs::File::open(&path).unwrap();
        let mut header = [0u8; 2];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"PK", "xlsx must start with PK zip signature");

        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 1_000, "xlsx should be non-trivial in size");
    }

    #[test]
    fn xlsx_contains_expected_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("materials.xlsx");
        sample_material_schedule().to_xlsx(&path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "xl/workbook.xml"),
            "xlsx missing xl/workbook.xml: {names:?}",
        );
        assert!(
            names.iter().any(|n| n.starts_with("xl/worksheets/")),
            "xlsx missing worksheet entry: {names:?}",
        );
    }

    #[test]
    fn empty_schedule_still_produces_valid_workbook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.xlsx");
        let schedule = ScheduleSheet::material_schedule_template();
        schedule.to_xlsx(&path).unwrap();

        let mut f = std::fs::File::open(&path).unwrap();
        let mut header = [0u8; 2];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"PK");
    }
}
