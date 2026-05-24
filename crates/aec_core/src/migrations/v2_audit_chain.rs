//! v1 → v2 migration: SQL-side mirror of the BLAKE3 audit chain.
//!
//! Background. The audit log is the durable record of every
//! command/tool invocation a project sees. [`aec_audit::AuditLog`]
//! persists entries to `audit/log.jsonl` inside the project package
//! and re-derives the chain head on open by replaying the file.
//! That works for one in-process consumer at a time but leaves the
//! bridge with no cheap "what is the chain head right now?" path —
//! it would have to read the entire JSONL on every request.
//!
//! v2 adds an `audit_chain` table that mirrors the JSONL chain into
//! SQL. The `AuditLog` writer can additionally `INSERT` each entry
//! into this table (in a separate, smaller follow-up PR; that change
//! is out of scope for the migration itself, which only adds the
//! schema). The bridge then queries `MAX(seq)` / `hash` from the
//! table in O(1).
//!
//! Schema rationale:
//!
//! - `seq INTEGER PRIMARY KEY AUTOINCREMENT` — mirror of the JSONL
//!   line index (1-based). `AUTOINCREMENT` guarantees monotonic
//!   ordering even across delete-and-rewrite paths.
//! - `ts TEXT NOT NULL` — ISO-8601 (matches `AuditEntry.ts`).
//! - `actor TEXT NOT NULL` — `Actor` serialised via serde (matches
//!   `AuditEntry.actor`).
//! - `scope TEXT NOT NULL` — `Scope::as_str()` (one of
//!   `design`/`draft`/`bim`/`render`/`deliver`).
//! - `tool TEXT NOT NULL` — bare string; `None` becomes `""` on the
//!   writer side so the column is `NOT NULL` and queryable.
//! - `payload_hash TEXT NOT NULL` — 64-hex BLAKE3.
//! - `prev_hash TEXT NOT NULL` — 64-hex BLAKE3 of the previous
//!   entry's `hash`, or all-zeros for the first entry.
//! - `hash TEXT NOT NULL UNIQUE` — 64-hex BLAKE3 of `(payload_hash,
//!   prev_hash, ts, actor, scope, tool)`. `UNIQUE` so chain
//!   corruption (two entries with the same hash) surfaces at INSERT
//!   rather than silently.
//!
//! Indexed by `seq` (the primary key) so `MAX(seq)` is O(1). No
//! foreign keys: the audit chain is forward-append-only and
//! intentionally stands alone from the entity/relation tables so a
//! corrupted entity row never poisons the audit history.

use crate::migrations::Migration;

pub const V2_AUDIT_CHAIN: Migration = Migration {
    target: 2,
    description: "add audit_chain table mirroring the JSONL hash chain",
    up_sql: "
        CREATE TABLE audit_chain (
            seq          INTEGER PRIMARY KEY AUTOINCREMENT,
            ts           TEXT    NOT NULL,
            actor        TEXT    NOT NULL,
            scope        TEXT    NOT NULL,
            tool         TEXT    NOT NULL DEFAULT '',
            payload_hash TEXT    NOT NULL,
            prev_hash    TEXT    NOT NULL,
            hash         TEXT    NOT NULL UNIQUE
        );
        CREATE INDEX idx_audit_chain_scope ON audit_chain(scope);
        CREATE INDEX idx_audit_chain_tool ON audit_chain(tool);
    ",
};
