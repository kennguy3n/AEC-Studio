//! Plan detection — converts a vision-model response into typed
//! [`PolylineProposal`]s ready to drop into Design mode.

use serde::{Deserialize, Serialize};

use crate::planner::PlanResponse;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolylineProposal {
    pub points_mm: Vec<[f64; 2]>,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDetectionResult {
    pub polylines: Vec<PolylineProposal>,
}

impl PlanDetectionResult {
    /// Build a typed result from a sidecar [`PlanResponse`] (already validated
    /// by the safety validator).
    pub fn from_response(r: &PlanResponse) -> Self {
        let mut out = Vec::new();
        if let Some(arr) = r.parsed.get("polylines").and_then(|v| v.as_array()) {
            for p in arr {
                let confidence = p
                    .get("confidence")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.5) as f32;
                let mut points = Vec::new();
                if let Some(pts) = p.get("points").and_then(|p| p.as_array()) {
                    for pt in pts {
                        if let Some(coords) = pt.as_array() {
                            if coords.len() == 2 {
                                let x = coords[0].as_f64().unwrap_or(0.0);
                                let y = coords[1].as_f64().unwrap_or(0.0);
                                points.push([x, y]);
                            }
                        }
                    }
                }
                out.push(PolylineProposal {
                    points_mm: points,
                    confidence,
                });
            }
        }
        Self { polylines: out }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_schema::ToolName;

    #[test]
    fn builds_polylines_from_response() {
        let r = PlanResponse {
            tool: ToolName::PlanDetection,
            raw_payload: String::new(),
            parsed: serde_json::json!({
                "polylines": [
                    { "points": [[0.0,0.0],[3000.0,0.0]], "confidence": 0.92 },
                    { "points": [[3000.0,0.0],[3000.0,2400.0]] }
                ]
            }),
            entities_modified: 2,
        };
        let res = PlanDetectionResult::from_response(&r);
        assert_eq!(res.polylines.len(), 2);
        assert!((res.polylines[0].confidence - 0.92).abs() < 1e-3);
        assert_eq!(res.polylines[0].points_mm.len(), 2);
    }
}
