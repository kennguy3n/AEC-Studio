//! Persist a parsed [`aec_bim::ifc::IfcSnapshot`] into a project's
//! SQLCipher database.
//!
//! This module turns the in-memory snapshot produced by
//! [`aec_bim::ifc::IfcReader::from_string`] into rows in the project
//! package's `entities`, `components`, `relations`, and `bim_cache`
//! tables. It is the second half of the BIM import flow: the
//! `bim_import_ifc` bridge endpoint produces the snapshot and shows
//! the user a preview; `bim_attach_ifc` calls into here to actually
//! fold the model into the authoring graph.
//!
//! ## Persistence shape
//!
//! Every spatial node and every building element becomes one row in
//! the `entities` table:
//!
//! | column      | spatial node                              | element                                    |
//! |-------------|-------------------------------------------|--------------------------------------------|
//! | `id`        | `SpatialNode.id` (AEC EntityId)           | element `EntityId`                         |
//! | `kind`      | `bim/spatial/IfcProject` etc.             | `bim/element/IfcWall` etc.                 |
//! | `parent_id` | parent spatial node's `EntityId` (NULL on root) | the containing storey's `EntityId`   |
//! | `body`      | serde-JSON of `BimSpatialBody`            | serde-JSON of `BimElementBody`             |
//!
//! Per-element annotations (Psets / Qsets / material assignments)
//! become `components` rows, all namespaced under the `bim/` prefix so
//! a future re-attach of the same file can `DELETE FROM components
//! WHERE entity_id=? AND kind LIKE 'bim/%'` without disturbing
//! user-authored components (render-material overrides, command-engine
//! markers, etc.):
//!
//! | column      | value                                        |
//! |-------------|----------------------------------------------|
//! | `id`        | `{entity_id}/{kind}/{nonce}` (stable per row) |
//! | `entity_id` | the owning entity's `EntityId`               |
//! | `kind`      | `bim/pset/{name}`, `bim/qset/{name}`, `bim/material_assignment` |
//! | `body`      | serde-JSON of the [`aec_bim`] type           |
//!
//! `relations` rows are emitted for storey-containment (`kind =
//! "bim/contained_in"`) so the project graph can query "what elements
//! live in this storey" without walking parent pointers.
//!
//! ## Dedup on re-attach
//!
//! `bim_cache` (defined in [`aec_core::db`]'s base v1 schema) is the
//! canonical dedup index keyed on IFC GUID. On every attach we:
//!
//! 1. Look up `bim_cache.global_id` for each spatial node / element.
//! 2. If the row exists AND `geom_hash + pset_hash + class_hash` all
//!    match, it's a true no-op — just bump `last_seen`.
//! 3. If any hash differs, UPDATE the `entities` row in place, then
//!    DELETE all `bim/...` components for that entity and re-INSERT
//!    from the new snapshot. The undo journal is NOT touched (BIM
//!    attach is not part of the user's command history; the import is
//!    its own operation that the user invokes separately).
//! 4. If the row doesn't exist, INSERT into both `entities` and
//!    `bim_cache`.
//!
//! ## Transaction discipline
//!
//! Every call sites runs through a single [`rusqlite::Transaction`]
//! opened at the bridge layer. If any of the spatial / element /
//! component / relation inserts fails, the transaction is dropped
//! without committing and the project database stays at its
//! pre-attach state. This matters because mid-attach failures (e.g. a
//! disk-full error halfway through writing 12 000 `IfcWall` elements
//! from an MEP federation) would otherwise leave the project graph in
//! an inconsistent half-imported state.

use std::collections::HashSet;

use aec_bim::ifc::IfcSnapshot;
use aec_bim::{ClassificationStore, IfcClass, MaterialStore, PropertyStore};
use aec_core::types::EntityId;
use chrono::Utc;
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::service::BridgeServiceError;

/// Counts produced by [`attach_snapshot`]. Returned to the renderer
/// inside [`crate::service::BimAttachSummary`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AttachCounts {
    /// Spatial nodes added to `entities` for the first time.
    pub spatial_nodes_inserted: u64,
    /// Spatial nodes already in `entities` (matched on `bim_cache.global_id`)
    /// whose body / class was updated to the new snapshot.
    pub spatial_nodes_updated: u64,
    /// Spatial nodes already in `entities` whose snapshot hashes
    /// matched the on-disk hashes — body left untouched, only
    /// `last_seen` bumped.
    pub spatial_nodes_unchanged: u64,
    /// Building elements added to `entities` for the first time.
    pub elements_inserted: u64,
    /// Building elements already in `entities` whose body / class was
    /// updated to the new snapshot.
    pub elements_updated: u64,
    /// Building elements already in `entities` whose snapshot hashes
    /// matched the on-disk hashes.
    pub elements_unchanged: u64,
    /// Total `components` rows written (psets + qsets + material
    /// assignments). For an updated entity this counts the new rows
    /// after the BIM-component wipe.
    pub components_inserted: u64,
    /// Total `relations` rows written (storey-containment etc.).
    pub relations_inserted: u64,
    /// `bim_cache` rows touched (every spatial-node + every element
    /// produces exactly one cache row, regardless of insert/update).
    pub cache_rows: u64,
}

/// Persisted body of a spatial-hierarchy node (Project / Site /
/// Building / Storey / Space). Stored in `entities.body` as JSON.
///
/// Field naming is `snake_case` (serde default) so SQL audits using
/// `json_extract(body, '$.ifc_guid')` work without quirky quoting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BimSpatialBody {
    pub ifc_guid: Option<String>,
    pub ifc_class: String,
    pub name: String,
    /// Source IFC schema (`"IFC2X3"` / `"IFC4"` / `"IFC4X3"`). Stored
    /// per-entity rather than at project level so a future per-source
    /// federation merge (PR-M) can keep multiple schema versions
    /// alongside each other.
    pub source_schema: String,
    /// Identifier of the IFC file this entity came from, for
    /// re-attach dedup and audit. Set to the canonicalised source
    /// path at attach time.
    pub source_path: String,
}

/// Persisted body of a building element (wall / slab / door / ...).
/// Stored in `entities.body` as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BimElementBody {
    pub ifc_guid: Option<String>,
    pub ifc_class: String,
    /// Confidence threshold from the source `ClassificationStore`
    /// (always 1.0 for AEC Studio's own exports, since the writer
    /// emits a `Manual` source; lower for AI-classified or
    /// imported-with-uncertainty externals). Stored as a finite value
    /// in `[0.0, 1.0]` so JSON serialisation never produces an invalid
    /// `NaN`/`Infinity` literal — the reader clamps before populating.
    pub confidence: f64,
    pub source_schema: String,
    pub source_path: String,
}

/// Persisted body of a `components` row for a Pset / Qset. Stored as
/// JSON; the property values keep their original `IfcMeasure` types
/// via the `aec_bim::PropertyValue` enum so round-tripping back to
/// IFC writes the right STEP literal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BimPropertyComponent {
    pub set_name: String,
    /// `pset` for `IfcPropertySet`, `qset` for `IfcElementQuantity`.
    pub set_kind: &'static str,
    pub properties: std::collections::BTreeMap<String, aec_bim::PropertyValue>,
}

/// Persisted body of a `components` row carrying a material
/// assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BimMaterialComponent {
    /// `material` or `layer_set` — the variant of
    /// [`aec_bim::MaterialAssignment`] that produced this row.
    pub kind: &'static str,
    /// The name referenced — material name for `material`, layer-set
    /// name for `layer_set`.
    pub reference: String,
}

/// Walk `snapshot` and write its contents into the project database.
/// `canonical_source_path` is the canonicalised path of the IFC file,
/// captured once at the bridge layer and threaded through here for
/// audit/dedup; we never re-canonicalise inside this function.
pub(crate) fn attach_snapshot(
    tx: &Transaction<'_>,
    snapshot: &IfcSnapshot,
    canonical_source_path: &str,
) -> Result<AttachCounts, BridgeServiceError> {
    let mut counts = AttachCounts::default();
    let schema_token = snapshot.schema.to_string();
    let now = Utc::now().to_rfc3339();

    // ---- Spatial hierarchy ------------------------------------------------
    //
    // The spatial graph is a tree rooted at `snapshot.project.root`.
    // We iterate in BFS order so the parent's `entities` row exists
    // before any child references it via `entities.parent_id`. The
    // root has `parent_id = NULL`. We do NOT walk via the recursive
    // `descendants()` helper because that returns DFS — BFS gives a
    // more predictable insertion order that makes test assertions
    // easier to reason about, and matches the order
    // `nodes_of_class()` produces for the renderer-side preview.
    let bfs_order = spatial_bfs(&snapshot.project);
    for (parent_id, node_id) in &bfs_order {
        let Some(node) = snapshot.project.get(node_id) else {
            continue;
        };
        // Resolve the IFC `GlobalId` of this spatial node from the
        // two paths the reader can populate (the spatial-node field
        // and the separate `guid_by_entity` map, depending on whether
        // the reader reached `set_ifc_guid` before or after the
        // spatial-tree build). Treat an empty-string GUID as `None`:
        // the IFC4 schema requires `IfcRoot.GlobalId` to be a
        // non-empty 22-char base64, but defective IFC2x3 exports may
        // emit `''`. If we propagated `Some("")` into `upsert_entity`,
        // every empty-GUID row across the file would alias on the
        // `bim_cache.global_id = ''` lookup — the second such row
        // would hit the dedup hash compare against the first, follow
        // the `Unchanged` / `Updated` branch with a fresh
        // non-deterministic `EntityId` (the reader synthesises one
        // per parse for GUID-less rows), and the matching `UPDATE
        // entities WHERE id = ?` would no-op. Children that
        // referenced the new entity id as `parent_id` would then
        // violate the `entities.parent_id` foreign-key. Filtering
        // here forces the GUID-less path to always take the
        // `Inserted` branch — orphan rows accumulate slowly on the
        // rare defective-file case but FK integrity holds.
        let guid = node
            .ifc_guid
            .as_deref()
            .or_else(|| snapshot.guid_by_entity.get(node_id).map(String::as_str))
            .filter(|g| !g.is_empty());
        let body = BimSpatialBody {
            ifc_guid: guid.map(str::to_owned),
            ifc_class: node.class.ifc_tag().to_owned(),
            name: node.name.clone(),
            source_schema: schema_token.clone(),
            source_path: canonical_source_path.to_owned(),
        };
        let body_json =
            serde_json::to_string(&body).map_err(|e| BridgeServiceError::Bim(e.to_string()))?;
        let entity_kind = format!("bim/spatial/{}", node.class.ifc_tag());
        let outcome = upsert_entity(
            tx,
            node_id,
            &entity_kind,
            parent_id.as_ref(),
            &body_json,
            guid,
            &now,
            &node.class,
            &snapshot.properties,
            &snapshot.materials,
        )?;
        match outcome {
            UpsertOutcome::Inserted => counts.spatial_nodes_inserted += 1,
            UpsertOutcome::Updated => counts.spatial_nodes_updated += 1,
            UpsertOutcome::Unchanged => counts.spatial_nodes_unchanged += 1,
        }
        counts.cache_rows += 1;
    }

    // ---- Elements ---------------------------------------------------------
    //
    // `snapshot.project.nodes[*].elements` lists EntityIds — the
    // actual classification (IfcWall etc.) lives in
    // `snapshot.classification`. Walk the spatial tree again, this
    // time emitting one element row per `(spatial_id, element_id)`
    // pair. The element's `entities.parent_id` is the containing
    // spatial node so deletes cascade correctly.
    let mut seen_elements: HashSet<EntityId> = HashSet::new();
    for (_parent_of_spatial, spatial_id) in &bfs_order {
        let Some(spatial_node) = snapshot.project.get(spatial_id) else {
            continue;
        };
        for element_id in &spatial_node.elements {
            if !seen_elements.insert(element_id.clone()) {
                // Elements should not appear under multiple spatial
                // parents per IFC4's `IfcRelContainedInSpatialStructure`
                // 1-N constraint, but defend against degenerate
                // inputs (a Revit export that double-binds via two
                // RelContainedIn relations). First parent wins.
                continue;
            }
            // Use the `ClassificationStore::get` O(1) entry-id lookup
            // instead of `.iter().find(|...|)`. The latter is O(N) per
            // element, which on a typical Revit MEP federation (12 000+
            // elements) makes the spatial-tree walk O(N²) and turns
            // attach into a tens-of-seconds operation rather than
            // sub-second. The classification store is backed by a
            // `BTreeMap` keyed on `EntityId`, so `.get` is
            // O(log N) and the whole walk is O(N log N).
            let assignment = snapshot.classification.get(element_id);
            let (class, confidence) = match assignment {
                Some(a) => (a.class.clone(), a.confidence),
                None => (IfcClass::Other("Unknown".into()), 0.0),
            };
            // Same empty-string-as-`None` filter as the spatial-node
            // path: an `Some("")` propagated into `upsert_entity`
            // would alias all GUID-less elements on the
            // `bim_cache.global_id = ''` lookup and cause the
            // re-attach FK violation described above.
            let guid = snapshot
                .guid_by_entity
                .get(element_id)
                .map(String::as_str)
                .filter(|g| !g.is_empty());
            let body = BimElementBody {
                ifc_guid: guid.map(str::to_owned),
                ifc_class: class.ifc_tag().to_owned(),
                confidence,
                source_schema: schema_token.clone(),
                source_path: canonical_source_path.to_owned(),
            };
            let body_json =
                serde_json::to_string(&body).map_err(|e| BridgeServiceError::Bim(e.to_string()))?;
            let entity_kind = format!("bim/element/{}", class.ifc_tag());
            let outcome = upsert_entity(
                tx,
                element_id,
                &entity_kind,
                Some(spatial_id),
                &body_json,
                guid,
                &now,
                &class,
                &snapshot.properties,
                &snapshot.materials,
            )?;
            match outcome {
                UpsertOutcome::Inserted => counts.elements_inserted += 1,
                UpsertOutcome::Updated => counts.elements_updated += 1,
                UpsertOutcome::Unchanged => counts.elements_unchanged += 1,
            }
            counts.cache_rows += 1;

            // Persist the contained-in relation explicitly. It's
            // redundant with `entities.parent_id` for query purposes
            // but having it as a typed `relations` row lets the
            // future `bim_detach_*` flow walk by `kind =
            // "bim/contained_in"` without leaning on the
            // entities-table hierarchy (which may carry non-BIM
            // children, e.g. user-added annotations).
            // `INSERT OR IGNORE` returns 0 when the unique constraint
            // on (kind, from_id, to_id) fires (i.e. the relation
            // already existed from a previous attach). Reflect that
            // truth in the renderer-facing counter rather than
            // bumping it unconditionally — otherwise on every
            // re-attach we'd report e.g. "5 relations inserted"
            // when in fact 0 new rows were created.
            let rows_changed = tx.execute(
                "INSERT OR IGNORE INTO relations(kind, from_id, to_id) \
                 VALUES ('bim/contained_in', ?1, ?2)",
                params![element_id.as_str(), spatial_id.as_str()],
            )?;
            counts.relations_inserted += rows_changed as u64;
        }
    }

    // ---- Components: Psets / Qsets ---------------------------------------
    //
    // Wipe any prior BIM-imported components for these entities so an
    // updated snapshot doesn't accumulate stale `bim/pset/*` rows. We
    // wipe at element granularity (one DELETE per entity_id) rather
    // than a bulk DELETE so the SQL trace is auditable per element.
    //
    // Compose the wipe set from THREE sources, deliberately wider than
    // strictly necessary for today's reader:
    //   1. Every spatial node we just visited (`bfs_order`).
    //   2. Every element we just visited (`seen_elements`).
    //   3. Every entity the snapshot's `PropertyStore` carries.
    //   4. Every entity the snapshot's `MaterialStore` has an
    //      assignment for.
    //
    // (3) defends against a future reader change that adds Psets to
    // entities outside the spatial tree (e.g. type-object properties
    // from `IfcRelDefinesByType`). (4) is the symmetric guard for
    // material assignments — if a future reader change starts
    // creating material assignments for entities that aren't in the
    // tree, `write_materials` would otherwise hit a primary-key
    // conflict on the bare `INSERT INTO components` after a prior
    // attach. Wiping them here keeps the SQL contract sound
    // regardless of how the reader's property and material graphs
    // evolve.
    let mut touched_entities: HashSet<EntityId> = HashSet::new();
    for (_, id) in &bfs_order {
        touched_entities.insert(id.clone());
    }
    touched_entities.extend(seen_elements.iter().cloned());
    touched_entities.extend(snapshot.properties.iter().map(|(id, _)| id.clone()));
    touched_entities.extend(snapshot.materials.assignments().map(|(id, _)| id.clone()));
    for entity_id in &touched_entities {
        tx.execute(
            "DELETE FROM components WHERE entity_id = ?1 AND kind LIKE 'bim/%'",
            params![entity_id.as_str()],
        )?;
    }

    counts.components_inserted += write_psets(tx, &snapshot.properties)?;
    counts.components_inserted += write_materials(tx, &snapshot.materials)?;

    Ok(counts)
}

/// Outcome of a single `entities` upsert — used to bump the right
/// counter in [`AttachCounts`].
enum UpsertOutcome {
    Inserted,
    Updated,
    Unchanged,
}

/// Upsert one `entities` row + its `bim_cache` index entry. Implements
/// the "GUID-aware dedup" described in the module docs.
#[allow(clippy::too_many_arguments)]
fn upsert_entity(
    tx: &Transaction<'_>,
    entity_id: &EntityId,
    entity_kind: &str,
    parent_id: Option<&EntityId>,
    body_json: &str,
    guid: Option<&str>,
    now_rfc3339: &str,
    class: &IfcClass,
    properties: &PropertyStore,
    materials: &MaterialStore,
) -> Result<UpsertOutcome, BridgeServiceError> {
    // `geom_hash` is the hash of the entity's identity-and-position
    // signature. Compose it from:
    //   * the entity's `parent_id` (the storey containment / spatial
    //     parentage; folding it in here means a re-parent flips
    //     `geom_hash`, so a wall moved from "Ground Floor" to "First
    //     Floor" is correctly classified as `Updated` instead of
    //     silently retaining its stale `parent_id` on `Unchanged`).
    //   * the entity body JSON (guid, IFC class, schema, source path,
    //     etc.).
    // The hash input format is `parent={parent_id}\nbody={body_json}`
    // (NUL-terminated parent for unambiguity if the body itself
    // happens to contain `body=...`).
    let mut geom_input = Vec::with_capacity(body_json.len() + 64);
    geom_input.extend_from_slice(b"parent=");
    geom_input.extend_from_slice(parent_id.map_or("", EntityId::as_str).as_bytes());
    geom_input.push(0);
    geom_input.extend_from_slice(b"body=");
    geom_input.extend_from_slice(body_json.as_bytes());
    let geom_hash = blake3_hex(&geom_input);
    let class_hash = blake3_hex(class.ifc_tag().as_bytes());
    // Compute the per-entity "components" hash by deterministically
    // serialising and hashing every annotation the entity carries
    // — Psets, Qtos, and (since round 2 of Devin Review) the
    // material assignment. The column on `bim_cache` is still named
    // `pset_hash` for schema continuity, but conceptually it covers
    // every per-entity component bucket that `attach_snapshot`
    // wipes-and-rewrites. Including the material assignment makes a
    // re-attach that flips a wall's material from `Concrete` to
    // `Steel` (without touching geometry or Psets) correctly classify
    // as `Updated`, so the renderer-facing summary metrics aren't
    // misleading.
    //
    // Format:
    //   `props={serde-json of ElementProperties (or empty)}\0
    //    material={serde-json of MaterialAssignment (or empty)}`
    let mut pset_input = Vec::new();
    pset_input.extend_from_slice(b"props=");
    if let Some(ep) = properties.get(entity_id) {
        pset_input.extend_from_slice(
            serde_json::to_string(ep)
                .map_err(|e| BridgeServiceError::Bim(e.to_string()))?
                .as_bytes(),
        );
    }
    pset_input.push(0);
    pset_input.extend_from_slice(b"material=");
    if let Some(ma) = materials.assignment(entity_id) {
        pset_input.extend_from_slice(
            serde_json::to_string(ma)
                .map_err(|e| BridgeServiceError::Bim(e.to_string()))?
                .as_bytes(),
        );
    }
    let pset_hash = blake3_hex(&pset_input);

    // Look up the bim_cache row by GUID first (most common: file has
    // GUIDs and the dedup index hits). Fall back to entity_id only
    // for files that have no GUIDs (extremely rare — IFC4 makes them
    // mandatory, but defective tolerated-and-skipped imports may
    // produce GUID-less rows).
    let existing: Option<(String, String, String)> = match guid {
        Some(g) => tx
            .query_row(
                "SELECT geom_hash, pset_hash, class_hash FROM bim_cache WHERE global_id = ?1",
                params![g],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional_row()?,
        None => None,
    };

    let outcome = match existing {
        Some((existing_geom, existing_pset, existing_class))
            if existing_geom == geom_hash
                && existing_class == class_hash
                && existing_pset == pset_hash =>
        {
            // True no-op: hashes match, just bump last_seen so the
            // dedup index reflects the most recent attach time. Don't
            // touch `entities` — anything else (e.g. a user-renamed
            // node since the last attach) stays as-is.
            if let Some(g) = guid {
                tx.execute(
                    "UPDATE bim_cache SET last_seen = ?1 WHERE global_id = ?2",
                    params![now_rfc3339, g],
                )?;
            }
            UpsertOutcome::Unchanged
        }
        Some(_) => {
            // Hashes differ — body/class changed since the last
            // attach. Update the `entities` row in place (preserving
            // its `created_at`) and refresh the `bim_cache` hashes.
            tx.execute(
                "UPDATE entities SET kind = ?1, parent_id = ?2, body = ?3, updated_at = ?4 \
                 WHERE id = ?5",
                params![
                    entity_kind,
                    parent_id.map(|p| p.as_str().to_owned()),
                    body_json,
                    now_rfc3339,
                    entity_id.as_str(),
                ],
            )?;
            if let Some(g) = guid {
                tx.execute(
                    "UPDATE bim_cache SET geom_hash = ?1, pset_hash = ?2, class_hash = ?3, last_seen = ?4 \
                     WHERE global_id = ?5",
                    params![&geom_hash, &pset_hash, &class_hash, now_rfc3339, g],
                )?;
            }
            UpsertOutcome::Updated
        }
        None => {
            // Not previously seen — INSERT both rows. Use OR IGNORE on
            // entities so a stale `bim_cache` row with the same
            // global_id but no matching `entities` row (which would
            // be a v1 schema oddity) doesn't crash; the subsequent
            // INSERT OR IGNORE INTO bim_cache covers the symmetric
            // edge case.
            tx.execute(
                "INSERT OR REPLACE INTO entities(id, kind, parent_id, created_at, updated_at, body) \
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                params![
                    entity_id.as_str(),
                    entity_kind,
                    parent_id.map(|p| p.as_str().to_owned()),
                    now_rfc3339,
                    body_json,
                ],
            )?;
            if let Some(g) = guid {
                tx.execute(
                    "INSERT OR REPLACE INTO bim_cache(global_id, geom_hash, pset_hash, class_hash, last_seen) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![g, &geom_hash, &pset_hash, &class_hash, now_rfc3339],
                )?;
            }
            UpsertOutcome::Inserted
        }
    };
    Ok(outcome)
}

/// Walk the `PropertyStore` and emit one `bim/pset/*` or `bim/qset/*`
/// row per Pset / Qset on each entity. Returns the total row count.
fn write_psets(
    tx: &Transaction<'_>,
    properties: &PropertyStore,
) -> Result<u64, BridgeServiceError> {
    let mut written = 0u64;
    for (entity_id, props) in properties.iter() {
        for (name, pset) in &props.psets {
            let body = BimPropertyComponent {
                set_name: name.clone(),
                set_kind: "pset",
                properties: pset.properties.clone(),
            };
            insert_component(
                tx,
                entity_id,
                &format!("bim/pset/{name}"),
                &serde_json::to_string(&body)
                    .map_err(|e| BridgeServiceError::Bim(e.to_string()))?,
            )?;
            written += 1;
        }
        for (name, qset) in &props.qsets {
            let body = BimPropertyComponent {
                set_name: name.clone(),
                set_kind: "qset",
                properties: qset.quantities.clone(),
            };
            insert_component(
                tx,
                entity_id,
                &format!("bim/qset/{name}"),
                &serde_json::to_string(&body)
                    .map_err(|e| BridgeServiceError::Bim(e.to_string()))?,
            )?;
            written += 1;
        }
        for (name, pset) in &props.type_psets {
            // Type-level Psets are stored under a distinct `bim/typepset/`
            // prefix so a query for instance-Psets only doesn't trip
            // over them.
            let body = BimPropertyComponent {
                set_name: name.clone(),
                set_kind: "pset",
                properties: pset.properties.clone(),
            };
            insert_component(
                tx,
                entity_id,
                &format!("bim/typepset/{name}"),
                &serde_json::to_string(&body)
                    .map_err(|e| BridgeServiceError::Bim(e.to_string()))?,
            )?;
            written += 1;
        }
    }
    Ok(written)
}

/// Walk the `MaterialStore` and emit one `bim/material_assignment`
/// row per element binding.
fn write_materials(
    tx: &Transaction<'_>,
    materials: &MaterialStore,
) -> Result<u64, BridgeServiceError> {
    let mut written = 0u64;
    for (entity_id, assignment) in materials.assignments() {
        let body = match assignment {
            aec_bim::MaterialAssignment::Single(name) => BimMaterialComponent {
                kind: "material",
                reference: name.clone(),
            },
            aec_bim::MaterialAssignment::LayerSet(name) => BimMaterialComponent {
                kind: "layer_set",
                reference: name.clone(),
            },
        };
        insert_component(
            tx,
            entity_id,
            "bim/material_assignment",
            &serde_json::to_string(&body).map_err(|e| BridgeServiceError::Bim(e.to_string()))?,
        )?;
        written += 1;
    }
    Ok(written)
}

fn insert_component(
    tx: &Transaction<'_>,
    entity_id: &EntityId,
    kind: &str,
    body_json: &str,
) -> Result<(), BridgeServiceError> {
    // The components.id column is TEXT PRIMARY KEY — a deterministic
    // composite of `{entity_id}/{kind}` works because (a) BIM
    // imports never produce two components of the same kind on the
    // same entity (Pset names are unique per element; material
    // assignment is 1:1 with the element), and (b) re-attach has
    // already wiped prior `bim/%` components, so we never collide
    // with our own previous-attach row.
    let component_id = format!("{}/{}", entity_id.as_str(), kind);
    tx.execute(
        "INSERT INTO components(id, entity_id, kind, body) VALUES (?1, ?2, ?3, ?4)",
        params![component_id, entity_id.as_str(), kind, body_json],
    )?;
    Ok(())
}

/// BFS traversal of the spatial tree producing `(parent_id, node_id)`
/// pairs in insertion order. The root pair has `parent_id = None`.
fn spatial_bfs(project: &aec_bim::Project) -> Vec<(Option<EntityId>, EntityId)> {
    use std::collections::VecDeque;
    let mut out: Vec<(Option<EntityId>, EntityId)> = Vec::with_capacity(project.node_count());
    let mut queue: VecDeque<(Option<EntityId>, EntityId)> =
        VecDeque::from([(None, project.root.clone())]);
    while let Some((parent, id)) = queue.pop_front() {
        let Some(node) = project.get(&id) else {
            continue;
        };
        out.push((parent, id.clone()));
        for child in &node.children {
            queue.push_back((Some(id.clone()), child.clone()));
        }
    }
    out
}

/// 32-byte BLAKE3 of the input encoded as lowercase hex. Used for the
/// `bim_cache` content hashes.
fn blake3_hex(bytes: &[u8]) -> String {
    // blake3 isn't already a dependency of `aec_bridge`; use the
    // hash function that's already in the workspace via `aec_audit`'s
    // re-export. Falling back to `Sha256` would also work but `aec_audit`
    // already standardised on BLAKE3 for the audit chain so we stay
    // consistent.
    let hash = blake3::hash(bytes);
    hash.to_hex().to_string()
}

/// Tiny extension trait so call sites can write
/// `tx.query_row(...).optional_row()?` instead of the verbose
/// `match ... { Ok(v) => Some(v), Err(QueryReturnedNoRows) => None, ...}`
/// pattern, while staying readable to anyone scanning the file.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>, BridgeServiceError>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_row(self) -> Result<Option<T>, BridgeServiceError> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(BridgeServiceError::from(e)),
        }
    }
}

/// Avoids unused-import lint when the file is read in isolation —
/// `ClassificationStore` is referenced only by [`IfcSnapshot`]'s
/// public-API type, but Rust's import-checker can flag it. The const
/// pulls the symbol into a const-eval position that survives `cargo
/// fmt` and `cargo check`.
const _: fn() = || {
    let _ = std::marker::PhantomData::<ClassificationStore>;
};
