//! Tool planner: builds [`PlanRequest`]s from domain calls, dispatches them
//! through the runtime, and surfaces a typed [`PlanResponse`].
//!
//! The planner is the only place that knows how to format the JSON envelope
//! sent to llama.cpp; everything outside this module talks in typed structs.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_core::types::Scope;

use crate::grammars::GrammarRegistry;
use crate::safety_validator::{SafetyError, SafetyValidator, ValidationContext};
use crate::tool_schema::{ToolName, ToolSchemaRegistry};

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("safety violation: {0}")]
    Safety(#[from] SafetyError),
    #[error("sidecar offline")]
    Offline,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRequest {
    pub tool: ToolName,
    pub scope: Scope,
    /// Free-form prompt fragment passed to the model — what we say.
    pub prompt: String,
    /// Caller-provided JSON context (already sanitized).
    pub context: serde_json::Value,
    pub max_entities_modified: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanResponse {
    pub tool: ToolName,
    pub raw_payload: String,
    pub parsed: serde_json::Value,
    pub entities_modified: u32,
}

pub struct ToolPlanner<'a> {
    schemas: &'a ToolSchemaRegistry,
    grammars: &'a GrammarRegistry,
}

impl<'a> ToolPlanner<'a> {
    pub fn new(schemas: &'a ToolSchemaRegistry, grammars: &'a GrammarRegistry) -> Self {
        Self { schemas, grammars }
    }

    /// Validate that `request` is well-formed *before* it is dispatched.
    pub fn precheck(&self, request: &PlanRequest) -> Result<(), PlanError> {
        let Some(schema) = self.schemas.get(request.tool) else {
            return Err(SafetyError::UnknownTool(request.tool.as_str().into()).into());
        };
        if !schema.allowed_scopes.contains(&request.scope) {
            return Err(SafetyError::ScopeViolation {
                tool: request.tool.as_str().into(),
                scope: request.scope,
            }
            .into());
        }
        if request.max_entities_modified > schema.max_entities_modified {
            return Err(SafetyError::BoundsExceeded {
                tool: request.tool.as_str().into(),
                entities: request.max_entities_modified,
                max: schema.max_entities_modified,
            }
            .into());
        }
        Ok(())
    }

    /// Wrap a raw sidecar response into a typed [`PlanResponse`] after
    /// running it through the [`SafetyValidator`].
    pub fn finalize(
        &self,
        tool: ToolName,
        scope: Scope,
        entities_modified: u32,
        raw_payload: String,
    ) -> Result<PlanResponse, PlanError> {
        let validator = SafetyValidator::new(self.schemas, self.grammars);
        let ctx = ValidationContext {
            scope,
            tool,
            entities_modified,
            payload: raw_payload.clone(),
        };
        validator.validate(&ctx)?;
        let parsed: serde_json::Value = serde_json::from_str(&raw_payload)
            .map_err(|e| SafetyError::Malformed(e.to_string()))?;
        Ok(PlanResponse {
            tool,
            raw_payload,
            parsed,
            entities_modified,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precheck_rejects_unknown_tool() {
        let s = ToolSchemaRegistry::new();
        let g = GrammarRegistry::defaults();
        let planner = ToolPlanner::new(&s, &g);
        let request = PlanRequest {
            tool: ToolName::StyleAssistant,
            scope: Scope::Design,
            prompt: String::new(),
            context: serde_json::json!({}),
            max_entities_modified: 1,
        };
        assert!(planner.precheck(&request).is_err());
    }

    #[test]
    fn finalize_runs_safety_gate() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let planner = ToolPlanner::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let r = planner
            .finalize(ToolName::StyleAssistant, Scope::Design, 3, payload.into())
            .unwrap();
        assert_eq!(r.tool, ToolName::StyleAssistant);
        assert!(r.parsed.get("furniture_ids").is_some());
    }

    #[test]
    fn finalize_layout_suggestion_passes_safety_gate() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let planner = ToolPlanner::new(&s, &g);
        let payload = r#"{"room_anchor":"ent_living","proposals":[{"asset_id":"ast:sofa","position_mm":[1200.0,800.0,0.0],"rotation_deg":90.0}]}"#;
        let r = planner
            .finalize(ToolName::LayoutSuggestion, Scope::Design, 1, payload.into())
            .unwrap();
        assert_eq!(r.tool, ToolName::LayoutSuggestion);
        assert!(r.parsed.get("proposals").is_some());
    }

    #[test]
    fn finalize_rejects_layout_suggestion_with_old_style_assistant_shape() {
        // A payload that satisfies style_assistant must NOT pass the
        // layout_suggestion grammar — the two tools were intentionally
        // decoupled so the safety validator rejects shape drift.
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let planner = ToolPlanner::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = planner
            .finalize(ToolName::LayoutSuggestion, Scope::Design, 1, payload.into())
            .unwrap_err();
        assert!(matches!(err, PlanError::Safety(_)));
    }
}
