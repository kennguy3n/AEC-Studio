//! Cross-language pin for the built-in render preset ids.
//!
//! The TS in-process backends in `apps/desktop/electron/bridge.ts`
//! and `apps/desktop/renderer/src/api/renderer-backend.ts`
//! validate preset ids against a hand-maintained constant
//! (`BUILT_IN_PRESET_IDS`). Without a cross-language guard, adding
//! or renaming a preset on the Rust side would silently desync the
//! two surfaces: dev/Vitest would accept the new id but the in-
//! process backend would reject it, or vice versa. This test pins
//! both sides to a shared fixture so the failure mode is a single
//! `assert_eq!` mismatch in CI, with a clear next step (update the
//! fixture + the TS constant in the same commit).
//!
//! The fixture lives at `crates/aec_render/tests/preset_ids.json`
//! and is loaded both here (in this Rust integration test) and by
//! the Vitest test `built_in_preset_ids_match_native` in
//! `apps/desktop/renderer/src/__tests__/bridge-catalogue.test.ts`.
//! When the bundled-preset list changes, regenerate the fixture by
//! running this test in update mode:
//!
//! ```bash
//! AEC_UPDATE_PRESET_IDS=1 cargo test -p aec_render --test preset_ids
//! ```

use aec_render::preset::RenderPreset;
use std::fs;
use std::path::Path;

#[test]
fn built_in_preset_ids_match_fixture() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("preset_ids.json");

    // The fixture stores a *sorted* list so the test asserts on the
    // set of bundled preset ids without baking in the iteration
    // order of `RenderPreset::defaults()` or the TS-side
    // `BUILT_IN_PRESET_IDS` array. Both are free to reorder their
    // local listing as long as the set is identical.
    let mut actual: Vec<String> = RenderPreset::defaults().into_iter().map(|p| p.id).collect();
    actual.sort();
    let actual_json =
        serde_json::to_string_pretty(&actual).expect("preset id list must serialise") + "\n";

    if std::env::var_os("AEC_UPDATE_PRESET_IDS").is_some() {
        fs::write(&fixture_path, &actual_json)
            .expect("must be able to write fixture in update mode");
        return;
    }

    let expected = fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!(
            "could not read preset-ids fixture at {}: {e}. \
             Run with AEC_UPDATE_PRESET_IDS=1 to regenerate.",
            fixture_path.display()
        )
    });

    assert_eq!(
        actual_json, expected,
        "built-in render preset ids drifted from the cross-language fixture. \
         Run `AEC_UPDATE_PRESET_IDS=1 cargo test -p aec_render --test preset_ids` \
         to regenerate, then mirror the change in `BUILT_IN_PRESET_IDS` in \
         `apps/desktop/electron/bridge.ts`."
    );
}
