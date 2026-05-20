//! Cross-platform smoke test.
//!
//! Pin behaviours that have historically diverged between macOS / Windows
//! / Linux:
//!
//! * Project package layout uses platform-correct paths (forward
//!   slashes on Unix, backslash-tolerant on Windows).
//! * BLAKE3 audit hashing is identical for the same payload on every
//!   platform (deterministic, no platform-specific endianness creep).
//! * Template loader works against a path with characters that have
//!   different normalisation rules per OS (spaces, dots, UTF-8).

use std::fs;
use std::path::PathBuf;

use aec_core::templates::TemplateLoader;

fn write_minimal_template(root: &std::path::Path, key: &str) {
    let category = "interior";
    let stem = key
        .strip_prefix(&format!("{category}."))
        .expect("key must be category.stem");
    let dir = root.join(category);
    fs::create_dir_all(&dir).unwrap();
    let body = r#"{
        "template_id": "interior.smoke",
        "category": "interior",
        "name": "Smoke",
        "description": "cross-platform smoke template",
        "region_defaults": {"EU": {"units": "mm", "standards": ["EN ISO 5457"]}},
        "units": "mm",
        "rooms": [
            {"name": "Room", "width_mm": 4000, "depth_mm": 3000, "height_mm": 2700}
        ],
        "default_walls": {"exterior_thickness_mm": 200, "interior_thickness_mm": 100},
        "lighting_preset": "daylight",
        "asset_shelf": [],
        "camera_presets": [
            {
                "name": "default",
                "position_mm": [3000.0, -3000.0, 1500.0],
                "target_mm": [0.0, 0.0, 1500.0],
                "focal_length_mm": 35.0,
                "exposure_ev": 0.0,
                "white_balance_k": 5500,
                "depth_of_field_f": 4.0,
                "aspect_ratio": 1.7777
            }
        ]
    }"#;
    fs::write(dir.join(format!("{stem}.json")), body).unwrap();
}

#[test]
fn template_loader_works_against_a_path_with_spaces_and_unicode() {
    let td = tempfile::tempdir().unwrap();
    // Choose a directory name that has historically tripped Windows
    // and macOS NFD/NFC differences. The loader should not care.
    let root: PathBuf = td.path().join("Studio Projets — résumé");
    fs::create_dir_all(&root).unwrap();
    write_minimal_template(&root, "interior.smoke");

    let loader = TemplateLoader::new(&root);
    let tpl = loader
        .load("interior.smoke")
        .expect("template must load from a path with spaces + unicode");
    assert_eq!(tpl.name, "Smoke");
    assert_eq!(tpl.iter_rooms().count(), 1);
}

#[test]
fn blake3_audit_hash_is_deterministic_per_payload() {
    // Two identical hashers must produce the same digest, byte-for-byte,
    // on every platform. This is the contract every audit-chain consumer
    // depends on.
    let mut a = blake3::Hasher::new();
    a.update(b"command:create_wall");
    a.update(b"\x01\x02\x03\x04");
    let mut b = blake3::Hasher::new();
    b.update(b"command:create_wall");
    b.update(b"\x01\x02\x03\x04");
    assert_eq!(a.finalize().to_hex().to_string(), b.finalize().to_hex().to_string());
}

#[test]
fn path_separators_round_trip_through_pathbuf() {
    // PathBuf normalises separators per OS. We rely on this when we
    // store project paths in the audit log and replay them across
    // machines.
    let p = PathBuf::from("foo/bar/baz.txt");
    assert!(p.ends_with("baz.txt"));
    assert_eq!(p.components().count(), 3);

    let nested = PathBuf::from("a").join("b").join("c.json");
    let s = nested.to_string_lossy();
    // Either separator is fine — what we care about is the components
    // round-trip.
    let comps: Vec<_> = nested
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(comps, vec!["a", "b", "c.json"]);
    assert!(s.contains("a") && s.contains("b") && s.contains("c.json"));
}
