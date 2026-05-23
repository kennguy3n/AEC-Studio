//! Shared test fixtures used across the LibreDWG oracle example, the
//! HANDSEED pinning test, and any future cross-version regression
//! tests that need the canonical 3-entity oracle document.
//!
//! Exposed as `pub` because the example binary in
//! `crates/aec_cad/examples/dwg_oracle_fixture.rs` lives outside the
//! crate's source tree and can only depend on items reachable from
//! the lib's public surface. Marked `#[doc(hidden)]` because external
//! consumers should NOT take a dependency on test-only helpers — the
//! geometry and entity count here are coupled to CI's
//! `ALLOW_HANDSEED` regex in `.github/workflows/ci.yml` and may
//! change in any release if the oracle fixture needs to grow new
//! coverage.

#![doc(hidden)]

use crate::dxf::{DxfCircle, DxfDocument, DxfEntity, DxfLine, DxfText};

/// The canonical LibreDWG oracle fixture: one LINE, one CIRCLE, one
/// TEXT, all on layer "0".
///
/// **Why three entities?** Three is the minimum that exercises every
/// active common-entity-data codepath in `DwgWriter` for the modern
/// versions (R14 → R2018):
///   * LINE — exercises 3RD-stream coordinate writes.
/// * CIRCLE — exercises BD radius + extrusion encoding.
///   * TEXT — exercises TV/T string dispatch (R2007+ uses T = UTF-16,
///     pre-R2007 uses TV = CP1252) and rotation/height fields.
///
/// **Why does the entity count matter?** `HANDSEED` is computed as
/// `max(handle) + 1` (see
/// `header_vars_for_records` in `crate::dwg::modern`). With 3 user
/// entities at handles 0x21 / 0x22 / 0x23, HANDSEED is `0x24`. CI's
/// `ALLOW_HANDSEED` regex in `.github/workflows/ci.yml` is hardcoded
/// to that exact decimal/hex pair (`36/0x24`). Changing the entity
/// count here MUST be accompanied by updates to:
///   1. `ALLOW_HANDSEED` in `.github/workflows/ci.yml` (the new
///      `<decimal>/0x<hex>` pair);
///   2. `EXPECTED_ORACLE_FIXTURE_HANDSEED` in
///      `crate::dwg::modern::tests::oracle_fixture_handseed_matches_ci_allow_list`.
///
/// The test pins the relationship in code: any change to this
/// function's entity count fails the test with a clear, actionable
/// message before it can land in CI.
pub fn oracle_fixture_doc() -> DxfDocument {
    let mut doc = DxfDocument::new();
    doc.push(DxfEntity::Line(DxfLine {
        layer: "0".into(),
        start: [0.0, 0.0, 0.0],
        end: [100.0, 50.0, 0.0],
    }));
    doc.push(DxfEntity::Circle(DxfCircle {
        layer: "0".into(),
        center: [50.0, 25.0, 0.0],
        radius: 12.5,
    }));
    doc.push(DxfEntity::Text(DxfText {
        layer: "0".into(),
        position: [10.0, 60.0, 0.0],
        height: 2.5,
        rotation: 0.0,
        text: "AEC-Studio".into(),
    }));
    doc
}
