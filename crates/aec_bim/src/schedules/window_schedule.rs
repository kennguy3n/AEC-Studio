//! Window schedule generated from `IfcWindow` elements.

use serde::{Deserialize, Serialize};

use crate::classification::ClassificationStore;
use crate::properties::PropertyStore;

use super::room_schedule::fmt_opt;
use super::{ScheduleColumn, ScheduleSheet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowScheduleEntry {
    pub mark: String,
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
    pub glass_type: String,
    pub u_value: Option<f64>,
    pub g_value: Option<f64>,
    pub is_external: Option<bool>,
}

pub fn generate_window_schedule(
    classification: &ClassificationStore,
    props: &PropertyStore,
) -> (Vec<WindowScheduleEntry>, ScheduleSheet) {
    let mut entries = Vec::new();
    for (id, asg) in classification.iter() {
        if !matches!(&asg.class, crate::classification::IfcClass::IfcWindow) {
            continue;
        }
        let p = props.get(id);
        let pget = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key))
                .and_then(|v| v.as_text().map(str::to_string))
                .unwrap_or_default()
        };
        let pnum = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key))
                .and_then(crate::properties::PropertyValue::as_real)
        };
        let pbool = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key)).and_then(|v| match v {
                crate::properties::PropertyValue::Boolean(b) => Some(*b),
                _ => None,
            })
        };
        entries.push(WindowScheduleEntry {
            mark: pget("Pset_WindowCommon", "Reference"),
            width_mm: pnum("Qto_WindowBaseQuantities", "Width").map(|m| m * 1000.0),
            height_mm: pnum("Qto_WindowBaseQuantities", "Height").map(|m| m * 1000.0),
            glass_type: pget("Pset_DoorWindowGlazingType", "GlazingType"),
            u_value: pnum("Pset_WindowCommon", "ThermalTransmittance"),
            g_value: pnum("Pset_DoorWindowGlazingType", "SolarHeatGainTransmittance"),
            is_external: pbool("Pset_WindowCommon", "IsExternal"),
        });
    }
    entries.sort_by(|a, b| a.mark.cmp(&b.mark));

    let cols = vec![
        col("mark", "Mark"),
        col("width_mm", "Width (mm)"),
        col("height_mm", "Height (mm)"),
        col("glass", "Glass"),
        col("u_value", "U-value (W/m²K)"),
        col("g_value", "g-value"),
        col("external", "External"),
    ];
    let mut sheet = ScheduleSheet::new("Window schedule", cols);
    for e in &entries {
        sheet.push_row(vec![
            e.mark.clone(),
            fmt_opt(e.width_mm, 0),
            fmt_opt(e.height_mm, 0),
            e.glass_type.clone(),
            fmt_opt(e.u_value, 2),
            fmt_opt(e.g_value, 2),
            match e.is_external {
                Some(true) => "Yes".into(),
                Some(false) => "No".into(),
                None => String::new(),
            },
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

    #[test]
    fn generates_windows_in_mark_order() {
        let mut store = ClassificationStore::default();
        let mut props = PropertyStore::new();
        let id1 = EntityId::new();
        store.assign_manual(id1.clone(), IfcClass::IfcWindow);
        let mut pwc = PropertySet::new("Pset_WindowCommon");
        pwc.set("Reference", PropertyValue::Label("W01".into()));
        pwc.set("ThermalTransmittance", PropertyValue::Real(1.10));
        pwc.set("IsExternal", PropertyValue::Boolean(true));
        let mut qto = QuantitySet::new("Qto_WindowBaseQuantities");
        qto.quantities
            .insert("Width".into(), PropertyValue::Length(1.20));
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(1.40));
        props.entry(id1).upsert_pset(pwc);
        let id2 = EntityId::new();
        store.assign_manual(id2.clone(), IfcClass::IfcWindow);
        let mut pwc2 = PropertySet::new("Pset_WindowCommon");
        pwc2.set("Reference", PropertyValue::Label("W02".into()));
        props.entry(id2).upsert_pset(pwc2);

        let (entries, sheet) = generate_window_schedule(&store, &props);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].mark, "W01");
        assert_eq!(entries[0].is_external, Some(true));
        assert_eq!(sheet.rows[0].cells.last().unwrap(), "Yes");
    }
}
