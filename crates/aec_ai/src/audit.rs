//! AI audit logger. Hash-chained log of every AI action (accepted /
//! rejected). Sits on top of the generic `aec_audit::AuditLog`.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use aec_audit::{AuditEntry, AuditError, AuditLog};
use aec_core::types::{Actor, CommandId, Scope};

use crate::diff_engine::DiffStatus;
use crate::tool_schema::ToolName;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAuditRecord {
    pub tool: ToolName,
    pub scope: Scope,
    pub status: DiffStatus,
    pub diff_id: String,
    pub payload_hash: String,
    pub ts: DateTime<Utc>,
}

pub struct AiAuditLogger {
    log: AuditLog,
}

impl AiAuditLogger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        Ok(Self {
            log: AuditLog::open(path)?,
        })
    }

    /// Append a new AI audit record. The underlying [`AuditLog`] hash-chains
    /// the payload; this wrapper just stringifies the AI-specific envelope.
    pub fn append(&mut self, record: AiAuditRecord) -> Result<AuditEntry, AuditError> {
        let payload = serde_json::to_value(&record)?;
        let tool_label = format!("ai.{}", record.tool.as_str());
        let entry = self
            .log
            .append(
                CommandId::new(),
                record.scope,
                Actor::ai(record.tool.as_str()),
                tool_label,
                &payload,
            )?
            .clone();
        Ok(entry)
    }

    pub fn head(&self) -> &str {
        self.log.head()
    }

    pub fn entry_count(&self) -> usize {
        self.log.entries().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_changes_head_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai_audit.jsonl");
        let mut logger = AiAuditLogger::open(&path).unwrap();
        let head_before = logger.head().to_string();
        let record = AiAuditRecord {
            tool: ToolName::StyleAssistant,
            scope: Scope::Design,
            status: DiffStatus::Accepted,
            diff_id: "diff_abc".into(),
            payload_hash: "blake3:cafe".into(),
            ts: Utc::now(),
        };
        logger.append(record).unwrap();
        assert_ne!(logger.head(), head_before);
        assert_eq!(logger.entry_count(), 1);
    }
}
