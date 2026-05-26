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

pub use before_after::{BeforeAfterPdfError, BeforeAfterPdfOptions, BeforeAfterRenderPair};
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
pub use project_export::{
    write_deliver_pack, write_project_dxf, write_project_gltf, write_project_ifc,
    write_project_package_zip, write_project_pdf, write_proposal_pack, DeliverPackKind,
    DeliverPackOptions, ProjectExportError, WriteDeliverPackResult, WriteProjectDxfResult,
    WriteProjectGltfResult, WriteProjectIfcResult, WriteProjectPackageResult,
    WriteProjectPdfResult, WriteProposalPackResult,
};
pub use proposal::{
    ProposalAssets, ProposalBranding, ProposalPack, ProposalPageOrder, RenderAttachment,
};
pub use schedule::{ScheduleColumn, ScheduleRow, ScheduleSheet};
pub use svg_export::{render_sheet_svg, SvgExportError, SvgExportOptions};
pub use xlsx::XlsxExportError;
