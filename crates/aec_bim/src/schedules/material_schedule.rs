//! Material schedule: aggregate quantities by material name across all
//! classified building elements.
//!
//! Strategy: look up each element's `Pset_MaterialLayerSet.Material`
//! (or `Pset_ElementMaterial.Material`) for the material name, and sum
//! `Qto_*.NetArea` (or `NetVolume` if `NetArea` is absent) per material.
//!
//! This is intentionally a single pass — projects with complex
//! multi-layer assemblies require a richer materials model that lives
//! outside the BIM core.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::classification::ClassificationStore;
use crate::properties::PropertyStore;

use super::room_schedule::fmt_opt;
use super::{ScheduleColumn, ScheduleSheet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialScheduleEntry {
    pub material: String,
    pub element_count: usize,
    pub total_area_m2: Option<f64>,
    pub total_volume_m3: Option<f64>,
    pub total_length_m: Option<f64>,
    pub supplier: String,
}

pub fn generate_material_schedule(
    classification: &ClassificationStore,
    props: &PropertyStore,
) -> (Vec<MaterialScheduleEntry>, ScheduleSheet) {
    let mut by_material: BTreeMap<String, MaterialAccum> = BTreeMap::new();
    for (id, asg) in classification.iter() {
        if !asg.class.is_building_element() {
            continue;
        }
        let Some(ep) = props.get(id) else { continue };
        let name = ep
            .get("Pset_MaterialLayerSet", "Material")
            .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
            .or_else(|| {
                ep.get("Pset_ElementMaterial", "Material")
                    .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
            });
        let Some(name) = name else { continue };
        let supplier = ep
            .get("Pset_ElementMaterial", "Supplier")
            .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
            .unwrap_or_default();
        let area = ep
            .get("Qto_WallBaseQuantities", "NetSideArea")
            .or_else(|| ep.get("Qto_SlabBaseQuantities", "NetArea"))
            .or_else(|| ep.get("Qto_CoveringBaseQuantities", "NetArea"))
            .and_then(crate::properties::PropertyValue::as_real);
        let volume = ep
            .get("Qto_WallBaseQuantities", "NetVolume")
            .or_else(|| ep.get("Qto_SlabBaseQuantities", "NetVolume"))
            .and_then(crate::properties::PropertyValue::as_real);
        let length = ep
            .get("Qto_BeamBaseQuantities", "Length")
            .or_else(|| ep.get("Qto_ColumnBaseQuantities", "Length"))
            .and_then(crate::properties::PropertyValue::as_real);
        let acc = by_material.entry(name).or_default();
        acc.count += 1;
        if let Some(a) = area {
            *acc.area.get_or_insert(0.0) += a;
        }
        if let Some(v) = volume {
            *acc.volume.get_or_insert(0.0) += v;
        }
        if let Some(l) = length {
            *acc.length.get_or_insert(0.0) += l;
        }
        if !supplier.is_empty() {
            acc.supplier = supplier;
        }
    }

    let entries: Vec<MaterialScheduleEntry> = by_material
        .into_iter()
        .map(|(name, a)| MaterialScheduleEntry {
            material: name,
            element_count: a.count,
            total_area_m2: a.area,
            total_volume_m3: a.volume,
            total_length_m: a.length,
            supplier: a.supplier,
        })
        .collect();

    let cols = vec![
        col("material", "Material"),
        col("count", "Elements"),
        col("area", "Area (m²)"),
        col("volume", "Volume (m³)"),
        col("length", "Length (m)"),
        col("supplier", "Supplier"),
    ];
    let mut sheet = ScheduleSheet::new("Material schedule", cols);
    for e in &entries {
        sheet.push_row(vec![
            e.material.clone(),
            e.element_count.to_string(),
            fmt_opt(e.total_area_m2, 2),
            fmt_opt(e.total_volume_m3, 2),
            fmt_opt(e.total_length_m, 2),
            e.supplier.clone(),
        ]);
    }
    (entries, sheet)
}

fn col(key: &str, display: &str) -> ScheduleColumn {
    ScheduleColumn {
        key: key.into(),
        display: display.into(),
    }
}

#[derive(Debug, Clone, Default)]
struct MaterialAccum {
    count: usize,
    area: Option<f64>,
    volume: Option<f64>,
    length: Option<f64>,
    supplier: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::{ClassificationStore, IfcClass};
    use crate::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
    use aec_core::types::EntityId;

    fn wall(
        store: &mut ClassificationStore,
        props: &mut PropertyStore,
        mat: &str,
        area: f64,
    ) -> EntityId {
        let id = EntityId::new();
        store.assign_manual(id.clone(), IfcClass::IfcWall);
        let mut layers = PropertySet::new("Pset_MaterialLayerSet");
        layers.set("Material", PropertyValue::Label(mat.into()));
        let mut qto = QuantitySet::new("Qto_WallBaseQuantities");
        qto.quantities
            .insert("NetSideArea".into(), PropertyValue::Area(area));
        let ep = props.entry(id.clone());
        ep.upsert_pset(layers);
        ep.upsert_qset(qto);
        id
    }

    #[test]
    fn sums_quantities_per_material() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        wall(&mut store, &mut props, "Concrete", 12.0);
        wall(&mut store, &mut props, "Concrete", 8.0);
        wall(&mut store, &mut props, "Brick", 5.0);
        let (entries, sheet) = generate_material_schedule(&store, &props);
        assert_eq!(entries.len(), 2);
        let concrete = entries.iter().find(|e| e.material == "Concrete").unwrap();
        assert_eq!(concrete.element_count, 2);
        assert!((concrete.total_area_m2.unwrap() - 20.0).abs() < 1e-9);
        assert_eq!(sheet.rows.len(), 2);
    }
}
