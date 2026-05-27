//! SQLCipher-backed project database.
//!
//! Schema:
//!
//! - `entities`        — the canonical entity table (id, kind, parent, JSON
//!   body).
//! - `components`      — per-entity components (id, entity_id, kind, JSON
//!   body) used to attach geometry/material/property data.
//! - `relations`       — directed relations between entities (kind, from,
//!   to).
//! - `undo_journal`    — append-only reversible deltas for the command
//!   engine.
//! - `bim_cache`       — per-element geometry/property hash cache for fast
//!   IFC re-open and re-export.

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags};

use crate::crypto::Key32;
use crate::error::AecResult;

/// Open (or create) a SQLCipher-encrypted project database at `path`,
/// applying `key` as the page-level encryption key, running schema
/// initialization for a fresh database and then advancing any existing
/// database to the current [`crate::manifest::SCHEMA_VERSION`] via
/// [`crate::migrations::run_pending`].
pub fn open_encrypted(path: &Path, key: &Key32) -> AecResult<Connection> {
    // `SQLITE_OPEN_NO_MUTEX` selects multi-thread mode — the connection
    // is used by at most one thread at a time and SQLite skips its per-
    // connection mutex. This matches the threading mode that
    // `open_existing` inherits from rusqlite's default `OpenFlags`
    // (`Connection::open` → `READ_WRITE | CREATE | NO_MUTEX | URI`); all
    // three `open_*` paths therefore run in the same threading mode so
    // there are no surprising performance differences between them.
    // Phase 12 Task 26: capture existence BEFORE the `OPEN_CREATE`
    // call below creates the file as a side effect, so the
    // pre-migration backup step can skip fresh databases — they have
    // no committed user state worth preserving.
    let existed_before_open = path.exists();
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_pragmas(&conn, key)?;
    initialize_schema(&conn)?;
    // Phase 12 Task 26: back up the SQLCipher database before running
    // any pending forward migrations. We only take the backup when:
    //  (a) the file existed before this `open_encrypted` call, AND
    //  (b) the recorded schema version is *behind* the current binary
    // For fresh databases (no pre-existing file) and steady-state opens
    // (already at current) the file copy is skipped so we don't
    // pollute the project directory on every open.
    let from_version = crate::migrations::read_schema_version(&conn)?;
    let to_version = crate::manifest::SCHEMA_VERSION;
    if existed_before_open && from_version < to_version {
        // Drop our handle first so the source file isn't held open by
        // the WAL writer when we copy it. We re-open immediately after
        // the backup so the rest of this function works against a
        // fully-initialized connection.
        //
        // Load-bearing invariant: this is the *only* connection to the
        // database at this point — `open_encrypted` opened it moments
        // ago and no other code path has had a chance to attach.
        // Dropping the last connection in WAL mode triggers SQLite's
        // passive checkpoint, which flushes the `-wal` sidecar back
        // into the main file *before* `backup_before_migration`'s
        // `std::fs::copy` runs. That's how the file copy can omit the
        // `-wal` / `-shm` sidecars (see the rationale on
        // `backup_before_migration`) and still capture a consistent
        // pre-migration snapshot.
        //
        // If a future change ever introduces a second concurrent
        // connection to the SQLCipher file (e.g. a reader handle held
        // by another subsystem for the lifetime of `BridgeService`),
        // the `drop(conn)` here will no longer be the last-connection
        // checkpoint trigger, and the file copy will silently capture
        // a half-committed view of the database. In that case the
        // backup path *must* either (a) explicitly run
        // `PRAGMA wal_checkpoint(FULL);` before the `drop`, or (b)
        // copy the `-wal` + `-shm` sidecars alongside the main file.
        drop(conn);
        backup_before_migration(path, from_version, to_version)?;
        conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        apply_pragmas(&conn, key)?;
    }
    crate::migrations::run_pending(&mut conn, crate::migrations::Migration::all(), to_version)?;
    Ok(conn)
}

/// Phase 12 Task 26: copy the SQLCipher database alongside the original
/// before forward migrations run. The backup name encodes the schema
/// version range so legacy backups don't collide with future migrations
/// — e.g. `project.sqlite.bak.v2-to-v4` lets a crash-recovery script
/// know exactly which pre-image to restore from.
///
/// Returns `Ok(())` when no backup is required (database doesn't exist
/// yet — `open_encrypted` is also the creation path for fresh
/// projects) or after the copy completes. SQLite's `-wal` / `-shm`
/// sidecar files aren't copied: the migration runner walks each
/// migration inside its own transaction, so a partially-flushed WAL
/// would be re-applied on the *next* open anyway, and the backup
/// captures the pre-migration committed state which is what we need
/// for rollback.
fn backup_before_migration(path: &Path, from: u32, to: u32) -> AecResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let backup_name = format!(
        "{}.bak.v{}-to-v{}",
        path.file_name().map_or_else(
            || "project.sqlite".to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        from,
        to
    );
    let backup_path = path.parent().map_or_else(
        || std::path::PathBuf::from(&backup_name),
        |p| p.join(&backup_name),
    );
    // Use std::fs::copy for the raw SQLCipher file copy — this is
    // intentionally NOT a `VACUUM INTO` because the source DB is
    // encrypted and we want the backup encrypted the same way (i.e.
    // bit-identical pages, decryptable with the same key derivation).
    std::fs::copy(path, &backup_path)?;
    Ok(())
}

/// Open an *already-initialized* encrypted database read/write. Does NOT
/// re-apply schema; useful for inspection and tests that should fail
/// loudly if the schema is missing.
///
/// For diffing snapshot files (`.snap`) that must never be mutated, use
/// [`open_readonly`] instead — it opens the file with
/// `SQLITE_OPEN_READ_ONLY` so a future bug can't accidentally write to
/// the snapshot and so no `-wal` / `-shm` sidecar files are created in
/// the revisions directory.
pub fn open_existing(path: &Path, key: &Key32) -> AecResult<Connection> {
    let conn = Connection::open(path)?;
    apply_pragmas(&conn, key)?;
    // Issue a small SELECT to force SQLCipher to validate the key.
    {
        let mut stmt = conn.prepare("SELECT count(*) FROM sqlite_master")?;
        let _: i64 = stmt.query_row([], |r| r.get(0))?;
    }
    Ok(conn)
}

/// Open an already-initialized SQLCipher database **strictly read-only**
/// — uses `SQLITE_OPEN_READ_ONLY` so the SQLite library refuses any
/// write at the engine level (not just by convention), and skips the
/// connection-tuning pragmas that the read-only path doesn't need:
///
/// * `journal_mode = WAL` requires write access to the DB header and
///   creates `-wal` / `-shm` sidecar files, which is *exactly* the
///   failure mode this function exists to prevent for `.snap` files.
/// * `foreign_keys = ON` and `busy_timeout = 5000` are intentionally
///   omitted because they only affect DML / write contention, neither
///   of which is reachable on a `SQLITE_OPEN_READ_ONLY` handle
///   (DML statements fail at parse time before constraint enforcement,
///   and a read-only connection takes a SHARED lock that never
///   conflicts with the snapshot-creation write path). Skipping them
///   keeps the open path purely cryptographic.
///
/// Designed for revision snapshot (`.snap`) diffing: a future caller
/// can't accidentally mutate the snapshot, and no `-wal` / `-shm`
/// sidecar files get scattered in the revisions directory after a
/// process crash.
///
/// The encryption key + cipher parameters are still applied so the call
/// fails loudly on a wrong key (the key check is the same
/// `SELECT count(*) FROM sqlite_master` probe used by
/// [`open_existing`]).
///
/// `SQLITE_OPEN_NO_MUTEX` is included to keep this on the same multi-
/// thread threading model as `open_existing` / `open_encrypted`, so all
/// three open paths have consistent per-connection locking semantics.
pub fn open_readonly(path: &Path, key: &Key32) -> AecResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_cipher_pragmas(&conn, key)?;
    // Validate key by forcing SQLCipher to decrypt page 1.
    {
        let mut stmt = conn.prepare("SELECT count(*) FROM sqlite_master")?;
        let _: i64 = stmt.query_row([], |r| r.get(0))?;
    }
    Ok(conn)
}

fn apply_pragmas(conn: &Connection, key: &Key32) -> AecResult<()> {
    apply_cipher_pragmas(conn, key)?;
    // `busy_timeout = 5000` makes every SQL statement on this connection
    // wait up to 5 seconds for a competing writer to release the database
    // lock before returning `SQLITE_BUSY`. This is what makes it safe for
    // bridge entry points like `bim_classify` / `bim_set_property` to use
    // the **read** side of the bridge-wide `RwLock<BridgeService>` even
    // though they perform DB writes: WAL mode lets readers and writers
    // co-exist, and `busy_timeout` lets two writers on different bridge
    // calls serialise themselves at the SQLite layer instead of at the
    // bridge layer. Status-poll readers (`runtime_status`,
    // `render_list_jobs`, etc.) therefore stay responsive while a long
    // classification walk runs. 5 seconds is the same default rusqlite
    // uses when callers explicitly opt in to a busy_handler.
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// Apply only the SQLCipher key + cipher-format pragmas. Safe on
/// [`OpenFlags::SQLITE_OPEN_READ_ONLY`] connections because none of
/// these pragmas need write access to the database file.
fn apply_cipher_pragmas(conn: &Connection, key: &Key32) -> AecResult<()> {
    // SQLCipher requires the key pragma before any other operation. The key
    // is supplied as a 64-character hex string (32 raw bytes) so we don't
    // depend on a PBKDF2 round-trip — we already derived a real 256-bit
    // key via BLAKE3 in `crypto::derive_project_key`.
    let pragma_key = format!("PRAGMA key = \"x'{}'\";", key.to_hex());
    conn.execute_batch(&pragma_key)?;
    conn.execute_batch(
        "PRAGMA cipher_page_size = 4096;
         PRAGMA kdf_iter = 256000;
         PRAGMA cipher_hmac_algorithm = HMAC_SHA512;
         PRAGMA cipher_kdf_algorithm = PBKDF2_HMAC_SHA512;",
    )?;
    Ok(())
}

fn initialize_schema(conn: &Connection) -> AecResult<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS entities (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL,
            parent_id   TEXT,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            body        TEXT NOT NULL,
            FOREIGN KEY (parent_id) REFERENCES entities(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_entities_kind ON entities(kind);
        CREATE INDEX IF NOT EXISTS idx_entities_parent ON entities(parent_id);

        CREATE TABLE IF NOT EXISTS components (
            id          TEXT PRIMARY KEY,
            entity_id   TEXT NOT NULL,
            kind        TEXT NOT NULL,
            body        TEXT NOT NULL,
            FOREIGN KEY (entity_id) REFERENCES entities(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_components_entity ON components(entity_id);
        CREATE INDEX IF NOT EXISTS idx_components_kind ON components(kind);

        CREATE TABLE IF NOT EXISTS relations (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            kind        TEXT NOT NULL,
            from_id     TEXT NOT NULL,
            to_id       TEXT NOT NULL,
            UNIQUE (kind, from_id, to_id)
        );
        CREATE INDEX IF NOT EXISTS idx_relations_kind ON relations(kind);
        CREATE INDEX IF NOT EXISTS idx_relations_from ON relations(from_id);

        CREATE TABLE IF NOT EXISTS undo_journal (
            seq         INTEGER PRIMARY KEY AUTOINCREMENT,
            command_id  TEXT NOT NULL,
            applied_at  TEXT NOT NULL,
            forward     TEXT NOT NULL,
            inverse     TEXT NOT NULL,
            superseded  INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_undo_command ON undo_journal(command_id);
        -- `undo_journal.scope` is intentionally NOT in this base v1
        -- schema: it is added by migration v3 (see
        -- `crates/aec_core/src/migrations/v3_undo_journal_scope.rs`)
        -- so every fresh database exercises the same migration path
        -- as an in-place upgrade from a pre-v3 project.

        CREATE TABLE IF NOT EXISTS bim_cache (
            global_id   TEXT PRIMARY KEY,
            geom_hash   TEXT,
            pset_hash   TEXT,
            class_hash  TEXT,
            last_seen   TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS meta (
            key         TEXT PRIMARY KEY,
            value       TEXT NOT NULL
        );
        ",
    )?;

    // Record the base schema version (v1). `open_encrypted` runs the
    // migration registry immediately after `initialize_schema`, which
    // is what advances the version to `crate::manifest::SCHEMA_VERSION`.
    // Setting the marker here to the base (1) and not the current
    // value is what makes a fresh database go through the exact same
    // upgrade walk as an existing v1 file — keeping the two code paths
    // semantically identical and eliminating "what version did this
    // database start as" as a possible drift point.
    //
    // Use `INSERT OR IGNORE` so re-running on an already-initialised
    // database (e.g. after a migration has advanced past v1) does not
    // clobber the recorded version back to 1.
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1')",
        params![],
    )?;
    Ok(())
}

/// Return the schema-version recorded in `meta` (used by tests and the
/// future migration runner).
pub fn schema_version(conn: &Connection) -> AecResult<u32> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = 'schema_version'")?;
    let v: String = stmt.query_row([], |r| r.get(0))?;
    Ok(v.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{derive_project_key, generate_project_nonce};

    fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("project.sqlite");
        (td, p)
    }

    #[test]
    fn open_encrypted_initializes_schema() {
        let (_td, p) = temp_db();
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        let conn = open_encrypted(&p, &key).unwrap();
        assert_eq!(
            schema_version(&conn).unwrap(),
            crate::manifest::SCHEMA_VERSION
        );

        // entities table exists and is empty
        let mut stmt = conn.prepare("SELECT count(*) FROM entities").unwrap();
        let n: i64 = stmt.query_row([], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn correct_key_reopens_the_db() {
        let (_td, p) = temp_db();
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        {
            let conn = open_encrypted(&p, &key).unwrap();
            conn.execute(
                "INSERT INTO entities(id, kind, created_at, updated_at, body) \
                 VALUES ('ent_test', 'space', datetime('now'), datetime('now'), '{}')",
                [],
            )
            .unwrap();
        }
        let conn = open_existing(&p, &key).unwrap();
        let mut stmt = conn.prepare("SELECT count(*) FROM entities").unwrap();
        let n: i64 = stmt.query_row([], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn wrong_key_fails_to_open() {
        let (_td, p) = temp_db();
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        {
            let _conn = open_encrypted(&p, &key).unwrap();
        }
        // Different key — should fail.
        let wrong = derive_project_key(&master, &[0u8; 32]);
        let err = open_existing(&p, &wrong);
        assert!(err.is_err(), "wrong key must not open the database");
    }

    #[test]
    fn fresh_open_does_not_create_a_pre_migration_backup() {
        // Phase 12 Task 26: opening a brand-new database must NOT
        // sprinkle backup files next to it — there's no pre-existing
        // state to preserve and the backup would just clutter the
        // project directory.
        let (td, p) = temp_db();
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        let _conn = open_encrypted(&p, &key).unwrap();
        // Walk the temp dir and assert no `.bak.v*-to-v*` files exist.
        let entries: Vec<String> = std::fs::read_dir(td.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !entries.iter().any(|n| n.contains(".bak.v")),
            "no backup file expected next to a freshly-created database; got entries: {entries:?}"
        );
    }

    #[test]
    fn legacy_v1_open_writes_a_pre_migration_backup() {
        // Phase 12 Task 26: open a fresh DB, force its recorded version
        // back to v1 to mimic a legacy project, close, then re-open to
        // trigger the backup-then-migrate path.
        let (td, p) = temp_db();
        let master = [11u8; 32];
        let nonce = generate_project_nonce().unwrap();
        let key = derive_project_key(&master, &nonce);
        {
            let conn = open_encrypted(&p, &key).unwrap();
            // Reverse every post-v1 migration so the next open's
            // migration walk has real work to do without colliding
            // with already-applied DDL. Mirrors what the legacy v1
            // simulation in `package.rs::legacy_v1_project_is_upgraded_end_to_end`
            // does.
            conn.execute_batch(
                "DROP TABLE IF EXISTS audit_chain; \
                 DROP INDEX IF EXISTS idx_undo_journal_scope; \
                 DROP INDEX IF EXISTS idx_components_entity_kind; \
                 ALTER TABLE undo_journal DROP COLUMN scope; \
                 INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '1');",
            )
            .unwrap();
        }
        // Re-open. The recorded version is now 1 < SCHEMA_VERSION so
        // the open path must (a) take a backup, (b) run migrations.
        let _conn = open_encrypted(&p, &key).unwrap();
        let entries: Vec<String> = std::fs::read_dir(td.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let target = crate::manifest::SCHEMA_VERSION;
        let expected_prefix = format!("project.sqlite.bak.v1-to-v{target}");
        assert!(
            entries.contains(&expected_prefix),
            "expected `{expected_prefix}` in {entries:?}"
        );
    }
}
