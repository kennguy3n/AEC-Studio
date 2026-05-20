//! Tool-call schemas for every AI capability AEC Studio ships with.
//!
//! Each tool declares:
//!   - the [`Scope`] it's allowed to be invoked from,
//!   - the upper bound on entities it may modify (used by
//!     [`safety_validator`](crate::safety_validator)),
//!   - the GBNF grammar key its responses must satisfy.
//!
//! **Source of truth.** The default registry is parsed at compile time from
//! [`crates/aec_ai/data/ai_tools.json`](../data/ai_tools.json) so that the
//! TypeScript bridge (`apps/desktop/electron/bridge.ts`) and the Rust safety
//! validator are guaranteed to agree on `max_entities_modified` and
//! `allowed_scopes`. The cross-language sync is enforced by:
//!   - this file's [`defaults_match_canonical_json`] unit test, and
//!   - the Vitest test at
//!     `apps/desktop/renderer/src/__tests__/ai-tools-sync.test.ts`
//!     which loads the same JSON and asserts the TS array matches.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::Scope;

/// Canonical JSON document, inlined at compile time. Bumping this path is
/// the only way to add or change a tool's safety envelope — both the Rust
/// validator and the TS bridge consume the same bytes.
pub const CANONICAL_TOOLS_JSON: &str = include_str!("../data/ai_tools.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    PlanDetection,
    PlanToWall,
    StyleAssistant,
    LayoutSuggestion,
    RenderDoctor,
    CadCleanup,
    ScheduleFill,
    Classification,
    PropertyFill,
    ValidationHelp,
    CoverPageDraft,
    LightingBalance,
}

impl ToolName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlanDetection => "plan_detection",
            Self::PlanToWall => "plan_to_wall",
            Self::StyleAssistant => "style_assistant",
            Self::LayoutSuggestion => "layout_suggestion",
            Self::RenderDoctor => "render_doctor",
            Self::CadCleanup => "cad_cleanup",
            Self::ScheduleFill => "schedule_fill",
            Self::Classification => "classification",
            Self::PropertyFill => "property_fill",
            Self::ValidationHelp => "validation_help",
            Self::CoverPageDraft => "cover_page_draft",
            Self::LightingBalance => "lighting_balance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: ToolName,
    pub display_name: String,
    pub allowed_scopes: Vec<Scope>,
    pub max_entities_modified: u32,
    /// Key into [`crate::grammars::GrammarRegistry`].
    pub grammar_key: String,
    /// Human-readable description shown in the UI tool picker.
    #[serde(default)]
    pub description: String,
    /// Allowed tool *targets* (other tool names this tool may call into).
    /// Empty = leaf tool.
    #[serde(default)]
    pub child_tools: Vec<ToolName>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolSchemaRegistry {
    schemas: HashMap<ToolName, ToolSchema>,
}

/// Top-level shape of `ai_tools.json`. The `version` field is a guard against
/// silent schema drift — bump it whenever the entry shape changes.
#[derive(Debug, Deserialize)]
struct CanonicalToolsFile {
    #[serde(rename = "version")]
    _version: u32,
    tools: Vec<CanonicalTool>,
}

#[derive(Debug, Deserialize)]
struct CanonicalTool {
    id: ToolName,
    display_name: String,
    description: String,
    allowed_scopes: Vec<Scope>,
    max_entities_modified: u32,
    grammar_key: String,
}

impl ToolSchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, schema: ToolSchema) {
        self.schemas.insert(schema.name, schema);
    }

    pub fn get(&self, name: ToolName) -> Option<&ToolSchema> {
        self.schemas.get(&name)
    }

    pub fn names(&self) -> impl Iterator<Item = ToolName> + '_ {
        self.schemas.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.schemas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }

    /// Iterate schemas in a stable order (sorted by tool name) — useful
    /// when emitting catalogues that must be deterministic across calls.
    pub fn iter_sorted(&self) -> impl Iterator<Item = &ToolSchema> {
        let mut entries: Vec<&ToolSchema> = self.schemas.values().collect();
        entries.sort_by_key(|s| s.name.as_str());
        entries.into_iter()
    }

    /// The default registry shipped with AEC Studio. Parses
    /// `crates/aec_ai/data/ai_tools.json` at compile time so the Rust
    /// safety validator and the TS bridge stay in lockstep.
    ///
    /// # Panics
    ///
    /// Panics if the bundled JSON is malformed. The bundled file is
    /// exercised by [`defaults_match_canonical_json`], so a malformed
    /// commit will fail `cargo test` long before any runtime path.
    pub fn defaults() -> Self {
        let file: CanonicalToolsFile = serde_json::from_str(CANONICAL_TOOLS_JSON)
            .expect("bundled ai_tools.json must be valid JSON matching CanonicalToolsFile");
        let mut r = Self::new();
        for t in file.tools {
            r.insert(ToolSchema {
                name: t.id,
                display_name: t.display_name,
                allowed_scopes: t.allowed_scopes,
                max_entities_modified: t.max_entities_modified,
                grammar_key: t.grammar_key,
                description: t.description,
                child_tools: Vec::new(),
            });
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_contain_all_tools() {
        let r = ToolSchemaRegistry::defaults();
        assert_eq!(r.len(), 12);
        assert!(r.get(ToolName::PlanDetection).is_some());
        assert!(r.get(ToolName::CoverPageDraft).is_some());
        assert!(r.get(ToolName::LightingBalance).is_some());
    }

    #[test]
    fn schemas_pin_scopes() {
        let r = ToolSchemaRegistry::defaults();
        assert_eq!(
            r.get(ToolName::StyleAssistant).unwrap().allowed_scopes,
            vec![Scope::Design]
        );
        assert_eq!(
            r.get(ToolName::RenderDoctor).unwrap().allowed_scopes,
            vec![Scope::Render]
        );
    }

    /// Pin every safety envelope so the bundled JSON cannot silently
    /// loosen a tool's bounds without an accompanying test update. This
    /// is the Rust half of the cross-language sync — the TS half lives
    /// in `apps/desktop/renderer/src/__tests__/ai-tools-sync.test.ts`.
    #[test]
    fn defaults_match_canonical_json() {
        let r = ToolSchemaRegistry::defaults();
        let cases: &[(ToolName, &[Scope], u32, &str)] = &[
            (
                ToolName::PlanDetection,
                &[Scope::Design, Scope::Draft],
                64,
                "plan_detection",
            ),
            (
                ToolName::PlanToWall,
                &[Scope::Design, Scope::Draft],
                64,
                "plan_detection",
            ),
            (
                ToolName::StyleAssistant,
                &[Scope::Design],
                24,
                "style_assistant",
            ),
            (
                ToolName::LayoutSuggestion,
                &[Scope::Design],
                16,
                "layout_suggestion",
            ),
            (ToolName::RenderDoctor, &[Scope::Render], 8, "render_doctor"),
            (ToolName::CadCleanup, &[Scope::Draft], 256, "cad_cleanup"),
            (
                ToolName::ScheduleFill,
                &[Scope::Bim, Scope::Deliver],
                128,
                "schedule_fill",
            ),
            (
                ToolName::Classification,
                &[Scope::Bim],
                128,
                "classification",
            ),
            (ToolName::PropertyFill, &[Scope::Bim], 128, "property_fill"),
            (
                ToolName::ValidationHelp,
                &[Scope::Bim],
                64,
                "validation_help",
            ),
            (
                ToolName::CoverPageDraft,
                &[Scope::Deliver],
                4,
                "cover_page_draft",
            ),
            (
                ToolName::LightingBalance,
                &[Scope::Render],
                8,
                "lighting_balance",
            ),
        ];
        for (name, scopes, max, grammar) in cases {
            let s = r.get(*name).unwrap_or_else(|| {
                panic!(
                    "canonical JSON missing tool `{}` — regenerate ai_tools.json",
                    name.as_str()
                )
            });
            assert_eq!(
                s.allowed_scopes.as_slice(),
                *scopes,
                "{} scopes drifted",
                name.as_str()
            );
            assert_eq!(
                s.max_entities_modified,
                *max,
                "{} max_entities_modified drifted",
                name.as_str()
            );
            assert_eq!(
                s.grammar_key,
                *grammar,
                "{} grammar_key drifted",
                name.as_str()
            );
        }
    }
}
