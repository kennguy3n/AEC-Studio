//! Local-AI property fill.
//!
//! Reads a project-standards file ("if this is an exterior wall in
//! Germany then FireRating is REI 60, ThermalTransmittance is 0.24,
//! Material is `Brick`") plus an element's class + already-known
//! properties, and proposes values for the *missing* keys.
//!
//! The standards file is a JSON document, structured as a list of
//! rules. Each rule has:
//!   * a `class` selector (an IfcClass tag),
//!   * an optional `match` predicate over already-set properties
//!     (subset-of-pset → matches), and
//!   * `defaults`: pset → key → value pairs to propose.
//!
//! Confidence is computed deterministically from the *specificity* of
//! the matching rule (more `match` keys → higher confidence). A
//! generic rule (`class` only) yields 0.80; each `match` predicate
//! adds 0.05, capped at 0.97.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use aec_bim::classification::IfcClass;
use aec_bim::properties::{standard_pset_keys, standard_pset_name_for_class, PropertyValue};
use aec_core::types::{DiffId, EntityId};

use crate::diff_engine::{Diff, DiffOperation, DiffStatus};
use crate::tool_schema::ToolName;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementContext {
    /// Stable id for the element. Required to be a valid `EntityId`
    /// when producing a Diff.
    pub entity: String,
    pub class: IfcClass,
    /// Already-known properties. Map `pset → key → value`.
    pub known: BTreeMap<String, BTreeMap<String, PropertyValue>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandardRule {
    pub class: IfcClass,
    /// All key/value pairs in `r#match` must be present in the
    /// element's `known` properties (for the rule's pset) for the
    /// rule to fire.
    #[serde(default)]
    pub r#match: BTreeMap<String, PropertyValue>,
    /// `pset → key → value`.
    pub defaults: BTreeMap<String, BTreeMap<String, PropertyValue>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectStandards {
    pub rules: Vec<StandardRule>,
}

impl ProjectStandards {
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn matching<'a>(&'a self, ctx: &'a ElementContext) -> Vec<&'a StandardRule> {
        self.rules
            .iter()
            .filter(|r| r.class == ctx.class)
            .filter(|r| rule_matches(r, ctx))
            .collect()
    }
}

fn rule_matches(rule: &StandardRule, ctx: &ElementContext) -> bool {
    if rule.r#match.is_empty() {
        return true;
    }
    for (key, expected) in &rule.r#match {
        let mut hit = false;
        for pset_values in ctx.known.values() {
            if pset_values.get(key) == Some(expected) {
                hit = true;
                break;
            }
        }
        if !hit {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyProposal {
    pub entity: String,
    pub pset: String,
    pub key: String,
    pub value: PropertyValue,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyFillResult {
    pub proposals: Vec<PropertyProposal>,
}

impl PropertyFillResult {
    pub fn to_diff(&self) -> Diff {
        // Group proposals by (entity, pset).
        let mut grouped: BTreeMap<(String, String), Vec<&PropertyProposal>> = BTreeMap::new();
        for p in &self.proposals {
            grouped
                .entry((p.entity.clone(), p.pset.clone()))
                .or_default()
                .push(p);
        }
        let mut ops = Vec::new();
        for ((entity, pset), group) in grouped {
            let Ok(target) = entity.parse::<EntityId>() else {
                continue;
            };
            let mut props_array = Vec::new();
            let mut sum_conf = 0.0_f64;
            for p in &group {
                sum_conf += p.confidence;
                props_array.push(serde_json::json!({
                    "key": p.key,
                    "value": property_value_to_json(&p.value),
                }));
            }
            let avg_conf = sum_conf / group.len() as f64;
            let patch = serde_json::json!({
                "pset": pset,
                "properties": props_array,
                "confidence": avg_conf,
                "source": "ai",
            });
            ops.push(DiffOperation::Update { target, patch });
        }
        Diff {
            id: DiffId::new(),
            tool: ToolName::PropertyFill,
            status: DiffStatus::Pending,
            operations: ops,
        }
    }
}

fn property_value_to_json(v: &PropertyValue) -> serde_json::Value {
    match v {
        PropertyValue::Text(s) | PropertyValue::Label(s) => serde_json::Value::String(s.clone()),
        PropertyValue::Boolean(b) => serde_json::Value::Bool(*b),
        // IfcLogical is tri-state; the two-valued `Bool` JSON shape
        // can't carry the `.U.` (unknown) case. Emit as a tagged
        // object so reviewers see the explicit tri-state rather than
        // a silent coercion of `.U.` → `false`. The `bool` field is
        // populated only for the True/False cases (via
        // [`LogicalValue::as_optional_bool`]); for `Unknown` the
        // `bool` field is `null` and the `logical` discriminator
        // makes the distinction unambiguous.
        PropertyValue::Logical(v) => serde_json::json!({
            "logical": v.as_step_literal(),
            "bool": v.as_optional_bool(),
        }),
        PropertyValue::Integer(i) => serde_json::Value::Number((*i).into()),
        PropertyValue::Real(f)
        | PropertyValue::Length(f)
        | PropertyValue::Area(f)
        | PropertyValue::Volume(f)
        | PropertyValue::Ratio(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        // Round-trip-only carrier for unmodeled IFC measure types
        // (e.g. `IfcMassDensityMeasure`). The diff payload exposes
        // both the raw STEP literal and the canonical measure tag
        // so reviewers can see what's being proposed.
        PropertyValue::Other { measure, raw } => serde_json::json!({
            "measure": measure,
            "raw": raw,
        }),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PropertyFillConfig {
    /// Drop proposals below this confidence.
    pub min_confidence: f64,
    /// Cap on confidence regardless of specificity.
    pub max_confidence: f64,
}

impl Default for PropertyFillConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.80,
            max_confidence: 0.97,
        }
    }
}

/// Walk every required key in the canonical pset for the element's
/// class, propose a value if a matching rule supplies one, and skip
/// keys that already have a known value.
pub fn fill_properties(
    elements: &[ElementContext],
    standards: &ProjectStandards,
    config: &PropertyFillConfig,
) -> PropertyFillResult {
    let mut out = Vec::new();
    for ctx in elements {
        let Some(pset_name) = standard_pset_name_for_class(&ctx.class) else {
            continue;
        };
        let required = standard_pset_keys(&ctx.class);
        if required.is_empty() {
            continue;
        }
        let known_for_pset = ctx.known.get(pset_name);

        let matching = standards.matching(ctx);
        for &key in required {
            if known_for_pset.and_then(|m| m.get(key)).is_some() {
                continue;
            }
            // Find best matching rule that supplies a value for this key.
            let mut best: Option<(f64, &PropertyValue)> = None;
            for rule in &matching {
                if let Some(props) = rule.defaults.get(pset_name) {
                    if let Some(val) = props.get(key) {
                        let conf = confidence_for_rule(rule, config);
                        if best.map_or(true, |(c, _)| conf > c) {
                            best = Some((conf, val));
                        }
                    }
                }
            }
            if let Some((conf, val)) = best {
                if conf >= config.min_confidence {
                    out.push(PropertyProposal {
                        entity: ctx.entity.clone(),
                        pset: pset_name.to_string(),
                        key: key.to_string(),
                        value: val.clone(),
                        confidence: conf,
                    });
                }
            }
        }
    }
    PropertyFillResult { proposals: out }
}

fn confidence_for_rule(rule: &StandardRule, config: &PropertyFillConfig) -> f64 {
    let specificity = rule.r#match.len() as f64;
    (0.80_f64 + 0.05_f64 * specificity).min(config.max_confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(entity: &str, class: IfcClass) -> ElementContext {
        ElementContext {
            entity: entity.into(),
            class,
            known: BTreeMap::new(),
        }
    }

    fn standards_for_walls() -> ProjectStandards {
        let mut generic = BTreeMap::new();
        let mut pset_common = BTreeMap::new();
        pset_common.insert("LoadBearing".into(), PropertyValue::Boolean(false));
        pset_common.insert("IsExternal".into(), PropertyValue::Boolean(false));
        pset_common.insert("FireRating".into(), PropertyValue::Label("EI30".into()));
        pset_common.insert("AcousticRating".into(), PropertyValue::Label("Rw45".into()));
        pset_common.insert("ThermalTransmittance".into(), PropertyValue::Real(0.30));
        pset_common.insert("Reference".into(), PropertyValue::Label("W-XX".into()));
        generic.insert("Pset_WallCommon".into(), pset_common);

        let mut specific = BTreeMap::new();
        let mut pset_common_ext = BTreeMap::new();
        pset_common_ext.insert("FireRating".into(), PropertyValue::Label("EI60".into()));
        pset_common_ext.insert("ThermalTransmittance".into(), PropertyValue::Real(0.18));
        specific.insert("Pset_WallCommon".into(), pset_common_ext);
        let mut m = BTreeMap::new();
        m.insert("IsExternal".into(), PropertyValue::Boolean(true));
        ProjectStandards {
            rules: vec![
                StandardRule {
                    class: IfcClass::IfcWall,
                    r#match: BTreeMap::new(),
                    defaults: generic,
                },
                StandardRule {
                    class: IfcClass::IfcWall,
                    r#match: m,
                    defaults: specific,
                },
            ],
        }
    }

    #[test]
    fn empty_known_props_get_generic_defaults() {
        let standards = standards_for_walls();
        let e = ctx("ent_aabbccdd", IfcClass::IfcWall);
        let r = fill_properties(&[e], &standards, &PropertyFillConfig::default());
        let keys: Vec<&str> = r.proposals.iter().map(|p| p.key.as_str()).collect();
        assert!(keys.contains(&"FireRating"));
        assert!(keys.contains(&"LoadBearing"));
        assert!(keys.contains(&"Reference"));
    }

    #[test]
    fn specific_rule_wins_over_generic_when_match_predicate_fires() {
        let standards = standards_for_walls();
        let mut e = ctx("ent_aabbccdd", IfcClass::IfcWall);
        let mut pset = BTreeMap::new();
        pset.insert("IsExternal".into(), PropertyValue::Boolean(true));
        e.known.insert("Pset_WallCommon".into(), pset);
        let r = fill_properties(&[e], &standards, &PropertyFillConfig::default());
        let fire = r.proposals.iter().find(|p| p.key == "FireRating").unwrap();
        assert!(matches!(&fire.value, PropertyValue::Label(l) if l == "EI60"));
        assert!(fire.confidence > 0.83); // specific rule has confidence 0.85
    }

    #[test]
    fn known_properties_are_not_proposed() {
        let standards = standards_for_walls();
        let mut e = ctx("ent_aabbccdd", IfcClass::IfcWall);
        let mut pset = BTreeMap::new();
        pset.insert("FireRating".into(), PropertyValue::Label("EI90".into()));
        e.known.insert("Pset_WallCommon".into(), pset);
        let r = fill_properties(&[e], &standards, &PropertyFillConfig::default());
        let fire = r.proposals.iter().find(|p| p.key == "FireRating");
        assert!(fire.is_none(), "should not propose a value for FireRating");
    }

    #[test]
    fn unknown_class_has_no_proposals() {
        let standards = standards_for_walls();
        let e = ctx("ent_aabbccdd", IfcClass::IfcFurniture);
        let r = fill_properties(&[e], &standards, &PropertyFillConfig::default());
        assert!(r.proposals.is_empty());
    }

    #[test]
    fn standards_serialize_round_trip() {
        let s = standards_for_walls();
        let json = serde_json::to_string(&s).unwrap();
        let round: ProjectStandards = serde_json::from_str(&json).unwrap();
        assert_eq!(round, s);
    }

    #[test]
    fn diff_groups_by_entity_and_pset() {
        let standards = standards_for_walls();
        let id1 = EntityId::new().to_string();
        let id2 = EntityId::new().to_string();
        let e1 = ctx(&id1, IfcClass::IfcWall);
        let e2 = ctx(&id2, IfcClass::IfcWall);
        let r = fill_properties(&[e1, e2], &standards, &PropertyFillConfig::default());
        let d = r.to_diff();
        // Two entities × one pset each.
        assert_eq!(d.operations.len(), 2);
        for op in &d.operations {
            if let DiffOperation::Update { patch, .. } = op {
                let props = patch.get("properties").and_then(|p| p.as_array()).unwrap();
                assert!(!props.is_empty());
            } else {
                panic!("expected Update");
            }
        }
    }

    #[test]
    fn diff_skips_invalid_entity_ids() {
        let standards = standards_for_walls();
        let e = ctx("not-a-real-id", IfcClass::IfcWall);
        let r = fill_properties(&[e], &standards, &PropertyFillConfig::default());
        let d = r.to_diff();
        assert!(d.operations.is_empty());
    }

    #[test]
    fn output_matches_grammar() {
        let standards = standards_for_walls();
        let id = EntityId::new().to_string();
        let e = ctx(&id, IfcClass::IfcWall);
        let r = fill_properties(&[e], &standards, &PropertyFillConfig::default());
        // Build JSON in the grammar's shape.
        let psets: Vec<_> = r
            .proposals
            .iter()
            .map(|p| {
                serde_json::json!({
                    "entity": p.entity,
                    "pset": p.pset,
                    "properties": [{
                        "key": p.key,
                        "value": property_value_to_json(&p.value),
                    }],
                    "confidence": p.confidence,
                })
            })
            .collect();
        let payload = serde_json::to_string(&serde_json::json!({ "psets": psets })).unwrap();
        let g = crate::grammars::GrammarRegistry::defaults();
        let grammar = g.get("property_fill").unwrap();
        assert!(grammar.matches(&payload), "{}", payload);
    }
}
