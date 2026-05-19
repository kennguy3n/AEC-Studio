//! Style assistant — typed wrapper around the `style_assistant` tool
//! response.

use serde::{Deserialize, Serialize};

use crate::planner::PlanResponse;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleSuggestion {
    pub furniture_ids: Vec<String>,
    pub material_ids: Vec<String>,
    pub lighting_preset_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleAssistantResult {
    pub suggestion: StyleSuggestion,
}

impl StyleAssistantResult {
    pub fn from_response(r: &PlanResponse) -> Option<Self> {
        let furniture = r
            .parsed
            .get("furniture_ids")
            .and_then(|v| v.as_array())?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let materials = r
            .parsed
            .get("material_ids")
            .and_then(|v| v.as_array())?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let preset = r
            .parsed
            .get("lighting_preset_id")
            .and_then(|v| v.as_str())?
            .to_string();
        Some(Self {
            suggestion: StyleSuggestion {
                furniture_ids: furniture,
                material_ids: materials,
                lighting_preset_id: preset,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_schema::ToolName;

    #[test]
    fn parses_full_payload() {
        let r = PlanResponse {
            tool: ToolName::StyleAssistant,
            raw_payload: String::new(),
            parsed: serde_json::json!({
                "furniture_ids": ["a","b"],
                "material_ids": ["m1"],
                "lighting_preset_id": "warm_evening",
            }),
            entities_modified: 3,
        };
        let res = StyleAssistantResult::from_response(&r).unwrap();
        assert_eq!(res.suggestion.furniture_ids.len(), 2);
        assert_eq!(res.suggestion.lighting_preset_id, "warm_evening");
    }
}
