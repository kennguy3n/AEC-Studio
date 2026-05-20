//! Lighting-balance AI tool.
//!
//! The model studies a reference image (or a scene + mood brief) and
//! proposes one or more **accent** lights to fill out the scene
//! without changing the dominant lighting direction. The tool returns
//! a structured list of light placements which the diff engine then
//! turns into `Insert` operations against `aec_render::scene::RenderLight`.
//!
//! Why a dedicated tool rather than re-using
//! [`crate::style_assistant`]? Style assistant returns aesthetic
//! choices (palette, materials, furniture); lighting balance returns
//! photometric choices (positions, intensities, colour temperatures).
//! Keeping them separate lets the safety validator bound the maximum
//! number of lights an AI run can introduce.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::diff_engine::DiffOperation;
use crate::planner::PlanResponse;

/// Maximum number of accent lights a single response may propose.
/// Mirrors the `max_entities_modified` for the `lighting_balance`
/// schema in `data/ai_tools.json`; the safety validator enforces this
/// independently — this constant is the parser's own cap.
pub const MAX_ACCENT_LIGHTS: usize = 8;

#[derive(Debug, Error)]
pub enum LightingBalanceError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("too many accent lights: {count} (max {max})")]
    TooManyLights { count: usize, max: usize },
    /// Reserved for actual NaN / ±Infinity floats that escape the JSON
    /// parser (e.g. via `"NaN"` numeric literals on some non-strict
    /// inputs, or values produced from arithmetic before validation).
    /// Out-of-range but finite values are reported via [`OutOfRange`]
    /// so callers can give the user a precise reason.
    #[error("non-finite numeric value for `{field}`")]
    NonFinite { field: &'static str },
    /// Finite but outside the accepted range for the field.
    #[error("`{field}` out of range: got {value}, expected {expected}")]
    OutOfRange {
        field: &'static str,
        value: f32,
        expected: &'static str,
    },
    #[error("rationale must not be empty")]
    EmptyRationale,
}

/// A single accent-light proposal. Maps onto an
/// `aec_render::scene::RenderLight::Area` or `::Point` depending on
/// `kind`. We keep the AI-side struct independent of the render
/// crate's enum (which would create a cycle) — the diff engine knows
/// how to translate one into the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccentLight {
    pub kind: AccentLightKind,
    /// Position in millimetres (x, y, z) relative to the room anchor.
    pub position_mm: [f32; 3],
    /// Linear intensity multiplier. `1.0` is the project default.
    pub intensity: f32,
    /// Colour temperature in kelvin. Roughly 2700-6500 for indoor
    /// scenes; the parser enforces a sanity range.
    pub color_temperature_k: f32,
    /// Optional rationale shown next to the suggestion in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentLightKind {
    Area,
    Point,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingBalanceResult {
    /// Overall rationale string from the model.
    pub rationale: String,
    pub suggested_accents: Vec<AccentLight>,
}

impl LightingBalanceResult {
    /// Parse a raw model response.
    pub fn parse(raw: &str) -> Result<Self, LightingBalanceError> {
        let result: LightingBalanceResult = serde_json::from_str(raw.trim())?;
        result.validate()?;
        Ok(result)
    }

    /// Build a result from a [`PlanResponse`]. Useful when the planner
    /// has already parsed the raw payload into a JSON value.
    pub fn from_response(r: &PlanResponse) -> Result<Self, LightingBalanceError> {
        let result: LightingBalanceResult = serde_json::from_value(r.parsed.clone())?;
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), LightingBalanceError> {
        if self.rationale.trim().is_empty() {
            return Err(LightingBalanceError::EmptyRationale);
        }
        if self.suggested_accents.len() > MAX_ACCENT_LIGHTS {
            return Err(LightingBalanceError::TooManyLights {
                count: self.suggested_accents.len(),
                max: MAX_ACCENT_LIGHTS,
            });
        }
        for (idx, light) in self.suggested_accents.iter().enumerate() {
            for (axis, v) in light.position_mm.iter().enumerate() {
                if !v.is_finite() {
                    return Err(LightingBalanceError::NonFinite {
                        field: ["position_mm[0]", "position_mm[1]", "position_mm[2]"][axis],
                    });
                }
            }
            if !light.intensity.is_finite() {
                return Err(LightingBalanceError::NonFinite { field: "intensity" });
            }
            if light.intensity < 0.0 {
                return Err(LightingBalanceError::OutOfRange {
                    field: "intensity",
                    value: light.intensity,
                    expected: ">= 0.0",
                });
            }
            if !light.color_temperature_k.is_finite() {
                return Err(LightingBalanceError::NonFinite {
                    field: "color_temperature_k",
                });
            }
            if !(1000.0..=12_000.0).contains(&light.color_temperature_k) {
                return Err(LightingBalanceError::OutOfRange {
                    field: "color_temperature_k",
                    value: light.color_temperature_k,
                    expected: "1000.0..=12000.0 K",
                });
            }
            let _ = idx;
        }
        Ok(())
    }

    /// Turn each accent into a `DiffOperation::Insert` carrying a
    /// `render_light` payload the engine can persist as a new light
    /// entity. The diff engine wires the actual `RenderLight` enum.
    pub fn to_diff_operations(&self) -> Vec<DiffOperation> {
        self.suggested_accents
            .iter()
            .map(|light| DiffOperation::Insert {
                entity_kind: "render_light".into(),
                payload: serde_json::json!({
                    "kind": light.kind,
                    "position_mm": light.position_mm,
                    "intensity": light.intensity,
                    "color_temperature_k": light.color_temperature_k,
                    "rationale": light.rationale,
                }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_payload() -> serde_json::Value {
        serde_json::json!({
            "rationale": "Add a warm rim to balance the cold daylight key.",
            "suggested_accents": [
                {
                    "kind": "area",
                    "position_mm": [1500.0, 800.0, 2200.0],
                    "intensity": 1.4,
                    "color_temperature_k": 3000.0,
                    "rationale": "Soft fill behind the sofa"
                },
                {
                    "kind": "point",
                    "position_mm": [-1000.0, 200.0, 1800.0],
                    "intensity": 0.6,
                    "color_temperature_k": 2700.0
                }
            ]
        })
    }

    #[test]
    fn parses_well_formed_payload() {
        let raw = good_payload().to_string();
        let r = LightingBalanceResult::parse(&raw).unwrap();
        assert_eq!(r.suggested_accents.len(), 2);
        assert_eq!(r.suggested_accents[0].kind, AccentLightKind::Area);
    }

    #[test]
    fn rejects_too_many_accents() {
        let mut accents = Vec::new();
        for _ in 0..=MAX_ACCENT_LIGHTS {
            accents.push(serde_json::json!({
                "kind": "point",
                "position_mm": [0.0, 0.0, 0.0],
                "intensity": 1.0,
                "color_temperature_k": 3000.0,
            }));
        }
        let payload = serde_json::json!({
            "rationale": "many",
            "suggested_accents": accents,
        });
        let err = LightingBalanceResult::parse(&payload.to_string()).unwrap_err();
        assert!(matches!(err, LightingBalanceError::TooManyLights { .. }));
    }

    #[test]
    fn rejects_non_finite_position() {
        let mut payload = good_payload();
        payload["suggested_accents"][0]["position_mm"] = serde_json::json!([0.0, 0.0, "NaN"]);
        // serde will fail to parse a string-where-number is expected before
        // our validator runs; assert that path fails too.
        assert!(LightingBalanceResult::parse(&payload.to_string()).is_err());
    }

    #[test]
    fn rejects_out_of_range_color_temperature() {
        let mut payload = good_payload();
        payload["suggested_accents"][0]["color_temperature_k"] = serde_json::json!(50_000.0);
        let err = LightingBalanceResult::parse(&payload.to_string()).unwrap_err();
        assert!(matches!(
            err,
            LightingBalanceError::OutOfRange {
                field: "color_temperature_k",
                ..
            }
        ));
    }

    #[test]
    fn rejects_negative_intensity_as_out_of_range() {
        let mut payload = good_payload();
        payload["suggested_accents"][0]["intensity"] = serde_json::json!(-0.25);
        let err = LightingBalanceResult::parse(&payload.to_string()).unwrap_err();
        assert!(matches!(
            err,
            LightingBalanceError::OutOfRange {
                field: "intensity",
                ..
            }
        ));
    }

    #[test]
    fn rejects_empty_rationale() {
        let mut payload = good_payload();
        payload["rationale"] = serde_json::json!("   ");
        let err = LightingBalanceResult::parse(&payload.to_string()).unwrap_err();
        assert!(matches!(err, LightingBalanceError::EmptyRationale));
    }

    #[test]
    fn diff_operations_insert_one_light_per_accent() {
        let r = LightingBalanceResult::parse(&good_payload().to_string()).unwrap();
        let ops = r.to_diff_operations();
        assert_eq!(ops.len(), 2);
        for op in &ops {
            assert!(
                matches!(op, DiffOperation::Insert { entity_kind, .. } if entity_kind == "render_light")
            );
        }
    }
}
