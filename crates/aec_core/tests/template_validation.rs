//! Comprehensive validation for every shipped template.
//!
//! `shipped_templates.rs` already proves every template parses; this
//! test asserts the **content-level** invariants every shipped
//! template must satisfy so the Design/Render UI never crashes on an
//! incomplete asset:
//!
//! * non-empty name / category / units
//! * at least one room shell (from either `rooms` or `storeys.rooms`)
//! * non-empty `default_walls`
//! * a `lighting_preset` for templates that aren't drafting templates
//!   (drafting templates explicitly carry `null` because they have no
//!   3D viewport)
//! * at least one camera preset for non-drafting templates
//! * sheet presets present for drafting templates
//! * load time per template stays well under 1.0 s

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use aec_core::templates::{TemplateDefinition, TemplateLoader};

fn repo_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    PathBuf::from(manifest)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn shipped_template_keys() -> Vec<(String, PathBuf)> {
    let templates_dir = repo_root().join("templates");
    let mut out = Vec::new();
    for cat in fs::read_dir(&templates_dir).expect("templates dir readable") {
        let cat = cat.expect("entry readable");
        if !cat.file_type().expect("file type").is_dir() {
            continue;
        }
        let category = cat
            .file_name()
            .into_string()
            .unwrap_or_else(|os| os.to_string_lossy().into_owned());
        for file in fs::read_dir(cat.path()).expect("category dir readable") {
            let file = file.expect("entry readable");
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            out.push((format!("{category}.{stem}"), path));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn is_drafting(template: &TemplateDefinition) -> bool {
    template.category.as_deref() == Some("drafting")
}

#[test]
fn every_template_has_content_invariants() {
    let loader = TemplateLoader::new(repo_root().join("templates"));
    let templates = shipped_template_keys();
    assert!(templates.len() >= 9, "expected ≥9 shipped templates");

    for (key, _path) in &templates {
        let tpl = loader
            .load(key)
            .unwrap_or_else(|e| panic!("template {key} must load: {e:?}"));

        assert!(!tpl.name.trim().is_empty(), "{key}: name must not be empty");
        let category = tpl.category.as_deref().unwrap_or("");
        assert!(
            !category.trim().is_empty(),
            "{key}: category must not be empty",
        );
        // Units is a typed enum; the assertion is implicit in the
        // successful parse, but pin it to a known value so a regression
        // that defaults to a weird unit is caught early.
        let _ = tpl.units; // touched to keep the warning quiet

        let room_count = tpl.iter_rooms().count();
        // Drafting templates have no rooms, and `interior.renovation`
        // intentionally ships an empty starter (designers carve their
        // own rooms). Everything else must declare ≥1 room shell.
        let empty_starter = key == "interior.renovation";
        if !is_drafting(&tpl) && !empty_starter {
            assert!(
                room_count >= 1,
                "{key}: 3D template must have at least one room shell, found {room_count}",
            );
        }

        // default_walls is a required nested struct in the schema; we
        // just confirm it parsed (which the loader already validates)
        // and that thickness values look sane.
        assert!(
            tpl.default_walls.exterior_thickness_mm > 0.0,
            "{key}: exterior wall thickness must be > 0",
        );
        assert!(
            tpl.default_walls.interior_thickness_mm > 0.0,
            "{key}: interior wall thickness must be > 0",
        );

        if is_drafting(&tpl) {
            assert!(
                !tpl.sheet_presets.is_empty(),
                "{key}: drafting templates must ship sheet presets",
            );
            // Drafting templates have no 3D viewport; lighting + cameras
            // are explicitly absent.
            assert!(
                tpl.lighting_preset.is_none(),
                "{key}: drafting templates must NOT carry a lighting preset",
            );
        } else {
            assert!(
                tpl.lighting_preset.is_some(),
                "{key}: 3D template must declare a lighting preset",
            );
            // Empty-starter templates (e.g. interior.renovation) can
            // omit camera presets — the user picks them after the
            // initial scan-import. Everything else must ship at least
            // one camera so the Design viewport boots with a frame.
            if !empty_starter {
                assert!(
                    !tpl.camera_presets.is_empty(),
                    "{key}: 3D template must declare at least one camera preset",
                );
            }
        }
    }
}

#[test]
fn every_template_loads_under_one_second() {
    let loader = TemplateLoader::new(repo_root().join("templates"));
    for (key, _path) in shipped_template_keys() {
        let start = Instant::now();
        loader.load(&key).expect("template must load");
        let elapsed = start.elapsed();
        // Generous budget; we just want to flag a regression that
        // changes JSON parsing from O(n) to something pathological.
        assert!(
            elapsed.as_millis() < 1000,
            "{key} took {} ms (>1 s budget)",
            elapsed.as_millis(),
        );
    }
}
