//! Schedule-fill AI tool.
//!
//! Given a schedule template (door / window / room / material) and
//! the project's classified entities, the model fills in the values
//! for each row. Each cell is annotated with a confidence so the
//! reviewer can quickly triage low-confidence fields.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::planner::PlanResponse;

/// Per-cell confidence floor below which the row is automatically
/// flagged for human review. Tuned so a model that's reasonably sure
/// about most cells but uncertain about one or two doesn't drown out
/// the human reviewer.
pub const REVIEW_CONFIDENCE_THRESHOLD: f32 = 0.65;

#[derive(Debug, Error)]
pub enum ScheduleFillError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("filled rows must have at least one cell")]
    EmptyRow,
    #[error("cell confidence must be in 0.0..=1.0, got {0}")]
    InvalidConfidence(f32),
    #[error("row {row} cell `{cell}` missing")]
    MissingCell { row: usize, cell: String },
    #[error("rationale must not be empty")]
    EmptyRationale,
}

/// Single row of the filled schedule. `cells` is keyed by the column
/// key from the schedule template (e.g. `door_id`, `width_mm`).
/// `confidence` is keyed by the same column keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilledRow {
    /// The element this row describes — typically the `EntityId` of a
    /// door / window / room / material. Kept as a plain string here
    /// because the schedule key space is wider than just `EntityId`.
    pub element_ref: String,
    pub cells: BTreeMap<String, String>,
    pub confidence: BTreeMap<String, f32>,
}

impl FilledRow {
    /// True if any cell falls below the review threshold.
    pub fn needs_review(&self) -> bool {
        self.confidence
            .values()
            .any(|c| *c < REVIEW_CONFIDENCE_THRESHOLD)
    }

    /// Cells the reviewer should look at first.
    pub fn low_confidence_cells(&self) -> Vec<&str> {
        self.confidence
            .iter()
            .filter_map(|(k, c)| {
                if *c < REVIEW_CONFIDENCE_THRESHOLD {
                    Some(k.as_str())
                } else {
                    None
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleFillResult {
    pub rationale: String,
    pub filled_rows: Vec<FilledRow>,
}

impl ScheduleFillResult {
    pub fn parse(raw: &str) -> Result<Self, ScheduleFillError> {
        let result: ScheduleFillResult = serde_json::from_str(raw.trim())?;
        result.validate()?;
        Ok(result)
    }

    pub fn from_response(r: &PlanResponse) -> Result<Self, ScheduleFillError> {
        let result: ScheduleFillResult = serde_json::from_value(r.parsed.clone())?;
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), ScheduleFillError> {
        if self.rationale.trim().is_empty() {
            return Err(ScheduleFillError::EmptyRationale);
        }
        for (row_idx, row) in self.filled_rows.iter().enumerate() {
            if row.cells.is_empty() {
                return Err(ScheduleFillError::EmptyRow);
            }
            for (cell, _) in row.cells.iter() {
                if !row.confidence.contains_key(cell) {
                    return Err(ScheduleFillError::MissingCell {
                        row: row_idx,
                        cell: cell.clone(),
                    });
                }
            }
            for c in row.confidence.values() {
                if !c.is_finite() || !(0.0..=1.0).contains(c) {
                    return Err(ScheduleFillError::InvalidConfidence(*c));
                }
            }
        }
        Ok(())
    }

    /// Rows that need a human pass.
    pub fn rows_needing_review(&self) -> Vec<&FilledRow> {
        self.filled_rows
            .iter()
            .filter(|r| r.needs_review())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_payload() -> serde_json::Value {
        serde_json::json!({
            "rationale": "Filled door schedule from element properties.",
            "filled_rows": [
                {
                    "element_ref": "ent_door_001",
                    "cells": {
                        "id": "D-001",
                        "width_mm": "900",
                        "height_mm": "2100",
                        "fire_rating": "EI30"
                    },
                    "confidence": {
                        "id": 1.0,
                        "width_mm": 0.92,
                        "height_mm": 0.92,
                        "fire_rating": 0.40
                    }
                }
            ]
        })
    }

    #[test]
    fn parses_well_formed_payload() {
        let r = ScheduleFillResult::parse(&good_payload().to_string()).unwrap();
        assert_eq!(r.filled_rows.len(), 1);
        let row = &r.filled_rows[0];
        assert_eq!(row.cells["id"], "D-001");
        assert!((row.confidence["width_mm"] - 0.92).abs() < 1e-6);
    }

    #[test]
    fn rows_needing_review_flags_low_confidence() {
        let r = ScheduleFillResult::parse(&good_payload().to_string()).unwrap();
        let flagged = r.rows_needing_review();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].low_confidence_cells(), vec!["fire_rating"]);
    }

    #[test]
    fn empty_rationale_is_rejected() {
        let mut payload = good_payload();
        payload["rationale"] = serde_json::json!("");
        assert!(matches!(
            ScheduleFillResult::parse(&payload.to_string()).unwrap_err(),
            ScheduleFillError::EmptyRationale
        ));
    }

    #[test]
    fn confidence_outside_zero_one_is_rejected() {
        let mut payload = good_payload();
        payload["filled_rows"][0]["confidence"]["id"] = serde_json::json!(1.5);
        assert!(matches!(
            ScheduleFillResult::parse(&payload.to_string()).unwrap_err(),
            ScheduleFillError::InvalidConfidence(_)
        ));
    }

    #[test]
    fn missing_confidence_entry_is_rejected() {
        let mut payload = good_payload();
        payload["filled_rows"][0]["confidence"]
            .as_object_mut()
            .unwrap()
            .remove("fire_rating");
        let err = ScheduleFillResult::parse(&payload.to_string()).unwrap_err();
        assert!(matches!(err, ScheduleFillError::MissingCell { .. }));
    }

    #[test]
    fn empty_row_cells_are_rejected() {
        let mut payload = good_payload();
        payload["filled_rows"][0]["cells"] = serde_json::json!({});
        let err = ScheduleFillResult::parse(&payload.to_string()).unwrap_err();
        assert!(matches!(err, ScheduleFillError::EmptyRow));
    }

    #[test]
    fn all_high_confidence_means_no_rows_need_review() {
        let mut payload = good_payload();
        payload["filled_rows"][0]["confidence"]["fire_rating"] = serde_json::json!(0.95);
        let r = ScheduleFillResult::parse(&payload.to_string()).unwrap();
        assert!(r.rows_needing_review().is_empty());
    }
}
