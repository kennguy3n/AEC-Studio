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

/// Reject any absolute paths or network URLs that escape the project
/// sandbox. We parse the payload as JSON and recursively inspect every
/// **string value** — keys, structural punctuation, and numeric/bool
/// literals are intentionally not checked, so a field name like
/// `home_path_hint` cannot trigger a false positive.
///
/// If the payload is not valid JSON (which the grammar check should have
/// already caught upstream) we fall back to a raw-substring scan so we
/// never silently accept an exfiltration attempt in a malformed envelope.
fn check_exfiltration(payload: &str) -> Result<(), SafetyError> {
    const BAD_PATTERNS: &[&str] = &[
        "http://", "https://", "ftp://", "file:///", "/etc/", "/var/", "/Users/", "/home/", "C:\\",
        "C:/",
    ];

    fn scan(value: &serde_json::Value, patterns: &[&str]) -> Result<(), SafetyError> {
        match value {
            serde_json::Value::String(s) => {
                let lower = s.to_ascii_lowercase();
                for pat in patterns {
                    if lower.contains(&pat.to_ascii_lowercase()) {
                        return Err(SafetyError::Exfiltration((*pat).into()));
                    }
                }
                Ok(())
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    scan(item, patterns)?;
                }
                Ok(())
            }
            serde_json::Value::Object(map) => {
                for v in map.values() {
                    scan(v, patterns)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        return scan(&v, BAD_PATTERNS);
    }
    // Non-JSON payload — apply the legacy raw scan so we err on the side
    // of caution rather than letting an exfiltration attempt slip through.
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

    #[test]
    fn check_exfiltration_ignores_keys_and_inspects_values() {
        // A key called `home_path_hint` MUST NOT trigger; only string values
        // are scanned. The values themselves are benign here.
        let benign = r#"{"home_path_hint":"living_room","tags":["sofa","home_lamp"]}"#;
        assert!(check_exfiltration(benign).is_ok());

        // Same shape, but a value points at the user's home directory —
        // that must still be rejected.
        let exfil = r#"{"home_path_hint":"living_room","target":"/home/user/.ssh/id_rsa"}"#;
        let err = check_exfiltration(exfil).unwrap_err();
        assert!(matches!(err, SafetyError::Exfiltration(_)));
    }

    #[test]
    fn check_exfiltration_falls_back_to_raw_scan_for_invalid_json() {
        // Malformed payload — fallback raw scan must still catch the URL.
        let bad = "not really json http://evil.example/secret";
        let err = check_exfiltration(bad).unwrap_err();
        assert!(matches!(err, SafetyError::Exfiltration(_)));
    }
}
