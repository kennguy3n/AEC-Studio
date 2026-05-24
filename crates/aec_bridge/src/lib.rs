//! N-API bridge for AEC Studio.
//!
//! The Electron main process loads `aec_bridge.node` (built with the
//! `napi` feature) and calls the functions exposed in [`napi_api`].
//! Those functions delegate to [`service`], which is plain Rust and
//! straightforward to unit-test.

mod engine_status_cache;
pub mod recents;
pub mod service;

#[cfg(feature = "napi")]
pub mod napi_api;

pub use recents::{RecentsStore, RecentsStoreError};
pub use service::{
    BridgeConfig, BridgeService, BridgeServiceError, EngineStatusReport, ProjectSummary,
    RuntimeStatusReport, TemplateChoice,
};
