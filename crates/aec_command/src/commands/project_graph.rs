//! Minimal in-memory project graph that commands mutate.
//!
//! Phase 1/2 keeps the graph in memory; persistence to `aec_core::db` will
//! land in Phase 3 alongside the on-disk command-log replay.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

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
}
