//! Query DSL.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub style_tags: Vec<String>,
    /// Default 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}
