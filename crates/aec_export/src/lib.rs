//! Client-deliverable exports.
//!
//! This crate produces the PDFs that ship at the end of every AEC Studio
//! project — proposal packs, schedules, mood-board summaries.

pub mod before_after;
pub mod bim_pack;
pub mod boq;
pub mod contractor_pack;
pub mod interior_pack;
pub mod pdf;
pub mod pdf_sheet;
pub mod plot_style;
pub mod proposal;
pub mod schedule;
pub mod svg_export;
pub mod xlsx;

pub use before_after::{BeforeAfterPdfError, BeforeAfterPdfOptions, BeforeAfterRenderPair};
pub use bim_pack::{BimPack, BimPackError};
pub use boq::{BoqExport, BoqExportError, BoqLine, BoqSection, RegionalConfig};
pub use contractor_pack::{ContractorPack, ContractorPackError, PackManifest};
pub use interior_pack::{InteriorPack, InteriorPackError};
pub use pdf::{PdfBuilder, PdfBuilderError};
pub use pdf_sheet::SheetPdfBuilder;
pub use plot_style::{PlotStyle, PlotStyleTable};
pub use proposal::{ProposalAssets, ProposalPack};
pub use schedule::{ScheduleColumn, ScheduleRow, ScheduleSheet};
pub use svg_export::{render_sheet_svg, SvgExportError, SvgExportOptions};
pub use xlsx::XlsxExportError;
