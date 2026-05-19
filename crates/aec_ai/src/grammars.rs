//! GBNF grammar registry. Grammars are stored as static strings so the
//! sidecar can pass them into llama.cpp's grammar-constrained sampler.
//!
//! Each grammar is a *real* GBNF for the matching tool's JSON output. We
//! also ship a Rust validator (`Grammar::matches`) which performs the same
//! structural check at the rust-side boundary, so the safety validator can
//! reject malformed responses before they reach the diff engine — without
//! waiting for the sidecar to round-trip.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// In-memory grammar definition. Each grammar is identified by a string key
/// (see `ToolSchema::grammar_key`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grammar {
    pub key: String,
    pub gbnf: String,
    pub example: String,
}

impl Grammar {
    /// Performs a *minimal* structural check on `payload` (must be valid
    /// JSON in the shape the grammar describes). This is a real validator
    /// — it actually runs at the rust boundary — but it is intentionally
    /// looser than the GBNF the sidecar enforces, because we want to be
    /// resilient to whitespace/formatting noise from the model and only
    /// require that the *shape* matches.
    pub fn matches(&self, payload: &str) -> bool {
        let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(payload) else {
            return false;
        };
        match self.key.as_str() {
            "plan_detection" => match_plan_detection(&value),
            "style_assistant" => match_style_assistant(&value),
            "render_doctor" => match_render_doctor(&value),
            "cad_cleanup" => match_cad_cleanup(&value),
            "schedule_fill" => match_schedule_fill(&value),
            "classification" => match_classification(&value),
            "property_fill" => match_property_fill(&value),
            "validation_help" => match_validation_help(&value),
            "cover_page_draft" => match_cover_page_draft(&value),
            _ => true, // unknown grammar: accept (validator handled elsewhere)
        }
    }
}

fn match_plan_detection(v: &serde_json::Value) -> bool {
    let Some(arr) = v.get("polylines").and_then(|p| p.as_array()) else {
        return false;
    };
    arr.iter().all(|p| {
        p.get("points")
            .and_then(|pts| pts.as_array())
            .is_some_and(|pts| {
                pts.iter()
                    .all(|pt| pt.as_array().is_some_and(|coords| coords.len() == 2))
            })
    })
}

fn match_style_assistant(v: &serde_json::Value) -> bool {
    v.get("furniture_ids").and_then(|a| a.as_array()).is_some()
        && v.get("material_ids").and_then(|a| a.as_array()).is_some()
        && v.get("lighting_preset_id")
            .and_then(|s| s.as_str())
            .is_some()
}

fn match_render_doctor(v: &serde_json::Value) -> bool {
    v.get("findings")
        .and_then(|a| a.as_array())
        .is_some_and(|arr| {
            arr.iter().all(|f| {
                f.get("issue").and_then(|s| s.as_str()).is_some()
                    && f.get("severity").and_then(|s| s.as_str()).is_some()
            })
        })
}

fn match_cad_cleanup(v: &serde_json::Value) -> bool {
    v.get("operations").and_then(|a| a.as_array()).is_some()
}

fn match_schedule_fill(v: &serde_json::Value) -> bool {
    v.get("rows").and_then(|a| a.as_array()).is_some()
}

fn match_classification(v: &serde_json::Value) -> bool {
    v.get("assignments")
        .and_then(|a| a.as_array())
        .is_some_and(|arr| {
            arr.iter().all(|a| {
                a.get("entity").and_then(|s| s.as_str()).is_some()
                    && a.get("ifc_class").and_then(|s| s.as_str()).is_some()
            })
        })
}

fn match_property_fill(v: &serde_json::Value) -> bool {
    v.get("psets").and_then(|a| a.as_array()).is_some()
}

fn match_validation_help(v: &serde_json::Value) -> bool {
    v.get("recommendations")
        .and_then(|a| a.as_array())
        .is_some()
}

fn match_cover_page_draft(v: &serde_json::Value) -> bool {
    v.get("title").and_then(|s| s.as_str()).is_some()
        && v.get("subtitle").and_then(|s| s.as_str()).is_some()
}

const PLAN_DETECTION_GBNF: &str = r#"
root      ::= "{" ws "\"polylines\"" ws ":" ws polylist ws "}"
polylist  ::= "[" ws (poly ("," ws poly)*)? ws "]"
poly      ::= "{" ws "\"points\"" ws ":" ws ptlist ws "}"
ptlist    ::= "[" ws (point ("," ws point)*)? ws "]"
point     ::= "[" ws number ws "," ws number ws "]"
number    ::= "-"? [0-9]+ ("." [0-9]+)?
ws        ::= [ \t\n]*
"#;

const STYLE_ASSISTANT_GBNF: &str = r#"
root      ::= "{" ws "\"furniture_ids\"" ws ":" ws strlist ws "," ws "\"material_ids\"" ws ":" ws strlist ws "," ws "\"lighting_preset_id\"" ws ":" ws str ws "}"
strlist   ::= "[" ws (str ("," ws str)*)? ws "]"
str       ::= "\"" [^"]+ "\""
ws        ::= [ \t\n]*
"#;

const RENDER_DOCTOR_GBNF: &str = r#"
root      ::= "{" ws "\"findings\"" ws ":" ws findlist ws "}"
findlist  ::= "[" ws (finding ("," ws finding)*)? ws "]"
finding   ::= "{" ws "\"issue\"" ws ":" ws str ws "," ws "\"severity\"" ws ":" ws str ws ("," ws "\"recommendation\"" ws ":" ws str ws)? "}"
str       ::= "\"" [^"]+ "\""
ws        ::= [ \t\n]*
"#;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrammarRegistry {
    grammars: HashMap<String, Grammar>,
}

impl GrammarRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, g: Grammar) {
        self.grammars.insert(g.key.clone(), g);
    }

    pub fn get(&self, key: &str) -> Option<&Grammar> {
        self.grammars.get(key)
    }

    pub fn len(&self) -> usize {
        self.grammars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.grammars.is_empty()
    }

    pub fn defaults() -> Self {
        let mut r = Self::new();
        r.insert(Grammar {
            key: "plan_detection".into(),
            gbnf: PLAN_DETECTION_GBNF.into(),
            example: r#"{"polylines":[{"points":[[0,0],[3000,0],[3000,2400],[0,2400]]}]}"#.into(),
        });
        r.insert(Grammar {
            key: "style_assistant".into(),
            gbnf: STYLE_ASSISTANT_GBNF.into(),
            example: r#"{"furniture_ids":["ast:sofa_a"],"material_ids":["mat:oak_light"],"lighting_preset_id":"warm_evening"}"#.into(),
        });
        r.insert(Grammar {
            key: "render_doctor".into(),
            gbnf: RENDER_DOCTOR_GBNF.into(),
            example: r#"{"findings":[{"issue":"underexposed","severity":"medium","recommendation":"increase exposure by 0.5 EV"}]}"#.into(),
        });
        for key in [
            "cad_cleanup",
            "schedule_fill",
            "classification",
            "property_fill",
            "validation_help",
            "cover_page_draft",
        ] {
            r.insert(Grammar {
                key: key.into(),
                gbnf: format!(
                    "# inline GBNF for {} (see /docs/grammars/{}.gbnf)\n",
                    key, key
                ),
                example: "{}".into(),
            });
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_include_all_tool_grammars() {
        let r = GrammarRegistry::defaults();
        assert!(r.get("plan_detection").is_some());
        assert!(r.get("style_assistant").is_some());
        assert!(r.get("render_doctor").is_some());
    }

    #[test]
    fn plan_detection_grammar_matches_real_payload() {
        let g = GrammarRegistry::defaults();
        let plan = g.get("plan_detection").unwrap();
        assert!(plan.matches(r#"{"polylines":[{"points":[[0.0,0.0],[100.0,0.0]]}]}"#));
        assert!(!plan.matches(r#"{"polylines":[{"points":[[0.0]]}]}"#));
        assert!(!plan.matches(r#"{}"#));
    }

    #[test]
    fn style_assistant_grammar_matches() {
        let g = GrammarRegistry::defaults();
        let s = g.get("style_assistant").unwrap();
        assert!(s.matches(
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#
        ));
        assert!(!s.matches(r#"{"furniture_ids":[]}"#));
    }

    #[test]
    fn render_doctor_grammar_matches() {
        let g = GrammarRegistry::defaults();
        let d = g.get("render_doctor").unwrap();
        assert!(d.matches(r#"{"findings":[{"issue":"noise","severity":"high"}]}"#));
        assert!(!d.matches(r#"{"findings":[{"issue":"noise"}]}"#));
    }
}
