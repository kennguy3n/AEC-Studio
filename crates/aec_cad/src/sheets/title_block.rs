//! Title block — border + populated fields.
//!
//! A title block is a simple template: a list of static text labels +
//! a list of field placeholders. The renderer substitutes the field
//! values from the project metadata at plot time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleBlockField {
    pub key: String,
    pub label: String,
    /// Paper-space position (mm).
    pub position: [f64; 2],
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleBlock {
    pub border_offset: [f64; 2],
    pub fields: Vec<TitleBlockField>,
    pub values: BTreeMap<String, String>,
}

impl TitleBlock {
    pub fn standard() -> Self {
        Self {
            border_offset: [10.0, 10.0],
            fields: vec![
                TitleBlockField {
                    key: "project.name".into(),
                    label: "Project".into(),
                    position: [20.0, 20.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "drawing.title".into(),
                    label: "Drawing".into(),
                    position: [20.0, 14.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "drawing.number".into(),
                    label: "Drawing No.".into(),
                    position: [20.0, 8.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "drawing.scale".into(),
                    label: "Scale".into(),
                    position: [120.0, 8.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "drawing.revision".into(),
                    label: "Revision".into(),
                    position: [160.0, 8.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "person.drawn_by".into(),
                    label: "Drawn".into(),
                    position: [200.0, 8.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "person.checked_by".into(),
                    label: "Checked".into(),
                    position: [240.0, 8.0],
                    height: 4.0,
                },
                TitleBlockField {
                    key: "date".into(),
                    label: "Date".into(),
                    position: [280.0, 8.0],
                    height: 4.0,
                },
            ],
            values: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    pub fn populated_fields(&self) -> impl Iterator<Item = (&TitleBlockField, &str)> {
        self.fields
            .iter()
            .filter_map(|f| self.values.get(&f.key).map(|v| (f, v.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_title_has_fields() {
        let mut t = TitleBlock::standard();
        assert!(!t.fields.is_empty());
        t.set("project.name", "Test Project");
        t.set("date", "2025-05-19");
        let populated: Vec<_> = t.populated_fields().collect();
        assert_eq!(populated.len(), 2);
    }
}
