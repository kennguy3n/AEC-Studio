# Real-world IFC4 test fixtures — attribution

This directory holds IFC files **authored by third parties** and
re-distributed under their original licences for the sole purpose of
regression-testing AEC Studio's IFC reader, writer, and `bim_attach`
pipeline against real authoring-tool output. They are **not**
project-template files and are not shipped with the desktop app.

If you add a new fixture here, append an entry below documenting:
provenance (upstream repo / URL / commit-or-snapshot date), file name,
licence + attribution string verbatim, and what real-world quirk(s)
the fixture exercises that the synthesised in-test fixtures
(`build_tiny_project`, `build_project_with_materials`, etc.) don't
cover. Keep new fixtures small — the soft cap is 100 KB; the file
should be representative, not exhaustive.

---

## `wall-with-opening-and-window.ifc`

* **Provenance**: <https://github.com/buildingSMART/Sample-Test-Files>,
  directory `IFC 4.0.2.1 (IFC 4)/ISO Spec - ReferenceView_V1.2/`.
  Snapshot fetched 2026-05-20 from the upstream `main` branch.
* **File size**: 12,492 bytes.
* **Upstream copyright**: © buildingSMART International Ltd.
* **Licence**: [Creative Commons Attribution 4.0 International
  (CC BY 4.0)](https://creativecommons.org/licenses/by/4.0/).
  Full licence text:
  <https://creativecommons.org/licenses/by/4.0/legalcode.txt>.
* **Why this fixture**: it is one of the official ISO 16739-1
  Reference View V1.2 exemplars — a single wall hosting an opening
  filled by a window, with material-layer-set usage. Exercises
  IFC4 features the synthesised fixtures don't naturally cover:
  - `IfcMaterialLayerSetUsage` with all three orientation fields
    populated (`LayerSetDirection = .AXIS2.`,
    `DirectionSense = .POSITIVE.`, `OffsetFromReferenceLine = -0.1`) —
    pins the synthetic-Pset round-trip path landed in PR-L.
  - `IfcMaterialConstituentSet` — the *other* material-assignment
    shape, used by the window. Distinct from the wall's
    `IfcMaterialLayerSetUsage` path.
  - `IfcRelDefinesByType` linking `IfcWindow` to `IfcWindowType`,
    with a type-Pset on the type that propagates to the instance
    on read.
  - `IfcRelDeclares` from an `IfcProjectLibrary` — an advanced IFC4
    "library" mechanism the synthesised fixtures don't touch.
  - `IfcOpeningElement` + `IfcRelVoidsElement` +
    `IfcRelFillsElement` — the wall/opening/window triplet.
  - `IfcThermalTransmittanceMeasure` +
    `IfcVolumetricFlowRateMeasure` — IFC4 measure types AEC Studio
    doesn't model natively, so they exercise the
    `PropertyValue::Other` verbatim-preservation channel.
  - `IfcConversionBasedUnit` (degree, derived from radian) — a unit
    indirection most simpler fixtures skip.

  The integration test
  `bim_attach_real_world_wall_with_opening_and_window` (in
  `crates/aec_bridge/tests/bim_attach_real_world.rs`) asserts the
  reader recovers the spatial hierarchy + material assignments +
  type-Pset propagation correctly, and that `bim_attach_ifc` folds
  the fixture's entities into the project DB without errors.

Per CC BY 4.0 §3(a)(1), if you redistribute this file unmodified,
the attribution string above must accompany it.
