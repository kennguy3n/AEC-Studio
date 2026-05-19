//! Audit envelope and BLAKE3 hash-chain helpers.

use serde::{Deserialize, Serialize};

use aec_core::types::CommandId;

/// Per-command audit envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub command_id: CommandId,
    pub hash: String,
    pub previous_hash: String,
    #[serde(default)]
    pub signed: bool,
}

/// Append-only BLAKE3 hash chain. Each new entry hashes
/// `previous_hash || command_id || canonical_json(payload)`.
#[derive(Debug, Default, Clone)]
pub struct AuditHashChain {
    head: String,
}

impl AuditHashChain {
    pub fn new() -> Self {
        Self { head: "blake3:genesis".to_string() }
    }

    pub fn head(&self) -> &str {
        &self.head
    }

    pub fn extend(&mut self, command_id: &CommandId, payload: &serde_json::Value) -> AuditEnvelope {
        let canonical = serde_json::to_vec(payload).unwrap_or_default();
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.head.as_bytes());
        hasher.update(command_id.as_str().as_bytes());
        hasher.update(&canonical);
        let next = format!("blake3:{}", hasher.finalize().to_hex());
        let envelope = AuditEnvelope {
            command_id: command_id.clone(),
            hash: next.clone(),
            previous_hash: std::mem::replace(&mut self.head, next),
            signed: false,
        };
        envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_links_each_entry_to_previous() {
        let mut chain = AuditHashChain::new();
        let cmd_a = CommandId::new();
        let e1 = chain.extend(&cmd_a, &serde_json::json!({"x": 1}));
        let cmd_b = CommandId::new();
        let e2 = chain.extend(&cmd_b, &serde_json::json!({"x": 2}));
        assert_eq!(e2.previous_hash, e1.hash);
        assert_ne!(e1.hash, e2.hash);
        assert_eq!(chain.head(), e2.hash);
    }

    #[test]
    fn chain_is_deterministic_for_same_input() {
        let mut a = AuditHashChain::new();
        let mut b = AuditHashChain::new();
        let cid = CommandId::from_string("cmd_1234567890abcdef").unwrap();
        let p = serde_json::json!({"x": 1});
        let e1 = a.extend(&cid, &p);
        let e2 = b.extend(&cid, &p);
        assert_eq!(e1.hash, e2.hash);
    }
}
