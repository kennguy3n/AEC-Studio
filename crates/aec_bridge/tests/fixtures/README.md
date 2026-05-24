# `aec_bridge` Test Fixtures

This directory contains IFC and project-package test fixtures used by
the `aec_bridge` integration tests.

## `small_office.ifc`

A hand-authored, minimal-but-representative IFC4 file modelling a
two-storey office building. Used by the `bim_attach_*` integration
tests to exercise the full read → attach → re-read path on a file
with the same shape Revit and ArchiCAD produce.

The file covers:

* Spatial hierarchy: `IfcProject` → `IfcSite` → `IfcBuilding` →
  two `IfcBuildingStorey`s → one `IfcSpace` per storey.
* Five elements: two `IfcWall`s on the ground floor (bound via
  `IfcMaterialLayerSetUsage` with different per-wall offsets), one
  `IfcSlab`, one `IfcBeam`, one `IfcColumn`.
* A material library with four `IfcMaterial`s and one
  `IfcMaterialLayerSet` (three layers: 150 mm concrete core, 80 mm
  mineral wool, 20 mm gypsum board).
* Two `IfcMaterialLayerSetUsage` wrappers exercising different
  `OffsetFromReferenceLine` values (so the
  `AEC_LayerSetUsage` synthetic Pset round-trip is non-trivial).
* `Pset_WallCommon` (`LoadBearing` + `FireRating`) and
  `Qto_WallBaseQuantities` (`Length` + `NetArea`) attached to the
  two walls via `IfcRelDefinesByProperties`.

### License

This fixture was authored from scratch by the AEC Studio project. It
contains no derived content and is contributed to the public domain
under [CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/).
You may use, modify, and redistribute it for any purpose without
attribution.
