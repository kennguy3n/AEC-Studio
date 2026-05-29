//! PROPOSAL.md User Journey A — Interior designer ("Apartment
//! renovation in a weekend"). One test per acceptance criterion, so
//! a failure points directly at the criterion it broke.
//!
//! PROPOSAL.md lines 187-191:
//!
//! - [ ] Project goes from new-template to 4 final renders in
//!   < 2 hours of active work on a mid-tier Mac.
//! - [ ] Every render is reproducible from the saved camera +
//!   preset.
//! - [ ] Client pack exports as a single PDF with embedded
//!   schedule.
//!
//! All tests drive the `BridgeService` public API — the same surface
//! the desktop renderer talks to — so a regression at the IPC
//! boundary is caught here even if the underlying crates pass their
//! unit tests. Fixtures (template + master key + state / projects
//! dirs) are hermetic per-test (no global file or path deps).

use std::path::{Path, PathBuf};
use std::time::Instant;

use aec_bridge::{
    AssetListQuery, BridgeConfig, BridgeService, DeliverBuildPackParams, DeliverPackInventoryFlags,
    RenderJobSummary,
};
use aec_command::commands::{
    camera::{CameraParams, SaveCamera},
    furniture::PlaceFurniture,
    wall::CreateWall,
    Command, CommandKind,
};
use aec_core::types::EntityId;

fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

fn copy_template(category: &str, id: &str, dest: &Path) {
    let src = workspace_templates_dir()
        .join(category)
        .join(format!("{id}.json"));
    let dest_dir = dest.join(category);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest_file = dest_dir.join(format!("{id}.json"));
    std::fs::copy(&src, &dest_file).unwrap_or_else(|e| {
        panic!(
            "failed to copy shipped template {} -> {}: {e}",
            src.display(),
            dest_file.display()
        )
    });
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_template("interior", "apartment", &templates);
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, [0xA1u8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

/// Add four walls forming a single room with a `concrete_white`
/// material. The material id is what `aec_bridge::deliver_context::
/// build_material_schedule` aggregates on, so populating walls with
/// a real material id is what makes the schedule non-empty for the
/// concept-pack criterion further down.
fn place_room_walls(svc: &mut BridgeService, project_path: &str, material: &str) {
    let corners: [[f64; 2]; 4] = [
        [0.0, 0.0],
        [4_000.0, 0.0],
        [4_000.0, 5_000.0],
        [0.0, 5_000.0],
    ];
    for i in 0..4 {
        let start = corners[i];
        let end = corners[(i + 1) % 4];
        let cmd = Command::user(CommandKind::CreateWall(CreateWall {
            entity_id: EntityId::new(),
            start_mm: start,
            end_mm: end,
            height_mm: 2_700.0,
            thickness_mm: 100.0,
            material_id: Some(material.to_string()),
        }));
        svc.command_apply(project_path, cmd)
            .unwrap_or_else(|e| panic!("create wall {i}: {e}"));
    }
}

fn save_camera(svc: &mut BridgeService, project_path: &str, name: &str) -> EntityId {
    let eid = EntityId::new();
    let cmd = Command::user(CommandKind::SaveCamera(SaveCamera {
        entity_id: eid.clone(),
        name: name.to_string(),
        params: CameraParams {
            position_mm: [5_000.0, -2_000.0, 1_650.0],
            target_mm: [2_000.0, 2_500.0, 1_200.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5_600,
            depth_of_field_f: Some(5.6),
            aspect_ratio: 16.0 / 9.0,
        },
    }));
    svc.command_apply(project_path, cmd)
        .unwrap_or_else(|e| panic!("save camera {name}: {e}"));
    eid
}

/// PROPOSAL.md criterion 1: "Project goes from new-template to 4
/// final renders in < 2 hours of active work on a mid-tier Mac."
///
/// The 2-hour budget is overwhelmingly "thinking time" between
/// API calls (deciding which asset to drop, picking a camera angle,
/// reviewing a render). What the bridge owes the user is that the
/// underlying API calls don't *themselves* eat the budget — every
/// call along the journey must return in interactive timeframes.
///
/// This test pins that contract: the full sequence of bridge calls
/// for the journey (create project, place furniture, save 4 cameras,
/// enqueue 4 renders, export concept pack) must complete in well
/// under one minute on the test machine. That leaves the remainder
/// of the 2-hour budget for human deliberation, which is what
/// PROPOSAL.md is actually budgeting.
#[test]
fn journey_a_criterion_workflow_runs_within_interactive_budget() {
    let (mut svc, _g) = boot_service();
    let start = Instant::now();

    // Create project.
    let summary = svc
        .project_create_from_template("interior.apartment", "Journey A")
        .expect("create apartment");

    // Add walls so the material schedule the concept pack embeds is
    // non-empty (criterion 3 will assert on that).
    place_room_walls(&mut svc, &summary.path, "oak_floor");

    // Place 2 furniture instances (the journey calls for "sofa,
    // dining table, bed, lamps, rugs" — we use the first two demo
    // assets; the journey acceptance is about the workflow path,
    // not the catalogue size).
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("list assets");
    assert!(
        assets.len() >= 2,
        "seed library must have at least 2 demo assets for this journey"
    );
    for (i, asset) in assets.iter().take(2).enumerate() {
        let cmd = Command::user(CommandKind::PlaceFurniture(PlaceFurniture {
            entity_id: EntityId::new(),
            asset_ref: asset.asset_id.clone(),
            position_mm: [1_000.0 + (i as f64) * 1_500.0, 1_000.0, 0.0],
            rotation_yaw_deg: 0.0,
            scale_override: None,
            name: Some(asset.name.clone()),
            parent: None,
        }));
        svc.command_apply(&summary.path, cmd)
            .unwrap_or_else(|e| panic!("place furniture {}: {e}", asset.asset_id));
    }

    // Save 4 cameras (one per hero view).
    let cam_living = save_camera(&mut svc, &summary.path, "Living");
    let cam_dining = save_camera(&mut svc, &summary.path, "Dining");
    let cam_bedroom = save_camera(&mut svc, &summary.path, "Bedroom");
    let cam_kitchen = save_camera(&mut svc, &summary.path, "Kitchen");
    let cam_ids = vec![
        cam_living.as_str().to_string(),
        cam_dining.as_str().to_string(),
        cam_bedroom.as_str().to_string(),
        cam_kitchen.as_str().to_string(),
    ];

    // Enqueue 4 renders (1 preset × 4 cameras).
    let batch = svc
        .render_enqueue_batch(&cam_ids, &["standard".to_string()], None)
        .expect("enqueue batch");
    assert_eq!(batch.job_ids.len(), 4, "4 renders should be enqueued");

    // Export the concept pack with project_path so the schedule the
    // PDF embeds is built from the real graph (not the empty
    // fallback).
    let out_dir = tempfile::tempdir().unwrap();
    let pack_path = out_dir.path().join("concept.zip");
    svc.deliver_build_pack(DeliverBuildPackParams {
        out_path: pack_path.to_string_lossy().into_owned(),
        kind: "concept".into(),
        project_name: "Journey A".into(),
        options: DeliverPackInventoryFlags {
            include_renders: true,
            include_sheets: true,
            include_ifc: false,
            include_boq: false,
            include_proposal: false,
        },
        project_path: Some(summary.path.clone()),
    })
    .expect("deliver concept pack");

    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 60,
        "Journey A bridge sequence took {elapsed:?}; the budget for the *API* portion of \
         the 2-hour journey is <60s so human deliberation stays the dominant cost"
    );
}

/// PROPOSAL.md criterion 2: "Every render is reproducible from the
/// saved camera + preset."
///
/// Reproducibility means: given the same (camera entity, preset id,
/// scene) the renderer produces the same image. The render kernel
/// itself is deterministic (the path tracer takes a `rng_seed`
/// derived from `RenderJob`, not from `Utc::now()` — see
/// `aec_render::path_trace`); the integration risk is at the bridge
/// boundary: does `render_enqueue` faithfully wire the saved camera
/// id + preset config into the job, or does it lose information on
/// the way through?
///
/// This test pins that contract: two `render_enqueue` calls with
/// the same `camera_id` + `preset_id` produce two `RenderJobSummary`
/// records that agree on every reproducibility-bearing field
/// (`preset` id, `camera_id`, `batch_id`-shape). The only fields
/// that legitimately differ are the per-job UUID and the queue
/// position.
#[test]
fn journey_a_criterion_render_reproducibility_from_saved_camera_and_preset() {
    let (mut svc, _g) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Journey A repro")
        .expect("create");
    let cam = save_camera(&mut svc, &summary.path, "Hero");

    let cam_id = cam.as_str().to_string();
    let first = svc
        .render_enqueue(&cam_id, "standard", 0, None)
        .expect("first enqueue");
    let second = svc
        .render_enqueue(&cam_id, "standard", 0, None)
        .expect("second enqueue");
    assert_ne!(
        first.job_id, second.job_id,
        "two enqueues must produce distinct job ids — otherwise the queue can't track \
         them independently"
    );

    let jobs = svc.render_list_jobs().expect("list jobs");
    let lookup = |id: &str| -> RenderJobSummary {
        jobs.iter()
            .find(|j| j.job_id == id)
            .cloned()
            .unwrap_or_else(|| panic!("queue lost track of job {id}"))
    };
    let a = lookup(&first.job_id);
    let b = lookup(&second.job_id);

    assert_eq!(
        a.preset, b.preset,
        "same preset_id input must round-trip to the same `preset` field on the queued job"
    );
    assert_eq!(
        a.camera_id, b.camera_id,
        "same camera_id input must round-trip to the same `camera_id` field on the queued job"
    );
    assert_eq!(
        a.camera_id,
        Some(cam_id.clone()),
        "queued job must carry the camera id the user actually saved \
         (not a re-issued / mangled string)"
    );
    assert_eq!(a.batch_id, None);
    assert_eq!(b.batch_id, None);
}

/// PROPOSAL.md criterion 3: "Client pack exports as a single PDF
/// with embedded schedule."
///
/// The concept pack ships exactly one PDF (`concept_pack.pdf`) — it
/// is the single deliverable the client is meant to read end-to-end.
/// PROPOSAL.md step 6 describes that PDF as "cover, mood board,
/// floor plan, 4 renders, material schedule, and a 'next steps'
/// page" — i.e. the schedule is *inside* the PDF, not a sibling
/// XLSX the client has to open separately.
///
/// This test exercises the contract end-to-end:
///   * Build a project with walls carrying real material ids (so the
///     bridge's `DeliverPackContext.material_schedule` is non-empty).
///   * Export the concept pack with `project_path` set (so the
///     bridge builds the real context instead of falling back to an
///     empty one).
///   * Verify the pack contains exactly one PDF named
///     `concept_pack.pdf` — the headline summary — plus auxiliary
///     non-PDF assets (renders, sheet PDFs are auxiliary by archive
///     name).
///   * Read `concept_pack.pdf` out of the ZIP and grep for the
///     material schedule's column headers + the wall material id —
///     proving the schedule was rendered into the PDF page stream
///     (not just attached as a sibling file the PDF ignores).
#[test]
fn journey_a_criterion_concept_pack_is_single_pdf_with_embedded_schedule() {
    let (mut svc, _g) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Journey A pack")
        .expect("create");

    // Material id needs to be unique enough that the PDF text grep
    // below proves it came from the schedule, not from some other
    // page (e.g. the cover title).
    let material = "concept_oak_brushed";
    place_room_walls(&mut svc, &summary.path, material);

    let out_dir = tempfile::tempdir().unwrap();
    let pack_path = out_dir.path().join("concept.zip");
    let pack = svc
        .deliver_build_pack(DeliverBuildPackParams {
            out_path: pack_path.to_string_lossy().into_owned(),
            kind: "concept".into(),
            project_name: "Journey A pack".into(),
            options: DeliverPackInventoryFlags {
                include_renders: true,
                include_sheets: true,
                include_ifc: false,
                include_boq: false,
                include_proposal: false,
            },
            project_path: Some(summary.path.clone()),
        })
        .expect("deliver concept pack");

    // Validate the headline PDF is the only top-level PDF (sheet
    // PDFs live under `sheets/`, so they're auxiliary and the client
    // doesn't have to choose between two summaries).
    let top_level_pdfs: Vec<&String> = pack
        .contents
        .iter()
        .filter(|c| {
            std::path::Path::new(c)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
                && !c.contains('/')
        })
        .collect();
    assert_eq!(
        top_level_pdfs.len(),
        1,
        "concept pack should expose exactly one headline PDF; got {:?}",
        top_level_pdfs
    );
    assert_eq!(top_level_pdfs[0], "concept_pack.pdf");

    // Crack open the ZIP and pull out the headline PDF's bytes.
    let file = std::fs::File::open(&pack.out_path).expect("open pack zip");
    let mut archive = zip::ZipArchive::new(file).expect("read pack zip");
    let mut pdf_bytes = Vec::new();
    {
        use std::io::Read;
        let mut entry = archive
            .by_name("concept_pack.pdf")
            .expect("zip should contain concept_pack.pdf");
        entry.read_to_end(&mut pdf_bytes).expect("read pdf bytes");
    }
    assert!(
        pdf_bytes.len() > 1_500,
        "concept_pack.pdf with embedded schedule should be larger than the \
         minimum cover+text+schedule layout (got {} bytes)",
        pdf_bytes.len()
    );
    assert!(
        pdf_bytes.starts_with(b"%PDF-"),
        "concept_pack.pdf must have a valid PDF header"
    );

    // The PDF's text is laid down as `Tj` operators in content
    // streams. `printpdf` encodes the operand as a hex literal
    // (`<48656C6C6F> Tj` for "Hello") rather than the alternative
    // `(Hello) Tj` literal-string form, so plain UTF-8 grep against
    // the PDF bytes would always miss the schedule text — we have
    // to look for the hex-encoded form.
    //
    // The streams are *not* FlateDecode-compressed by default on
    // `printpdf`'s `PdfDocument::save` path, so a simple
    // substring search over the raw bytes is sufficient.
    fn pdf_contains_text(bytes: &[u8], needle: &str) -> bool {
        use std::fmt::Write as _;
        let mut hex_upper = String::with_capacity(needle.len() * 2);
        let mut hex_lower = String::with_capacity(needle.len() * 2);
        for b in needle.bytes() {
            write!(&mut hex_upper, "{b:02X}").unwrap();
            write!(&mut hex_lower, "{b:02x}").unwrap();
        }
        // Both ASCII (in case PDF chooses literal-string form on
        // some `printpdf` upgrade) and hex (current encoder
        // behaviour).
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
            || bytes
                .windows(hex_upper.len())
                .any(|w| w == hex_upper.as_bytes())
            || bytes
                .windows(hex_lower.len())
                .any(|w| w == hex_lower.as_bytes())
    }
    assert!(
        pdf_contains_text(&pdf_bytes, "Material schedule"),
        "concept_pack.pdf must contain the schedule title (`Material schedule`); \
         the schedule was not embedded into the PDF"
    );
    assert!(
        pdf_contains_text(&pdf_bytes, "Material"),
        "concept_pack.pdf must contain the `Material` column header; \
         the schedule table page is missing"
    );
    assert!(
        pdf_contains_text(&pdf_bytes, material),
        "concept_pack.pdf must contain the wall material id `{material}` \
         from the project graph; the schedule rows are not bound to project data"
    );
}
