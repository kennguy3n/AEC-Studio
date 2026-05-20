//! Dynamic blocks — block definitions with parameter-driven variants.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DynamicParameter {
    /// Discrete visibility state — switches which entities are visible.
    Visibility {
        name: String,
        states: Vec<String>,
        current: String,
    },
    /// Stretch parameter — moves selected grips by a distance.
    Stretch {
        name: String,
        min: f64,
        max: f64,
        current: f64,
    },
    /// Lookup parameter — pick from a fixed list of pre-baked values.
    Lookup {
        name: String,
        table: BTreeMap<String, String>,
        current: String,
    },
}

impl DynamicParameter {
    pub fn name(&self) -> &str {
        match self {
            DynamicParameter::Visibility { name, .. }
            | DynamicParameter::Stretch { name, .. }
            | DynamicParameter::Lookup { name, .. } => name,
        }
    }

    pub fn set_current(&mut self, value: &str) -> bool {
        match self {
            DynamicParameter::Visibility {
                states, current, ..
            } => {
                if states.iter().any(|s| s == value) {
                    *current = value.into();
                    true
                } else {
                    false
                }
            }
            DynamicParameter::Lookup { table, current, .. } => {
                if table.contains_key(value) {
                    *current = value.into();
                    true
                } else {
                    false
                }
            }
            DynamicParameter::Stretch {
                current, min, max, ..
            } => {
                if let Ok(v) = value.parse::<f64>() {
                    if v >= *min && v <= *max {
                        *current = v;
                        return true;
                    }
                }
                false
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DynamicBlockSpec {
    pub parameters: Vec<DynamicParameter>,
    /// Map a parameter name → set of entity indices that respond to it.
    pub entity_visibility: BTreeMap<String, Vec<usize>>,
}

impl DynamicBlockSpec {
    pub fn visible_entities(&self, all_entity_indices: &[usize]) -> Vec<usize> {
        // Start by including all entity indices.
        let mut visible: std::collections::BTreeSet<usize> =
            all_entity_indices.iter().copied().collect();
        for param in &self.parameters {
            if let DynamicParameter::Visibility { current, .. } = param {
                if let Some(list) = self.entity_visibility.get(current) {
                    // Hide everything not in this list.
                    let allowed: std::collections::BTreeSet<usize> = list.iter().copied().collect();
                    visible.retain(|i| allowed.contains(i));
                }
            }
        }
        visible.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_state_switch() {
        let mut p = DynamicParameter::Visibility {
            name: "VIEW".into(),
            states: vec!["LEFT".into(), "RIGHT".into()],
            current: "LEFT".into(),
        };
        assert!(p.set_current("RIGHT"));
        assert!(!p.set_current("UP"));
    }

    #[test]
    fn stretch_parameter_clamps() {
        let mut p = DynamicParameter::Stretch {
            name: "WIDTH".into(),
            min: 0.0,
            max: 10.0,
            current: 5.0,
        };
        assert!(p.set_current("7.5"));
        assert!(!p.set_current("11.0"));
        assert!(!p.set_current("oops"));
    }

    #[test]
    fn visibility_filters_entities() {
        let mut spec = DynamicBlockSpec::default();
        spec.parameters.push(DynamicParameter::Visibility {
            name: "STATE".into(),
            states: vec!["A".into(), "B".into()],
            current: "A".into(),
        });
        spec.entity_visibility.insert("A".into(), vec![0, 1]);
        spec.entity_visibility.insert("B".into(), vec![2]);
        let v = spec.visible_entities(&[0, 1, 2]);
        assert_eq!(v, vec![0, 1]);
    }
}
