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
/// applying `key` as the page-level encryption key and running schema
/// initialization migrations.
pub fn open_encrypted(path: &Path, key: &Key32) -> AecResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    apply_pragmas(&conn, key)?;
    initialize_schema(&conn)?;
    Ok(conn)
}

/// Open an *already-initialized* encrypted database read-only. Does NOT
/// re-apply schema; useful for inspection and tests that should fail
/// loudly if the schema is missing.
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

fn apply_pragmas(conn: &Connection, key: &Key32) -> AecResult<()> {
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
         PRAGMA cipher_kdf_algorithm = PBKDF2_HMAC_SHA512;
         PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;",
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

    // Write a schema-version marker so future migrations can detect it.
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![crate::manifest::SCHEMA_VERSION.to_string()],
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
        let nonce = generate_project_nonce();
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
        let nonce = generate_project_nonce();
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
        let nonce = generate_project_nonce();
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
