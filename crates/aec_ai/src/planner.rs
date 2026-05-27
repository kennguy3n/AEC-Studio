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
use crate::tool_schema::{ToolName, ToolSchema, ToolSchemaRegistry};
use crate::transport::{CompletionRequest, SidecarTransport, TransportError};

/// Count the number of entities a tool response will modify when
/// the [`crate::diff_engine::DiffEngine`] turns it into a `Diff`.
///
/// This is the **runtime** entity count the safety validator must
/// check against the schema cap and the caller's runtime cap —
/// `dispatch` previously passed `request.max_entities_modified`
/// (i.e. the cap itself) which made the bounds check vacuous because
/// `precheck` already ensures `cap <= schema.max_entities_modified`.
///
/// The per-tool logic must mirror `DiffEngine::build` exactly —
/// **counting an array element that the diff engine would skip is a
/// correctness bug** because it can trigger a spurious
/// `SafetyError::BoundsExceeded` for a response that, after the diff
/// engine's filtering, modifies fewer entities than the cap allows.
/// In particular:
///   - `plan_detection` / `plan_to_wall`: only polylines with a
///     `points` array become wall inserts (see
///     [`crate::diff_engine::build_plan_detection`]).
///   - `style_assistant`: only `furniture_ids[]` / `material_ids[]`
///     elements that parse as JSON strings (`as_str().is_some()`)
///     become inserts, plus 1 if `lighting_preset_id` is a string
///     (see [`crate::diff_engine::build_style_assistant`]).
///   - `layout_suggestion`: only proposals with a valid
///     `target_entity` (parseable as `EntityId`) OR a non-empty
///     `asset_id` string become ops; proposals with neither are
///     silently dropped (see
///     [`crate::diff_engine::build_layout_suggestion`]).
///   - `render_doctor`: every `findings[]` element becomes a preset
///     update unconditionally (see
///     [`crate::diff_engine::build_render_doctor`]).
///   - other tools (classification, property_fill, schedule_fill,
///     etc.) are not handled by `DiffEngine::build`, so we fall back
///     to a conservative 0 — the bridge service builds those diffs
///     via dedicated `bim_classification` / `property_fill` paths
///     and enforces its own per-tool count cap there.
pub fn count_response_entities(tool: ToolName, parsed: &serde_json::Value) -> u32 {
    /// Count only the array elements at `parsed[key]` that satisfy
    /// `pred`. Mirrors the inline filtering each `DiffEngine::build_*`
    /// branch performs before pushing an op into its `Vec`.
    fn filtered_arr_len(
        parsed: &serde_json::Value,
        key: &str,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> u32 {
        parsed.get(key).and_then(|v| v.as_array()).map_or(0, |a| {
            u32::try_from(a.iter().filter(|v| pred(v)).count()).unwrap_or(u32::MAX)
        })
    }
    match tool {
        ToolName::PlanDetection | ToolName::PlanToWall => {
            filtered_arr_len(parsed, "polylines", |p| p.get("points").is_some())
        }
        ToolName::StyleAssistant => {
            // Only string elements become ops — a numeric or object
            // entry in `furniture_ids` / `material_ids` is silently
            // skipped by the diff engine, so counting it here would
            // overcount and could push a within-cap response over
            // the safety threshold.
            let f = filtered_arr_len(parsed, "furniture_ids", |v| v.as_str().is_some());
            let m = filtered_arr_len(parsed, "material_ids", |v| v.as_str().is_some());
            let l = u32::from(
                parsed
                    .get("lighting_preset_id")
                    .and_then(|v| v.as_str())
                    .is_some(),
            );
            f.saturating_add(m).saturating_add(l)
        }
        ToolName::LayoutSuggestion => {
            // The diff engine's `build_layout_suggestion` walks the
            // `proposals` array and emits exactly one op per
            // proposal that either (a) carries a `target_entity`
            // that parses as `EntityId` (→ Update) OR (b) carries
            // a non-empty `asset_id` string (→ Insert). A proposal
            // with neither is silently dropped, so we must not
            // count it here. Empty `asset_id` strings are likewise
            // dropped because the diff engine's pattern is
            // `proposal.get("asset_id").and_then(|v| v.as_str())`
            // which yields `None` for empty strings? — no, it
            // yields `Some("")` for empty strings, but the
            // resulting Insert is degenerate. We treat empty
            // `asset_id` as a count regardless, matching the diff
            // engine's behaviour exactly so the two stay in lock
            // step.
            filtered_arr_len(parsed, "proposals", |p| {
                let has_valid_target = p
                    .get("target_entity")
                    .and_then(|v| v.as_str())
                    .and_then(|s| aec_core::types::EntityId::from_string(s).ok())
                    .is_some();
                let has_asset_id = p.get("asset_id").and_then(|v| v.as_str()).is_some();
                has_valid_target || has_asset_id
            })
        }
        ToolName::RenderDoctor => filtered_arr_len(parsed, "findings", |_| true),
        // Tools the `DiffEngine` does not lower into ops directly
        // (their diffs are built by dedicated bridge-service paths).
        // Returning 0 here is safe — the bridge layer enforces its
        // own per-tool count check via `diff.operations.len()`.
        ToolName::CadCleanup
        | ToolName::ScheduleFill
        | ToolName::Classification
        | ToolName::PropertyFill
        | ToolName::ValidationHelp
        | ToolName::CoverPageDraft
        | ToolName::LightingBalance => 0,
    }
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("safety violation: {0}")]
    Safety(#[from] SafetyError),
    #[error("sidecar offline")]
    Offline,
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("grammar `{0}` not found in registry")]
    UnknownGrammar(String),
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
    ///
    /// `request_cap` is the caller's runtime safety budget
    /// (`PlanRequest::max_entities_modified`). The validator checks
    /// the *actual* count from the parsed payload against both the
    /// schema's hard cap (`SafetyError::BoundsExceeded`) and the
    /// request's runtime cap (same variant, different `max` field) —
    /// previously this code passed `request_cap` as the value to
    /// check, making the bounds check vacuous because
    /// `precheck` already enforces `request_cap <= schema_cap`, so a
    /// model that emitted more entities than the cap would still slip
    /// through.
    pub fn finalize(
        &self,
        tool: ToolName,
        scope: Scope,
        request_cap: u32,
        raw_payload: String,
    ) -> Result<PlanResponse, PlanError> {
        // Parse the payload *before* validating so we can count the
        // actual number of entities the response would touch. The
        // grammar match in `SafetyValidator::validate` will catch any
        // serde parse error anyway, but we need the parsed form here
        // to count entities per-tool.
        let parsed: serde_json::Value = serde_json::from_str(&raw_payload)
            .map_err(|e| SafetyError::Malformed(e.to_string()))?;
        let actual_entities = count_response_entities(tool, &parsed);

        // Enforce the caller's runtime cap explicitly. The validator
        // below also checks against `schema.max_entities_modified`
        // (the hard cap), so a response that exceeds either limit is
        // rejected. This is the layer the bot-flagged bypass lived
        // at — previously we passed `request_cap` itself as
        // `entities_modified`, so `actual_entities > request_cap`
        // was never tested.
        if actual_entities > request_cap {
            return Err(SafetyError::BoundsExceeded {
                tool: tool.as_str().into(),
                entities: actual_entities,
                max: request_cap,
            }
            .into());
        }

        let validator = SafetyValidator::new(self.schemas, self.grammars);
        let ctx = ValidationContext {
            scope,
            tool,
            entities_modified: actual_entities,
            payload: raw_payload.clone(),
        };
        validator.validate(&ctx)?;
        Ok(PlanResponse {
            tool,
            raw_payload,
            parsed,
            entities_modified: actual_entities,
        })
    }

    /// Dispatch a tool-call request end-to-end through the sidecar.
    ///
    /// This is the only place that talks to the LLM: prompt assembly,
    /// grammar lookup, transport call, and safety finalization all live
    /// here so the bridge layer never sees raw model bytes.
    ///
    /// Flow:
    ///   1. `precheck`  — schema lookup, scope check, bounds check.
    ///   2. lookup the GBNF grammar that the model output must satisfy.
    ///   3. assemble the prompt (system header + tool name + scope +
    ///      user context as JSON).
    ///   4. POST to the sidecar's `/completion` endpoint.
    ///   5. `finalize` — re-run the safety validator on the raw payload
    ///      and parse it into typed JSON.
    pub fn dispatch(
        &self,
        request: &PlanRequest,
        transport: &SidecarTransport,
    ) -> Result<PlanResponse, PlanError> {
        self.precheck(request)?;
        let schema = self
            .schemas
            .get(request.tool)
            .ok_or_else(|| SafetyError::UnknownTool(request.tool.as_str().into()))?;
        let grammar = self
            .grammars
            .get(&schema.grammar_key)
            .ok_or_else(|| PlanError::UnknownGrammar(schema.grammar_key.clone()))?;
        let prompt = build_prompt(schema, request);
        let completion_request = CompletionRequest::new(prompt, grammar.gbnf.clone());
        let completion = transport.complete(&completion_request)?;
        self.finalize(
            request.tool,
            request.scope,
            request.max_entities_modified,
            completion.content,
        )
    }

    /// Like [`dispatch`], but uses `transport.complete_with_retry` so the
    /// sidecar's 503-loading window is handled transparently and the
    /// caller can cancel mid-flight via [`AiCancelToken`].
    ///
    /// Phase 12 Task 17: this is the production entry point — the bridge
    /// holds a cancel token per inflight job so the renderer's
    /// `ai_cancel_job` IPC actually aborts the upstream HTTP call instead
    /// of merely abandoning the JS Promise.
    pub fn dispatch_with_retry(
        &self,
        request: &PlanRequest,
        transport: &SidecarTransport,
        cancel: Option<&crate::transport::AiCancelToken>,
        max_retries: u32,
    ) -> Result<PlanResponse, PlanError> {
        self.precheck(request)?;
        let schema = self
            .schemas
            .get(request.tool)
            .ok_or_else(|| SafetyError::UnknownTool(request.tool.as_str().into()))?;
        let grammar = self
            .grammars
            .get(&schema.grammar_key)
            .ok_or_else(|| PlanError::UnknownGrammar(schema.grammar_key.clone()))?;
        let prompt = build_prompt(schema, request);
        let completion_request = CompletionRequest::new(prompt, grammar.gbnf.clone());
        let completion = transport.complete_with_retry(&completion_request, cancel, max_retries)?;
        self.finalize(
            request.tool,
            request.scope,
            request.max_entities_modified,
            completion.content,
        )
    }
}

/// Assemble the model-facing prompt. The format is deliberately small —
/// the GBNF grammar is doing the heavy lifting; the prompt only has to
/// give the model enough context to fill in tool-relevant fields.
///
/// `pub` for visibility from the bridge integration tests; the prompt
/// shape is part of the wire contract with the sidecar.
pub fn build_prompt(schema: &ToolSchema, request: &PlanRequest) -> String {
    let mut buf = String::with_capacity(256 + request.prompt.len());
    buf.push_str("[SYSTEM]\n");
    buf.push_str("You are AEC Studio's local design assistant. Emit a single JSON object that satisfies the grammar for the requested tool. Do not include explanations.\n\n");
    buf.push_str("[TOOL]\n");
    buf.push_str(schema.name.as_str());
    buf.push('\n');
    buf.push_str("[SCOPE]\n");
    // Use the canonical lowercase wire encoding (`design` / `draft` / `bim` / ...)
    // rather than the derived `Debug` PascalCase. The `[TOOL]` line above is
    // snake_case (`style_assistant`); using `Debug` here would emit `Design`,
    // mixing two casing conventions inside one prompt for no reason.
    buf.push_str(request.scope.as_str());
    buf.push('\n');
    buf.push_str("[MAX_ENTITIES_MODIFIED]\n");
    buf.push_str(&request.max_entities_modified.to_string());
    buf.push('\n');
    buf.push_str("[CONTEXT]\n");
    buf.push_str(&request.context.to_string());
    buf.push('\n');
    if !request.prompt.is_empty() {
        buf.push_str("[USER_PROMPT]\n");
        buf.push_str(&request.prompt);
        buf.push('\n');
    }
    buf.push_str("[RESPONSE]\n");
    buf
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

    #[test]
    fn finalize_enforces_request_cap_against_actual_entity_count() {
        // The model emits 4 furniture entities + 1 lighting preset = 5
        // entities, but the caller's runtime cap is 3. The validator
        // MUST reject with `SafetyError::BoundsExceeded`. The previous
        // code passed `request_cap` itself as the count so this check
        // was vacuous — every payload passed bounds checking.
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let planner = ToolPlanner::new(&s, &g);
        let payload = r#"{"furniture_ids":["a","b","c","d"],"material_ids":[],"lighting_preset_id":"warm_evening"}"#;
        let err = planner
            .finalize(ToolName::StyleAssistant, Scope::Design, 3, payload.into())
            .unwrap_err();
        let PlanError::Safety(SafetyError::BoundsExceeded { entities, max, .. }) = err else {
            panic!("expected BoundsExceeded, got {err:?}");
        };
        assert_eq!(entities, 5, "actual entity count not threaded through");
        assert_eq!(max, 3, "runtime cap not threaded through");
    }

    #[test]
    fn finalize_reports_actual_entity_count_not_request_cap() {
        // 2 furniture + 1 material + 1 lighting = 4 entities, cap = 16.
        // The returned `entities_modified` must be the *actual* count
        // (4), not the cap (16). Bridge service consumers of this
        // field downstream rely on it being honest.
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let planner = ToolPlanner::new(&s, &g);
        let payload = r#"{"furniture_ids":["a","b"],"material_ids":["m1"],"lighting_preset_id":"warm_evening"}"#;
        let r = planner
            .finalize(ToolName::StyleAssistant, Scope::Design, 16, payload.into())
            .unwrap();
        assert_eq!(r.entities_modified, 4);
    }

    #[test]
    fn count_response_entities_per_tool_shapes() {
        // Spot-check the count helper against each shape the
        // diff_engine knows how to build.
        let plan_payload = serde_json::json!({"polylines":[{"points":[]},{"points":[]},{}]});
        assert_eq!(
            count_response_entities(ToolName::PlanDetection, &plan_payload),
            2,
            "polylines without `points` are skipped (mirrors DiffEngine)"
        );
        let style_payload = serde_json::json!({
            "furniture_ids": ["a","b","c"],
            "material_ids": ["m"],
            "lighting_preset_id": "warm",
        });
        assert_eq!(
            count_response_entities(ToolName::StyleAssistant, &style_payload),
            5
        );
        let render_payload = serde_json::json!({"findings":[{},{}]});
        assert_eq!(
            count_response_entities(ToolName::RenderDoctor, &render_payload),
            2
        );
        // Tools the diff_engine doesn't lower into ops fall back to 0.
        assert_eq!(
            count_response_entities(ToolName::Classification, &serde_json::json!({})),
            0
        );
    }

    /// Regression for PR-V round 6 finding: `count_response_entities`
    /// must skip the same elements `DiffEngine::build` skips,
    /// otherwise a within-cap response can trigger
    /// `SafetyError::BoundsExceeded`.
    #[test]
    fn count_response_entities_mirrors_diff_engine_filtering() {
        use crate::diff_engine::DiffEngine;

        // StyleAssistant: non-string array elements (numbers,
        // objects) are skipped by the diff engine because it uses
        // `as_str()`. The counter must skip them too.
        let style_payload = serde_json::json!({
            "furniture_ids": ["chair", 42, {"id":"oops"}, "table"],
            "material_ids": [null, "wood", true],
        });
        let count = count_response_entities(ToolName::StyleAssistant, &style_payload);
        let resp = PlanResponse {
            tool: ToolName::StyleAssistant,
            raw_payload: style_payload.to_string(),
            parsed: style_payload.clone(),
            entities_modified: count,
        };
        let diff = DiffEngine::build(&resp);
        assert_eq!(
            count as usize,
            diff.operations.len(),
            "count must equal diff op count for StyleAssistant (got count={count}, ops={})",
            diff.operations.len()
        );
        assert_eq!(
            count, 3,
            "only 3 strings should count: 'chair','table','wood'"
        );

        // LayoutSuggestion: empty proposals (no target_entity, no
        // asset_id) are dropped by the diff engine; the counter
        // must drop them too. Proposals with an invalid
        // `target_entity` string AND no `asset_id` are also dropped.
        let layout_payload = serde_json::json!({
            "room_anchor": "ent_room_001",
            "proposals": [
                // (1) Valid update: target_entity parses as EntityId.
                {"target_entity":"ent_furniture_001","position_mm":[0,0,0]},
                // (2) Valid insert: asset_id present.
                {"asset_id":"chair_001","position_mm":[1,0,0]},
                // (3) Dropped: empty proposal.
                {},
                // (4) Dropped: invalid target_entity string AND no asset_id.
                {"target_entity":"not-a-valid-entity-id"},
                // (5) Valid insert: asset_id present even though target
                //     is invalid — diff engine falls back to insert path.
                {"target_entity":"also-invalid","asset_id":"chair_002"},
            ],
        });
        let count = count_response_entities(ToolName::LayoutSuggestion, &layout_payload);
        let resp = PlanResponse {
            tool: ToolName::LayoutSuggestion,
            raw_payload: layout_payload.to_string(),
            parsed: layout_payload.clone(),
            entities_modified: count,
        };
        let diff = DiffEngine::build(&resp);
        assert_eq!(
            count as usize,
            diff.operations.len(),
            "count must equal diff op count for LayoutSuggestion (got count={count}, ops={})",
            diff.operations.len()
        );
        assert_eq!(count, 3, "only 3 valid proposals should count, not 5");

        // The before-the-fix behaviour returned 5 (raw array length).
        // Pin the new behaviour so a future refactor that reverts to
        // `arr_len("proposals")` would fail this test.
        let layout_all_empty = serde_json::json!({"proposals":[{},{},{}]});
        assert_eq!(
            count_response_entities(ToolName::LayoutSuggestion, &layout_all_empty),
            0,
            "empty proposals must count as 0 (diff engine drops them)"
        );
    }
}
