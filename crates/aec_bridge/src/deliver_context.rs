//! Build a real `DeliverPackContext` from an open project package.
//!
//! Phase 14 Group A Task 1: the renderer's `deliverBuildPack` and
//! `exportBuildProposalPack` now pass the active project's path
//! through to the bridge so the pack can carry actual project content
//! — renders from `<project>/renders/`, an XLSX material schedule
//! built from the project's wall / floor / ceiling materials, a BOQ
//! workbook with door/window counts and element areas, the sheet
//! definitions + their primitives serialised as a real PDF, an IFC
//! string serialised from the project graph, and a floor-plan SVG
//! rendered from the first sheet. When `project_path` is `None` the
//! caller still gets the previous `DeliverPackContext::default()`
//! shape so existing callers keep working.
//!
//! The struct returned by [`build_for_project`] owns every byte buffer
//! and `ScheduleSheet` so a `DeliverPackContext` borrowed from it
//! lives only as long as the owner — see [`BuiltDeliverContext::as_ctx`].

use std::path::{Path, PathBuf};

use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::ifc::writer::IfcWriter;
use aec_bim::materials::MaterialStore;
use aec_bim::properties::PropertyStore;
use aec_bim::schedules::{ScheduleColumn, ScheduleSheet};
use aec_bim::Project as BimProject;
use aec_cad::dxf::convert::primitive_to_dxf;
use aec_cad::dxf::DxfEntity;
use aec_cad::primitives::Primitive;
use aec_cad::sheets::Sheet;
use aec_command::commands::ProjectGraph;
use aec_core::package::ProjectPackage;
use aec_core::types::EntityId;
use aec_export::svg_export::{
    render_sheet_svg_full, BlockTable, DimStyleTable, LayerTable, SvgExportOptions,
};
use aec_export::DeliverPackContext;

use crate::service::BridgeServiceError;

/// Owner of every borrowed slice / string in a [`DeliverPackContext`]
/// built from a real project. Hand a reference to [`Self::as_ctx`] to
/// `write_deliver_pack_with_context` / `write_proposal_pack_with_context`.
pub struct BuiltDeliverContext {
    renders_dir: PathBuf,
    material_schedule: ScheduleSheet,
    boq_schedule: ScheduleSheet,
    sheets: Vec<(Sheet, Vec<DxfEntity>)>,
    ifc_string: String,
    floor_plan_svg: Option<String>,
    room_count: usize,
    material_count: usize,
    template_name: Option<String>,
}

impl BuiltDeliverContext {
    /// Borrow as a [`DeliverPackContext`] suitable for
    /// `write_deliver_pack_with_context` /
    /// `write_proposal_pack_with_context`.
    pub fn as_ctx(&self) -> DeliverPackContext<'_> {
        DeliverPackContext {
            renders_dir: Some(self.renders_dir.as_path()),
            material_schedule: Some(&self.material_schedule),
            boq_schedule: Some(&self.boq_schedule),
            sheets: Some(self.sheets.as_slice()),
            ifc_string: Some(self.ifc_string.as_str()),
            floor_plan_svg: self.floor_plan_svg.as_deref(),
            room_count: Some(self.room_count),
            material_count: Some(self.material_count),
            template_name: self.template_name.as_deref(),
        }
    }
}

/// Build a [`BuiltDeliverContext`] by reading the SQLCipher database
/// at `project_path` and walking its entity graph.
///
/// * `renders_dir` is `<project_path>/renders/` (which may be empty —
///   the export crate already falls back to a generated thumbnail
///   when an expected render file is missing).
/// * `material_schedule` aggregates wall / floor / ceiling counts by
///   `material_id`.
/// * `boq_schedule` lists wall / floor / ceiling areas and
///   door / window counts.
/// * `sheets` deserialises every `kind == "sheet"` entity back into
///   `aec_cad::sheets::Sheet`, and pairs it with the project's
///   `kind == "primitive"` entities converted to `DxfEntity`s via
///   `aec_cad::dxf::primitive_to_dxf` so the contractor pack can emit
///   real per-sheet PDFs.
/// * `ifc_string` builds a minimal-but-real IFC spatial graph from
///   the project graph (one Site / Building / Storey, one Space per
///   room, one IfcWall / IfcSlab / IfcCovering / IfcDoor / IfcWindow
///   per element) and serialises through
///   [`IfcWriter::to_string_with_materials`].
/// * `floor_plan_svg` is rendered from the first sheet (when any
///   sheet entity exists).
/// * `room_count`, `material_count` are walked off the graph;
///   `template_name` comes from the project manifest's `template_id`.
pub fn build_for_project(
    project_path: &str,
    master_key: &[u8; 32],
) -> Result<BuiltDeliverContext, BridgeServiceError> {
    let path = Path::new(project_path);
    let (pkg, conn) = ProjectPackage::open_with_master_key_and_database(path, master_key)?;
    let graph = ProjectGraph::load(&conn)
        .map_err(|e| BridgeServiceError::Invalid(format!("load project graph: {e}")))?;

    let renders_dir = pkg.root().join("renders");

    let material_schedule = build_material_schedule(&graph);
    let boq_schedule = build_boq_schedule(&graph);
    let sheets = collect_sheets(&graph);
    let floor_plan_svg = sheets.first().and_then(|(sheet, entities)| {
        render_sheet_svg_full(
            sheet,
            entities,
            &aec_export::plot_style::PlotStyleTable::new("AEC Studio Default"),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .ok()
    });

    let (bim_project, classification, properties, materials) =
        build_bim_from_graph(pkg.manifest().name.as_str(), &graph);
    let ifc_string =
        IfcWriter::to_string_with_materials(&bim_project, &classification, &properties, &materials);

    let room_count = graph.entities_of_kind("room").count();
    let material_count = count_distinct_materials(&graph);
    let template_name = pkg.manifest().template_id.clone();

    Ok(BuiltDeliverContext {
        renders_dir,
        material_schedule,
        boq_schedule,
        sheets,
        ifc_string,
        floor_plan_svg,
        room_count,
        material_count,
        template_name,
    })
}

/// Walk the project graph and aggregate element counts + areas by
/// `material_id`. Walls / floors / ceilings carry an optional
/// `material_id` on their command body (see
/// `aec_command::commands::wall::CreateWall::material_id`).
fn build_material_schedule(graph: &ProjectGraph) -> ScheduleSheet {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Accum {
        walls: usize,
        floors: usize,
        ceilings: usize,
        area_m2: f64,
    }

    let mut by_material: BTreeMap<String, Accum> = BTreeMap::new();

    for record in graph.entities_of_kind("wall") {
        let material = read_material_id(&record.body);
        let acc = by_material.entry(material).or_default();
        acc.walls += 1;
        if let (Some(start), Some(end), Some(height_mm)) = (
            read_xy(&record.body, "start_mm"),
            read_xy(&record.body, "end_mm"),
            read_number(&record.body, "height_mm"),
        ) {
            let length_mm = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
            acc.area_m2 += (length_mm * height_mm) / 1_000_000.0;
        }
    }

    for record in graph.entities_of_kind("floor") {
        let material = read_material_id(&record.body);
        let acc = by_material.entry(material).or_default();
        acc.floors += 1;
        acc.area_m2 += polygon_area_m2(&record.body);
    }

    for record in graph.entities_of_kind("ceiling") {
        let material = read_material_id(&record.body);
        let acc = by_material.entry(material).or_default();
        acc.ceilings += 1;
        acc.area_m2 += polygon_area_m2(&record.body);
    }

    let columns = vec![
        ScheduleColumn {
            key: "material".into(),
            display: "Material".into(),
        },
        ScheduleColumn {
            key: "walls".into(),
            display: "Walls".into(),
        },
        ScheduleColumn {
            key: "floors".into(),
            display: "Floors".into(),
        },
        ScheduleColumn {
            key: "ceilings".into(),
            display: "Ceilings".into(),
        },
        ScheduleColumn {
            key: "area_m2".into(),
            display: "Area (m²)".into(),
        },
    ];
    let mut sheet = ScheduleSheet::new("Material schedule", columns);
    for (name, acc) in by_material {
        sheet.push_row(vec![
            name,
            acc.walls.to_string(),
            acc.floors.to_string(),
            acc.ceilings.to_string(),
            format!("{:.2}", acc.area_m2),
        ]);
    }
    sheet
}

/// Build a BOQ-lite sheet — totals walls / floors / ceilings / doors /
/// windows with element-level areas.
fn build_boq_schedule(graph: &ProjectGraph) -> ScheduleSheet {
    let mut wall_count = 0usize;
    let mut wall_area = 0.0;
    for record in graph.entities_of_kind("wall") {
        wall_count += 1;
        if let (Some(start), Some(end), Some(height_mm)) = (
            read_xy(&record.body, "start_mm"),
            read_xy(&record.body, "end_mm"),
            read_number(&record.body, "height_mm"),
        ) {
            let length_mm = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
            wall_area += (length_mm * height_mm) / 1_000_000.0;
        }
    }
    let mut floor_count = 0usize;
    let mut floor_area = 0.0;
    for record in graph.entities_of_kind("floor") {
        floor_count += 1;
        floor_area += polygon_area_m2(&record.body);
    }
    let mut ceiling_count = 0usize;
    let mut ceiling_area = 0.0;
    for record in graph.entities_of_kind("ceiling") {
        ceiling_count += 1;
        ceiling_area += polygon_area_m2(&record.body);
    }
    let door_count = graph.entities_of_kind("door").count();
    let window_count = graph.entities_of_kind("window").count();

    let columns = vec![
        ScheduleColumn {
            key: "category".into(),
            display: "Category".into(),
        },
        ScheduleColumn {
            key: "count".into(),
            display: "Count".into(),
        },
        ScheduleColumn {
            key: "area_m2".into(),
            display: "Total area (m²)".into(),
        },
    ];
    let mut sheet = ScheduleSheet::new("Bill of quantities", columns);
    sheet.push_row(vec![
        "Walls".into(),
        wall_count.to_string(),
        format!("{wall_area:.2}"),
    ]);
    sheet.push_row(vec![
        "Floors".into(),
        floor_count.to_string(),
        format!("{floor_area:.2}"),
    ]);
    sheet.push_row(vec![
        "Ceilings".into(),
        ceiling_count.to_string(),
        format!("{ceiling_area:.2}"),
    ]);
    sheet.push_row(vec!["Doors".into(), door_count.to_string(), String::new()]);
    sheet.push_row(vec![
        "Windows".into(),
        window_count.to_string(),
        String::new(),
    ]);
    sheet
}

/// Deserialise every `kind == "sheet"` entity back into a
/// `aec_cad::sheets::Sheet` and pair it with the project's primitive
/// entities (converted to DXF entities via `primitive_to_dxf`).
fn collect_sheets(graph: &ProjectGraph) -> Vec<(Sheet, Vec<DxfEntity>)> {
    let dxf_entities: Vec<DxfEntity> = graph
        .entities_of_kind("primitive")
        .filter_map(|record| {
            let prim: Primitive =
                serde_json::from_value(record.body.get("primitive").cloned().unwrap_or_default())
                    .ok()?;
            primitive_to_dxf(&prim)
        })
        .collect();
    let mut sheets: Vec<(Sheet, Vec<DxfEntity>)> = Vec::new();
    for record in graph.entities_of_kind("sheet") {
        let Ok(sheet) = serde_json::from_value::<Sheet>(record.body.clone()) else {
            continue;
        };
        sheets.push((sheet, dxf_entities.clone()));
    }
    sheets
}

/// Build an `aec_bim::Project` spatial graph from the project graph.
/// The result is intentionally simple — one Site, one Building, one
/// Storey — but it carries every wall / floor / ceiling / door /
/// window as an element with the matching IFC class so the resulting
/// IFC file passes the `IfcWriter` round-trip check.
fn build_bim_from_graph(
    project_name: &str,
    graph: &ProjectGraph,
) -> (
    BimProject,
    ClassificationStore,
    PropertyStore,
    MaterialStore,
) {
    let mut project = BimProject::new(project_name);
    let root = project.root.clone();
    let site = project
        .add_child(&root, IfcClass::IfcSite, "Site")
        .expect("project root exists");
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Building")
        .expect("site exists");
    let storey = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "Level 1")
        .expect("building exists");

    let mut classification = ClassificationStore::new();
    let properties = PropertyStore::new();
    let materials = MaterialStore::new();

    // Rooms → IfcSpace, scoped under the storey.
    for record in graph.entities_of_kind("room") {
        let name = record
            .body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Room");
        let Some(space) = project.add_child(&storey, IfcClass::IfcSpace, name.to_string()) else {
            continue;
        };
        classification.assign_imported(space, IfcClass::IfcSpace);
    }

    // Building elements attach to the storey. We use the
    // entity record's id directly so an out-of-line audit can
    // correlate IFC GlobalIds back to project graph rows.
    for (kind, class) in [
        ("wall", IfcClass::IfcWall),
        ("floor", IfcClass::IfcSlab),
        ("ceiling", IfcClass::IfcCovering),
        ("door", IfcClass::IfcDoor),
        ("window", IfcClass::IfcWindow),
    ] {
        for record in graph.entities_of_kind(kind) {
            let element_id: EntityId = record.id.clone();
            project.attach_element(&storey, element_id.clone());
            classification.assign_imported(element_id, class.clone());
        }
    }

    (project, classification, properties, materials)
}

fn count_distinct_materials(graph: &ProjectGraph) -> usize {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for kind in ["wall", "floor", "ceiling"] {
        for record in graph.entities_of_kind(kind) {
            if let Some(mid) = record
                .body
                .get("material_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                seen.insert(mid.to_string());
            }
        }
    }
    // Also count standalone material entities (kind == "material")
    // if a future template emits them — keeps the count meaningful
    // even when wall bodies don't carry an explicit id yet.
    for record in graph.entities_of_kind("material") {
        if let Some(id) = record.body.get("id").and_then(|v| v.as_str()) {
            seen.insert(id.to_string());
        }
    }
    seen.len()
}

fn read_material_id(body: &serde_json::Value) -> String {
    body.get("material_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map_or_else(|| "(unassigned)".to_string(), str::to_string)
}

fn read_xy(body: &serde_json::Value, key: &str) -> Option<[f64; 2]> {
    let arr = body.get(key)?.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let x = arr[0].as_f64()?;
    let y = arr[1].as_f64()?;
    Some([x, y])
}

fn read_number(body: &serde_json::Value, key: &str) -> Option<f64> {
    body.get(key)?.as_f64()
}

/// Compute a 2D polygon area from a body that stores
/// `polygon_mm: Vec<[f64; 2]>` (floors / ceilings).
fn polygon_area_m2(body: &serde_json::Value) -> f64 {
    let Some(arr) = body.get("polygon_mm").and_then(|v| v.as_array()) else {
        return 0.0;
    };
    let pts: Vec<[f64; 2]> = arr
        .iter()
        .filter_map(|p| {
            let inner = p.as_array()?;
            Some([inner.first()?.as_f64()?, inner.get(1)?.as_f64()?])
        })
        .collect();
    if pts.len() < 3 {
        return 0.0;
    }
    let mut signed_area_mm2 = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        signed_area_mm2 += pts[i][0] * pts[j][1] - pts[j][0] * pts[i][1];
    }
    (signed_area_mm2.abs() * 0.5) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_command::commands::{EntityDelta, EntityRecord};
    use aec_core::types::EntityId;

    fn insert(graph: &mut ProjectGraph, kind: &str, body: serde_json::Value) -> EntityId {
        let id = EntityId::new();
        let delta = EntityDelta::Create {
            record: EntityRecord {
                id: id.clone(),
                kind: kind.into(),
                body,
                parent: None,
            },
        };
        graph.apply(&delta).expect("apply create");
        id
    }

    #[test]
    fn material_schedule_aggregates_by_material_id() {
        let mut graph = ProjectGraph::new();
        insert(
            &mut graph,
            "wall",
            serde_json::json!({
                "start_mm": [0.0, 0.0],
                "end_mm": [1000.0, 0.0],
                "height_mm": 2400.0,
                "thickness_mm": 100.0,
                "material_id": "concrete",
            }),
        );
        insert(
            &mut graph,
            "wall",
            serde_json::json!({
                "start_mm": [0.0, 0.0],
                "end_mm": [0.0, 1000.0],
                "height_mm": 2400.0,
                "thickness_mm": 100.0,
                "material_id": "concrete",
            }),
        );
        insert(
            &mut graph,
            "floor",
            serde_json::json!({
                "polygon_mm": [[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]],
                "thickness_mm": 200.0,
                "material_id": "oak",
            }),
        );
        let sheet = build_material_schedule(&graph);
        assert_eq!(sheet.title, "Material schedule");
        // Two materials, sorted: concrete, oak.
        assert_eq!(sheet.rows.len(), 2);
        let concrete = sheet
            .rows
            .iter()
            .find(|r| r.cells[0] == "concrete")
            .unwrap();
        assert_eq!(concrete.cells[1], "2"); // walls
        let oak = sheet.rows.iter().find(|r| r.cells[0] == "oak").unwrap();
        assert_eq!(oak.cells[2], "1"); // floors
    }

    #[test]
    fn boq_schedule_counts_doors_and_windows() {
        let mut graph = ProjectGraph::new();
        insert(
            &mut graph,
            "door",
            serde_json::json!({ "host_wall_id": "w1", "position_mm": 500.0, "width_mm": 900.0 }),
        );
        insert(
            &mut graph,
            "window",
            serde_json::json!({ "host_wall_id": "w1", "position_mm": 1500.0, "width_mm": 1200.0 }),
        );
        insert(
            &mut graph,
            "window",
            serde_json::json!({ "host_wall_id": "w1", "position_mm": 2500.0, "width_mm": 1200.0 }),
        );
        let sheet = build_boq_schedule(&graph);
        // Walls/Floors/Ceilings/Doors/Windows — five rows always emitted.
        assert_eq!(sheet.rows.len(), 5);
        let doors = sheet.rows.iter().find(|r| r.cells[0] == "Doors").unwrap();
        assert_eq!(doors.cells[1], "1");
        let windows = sheet.rows.iter().find(|r| r.cells[0] == "Windows").unwrap();
        assert_eq!(windows.cells[1], "2");
    }

    #[test]
    fn polygon_area_handles_unit_square() {
        let body = serde_json::json!({
            "polygon_mm": [[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]]
        });
        // 1 m × 1 m = 1 m² (input is in mm).
        let area = polygon_area_m2(&body);
        assert!((area - 1.0).abs() < 1e-9, "expected 1 m², got {area}");
    }

    #[test]
    fn count_distinct_materials_dedupes_across_kinds() {
        let mut graph = ProjectGraph::new();
        insert(
            &mut graph,
            "wall",
            serde_json::json!({"material_id": "concrete"}),
        );
        insert(
            &mut graph,
            "floor",
            serde_json::json!({"material_id": "concrete"}),
        );
        insert(
            &mut graph,
            "ceiling",
            serde_json::json!({"material_id": "drywall"}),
        );
        assert_eq!(count_distinct_materials(&graph), 2);
    }

    #[test]
    fn bim_from_graph_emits_one_storey_plus_elements() {
        let mut graph = ProjectGraph::new();
        insert(
            &mut graph,
            "room",
            serde_json::json!({"name": "Living", "wall_ids": []}),
        );
        insert(
            &mut graph,
            "wall",
            serde_json::json!({"start_mm":[0.0,0.0],"end_mm":[1000.0,0.0],"height_mm":2400.0,"thickness_mm":100.0}),
        );
        let (bim, classification, _props, _materials) = build_bim_from_graph("test", &graph);
        // Project, Site, Building, Storey, Space (1 room) = 5 nodes.
        assert_eq!(bim.node_count(), 5);
        // One wall element was attached.
        let storey = bim
            .nodes_of_class(&IfcClass::IfcBuildingStorey)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(storey.elements.len(), 1);
        // The wall element has a classification entry.
        let wall_id = &storey.elements[0];
        assert!(classification.get(wall_id).is_some());
    }
}
