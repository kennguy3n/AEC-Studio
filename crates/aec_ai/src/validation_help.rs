//! Validation-help AI tool.
//!
//! After the BIM validator (`aec_bim::validate_project`) produces a
//! list of findings, this tool turns each into a concrete suggested
//! action the user can preview and accept. The actions are bounded
//! to a small set so the diff engine knows how to interpret them.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::planner::PlanResponse;

#[derive(Debug, Error)]
pub enum ValidationHelpError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rationale must not be empty")]
    EmptyRationale,
    #[error("finding_id `{0}` does not match a known finding")]
    UnknownFinding(String),
    #[error("fix action `{0}` is not a recognised verb")]
    UnknownAction(String),
}

/// Suggested action verbs. Kept narrow so the diff engine can
/// dispatch deterministically rather than parsing free-form text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixAction {
    /// Classify an unclassified entity (sets `IfcClass`).
    Classify,
    /// Set or change a property value (e.g. a Pset key).
    SetProperty,
    /// Delete an orphaned entity.
    DeleteEntity,
    /// Attach a relation that should exist (e.g. containment).
    AddRelation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationFix {
    /// `ValidationFinding::code` from the validation report.
    pub finding_id: String,
    /// Bounded verb describing the kind of edit.
    pub suggested_action: FixAction,
    /// Free-form rationale shown next to the suggestion.
    pub rationale: String,
    /// Tool-call payload (e.g. `{ "class": "IfcWall" }` for a
    /// `Classify` action). Kept as JSON because the shape varies per
    /// action — the diff engine inspects `suggested_action` first
    /// and reads only the keys it knows for that verb.
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationHelpResult {
    pub rationale: String,
    pub fixes: Vec<ValidationFix>,
}

impl ValidationHelpResult {
    pub fn parse(raw: &str) -> Result<Self, ValidationHelpError> {
        let result: ValidationHelpResult = serde_json::from_str(raw.trim())?;
        result.basic_validate()?;
        Ok(result)
    }

    pub fn from_response(r: &PlanResponse) -> Result<Self, ValidationHelpError> {
        let result: ValidationHelpResult = serde_json::from_value(r.parsed.clone())?;
        result.basic_validate()?;
        Ok(result)
    }

    /// Stronger validation that requires the caller to supply the
    /// known finding-ids from the BIM report. Use this when wiring
    /// the result into the audit trail.
    pub fn validate_against<'a, I, S>(&self, known_ids: I) -> Result<(), ValidationHelpError>
    where
        I: IntoIterator<Item = &'a S>,
        S: AsRef<str> + 'a + ?Sized,
    {
        self.basic_validate()?;
        let known: std::collections::HashSet<&str> =
            known_ids.into_iter().map(AsRef::as_ref).collect();
        for fix in &self.fixes {
            if !known.contains(fix.finding_id.as_str()) {
                return Err(ValidationHelpError::UnknownFinding(fix.finding_id.clone()));
            }
        }
        Ok(())
    }

    fn basic_validate(&self) -> Result<(), ValidationHelpError> {
        if self.rationale.trim().is_empty() {
            return Err(ValidationHelpError::EmptyRationale);
        }
        for fix in &self.fixes {
            if fix.rationale.trim().is_empty() {
                return Err(ValidationHelpError::EmptyRationale);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_payload() -> serde_json::Value {
        serde_json::json!({
            "rationale": "Resolve missing classifications and stray relations.",
            "fixes": [
                {
                    "finding_id": "MISSING_CLASSIFICATION",
                    "suggested_action": "classify",
                    "rationale": "Wall-shaped entity is unclassified.",
                    "payload": { "class": "IfcWall" }
                },
                {
                    "finding_id": "ORPHAN_ELEMENT",
                    "suggested_action": "delete_entity",
                    "rationale": "Entity has no containment relation."
                }
            ]
        })
    }

    #[test]
    fn parses_well_formed_payload() {
        let r = ValidationHelpResult::parse(&good_payload().to_string()).unwrap();
        assert_eq!(r.fixes.len(), 2);
        assert_eq!(r.fixes[0].suggested_action, FixAction::Classify);
        assert_eq!(r.fixes[0].payload["class"], "IfcWall");
    }

    #[test]
    fn validates_against_known_ids() {
        let r = ValidationHelpResult::parse(&good_payload().to_string()).unwrap();
        let known = ["MISSING_CLASSIFICATION", "ORPHAN_ELEMENT", "OTHER"];
        r.validate_against(&known).unwrap();
    }

    #[test]
    fn rejects_unknown_finding_id() {
        let r = ValidationHelpResult::parse(&good_payload().to_string()).unwrap();
        let known = ["ONLY_THIS"];
        let err = r.validate_against(&known).unwrap_err();
        assert!(matches!(err, ValidationHelpError::UnknownFinding(_)));
    }

    #[test]
    fn rejects_empty_rationale() {
        let mut payload = good_payload();
        payload["rationale"] = serde_json::json!("");
        assert!(matches!(
            ValidationHelpResult::parse(&payload.to_string()).unwrap_err(),
            ValidationHelpError::EmptyRationale
        ));
    }

    #[test]
    fn rejects_empty_per_fix_rationale() {
        let mut payload = good_payload();
        payload["fixes"][0]["rationale"] = serde_json::json!("   ");
        assert!(matches!(
            ValidationHelpResult::parse(&payload.to_string()).unwrap_err(),
            ValidationHelpError::EmptyRationale
        ));
    }

    #[test]
    fn fix_action_round_trips() {
        for v in [
            FixAction::Classify,
            FixAction::SetProperty,
            FixAction::DeleteEntity,
            FixAction::AddRelation,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: FixAction = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }
}
