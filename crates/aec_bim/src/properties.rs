//! Property sets (Pset_*), quantity sets (Qto_*), and custom psets.
//!
//! Property values are typed; the BIM cache fingerprints them with BLAKE3 so
//! we can detect changes when re-importing an IFC.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    Text(String),
    Real(f64),
    Integer(i64),
    Boolean(bool),
    Length(f64),
    Area(f64),
    Volume(f64),
    Ratio(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertySet {
    pub name: String,
    pub properties: BTreeMap<String, PropertyValue>,
}

impl PropertySet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: PropertyValue) -> &mut Self {
        self.properties.insert(key.into(), value);
        self
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("PropertySet serializes");
        blake3::hash(&bytes).into()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantitySet {
    pub name: String,
    pub quantities: BTreeMap<String, PropertyValue>,
}

impl QuantitySet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            quantities: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_with_content() {
        let mut a = PropertySet::new("Pset_WallCommon");
        a.set("LoadBearing", PropertyValue::Boolean(false));
        let f1 = a.fingerprint();
        a.set("LoadBearing", PropertyValue::Boolean(true));
        let f2 = a.fingerprint();
        assert_ne!(f1, f2);
    }
}
