//! Bill of Quantities (BOQ) XLSX exporter.
//!
//! Produces a single XLSX workbook with one section per discipline
//! (architecture, finishes, mechanical, ...). The regional config
//! controls the units (m vs ft / m³ vs ft³), the currency string, and
//! the standards reference quoted on the cover row of every section.
//!
//! Regional sheet sizes affect downstream sheet PDFs, not the BOQ
//! itself — keeping the BOQ purely about line items keeps the file
//! useful when a client opens it on a system with different paper
//! defaults.

use std::path::{Path, PathBuf};

use rust_xlsxwriter::{Format, Workbook, XlsxError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BoqExportError {
    #[error("xlsx writer error: {0}")]
    Writer(#[from] XlsxError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("boq has no sections")]
    EmptyBoq,
}

/// Region the project will be delivered in. Affects units, currency,
/// and the standards reference printed at the top of each section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegionalConfig {
    Eu,
    Na,
    Apac,
}

impl RegionalConfig {
    pub fn unit_label(self) -> &'static str {
        match self {
            RegionalConfig::Eu | RegionalConfig::Apac => "m",
            RegionalConfig::Na => "ft",
        }
    }

    pub fn area_unit_label(self) -> &'static str {
        match self {
            RegionalConfig::Eu | RegionalConfig::Apac => "m²",
            RegionalConfig::Na => "ft²",
        }
    }

    pub fn volume_unit_label(self) -> &'static str {
        match self {
            RegionalConfig::Eu | RegionalConfig::Apac => "m³",
            RegionalConfig::Na => "ft³",
        }
    }

    pub fn currency(self) -> &'static str {
        match self {
            RegionalConfig::Eu => "EUR",
            RegionalConfig::Na => "USD",
            RegionalConfig::Apac => "USD",
        }
    }

    pub fn standards_reference(self) -> &'static str {
        match self {
            RegionalConfig::Eu => "EN ISO 22263",
            RegionalConfig::Na => "ASTM E1557",
            RegionalConfig::Apac => "AS 1181",
        }
    }
}

/// Quantity unit used by a single BOQ line. We keep this small and
/// regional config converts to the local label at export time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuantityUnit {
    Each,
    Length,
    Area,
    Volume,
}

impl QuantityUnit {
    pub fn label(self, region: RegionalConfig) -> &'static str {
        match self {
            QuantityUnit::Each => "ea",
            QuantityUnit::Length => region.unit_label(),
            QuantityUnit::Area => region.area_unit_label(),
            QuantityUnit::Volume => region.volume_unit_label(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoqLine {
    pub code: String,
    pub description: String,
    pub quantity: f64,
    pub unit: QuantityUnit,
    pub rate: f64,
}

impl BoqLine {
    pub fn total(&self) -> f64 {
        self.quantity * self.rate
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoqSection {
    pub discipline: String,
    pub lines: Vec<BoqLine>,
}

impl BoqSection {
    pub fn subtotal(&self) -> f64 {
        self.lines.iter().map(BoqLine::total).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoqExport {
    pub project_name: String,
    pub region: RegionalConfig,
    pub sections: Vec<BoqSection>,
}

impl BoqExport {
    pub fn new(project_name: impl Into<String>, region: RegionalConfig) -> Self {
        Self {
            project_name: project_name.into(),
            region,
            sections: Vec::new(),
        }
    }

    pub fn push_section(&mut self, discipline: impl Into<String>, lines: Vec<BoqLine>) {
        self.sections.push(BoqSection {
            discipline: discipline.into(),
            lines,
        });
    }

    pub fn total(&self) -> f64 {
        self.sections.iter().map(BoqSection::subtotal).sum()
    }

    /// Write the BOQ to disk as an XLSX workbook with one sheet per
    /// discipline section plus a summary sheet.
    pub fn to_xlsx(&self, path: impl AsRef<Path>) -> Result<PathBuf, BoqExportError> {
        if self.sections.is_empty() {
            return Err(BoqExportError::EmptyBoq);
        }

        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let mut workbook = Workbook::new();
        let bold = Format::new().set_bold();
        let bold_right = Format::new()
            .set_bold()
            .set_align(rust_xlsxwriter::FormatAlign::Right);
        let money = Format::new().set_num_format("#,##0.00");
        let header_format = Format::new().set_bold().set_background_color("D9E1F2");

        // Summary sheet first so it lands as the active tab when the
        // workbook is opened in Excel/LibreOffice.
        {
            let sheet = workbook.add_worksheet();
            sheet.set_name("Summary")?;
            sheet.write_with_format(
                0,
                0,
                &self.project_name,
                &Format::new().set_bold().set_font_size(14),
            )?;
            sheet.write_with_format(1, 0, format!("Region: {:?}", self.region), &bold)?;
            sheet.write_with_format(
                2,
                0,
                format!("Standard: {}", self.region.standards_reference()),
                &bold,
            )?;
            sheet.write_with_format(
                3,
                0,
                format!("Currency: {}", self.region.currency()),
                &bold,
            )?;

            sheet.write_with_format(5, 0, "Discipline", &header_format)?;
            sheet.write_with_format(5, 1, "Subtotal", &header_format)?;

            for (i, section) in self.sections.iter().enumerate() {
                let r = (6 + i) as u32;
                sheet.write_string(r, 0, &section.discipline)?;
                sheet.write_number_with_format(r, 1, section.subtotal(), &money)?;
            }

            let total_row = (6 + self.sections.len()) as u32;
            sheet.write_with_format(total_row, 0, "TOTAL", &bold_right)?;
            sheet.write_number_with_format(
                total_row,
                1,
                self.total(),
                &Format::new().set_bold().set_num_format("#,##0.00"),
            )?;
            sheet.autofit();
        }

        // One sheet per discipline with full line items.
        for section in &self.sections {
            let sheet = workbook.add_worksheet();
            // Excel sheet names are capped at 31 chars and cannot contain
            // certain characters — sanitize defensively.
            let name = sanitize_sheet_name(&section.discipline);
            sheet.set_name(&name)?;

            sheet.write_with_format(
                0,
                0,
                &section.discipline,
                &Format::new().set_bold().set_font_size(14),
            )?;

            let headers = ["Code", "Description", "Qty", "Unit", "Rate", "Total"];
            for (col, header) in headers.iter().enumerate() {
                sheet.write_with_format(2, col as u16, *header, &header_format)?;
            }
            for (i, line) in section.lines.iter().enumerate() {
                let r = (3 + i) as u32;
                sheet.write_string(r, 0, &line.code)?;
                sheet.write_string(r, 1, &line.description)?;
                sheet.write_number_with_format(r, 2, line.quantity, &money)?;
                sheet.write_string(r, 3, line.unit.label(self.region))?;
                sheet.write_number_with_format(r, 4, line.rate, &money)?;
                sheet.write_number_with_format(r, 5, line.total(), &money)?;
            }
            let total_row = (3 + section.lines.len()) as u32;
            sheet.write_with_format(total_row, 4, "Subtotal", &bold_right)?;
            sheet.write_number_with_format(
                total_row,
                5,
                section.subtotal(),
                &Format::new().set_bold().set_num_format("#,##0.00"),
            )?;
            sheet.autofit();
        }

        workbook.save(path)?;
        Ok(path.to_path_buf())
    }
}

/// Trim/replace characters disallowed by Excel sheet names so that
/// users naming a discipline "[Phase 1] Arch/Struct" doesn't crash the
/// exporter. Limit to 31 chars, replace forbidden characters with `-`.
fn sanitize_sheet_name(raw: &str) -> String {
    const FORBIDDEN: &[char] = &['\\', '/', '?', '*', '[', ']', ':'];
    let mut s: String = raw
        .chars()
        .map(|c| if FORBIDDEN.contains(&c) { '-' } else { c })
        .collect();
    if s.is_empty() {
        s.push_str("Sheet");
    }
    if s.len() > 31 {
        s.truncate(31);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample_boq(region: RegionalConfig) -> BoqExport {
        let mut boq = BoqExport::new("Apartment 12B", region);
        boq.push_section(
            "Architecture",
            vec![
                BoqLine {
                    code: "A.01".into(),
                    description: "Partition walls".into(),
                    quantity: 45.0,
                    unit: QuantityUnit::Area,
                    rate: 120.0,
                },
                BoqLine {
                    code: "A.02".into(),
                    description: "Internal doors".into(),
                    quantity: 6.0,
                    unit: QuantityUnit::Each,
                    rate: 380.0,
                },
            ],
        );
        boq.push_section(
            "Finishes",
            vec![BoqLine {
                code: "F.01".into(),
                description: "Engineered oak floor".into(),
                quantity: 82.5,
                unit: QuantityUnit::Area,
                rate: 95.0,
            }],
        );
        boq
    }

    #[test]
    fn unit_labels_change_with_region() {
        assert_eq!(QuantityUnit::Area.label(RegionalConfig::Eu), "m²");
        assert_eq!(QuantityUnit::Area.label(RegionalConfig::Na), "ft²");
        assert_eq!(QuantityUnit::Length.label(RegionalConfig::Apac), "m");
        assert_eq!(QuantityUnit::Volume.label(RegionalConfig::Na), "ft³");
    }

    #[test]
    fn region_metadata_is_distinct() {
        assert_ne!(RegionalConfig::Eu.currency(), RegionalConfig::Na.currency());
        assert_ne!(
            RegionalConfig::Eu.standards_reference(),
            RegionalConfig::Apac.standards_reference()
        );
    }

    #[test]
    fn totals_match_line_arithmetic() {
        let boq = sample_boq(RegionalConfig::Eu);
        // Architecture: 45*120 + 6*380 = 5400 + 2280 = 7680
        assert!((boq.sections[0].subtotal() - 7680.0).abs() < 0.001);
        // Finishes: 82.5 * 95 = 7837.5
        assert!((boq.sections[1].subtotal() - 7837.5).abs() < 0.001);
        // Total: 7680 + 7837.5 = 15517.5
        assert!((boq.total() - 15517.5).abs() < 0.001);
    }

    #[test]
    fn xlsx_output_is_a_zip_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boq.xlsx");
        sample_boq(RegionalConfig::Eu).to_xlsx(&path).unwrap();

        let mut f = std::fs::File::open(&path).unwrap();
        let mut header = [0u8; 2];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"PK");

        let file = std::fs::File::open(&path).unwrap();
        let archive = zip::ZipArchive::new(file).unwrap();
        assert!(
            archive.len() > 5,
            "xlsx archive should have several entries"
        );
    }

    #[test]
    fn empty_boq_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.xlsx");
        let boq = BoqExport::new("Empty", RegionalConfig::Eu);
        let err = boq.to_xlsx(&path).unwrap_err();
        assert!(matches!(err, BoqExportError::EmptyBoq));
    }

    #[test]
    fn sheet_names_are_sanitized() {
        assert_eq!(sanitize_sheet_name("Arch/Struct"), "Arch-Struct");
        assert_eq!(sanitize_sheet_name(""), "Sheet");
        let long = "Discipline name that is way too long for excel sheet name limit";
        let sanitized = sanitize_sheet_name(long);
        assert_eq!(sanitized.len(), 31);
    }
}
