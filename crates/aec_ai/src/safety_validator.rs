//! Safety validator. Every AI response goes through this gate before it
//! reaches the diff engine.
//!
//! Enforces:
//!   1. Scope: the tool is allowed in the current workflow mode.
//!   2. Allowlist: only tools registered in the schema registry can run.
//!   3. Bounded change: # of entities to modify ≤ schema's
//!      `max_entities_modified`.
//!   4. Grammar shape: payload matches the tool's GBNF (via the rust-side
//!      validator).
//!   5. No exfiltration: the proposed diff doesn't reference network URLs
//!      or absolute filesystem paths outside the project package.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_core::types::Scope;

use crate::grammars::GrammarRegistry;
use crate::tool_schema::{ToolName, ToolSchemaRegistry};

#[derive(Debug, Error)]
pub enum SafetyError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("tool `{tool}` not allowed in scope `{scope:?}`")]
    ScopeViolation { tool: String, scope: Scope },
    #[error("payload would modify {entities} entities (max for `{tool}` is {max})")]
    BoundsExceeded {
        tool: String,
        entities: u32,
        max: u32,
    },
    #[error("payload failed grammar validation for `{0}`")]
    GrammarMismatch(String),
    #[error("payload referenced disallowed resource: `{0}`")]
    Exfiltration(String),
    #[error("malformed payload: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafetyViolation {
    UnknownTool,
    ScopeViolation,
    BoundsExceeded,
    GrammarMismatch,
    Exfiltration,
    Malformed,
}

pub struct SafetyValidator<'a> {
    schemas: &'a ToolSchemaRegistry,
    grammars: &'a GrammarRegistry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationContext {
    pub scope: Scope,
    pub tool: ToolName,
    pub entities_modified: u32,
    pub payload: String,
}

impl<'a> SafetyValidator<'a> {
    pub fn new(schemas: &'a ToolSchemaRegistry, grammars: &'a GrammarRegistry) -> Self {
        Self { schemas, grammars }
    }

    pub fn validate(&self, ctx: &ValidationContext) -> Result<(), SafetyError> {
        let schema = self
            .schemas
            .get(ctx.tool)
            .ok_or_else(|| SafetyError::UnknownTool(ctx.tool.as_str().into()))?;
        if !schema.allowed_scopes.contains(&ctx.scope) {
            return Err(SafetyError::ScopeViolation {
                tool: ctx.tool.as_str().into(),
                scope: ctx.scope,
            });
        }
        if ctx.entities_modified > schema.max_entities_modified {
            return Err(SafetyError::BoundsExceeded {
                tool: ctx.tool.as_str().into(),
                entities: ctx.entities_modified,
                max: schema.max_entities_modified,
            });
        }
        let grammar = self
            .grammars
            .get(&schema.grammar_key)
            .ok_or_else(|| SafetyError::GrammarMismatch(schema.grammar_key.clone()))?;
        if !grammar.matches(&ctx.payload) {
            return Err(SafetyError::GrammarMismatch(schema.grammar_key.clone()));
        }
        check_exfiltration(&ctx.payload)?;
        Ok(())
    }
}

fn check_exfiltration(payload: &str) -> Result<(), SafetyError> {
    // Reject any absolute paths or network URLs that escape the project
    // sandbox. These patterns deliberately catch the common attempts to
    // exfiltrate local data via an AI-suggested asset URL or to overwrite
    // files outside the project package.
    const BAD_PATTERNS: &[&str] = &[
        "http://", "https://", "ftp://", "file:///", "/etc/", "/var/", "/Users/", "/home/", "C:\\",
        "C:/",
    ];
    let lower = payload.to_ascii_lowercase();
    for pat in BAD_PATTERNS {
        if lower.contains(&pat.to_ascii_lowercase()) {
            return Err(SafetyError::Exfiltration((*pat).into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tool: ToolName, scope: Scope, modified: u32, payload: &str) -> ValidationContext {
        ValidationContext {
            scope,
            tool,
            entities_modified: modified,
            payload: payload.into(),
        }
    }

    #[test]
    fn accepts_valid_style_assistant_payload() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        assert!(v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Design, 4, payload))
            .is_ok());
    }

    #[test]
    fn rejects_wrong_scope() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Render, 1, payload))
            .unwrap_err();
        assert!(matches!(err, SafetyError::ScopeViolation { .. }));
    }

    #[test]
    fn rejects_overlarge_change() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Design, 9999, payload))
            .unwrap_err();
        assert!(matches!(err, SafetyError::BoundsExceeded { .. }));
    }

    #[test]
    fn rejects_grammar_mismatch() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let err = v
            .validate(&ctx(ToolName::PlanDetection, Scope::Design, 4, "{}"))
            .unwrap_err();
        assert!(matches!(err, SafetyError::GrammarMismatch(_)));
    }

    #[test]
    fn rejects_exfiltration_url() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload = r#"{"furniture_ids":["http://evil.example/x"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Design, 1, payload))
            .unwrap_err();
        assert!(matches!(err, SafetyError::Exfiltration(_)));
    }
}
