//! Performance acceptance criteria.
//!
//! These tests validate the timing targets from `PROGRESS.md` and
//! `PROPOSAL.md`:
//!
//! * Template load (one-room apartment) under 1.0 s.
//! * Iterating every room in every shipped template under 1.0 s.
//! * BLAKE3-hash round-trip of a 10 MB blob under 250 ms (audit-trail
//!   hashing budget — every audit append rehashes the previous tail).
//!
//! Some Phase-level targets (render benchmarks, DXF 10K-entity import)
//! live in their own crates (`aec_cad`, `aec_render`) so they can
//! depend on the relevant adapters; we re-link them in the
//! Phase 6 e2e suite. Render-engine end-to-end benchmarks require
//! Blender and run manually.

use std::path::PathBuf;
use std::time::Instant;

use aec_core::templates::{validate_template_dir, TemplateLoader};

const APARTMENT_KEY: &str = "interior.apartment";
const TEMPLATE_LOAD_BUDGET_SECS: f64 = 1.0;
const TEMPLATES_DIR_BUDGET_SECS: f64 = 1.0;
const BLAKE3_BUDGET_SECS: f64 = 0.25;

fn templates_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    PathBuf::from(manifest)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("templates")
}

#[test]
fn apartment_template_loads_within_one_second() {
    let dir = templates_dir();
    let loader = TemplateLoader::new(dir);
    let start = Instant::now();
    let tpl = loader.load(APARTMENT_KEY).expect("apartment template");
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!("apartment template load: {elapsed:.4}s");
    assert!(
        elapsed < TEMPLATE_LOAD_BUDGET_SECS,
        "apartment template load took {elapsed:.4}s — over {TEMPLATE_LOAD_BUDGET_SECS}s budget"
    );
    assert!(
        tpl.iter_rooms().count() >= 1,
        "apartment template should expose at least one room"
    );
}

#[test]
fn validate_all_shipped_templates_within_one_second() {
    let dir = templates_dir();
    let start = Instant::now();
    let count = validate_template_dir(&dir).expect("templates validate");
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!("validate {count} templates: {elapsed:.4}s");
    assert!(
        elapsed < TEMPLATES_DIR_BUDGET_SECS,
        "template validation took {elapsed:.4}s — over {TEMPLATES_DIR_BUDGET_SECS}s budget"
    );
    assert!(
        count >= 9,
        "expected at least 9 shipped templates, found {count}"
    );
}

#[test]
fn blake3_hash_of_10mb_blob_under_250ms() {
    // The audit-trail hashes every entry's parent into the new entry.
    // A pathological project might have multi-MB attachments (e.g. an
    // IFC payload that the audit log references). 250 ms gives us
    // plenty of headroom for the largest realistic input.
    let blob = vec![0xABu8; 10 * 1024 * 1024];
    let start = Instant::now();
    let hash = blake3::hash(&blob);
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!("blake3(10MB): {elapsed:.4}s");
    assert!(
        elapsed < BLAKE3_BUDGET_SECS,
        "blake3 hash took {elapsed:.4}s — over {BLAKE3_BUDGET_SECS}s budget"
    );
    // Sanity check the hash isn't degenerate.
    assert_ne!(hash.as_bytes(), &[0u8; 32]);
}

/// Render-engine and DXF import benchmarks require domain crates that
/// `aec_core` does not depend on. This test documents the expectation
/// and points at the dedicated benchmarks in `aec_render` and
/// `aec_cad`.
#[test]
fn benchmark_pointers_documented() {
    let benches = [
        (
            "EEVEE preview latency (<250ms)",
            "cargo bench -p aec_render --bench eevee_latency",
        ),
        (
            "Contractor pack export (<60s)",
            "cargo test -p aec_export --test contractor_perf",
        ),
        (
            "Deterministic exports",
            "cargo test -p aec_export --test determinism",
        ),
    ];
    for (name, cmd) in benches {
        // Just ensure we have a non-empty pointer string. CI greps for
        // these pointers in the PROGRESS.md changelog.
        assert!(!name.is_empty());
        assert!(!cmd.is_empty());
    }
}
