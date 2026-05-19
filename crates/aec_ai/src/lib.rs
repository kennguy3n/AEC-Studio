//! Local AI sidecar runtime.
//!
//! `aec_ai` is the only crate that knows the AI sidecar exists. Domain
//! crates produce structured tool-call requests (typed by `tool_schema`)
//! and consume previewable diffs (`diff_engine`). Everything that goes
//! out to the sidecar passes through `safety_validator`; everything that
//! comes back is logged by `audit`.

pub mod audit;
pub mod diff_engine;
pub mod grammars;
pub mod plan_detection;
pub mod planner;
pub mod render_doctor;
pub mod runtime;
pub mod safety_validator;
pub mod style_assistant;
pub mod tool_schema;

pub use audit::{AiAuditLogger, AiAuditRecord};
pub use diff_engine::{Diff, DiffEngine, DiffOperation, DiffStatus};
pub use grammars::{Grammar, GrammarRegistry};
pub use plan_detection::{PlanDetectionResult, PolylineProposal};
pub use planner::{PlanRequest, PlanResponse, ToolPlanner};
pub use render_doctor::{RenderDoctorFinding, RenderDoctorResult, RenderIssue};
pub use runtime::{RuntimeConfig, RuntimeError, RuntimeState, SidecarRuntime};
pub use safety_validator::{SafetyError, SafetyValidator, SafetyViolation};
pub use style_assistant::{StyleAssistantResult, StyleSuggestion};
pub use tool_schema::{ToolName, ToolSchema, ToolSchemaRegistry};
