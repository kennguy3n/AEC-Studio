//! IFC classification enums + per-element classification store with
//! confidence tracking and an adjustable acceptance threshold.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum IfcClass {
    IfcProject,
    IfcSite,
    IfcBuilding,
    IfcBuildingStorey,
    IfcSpace,
    IfcWall,
    IfcWallStandardCase,
    IfcSlab,
    IfcCovering,
    IfcDoor,
    IfcWindow,
    IfcColumn,
    IfcBeam,
    IfcStair,
    IfcRailing,
    IfcRoof,
    IfcCurtainWall,
    IfcFurniture,
    IfcFurnishingElement,
    IfcSanitaryTerminal,
    IfcLightFixture,
    IfcPlumbingFixture,
    IfcOpeningElement,
    /// `IfcBuildingElementProxy` — the IFC schema's first-class
    /// catch-all for "this is a building element but it doesn't
    /// fit any specific subtype". Heavily used by:
    /// * **Revit**: families that don't map cleanly to a precise IFC
    ///   class (custom families, in-place components,
    ///   non-load-bearing prismatic elements) export as
    ///   `IFCBUILDINGELEMENTPROXY`.
    /// * **ArchiCAD**: MEP / structural-system extensions and
    ///   skin-component decorations that don't have an IFC2x3/IFC4
    ///   equivalent.
    /// * **buildingSMART** exemplars and Coordination View Class
    ///   files: any "shaped placeholder" where the geometry is
    ///   accurate but the schema-level type is unknown.
    ///
    /// Promoting this to a first-class variant (rather than landing
    /// in `Other("IfcBuildingElementProxy")`) is what makes proxy
    /// elements visible in the external-IFC element capture path
    /// (`reader.rs` path (b)) — the `Other(_)` arm there is the
    /// "drop non-element STEP records" filter, so anything routed
    /// through `Other` is silently invisible to the project graph.
    /// Real proxies need to be captured the same way an `IfcWall`
    /// is.
    IfcBuildingElementProxy,
    Other(String),
}

impl IfcClass {
    pub fn ifc_tag(&self) -> &str {
        match self {
            Self::IfcProject => "IfcProject",
            Self::IfcSite => "IfcSite",
            Self::IfcBuilding => "IfcBuilding",
            Self::IfcBuildingStorey => "IfcBuildingStorey",
            Self::IfcSpace => "IfcSpace",
            Self::IfcWall => "IfcWall",
            Self::IfcWallStandardCase => "IfcWallStandardCase",
            Self::IfcSlab => "IfcSlab",
            Self::IfcCovering => "IfcCovering",
            Self::IfcDoor => "IfcDoor",
            Self::IfcWindow => "IfcWindow",
            Self::IfcColumn => "IfcColumn",
            Self::IfcBeam => "IfcBeam",
            Self::IfcStair => "IfcStair",
            Self::IfcRailing => "IfcRailing",
            Self::IfcRoof => "IfcRoof",
            Self::IfcCurtainWall => "IfcCurtainWall",
            Self::IfcFurniture => "IfcFurniture",
            Self::IfcFurnishingElement => "IfcFurnishingElement",
            Self::IfcSanitaryTerminal => "IfcSanitaryTerminal",
            Self::IfcLightFixture => "IfcLightFixture",
            Self::IfcPlumbingFixture => "IfcPlumbingFixture",
            Self::IfcOpeningElement => "IfcOpeningElement",
            Self::IfcBuildingElementProxy => "IfcBuildingElementProxy",
            Self::Other(s) => s.as_str(),
        }
    }

    /// True if this class is a spatial container (Project/Site/Building/
    /// Storey/Space) rather than a physical element.
    pub fn is_spatial(&self) -> bool {
        matches!(
            self,
            Self::IfcProject
                | Self::IfcSite
                | Self::IfcBuilding
                | Self::IfcBuildingStorey
                | Self::IfcSpace
        )
    }

    /// True if this class is a building element that physically lives
    /// inside a storey (walls, doors, windows, etc.).
    pub fn is_building_element(&self) -> bool {
        !self.is_spatial() && !matches!(self, Self::Other(_))
    }
}

/// Source of a classification assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationSource {
    Manual,
    Ai,
    Imported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationAssignment {
    pub class: IfcClass,
    /// Confidence in [0.0, 1.0]. Manual = 1.0.
    pub confidence: f64,
    pub source: ClassificationSource,
}

/// Per-element classification store with a configurable accept threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationStore {
    /// Minimum confidence to count as "accepted" in
    /// [`Self::accepted_for`]. Manual entries always count.
    pub accept_threshold: f64,
    entries: HashMap<EntityId, ClassificationAssignment>,
}

impl Default for ClassificationStore {
    fn default() -> Self {
        Self {
            accept_threshold: 0.85,
            entries: HashMap::new(),
        }
    }
}

impl ClassificationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assign_manual(&mut self, id: EntityId, class: IfcClass) {
        self.entries.insert(
            id,
            ClassificationAssignment {
                class,
                confidence: 1.0,
                source: ClassificationSource::Manual,
            },
        );
    }

    pub fn assign_ai(&mut self, id: EntityId, class: IfcClass, confidence: f64) {
        // Preserve existing manual / imported assignments — AI suggestions
        // never overwrite human-confirmed data.
        if let Some(existing) = self.entries.get(&id) {
            if matches!(
                existing.source,
                ClassificationSource::Manual | ClassificationSource::Imported
            ) {
                return;
            }
        }
        self.entries.insert(
            id,
            ClassificationAssignment {
                class,
                confidence: confidence.clamp(0.0, 1.0),
                source: ClassificationSource::Ai,
            },
        );
    }

    pub fn assign_imported(&mut self, id: EntityId, class: IfcClass) {
        self.entries.insert(
            id,
            ClassificationAssignment {
                class,
                confidence: 1.0,
                source: ClassificationSource::Imported,
            },
        );
    }

    pub fn remove(&mut self, id: &EntityId) -> Option<ClassificationAssignment> {
        self.entries.remove(id)
    }

    pub fn get(&self, id: &EntityId) -> Option<&ClassificationAssignment> {
        self.entries.get(id)
    }

    /// Return the class for an element only when it is accepted (manual
    /// or above the threshold).
    pub fn accepted_for(&self, id: &EntityId) -> Option<&IfcClass> {
        let entry = self.entries.get(id)?;
        if matches!(
            entry.source,
            ClassificationSource::Manual | ClassificationSource::Imported
        ) || entry.confidence >= self.accept_threshold
        {
            Some(&entry.class)
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&EntityId, &ClassificationAssignment)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_assignment_overrides_threshold() {
        let mut store = ClassificationStore {
            accept_threshold: 0.99,
            ..ClassificationStore::default()
        };
        let e = EntityId::new();
        store.assign_manual(e.clone(), IfcClass::IfcWall);
        assert_eq!(store.accepted_for(&e), Some(&IfcClass::IfcWall));
    }

    #[test]
    fn ai_below_threshold_is_not_accepted() {
        let mut store = ClassificationStore {
            accept_threshold: 0.85,
            ..ClassificationStore::default()
        };
        let e = EntityId::new();
        store.assign_ai(e.clone(), IfcClass::IfcDoor, 0.5);
        assert!(store.accepted_for(&e).is_none());
    }

    #[test]
    fn ai_above_threshold_is_accepted() {
        let mut store = ClassificationStore::default();
        let e = EntityId::new();
        store.assign_ai(e.clone(), IfcClass::IfcDoor, 0.95);
        assert_eq!(store.accepted_for(&e), Some(&IfcClass::IfcDoor));
    }

    #[test]
    fn ai_does_not_overwrite_manual() {
        let mut store = ClassificationStore::default();
        let e = EntityId::new();
        store.assign_manual(e.clone(), IfcClass::IfcWall);
        store.assign_ai(e.clone(), IfcClass::IfcDoor, 0.99);
        assert_eq!(store.accepted_for(&e), Some(&IfcClass::IfcWall));
    }

    #[test]
    fn confidence_is_clamped_to_unit_range() {
        let mut store = ClassificationStore::default();
        let e = EntityId::new();
        store.assign_ai(e.clone(), IfcClass::IfcWall, 5.0);
        assert!((store.get(&e).unwrap().confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn is_spatial_classification() {
        assert!(IfcClass::IfcBuilding.is_spatial());
        assert!(!IfcClass::IfcWall.is_spatial());
        assert!(IfcClass::IfcWall.is_building_element());
    }
}
