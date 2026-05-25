//! v3 → v4 migration: promote `(entity_id, kind)` to a uniqueness
//! invariant on the `components` table.
//!
//! Background. The base v1 schema declares `components.id TEXT
//! PRIMARY KEY` and lets every caller invent its own `id` scheme.
//! Three callers exist today, each with a different deterministic id:
//!
//! - `crates/aec_bridge/src/service.rs::upsert_component`
//!   uses `comp_{entity_id}_{kind_with_slashes_replaced_by_underscore}`
//!   for `aec/property/*` and `aec/classification/*` rows
//!   (`bim_set_property` and `bim_classify`).
//! - `crates/aec_bridge/src/bim_attach.rs::write_component`
//!   uses `{entity_id}/{kind}` for `bim/*` rows (IFC pset / qto /
//!   material assignments).
//! - Future code paths could plausibly use yet another scheme.
//!
//! The natural key for a component row is `(entity_id, kind)`: for a
//! given entity, there can only be one row of a given kind. Today
//! each caller's id scheme is internally consistent, so the
//! deterministic id transparently implements the natural-key
//! invariant. But because the invariant is implicit, two callers
//! with DIFFERENT id schemes pointing at the SAME `(entity_id, kind)`
//! would silently coexist as two rows — the read path (`SELECT body
//! FROM components WHERE entity_id = ?1 AND kind = ?2`) would
//! arbitrarily pick one of them, and `bim_set_property`'s "read
//! previous value" → "merge" → "write back" pipeline could quietly
//! drop user data.
//!
//! Reported by Devin Review PR-W round 2 as a latent fragility.
//! Adding a `UNIQUE` index on `(entity_id, kind)` promotes the
//! implicit invariant to a database-enforced one: SQLite will reject
//! the duplicate `INSERT` at the boundary rather than silently
//! creating a second row. Every existing upsert call is also
//! converted to use `ON CONFLICT(entity_id, kind)` so the natural
//! key (not the surrogate `id`) is what governs upsert semantics.
//!
//! Migration safety:
//!
//! - No existing code path produces a duplicate `(entity_id, kind)`
//!   pair across schemes, because the three callers use disjoint
//!   `kind` prefixes (`aec/property/`, `aec/classification/`,
//!   `bim/`). The `CREATE UNIQUE INDEX` therefore succeeds on every
//!   legitimate database. If it fails, the project DB has been
//!   corrupted by some out-of-band write path that this PR can't
//!   anticipate; failing the migration loudly is correct behavior
//!   per [`crate::migrations`]'s forward-only fail-loud philosophy.
//!
//! - Once the index exists, future readers can rely on `(entity_id,
//!   kind)` being unique without re-validating. The downside — a
//!   tiny per-row INSERT cost from index maintenance — is unmeasurable
//!   against SQLCipher's KDF and JSON serialisation overhead.
//!
//! - Forward-only: a v4 database opened by a binary that only knows
//!   v3 fails `ProjectManifest::validate` with `SchemaMismatch`.

use crate::migrations::Migration;

pub const V4_COMPONENTS_NATURAL_KEY: Migration = Migration {
    target: 4,
    description: "promote (entity_id, kind) to a UNIQUE constraint on components",
    up_sql: "
        CREATE UNIQUE INDEX idx_components_entity_kind
            ON components(entity_id, kind);
    ",
};
