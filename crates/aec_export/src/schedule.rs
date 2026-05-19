//! Material / furniture schedules formatted as tabular PDF pages.

use serde::{Deserialize, Serialize};

use crate::pdf::{PageSize, PdfBuilder, PdfBuilderError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleColumn {
    pub key: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRow {
    pub cells: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleSheet {
    pub title: String,
    pub columns: Vec<ScheduleColumn>,
    pub rows: Vec<ScheduleRow>,
}

impl ScheduleSheet {
    pub fn material_schedule_template() -> Self {
        Self {
            title: "Material schedule".into(),
            columns: vec![
                ScheduleColumn {
                    key: "id".into(),
                    display_name: "ID".into(),
                },
                ScheduleColumn {
                    key: "name".into(),
                    display_name: "Name".into(),
                },
                ScheduleColumn {
                    key: "location".into(),
                    display_name: "Location".into(),
                },
                ScheduleColumn {
                    key: "qty".into(),
                    display_name: "Qty".into(),
                },
                ScheduleColumn {
                    key: "vendor".into(),
                    display_name: "Vendor".into(),
                },
            ],
            rows: Vec::new(),
        }
    }

    pub fn furniture_schedule_template() -> Self {
        Self {
            title: "Furniture schedule".into(),
            columns: vec![
                ScheduleColumn {
                    key: "id".into(),
                    display_name: "ID".into(),
                },
                ScheduleColumn {
                    key: "asset".into(),
                    display_name: "Asset".into(),
                },
                ScheduleColumn {
                    key: "room".into(),
                    display_name: "Room".into(),
                },
                ScheduleColumn {
                    key: "qty".into(),
                    display_name: "Qty".into(),
                },
                ScheduleColumn {
                    key: "notes".into(),
                    display_name: "Notes".into(),
                },
            ],
            rows: Vec::new(),
        }
    }

    pub fn push_row(&mut self, cells: impl IntoIterator<Item = impl Into<String>>) {
        self.rows.push(ScheduleRow {
            cells: cells.into_iter().map(Into::into).collect(),
        });
    }

    /// Write the schedule out as a standalone single-page PDF.
    pub fn to_pdf(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<std::path::PathBuf, PdfBuilderError> {
        let mut b = PdfBuilder::new(&self.title, PageSize::A4_PORTRAIT)?;
        let headers: Vec<String> = self
            .columns
            .iter()
            .map(|c| c.display_name.clone())
            .collect();
        let rows: Vec<Vec<String>> = self.rows.iter().map(|r| r.cells.clone()).collect();
        b.add_table_page(&self.title, &headers, &rows)?;
        b.save(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_schedule_writes_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = ScheduleSheet::material_schedule_template();
        s.push_row(["MAT-001", "Oak floor", "Living room", "12 m²", "Acme"]);
        s.push_row(["MAT-002", "Linen cushion", "Living room", "4", "Casa"]);
        let path = s.to_pdf(dir.path().join("schedule.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }
}
