//! Client-deliverable exports.
//!
//! This crate produces the PDFs that ship at the end of every AEC Studio
//! project — proposal packs, schedules, mood-board summaries.

pub mod pdf;
pub mod proposal;
pub mod schedule;

pub use pdf::{PdfBuilder, PdfBuilderError};
pub use proposal::{ProposalAssets, ProposalPack};
pub use schedule::{ScheduleColumn, ScheduleRow, ScheduleSheet};
