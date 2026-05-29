//! XLSX read-back for previously-written schedules.
//!
//! `ScheduleSheet::write_xlsx` (in
//! [`crate::schedules::xlsx`]) writes a schedule out to disk as a
//! standard OOXML workbook via `rust_xlsxwriter`. Once the file is on
//! disk it becomes the ground truth — re-deriving rows from the
//! in-memory `ScheduleSheet` would silently skip any post-processing
//! (manual edits, future schedule extensions that mutate cells before
//! save) the user may have done. This module reads the file back so
//! every UI surface that wants to render the data — the renderer's
//! `ScheduleView` table, BIM round-trip tests, future report
//! generators — agrees on what's actually in the workbook.
//!
//! The implementation uses [`calamine`], which is a pure-Rust OOXML
//! reader (no native deps, no Office runtime). We only target the
//! schedule files that `ScheduleSheet::write_xlsx` produces:
//!
//! - First worksheet only (single-sheet workbooks are what
//!   `write_xlsx` writes; multi-sheet workbooks from
//!   `write_xlsx_multi` are also supported by reading whichever sheet
//!   the caller names).
//! - Row 0 is the header — column display names verbatim, in the
//!   same order `ScheduleColumn::display` was written.
//! - Rows 1..N are body rows. Cells preserve their textual form
//!   because the writer in [`crate::schedules::xlsx::write_into`]
//!   uses `write_string_with_format` for every cell (no numeric or
//!   date typing), so round-tripping back into strings is loss-free.
//!
//! Anything else (e.g. cells the writer rendered as numbers/dates
//! via custom formats, or extra worksheets the writer never
//! produces) is best-effort: numeric cells become their decimal
//! string form, dates fall back to the calamine display, and empty
//! cells become empty strings — never `null` — so the renderer can
//! treat every row as a uniform `Record<string, string>`.

use std::collections::BTreeMap;
use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx, XlsxError};

/// Errors surfaced by [`read_xlsx_rows`]. These are distinct from
/// [`crate::schedules::ScheduleError`] because read-back is a
/// separate workflow (the writer's errors are about *producing*
/// XLSX; these errors are about *consuming* one).
#[derive(Debug, thiserror::Error)]
pub enum XlsxReadError {
    /// The file could not be opened at all — wrong path, unreadable,
    /// or not actually an XLSX (calamine returns `Xlsx(Zip(...))`
    /// for non-zip files).
    #[error("open xlsx '{path}': {source}")]
    Open {
        path: String,
        #[source]
        source: XlsxError,
    },

    /// The workbook is structurally valid but has no worksheets, or
    /// the named worksheet doesn't exist.
    #[error("xlsx has no worksheets (or sheet '{requested:?}' not found)")]
    NoWorksheet { requested: Option<String> },

    /// A worksheet exists but is completely empty (no rows). This
    /// is distinct from a header-only sheet, which is *allowed* and
    /// returns `rows: vec![]`.
    #[error("xlsx worksheet '{sheet}' has no header row")]
    NoHeader { sheet: String },

    /// A row has more cells than the header has columns, which would
    /// silently drop data on the renderer side. We surface it as an
    /// error so the upstream writer drift is visible immediately
    /// rather than presenting truncated rows.
    #[error("xlsx row {row_index} has {row_cells} cells but header has {header_cells}")]
    RowWiderThanHeader {
        row_index: usize,
        row_cells: usize,
        header_cells: usize,
    },

    /// `calamine` returned an error while iterating cells.
    #[error("xlsx iteration error: {0}")]
    Iter(#[from] XlsxError),
}

/// One body row read back from an XLSX schedule, keyed by header
/// display name. Insertion order is preserved by `BTreeMap` only as
/// an alphabetical convention — when the column order matters (e.g.
/// for rendering), the caller should iterate `header_columns` (also
/// returned by [`read_xlsx_rows_with_header`]).
pub type ScheduleRowMap = BTreeMap<String, String>;

/// Convenience wrapper around [`read_xlsx_rows_with_header`] that
/// discards the header column order and returns only the rows. Use
/// this when the column order is already known on the caller side
/// (e.g. the renderer already has the column metadata from the
/// `BimScheduleSummary` it received from `bimGenerateSchedule`).
pub fn read_xlsx_rows(path: &Path) -> Result<Vec<ScheduleRowMap>, XlsxReadError> {
    let (_columns, rows) = read_xlsx_rows_with_header(path, None)?;
    Ok(rows)
}

/// Read the first worksheet of an XLSX file as a vector of row maps.
///
/// `sheet_name` selects a specific worksheet (case-sensitive, matched
/// against `Workbook::sheet_names`). When `None`, the first sheet in
/// the workbook is used.
///
/// Returns `(header_columns, rows)`:
/// - `header_columns` is the ordered list of display names from row
///   0 — useful for the caller to reconstruct the column order when
///   `ScheduleRowMap` (a `BTreeMap`) alphabetizes the keys.
/// - `rows` is one [`ScheduleRowMap`] per body row. Every map has
///   exactly `header_columns.len()` entries, with empty cells filled
///   in as `""` so the renderer can treat every row as a uniform
///   `Record<string, string>`.
pub fn read_xlsx_rows_with_header(
    path: &Path,
    sheet_name: Option<&str>,
) -> Result<(Vec<String>, Vec<ScheduleRowMap>), XlsxReadError> {
    let mut workbook: Xlsx<_> = open_workbook(path).map_err(|source| XlsxReadError::Open {
        path: path.display().to_string(),
        source,
    })?;

    // Resolve the target sheet — caller-named, else first.
    let target_sheet = match sheet_name {
        Some(name) => {
            if workbook.sheet_names().iter().any(|n| n.as_str() == name) {
                name.to_string()
            } else {
                return Err(XlsxReadError::NoWorksheet {
                    requested: Some(name.to_string()),
                });
            }
        }
        None => workbook
            .sheet_names()
            .first()
            .cloned()
            .ok_or(XlsxReadError::NoWorksheet { requested: None })?,
    };

    let range = workbook
        .worksheet_range(&target_sheet)
        .map_err(XlsxReadError::Iter)?;

    let mut rows_iter = range.rows();
    let header_row = rows_iter.next().ok_or_else(|| XlsxReadError::NoHeader {
        sheet: target_sheet.clone(),
    })?;
    let header: Vec<String> = header_row.iter().map(cell_to_string).collect();

    let header_cells = header.len();
    let mut rows: Vec<ScheduleRowMap> = Vec::new();
    for (idx, row) in rows_iter.enumerate() {
        if row.iter().all(|c| matches!(c, Data::Empty)) {
            continue;
        }
        if row.len() > header_cells {
            return Err(XlsxReadError::RowWiderThanHeader {
                row_index: idx + 1,
                row_cells: row.len(),
                header_cells,
            });
        }
        let mut map = ScheduleRowMap::new();
        for (col_idx, col_name) in header.iter().enumerate() {
            let value = row.get(col_idx).map_or_else(String::new, cell_to_string);
            map.insert(col_name.clone(), value);
        }
        rows.push(map);
    }

    Ok((header, rows))
}

/// Convert a calamine cell to its textual representation, matching
/// what the writer in [`crate::schedules::xlsx::write_into`] would
/// have written. Numeric cells get a stable decimal representation
/// (no scientific notation for normal magnitudes); booleans become
/// `"TRUE"`/`"FALSE"` (matching Excel's display); empty cells become
/// `""`.
fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERR:{e:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedules::{ScheduleColumn, ScheduleSheet};
    use tempfile::tempdir;

    fn make_sheet() -> ScheduleSheet {
        let mut sheet = ScheduleSheet::new(
            "Room Schedule",
            vec![
                ScheduleColumn {
                    key: "name".into(),
                    display: "Name".into(),
                },
                ScheduleColumn {
                    key: "area".into(),
                    display: "Area (m²)".into(),
                },
                ScheduleColumn {
                    key: "occupancy".into(),
                    display: "Occupancy".into(),
                },
            ],
        );
        sheet.push_row(vec!["Living".into(), "28.5".into(), "Family".into()]);
        sheet.push_row(vec!["Kitchen".into(), "12.1".into(), "Family".into()]);
        sheet.push_row(vec!["Bath".into(), "5.4".into(), "Family".into()]);
        sheet
    }

    #[test]
    fn round_trip_preserves_rows_and_columns() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rooms.xlsx");
        let sheet = make_sheet();
        sheet.write_xlsx(&path).unwrap();

        let (columns, rows) = read_xlsx_rows_with_header(&path, None).unwrap();
        assert_eq!(columns, vec!["Name", "Area (m²)", "Occupancy"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get("Name").unwrap(), "Living");
        assert_eq!(rows[0].get("Area (m²)").unwrap(), "28.5");
        assert_eq!(rows[1].get("Name").unwrap(), "Kitchen");
        assert_eq!(rows[2].get("Name").unwrap(), "Bath");
    }

    #[test]
    fn read_xlsx_rows_convenience_returns_same_rows() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rooms.xlsx");
        make_sheet().write_xlsx(&path).unwrap();

        let rows = read_xlsx_rows(&path).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get("Name").unwrap(), "Living");
    }

    #[test]
    fn empty_schedule_yields_header_only() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("empty.xlsx");
        let sheet = ScheduleSheet::new(
            "Empty",
            vec![ScheduleColumn {
                key: "name".into(),
                display: "Name".into(),
            }],
        );
        sheet.write_xlsx(&path).unwrap();

        let (columns, rows) = read_xlsx_rows_with_header(&path, None).unwrap();
        assert_eq!(columns, vec!["Name"]);
        assert!(rows.is_empty());
    }

    #[test]
    fn open_returns_typed_error_for_missing_file() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.xlsx");
        let err = read_xlsx_rows(&missing).unwrap_err();
        assert!(matches!(err, XlsxReadError::Open { .. }));
    }

    #[test]
    fn missing_sheet_returns_typed_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rooms.xlsx");
        make_sheet().write_xlsx(&path).unwrap();
        let err = read_xlsx_rows_with_header(&path, Some("nope")).unwrap_err();
        assert!(matches!(
            err,
            XlsxReadError::NoWorksheet { requested: Some(ref n) } if n == "nope"
        ));
    }

    #[test]
    fn multi_sheet_workbook_reads_named_sheet() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("multi.xlsx");

        let mut sheet_a = ScheduleSheet::new(
            "Rooms",
            vec![ScheduleColumn {
                key: "name".into(),
                display: "Name".into(),
            }],
        );
        sheet_a.push_row(vec!["Living".into()]);

        let mut sheet_b = ScheduleSheet::new(
            "Doors",
            vec![ScheduleColumn {
                key: "id".into(),
                display: "ID".into(),
            }],
        );
        sheet_b.push_row(vec!["D-01".into()]);
        sheet_b.push_row(vec!["D-02".into()]);

        ScheduleSheet::write_xlsx_multi(&[sheet_a, sheet_b], &path).unwrap();

        let (cols_a, rows_a) = read_xlsx_rows_with_header(&path, Some("Rooms")).unwrap();
        assert_eq!(cols_a, vec!["Name"]);
        assert_eq!(rows_a.len(), 1);

        let (cols_b, rows_b) = read_xlsx_rows_with_header(&path, Some("Doors")).unwrap();
        assert_eq!(cols_b, vec!["ID"]);
        assert_eq!(rows_b.len(), 2);
        assert_eq!(rows_b[1].get("ID").unwrap(), "D-02");
    }
}
