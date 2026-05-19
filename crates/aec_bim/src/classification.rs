//! IFC classification enums. Keeping the set tight to what BIM mode actually
//! handles in Phase 2/3 — the worker can pass through additional classes
//! as `IfcClass::Other("...")` without forcing a code change here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
            Self::Other(s) => s.as_str(),
        }
    }
}
