//! Construct a real [`aec_export::DeliverPackContext`] from an open
//! AEC Studio project package (Phase 13 Tasks 7 + 10–12).
//!
//! Every production caller of
//! [`aec_export::write_deliver_pack_with_context`] /
//! [`aec_export::write_proposal_pack_with_context`] in the bridge
//! routes through this module so that:
//!
//!   * `material_schedule` / `boq_schedule` carry real generated
//!     rows from the project's attached IFC snapshot (instead of
//!     the legacy header-only XLSX scaffolding the
//!     `#[cfg(test)]`-only `placeholder_xlsx()` used to produce);
//!   * `sheets` carry the project's real CAD sheet definitions and
//!     their associated DXF entities (so the contractor pack's PDF
//!     pages contain actual entity geometry, not a title-only
//!     fallback);
//!   * `ifc_string` carries the bytes emitted by
//!     `IfcWriter::to_string_with_materials` against the project's
//!     attached source (so the contractor and BIM pack's
//!     `model/project.ifc` round-trips through `IfcReader` cleanly);
//!   * `renders_dir` points at the project's `<root>/renders/`
//!     directory (so any cached render PNG ends up embedded
//!     directly rather than falling back to the synthesised
//!     gradient thumbnail);
//!   * `room_count`, `material_count`, `template_name`, and
//!     `floor_plan_svg` populate the proposal pack's cover page
//!     with project-specific facts instead of the generic empty
//!     paragraph.
//!
//! ## Why this is its own module
//!
//! [`BridgeService`] is stateless — every endpoint that needs
//! project data opens the SQLCipher database on demand via
//! `ProjectPackage::open_with_master_key_and_database`. Hanging the
//! pack-context construction off the `BridgeService` impl directly
//! would mix in-process data-shape concerns (which classification
//! store goes with which schedule generator, how to convert the
//! project's `Primitive` entities to `DxfEntity`s, how to look up
//! the canonical IFC source path stored inside each `bim/spatial`
//! entity's body JSON) with the bridge's per-endpoint orchestration.
//!
//! Pulling it into [`pack_context`] gives the bridge a single named
//! entry point (`build_for_project`) that returns an `OwnedPackContext`
//! carrying every owned buffer the export crate needs, plus a tested
//! `.as_context()` helper that materialises a borrowed
//! `DeliverPackContext<'_>` over those buffers. The `'a` lifetime
//! stays inside the bridge, so changes to the `DeliverPackContext`
//! struct (e.g. adding a new optional field) cascade through one
//! place rather than every deliver/proposal endpoint.

use std::path::PathBuf;

use aec_bim::ifc::IfcSnapshot;
use aec_bim::schedules::ScheduleSheet;
use aec_cad::dxf::{primitive_to_dxf, DxfEntity};
use aec_cad::sheets::Sheet;
use aec_command::commands::draft::DrawPrimitive;
use aec_command::commands::ProjectGraph;
use aec_core::package::ProjectPackage;
use aec_export::DeliverPackContext;
use serde::Deserialize;

use crate::service::BridgeServiceError;

/// Owned data backing a [`DeliverPackContext`]. The export crate
/// borrows everything by reference, so the caller (the bridge
/// endpoint) keeps this struct alive across the
/// `write_*_with_context` call and reads `.as_context()` on it.
///
/// `Option`-ised fields exactly mirror the `DeliverPackContext`
/// shape so a missing data source (no IFC attached, no CAD sheets
/// authored, no renders rendered yet) cleanly degrades to the
/// fallback path on the export side — never to the legacy
/// `placeholder_xlsx()` / `build_summary_ifc()` paths, which are
/// now `#[cfg(test)]`-only / context-fallback-only respectively.
pub struct OwnedPackContext {
    /// `<project_path>/renders/`. Populated unconditionally; the
    /// export crate probes for the per-archive-name file before
    /// falling back to the gradient thumbnail, so pointing at an
    /// empty dir is harmless and pointing at a populated one
    /// embeds the real PNG without any extra branching here.
    pub renders_dir: PathBuf,
    pub material_schedule: Option<ScheduleSheet>,
    pub boq_schedule: Option<ScheduleSheet>,
    pub sheets: Vec<(Sheet, Vec<DxfEntity>)>,
    pub ifc_string: Option<String>,
    pub floor_plan_svg: Option<String>,
    pub room_count: Option<usize>,
    pub material_count: Option<usize>,
    pub template_name: Option<String>,
}

impl OwnedPackContext {
    /// Return a borrowed [`DeliverPackContext`] view.
    ///
    /// All fields are zero-copy references into `self`; the
    /// returned struct must not outlive `self`. The `'a` is
    /// explicitly bound to `self`'s lifetime so accidental misuse
    /// (`drop(owned); owned.as_context()`) is a compile error.
    pub fn as_context(&self) -> DeliverPackContext<'_> {
        DeliverPackContext {
            renders_dir: Some(self.renders_dir.as_path()),
            material_schedule: self.material_schedule.as_ref(),
            boq_schedule: self.boq_schedule.as_ref(),
            // `Some(&[])` here means "use the explicit empty list"
            // rather than "no context supplied" — that distinction
            // matters on the export side where `None` triggers the
            // kind-based fallback sheet list (`A100.pdf` etc.). For
            // projects that genuinely have zero sheets we still
            // want the fallback path, so we collapse the empty case
            // to `None`.
            sheets: if self.sheets.is_empty() {
                None
            } else {
                Some(self.sheets.as_slice())
            },
            ifc_string: self.ifc_string.as_deref(),
            floor_plan_svg: self.floor_plan_svg.as_deref(),
            room_count: self.room_count,
            material_count: self.material_count,
            template_name: self.template_name.as_deref(),
        }
    }
}

/// Body shape of a `bim/spatial/*` entity row, used only to recover
/// the canonical IFC `source_path` stored at attach time. This is
/// the structural subset of [`crate::bim_attach::BimSpatialBody`] we
/// care about here — duplicated as a `Deserialize`-only struct so
/// the cross-module coupling stays one-way (pack_context reads
/// rows, bim_attach writes them, neither imports the other's
/// types).
#[derive(Deserialize)]
struct BimSpatialBodyView {
    source_path: Option<String>,
}

/// Build an [`OwnedPackContext`] for `project_path`.
///
/// Steps (every one is best-effort — a missing data source is not
/// an error, just a `None` field in the resulting context that the
/// export crate handles via its fallback path):
///
/// 1. Open the project DB via
///    `ProjectPackage::open_with_master_key_and_database` and read
///    the manifest for `name` / `template_id`.
/// 2. Load the project graph (`ProjectGraph::load`).
/// 3. If any `bim/spatial/*` entity exists, parse its body JSON,
///    extract `source_path`, and re-parse the IFC via
///    `IfcReader::from_string` (re-using the bridge's snapshot
///    cache would require holding a `&BridgeService` here — we
///    instead do a fresh parse since this is a deliver-time
///    one-shot, not a hot path).
/// 4. From the snapshot, generate material + room schedules and
///    serialise the IFC back via `IfcWriter::to_string_with_materials`.
/// 5. Walk the graph for `kind == "sheet"` entries; deserialise each
///    body into a [`Sheet`], pair with the DXF entities derived from
///    all `kind == "primitive"` entries via `primitive_to_dxf`.
/// 6. Compute counts: `room_count = #IfcSpace`, `material_count =
///    #snapshot.materials.materials`.
///
/// The function returns `Err` only for *real* failures: project DB
/// unopenable, manifest unreadable, project_graph load failure. A
/// missing IFC source file is logged as a `None` on the resulting
/// context, not an error.
pub fn build_for_project(
    project_path: &str,
    master_key: &[u8; 32],
) -> Result<OwnedPackContext, BridgeServiceError> {
    let project_root = PathBuf::from(project_path);
    let renders_dir = project_root.join("renders");

    let (pkg, conn) = ProjectPackage::open_with_master_key_and_database(project_path, master_key)?;
    let template_name = pkg.manifest().template_id.clone();

    let graph = ProjectGraph::load(&conn)?;

    // --- IFC: recover canonical source path from any spatial row.
    //
    // `bim_attach::attach_snapshot` stamps the canonical source path
    // onto every `bim/spatial/*` and `bim/element/*` entity's body
    // JSON (`BimSpatialBody.source_path` / `BimElementBody.source_path`).
    // The first spatial row's path is sufficient for the deliver
    // pack — the IFC is project-scoped, not per-element. If a future
    // PR-x attaches multiple IFCs into one project (federation), this
    // is the place that needs to grow into a per-federation walk.
    let mut snapshot: Option<IfcSnapshot> = None;
    for entity in graph.iter() {
        if !entity.kind.starts_with("bim/spatial/") {
            continue;
        }
        let Ok(body): Result<BimSpatialBodyView, _> = serde_json::from_value(entity.body.clone())
        else {
            continue;
        };
        let Some(source_path) = body.source_path.filter(|s| !s.is_empty()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&source_path) else {
            continue;
        };
        let Ok(body_str) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if let Ok(snap) = aec_bim::ifc::IfcReader::from_string(body_str) {
            snapshot = Some(snap);
            break;
        }
    }

    let (material_schedule, ifc_string, room_count, material_count) = match snapshot.as_ref() {
        Some(snap) => {
            let (_entries, sheet) = aec_bim::schedules::generate_material_schedule(
                &snap.classification,
                &snap.properties,
            );
            let ifc = aec_bim::ifc::IfcWriter::to_string_with_materials(
                &snap.project,
                &snap.classification,
                &snap.properties,
                &snap.materials,
            );
            let rooms = snap
                .classification
                .iter()
                .filter(|(_, a)| matches!(a.class, aec_bim::IfcClass::IfcSpace))
                .count();
            // `MaterialStore::materials()` exposes the distinct
            // materials as an iterator (the field itself is
            // private). Counting distinct materials (not
            // assignments) matches the proposal pack cover
            // wording ("N materials").
            let mats = snap.materials.materials().count();
            (Some(sheet), Some(ifc), Some(rooms), Some(mats))
        }
        None => (None, None, None, None),
    };

    // --- Sheets + DxfEntity vector.
    let mut all_primitives: Vec<DxfEntity> = Vec::new();
    for entity in graph.entities_of_kind("primitive") {
        let dp: DrawPrimitive = match serde_json::from_value(entity.body.clone()) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if let Some(dxf) = primitive_to_dxf(&dp.primitive) {
            all_primitives.push(dxf);
        }
    }
    let mut sheets: Vec<(Sheet, Vec<DxfEntity>)> = Vec::new();
    for entity in graph.entities_of_kind("sheet") {
        let sheet: Sheet = match serde_json::from_value(entity.body.clone()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Pair each sheet with the full primitive set. The
        // `SheetPdfBuilder::add_sheet` impl clips by the sheet's
        // viewport so a sheet-specific filter at this layer would
        // be redundant.
        sheets.push((sheet, all_primitives.clone()));
    }

    Ok(OwnedPackContext {
        renders_dir,
        material_schedule,
        // The BOQ schedule isn't currently generated from the IFC
        // snapshot (the BOQ pipeline lives in `aec_bim::boq` and
        // operates on `BoqLine`, not `ScheduleSheet`). When the
        // bridge gains a `bim_generate_boq_schedule_sheet` endpoint
        // this `None` becomes that call's result.
        boq_schedule: None,
        sheets,
        ifc_string,
        floor_plan_svg: None,
        room_count,
        material_count,
        template_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn as_context_with_no_data_still_carries_renders_dir() {
        let owned = OwnedPackContext {
            renders_dir: PathBuf::from("/tmp/aec-pack-context-test"),
            material_schedule: None,
            boq_schedule: None,
            sheets: Vec::new(),
            ifc_string: None,
            floor_plan_svg: None,
            room_count: None,
            material_count: None,
            template_name: None,
        };
        let ctx = owned.as_context();
        assert_eq!(
            ctx.renders_dir.expect("renders_dir always populated"),
            Path::new("/tmp/aec-pack-context-test")
        );
        // Empty sheets vec collapses to `None` so the export crate
        // takes the kind-based fallback list rather than emitting an
        // empty `sheets/` directory.
        assert!(ctx.sheets.is_none());
        assert!(ctx.material_schedule.is_none());
        assert!(ctx.ifc_string.is_none());
    }

    #[test]
    fn as_context_collapses_empty_sheets_to_none() {
        let owned = OwnedPackContext {
            renders_dir: PathBuf::from("."),
            material_schedule: None,
            boq_schedule: None,
            sheets: Vec::new(),
            ifc_string: None,
            floor_plan_svg: None,
            room_count: None,
            material_count: None,
            template_name: None,
        };
        let ctx = owned.as_context();
        assert!(
            ctx.sheets.is_none(),
            "empty sheets vec must surface as None so export crate takes kind-based fallback list"
        );
    }

    #[test]
    fn build_for_project_returns_renders_dir_even_with_empty_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().join("empty.aecstudio");
        let master_key = [0u8; 32];
        // Bootstrap an empty project so build_for_project has a real
        // SQLCipher DB to open. We use the public Create API to keep
        // the test honest about the schema we read back. The
        // `ProjectPackage::create` signature is
        // `(root, name, settings, template_id, master_key)`.
        let _pkg = ProjectPackage::create(
            project_path.to_str().unwrap(),
            "Empty",
            aec_core::ProjectSettings::default(),
            None,
            &master_key,
        )
        .unwrap();
        let ctx = build_for_project(project_path.to_str().unwrap(), &master_key).unwrap();
        assert_eq!(ctx.renders_dir, project_path.join("renders"));
        assert!(ctx.material_schedule.is_none());
        assert!(ctx.ifc_string.is_none());
        assert!(ctx.sheets.is_empty());
        assert!(ctx.room_count.is_none());
        assert!(ctx.material_count.is_none());
        // template_id was passed as `None` so this round-trips as
        // `None`, not as `Some(\"\")`.
        assert!(ctx.template_name.is_none());
    }
}
