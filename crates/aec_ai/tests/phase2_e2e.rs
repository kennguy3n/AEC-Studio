//! Phase 2 — AI integration end-to-end tests.
//!
//! These tests exercise the AI side of the Phase 2 happy path: a
//! plan-detection response coming back from a (mocked) sidecar gets
//! turned into a `Diff` of wall inserts, the `AiAuditLogger`
//! chain-hashes every accepted diff, and the style assistant /
//! layout suggestion tools produce the same kind of pending diff a
//! reviewer can accept or reject.
//!
//! The point of these tests is to validate that:
//! * `DiffEngine::build` produces the correct `DiffOperation` mix
//!   for each AI tool — wall inserts for plan detection, furniture
//!   inserts + material/lighting updates for the style assistant,
//!   layout proposals for the layout-suggestion tool.
//! * Every AI action records a tamper-evident audit envelope.
//! * The hash chain advances on each appended record.

use serde_json::json;

use aec_ai::{
    AiAuditLogger, AiAuditRecord, DiffEngine, DiffOperation, DiffStatus, PlanResponse, ToolName,
};
use aec_core::Scope;

fn response(tool: ToolName, payload: serde_json::Value) -> PlanResponse {
    PlanResponse {
        tool,
        raw_payload: payload.to_string(),
        parsed: payload,
        entities_modified: 1,
    }
}

#[test]
fn plan_detection_diff_produces_wall_inserts_and_is_auditable() {
    let payload = json!({
        "polylines": [
            { "points": [[0, 0], [4500, 0]] },
            { "points": [[4500, 0], [4500, 3000]] },
            { "points": [[4500, 3000], [0, 3000]] },
            { "points": [[0, 3000], [0, 0]] }
        ]
    });
    let plan = response(ToolName::PlanDetection, payload.clone());
    let diff = DiffEngine::build(&plan);
    assert_eq!(diff.tool, ToolName::PlanDetection);
    assert_eq!(diff.status, DiffStatus::Pending);
    assert_eq!(
        diff.operations.len(),
        4,
        "every polyline must become an Insert"
    );
    for op in &diff.operations {
        match op {
            DiffOperation::Insert { entity_kind, .. } => {
                assert_eq!(entity_kind, "wall");
            }
            other => panic!("plan detection must only emit Insert ops, got {other:?}"),
        }
    }

    // Audit the diff: a real Phase 2 workflow appends a single audit
    // envelope per accepted diff. We use a tempdir so the log is
    // cleaned up after the test.
    let tmp = tempfile::tempdir().unwrap();
    let mut logger = AiAuditLogger::open(tmp.path().join("ai_audit.log")).unwrap();
    let first_head = logger.head().to_string();
    let record = AiAuditRecord {
        tool: ToolName::PlanDetection,
        scope: Scope::Design,
        status: DiffStatus::Accepted,
        diff_id: diff.id.to_string(),
        payload_hash: blake3::hash(payload.to_string().as_bytes())
            .to_hex()
            .to_string(),
        ts: chrono::Utc::now(),
        reason: String::new(),
    };
    let entry = logger.append(record).unwrap();
    assert!(!entry.hash.is_empty(), "audit hash must be non-empty");
    assert_ne!(
        logger.head(),
        first_head,
        "audit chain head must advance after append"
    );
    assert_eq!(logger.entry_count(), 1);
}

#[test]
fn style_assistant_diff_emits_furniture_material_and_lighting() {
    let plan = response(
        ToolName::StyleAssistant,
        json!({
            "furniture_ids": ["asset_sofa", "asset_armchair"],
            "material_ids": ["mat_oak"],
            "lighting_preset_id": "warm_evening",
        }),
    );
    let diff = DiffEngine::build(&plan);
    // 2 furniture + 1 material + 1 lighting preset = 4 ops, all Inserts.
    assert_eq!(diff.operations.len(), 4);
    let mut kinds: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for op in &diff.operations {
        match op {
            DiffOperation::Insert { entity_kind, .. } => {
                *kinds.entry(entity_kind.as_str()).or_default() += 1;
            }
            other => panic!("style assistant must only emit Insert ops, got {other:?}"),
        }
    }
    assert_eq!(kinds.get("furniture").copied(), Some(2));
    assert_eq!(kinds.get("material_binding").copied(), Some(1));
    assert_eq!(kinds.get("lighting_preset").copied(), Some(1));
}

#[test]
fn layout_suggestion_diff_carries_proposals() {
    let plan = response(
        ToolName::LayoutSuggestion,
        json!({
            "room_anchor": "ent_living_room",
            "proposals": [
                {
                    "asset_id": "asset_sofa_3s",
                    "position_mm": [1000.0, 2000.0, 0.0],
                    "rotation_deg": 90.0
                },
                {
                    "asset_id": "asset_coffee_table",
                    "position_mm": [1000.0, 3500.0, 0.0],
                    "rotation_deg": 0.0
                }
            ]
        }),
    );
    let diff = DiffEngine::build(&plan);
    assert_eq!(
        diff.operations.len(),
        2,
        "every layout proposal must become a diff operation"
    );
    // Layout suggestion can be either Insert (new furniture) or
    // Update (repositioning existing furniture); the contract is
    // that both kinds are allowed and at least one is present.
    assert!(diff.operations.iter().all(|op| matches!(
        op,
        DiffOperation::Insert { .. } | DiffOperation::Update { .. }
    )));
}

#[test]
fn audit_chain_advances_per_record() {
    let tmp = tempfile::tempdir().unwrap();
    let mut logger = AiAuditLogger::open(tmp.path().join("audit.log")).unwrap();
    let mut heads: Vec<String> = vec![logger.head().to_string()];

    for tool in [
        ToolName::PlanDetection,
        ToolName::StyleAssistant,
        ToolName::LayoutSuggestion,
        ToolName::RenderDoctor,
    ] {
        let r = AiAuditRecord {
            tool,
            scope: Scope::Design,
            status: DiffStatus::Accepted,
            diff_id: format!("diff-{}", tool.as_str()),
            payload_hash: blake3::hash(tool.as_str().as_bytes()).to_hex().to_string(),
            ts: chrono::Utc::now(),
            reason: String::new(),
        };
        logger.append(r).unwrap();
        heads.push(logger.head().to_string());
    }

    // Each head must be unique — the chain is strictly monotonic.
    let mut sorted = heads.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        heads.len(),
        "audit heads must be unique across the chain"
    );
    assert_eq!(logger.entry_count(), 4);
}
