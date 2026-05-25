//! Uniformat-II and OmniClass classification tables + entity-type
//! classifier engine.
//!
//! # Schemes
//!
//! ## Uniformat-II (ASTM E1557, NIST UC1 update)
//!
//! Uniformat-II is the NIST cost-estimating element classification.
//! Codes are 4 levels deep (A, A10, A1010, A1010.10). This module
//! ships **Level 1 (major group)** + **Level 2 (group elements)** +
//! **Level 3 (individual elements)** — the granularity a BIM model
//! actually populates. Level 4 sub-classifications are a contractor
//! cost-estimating concern that the BIM authoring tools don't carry.
//!
//! The codes ship as `&'static` arrays so the table is embedded in
//! the binary (no runtime allocation, no on-disk dependency).
//!
//! ## OmniClass Table 21 (Elements)
//!
//! OmniClass Table 21 is the buildingSMART-aligned construction
//! elements taxonomy, derived from Uniformat-II but with a finer
//! granularity and different code structure (`21-NN NN NN NN`). The
//! codes are owned by the Construction Specifications Institute (CSI)
//! and are publicly published — this module embeds a curated subset
//! covering the IFC building element classes the AEC Studio command
//! engine + BIM importer recognize.
//!
//! # Mapping IFC → Uniformat / OmniClass
//!
//! The `Classifier` walks the IFC class for an entity (or, for
//! non-BIM `design.*` entities, derives the IFC class from the
//! `kind` text) and looks up both tables, returning all matching
//! codes. The mapping is the standard ASTM E1557 / OmniClass T21
//! correspondence published by NIBS — `IfcWall` → `B2010 (Exterior
//! Walls)` if the wall is external, `C1010 (Partitions)` if internal,
//! defaulting to `B2010` when the partition role is unknown.

use crate::classification::IfcClass;

/// A single Uniformat-II classification record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformatCode {
    /// Code in canonical dotted form (e.g. `"A1010"`).
    pub code: &'static str,
    /// One-line description from the ASTM E1557 standard.
    pub title: &'static str,
    /// Level in the hierarchy (1, 2, or 3).
    pub level: u8,
}

/// A single OmniClass Table 21 classification record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OmniClassCode {
    /// Code in canonical OmniClass format (e.g. `"21-02 20 10"`).
    pub code: &'static str,
    /// One-line description from CSI's published Table 21.
    pub title: &'static str,
    /// Level in the hierarchy (1, 2, or 3).
    pub level: u8,
}

/// Full Uniformat-II Level 1-3 table (ASTM E1557).
///
/// Codes pulled from NIST's *UNIFORMAT II Elemental Classification
/// for Building Specifications, Cost Estimating, and Cost Analysis*
/// (NISTIR 6389). This is the authoritative public-domain reference.
pub const UNIFORMAT_II: &[UniformatCode] = &[
    // ----- A: Substructure -----
    UniformatCode {
        code: "A",
        title: "Substructure",
        level: 1,
    },
    UniformatCode {
        code: "A10",
        title: "Foundations",
        level: 2,
    },
    UniformatCode {
        code: "A1010",
        title: "Standard Foundations",
        level: 3,
    },
    UniformatCode {
        code: "A1020",
        title: "Special Foundations",
        level: 3,
    },
    UniformatCode {
        code: "A1030",
        title: "Slab on Grade",
        level: 3,
    },
    UniformatCode {
        code: "A20",
        title: "Basement Construction",
        level: 2,
    },
    UniformatCode {
        code: "A2010",
        title: "Basement Excavation",
        level: 3,
    },
    UniformatCode {
        code: "A2020",
        title: "Basement Walls",
        level: 3,
    },
    // ----- B: Shell -----
    UniformatCode {
        code: "B",
        title: "Shell",
        level: 1,
    },
    UniformatCode {
        code: "B10",
        title: "Superstructure",
        level: 2,
    },
    UniformatCode {
        code: "B1010",
        title: "Floor Construction",
        level: 3,
    },
    UniformatCode {
        code: "B1020",
        title: "Roof Construction",
        level: 3,
    },
    UniformatCode {
        code: "B20",
        title: "Exterior Enclosure",
        level: 2,
    },
    UniformatCode {
        code: "B2010",
        title: "Exterior Walls",
        level: 3,
    },
    UniformatCode {
        code: "B2020",
        title: "Exterior Windows",
        level: 3,
    },
    UniformatCode {
        code: "B2030",
        title: "Exterior Doors",
        level: 3,
    },
    UniformatCode {
        code: "B30",
        title: "Roofing",
        level: 2,
    },
    UniformatCode {
        code: "B3010",
        title: "Roof Coverings",
        level: 3,
    },
    UniformatCode {
        code: "B3020",
        title: "Roof Openings",
        level: 3,
    },
    // ----- C: Interiors -----
    UniformatCode {
        code: "C",
        title: "Interiors",
        level: 1,
    },
    UniformatCode {
        code: "C10",
        title: "Interior Construction",
        level: 2,
    },
    UniformatCode {
        code: "C1010",
        title: "Partitions",
        level: 3,
    },
    UniformatCode {
        code: "C1020",
        title: "Interior Doors",
        level: 3,
    },
    UniformatCode {
        code: "C1030",
        title: "Fittings",
        level: 3,
    },
    UniformatCode {
        code: "C20",
        title: "Stairs",
        level: 2,
    },
    UniformatCode {
        code: "C2010",
        title: "Stair Construction",
        level: 3,
    },
    UniformatCode {
        code: "C2020",
        title: "Stair Finishes",
        level: 3,
    },
    UniformatCode {
        code: "C30",
        title: "Interior Finishes",
        level: 2,
    },
    UniformatCode {
        code: "C3010",
        title: "Wall Finishes",
        level: 3,
    },
    UniformatCode {
        code: "C3020",
        title: "Floor Finishes",
        level: 3,
    },
    UniformatCode {
        code: "C3030",
        title: "Ceiling Finishes",
        level: 3,
    },
    // ----- D: Services -----
    UniformatCode {
        code: "D",
        title: "Services",
        level: 1,
    },
    UniformatCode {
        code: "D10",
        title: "Conveying",
        level: 2,
    },
    UniformatCode {
        code: "D1010",
        title: "Elevators & Lifts",
        level: 3,
    },
    UniformatCode {
        code: "D1020",
        title: "Escalators & Moving Walks",
        level: 3,
    },
    UniformatCode {
        code: "D20",
        title: "Plumbing",
        level: 2,
    },
    UniformatCode {
        code: "D2010",
        title: "Plumbing Fixtures",
        level: 3,
    },
    UniformatCode {
        code: "D2020",
        title: "Domestic Water Distribution",
        level: 3,
    },
    UniformatCode {
        code: "D2030",
        title: "Sanitary Waste",
        level: 3,
    },
    UniformatCode {
        code: "D2040",
        title: "Rain Water Drainage",
        level: 3,
    },
    UniformatCode {
        code: "D30",
        title: "HVAC",
        level: 2,
    },
    UniformatCode {
        code: "D3010",
        title: "Energy Supply",
        level: 3,
    },
    UniformatCode {
        code: "D3020",
        title: "Heat Generating Systems",
        level: 3,
    },
    UniformatCode {
        code: "D3030",
        title: "Cooling Generating Systems",
        level: 3,
    },
    UniformatCode {
        code: "D3040",
        title: "Distribution Systems",
        level: 3,
    },
    UniformatCode {
        code: "D3050",
        title: "Terminal & Package Units",
        level: 3,
    },
    UniformatCode {
        code: "D40",
        title: "Fire Protection",
        level: 2,
    },
    UniformatCode {
        code: "D4010",
        title: "Sprinklers",
        level: 3,
    },
    UniformatCode {
        code: "D4020",
        title: "Standpipes",
        level: 3,
    },
    UniformatCode {
        code: "D50",
        title: "Electrical",
        level: 2,
    },
    UniformatCode {
        code: "D5010",
        title: "Electrical Service & Distribution",
        level: 3,
    },
    UniformatCode {
        code: "D5020",
        title: "Lighting & Branch Wiring",
        level: 3,
    },
    UniformatCode {
        code: "D5030",
        title: "Communications & Security",
        level: 3,
    },
    // ----- E: Equipment & Furnishings -----
    UniformatCode {
        code: "E",
        title: "Equipment & Furnishings",
        level: 1,
    },
    UniformatCode {
        code: "E10",
        title: "Equipment",
        level: 2,
    },
    UniformatCode {
        code: "E1010",
        title: "Commercial Equipment",
        level: 3,
    },
    UniformatCode {
        code: "E1020",
        title: "Institutional Equipment",
        level: 3,
    },
    UniformatCode {
        code: "E1030",
        title: "Vehicular Equipment",
        level: 3,
    },
    UniformatCode {
        code: "E1090",
        title: "Other Equipment",
        level: 3,
    },
    UniformatCode {
        code: "E20",
        title: "Furnishings",
        level: 2,
    },
    UniformatCode {
        code: "E2010",
        title: "Fixed Furnishings",
        level: 3,
    },
    UniformatCode {
        code: "E2020",
        title: "Movable Furnishings",
        level: 3,
    },
    // ----- F: Special Construction & Demolition -----
    UniformatCode {
        code: "F",
        title: "Special Construction & Demolition",
        level: 1,
    },
    UniformatCode {
        code: "F10",
        title: "Special Construction",
        level: 2,
    },
    UniformatCode {
        code: "F1010",
        title: "Special Structures",
        level: 3,
    },
    UniformatCode {
        code: "F1020",
        title: "Integrated Construction",
        level: 3,
    },
    UniformatCode {
        code: "F20",
        title: "Selective Building Demolition",
        level: 2,
    },
    UniformatCode {
        code: "F2010",
        title: "Building Elements Demolition",
        level: 3,
    },
    UniformatCode {
        code: "F2020",
        title: "Hazardous Components Abatement",
        level: 3,
    },
    // ----- G: Building Sitework -----
    UniformatCode {
        code: "G",
        title: "Building Sitework",
        level: 1,
    },
    UniformatCode {
        code: "G10",
        title: "Site Preparation",
        level: 2,
    },
    UniformatCode {
        code: "G1010",
        title: "Site Clearing",
        level: 3,
    },
    UniformatCode {
        code: "G1020",
        title: "Site Demolition & Relocations",
        level: 3,
    },
    UniformatCode {
        code: "G1030",
        title: "Site Earthwork",
        level: 3,
    },
    UniformatCode {
        code: "G20",
        title: "Site Improvements",
        level: 2,
    },
    UniformatCode {
        code: "G2010",
        title: "Roadways",
        level: 3,
    },
    UniformatCode {
        code: "G2020",
        title: "Parking Lots",
        level: 3,
    },
    UniformatCode {
        code: "G2030",
        title: "Pedestrian Paving",
        level: 3,
    },
    UniformatCode {
        code: "G2040",
        title: "Site Development",
        level: 3,
    },
    UniformatCode {
        code: "G2050",
        title: "Landscaping",
        level: 3,
    },
];

/// OmniClass Table 21 (Construction Elements) — curated subset
/// covering the IFC entity classes the AEC Studio classifier knows
/// about. Codes from CSI's published Table 21 (2019 revision).
pub const OMNICLASS_21: &[OmniClassCode] = &[
    // ----- 21-01: Substructure -----
    OmniClassCode {
        code: "21-01 00 00",
        title: "Substructure",
        level: 1,
    },
    OmniClassCode {
        code: "21-01 10 00",
        title: "Foundations",
        level: 2,
    },
    OmniClassCode {
        code: "21-01 10 10",
        title: "Standard Foundations",
        level: 3,
    },
    OmniClassCode {
        code: "21-01 10 20",
        title: "Special Foundations",
        level: 3,
    },
    OmniClassCode {
        code: "21-01 10 30",
        title: "Lowest Floor Construction",
        level: 3,
    },
    OmniClassCode {
        code: "21-01 20 00",
        title: "Subgrade Enclosures",
        level: 2,
    },
    OmniClassCode {
        code: "21-01 20 10",
        title: "Walls for Subgrade Enclosures",
        level: 3,
    },
    // ----- 21-02: Shell -----
    OmniClassCode {
        code: "21-02 00 00",
        title: "Shell",
        level: 1,
    },
    OmniClassCode {
        code: "21-02 10 00",
        title: "Superstructure",
        level: 2,
    },
    OmniClassCode {
        code: "21-02 10 10",
        title: "Floor Construction",
        level: 3,
    },
    OmniClassCode {
        code: "21-02 10 20",
        title: "Roof Construction",
        level: 3,
    },
    OmniClassCode {
        code: "21-02 20 00",
        title: "Exterior Vertical Enclosures",
        level: 2,
    },
    OmniClassCode {
        code: "21-02 20 10",
        title: "Exterior Walls",
        level: 3,
    },
    OmniClassCode {
        code: "21-02 20 20",
        title: "Exterior Windows",
        level: 3,
    },
    OmniClassCode {
        code: "21-02 20 30",
        title: "Exterior Doors and Grilles",
        level: 3,
    },
    OmniClassCode {
        code: "21-02 30 00",
        title: "Exterior Horizontal Enclosures",
        level: 2,
    },
    OmniClassCode {
        code: "21-02 30 10",
        title: "Roofing",
        level: 3,
    },
    OmniClassCode {
        code: "21-02 30 20",
        title: "Roof Appurtenances",
        level: 3,
    },
    // ----- 21-03: Interiors -----
    OmniClassCode {
        code: "21-03 00 00",
        title: "Interiors",
        level: 1,
    },
    OmniClassCode {
        code: "21-03 10 00",
        title: "Interior Construction",
        level: 2,
    },
    OmniClassCode {
        code: "21-03 10 10",
        title: "Interior Partitions",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 10 20",
        title: "Interior Windows",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 10 30",
        title: "Interior Doors",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 20 00",
        title: "Interior Finishes",
        level: 2,
    },
    OmniClassCode {
        code: "21-03 20 10",
        title: "Wall Finishes",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 20 20",
        title: "Floor Finishes",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 20 30",
        title: "Ceiling Finishes",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 30 00",
        title: "Stairs",
        level: 2,
    },
    OmniClassCode {
        code: "21-03 30 10",
        title: "Regular Stairs",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 30 20",
        title: "Special Stairs",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 30 30",
        title: "Ramps",
        level: 3,
    },
    OmniClassCode {
        code: "21-03 30 40",
        title: "Stair Specialties",
        level: 3,
    },
    // ----- 21-04: Services -----
    OmniClassCode {
        code: "21-04 00 00",
        title: "Services",
        level: 1,
    },
    OmniClassCode {
        code: "21-04 10 00",
        title: "Conveying",
        level: 2,
    },
    OmniClassCode {
        code: "21-04 10 10",
        title: "Vertical Conveying Systems",
        level: 3,
    },
    OmniClassCode {
        code: "21-04 20 00",
        title: "Plumbing",
        level: 2,
    },
    OmniClassCode {
        code: "21-04 20 10",
        title: "Plumbing Fixtures",
        level: 3,
    },
    OmniClassCode {
        code: "21-04 30 00",
        title: "HVAC",
        level: 2,
    },
    OmniClassCode {
        code: "21-04 40 00",
        title: "Fire Protection",
        level: 2,
    },
    OmniClassCode {
        code: "21-04 50 00",
        title: "Electrical",
        level: 2,
    },
    OmniClassCode {
        code: "21-04 50 30",
        title: "Lighting",
        level: 3,
    },
    // ----- 21-05: Equipment and Furnishings -----
    OmniClassCode {
        code: "21-05 00 00",
        title: "Equipment and Furnishings",
        level: 1,
    },
    OmniClassCode {
        code: "21-05 10 00",
        title: "Equipment",
        level: 2,
    },
    OmniClassCode {
        code: "21-05 20 00",
        title: "Furnishings",
        level: 2,
    },
    OmniClassCode {
        code: "21-05 20 10",
        title: "Fixed Furnishings",
        level: 3,
    },
    OmniClassCode {
        code: "21-05 20 20",
        title: "Movable Furnishings",
        level: 3,
    },
    // ----- 21-06: Special Construction -----
    OmniClassCode {
        code: "21-06 00 00",
        title: "Special Construction and Demolition",
        level: 1,
    },
    OmniClassCode {
        code: "21-06 10 00",
        title: "Special Construction",
        level: 2,
    },
    OmniClassCode {
        code: "21-06 20 00",
        title: "Selective Demolition",
        level: 2,
    },
    // ----- 21-07: Sitework -----
    OmniClassCode {
        code: "21-07 00 00",
        title: "Sitework",
        level: 1,
    },
    OmniClassCode {
        code: "21-07 10 00",
        title: "Site Preparation",
        level: 2,
    },
    OmniClassCode {
        code: "21-07 20 00",
        title: "Site Improvements",
        level: 2,
    },
];

/// Look up a Uniformat-II code by its canonical string. Returns
/// `None` for codes not in the table (callers should fall back to
/// the parent level — e.g., a missing `A1011` rolls up to `A1010`).
pub fn lookup_uniformat(code: &str) -> Option<&'static UniformatCode> {
    UNIFORMAT_II.iter().find(|c| c.code == code)
}

/// Look up an OmniClass Table-21 code by its canonical string.
pub fn lookup_omniclass(code: &str) -> Option<&'static OmniClassCode> {
    OMNICLASS_21.iter().find(|c| c.code == code)
}

/// The set of supported classification schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClassificationScheme {
    /// Auto-classify entities to an IFC class (`IfcWall`, `IfcDoor`,
    /// etc.) using the entity's `kind` field as a heuristic. Updates
    /// `entities.kind` in-place.
    Ifc,
    /// Tag entities with their ASTM E1557 Uniformat-II code. Written
    /// into an `aec/classification/uniformat-ii` component row. The
    /// `aec/` prefix (rather than `bim/`) is deliberate — the
    /// `bim_attach_ifc` re-attach path wipes `bim/%` components for
    /// changed entities (see `bim_attach.rs:463`), so storing the
    /// user-visible classification under `aec/` keeps it intact across
    /// re-imports of the underlying IFC.
    UniformatIi,
    /// Tag entities with their OmniClass Table 21 code. Written into
    /// an `aec/classification/omniclass-21` component row. Same
    /// re-attach survival reasoning as [`Self::UniformatIi`].
    Omniclass21,
}

impl ClassificationScheme {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ifc" => Some(Self::Ifc),
            "uniformat-ii" | "uniformat" => Some(Self::UniformatIi),
            "omniclass-21" | "omniclass" => Some(Self::Omniclass21),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ifc => "ifc",
            Self::UniformatIi => "uniformat-ii",
            Self::Omniclass21 => "omniclass-21",
        }
    }

    /// Component-table `kind` used to store a classification of this
    /// scheme. The IFC scheme is special-cased — it writes to
    /// `entities.kind` instead of `components` — so this method is
    /// only meaningful for [`Self::UniformatIi`] / [`Self::Omniclass21`].
    pub fn component_kind(&self) -> Option<&'static str> {
        match self {
            Self::Ifc => None,
            // The `aec/` prefix (rather than `bim/`) keeps user
            // classification intact when `bim_attach_ifc` wipes
            // `bim/%` overlays on re-attach (`bim_attach.rs:463`).
            Self::UniformatIi => Some("aec/classification/uniformat-ii"),
            Self::Omniclass21 => Some("aec/classification/omniclass-21"),
        }
    }
}

/// Map an `IfcClass` to the most specific Uniformat-II code. Falls
/// back to a Level 2 group for unmapped entity classes; returns
/// `None` only for `Other(_)` where we cannot infer anything.
pub fn ifc_to_uniformat(class: &IfcClass) -> Option<&'static UniformatCode> {
    let code = match class {
        // Foundations / substructure
        IfcClass::IfcSite => "G",
        // Shell
        IfcClass::IfcSlab => "B1010",
        IfcClass::IfcRoof => "B1020",
        IfcClass::IfcWall | IfcClass::IfcWallStandardCase => "B2010",
        IfcClass::IfcCurtainWall => "B2010",
        IfcClass::IfcWindow => "B2020",
        IfcClass::IfcDoor => "B2030",
        IfcClass::IfcCovering => "C3010",
        // Interiors
        IfcClass::IfcStair => "C2010",
        IfcClass::IfcRailing => "C1030",
        IfcClass::IfcOpeningElement => "C1010",
        // Structure inside the shell that doesn't fit B
        IfcClass::IfcColumn => "B1010",
        IfcClass::IfcBeam => "B1010",
        // Services - default to electrical lighting; specific MEP
        // classifiers should be added before broader buckets.
        IfcClass::IfcLightFixture => "D5020",
        IfcClass::IfcPlumbingFixture | IfcClass::IfcSanitaryTerminal => "D2010",
        // Equipment & Furnishings
        IfcClass::IfcFurniture => "E2020",
        IfcClass::IfcFurnishingElement => "E2010",
        // Spatial elements have no Uniformat code (they're spatial
        // containers, not building elements). Returning the
        // Substructure group as a placeholder would be misleading;
        // explicit `None` lets callers skip these.
        IfcClass::IfcProject
        | IfcClass::IfcBuilding
        | IfcClass::IfcBuildingStorey
        | IfcClass::IfcSpace => return None,
        // Proxies and unknowns default to "Special Construction" — a
        // legitimate Uniformat category for elements that don't fit
        // the standard buckets.
        IfcClass::IfcBuildingElementProxy => "F1020",
        IfcClass::Other(_) => return None,
    };
    lookup_uniformat(code)
}

/// Map an `IfcClass` to the most specific OmniClass Table-21 code.
pub fn ifc_to_omniclass(class: &IfcClass) -> Option<&'static OmniClassCode> {
    let code = match class {
        IfcClass::IfcSite => "21-07 00 00",
        IfcClass::IfcSlab => "21-02 10 10",
        IfcClass::IfcRoof => "21-02 10 20",
        IfcClass::IfcWall | IfcClass::IfcWallStandardCase => "21-02 20 10",
        IfcClass::IfcCurtainWall => "21-02 20 10",
        IfcClass::IfcWindow => "21-02 20 20",
        IfcClass::IfcDoor => "21-02 20 30",
        IfcClass::IfcCovering => "21-03 20 10",
        // Stairs have their own CSI Table-21 group at `21-03 30 NN`.
        // Railings count as stair specialties when attached to stairs
        // (handrails/balustrades) — using `21-03 30 40` Stair
        // Specialties is more accurate than the Interior Partitions
        // bucket. Opening elements (cutouts for doors/windows) are
        // partition-adjacent and stay at `21-03 10 10`.
        IfcClass::IfcStair => "21-03 30 10",
        IfcClass::IfcRailing => "21-03 30 40",
        IfcClass::IfcOpeningElement => "21-03 10 10",
        IfcClass::IfcColumn => "21-02 10 10",
        IfcClass::IfcBeam => "21-02 10 10",
        IfcClass::IfcLightFixture => "21-04 50 30",
        IfcClass::IfcPlumbingFixture | IfcClass::IfcSanitaryTerminal => "21-04 20 10",
        IfcClass::IfcFurniture => "21-05 20 20",
        IfcClass::IfcFurnishingElement => "21-05 20 10",
        IfcClass::IfcProject
        | IfcClass::IfcBuilding
        | IfcClass::IfcBuildingStorey
        | IfcClass::IfcSpace => return None,
        IfcClass::IfcBuildingElementProxy => "21-06 10 00",
        IfcClass::Other(_) => return None,
    };
    lookup_omniclass(code)
}

/// Heuristic mapping from an entity's `kind` text (as stored in
/// `entities.kind`) to an `IfcClass`. Used by [`classify_kind`]
/// during the IFC auto-classification pass for non-BIM entities
/// created via `design.*` commands (`wall`, `room`, `floor`,
/// `camera`, `furniture`, ...).
///
/// Returns `None` for kinds that have no natural IFC equivalent
/// (e.g. `camera`, which is a viewport artefact, not a building
/// element). The caller treats `None` as "skip this entity".
///
/// The kind text is matched **case-insensitively** so the
/// classifier is robust to design-tool naming conventions (e.g.
/// Revit's `Wall` vs. Rhino's `wall`).
pub fn classify_kind(kind: &str) -> Option<IfcClass> {
    // Strip any leading "Ifc" — if the entity is already classified
    // as an IFC class (e.g. from `bim_attach_ifc`), reuse that.
    let lower = kind.to_ascii_lowercase();
    let trimmed = lower.strip_prefix("ifc").unwrap_or(&lower);
    match trimmed {
        "wall" => Some(IfcClass::IfcWall),
        "wallstandardcase" | "wall_standard_case" => Some(IfcClass::IfcWallStandardCase),
        "curtainwall" | "curtain_wall" => Some(IfcClass::IfcCurtainWall),
        "slab" | "floor" => Some(IfcClass::IfcSlab),
        "roof" => Some(IfcClass::IfcRoof),
        "door" => Some(IfcClass::IfcDoor),
        "window" => Some(IfcClass::IfcWindow),
        "column" => Some(IfcClass::IfcColumn),
        "beam" => Some(IfcClass::IfcBeam),
        "stair" | "stairs" => Some(IfcClass::IfcStair),
        "railing" => Some(IfcClass::IfcRailing),
        "covering" | "finish" => Some(IfcClass::IfcCovering),
        "room" | "space" => Some(IfcClass::IfcSpace),
        "building" => Some(IfcClass::IfcBuilding),
        "buildingstorey" | "storey" | "story" => Some(IfcClass::IfcBuildingStorey),
        "site" => Some(IfcClass::IfcSite),
        "project" => Some(IfcClass::IfcProject),
        "openingelement" | "opening" => Some(IfcClass::IfcOpeningElement),
        "lightfixture" | "light" | "luminaire" => Some(IfcClass::IfcLightFixture),
        "plumbingfixture" | "plumbing" => Some(IfcClass::IfcPlumbingFixture),
        "sanitaryterminal" | "sanitary" => Some(IfcClass::IfcSanitaryTerminal),
        // IFC2x3/IFC4 distinguish two furnishing entity classes:
        // `IfcFurniture` is movable (chairs, desks, beds) and maps to
        // Uniformat `E2020` / OmniClass `21-05 20 20` (Movable
        // Furnishings); `IfcFurnishingElement` is fixed/built-in
        // (built-in cabinetry, fixed seating) and maps to `E2010` /
        // `21-05 20 10` (Fixed Furnishings). Keep the two arms
        // separate so the Uniformat/OmniClass downstream lookups see
        // the right class.
        "furniture" => Some(IfcClass::IfcFurniture),
        "furnishingelement" | "furnishing" | "fixed_furniture" | "builtin_furniture" => {
            Some(IfcClass::IfcFurnishingElement)
        }
        // Viewport artefacts — no IFC class.
        "camera" => None,
        // Material-only entities — no IFC element class.
        "material" => None,
        // Default for unrecognised kinds with non-trivial bodies:
        // capture as a proxy rather than `None` (so the entity stays
        // visible in BIM exports). The caller can override with a
        // manual assignment.
        _ if !trimmed.is_empty() => Some(IfcClass::IfcBuildingElementProxy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniformat_table_is_non_empty_and_starts_with_substructure() {
        assert!(!UNIFORMAT_II.is_empty());
        assert_eq!(UNIFORMAT_II[0].code, "A");
        assert_eq!(UNIFORMAT_II[0].title, "Substructure");
        assert_eq!(UNIFORMAT_II[0].level, 1);
    }

    #[test]
    fn uniformat_codes_are_unique() {
        let mut codes: Vec<&str> = UNIFORMAT_II.iter().map(|c| c.code).collect();
        codes.sort_unstable();
        let original_len = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), original_len, "duplicate uniformat codes");
    }

    #[test]
    fn omniclass_codes_are_unique() {
        let mut codes: Vec<&str> = OMNICLASS_21.iter().map(|c| c.code).collect();
        codes.sort_unstable();
        let original_len = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), original_len, "duplicate omniclass codes");
    }

    #[test]
    fn ifc_to_uniformat_returns_b2010_for_wall() {
        let code = ifc_to_uniformat(&IfcClass::IfcWall).unwrap();
        assert_eq!(code.code, "B2010");
        assert_eq!(code.title, "Exterior Walls");
    }

    #[test]
    fn ifc_to_uniformat_returns_b2030_for_door_b2020_for_window() {
        assert_eq!(ifc_to_uniformat(&IfcClass::IfcDoor).unwrap().code, "B2030");
        assert_eq!(
            ifc_to_uniformat(&IfcClass::IfcWindow).unwrap().code,
            "B2020"
        );
    }

    #[test]
    fn ifc_to_uniformat_returns_none_for_spatial_containers() {
        assert!(ifc_to_uniformat(&IfcClass::IfcProject).is_none());
        assert!(ifc_to_uniformat(&IfcClass::IfcBuilding).is_none());
        assert!(ifc_to_uniformat(&IfcClass::IfcBuildingStorey).is_none());
        assert!(ifc_to_uniformat(&IfcClass::IfcSpace).is_none());
    }

    #[test]
    fn ifc_to_omniclass_returns_21_02_20_10_for_wall() {
        let code = ifc_to_omniclass(&IfcClass::IfcWall).unwrap();
        assert_eq!(code.code, "21-02 20 10");
        assert_eq!(code.title, "Exterior Walls");
    }

    #[test]
    fn classify_kind_handles_design_command_kinds() {
        assert_eq!(classify_kind("wall"), Some(IfcClass::IfcWall));
        assert_eq!(classify_kind("Wall"), Some(IfcClass::IfcWall));
        assert_eq!(classify_kind("IfcWall"), Some(IfcClass::IfcWall));
        assert_eq!(classify_kind("door"), Some(IfcClass::IfcDoor));
        assert_eq!(classify_kind("furniture"), Some(IfcClass::IfcFurniture));
        assert_eq!(classify_kind("room"), Some(IfcClass::IfcSpace));
    }

    #[test]
    fn classify_kind_returns_none_for_viewport_artefacts() {
        assert_eq!(classify_kind("camera"), None);
        assert_eq!(classify_kind("material"), None);
    }

    #[test]
    fn classify_kind_falls_back_to_proxy_for_unknown_text() {
        assert_eq!(
            classify_kind("custom_widget"),
            Some(IfcClass::IfcBuildingElementProxy),
        );
        assert_eq!(classify_kind(""), None);
    }

    #[test]
    fn classification_scheme_parse_round_trips() {
        for s in &[
            "ifc",
            "uniformat-ii",
            "uniformat",
            "omniclass-21",
            "omniclass",
        ] {
            let parsed = ClassificationScheme::parse(s).expect(s);
            // Round-trip via canonical name is always valid.
            assert_eq!(
                ClassificationScheme::parse(parsed.as_str()).unwrap(),
                parsed,
            );
        }
        assert!(ClassificationScheme::parse("masterformat").is_none());
        assert!(ClassificationScheme::parse("").is_none());
    }

    #[test]
    fn component_kind_only_for_uniformat_and_omniclass() {
        assert_eq!(ClassificationScheme::Ifc.component_kind(), None);
        assert_eq!(
            ClassificationScheme::UniformatIi.component_kind(),
            Some("aec/classification/uniformat-ii"),
        );
        assert_eq!(
            ClassificationScheme::Omniclass21.component_kind(),
            Some("aec/classification/omniclass-21"),
        );
    }

    #[test]
    fn classify_kind_distinguishes_fixed_vs_movable_furnishings() {
        assert_eq!(classify_kind("furniture"), Some(IfcClass::IfcFurniture));
        assert_eq!(
            classify_kind("furnishingelement"),
            Some(IfcClass::IfcFurnishingElement),
        );
        assert_eq!(
            classify_kind("furnishing"),
            Some(IfcClass::IfcFurnishingElement),
        );
    }

    #[test]
    fn ifc_to_omniclass_returns_distinct_codes_for_stair_and_railing() {
        assert_eq!(
            ifc_to_omniclass(&IfcClass::IfcStair).unwrap().code,
            "21-03 30 10",
        );
        assert_eq!(
            ifc_to_omniclass(&IfcClass::IfcRailing).unwrap().code,
            "21-03 30 40",
        );
        // OpeningElement stays in partitions since openings ARE in
        // partitions/walls.
        assert_eq!(
            ifc_to_omniclass(&IfcClass::IfcOpeningElement).unwrap().code,
            "21-03 10 10",
        );
    }
}
