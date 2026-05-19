//! Regression test: every JSON file under `templates/` at the repo root
//! must parse against the live [`aec_core::TemplateDefinition`] struct.
//!
//! This catches the class of bug where a template author and the struct
//! author drift apart (e.g. JSON shipping `location_mm` while the struct
//! expects `position_mm`, or JSON shipping `storeys` while the struct only
//! understands flat rooms).

use std::path::PathBuf;

use aec_core::templates::{validate_template_dir, TemplateLoader};

fn templates_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/crates/aec_core`; templates live at
    // `<repo>/templates`.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    PathBuf::from(manifest)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("templates")
}

#[test]
fn every_shipped_template_parses() {
    let dir = templates_dir();
    assert!(
        dir.is_dir(),
        "expected `templates/` directory at repo root: {}",
        dir.display(),
    );
    let count = validate_template_dir(&dir).expect("templates parse");
    // We ship 9 templates today (8 interior/architecture + 2D drafting).
    assert!(
        count >= 9,
        "expected at least 9 shipped templates, found {count}",
    );
}

#[test]
fn villa_template_loads_with_multi_storey_hierarchy() {
    let loader = TemplateLoader::new(templates_dir());
    let villa = loader.load("architecture.villa").expect("villa parses");
    assert!(!villa.storeys.is_empty(), "villa should have storeys");
    assert!(
        villa.rooms.is_empty(),
        "villa rooms list is empty; rooms live under storeys",
    );
    assert!(villa.iter_rooms().count() >= 3);
}

#[test]
fn drafting_template_has_sheet_presets_and_no_lighting() {
    let loader = TemplateLoader::new(templates_dir());
    let drafting = loader
        .load("drafting.2d_drafting")
        .expect("drafting parses");
    assert!(drafting.lighting_preset.is_none());
    assert!(!drafting.sheet_presets.is_empty());
}
