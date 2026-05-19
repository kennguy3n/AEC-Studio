//! N-API bridge for AEC Studio.
//!
//! The Electron main process loads `aec_bridge.node` (built with the
//! `napi` feature) and calls the functions exposed in [`napi_api`].
//! Those functions delegate to [`service`], which is plain Rust and
//! straightforward to unit-test.

pub mod recents;
pub mod service;

#[cfg(feature = "napi")]
pub mod napi_api;

pub use recents::{RecentsStore, RecentsStoreError};
pub use service::{
    BridgeService, BridgeServiceError, ProjectSummary, RuntimeStatusReport, TemplateChoice,
};
