//! Block attributes — tagged user-editable values on each block insert.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeKind {
    /// User-editable text (default).
    Text,
    /// Constant value — cannot be edited on the insert.
    Constant,
    /// Verified value — the editor prompts on insert.
    Verify,
    /// Preset — auto-fills the default value without prompting.
    Preset,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockAttribute {
    pub tag: String,
    pub prompt: String,
    pub default_value: String,
    pub kind: AttributeKind,
    pub invisible: bool,
    #[serde(default)]
    pub position: [f64; 2],
    #[serde(default = "default_height")]
    pub height: f64,
}

fn default_height() -> f64 {
    2.5
}

impl BlockAttribute {
    pub fn new(tag: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            prompt: prompt.into(),
            default_value: String::new(),
            kind: AttributeKind::Text,
            invisible: false,
            position: [0.0, 0.0],
            height: default_height(),
        }
    }

    pub fn with_default(mut self, value: impl Into<String>) -> Self {
        self.default_value = value.into();
        self
    }

    pub fn constant(mut self) -> Self {
        self.kind = AttributeKind::Constant;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_attribute_with_default() {
        let a = BlockAttribute::new("TAG_NUM", "Tag Number").with_default("A-101");
        assert_eq!(a.default_value, "A-101");
        assert_eq!(a.kind, AttributeKind::Text);
    }
}
