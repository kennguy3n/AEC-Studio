//! Linetype table — standard CAD linetype definitions.
//!
//! A linetype is a sequence of `LinetypeElement`s. Positive lengths are
//! pen-down (drawn), negative lengths are pen-up (gap). Zero is a dot.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{CadError, CadResult};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinetypeElement {
    /// `+` = dash, `-` = gap, `0` = dot. Length in millimetres at scale 1.
    pub length: f64,
}

impl LinetypeElement {
    pub fn dash(length: f64) -> Self {
        Self {
            length: length.abs().max(0.0),
        }
    }

    pub fn gap(length: f64) -> Self {
        Self {
            length: -length.abs(),
        }
    }

    pub fn dot() -> Self {
        Self { length: 0.0 }
    }

    pub fn is_dash(&self) -> bool {
        self.length > 0.0
    }
    pub fn is_gap(&self) -> bool {
        self.length < 0.0
    }
    pub fn is_dot(&self) -> bool {
        self.length == 0.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Linetype {
    pub name: String,
    pub description: String,
    pub elements: Vec<LinetypeElement>,
}

impl Linetype {
    pub fn pattern_length(&self) -> f64 {
        self.elements.iter().map(|e| e.length.abs()).sum()
    }

    pub fn continuous() -> Self {
        Self {
            name: "CONTINUOUS".into(),
            description: "Solid line".into(),
            elements: Vec::new(),
        }
    }

    pub fn dashed() -> Self {
        Self {
            name: "DASHED".into(),
            description: "Dashed".into(),
            elements: vec![LinetypeElement::dash(6.35), LinetypeElement::gap(3.175)],
        }
    }

    pub fn center() -> Self {
        Self {
            name: "CENTER".into(),
            description: "Center long-short".into(),
            elements: vec![
                LinetypeElement::dash(31.75),
                LinetypeElement::gap(6.35),
                LinetypeElement::dash(6.35),
                LinetypeElement::gap(6.35),
            ],
        }
    }

    pub fn hidden() -> Self {
        Self {
            name: "HIDDEN".into(),
            description: "Hidden".into(),
            elements: vec![LinetypeElement::dash(3.175), LinetypeElement::gap(1.5875)],
        }
    }

    pub fn phantom() -> Self {
        Self {
            name: "PHANTOM".into(),
            description: "Phantom long-short-short".into(),
            elements: vec![
                LinetypeElement::dash(31.75),
                LinetypeElement::gap(6.35),
                LinetypeElement::dash(6.35),
                LinetypeElement::gap(6.35),
                LinetypeElement::dash(6.35),
                LinetypeElement::gap(6.35),
            ],
        }
    }

    pub fn dashdot() -> Self {
        Self {
            name: "DASHDOT".into(),
            description: "Dash dot".into(),
            elements: vec![
                LinetypeElement::dash(12.7),
                LinetypeElement::gap(6.35),
                LinetypeElement::dot(),
                LinetypeElement::gap(6.35),
            ],
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinetypeTable {
    types: BTreeMap<String, Linetype>,
}

impl LinetypeTable {
    pub fn standard() -> Self {
        let mut t = Self::default();
        for lt in [
            Linetype::continuous(),
            Linetype::dashed(),
            Linetype::center(),
            Linetype::hidden(),
            Linetype::phantom(),
            Linetype::dashdot(),
        ] {
            t.types.insert(lt.name.clone(), lt);
        }
        t
    }

    pub fn get(&self, name: &str) -> Option<&Linetype> {
        self.types.get(name)
    }

    pub fn upsert(&mut self, lt: Linetype) {
        self.types.insert(lt.name.clone(), lt);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Linetype> {
        self.types.values()
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn remove(&mut self, name: &str) -> CadResult<()> {
        if name == "CONTINUOUS" {
            return Err(CadError::InvalidLayerName(
                "cannot remove CONTINUOUS linetype".into(),
            ));
        }
        self.types
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_table_has_continuous() {
        let t = LinetypeTable::standard();
        assert!(t.get("CONTINUOUS").is_some());
        assert!(t.get("DASHED").is_some());
        assert!(t.get("HIDDEN").is_some());
        assert!(t.get("CENTER").is_some());
    }

    #[test]
    fn pattern_length_sums_absolute() {
        let lt = Linetype::dashed();
        assert!((lt.pattern_length() - (6.35 + 3.175)).abs() < 1e-9);
    }

    #[test]
    fn cannot_remove_continuous() {
        let mut t = LinetypeTable::standard();
        assert!(t.remove("CONTINUOUS").is_err());
    }
}
