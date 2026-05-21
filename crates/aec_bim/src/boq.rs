//! Bill-of-quantities ("BOQ-lite") generator.
//!
//! Pulls quantities directly from the property store and groups them by
//! IFC class + material. The grouping + column choice differs slightly
//! per region to match common cost-estimation conventions:
//!
//! * `EU` — m², m³, lm (architectural + finishes, no schedule of rates)
//! * `NA` — sq ft, cu yd, lf
//! * `APAC` — m², m³, lm + count per room
//!
//! The XLSX export writes one sheet per discipline (architecture,
//! finishes).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::classification::{ClassificationStore, IfcClass};
use crate::properties::PropertyStore;
use crate::schedules::{ScheduleColumn, ScheduleSheet};
use crate::spatial::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoqRegion {
    Eu,
    Na,
    Apac,
}

impl BoqRegion {
    pub fn area_unit(self) -> &'static str {
        match self {
            Self::Eu | Self::Apac => "m²",
            Self::Na => "sq ft",
        }
    }
    pub fn volume_unit(self) -> &'static str {
        match self {
            Self::Eu | Self::Apac => "m³",
            Self::Na => "cu yd",
        }
    }
    pub fn length_unit(self) -> &'static str {
        match self {
            Self::Eu | Self::Apac => "m",
            Self::Na => "ft",
        }
    }
    pub fn convert_area(self, m2: f64) -> f64 {
        match self {
            Self::Eu | Self::Apac => m2,
            Self::Na => m2 * 10.7639,
        }
    }
    pub fn convert_volume(self, m3: f64) -> f64 {
        match self {
            Self::Eu | Self::Apac => m3,
            Self::Na => m3 * 1.30795,
        }
    }
    pub fn convert_length(self, m: f64) -> f64 {
        match self {
            Self::Eu | Self::Apac => m,
            Self::Na => m * 3.28084,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoqLine {
    pub discipline: String,
    pub class: IfcClass,
    pub material: String,
    pub element_count: usize,
    pub area: Option<f64>,
    pub volume: Option<f64>,
    pub length: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoqReport {
    pub region: BoqRegion,
    pub lines: Vec<BoqLine>,
    /// Fraction of building elements that contributed at least one
    /// quantity (used as the "% accounted for" metric).
    pub coverage_ratio: f64,
}

pub fn boq_for_project(
    project: &Project,
    classification: &ClassificationStore,
    props: &PropertyStore,
    region: BoqRegion,
) -> BoqReport {
    let _ = project; // reserved for room-by-room breakdown in callers
    let mut by_key: BTreeMap<(String, IfcClass, String), Accum> = BTreeMap::new();
    let mut total_building_elements: usize = 0;
    let mut accounted_elements: usize = 0;
    for (id, asg) in classification.iter() {
        if !asg.class.is_building_element() {
            continue;
        }
        total_building_elements += 1;
        let discipline = discipline_for(&asg.class).to_string();
        let mat = props
            .get(id)
            .and_then(|ep| ep.get("Pset_MaterialLayerSet", "Material"))
            .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
            .or_else(|| {
                props
                    .get(id)
                    .and_then(|ep| ep.get("Pset_ElementMaterial", "Material"))
                    .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
            })
            .unwrap_or_else(|| "Unspecified".to_string());
        let area = quantity(props, id, &asg.class, QuantityKind::Area);
        let volume = quantity(props, id, &asg.class, QuantityKind::Volume);
        let length = quantity(props, id, &asg.class, QuantityKind::Length);
        if area.is_some() || volume.is_some() || length.is_some() {
            accounted_elements += 1;
        }
        let acc = by_key
            .entry((discipline, asg.class.clone(), mat))
            .or_default();
        acc.count += 1;
        if let Some(v) = area {
            *acc.area.get_or_insert(0.0) += v;
        }
        if let Some(v) = volume {
            *acc.volume.get_or_insert(0.0) += v;
        }
        if let Some(v) = length {
            *acc.length.get_or_insert(0.0) += v;
        }
    }
    let lines: Vec<BoqLine> = by_key
        .into_iter()
        .map(|((discipline, class, material), a)| BoqLine {
            discipline,
            class,
            material,
            element_count: a.count,
            area: a.area.map(|v| region.convert_area(v)),
            volume: a.volume.map(|v| region.convert_volume(v)),
            length: a.length.map(|v| region.convert_length(v)),
        })
        .collect();
    let coverage_ratio = if total_building_elements == 0 {
        1.0
    } else {
        accounted_elements as f64 / total_building_elements as f64
    };
    BoqReport {
        region,
        lines,
        coverage_ratio,
    }
}

impl BoqReport {
    /// Render as a list of schedule sheets, one per discipline.
    pub fn to_sheets(&self) -> Vec<ScheduleSheet> {
        let mut by_disc: BTreeMap<String, Vec<&BoqLine>> = BTreeMap::new();
        for line in &self.lines {
            by_disc
                .entry(line.discipline.clone())
                .or_default()
                .push(line);
        }
        let area_h = format!("Area ({})", self.region.area_unit());
        let vol_h = format!("Volume ({})", self.region.volume_unit());
        let len_h = format!("Length ({})", self.region.length_unit());
        by_disc
            .into_iter()
            .map(|(disc, lines)| {
                let cols = vec![
                    col("class", "Class"),
                    col("material", "Material"),
                    col("count", "Count"),
                    col("area", &area_h),
                    col("volume", &vol_h),
                    col("length", &len_h),
                ];
                let mut sheet = ScheduleSheet::new(disc.clone(), cols);
                for l in lines {
                    sheet.push_row(vec![
                        l.class.ifc_tag().to_string(),
                        l.material.clone(),
                        l.element_count.to_string(),
                        fmt_opt(l.area, 2),
                        fmt_opt(l.volume, 3),
                        fmt_opt(l.length, 2),
                    ]);
                }
                sheet
            })
            .collect()
    }
}

fn col(key: &str, display: &str) -> ScheduleColumn {
    ScheduleColumn {
        key: key.into(),
        display: display.into(),
    }
}

fn fmt_opt(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) => format!("{:.*}", prec, v),
        None => String::new(),
    }
}

fn discipline_for(class: &IfcClass) -> &'static str {
    match class {
        IfcClass::IfcCovering | IfcClass::IfcFurniture | IfcClass::IfcFurnishingElement => {
            "Finishes"
        }
        _ => "Architecture",
    }
}

#[derive(Debug, Default)]
struct Accum {
    count: usize,
    area: Option<f64>,
    volume: Option<f64>,
    length: Option<f64>,
}

enum QuantityKind {
    Area,
    Volume,
    Length,
}

fn quantity(
    props: &PropertyStore,
    id: &aec_core::types::EntityId,
    class: &IfcClass,
    kind: QuantityKind,
) -> Option<f64> {
    let ep = props.get(id)?;
    let candidates: &[(&str, &str)] = match (class, &kind) {
        (IfcClass::IfcWall | IfcClass::IfcWallStandardCase, QuantityKind::Area) => {
            &[("Qto_WallBaseQuantities", "NetSideArea")]
        }
        (IfcClass::IfcWall | IfcClass::IfcWallStandardCase, QuantityKind::Volume) => {
            &[("Qto_WallBaseQuantities", "NetVolume")]
        }
        (IfcClass::IfcWall | IfcClass::IfcWallStandardCase, QuantityKind::Length) => {
            &[("Qto_WallBaseQuantities", "Length")]
        }
        (IfcClass::IfcSlab | IfcClass::IfcCovering, QuantityKind::Area) => &[
            ("Qto_SlabBaseQuantities", "NetArea"),
            ("Qto_CoveringBaseQuantities", "NetArea"),
        ],
        (IfcClass::IfcSlab, QuantityKind::Volume) => &[("Qto_SlabBaseQuantities", "NetVolume")],
        (IfcClass::IfcDoor, QuantityKind::Area) => &[("Qto_DoorBaseQuantities", "Area")],
        (IfcClass::IfcWindow, QuantityKind::Area) => &[("Qto_WindowBaseQuantities", "Area")],
        (IfcClass::IfcBeam | IfcClass::IfcColumn, QuantityKind::Length) => &[
            ("Qto_BeamBaseQuantities", "Length"),
            ("Qto_ColumnBaseQuantities", "Length"),
        ],
        _ => &[],
    };
    for (pset, key) in candidates {
        if let Some(v) = ep
            .get(pset, key)
            .and_then(crate::properties::PropertyValue::as_real)
        {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::{ClassificationStore, IfcClass};
    use crate::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
    use aec_core::types::EntityId;

    fn classified(
        store: &mut ClassificationStore,
        props: &mut PropertyStore,
        class: IfcClass,
        material: &str,
        qto_name: &str,
        qto_key: &str,
        value: PropertyValue,
    ) -> EntityId {
        let id = EntityId::new();
        store.assign_manual(id.clone(), class);
        let mut layers = PropertySet::new("Pset_MaterialLayerSet");
        layers.set("Material", PropertyValue::Label(material.into()));
        let mut qto = QuantitySet::new(qto_name);
        qto.quantities.insert(qto_key.into(), value);
        let ep = props.entry(id.clone());
        ep.upsert_pset(layers);
        ep.upsert_qset(qto);
        id
    }

    #[test]
    fn rolls_up_walls_by_material() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        classified(
            &mut store,
            &mut props,
            IfcClass::IfcWall,
            "Concrete",
            "Qto_WallBaseQuantities",
            "NetSideArea",
            PropertyValue::Area(10.0),
        );
        classified(
            &mut store,
            &mut props,
            IfcClass::IfcWall,
            "Concrete",
            "Qto_WallBaseQuantities",
            "NetSideArea",
            PropertyValue::Area(15.0),
        );
        let proj = Project::new("Demo");
        let report = boq_for_project(&proj, &store, &props, BoqRegion::Eu);
        let line = report
            .lines
            .iter()
            .find(|l| l.material == "Concrete")
            .unwrap();
        assert_eq!(line.element_count, 2);
        assert!((line.area.unwrap() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn region_converts_to_imperial() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        classified(
            &mut store,
            &mut props,
            IfcClass::IfcWall,
            "Concrete",
            "Qto_WallBaseQuantities",
            "NetSideArea",
            PropertyValue::Area(1.0),
        );
        let proj = Project::new("Demo");
        let r = boq_for_project(&proj, &store, &props, BoqRegion::Na);
        let line = &r.lines[0];
        assert!((line.area.unwrap() - 10.7639).abs() < 1e-3);
    }

    #[test]
    fn coverage_tracks_unquantified_elements() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        classified(
            &mut store,
            &mut props,
            IfcClass::IfcWall,
            "Concrete",
            "Qto_WallBaseQuantities",
            "NetSideArea",
            PropertyValue::Area(10.0),
        );
        // Element with no quantities.
        let bare = EntityId::new();
        store.assign_manual(bare, IfcClass::IfcWall);
        let proj = Project::new("Demo");
        let r = boq_for_project(&proj, &store, &props, BoqRegion::Eu);
        assert!((r.coverage_ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn covers_95_percent_when_all_elements_have_quantities() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        for _ in 0..20 {
            classified(
                &mut store,
                &mut props,
                IfcClass::IfcWall,
                "Concrete",
                "Qto_WallBaseQuantities",
                "NetSideArea",
                PropertyValue::Area(1.0),
            );
        }
        let proj = Project::new("Demo");
        let r = boq_for_project(&proj, &store, &props, BoqRegion::Eu);
        assert!(r.coverage_ratio >= 0.95);
    }

    #[test]
    fn discipline_splits_into_separate_sheets() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        classified(
            &mut store,
            &mut props,
            IfcClass::IfcWall,
            "Concrete",
            "Qto_WallBaseQuantities",
            "NetSideArea",
            PropertyValue::Area(10.0),
        );
        classified(
            &mut store,
            &mut props,
            IfcClass::IfcCovering,
            "Tile",
            "Qto_CoveringBaseQuantities",
            "NetArea",
            PropertyValue::Area(20.0),
        );
        let proj = Project::new("Demo");
        let r = boq_for_project(&proj, &store, &props, BoqRegion::Eu);
        let sheets = r.to_sheets();
        let titles: Vec<&str> = sheets.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.contains(&"Architecture"));
        assert!(titles.contains(&"Finishes"));
    }
}
