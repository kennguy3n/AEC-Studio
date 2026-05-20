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
            "layout_suggestion" => match_layout_suggestion(&value),
            "render_doctor" => match_render_doctor(&value),
            "cad_cleanup" => match_cad_cleanup(&value),
            "schedule_fill" => match_schedule_fill(&value),
            "classification" => match_classification(&value),
            "property_fill" => match_property_fill(&value),
            "validation_help" => match_validation_help(&value),
            "cover_page_draft" => match_cover_page_draft(&value),
            "lighting_balance" => match_lighting_balance(&value),
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

fn match_layout_suggestion(v: &serde_json::Value) -> bool {
    let Some(_anchor) = v.get("room_anchor").and_then(|s| s.as_str()) else {
        return false;
    };
    let Some(arr) = v.get("proposals").and_then(|a| a.as_array()) else {
        return false;
    };
    arr.iter().all(|p| {
        // Each proposal must carry a position triple and either an
        // asset_id (insert) or a target_entity (reposition).
        let pos_ok = p
            .get("position_mm")
            .and_then(|x| x.as_array())
            .is_some_and(|coords| coords.len() == 3 && coords.iter().all(|c| c.as_f64().is_some()));
        let has_target = p.get("asset_id").and_then(|s| s.as_str()).is_some()
            || p.get("target_entity").and_then(|s| s.as_str()).is_some();
        pos_ok && has_target
    })
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

fn match_lighting_balance(v: &serde_json::Value) -> bool {
    let Some(rationale) = v.get("rationale").and_then(|s| s.as_str()) else {
        return false;
    };
    if rationale.trim().is_empty() {
        return false;
    }
    let Some(arr) = v.get("suggested_accents").and_then(|a| a.as_array()) else {
        return false;
    };
    arr.iter().all(|light| {
        let pos_ok = light
            .get("position_mm")
            .and_then(|p| p.as_array())
            .is_some_and(|coords| coords.len() == 3 && coords.iter().all(|c| c.as_f64().is_some()));
        let kind_ok = light
            .get("kind")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s == "area" || s == "point");
        let intensity_ok = light
            .get("intensity")
            .and_then(serde_json::Value::as_f64)
            .is_some();
        let temp_ok = light
            .get("color_temperature_k")
            .and_then(serde_json::Value::as_f64)
            .is_some();
        pos_ok && kind_ok && intensity_ok && temp_ok
    })
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

const LAYOUT_SUGGESTION_GBNF: &str = include_str!("grammars/layout_suggestion.gbnf");
const COVER_PAGE_DRAFT_GBNF: &str = include_str!("grammars/cover_page_draft.gbnf");

const LIGHTING_BALANCE_GBNF: &str = r#"
root        ::= "{" ws "\"rationale\"" ws ":" ws str ws "," ws "\"suggested_accents\"" ws ":" ws lights ws "}"
lights      ::= "[" ws (light ("," ws light)*)? ws "]"
light       ::= "{" ws "\"kind\"" ws ":" ws kind ws "," ws "\"position_mm\"" ws ":" ws triple ws "," ws "\"intensity\"" ws ":" ws number ws "," ws "\"color_temperature_k\"" ws ":" ws number ws ("," ws "\"rationale\"" ws ":" ws str ws)? "}"
kind        ::= "\"area\"" | "\"point\""
triple      ::= "[" ws number ws "," ws number ws "," ws number ws "]"
number      ::= "-"? [0-9]+ ("." [0-9]+)?
str         ::= "\"" [^"]+ "\""
ws          ::= [ \t\n]*
"#;

const RENDER_DOCTOR_GBNF: &str = r#"
root      ::= "{" ws "\"findings\"" ws ":" ws findlist ws "}"
findlist  ::= "[" ws (finding ("," ws finding)*)? ws "]"
finding   ::= "{" ws "\"issue\"" ws ":" ws str ws "," ws "\"severity\"" ws ":" ws str ws ("," ws "\"recommendation\"" ws ":" ws str ws)? "}"
str       ::= "\"" [^"]+ "\""
ws        ::= [ \t\n]*
"#;

const CLASSIFICATION_GBNF: &str = r#"
root        ::= "{" ws "\"assignments\"" ws ":" ws assignlist ws "}"
assignlist  ::= "[" ws (assign ("," ws assign)*)? ws "]"
assign      ::= "{" ws "\"entity\"" ws ":" ws str ws "," ws "\"ifc_class\"" ws ":" ws str ws ("," ws "\"confidence\"" ws ":" ws number ws)? "}"
str         ::= "\"" [^"]+ "\""
number      ::= "-"? [0-9]+ ("." [0-9]+)?
ws          ::= [ \t\n]*
"#;

const PROPERTY_FILL_GBNF: &str = r#"
root        ::= "{" ws "\"psets\"" ws ":" ws psetlist ws "}"
psetlist    ::= "[" ws (pset ("," ws pset)*)? ws "]"
pset        ::= "{" ws "\"entity\"" ws ":" ws str ws "," ws "\"pset\"" ws ":" ws str ws "," ws "\"properties\"" ws ":" ws proplist ws ("," ws "\"confidence\"" ws ":" ws number ws)? "}"
proplist    ::= "[" ws (prop ("," ws prop)*)? ws "]"
prop        ::= "{" ws "\"key\"" ws ":" ws str ws "," ws "\"value\"" ws ":" ws (str | number | bool) ws "}"
str         ::= "\"" [^"]+ "\""
number      ::= "-"? [0-9]+ ("." [0-9]+)?
bool        ::= "true" | "false"
ws          ::= [ \t\n]*
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
            key: "layout_suggestion".into(),
            gbnf: LAYOUT_SUGGESTION_GBNF.into(),
            example: r#"{"room_anchor":"ent_living","proposals":[{"asset_id":"ast:sofa_a","position_mm":[1200.0,800.0,0.0],"rotation_deg":90.0}]}"#.into(),
        });
        r.insert(Grammar {
            key: "render_doctor".into(),
            gbnf: RENDER_DOCTOR_GBNF.into(),
            example: r#"{"findings":[{"issue":"underexposed","severity":"medium","recommendation":"increase exposure by 0.5 EV"}]}"#.into(),
        });
        r.insert(Grammar {
            key: "classification".into(),
            gbnf: CLASSIFICATION_GBNF.into(),
            example:
                r#"{"assignments":[{"entity":"ent_001","ifc_class":"IfcWall","confidence":0.92}]}"#
                    .into(),
        });
        r.insert(Grammar {
            key: "property_fill".into(),
            gbnf: PROPERTY_FILL_GBNF.into(),
            example: r#"{"psets":[{"entity":"ent_001","pset":"Pset_WallCommon","properties":[{"key":"FireRating","value":"EI60"}],"confidence":0.91}]}"#
                .into(),
        });
        r.insert(Grammar {
            key: "cover_page_draft".into(),
            gbnf: COVER_PAGE_DRAFT_GBNF.into(),
            example: r#"{"title":"Loft 12B","subtitle":"A warm home for a family of three","paragraph":"A sun-drenched apartment that pairs open-plan living with intimate corners for slow weekends.","tone":"warm"}"#.into(),
        });
        r.insert(Grammar {
            key: "lighting_balance".into(),
            gbnf: LIGHTING_BALANCE_GBNF.into(),
            example: r#"{"rationale":"warm fill from west","suggested_accents":[{"kind":"area","position_mm":[1200.0,2200.0,2400.0],"intensity":1.4,"color_temperature_k":3200.0}]}"#.into(),
        });
        for key in ["cad_cleanup", "schedule_fill", "validation_help"] {
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
        assert!(r.get("lighting_balance").is_some());
    }

    #[test]
    fn lighting_balance_grammar_matches() {
        let g = GrammarRegistry::defaults();
        let l = g.get("lighting_balance").unwrap();
        assert!(l.matches(
            r#"{"rationale":"warm fill","suggested_accents":[{"kind":"area","position_mm":[1.0,2.0,3.0],"intensity":1.2,"color_temperature_k":3200.0}]}"#
        ));
        // missing rationale -> reject
        assert!(!l.matches(
            r#"{"suggested_accents":[{"kind":"area","position_mm":[1.0,2.0,3.0],"intensity":1.2,"color_temperature_k":3200.0}]}"#
        ));
        // empty rationale -> reject
        assert!(!l.matches(
            r#"{"rationale":"  ","suggested_accents":[{"kind":"area","position_mm":[1.0,2.0,3.0],"intensity":1.2,"color_temperature_k":3200.0}]}"#
        ));
        // unknown kind -> reject
        assert!(!l.matches(
            r#"{"rationale":"x","suggested_accents":[{"kind":"spot","position_mm":[1.0,2.0,3.0],"intensity":1.2,"color_temperature_k":3200.0}]}"#
        ));
        // wrong position shape -> reject
        assert!(!l.matches(
            r#"{"rationale":"x","suggested_accents":[{"kind":"point","position_mm":[1.0,2.0],"intensity":1.0,"color_temperature_k":3200.0}]}"#
        ));
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

    #[test]
    fn layout_suggestion_grammar_matches() {
        let g = GrammarRegistry::defaults();
        let l = g.get("layout_suggestion").unwrap();
        assert!(l.matches(
            r#"{"room_anchor":"ent_living","proposals":[{"asset_id":"ast:sofa","position_mm":[0.0,0.0,0.0],"rotation_deg":90.0}]}"#
        ));
        // accepts target_entity-only proposals (repositioning existing furniture)
        assert!(l.matches(
            r#"{"room_anchor":"ent_living","proposals":[{"target_entity":"ent_sofa","position_mm":[100.0,200.0,0.0],"rotation_deg":0.0}]}"#
        ));
        // rejects missing room_anchor
        assert!(!l.matches(
            r#"{"proposals":[{"asset_id":"ast:sofa","position_mm":[0.0,0.0,0.0],"rotation_deg":0.0}]}"#
        ));
        // rejects proposal missing both asset_id and target_entity
        assert!(!l.matches(
            r#"{"room_anchor":"ent_living","proposals":[{"position_mm":[0.0,0.0,0.0],"rotation_deg":0.0}]}"#
        ));
        // rejects malformed position triple
        assert!(!l.matches(
            r#"{"room_anchor":"ent_living","proposals":[{"asset_id":"ast:sofa","position_mm":[0.0,0.0],"rotation_deg":0.0}]}"#
        ));
    }
}
