//! Layer state manager.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{CadError, CadResult};

/// AutoCAD Color Index. 0 = ByBlock, 256 = ByLayer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LayerColor(pub i16);

impl LayerColor {
    pub const BYLAYER: LayerColor = LayerColor(256);
    pub const BYBLOCK: LayerColor = LayerColor(0);
    pub const WHITE: LayerColor = LayerColor(7);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LayerLineweight(pub i16);

impl LayerLineweight {
    pub const DEFAULT: LayerLineweight = LayerLineweight(-3);
    /// Hundredths of mm (AutoCAD encoding); `25` = 0.25mm, `50` = 0.50mm, etc.
    pub fn from_mm(mm: f64) -> Self {
        Self((mm * 100.0).round() as i16)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub color: LayerColor,
    pub linetype: String,
    pub lineweight: LayerLineweight,
    pub frozen: bool,
    pub locked: bool,
    pub plottable: bool,
}

impl Layer {
    pub fn new(name: impl Into<String>) -> CadResult<Self> {
        let name = name.into();
        if name.is_empty()
            || name.chars().any(|c| {
                matches!(
                    c,
                    '<' | '>' | '/' | '"' | ':' | ';' | '?' | '*' | '|' | ',' | '=' | '`'
                )
            })
        {
            return Err(CadError::InvalidLayerName(name));
        }
        Ok(Self {
            name,
            color: LayerColor::WHITE,
            linetype: "CONTINUOUS".into(),
            lineweight: LayerLineweight::DEFAULT,
            frozen: false,
            locked: false,
            plottable: true,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSystem {
    layers: BTreeMap<String, Layer>,
}

impl LayerSystem {
    pub fn new() -> Self {
        let mut system = Self::default();
        // The "0" layer always exists in DXF.
        system
            .insert(Layer::new("0").expect("`0` is a valid layer name"))
            .expect("first insert");
        system
    }

    pub fn insert(&mut self, layer: Layer) -> CadResult<()> {
        self.layers.insert(layer.name.clone(), layer);
        Ok(())
    }

    pub fn upsert(&mut self, layer: Layer) {
        self.layers.insert(layer.name.clone(), layer);
    }

    pub fn get(&self, name: &str) -> Option<&Layer> {
        self.layers.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Layer> {
        self.layers.get_mut(name)
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Layer> {
        self.layers.values()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.layers.keys().map(String::as_str)
    }

    pub fn freeze(&mut self, name: &str) -> CadResult<()> {
        let l = self
            .get_mut(name)
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))?;
        l.frozen = true;
        Ok(())
    }

    pub fn thaw(&mut self, name: &str) -> CadResult<()> {
        let l = self
            .get_mut(name)
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))?;
        l.frozen = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_system_has_layer_zero() {
        let s = LayerSystem::new();
        assert!(s.get("0").is_some());
    }

    #[test]
    fn freeze_then_thaw_layer() {
        let mut s = LayerSystem::new();
        s.upsert(Layer::new("Walls").unwrap());
        s.freeze("Walls").unwrap();
        assert!(s.get("Walls").unwrap().frozen);
        s.thaw("Walls").unwrap();
        assert!(!s.get("Walls").unwrap().frozen);
    }

    #[test]
    fn invalid_layer_name_rejected() {
        assert!(Layer::new("foo/bar").is_err());
        assert!(Layer::new("").is_err());
    }
}
