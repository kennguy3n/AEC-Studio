//! Door schedule generated from `IfcDoor` elements across all storeys.
//!
//! Reads from:
//!   * `Pset_DoorCommon` → `Reference`, `FireRating`, `AcousticRating`,
//!     `SecurityRating`, `IsExternal`, `ThermalTransmittance`
//!   * `Qto_DoorBaseQuantities` → `Width`, `Height`, `Area`
//!   * `Pset_DoorWindowGlazingType` → `GlassLayers`, `GlassThickness`

use serde::{Deserialize, Serialize};

use crate::classification::ClassificationStore;
use crate::properties::PropertyStore;

use super::room_schedule::fmt_opt;
use super::{ScheduleColumn, ScheduleSheet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoorScheduleEntry {
    pub mark: String,
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
    pub door_type: String,
    pub fire_rating: String,
    pub acoustic_rating: String,
    pub security_rating: String,
    pub hardware: String,
}

pub fn generate_door_schedule(
    classification: &ClassificationStore,
    props: &PropertyStore,
) -> (Vec<DoorScheduleEntry>, ScheduleSheet) {
    let mut entries = Vec::new();
    for (id, asg) in classification.iter() {
        if !matches!(&asg.class, crate::classification::IfcClass::IfcDoor) {
            continue;
        }
        let p = props.get(id);
        let pget = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key))
                .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
                .unwrap_or_default()
        };
        let pnum = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key))
                .and_then(crate::properties::PropertyValue::as_real)
        };
        entries.push(DoorScheduleEntry {
            mark: pget("Pset_DoorCommon", "Reference"),
            width_mm: pnum("Qto_DoorBaseQuantities", "Width").map(|m| m * 1000.0),
            height_mm: pnum("Qto_DoorBaseQuantities", "Height").map(|m| m * 1000.0),
            door_type: pget("Pset_DoorCommon", "OperationType"),
            fire_rating: pget("Pset_DoorCommon", "FireRating"),
            acoustic_rating: pget("Pset_DoorCommon", "AcousticRating"),
            security_rating: pget("Pset_DoorCommon", "SecurityRating"),
            hardware: pget("Pset_DoorHardware", "HardwareSet"),
        });
    }
    entries.sort_by(|a, b| a.mark.cmp(&b.mark));

    let cols = vec![
        col("mark", "Mark"),
        col("width_mm", "Width (mm)"),
        col("height_mm", "Height (mm)"),
        col("type", "Type"),
        col("fire", "Fire rating"),
        col("acoustic", "Acoustic"),
        col("security", "Security"),
        col("hardware", "Hardware"),
    ];
    let mut sheet = ScheduleSheet::new("Door schedule", cols);
    for e in &entries {
        sheet.push_row(vec![
            e.mark.clone(),
            fmt_opt(e.width_mm, 0),
            fmt_opt(e.height_mm, 0),
            e.door_type.clone(),
            e.fire_rating.clone(),
            e.acoustic_rating.clone(),
            e.security_rating.clone(),
            e.hardware.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::{ClassificationStore, IfcClass};
    use crate::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
    use aec_core::types::EntityId;

    fn make_door(
        store: &mut ClassificationStore,
        props: &mut PropertyStore,
        mark: &str,
        w: f64,
        h: f64,
        fire: &str,
    ) -> EntityId {
        let id = EntityId::new();
        store.assign_manual(id.clone(), IfcClass::IfcDoor);
        let mut pdc = PropertySet::new("Pset_DoorCommon");
        pdc.set("Reference", PropertyValue::Label(mark.into()));
        pdc.set("FireRating", PropertyValue::Label(fire.into()));
        pdc.set(
            "OperationType",
            PropertyValue::Label("SingleSwingLeft".into()),
        );
        let mut qto = QuantitySet::new("Qto_DoorBaseQuantities");
        qto.quantities
            .insert("Width".into(), PropertyValue::Length(w));
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(h));
        let ep = props.entry(id.clone());
        ep.upsert_pset(pdc);
        ep.upsert_qset(qto);
        id
    }

    #[test]
    fn generates_doors_in_mark_order() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        make_door(&mut store, &mut props, "D02", 0.9, 2.1, "EI30");
        make_door(&mut store, &mut props, "D01", 1.0, 2.1, "EI60");
        let (entries, sheet) = generate_door_schedule(&store, &props);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].mark, "D01");
        assert_eq!(entries[1].mark, "D02");
        assert!((entries[0].width_mm.unwrap() - 1000.0).abs() < 1e-6);
        assert_eq!(sheet.title, "Door schedule");
        assert_eq!(sheet.rows.len(), 2);
    }

    #[test]
    fn ignores_non_door_classes() {
        let mut store = ClassificationStore::default();
        let props = PropertyStore::new();
        let id = EntityId::new();
        store.assign_manual(id, IfcClass::IfcWall);
        let (entries, sheet) = generate_door_schedule(&store, &props);
        assert!(entries.is_empty());
        assert!(sheet.rows.is_empty());
    }
}
