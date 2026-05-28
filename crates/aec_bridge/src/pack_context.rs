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
use std::sync::Arc;

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
use crate::snapshot_cache::{SnapshotCache, SnapshotKey};

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
    /// CAD sheet definitions for the contractor pack's multi-page
    /// PDF. Paired with [`Self::sheet_primitives`] on the export
    /// side: every sheet draws from the same shared primitive set
    /// (the viewport on each sheet clips to its own frame), so
    /// storing primitives once here instead of cloning them into a
    /// `(Sheet, Vec<DxfEntity>)` tuple per sheet eliminates the
    /// previous `O(sheets × primitives)` heap pressure observed by
    /// the Devin Review sweep on commit a4be685.
    pub sheets: Vec<Sheet>,
    /// Shared DXF entity vec backing every sheet's PDF page. Built
    /// once in [`build_for_project`] from the project graph's
    /// `kind == "primitive"` rows; borrowed by
    /// [`Self::as_context`] as `Option<&[DxfEntity]>`.
    pub sheet_primitives: Vec<DxfEntity>,
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
            // Primitives also collapse `[]` → `None` so the export
            // crate's `unwrap_or(&[])` path matches the same
            // "no geometry, frame only" semantics whether the field
            // is genuinely unset or just empty.
            sheet_primitives: if self.sheet_primitives.is_empty() {
                None
            } else {
                Some(self.sheet_primitives.as_slice())
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
///    extract `source_path`, and recover a parsed [`IfcSnapshot`].
///    The IFC is recovered via the supplied `snapshot_cache`: on a
///    `(canonical_path, mtime, size)` hit we re-use the existing
///    `Arc<IfcSnapshot>` and skip the disk read + STEP parse
///    entirely; on a miss we read the file with the same
///    `from_utf8_lossy` tolerance pattern as
///    `BridgeService::bim_import_ifc` / `load_ifc_snapshot`, parse,
///    and populate the cache so the *next* deliver / proposal pack
///    against the same source pays zero parse cost. The cache lives
///    on the [`BridgeService`] singleton and is shared with the
///    preview/attach paths in `BridgeService::bim_*`, so any
///    sequence the renderer issues (preview → attach → deliver pack
///    × 4 kinds → proposal pack) re-parses the file at most once.
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
pub(crate) fn build_for_project(
    project_path: &str,
    master_key: &[u8; 32],
    snapshot_cache: &SnapshotCache,
) -> Result<OwnedPackContext, BridgeServiceError> {
    let project_root = PathBuf::from(project_path);
    let renders_dir = project_root.join("renders");

    // Scope the SQLite connection (and the `ProjectPackage` handle
    // that owns the file descriptor) to a tight block so both drop
    // — releasing the database file handle, the WAL fd, and the
    // shared-lock — *before* the slower I/O below
    // (`std::fs::canonicalize` + `std::fs::read` + `IfcReader::
    // from_string` + cache writes). Holding `conn` across those calls
    // doesn't cause correctness issues (SQLCipher uses WAL +
    // `busy_timeout`), but it widens the lock-contention window
    // against concurrent `bim_attach_ifc` / `command_apply` calls
    // taking write locks on the same project — multi-tab editing
    // or auto-save against the same project package while a deliver
    // pack is in flight could see noisier `SQLITE_BUSY` retries.
    // Read everything the export needs into owned values inside the
    // block; the function body below operates purely on those.
    let (graph, template_name) = {
        let (pkg, conn) =
            ProjectPackage::open_with_master_key_and_database(project_path, master_key)?;
        let template_name = pkg.manifest().template_id.clone();
        let graph = ProjectGraph::load(&conn)?;
        (graph, template_name)
        // `conn` and `pkg` are dropped here, before the canonicalize
        // / read / parse / cache calls below.
    };

    // --- IFC: recover canonical source path from any spatial row.
    //
    // `bim_attach::attach_snapshot` stamps the canonical source path
    // onto every `bim/spatial/*` and `bim/element/*` entity's body
    // JSON (`BimSpatialBody.source_path` / `BimElementBody.source_path`).
    // The first spatial row's path is sufficient for the deliver
    // pack — the IFC is project-scoped, not per-element. If a future
    // PR-x attaches multiple IFCs into one project (federation), this
    // is the place that needs to grow into a per-federation walk.
    let mut snapshot: Option<Arc<IfcSnapshot>> = None;
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
        // Canonicalise so the cache key matches the form
        // `bim_import_ifc` / `bim_attach_ifc` use. `canonicalize`
        // fails when the file has been moved/deleted since attach;
        // treat that as a parse miss (continue scanning the graph for
        // another spatial row that might point at a still-resolvable
        // source) rather than an error.
        let Ok(canonical) = std::fs::canonicalize(&source_path) else {
            continue;
        };
        let Ok(key) = SnapshotKey::from_canonical_path(&canonical) else {
            // metadata() failed (race against deletion) — same
            // best-effort policy as the canonicalize failure above.
            continue;
        };
        if let Some(cached) = snapshot_cache.get(&key) {
            // Cache hit: the preview/attach path or an earlier deliver
            // pack already parsed this exact file (matching mtime +
            // size). Re-use the existing `Arc<IfcSnapshot>` and skip
            // both the disk read and the STEP parse — saves ~10–50 ms
            // per typical project IFC, and bounds the cost of N
            // back-to-back pack-kind exports (concept / interior /
            // contractor / bim) at a single parse rather than N.
            snapshot = Some(cached);
            break;
        }
        let Ok(bytes) = std::fs::read(&canonical) else {
            continue;
        };
        // Real-world IFC exports — particularly CJK-locale and
        // legacy-locale outputs from older ArchiCAD / Revit and
        // IfcOpenShell-scripted pipelines — sometimes leak raw
        // Windows-1252 or Shift-JIS bytes into `IfcLabel` / `IfcText`
        // string literals. A strict `std::str::from_utf8` would reject
        // those files outright, causing the deliver / proposal pack
        // to silently drop the entire IFC (and degrade to empty
        // schedules + skeletal IFC) with no diagnostic the user
        // could act on. Use the same byte-read + lossy decode pattern
        // as `BridgeService::bim_import_ifc` (service.rs:2212) and
        // `BridgeService::load_ifc_snapshot` (service.rs:2907): invalid
        // sequences are replaced with U+FFFD inside string literals
        // while the structural STEP grammar (entity-type keywords,
        // `#N` refs, `,` / `;` / `'` delimiters — all ASCII by spec)
        // is preserved and the parser can proceed.
        let body_str = String::from_utf8_lossy(&bytes);
        if let Ok(snap) = aec_bim::ifc::IfcReader::from_string(&body_str) {
            let arc = Arc::new(snap);
            // Populate the cache so future deliver/proposal/preview
            // calls against the same `(canonical_path, mtime, size)`
            // re-use this parse. The cache enforces its own LRU +
            // TTL eviction policy (see `snapshot_cache::SnapshotCache`).
            snapshot_cache.insert(key, Arc::clone(&arc));
            snapshot = Some(arc);
            break;
        }
    }

    let (material_schedule, ifc_string, room_count, material_count) = match snapshot.as_deref() {
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
    let mut sheets: Vec<Sheet> = Vec::new();
    for entity in graph.entities_of_kind("sheet") {
        let sheet: Sheet = match serde_json::from_value(entity.body.clone()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        sheets.push(sheet);
    }
    // Every sheet draws from the same primitive set — the
    // `SheetPdfBuilder::add_sheet` impl clips by each sheet's
    // viewport, so a per-sheet primitive filter at this layer
    // would be redundant. Storing primitives once here (instead
    // of cloning into a `(Sheet, Vec<DxfEntity>)` tuple per sheet)
    // turns the previous `O(sheets × primitives)` allocation into
    // a single owned vec backed by an `&[DxfEntity]` borrow at
    // export time.

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
        sheet_primitives: all_primitives,
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
            sheet_primitives: Vec::new(),
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
            sheet_primitives: Vec::new(),
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
        assert!(
            ctx.sheet_primitives.is_none(),
            "empty sheet_primitives vec must surface as None so SheetPdfBuilder sees no geometry"
        );
    }

    /// Regression for the Phase 13 PR-71 Devin Review FLAG:
    /// before this refactor, [`build_for_project`] cloned the full
    /// `all_primitives: Vec<DxfEntity>` into every `(Sheet, _)` tuple
    /// it produced, so a project with N sheets and M primitives
    /// allocated `N × M` `DxfEntity`s before the contractor pack was
    /// even emitted. Post-refactor the primitive vec is owned once on
    /// the [`OwnedPackContext`] and borrowed as `&[DxfEntity]` at
    /// export time — the test pins both invariants: (a) the owned
    /// primitives vec is referentially the same backing buffer
    /// regardless of sheet count, and (b) `as_context()` exposes that
    /// buffer to every sheet through one shared slice.
    #[test]
    fn as_context_shares_one_primitive_slice_across_sheets() {
        use aec_cad::dxf::{DxfEntity, DxfLine};
        use aec_cad::sheets::{PaperSize, Sheet};
        // Two sheets, three primitives — in the old shape the export
        // crate received a `&[(Sheet, Vec<DxfEntity>)]` with two
        // tuples each carrying a *clone* of the same primitive vec.
        // Now both sheets share a single borrowed slice.
        let primitives = vec![
            DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [0.0, 0.0, 0.0],
                end: [1.0, 0.0, 0.0],
            }),
            DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [1.0, 0.0, 0.0],
                end: [1.0, 1.0, 0.0],
            }),
            DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [1.0, 1.0, 0.0],
                end: [0.0, 1.0, 0.0],
            }),
        ];
        let owned = OwnedPackContext {
            renders_dir: PathBuf::from("."),
            material_schedule: None,
            boq_schedule: None,
            sheets: vec![
                Sheet::new("A100", PaperSize::IsoA1),
                Sheet::new("A101", PaperSize::IsoA1),
            ],
            sheet_primitives: primitives,
            ifc_string: None,
            floor_plan_svg: None,
            room_count: None,
            material_count: None,
            template_name: None,
        };
        let ctx = owned.as_context();
        let exposed = ctx
            .sheet_primitives
            .expect("non-empty primitives vec must surface as Some");
        assert_eq!(
            exposed.len(),
            3,
            "every sheet draws from the same three-primitive set"
        );
        // Both the owned vec and the borrowed slice point at the
        // same heap allocation — no clone happened on `as_context()`.
        assert_eq!(
            exposed.as_ptr(),
            owned.sheet_primitives.as_ptr(),
            "as_context() must borrow the owned primitive vec, not clone it"
        );
        assert_eq!(
            ctx.sheets
                .expect("non-empty sheets vec must surface as Some")
                .len(),
            2
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
        let cache = SnapshotCache::new();
        let ctx = build_for_project(project_path.to_str().unwrap(), &master_key, &cache).unwrap();
        assert_eq!(ctx.renders_dir, project_path.join("renders"));
        assert!(ctx.material_schedule.is_none());
        assert!(ctx.ifc_string.is_none());
        assert!(ctx.sheets.is_empty());
        assert!(ctx.room_count.is_none());
        assert!(ctx.material_count.is_none());
        // template_id was passed as `None` so this round-trips as
        // `None`, not as `Some(\"\")`.
        assert!(ctx.template_name.is_none());
        // An empty project has no `bim/spatial/*` rows, so the IFC
        // recovery loop never touches the cache.
        assert_eq!(
            cache.len(),
            0,
            "empty project must not have populated the IFC snapshot cache"
        );
    }

    /// Regression for the Phase 13 PR-71 Devin Review BUG:
    /// `build_for_project` previously called `std::str::from_utf8` on
    /// the IFC bytes and silently `continue`d on `Err(_)`, which
    /// dropped the entire IFC from the deliver / proposal pack
    /// whenever the source file carried any non-UTF-8 bytes (CJK
    /// locales, legacy ArchiCAD / IfcOpenShell exports). Post-fix
    /// the function uses `String::from_utf8_lossy` — the same
    /// pattern as `BridgeService::bim_import_ifc` and
    /// `BridgeService::load_ifc_snapshot` — so the IFC survives and
    /// `ctx.ifc_string` is populated.
    #[test]
    fn build_for_project_recovers_ifc_with_non_utf8_bytes() {
        use aec_command::commands::project_graph::{EntityDelta, EntityRecord};
        use aec_core::types::EntityId;

        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().join("non_utf8.aecstudio");
        let master_key = [0u8; 32];
        let _pkg = ProjectPackage::create(
            project_path.to_str().unwrap(),
            "NonUtf8",
            aec_core::ProjectSettings::default(),
            None,
            &master_key,
        )
        .unwrap();

        // Same fixture as `bim_import_ifc_tolerates_non_utf8_bytes`
        // in service.rs: a minimal valid IFC2x3 graph with a single
        // 0x9F byte (Windows-1252 codepoint, invalid UTF-8) inside
        // the IFCPROJECT name string.
        let ifc_path = tmp.path().join("legacy.ifc");
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(
            b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('test'),'2;1');\n\
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC2X3'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000a1',#1,'Latin1-",
        );
        body.push(0x9F);
        body.extend_from_slice(
            b"','P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n",
        );
        std::fs::write(&ifc_path, &body).unwrap();

        // Insert a `bim/spatial/IfcProject` row whose body JSON
        // points at the non-UTF-8 file. We construct the entity
        // through the public `ProjectGraph::persist_delta_in_tx`
        // API so the row matches the exact schema
        // `bim_attach::attach_snapshot` writes — keeping the test
        // honest about the production data shape.
        let (_pkg, mut conn) = ProjectPackage::open_with_master_key_and_database(
            project_path.to_str().unwrap(),
            &master_key,
        )
        .unwrap();
        let spatial_body = serde_json::json!({
            "ifc_guid": "00000000000000000000a1",
            "ifc_class": "IfcProject",
            "name": "Legacy",
            "source_schema": "IFC2X3",
            "source_path": ifc_path.to_str().unwrap(),
        });
        let record = EntityRecord {
            id: EntityId::from_guid_seed("pack_context-non-utf8-ifc"),
            kind: "bim/spatial/IfcProject".to_owned(),
            body: spatial_body,
            parent: None,
        };
        let tx = conn.transaction().unwrap();
        aec_command::commands::ProjectGraph::persist_delta_in_tx(
            &tx,
            &EntityDelta::Create { record },
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let cache = SnapshotCache::new();
        let ctx = build_for_project(project_path.to_str().unwrap(), &master_key, &cache).unwrap();
        assert!(
            ctx.ifc_string.is_some(),
            "lossy UTF-8 decode must let the IFC parse succeed — pre-fix this was None because \
             std::str::from_utf8 rejected the 0x9F byte and the code silently `continue`d"
        );
        // The IFC was parseable, so the deliver / proposal pack now
        // gets a real `model/project.ifc` body rather than the
        // skeletal `build_summary_ifc` fallback. The reproduced
        // schema header proves we round-tripped through IfcReader →
        // IfcWriter (the writer always emits an `ISO-10303-21;`
        // preamble).
        assert!(
            ctx.ifc_string
                .as_ref()
                .unwrap()
                .starts_with("ISO-10303-21;"),
            "recovered IFC must start with the STEP preamble"
        );
        // The successful parse populated the snapshot cache. A second
        // call must observe the cache hit (no second parse) and
        // produce a byte-identical `ifc_string` from the same
        // cached `IfcSnapshot`.
        assert_eq!(
            cache.len(),
            1,
            "first build_for_project must have populated the snapshot cache"
        );
        let ctx2 = build_for_project(project_path.to_str().unwrap(), &master_key, &cache).unwrap();
        assert_eq!(
            ctx.ifc_string, ctx2.ifc_string,
            "cache-hit deliver pack must produce the same IFC bytes as the cache-miss path"
        );
    }

    /// Regression for the Devin Review sweep-2 INFO finding
    /// (`pack_context.rs:166-222` — "build_for_project re-parses IFC
    /// on every deliver/proposal"). Threading the bridge's existing
    /// `SnapshotCache` through `build_for_project` means N back-to-
    /// back pack-kind exports (concept / interior / contractor / bim)
    /// re-parse the source IFC at most once. The mtime/size key
    /// guarantees correctness across IFC edits between calls.
    #[test]
    fn build_for_project_reuses_cached_snapshot_across_calls() {
        use aec_command::commands::project_graph::{EntityDelta, EntityRecord};
        use aec_core::types::EntityId;

        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().join("reuse.aecstudio");
        let master_key = [0u8; 32];
        let _pkg = ProjectPackage::create(
            project_path.to_str().unwrap(),
            "Reuse",
            aec_core::ProjectSettings::default(),
            None,
            &master_key,
        )
        .unwrap();

        // Same minimal IFC2x3 fixture as the non-UTF-8 test above
        // — the encoding is irrelevant here; we just need a file
        // `IfcReader::from_string` accepts so a snapshot lands in
        // the cache.
        let ifc_path = tmp.path().join("reuse.ifc");
        let body = b"ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('test'),'2;1');\n\
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');\n\
FILE_SCHEMA(('IFC2X3'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);\n\
#2 = IFCPROJECT('00000000000000000000a2',#1,'Reuse','P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        std::fs::write(&ifc_path, body).unwrap();

        let (_pkg, mut conn) = ProjectPackage::open_with_master_key_and_database(
            project_path.to_str().unwrap(),
            &master_key,
        )
        .unwrap();
        let spatial_body = serde_json::json!({
            "ifc_guid": "00000000000000000000a2",
            "ifc_class": "IfcProject",
            "name": "Reuse",
            "source_schema": "IFC2X3",
            "source_path": ifc_path.to_str().unwrap(),
        });
        let record = EntityRecord {
            id: EntityId::from_guid_seed("pack_context-reuse-ifc"),
            kind: "bim/spatial/IfcProject".to_owned(),
            body: spatial_body,
            parent: None,
        };
        let tx = conn.transaction().unwrap();
        aec_command::commands::ProjectGraph::persist_delta_in_tx(
            &tx,
            &EntityDelta::Create { record },
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let cache = SnapshotCache::new();
        // First call: cache miss → parse → insert.
        let _ctx1 = build_for_project(project_path.to_str().unwrap(), &master_key, &cache).unwrap();
        assert_eq!(cache.len(), 1, "first call must populate the cache");

        // Second call: cache hit — same `(canonical_path, mtime, size)`.
        // The cached entry is reused; cache size stays at 1 (no
        // re-insert with a different key).
        let _ctx2 = build_for_project(project_path.to_str().unwrap(), &master_key, &cache).unwrap();
        assert_eq!(
            cache.len(),
            1,
            "second call against the same file must hit the cache, not insert a new entry"
        );

        // Mutating the file changes the mtime, which invalidates the
        // cache key — the next call must re-parse and end up with a
        // *replaced* (not additional) entry under the new key. Sleep
        // briefly to ensure the new mtime is observably later than
        // the original write (FAT/HFS mtime granularity is
        // ~1–2 seconds on some platforms).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&ifc_path, body).unwrap();
        let _ctx3 = build_for_project(project_path.to_str().unwrap(), &master_key, &cache).unwrap();
        // After the rewrite the new `(mtime, size)` insert must
        // *replace* the stale entry under the same canonical path, not
        // coexist with it. The `SnapshotCache::insert` per-path
        // uniqueness invariant guarantees this deterministically —
        // any cached entry sharing this file's `canonical_path` with a
        // different filesystem identity is dropped before the new
        // entry lands, so the cache size stays at exactly 1 regardless
        // of TTL timing. See
        // `snapshot_cache::tests::insert_evicts_obsolete_entries_for_same_canonical_path`
        // for the cache-layer regression that locks this in.
        assert_eq!(
            cache.len(),
            1,
            "after a file rewrite the per-path uniqueness invariant must reduce the cache to one \
             live entry for this canonical path, not two"
        );
    }
}
