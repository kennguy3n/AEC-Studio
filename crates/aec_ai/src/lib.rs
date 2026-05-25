//! Local AI sidecar runtime.
//!
//! `aec_ai` is the only crate that knows the AI sidecar exists. Domain
//! crates produce structured tool-call requests (typed by `tool_schema`)
//! and consume previewable diffs (`diff_engine`). Everything that goes
//! out to the sidecar passes through `safety_validator`; everything that
//! comes back is logged by `audit`.

pub mod audit;
pub mod bim_classification;
pub mod cad_cleanup;
pub mod cover_draft;
pub mod diff_engine;
pub mod extension_tools;
pub mod grammars;
pub mod http;
pub mod layout_suggestion;
pub mod lighting_balance;
pub mod plan_detection;
pub mod plan_to_wall;
pub mod planner;
pub mod property_fill;
pub mod render_doctor;
pub mod runtime;
pub mod safety_validator;
pub mod schedule_fill;
pub mod sidecar;
pub mod style_assistant;
pub mod tool_schema;
pub mod transport;
pub mod validation_help;

pub use audit::{AiAuditLogger, AiAuditRecord};
pub use bim_classification::{
    classify as bim_classify, ClassificationConfig, ClassificationProposal, ClassificationResult,
    GeometryFeatures,
};
pub use cad_cleanup::{
    build_cleanup_proposal, close_gaps, dedupe_entities, merge_collinear, normalize_layers,
    CleanupConfig, CleanupProposal, CollinearMerge, DuplicateGroup, GapClosure, LayerPolicyRule,
    LayerReassignment, LineEntity,
};
pub use cover_draft::{CoverDraftError, CoverDraftTone, CoverPageDraft};
pub use diff_engine::{Diff, DiffEngine, DiffOperation, DiffStatus};
pub use grammars::{Grammar, GrammarRegistry};
pub use layout_suggestion::{LayoutProposal, LayoutSuggestionResult, LayoutValidationError};
pub use lighting_balance::{
    AccentLight, AccentLightKind, LightingBalanceError, LightingBalanceResult, MAX_ACCENT_LIGHTS,
};
pub use plan_detection::{PlanDetectionResult, PolylineProposal};
pub use plan_to_wall::{
    convert as plan_to_wall_convert, convert_from_polylines as plan_to_wall_from_polylines,
    PlanToWallConfig, PlanToWallResult, Wall, WallAxisSegment, WallEndCap,
};
pub use planner::{PlanError, PlanRequest, PlanResponse, ToolPlanner};
pub use property_fill::{
    fill_properties, ElementContext, ProjectStandards, PropertyFillConfig, PropertyFillResult,
    PropertyProposal, StandardRule,
};
pub use render_doctor::{RenderDoctorFinding, RenderDoctorResult, RenderIssue};
pub use runtime::{RuntimeConfig, RuntimeError, RuntimeState, SidecarRuntime};
pub use safety_validator::{SafetyError, SafetyValidator, SafetyViolation};
pub use schedule_fill::{
    FilledRow, ScheduleFillError, ScheduleFillResult, REVIEW_CONFIDENCE_THRESHOLD,
};
pub use sidecar::{SidecarHandle, SidecarSpawnError};
pub use style_assistant::{StyleAssistantResult, StyleSuggestion};
pub use tool_schema::{ToolName, ToolSchema, ToolSchemaRegistry};
pub use transport::{
    CompletionRequest, CompletionResponse, SidecarTransport, TransportError,
    DEFAULT_HEALTH_TIMEOUT, DEFAULT_MAX_PREDICT, DEFAULT_TEMPERATURE,
};
pub use validation_help::{FixAction, ValidationFix, ValidationHelpError, ValidationHelpResult};
