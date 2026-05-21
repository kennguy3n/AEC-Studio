//! Room schedule generated from `IfcSpace` nodes in the project.
//!
//! Each row sources its data from the property store:
//!   * `Pset_SpaceCommon` → `Reference`, `Category`
//!   * `Qto_SpaceBaseQuantities` → `NetFloorArea`, `GrossPerimeter`,
//!     `Height`
//!   * `Pset_SpaceFinishes` (project-specific) → `FloorFinish`,
//!     `WallFinish`, `CeilingFinish`
//!
//! Missing values render as an empty string — the validator (Task 25)
//! is responsible for surfacing them as findings.

use serde::{Deserialize, Serialize};

use crate::classification::IfcClass;
use crate::properties::PropertyStore;
use crate::spatial::Project;

use super::{ScheduleColumn, ScheduleSheet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomScheduleEntry {
    pub number: String,
    pub name: String,
    pub category: String,
    pub area_m2: Option<f64>,
    pub perimeter_m: Option<f64>,
    pub height_m: Option<f64>,
    pub floor_finish: String,
    pub wall_finish: String,
    pub ceiling_finish: String,
}

pub fn generate_room_schedule(
    project: &Project,
    props: &PropertyStore,
) -> (Vec<RoomScheduleEntry>, ScheduleSheet) {
    let spaces = project.nodes_of_class(&IfcClass::IfcSpace);
    let mut entries: Vec<RoomScheduleEntry> = Vec::with_capacity(spaces.len());
    for space in spaces {
        let p = props.get(&space.id);
        let pget = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key))
                .and_then(|v| v.as_text().map(std::borrow::Cow::into_owned))
                .unwrap_or_default()
        };
        let pnum = |pset: &str, key: &str| {
            p.and_then(|e| e.get(pset, key))
                .and_then(crate::properties::PropertyValue::as_real)
        };

        entries.push(RoomScheduleEntry {
            number: pget("Pset_SpaceCommon", "Reference"),
            name: space.name.clone(),
            category: pget("Pset_SpaceCommon", "Category"),
            area_m2: pnum("Qto_SpaceBaseQuantities", "NetFloorArea"),
            perimeter_m: pnum("Qto_SpaceBaseQuantities", "GrossPerimeter"),
            height_m: pnum("Qto_SpaceBaseQuantities", "Height"),
            floor_finish: pget("Pset_SpaceFinishes", "FloorFinish"),
            wall_finish: pget("Pset_SpaceFinishes", "WallFinish"),
            ceiling_finish: pget("Pset_SpaceFinishes", "CeilingFinish"),
        });
    }
    entries.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.name.cmp(&b.name)));

    let columns = vec![
        col("number", "Number"),
        col("name", "Name"),
        col("category", "Category"),
        col("area_m2", "Area (m²)"),
        col("perimeter_m", "Perimeter (m)"),
        col("height_m", "Height (m)"),
        col("floor_finish", "Floor finish"),
        col("wall_finish", "Wall finish"),
        col("ceiling_finish", "Ceiling finish"),
    ];
    let mut sheet = ScheduleSheet::new("Room schedule", columns);
    for e in &entries {
        sheet.push_row(vec![
            e.number.clone(),
            e.name.clone(),
            e.category.clone(),
            fmt_opt(e.area_m2, 2),
            fmt_opt(e.perimeter_m, 2),
            fmt_opt(e.height_m, 2),
            e.floor_finish.clone(),
            e.wall_finish.clone(),
            e.ceiling_finish.clone(),
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

pub(crate) fn fmt_opt(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) => format!("{:.*}", prec, v),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::properties::{PropertySet, PropertyValue, QuantitySet};

    fn fixture() -> (Project, PropertyStore) {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let bldg = p
            .add_child(&site, IfcClass::IfcBuilding, "Building A")
            .unwrap();
        let storey = p
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "Storey 1")
            .unwrap();
        let s1 = p.add_child(&storey, IfcClass::IfcSpace, "Living").unwrap();
        let s2 = p.add_child(&storey, IfcClass::IfcSpace, "Bedroom").unwrap();
        let mut props = PropertyStore::new();
        let mut psc = PropertySet::new("Pset_SpaceCommon");
        psc.set("Reference", PropertyValue::Label("101".into()));
        psc.set("Category", PropertyValue::Text("Living room".into()));
        let mut qto = QuantitySet::new("Qto_SpaceBaseQuantities");
        qto.quantities
            .insert("NetFloorArea".into(), PropertyValue::Area(24.50));
        qto.quantities
            .insert("GrossPerimeter".into(), PropertyValue::Length(19.80));
        qto.quantities
            .insert("Height".into(), PropertyValue::Length(2.70));
        let entry = props.entry(s1.clone());
        entry.upsert_pset(psc);
        entry.upsert_qset(qto);

        let mut psc2 = PropertySet::new("Pset_SpaceCommon");
        psc2.set("Reference", PropertyValue::Label("102".into()));
        props.entry(s2.clone()).upsert_pset(psc2);
        (p, props)
    }

    #[test]
    fn generates_one_row_per_space() {
        let (proj, props) = fixture();
        let (entries, sheet) = generate_room_schedule(&proj, &props);
        assert_eq!(entries.len(), 2);
        assert_eq!(sheet.rows.len(), 2);
        // Sorted by Reference.
        assert_eq!(entries[0].number, "101");
        assert_eq!(entries[0].name, "Living");
        assert!((entries[0].area_m2.unwrap() - 24.50).abs() < 1e-9);
    }

    #[test]
    fn missing_quantities_render_empty() {
        let (proj, props) = fixture();
        let (entries, sheet) = generate_room_schedule(&proj, &props);
        let bedroom = entries.iter().find(|e| e.name == "Bedroom").unwrap();
        assert!(bedroom.area_m2.is_none());
        let row = sheet.rows.last().unwrap();
        assert_eq!(row.cells[3], "");
    }

    #[test]
    fn empty_project_yields_no_rows() {
        let proj = Project::new("Empty");
        let props = PropertyStore::new();
        let (entries, sheet) = generate_room_schedule(&proj, &props);
        assert!(entries.is_empty());
        assert!(sheet.rows.is_empty());
        // But the columns are still defined.
        assert_eq!(sheet.columns.len(), 9);
    }

    #[test]
    fn xlsx_export_writes_file() {
        let (proj, props) = fixture();
        let (_e, sheet) = generate_room_schedule(&proj, &props);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rooms.xlsx");
        sheet.write_xlsx(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"PK"));
    }
}
