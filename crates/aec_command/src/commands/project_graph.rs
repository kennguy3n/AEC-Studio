//! Project graph that commands mutate.
//!
//! In-memory representation backed by the `entities` table in the
//! SQLCipher database (see `aec_core::db`). Use [`ProjectGraph::load`]
//! to read the current state from disk and [`ProjectGraph::persist_delta`]
//! to apply a single [`EntityDelta`] inside a transaction so the on-disk
//! state and the in-memory state stay in lock-step.

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    pub id: EntityId,
    pub kind: String,
    pub body: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntityId>,
}

/// A single applied or reversible change to the project graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntityDelta {
    Create {
        record: EntityRecord,
    },
    Update {
        id: EntityId,
        before: serde_json::Value,
        after: serde_json::Value,
    },
    Delete {
        record: EntityRecord,
    },
}

impl EntityDelta {
    pub fn invert(&self) -> Self {
        match self {
            Self::Create { record } => Self::Delete {
                record: record.clone(),
            },
            Self::Delete { record } => Self::Create {
                record: record.clone(),
            },
            Self::Update { id, before, after } => Self::Update {
                id: id.clone(),
                before: after.clone(),
                after: before.clone(),
            },
        }
    }
}

/// In-memory project graph keyed by `EntityId`. The graph stores entities
/// (rooms, walls, openings, lights, cameras, …) and exposes a small,
/// inversible apply API.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectGraph {
    entities: HashMap<EntityId, EntityRecord>,
}

impl ProjectGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn contains(&self, id: &EntityId) -> bool {
        self.entities.contains_key(id)
    }

    pub fn get(&self, id: &EntityId) -> Option<&EntityRecord> {
        self.entities.get(id)
    }

    pub fn entities_of_kind<'a>(
        &'a self,
        kind: &'a str,
    ) -> impl Iterator<Item = &'a EntityRecord> + 'a {
        self.entities.values().filter(move |e| e.kind == kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = &EntityRecord> {
        self.entities.values()
    }

    /// Load the graph from the `entities` table of an open SQLCipher
    /// connection. The schema is initialised by `aec_core::db::open_encrypted`;
    /// a freshly-created package has the table but zero rows.
    pub fn load(conn: &Connection) -> CommandResult<Self> {
        let mut stmt = conn.prepare("SELECT id, kind, parent_id, body FROM entities")?;
        let rows = stmt.query_map(params![], |row| {
            let id_str: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let parent: Option<String> = row.get(2)?;
            let body_text: String = row.get(3)?;
            Ok((id_str, kind, parent, body_text))
        })?;
        let mut entities = HashMap::new();
        for r in rows {
            let (id_str, kind, parent, body_text) = r?;
            let id = EntityId::from_string(id_str)
                .map_err(|e| CommandError::JournalCorrupt(format!("invalid entity id: {e}")))?;
            let body: serde_json::Value = serde_json::from_str(&body_text)?;
            let parent = parent
                .map(EntityId::from_string)
                .transpose()
                .map_err(|e| CommandError::JournalCorrupt(format!("invalid parent id: {e}")))?;
            entities.insert(
                id.clone(),
                EntityRecord {
                    id,
                    kind,
                    body,
                    parent,
                },
            );
        }
        Ok(Self { entities })
    }

    /// Read-only validation: would this delta apply cleanly against the
    /// current in-memory state? Used by the persistent execution path to
    /// detect duplicate-create / not-found-update / not-found-delete
    /// **before** opening the SQL transaction, so a validation failure
    /// leaves both layers untouched.
    pub fn validate(&self, delta: &EntityDelta) -> CommandResult<()> {
        match delta {
            EntityDelta::Create { record } => {
                if self.entities.contains_key(&record.id) {
                    return Err(CommandError::EntityAlreadyExists(record.id.to_string()));
                }
            }
            EntityDelta::Update { id, .. } => {
                if !self.entities.contains_key(id) {
                    return Err(CommandError::EntityNotFound(id.to_string()));
                }
            }
            EntityDelta::Delete { record } => {
                if !self.entities.contains_key(&record.id) {
                    return Err(CommandError::EntityNotFound(record.id.to_string()));
                }
            }
        }
        Ok(())
    }

    /// Validate a sequence of deltas as a unit. Walks a shadow copy of
    /// the entity map so the n-th delta is checked against the state
    /// after applying the first n-1 — necessary for multi-delta commands
    /// where e.g. a `Create` followed by an `Update` on the same id is
    /// valid even though the second `Update` alone wouldn't be against
    /// the pre-state.
    pub fn validate_all(&self, deltas: &[EntityDelta]) -> CommandResult<()> {
        let mut shadow = self.clone();
        for d in deltas {
            shadow.apply(d)?;
        }
        Ok(())
    }

    /// SQL-only persist of a single delta inside an externally-managed
    /// transaction. Does **not** mutate the in-memory state — the
    /// caller is responsible for calling [`Self::apply`] after the
    /// transaction has been committed. This split makes
    /// validate → SQL → commit → in-memory an all-or-nothing pipeline:
    /// if SQL fails (or the transaction is dropped without commit), the
    /// in-memory graph is never touched.
    pub fn persist_delta_in_tx(tx: &Transaction, delta: &EntityDelta) -> CommandResult<()> {
        let now = Utc::now().to_rfc3339();
        match delta {
            EntityDelta::Create { record } => {
                let body = serde_json::to_string(&record.body)?;
                tx.execute(
                    "INSERT INTO entities (id, kind, parent_id, created_at, updated_at, body) \
                     VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                    params![
                        record.id.to_string(),
                        record.kind,
                        record.parent.as_ref().map(EntityId::to_string),
                        now,
                        body,
                    ],
                )?;
            }
            EntityDelta::Update { id, after, .. } => {
                let body = serde_json::to_string(after)?;
                let n = tx.execute(
                    "UPDATE entities SET body = ?1, updated_at = ?2 WHERE id = ?3",
                    params![body, now, id.to_string()],
                )?;
                if n == 0 {
                    return Err(CommandError::EntityNotFound(id.to_string()));
                }
            }
            EntityDelta::Delete { record } => {
                let n = tx.execute(
                    "DELETE FROM entities WHERE id = ?1",
                    params![record.id.to_string()],
                )?;
                if n == 0 {
                    return Err(CommandError::EntityNotFound(record.id.to_string()));
                }
            }
        }
        Ok(())
    }

    /// Convenience wrapper that opens its own transaction. Kept for
    /// callers that want a single self-contained persist; the engine
    /// uses [`Self::persist_delta_in_tx`] directly so it can combine
    /// entity writes and journal writes into one atomic commit.
    pub fn persist_delta(
        &mut self,
        conn: &mut Connection,
        delta: &EntityDelta,
    ) -> CommandResult<()> {
        self.validate(delta)?;
        let tx = conn.transaction()?;
        Self::persist_delta_in_tx(&tx, delta)?;
        tx.commit()?;
        // Only mutate in-memory after the commit succeeds. If the commit
        // fails the `?` above returns early and the graph is untouched.
        self.apply(delta)
            .expect("validated above; apply cannot fail");
        Ok(())
    }

    /// Apply a single delta. The returned bool indicates whether the
    /// graph was changed (false would indicate the delta was a no-op, which
    /// is a bug; we panic-free-return so the engine can roll back).
    pub fn apply(&mut self, delta: &EntityDelta) -> Result<(), crate::error::CommandError> {
        use crate::error::CommandError;
        match delta {
            EntityDelta::Create { record } => {
                if self.entities.contains_key(&record.id) {
                    return Err(CommandError::EntityAlreadyExists(record.id.to_string()));
                }
                self.entities.insert(record.id.clone(), record.clone());
            }
            EntityDelta::Delete { record } => {
                if self.entities.remove(&record.id).is_none() {
                    return Err(CommandError::EntityNotFound(record.id.to_string()));
                }
            }
            EntityDelta::Update { id, after, .. } => {
                let entry = self
                    .entities
                    .get_mut(id)
                    .ok_or_else(|| CommandError::EntityNotFound(id.to_string()))?;
                entry.body = after.clone();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::types::EntityId;

    fn record(kind: &str) -> EntityRecord {
        EntityRecord {
            id: EntityId::new(),
            kind: kind.into(),
            body: serde_json::json!({"v": 1}),
            parent: None,
        }
    }

    #[test]
    fn apply_create_then_invert_deletes() {
        let mut g = ProjectGraph::new();
        let r = record("wall");
        let delta = EntityDelta::Create { record: r.clone() };
        g.apply(&delta).unwrap();
        assert_eq!(g.len(), 1);
        g.apply(&delta.invert()).unwrap();
        assert!(g.is_empty());
    }

    #[test]
    fn apply_update_swaps_body() {
        let mut g = ProjectGraph::new();
        let r = record("room");
        g.apply(&EntityDelta::Create { record: r.clone() }).unwrap();
        g.apply(&EntityDelta::Update {
            id: r.id.clone(),
            before: serde_json::json!({"v": 1}),
            after: serde_json::json!({"v": 42}),
        })
        .unwrap();
        assert_eq!(g.get(&r.id).unwrap().body["v"], 42);
    }

    #[test]
    fn duplicate_create_errors() {
        let mut g = ProjectGraph::new();
        let r = record("wall");
        g.apply(&EntityDelta::Create { record: r.clone() }).unwrap();
        let err = g.apply(&EntityDelta::Create { record: r }).unwrap_err();
        matches!(err, crate::error::CommandError::EntityAlreadyExists(_));
    }

    #[test]
    fn delete_missing_errors() {
        let mut g = ProjectGraph::new();
        let r = record("wall");
        let err = g.apply(&EntityDelta::Delete { record: r }).unwrap_err();
        matches!(err, crate::error::CommandError::EntityNotFound(_));
    }

    fn open_in_memory_with_entities_table() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entities (
                id          TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                parent_id   TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                body        TEXT NOT NULL,
                FOREIGN KEY (parent_id) REFERENCES entities(id) ON DELETE CASCADE
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn persist_delta_create_then_load_round_trips_record() {
        let mut conn = open_in_memory_with_entities_table();
        let mut g = ProjectGraph::new();
        let r = record("wall");
        g.persist_delta(&mut conn, &EntityDelta::Create { record: r.clone() })
            .unwrap();
        let reloaded = ProjectGraph::load(&conn).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.get(&r.id).unwrap(), &r);
    }

    #[test]
    fn persist_delta_update_then_load_reads_after_body() {
        let mut conn = open_in_memory_with_entities_table();
        let mut g = ProjectGraph::new();
        let r = record("room");
        g.persist_delta(&mut conn, &EntityDelta::Create { record: r.clone() })
            .unwrap();
        g.persist_delta(
            &mut conn,
            &EntityDelta::Update {
                id: r.id.clone(),
                before: r.body.clone(),
                after: serde_json::json!({"v": 99}),
            },
        )
        .unwrap();
        let reloaded = ProjectGraph::load(&conn).unwrap();
        assert_eq!(reloaded.get(&r.id).unwrap().body["v"], 99);
    }

    #[test]
    fn persist_delta_delete_then_load_drops_record() {
        let mut conn = open_in_memory_with_entities_table();
        let mut g = ProjectGraph::new();
        let r = record("light");
        g.persist_delta(&mut conn, &EntityDelta::Create { record: r.clone() })
            .unwrap();
        g.persist_delta(&mut conn, &EntityDelta::Delete { record: r })
            .unwrap();
        let reloaded = ProjectGraph::load(&conn).unwrap();
        assert!(reloaded.is_empty());
    }

    #[test]
    fn persist_delta_duplicate_create_returns_error_without_disturbing_sql() {
        let mut conn = open_in_memory_with_entities_table();
        let mut g = ProjectGraph::new();
        let r = record("wall");
        g.persist_delta(&mut conn, &EntityDelta::Create { record: r.clone() })
            .unwrap();
        let err = g
            .persist_delta(&mut conn, &EntityDelta::Create { record: r.clone() })
            .unwrap_err();
        assert!(matches!(err, CommandError::EntityAlreadyExists(_)));
        // SQL table still has exactly the original row.
        let reloaded = ProjectGraph::load(&conn).unwrap();
        assert_eq!(reloaded.len(), 1);
    }
}
