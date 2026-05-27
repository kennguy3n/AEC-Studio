//! Client-deliverable exports.
//!
//! This crate produces the PDFs that ship at the end of every AEC Studio
//! project — proposal packs, schedules, mood-board summaries.

pub mod before_after;
pub mod bim_pack;
pub mod boq;
pub mod contractor_pack;
pub mod extension_targets;
pub mod gltf_export;
pub mod interior_pack;
pub mod pdf;
pub mod pdf_sheet;
pub mod plot_style;
pub mod project_export;
pub mod proposal;
pub mod schedule;
pub mod svg_export;
pub mod xlsx;

pub use before_after::{
    BeforeAfterPdfError, BeforeAfterPdfOptions, BeforeAfterRenderPair, BeforeAfterReport,
    BeforeAfterReportError, PlanOverlay, PlanOverlayCounts, PlanOverlayLevel, PlanOverlaySegment,
};
pub use bim_pack::{BimPack, BimPackError, ValidationReport, ValidationReportKind};
pub use boq::{BoqExport, BoqExportError, BoqLine, BoqSection, RegionalConfig};
pub use contractor_pack::{
    ContractorPack, ContractorPackError, ManifestEntry, PackFile, PackManifest,
};
pub use extension_targets::{
    list_extension_export_targets, resolve_export_target, ExportFormat as ExtensionExportFormat,
    ExportTargetExtensionError, ExtensionExportTarget,
};
pub use gltf_export::{
    write_gltf, GltfCamera, GltfExportError, GltfFormat, GltfLight, GltfLightKind, GltfMaterial,
    GltfMesh, GltfScene, WriteGltfOptions, WriteGltfResult,
};
pub use interior_pack::{InteriorPack, InteriorPackError, InteriorRender};
pub use pdf::{PdfBuilder, PdfBuilderError};
pub use pdf_sheet::SheetPdfBuilder;
pub use plot_style::{PlotStyle, PlotStyleTable};
// The legacy `write_deliver_pack` / `write_proposal_pack` entry
// points are kept in the public API (some downstream tests and
// examples still call the no-context variant) but are
// `#[deprecated]` (Phase 13 Tasks 7 + 10) so any new production
// caller is caught at build time. The re-export itself triggers
// the `deprecated` lint because it names the deprecated items;
// scope the allow to this `pub use` block so any other
// accidental use elsewhere in the crate still fires the warning.
#[allow(deprecated)]
pub use project_export::{
    write_deliver_pack, write_deliver_pack_with_context, write_project_dxf, write_project_gltf,
    write_project_ifc, write_project_package_zip, write_project_pdf, write_proposal_pack,
    write_proposal_pack_with_context, DeliverPackContext, DeliverPackKind, DeliverPackOptions,
    ProjectExportError, WriteDeliverPackResult, WriteProjectDxfResult, WriteProjectGltfResult,
    WriteProjectIfcResult, WriteProjectPackageResult, WriteProjectPdfResult,
    WriteProposalPackResult,
};
pub use proposal::{
    ProposalAssets, ProposalBranding, ProposalPack, ProposalPageOrder, RenderAttachment,
};
pub use schedule::{ScheduleColumn, ScheduleRow, ScheduleSheet};
pub use svg_export::{
    render_sheet_svg, render_sheet_svg_full, BlockTable, DimStyleTable, LayerTable, SvgExportError,
    SvgExportOptions, SvgLinetype, BLOCK_EXPANSION_MAX_DEPTH,
};
pub use xlsx::XlsxExportError;
