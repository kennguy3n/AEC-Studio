//! Render doctor — typed wrapper over the `render_doctor` tool response.

use serde::{Deserialize, Serialize};

use crate::planner::PlanResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderIssue {
    Noisy,
    Underexposed,
    Overexposed,
    ClippedShadows,
    Aliased,
    Other,
}

impl RenderIssue {
    pub fn parse_kind(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "noise" | "noisy" => Self::Noisy,
            "underexposed" | "underexposure" => Self::Underexposed,
            "overexposed" | "overexposure" => Self::Overexposed,
            "clipped_shadows" | "clipping" => Self::ClippedShadows,
            "aliased" | "aliasing" => Self::Aliased,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderDoctorFinding {
    pub issue: RenderIssue,
    pub severity: String,
    pub recommendation: Option<String>,
    /// Optional suggested sample-count bump.
    pub suggested_samples: Option<u32>,
    /// Optional EV adjustment (positive = brighter).
    pub suggested_ev_delta: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderDoctorResult {
    pub findings: Vec<RenderDoctorFinding>,
}

impl RenderDoctorResult {
    pub fn from_response(r: &PlanResponse) -> Self {
        let mut findings = Vec::new();
        if let Some(arr) = r.parsed.get("findings").and_then(|v| v.as_array()) {
            for f in arr {
                let issue = f
                    .get("issue")
                    .and_then(|s| s.as_str())
                    .map_or(RenderIssue::Other, RenderIssue::parse_kind);
                let severity = f
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("medium")
                    .to_string();
                let recommendation = f
                    .get("recommendation")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                let suggested_samples = f
                    .get("suggested_samples")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as u32);
                let suggested_ev_delta = f
                    .get("suggested_ev_delta")
                    .and_then(serde_json::Value::as_f64)
                    .map(|v| v as f32);
                findings.push(RenderDoctorFinding {
                    issue,
                    severity,
                    recommendation,
                    suggested_samples,
                    suggested_ev_delta,
                });
            }
        }
        Self { findings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_schema::ToolName;

    #[test]
    fn maps_noise_finding() {
        let r = PlanResponse {
            tool: ToolName::RenderDoctor,
            raw_payload: String::new(),
            parsed: serde_json::json!({
                "findings": [
                    { "issue": "noisy", "severity": "high",
                      "recommendation": "increase samples", "suggested_samples": 512 }
                ]
            }),
            entities_modified: 1,
        };
        let r = RenderDoctorResult::from_response(&r);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].issue, RenderIssue::Noisy);
        assert_eq!(r.findings[0].suggested_samples, Some(512));
    }
}
