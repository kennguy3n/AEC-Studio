//! Phase 11 Task 25 — IES profile parsing + sphere integration.
//!
//! Spec:
//! > Implement a real IES (IESNA LM-63) file parser that extracts the
//! > photometric web. Convert the photometric data to a lookup texture
//! > for the path tracer's light sampling.
//! > Test: parse a sample IES file → verify candela values → verify
//! > integration equals total lumens.
//!
//! This integration test reads a real LM-63-2002 IES file from disk
//! (`tests/fixtures/lambertian_downlight.ies`) — a Lambertian
//! cosine-distribution downlight with declared 3141.6 lumens. The
//! analytical sphere integral of `I₀ · cos(θ)` over the lower
//! hemisphere is exactly `π · I₀`. With `I₀ = 1000 cd` we expect
//! `π · 1000 ≈ 3141.59 lm`.

use std::path::PathBuf;

use aec_render::{IesLookupTexture, IesPhotometricType, IesProfile};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lambertian_downlight.ies")
}

#[test]
fn parses_real_ies_file_from_disk() {
    let p = IesProfile::load_ies(fixture_path()).unwrap();

    assert_eq!(p.lamp_count, 1);
    assert!(
        (p.lumens_per_lamp - 3141.6).abs() < 0.1,
        "declared lumens should be 3141.6, got {}",
        p.lumens_per_lamp
    );
    assert_eq!(p.candela_multiplier, 1.0);
    assert_eq!(p.vertical_angles.len(), 7);
    assert_eq!(p.horizontal_angles.len(), 1);
    assert_eq!(p.photometric_type, IesPhotometricType::C);
    assert!((p.peak_candela() - 1000.0).abs() < 1e-3);
    assert_eq!(p.declared_lumens(), Some(3141.6));
}

#[test]
fn candela_at_returns_table_values_at_sampled_angles() {
    let p = IesProfile::load_ies(fixture_path()).unwrap();
    assert!((p.candela_at(0.0, 0.0) - 1000.0).abs() < 1e-3);
    assert!((p.candela_at(15.0, 0.0) - 965.93).abs() < 1e-3);
    assert!((p.candela_at(30.0, 0.0) - 866.03).abs() < 1e-3);
    assert!((p.candela_at(45.0, 0.0) - 707.11).abs() < 1e-3);
    assert!((p.candela_at(60.0, 0.0) - 500.0).abs() < 1e-3);
    assert!((p.candela_at(75.0, 0.0) - 258.82).abs() < 1e-3);
    assert!((p.candela_at(90.0, 0.0) - 0.0).abs() < 1e-3);
}

#[test]
fn candela_at_interpolates_between_samples() {
    let p = IesProfile::load_ies(fixture_path()).unwrap();
    // Midway between θ=30° (866.03) and θ=45° (707.11) → linear avg 786.57.
    let half = p.candela_at(37.5, 0.0);
    let expected = (866.03 + 707.11) / 2.0;
    assert!(
        (half - expected).abs() < 1.0,
        "interpolated candela at 37.5° should be ~{expected}, got {half}"
    );
}

#[test]
fn sphere_integration_matches_declared_lumens() {
    // Spec acceptance: integration over the photometric sphere
    // must approximately equal the file's declared total lumens.
    // The Lambertian fixture is hand-tuned: analytical Φ = π · I₀
    // = π · 1000 ≈ 3141.59 lm. The declared value is 3141.6.
    let p = IesProfile::load_ies(fixture_path()).unwrap();
    let integrated = p.integrate_lumens();
    let declared = p.declared_lumens().expect("fixture declares lumens");
    let relative_error = (integrated - declared).abs() / declared;
    assert!(
        relative_error < 0.05,
        "integrated Φ = {integrated} lm vs declared {declared} lm (relative error {relative_error:.4})"
    );
}

#[test]
fn lookup_texture_can_be_baked_and_sampled() {
    let p = IesProfile::load_ies(fixture_path()).unwrap();
    let tex: IesLookupTexture = p.to_lookup_texture(128, 64);
    assert_eq!(tex.width, 128);
    assert_eq!(tex.height, 64);
    assert_eq!(tex.candela.len(), 128 * 64);

    // Sampling at the same angle the source file declares should
    // return ~1000 cd at θ=0 (texture (x=0, y=0)).
    let at_zero = tex.sample(0.0, 0.0);
    assert!(
        (at_zero - 1000.0).abs() < 1.0,
        "tex sample at θ=0 should be ~1000, got {at_zero}"
    );
    // At θ=90° (horizon) the Lambertian cosine vanishes. The 64-row
    // resolution means the nearest sampled rows straddle θ≈88.6° and
    // θ≈91.4°, so the bilinear interpolation still has ~25 cd at the
    // near sample. Tolerance loosened for that.
    let at_horizon = tex.sample(90.0, 0.0);
    assert!(
        at_horizon.abs() < 50.0,
        "tex sample at θ=90° should be small, got {at_horizon}"
    );
    // And at θ=89° the texture should be much smaller than the
    // peak.
    let near_horizon = tex.sample(89.0, 0.0);
    assert!(
        near_horizon < 100.0,
        "tex sample near horizon should be << peak, got {near_horizon}"
    );
    // Rotationally symmetric — horizontal angle should not matter.
    assert!((tex.sample(45.0, 90.0) - tex.sample(45.0, 270.0)).abs() < 1e-3);
}

#[test]
fn lookup_texture_handles_horizontal_wrap_around() {
    let p = IesProfile::load_ies(fixture_path()).unwrap();
    let tex = p.to_lookup_texture(32, 32);
    // Horizontal -90° wraps to 270°. Both should sample identically.
    let neg90 = tex.sample(45.0, -90.0);
    let pos270 = tex.sample(45.0, 270.0);
    assert!(
        (neg90 - pos270).abs() < 1e-3,
        "neg90={neg90} pos270={pos270}"
    );
}

#[test]
fn lookup_texture_low_resolution_still_preserves_peak() {
    let p = IesProfile::load_ies(fixture_path()).unwrap();
    // A 2x2 texture is the degenerate case; the corners must still
    // reflect the peak / nadir candela.
    let tex = p.to_lookup_texture(2, 2);
    assert_eq!(tex.candela.len(), 4);
    assert!((tex.candela[0] - 1000.0).abs() < 1.0); // (θ=0, φ=0)
                                                    // The y=1 row corresponds to θ=180°, which is outside the
                                                    // sampled range [0, 90°] — clamps to the boundary (=0 at θ=90).
    assert!(tex.candela[2].abs() < 1.0);
}
