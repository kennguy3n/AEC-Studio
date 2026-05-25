//! Forward-only schema migrations for the SQLCipher project database.
//!
//! [`open_encrypted`](crate::db::open_encrypted) creates the base v1
//! schema, then [`run_pending`] iterates the registered migrations
//! from v2 onwards, applying any whose `target` is greater than the
//! database's recorded `schema_version`.
//!
//! The design rules are:
//!
//! 1. Migrations are **forward-only** — there is no `down`. The on-disk
//!    package format is the durable record; downgrade is the user's
//!    "open in an older app version" problem (the manifest's
//!    `schema_version` rejects the project loudly via
//!    [`crate::manifest::ProjectManifest::validate`]).
//! 2. Migrations are **transactional** — every step runs inside a
//!    `BEGIN ... COMMIT` and the `meta.schema_version` bump is part of
//!    the same transaction. A partial migration cannot leave the
//!    database in an undefined state.
//! 3. Migrations are **idempotent** — running the registry against a
//!    database that's already at the target version is a no-op (the
//!    `for_each_pending` walk simply finds nothing to do).
//! 4. Migrations form a **dense chain** — every integer from 2 to
//!    `CURRENT_SCHEMA_VERSION` must have exactly one [`Migration`]
//!    registered. Gaps or duplicates are caught at registry
//!    construction with a typed error.
//!
//! Adding a new migration:
//!
//! - Add a `MIGRATION_TO_VN` constant in [`registry`] returning a
//!   [`Migration`] with `target = N` and the `up` SQL.
//! - Push the constant onto the slice returned by
//!   [`Migration::all`] (sorted ascending by target).
//! - Bump [`CURRENT_SCHEMA_VERSION`] in [`crate::manifest`] to `N`.
//! - Add a v(N-1) → vN upgrade test in [`tests`] (and verify the
//!   manifest-validator still accepts vN).

use rusqlite::{params, Connection};

use crate::error::{AecError, AecResult};

pub mod v2_audit_chain;
pub mod v3_undo_journal_scope;
pub mod v4_components_natural_key;

/// A forward-only DDL step that moves a database from
/// `target - 1` to `target`. SQL is run inside the migration runner's
/// transaction; the runner appends the `meta.schema_version` bump
/// itself, so migration SQL must NOT touch that key.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The schema version this migration brings the database **to**.
    /// `target = 2` means "applied against a v1 database, leaves a v2
    /// database". The base v1 schema is created by
    /// [`crate::db::initialize_schema`] itself and has no migration
    /// entry.
    pub target: u32,
    /// Human-readable summary used in audit / error messages.
    pub description: &'static str,
    /// DDL applied to bring the database up to `target`. Must be safe
    /// to run inside an existing `BEGIN` transaction (no nested
    /// `BEGIN`/`COMMIT`). The runner wraps every migration in its own
    /// transaction so individual migrations should NOT.
    pub up_sql: &'static str,
}

impl Migration {
    /// The full registry, in ascending order by `target`. The chain
    /// must be dense (no gaps) and unique (no duplicate targets);
    /// [`Self::validate_registry`] enforces both at runner start time.
    pub fn all() -> &'static [Migration] {
        // Concrete migrations live in their own modules to keep the
        // SQL local to its diff. Order must be ascending by `target`
        // and dense; `validate_registry` enforces both at runner
        // start time.
        &[
            v2_audit_chain::V2_AUDIT_CHAIN,
            v3_undo_journal_scope::V3_UNDO_JOURNAL_SCOPE,
            v4_components_natural_key::V4_COMPONENTS_NATURAL_KEY,
        ]
    }

    /// Verify the registry is a dense, ascending, duplicate-free chain
    /// starting at 2 and ending at `current_schema_version` (i.e. the
    /// value [`crate::manifest::SCHEMA_VERSION`]). Called by
    /// [`run_pending`] before any SQL is executed so a malformed
    /// registry surfaces as a typed error rather than as silently-
    /// skipped migrations.
    pub fn validate_registry(
        migrations: &[Migration],
        current_schema_version: u32,
    ) -> AecResult<()> {
        if migrations.is_empty() {
            // An empty registry is valid only when the current schema
            // version is 1 (the base schema with no migrations
            // applied on top).
            if current_schema_version != 1 {
                return Err(AecError::Other(format!(
                    "migration registry is empty but CURRENT_SCHEMA_VERSION = {}; \
                     expected migrations for every version 2..={current_schema_version}",
                    current_schema_version
                )));
            }
            return Ok(());
        }
        let first = migrations[0].target;
        if first != 2 {
            return Err(AecError::Other(format!(
                "first registered migration must target version 2, got {first}"
            )));
        }
        for window in migrations.windows(2) {
            let (prev, next) = (window[0].target, window[1].target);
            if next <= prev {
                return Err(AecError::Other(format!(
                    "migrations must be sorted ascending by target; \
                     found {prev} followed by {next}"
                )));
            }
            if next != prev + 1 {
                return Err(AecError::Other(format!(
                    "migration chain has a gap: v{prev} followed by v{next} \
                     (expected v{})",
                    prev + 1
                )));
            }
        }
        let last = migrations[migrations.len() - 1].target;
        if last != current_schema_version {
            return Err(AecError::Other(format!(
                "last registered migration targets v{last} but \
                 CURRENT_SCHEMA_VERSION = {current_schema_version}"
            )));
        }
        Ok(())
    }
}

/// Read the database's recorded `meta.schema_version`, defaulting to 1
/// for databases that initialised under the pre-migration code path
/// (those still have the base schema, equivalent to a fresh v1).
fn read_schema_version(conn: &Connection) -> AecResult<u32> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = 'schema_version'")?;
    let v: Option<String> = stmt
        .query_row([], |r| r.get::<_, String>(0))
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(1))
}

/// Update `meta.schema_version` to `new_version`. Called inside the
/// migration runner's transaction together with the migration's `up_sql`
/// so an aborted migration leaves the recorded version unchanged.
fn write_schema_version(conn: &Connection, new_version: u32) -> AecResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![new_version.to_string()],
    )?;
    Ok(())
}

/// Apply every pending migration in `migrations` to `conn`, advancing
/// the database from its recorded `schema_version` to
/// `current_schema_version` one step at a time. Each step runs inside
/// its own transaction so a failure aborts cleanly with the previous
/// state intact.
///
/// Returns the new schema version (always equal to
/// `current_schema_version` on success, regardless of how many steps
/// were applied).
pub fn run_pending(
    conn: &mut Connection,
    migrations: &[Migration],
    current_schema_version: u32,
) -> AecResult<u32> {
    Migration::validate_registry(migrations, current_schema_version)?;
    let mut version = read_schema_version(conn)?;
    if version > current_schema_version {
        return Err(AecError::SchemaMismatch {
            found: version,
            expected: current_schema_version,
        });
    }
    for m in migrations {
        if m.target <= version {
            continue;
        }
        // `m.target` is `version + 1` by the dense-chain invariant.
        debug_assert_eq!(m.target, version + 1);
        let tx = conn.transaction()?;
        tx.execute_batch(m.up_sql)?;
        write_schema_version(&tx, m.target)?;
        tx.commit()?;
        version = m.target;
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_encrypted;

    fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("project.sqlite");
        (td, p)
    }

    fn open_key() -> crate::crypto::Key32 {
        let master = [11u8; 32];
        let nonce = crate::crypto::generate_project_nonce().unwrap();
        crate::crypto::derive_project_key(&master, &nonce)
    }

    #[test]
    fn empty_registry_against_v1_db_is_noop() {
        let (_td, p) = temp_db();
        // open_encrypted now applies migrations up to v2 automatically.
        // Reset the version back to 1 in the meta table so we can
        // exercise the empty-registry no-op path.
        let mut conn = open_encrypted(&p, &open_key()).unwrap();
        write_schema_version(&conn, 1).unwrap();
        let before = read_schema_version(&conn).unwrap();
        let after = run_pending(&mut conn, &[], 1).unwrap();
        assert_eq!(before, 1);
        assert_eq!(after, 1);
    }

    #[test]
    fn empty_registry_with_higher_current_version_is_rejected() {
        let (_td, p) = temp_db();
        let mut conn = open_encrypted(&p, &open_key()).unwrap();
        // Force the recorded schema_version back to 1 so the runner
        // genuinely has work to do that an empty registry can't supply.
        write_schema_version(&conn, 1).unwrap();
        // current_schema_version = 2 but no migrations registered;
        // registry validation must surface this as a clear error so a
        // forgotten registry entry doesn't silently leave brand-new
        // databases stuck at v1.
        let err = run_pending(&mut conn, &[], 2).unwrap_err();
        assert!(matches!(err, AecError::Other(msg) if msg.contains("registry is empty")));
    }

    #[test]
    fn validate_registry_rejects_gaps() {
        let chain = [
            Migration {
                target: 2,
                description: "v1->v2",
                up_sql: "",
            },
            Migration {
                target: 4,
                description: "v3->v4",
                up_sql: "",
            },
        ];
        let err = Migration::validate_registry(&chain, 4).unwrap_err();
        assert!(matches!(err, AecError::Other(msg) if msg.contains("gap")));
    }

    #[test]
    fn validate_registry_rejects_duplicates() {
        let chain = [
            Migration {
                target: 2,
                description: "v1->v2",
                up_sql: "",
            },
            Migration {
                target: 2,
                description: "duplicate",
                up_sql: "",
            },
        ];
        let err = Migration::validate_registry(&chain, 2).unwrap_err();
        assert!(matches!(err, AecError::Other(msg) if msg.contains("sorted ascending")));
    }

    #[test]
    fn validate_registry_rejects_first_target_other_than_two() {
        let chain = [Migration {
            target: 3,
            description: "v2->v3 with no v1->v2",
            up_sql: "",
        }];
        let err = Migration::validate_registry(&chain, 3).unwrap_err();
        assert!(matches!(err, AecError::Other(msg) if msg.contains("target version 2")));
    }

    #[test]
    fn validate_registry_rejects_tail_mismatch() {
        let chain = [Migration {
            target: 2,
            description: "v1->v2",
            up_sql: "",
        }];
        let err = Migration::validate_registry(&chain, 5).unwrap_err();
        assert!(matches!(err, AecError::Other(msg) if msg.contains("CURRENT_SCHEMA_VERSION")));
    }

    #[test]
    fn run_pending_applies_a_single_migration() {
        let (_td, p) = temp_db();
        let mut conn = open_encrypted(&p, &open_key()).unwrap();
        // Reset to v1 so this test exercises the v1→v2 path
        // independently of the production registry.
        write_schema_version(&conn, 1).unwrap();
        let chain = [Migration {
            target: 2,
            description: "add migration_smoke table",
            up_sql: "CREATE TABLE migration_smoke (id INTEGER PRIMARY KEY);",
        }];
        let after = run_pending(&mut conn, &chain, 2).unwrap();
        assert_eq!(after, 2);
        // Re-running is a no-op (idempotent).
        let after2 = run_pending(&mut conn, &chain, 2).unwrap();
        assert_eq!(after2, 2);
        // The recorded schema_version is updated.
        assert_eq!(read_schema_version(&conn).unwrap(), 2);
        // The CREATE TABLE was applied.
        let n: i64 = conn
            .query_row("SELECT count(*) FROM migration_smoke", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn run_pending_applies_a_chain_of_two_migrations() {
        let (_td, p) = temp_db();
        let mut conn = open_encrypted(&p, &open_key()).unwrap();
        // Reset to v1 so we drive the v1→v2→v3 walk under test
        // independently of the production registry.
        write_schema_version(&conn, 1).unwrap();
        let chain = [
            Migration {
                target: 2,
                description: "v1->v2",
                up_sql: "CREATE TABLE step_two (id INTEGER PRIMARY KEY);",
            },
            Migration {
                target: 3,
                description: "v2->v3",
                up_sql: "CREATE TABLE step_three (id INTEGER PRIMARY KEY);",
            },
        ];
        let after = run_pending(&mut conn, &chain, 3).unwrap();
        assert_eq!(after, 3);
        // Both tables exist.
        for tbl in ["step_two", "step_three"] {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {tbl}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{tbl} should exist and be empty");
        }
    }

    #[test]
    fn run_pending_rejects_future_schema_version() {
        let (_td, p) = temp_db();
        let mut conn = open_encrypted(&p, &open_key()).unwrap();
        write_schema_version(&conn, 5).unwrap();
        let chain = [Migration {
            target: 2,
            description: "v1->v2",
            up_sql: "CREATE TABLE step_two (id INTEGER PRIMARY KEY);",
        }];
        let err = run_pending(&mut conn, &chain, 2).unwrap_err();
        assert!(matches!(
            err,
            AecError::SchemaMismatch {
                found: 5,
                expected: 2
            }
        ));
    }

    #[test]
    fn opening_a_v1_database_upgrades_to_current_schema() {
        let (_td, p) = temp_db();
        let key = open_key();
        // 1. Open the database; this brings it up to the current
        //    SCHEMA_VERSION via the production registry.
        let conn = open_encrypted(&p, &key).unwrap();
        assert_eq!(
            read_schema_version(&conn).unwrap(),
            crate::manifest::SCHEMA_VERSION
        );
        // The v2 audit_chain table exists.
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
                 WHERE type='table' AND name='audit_chain'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);
        // 2. Simulate a "legacy v1" database by clobbering the recorded
        //    schema_version back to 1 and rolling every post-v1 DDL
        //    out: drop the v2 audit_chain table, the v3
        //    `undo_journal.scope` column / index, and the v4
        //    `idx_components_entity_kind` unique index.
        conn.execute_batch(
            "DROP TABLE audit_chain;
             DROP INDEX idx_undo_journal_scope;
             DROP INDEX idx_components_entity_kind;
             ALTER TABLE undo_journal DROP COLUMN scope;",
        )
        .unwrap();
        write_schema_version(&conn, 1).unwrap();
        drop(conn);
        // 3. Re-open. The production migration runner should upgrade
        //    it back to the current version.
        let conn = open_encrypted(&p, &key).unwrap();
        assert_eq!(
            read_schema_version(&conn).unwrap(),
            crate::manifest::SCHEMA_VERSION
        );
        // audit_chain table is back.
        let exists_after: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
                 WHERE type='table' AND name='audit_chain'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists_after, 1);
    }

    #[test]
    fn production_registry_is_well_formed() {
        // The production registry is checked at every open, but assert
        // it explicitly here too so a malformed entry surfaces as a
        // direct test failure rather than as a confusing
        // open_encrypted error.
        Migration::validate_registry(Migration::all(), crate::manifest::SCHEMA_VERSION).unwrap();
    }

    #[test]
    fn run_pending_aborts_failed_migration_cleanly() {
        let (_td, p) = temp_db();
        let mut conn = open_encrypted(&p, &open_key()).unwrap();
        // Reset to v1 so we can exercise a synthetic broken v1→v2
        // migration without the production registry interfering.
        write_schema_version(&conn, 1).unwrap();
        // A migration whose SQL fails halfway through must leave the
        // recorded schema_version unchanged.
        let chain = [Migration {
            target: 2,
            description: "v1->v2 (will fail)",
            up_sql:
                "CREATE TABLE first_ok (id INTEGER PRIMARY KEY);\nINSERT INTO nonexistent VALUES (1);",
        }];
        let err = run_pending(&mut conn, &chain, 2);
        assert!(err.is_err());
        // Schema version stayed at 1.
        assert_eq!(read_schema_version(&conn).unwrap(), 1);
        // The aborted migration's intermediate state was rolled back
        // (no `first_ok` table).
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='first_ok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0);
    }
}
