//! XLSX writer for schedules. Uses `rust_xlsxwriter` (pure-Rust, no
//! native deps).

use rust_xlsxwriter::{Format, FormatBorder, Workbook};

use super::{ScheduleError, ScheduleSheet};

pub(crate) fn write_sheet(
    sheet: &ScheduleSheet,
    path: &std::path::Path,
) -> Result<(), ScheduleError> {
    let mut wb = Workbook::new();
    write_into(&mut wb, sheet)?;
    wb.save(path)?;
    Ok(())
}

pub(crate) fn write_multi_sheet(
    sheets: &[ScheduleSheet],
    path: &std::path::Path,
) -> Result<(), ScheduleError> {
    let mut wb = Workbook::new();
    for s in sheets {
        write_into(&mut wb, s)?;
    }
    wb.save(path)?;
    Ok(())
}

fn write_into(wb: &mut Workbook, sheet: &ScheduleSheet) -> Result<(), ScheduleError> {
    let ws = wb.add_worksheet();
    let safe_name = sanitize_sheet_name(&sheet.title);
    ws.set_name(&safe_name)?;
    let header = Format::new()
        .set_bold()
        .set_background_color("#E0E0E0")
        .set_border(FormatBorder::Thin);
    let body = Format::new().set_border(FormatBorder::Thin);
    for (c, col) in sheet.columns.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, &col.display, &header)?;
    }
    for (r, row) in sheet.rows.iter().enumerate() {
        for (c, cell) in row.cells.iter().enumerate() {
            ws.write_string_with_format((r + 1) as u32, c as u16, cell, &body)?;
        }
    }
    for c in 0..sheet.columns.len() {
        ws.set_column_width(c as u16, 18)?;
    }
    Ok(())
}

/// Excel sheet names cannot contain `[ ] : * ? / \` and must be ≤ 31
/// chars.
fn sanitize_sheet_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => ' ',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    let mut out: String = trimmed.chars().take(31).collect();
    if out.is_empty() {
        out.push_str("Sheet");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedules::{ScheduleColumn, ScheduleSheet};

    #[test]
    fn writes_simple_workbook() {
        let mut s = ScheduleSheet::new(
            "Rooms",
            vec![
                ScheduleColumn {
                    key: "id".into(),
                    display: "ID".into(),
                },
                ScheduleColumn {
                    key: "name".into(),
                    display: "Name".into(),
                },
            ],
        );
        s.push_row(vec!["101".into(), "Office".into()]);
        s.push_row(vec!["102".into(), "Kitchen".into()]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rooms.xlsx");
        s.write_xlsx(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 200, "xlsx file should be non-trivial");
        // XLSX files start with PK\u{0003}\u{0004} (ZIP magic).
        assert_eq!(&bytes[0..2], b"PK");
    }

    #[test]
    fn sanitizes_sheet_names() {
        assert_eq!(sanitize_sheet_name("A/B*C?"), "A B C");
        assert_eq!(sanitize_sheet_name(""), "Sheet");
        let long = "x".repeat(40);
        assert_eq!(sanitize_sheet_name(&long).chars().count(), 31);
    }

    #[test]
    fn multi_sheet_writes_all() {
        let a = ScheduleSheet::new(
            "A",
            vec![ScheduleColumn {
                key: "id".into(),
                display: "ID".into(),
            }],
        );
        let b = ScheduleSheet::new(
            "B",
            vec![ScheduleColumn {
                key: "id".into(),
                display: "ID".into(),
            }],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.xlsx");
        ScheduleSheet::write_xlsx_multi(&[a, b], &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 200);
    }
}
