//! BIM-mode schedules: room, door, window, and material.
//!
//! Schedules read directly from the project graph (`Project`,
//! `PropertyStore`) — they are projections, not stored data. Each
//! schedule renders to an [`ScheduleSheet`] which can be exported as
//! XLSX via [`ScheduleSheet::write_xlsx`].

pub mod door_schedule;
pub mod material_schedule;
pub mod room_schedule;
pub mod window_schedule;
pub mod xlsx;

pub use door_schedule::{generate_door_schedule, DoorScheduleEntry};
pub use material_schedule::{generate_material_schedule, MaterialScheduleEntry};
pub use room_schedule::{generate_room_schedule, RoomScheduleEntry};
pub use window_schedule::{generate_window_schedule, WindowScheduleEntry};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleColumn {
    pub key: String,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRow {
    /// Cells are aligned with `ScheduleSheet::columns`.
    pub cells: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleSheet {
    pub title: String,
    pub columns: Vec<ScheduleColumn>,
    pub rows: Vec<ScheduleRow>,
}

impl ScheduleSheet {
    pub fn new(title: impl Into<String>, columns: Vec<ScheduleColumn>) -> Self {
        Self {
            title: title.into(),
            columns,
            rows: Vec::new(),
        }
    }

    pub fn push_row(&mut self, cells: Vec<String>) {
        debug_assert_eq!(
            cells.len(),
            self.columns.len(),
            "schedule row must have one cell per column"
        );
        self.rows.push(ScheduleRow { cells });
    }

    /// Write this schedule to an XLSX file at `path`. Each row is one
    /// spreadsheet row; the header row uses the column `display` names.
    pub fn write_xlsx(&self, path: &std::path::Path) -> Result<(), ScheduleError> {
        xlsx::write_sheet(self, path)
    }

    /// Write multiple schedules to a single XLSX workbook, one sheet
    /// per schedule (sheet name = schedule `title`).
    pub fn write_xlsx_multi(
        sheets: &[ScheduleSheet],
        path: &std::path::Path,
    ) -> Result<(), ScheduleError> {
        xlsx::write_multi_sheet(sheets, path)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("xlsx error: {0}")]
    Xlsx(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<rust_xlsxwriter::XlsxError> for ScheduleError {
    fn from(e: rust_xlsxwriter::XlsxError) -> Self {
        ScheduleError::Xlsx(e.to_string())
    }
}
