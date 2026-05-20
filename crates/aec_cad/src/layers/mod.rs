//! Layer state manager + linetype/lineweight tables.

pub mod linetype;
pub mod lineweight;

pub use linetype::{Linetype, LinetypeElement, LinetypeTable};
pub use lineweight::{Lineweight, STANDARD_LINEWEIGHTS_MM};

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

    /// Map an ACI (AutoCAD color index) to an approximate sRGB tuple. The
    /// full AutoCAD palette is 256 entries; we ship the common ones used by
    /// our PDF/SVG plot output.
    pub fn to_srgb(self) -> [u8; 3] {
        match self.0 {
            1 => [255, 0, 0],
            2 => [255, 255, 0],
            3 => [0, 255, 0],
            4 => [0, 255, 255],
            5 => [0, 0, 255],
            6 => [255, 0, 255],
            7 => [255, 255, 255],
            8 => [128, 128, 128],
            9 => [192, 192, 192],
            // BYLAYER/BYBLOCK render as black on white paper.
            0 | 256 => [0, 0, 0],
            _ => [128, 128, 128],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LayerLineweight(pub i16);

impl LayerLineweight {
    pub const DEFAULT: LayerLineweight = LayerLineweight(-3);
    /// Hundredths of mm (AutoCAD encoding); `25` = 0.25mm, `50` = 0.50mm, etc.
    pub fn from_mm(mm: f64) -> Self {
        Self(Lineweight::from_mm(mm).0)
    }

    pub fn to_mm(self) -> Option<f64> {
        Lineweight(self.0).to_mm()
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
    #[serde(default = "default_on")]
    pub on: bool,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_on() -> bool {
    true
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
            on: true,
            description: None,
        })
    }

    /// True if entities on this layer should be rendered in the viewport.
    pub fn is_visible(&self) -> bool {
        !self.frozen && self.on
    }

    /// True if entities on this layer can be modified by editing tools.
    pub fn is_editable(&self) -> bool {
        self.is_visible() && !self.locked
    }
}

/// Layer system + the active "current layer" cursor. All new entities
/// inherit `current_layer()` unless the user overrides it explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSystem {
    layers: BTreeMap<String, Layer>,
    current: String,
    linetypes: LinetypeTable,
}

impl Default for LayerSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerSystem {
    pub fn new() -> Self {
        let mut system = Self {
            layers: BTreeMap::new(),
            current: "0".into(),
            linetypes: LinetypeTable::standard(),
        };
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

    pub fn remove(&mut self, name: &str) -> CadResult<()> {
        if name == "0" {
            return Err(CadError::InvalidLayerName("cannot delete layer 0".into()));
        }
        if self.current == name {
            self.current = "0".into();
        }
        self.layers
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))
    }

    pub fn rename(&mut self, old: &str, new: impl Into<String>) -> CadResult<()> {
        if old == "0" {
            return Err(CadError::InvalidLayerName("cannot rename layer 0".into()));
        }
        let new_name = new.into();
        if new_name.is_empty() {
            return Err(CadError::InvalidLayerName(new_name));
        }
        if self.layers.contains_key(&new_name) {
            return Err(CadError::InvalidLayerName(new_name));
        }
        let mut layer = self
            .layers
            .remove(old)
            .ok_or_else(|| CadError::InvalidLayerName(old.into()))?;
        layer.name.clone_from(&new_name);
        if self.current == old {
            self.current.clone_from(&new_name);
        }
        self.layers.insert(new_name, layer);
        Ok(())
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

    pub fn current_layer(&self) -> &str {
        &self.current
    }

    pub fn set_current(&mut self, name: &str) -> CadResult<()> {
        if !self.layers.contains_key(name) {
            return Err(CadError::InvalidLayerName(name.into()));
        }
        self.current = name.into();
        Ok(())
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

    pub fn lock(&mut self, name: &str) -> CadResult<()> {
        let l = self
            .get_mut(name)
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))?;
        l.locked = true;
        Ok(())
    }

    pub fn unlock(&mut self, name: &str) -> CadResult<()> {
        let l = self
            .get_mut(name)
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))?;
        l.locked = false;
        Ok(())
    }

    pub fn set_on(&mut self, name: &str, on: bool) -> CadResult<()> {
        let l = self
            .get_mut(name)
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))?;
        l.on = on;
        Ok(())
    }

    pub fn set_plot(&mut self, name: &str, plot: bool) -> CadResult<()> {
        let l = self
            .get_mut(name)
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))?;
        l.plottable = plot;
        Ok(())
    }

    pub fn linetypes(&self) -> &LinetypeTable {
        &self.linetypes
    }

    pub fn linetypes_mut(&mut self) -> &mut LinetypeTable {
        &mut self.linetypes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_system_has_layer_zero() {
        let s = LayerSystem::new();
        assert!(s.get("0").is_some());
        assert_eq!(s.current_layer(), "0");
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

    #[test]
    fn cannot_delete_layer_zero() {
        let mut s = LayerSystem::new();
        assert!(s.remove("0").is_err());
    }

    #[test]
    fn rename_layer_updates_current() {
        let mut s = LayerSystem::new();
        s.upsert(Layer::new("Walls").unwrap());
        s.set_current("Walls").unwrap();
        s.rename("Walls", "Architecture").unwrap();
        assert_eq!(s.current_layer(), "Architecture");
        assert!(s.get("Walls").is_none());
    }

    #[test]
    fn lock_blocks_editing() {
        let mut s = LayerSystem::new();
        s.upsert(Layer::new("L1").unwrap());
        s.lock("L1").unwrap();
        assert!(!s.get("L1").unwrap().is_editable());
        s.unlock("L1").unwrap();
        assert!(s.get("L1").unwrap().is_editable());
    }

    #[test]
    fn linetypes_table_present() {
        let s = LayerSystem::new();
        assert!(s.linetypes().get("DASHED").is_some());
    }
}
