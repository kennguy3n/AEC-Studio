//! v4 → v5 migration: add the `project_thumbnail` singleton table.
//!
//! Phase 17 Group B Task 12 — real project thumbnails on the Home
//! page's recent-project grid. Today every `ProjectCard` renders a
//! CSS gradient placeholder regardless of the project's actual
//! viewport state, which gives the gallery a uniform stock look
//! that fails the "I recognise my own project" sniff test.
//!
//! The thumbnail is a single PNG-encoded byte buffer per project,
//! re-rendered on each project save with the current viewport
//! camera. Refresh is debounced upstream so geometry-and-camera-
//! identical saves do not re-encode.
//!
//! Schema choice — singleton row vs blob column on `meta`:
//!
//! - The `meta` key-value table already exists in the v1 base
//!   schema and exposes `key TEXT PRIMARY KEY, value TEXT`. It's
//!   the obvious home for "one value per project" data, but
//!   `value TEXT` would force PNG bytes to be base64-encoded for
//!   every read and write, which is ~1.33× larger than raw bytes
//!   and adds a CPU cost on the load path (~150 µs to decode a
//!   60 KB base64 string).
//! - A dedicated `project_thumbnail` table with a `BLOB`-typed
//!   `png` column lets us store the raw bytes, makes the schema
//!   explicit (a `CHECK (singleton = 1)` guard enforces "exactly
//!   one row per project" without relying on application code),
//!   and gives us a natural place to record metadata fields
//!   (width / height / updated_at) for future cache-busting and
//!   debounce decisions.
//!
//! The `singleton` column is a SQLite trick: a `CHECK (singleton =
//! 1)` constraint plus `PRIMARY KEY (singleton)` means the table
//! can hold at most one row. Subsequent thumbnail writes use
//! `INSERT OR REPLACE` so the row gets overwritten in place rather
//! than appended.
//!
//! Forward-only / fail-loud, per the registry's invariants
//! documented in [`crate::migrations`].

use crate::migrations::Migration;

pub const V5_PROJECT_THUMBNAIL: Migration = Migration {
    target: 5,
    description: "add singleton project_thumbnail table for PBR-preview blobs",
    up_sql: "
        CREATE TABLE project_thumbnail (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            png BLOB NOT NULL,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
    ",
};
