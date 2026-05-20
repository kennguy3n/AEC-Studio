//! Client-deliverable exports.
//!
//! This crate produces the PDFs that ship at the end of every AEC Studio
//! project — proposal packs, schedules, mood-board summaries.

pub mod pdf;
pub mod pdf_sheet;
pub mod plot_style;
pub mod proposal;
pub mod schedule;
pub mod svg_export;

pub use pdf::{PdfBuilder, PdfBuilderError};
pub use pdf_sheet::SheetPdfBuilder;
pub use plot_style::{PlotStyle, PlotStyleTable};
pub use proposal::{ProposalAssets, ProposalPack};
pub use schedule::{ScheduleColumn, ScheduleRow, ScheduleSheet};
pub use svg_export::{render_sheet_svg, SvgExportError, SvgExportOptions};
