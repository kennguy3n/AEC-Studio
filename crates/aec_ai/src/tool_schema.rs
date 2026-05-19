//! Tool-call schemas for every AI capability AEC Studio ships with.
//!
//! Each tool declares:
//!   - the [`Scope`] it's allowed to be invoked from,
//!   - the upper bound on entities it may modify (used by
//!     [`safety_validator`](crate::safety_validator)),
//!   - the GBNF grammar key its responses must satisfy.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::Scope;

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
    /// Allowed tool *targets* (other tool names this tool may call into).
    /// Empty = leaf tool.
    pub child_tools: Vec<ToolName>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolSchemaRegistry {
    schemas: HashMap<ToolName, ToolSchema>,
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

    /// The default registry shipped with AEC Studio, matching the table in
    /// `ARCHITECTURE.md`.
    pub fn defaults() -> Self {
        let mut r = Self::new();
        let entries = [
            (
                ToolName::PlanDetection,
                "Plan detection",
                vec![Scope::Design, Scope::Draft],
                64,
                "plan_detection",
            ),
            (
                ToolName::PlanToWall,
                "Plan → walls",
                vec![Scope::Design, Scope::Draft],
                64,
                "plan_detection",
            ),
            (
                ToolName::StyleAssistant,
                "Style assistant",
                vec![Scope::Design],
                24,
                "style_assistant",
            ),
            (
                ToolName::LayoutSuggestion,
                "Layout suggestion",
                vec![Scope::Design],
                16,
                "style_assistant",
            ),
            (
                ToolName::RenderDoctor,
                "Render doctor",
                vec![Scope::Render],
                8,
                "render_doctor",
            ),
            (
                ToolName::CadCleanup,
                "CAD cleanup",
                vec![Scope::Draft],
                256,
                "cad_cleanup",
            ),
            (
                ToolName::ScheduleFill,
                "Schedule fill",
                vec![Scope::Bim, Scope::Deliver],
                128,
                "schedule_fill",
            ),
            (
                ToolName::Classification,
                "BIM classification",
                vec![Scope::Bim],
                128,
                "classification",
            ),
            (
                ToolName::PropertyFill,
                "BIM property fill",
                vec![Scope::Bim],
                128,
                "property_fill",
            ),
            (
                ToolName::ValidationHelp,
                "Validation helper",
                vec![Scope::Bim],
                64,
                "validation_help",
            ),
            (
                ToolName::CoverPageDraft,
                "Cover page draft",
                vec![Scope::Deliver],
                4,
                "cover_page_draft",
            ),
        ];
        for (name, display, scopes, max, grammar) in entries {
            r.insert(ToolSchema {
                name,
                display_name: display.into(),
                allowed_scopes: scopes,
                max_entities_modified: max,
                grammar_key: grammar.into(),
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
    fn defaults_contain_eleven_tools() {
        let r = ToolSchemaRegistry::defaults();
        assert_eq!(r.len(), 11);
        assert!(r.get(ToolName::PlanDetection).is_some());
        assert!(r.get(ToolName::CoverPageDraft).is_some());
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
}
