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
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    apply_pragmas(&conn, key)?;
    initialize_schema(&conn)?;
    crate::migrations::run_pending(
        &mut conn,
        crate::migrations::Migration::all(),
        crate::manifest::SCHEMA_VERSION,
    )?;
    Ok(conn)
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
/// pragmas that need write access (`journal_mode = WAL`,
/// `foreign_keys = ON`). Designed for revision snapshot (`.snap`)
/// diffing: a future caller can't accidentally mutate the snapshot, and
/// no `-wal` / `-shm` sidecar files get scattered in the revisions
/// directory after a process crash.
///
/// The encryption key + cipher parameters are still applied so the call
/// fails loudly on a wrong key (the key check is the same
/// `SELECT count(*) FROM sqlite_master` probe used by
/// [`open_existing`]).
pub fn open_readonly(path: &Path, key: &Key32) -> AecResult<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
}
