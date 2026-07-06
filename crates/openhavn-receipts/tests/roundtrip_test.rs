// SPDX-License-Identifier: Apache-2.0

//! Permissive-consumption round trips: unknown fields anywhere in a record (top level, nested
//! envelopes, gate, distilled, artifacts) must survive parse -> serialize -> parse unchanged
//! (SPEC.md "Permissive consumption": "producers may extend, consumers must tolerate").

use openhavn_receipts::Receipt;

fn roundtrip(line: &str) -> (Receipt, Receipt, String) {
    let first = Receipt::from_jsonl_line(line).expect("first parse must succeed");
    let serialized = serde_json::to_string(&first).expect("serialize must succeed");
    let second = Receipt::from_jsonl_line(&serialized).expect("second parse must succeed");
    (first, second, serialized)
}

#[test]
fn spawn_with_unknown_fields_roundtrips_byte_semantically() {
    let line = r#"{
        "kind": "spawn",
        "receipt_id": "rc_run9_000001",
        "ts": "2026-07-06T00:00:00Z",
        "parent": "root",
        "role": "worker",
        "harness": "claude-code",
        "task_boundary": "do the thing",
        "budget": {"max_tokens": 1000, "priority": "high"},
        "tool_allowlist": ["read", "edit"],
        "vendor_note": "unrecognized top-level field",
        "future_field": {"nested": [1, 2, 3]}
    }"#;

    let (first, second, serialized) = roundtrip(line);
    assert_eq!(
        first, second,
        "parse -> serialize -> parse must be a fixed point"
    );

    let Receipt::Spawn(spawn) = &first else {
        panic!("expected a spawn receipt");
    };
    assert_eq!(
        spawn.extra.get("vendor_note").and_then(|v| v.as_str()),
        Some("unrecognized top-level field")
    );
    assert_eq!(
        spawn.budget.extra.get("priority").and_then(|v| v.as_str()),
        Some("high")
    );

    // Also check the reserialized JSON itself still carries the unknown data (not just that the
    // Rust struct round-tripped through its own extra map).
    let reparsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(reparsed["vendor_note"], "unrecognized top-level field");
    assert_eq!(reparsed["budget"]["priority"], "high");
    assert_eq!(
        reparsed["future_field"]["nested"],
        serde_json::json!([1, 2, 3])
    );
}

#[test]
fn return_with_unknown_fields_roundtrips_byte_semantically() {
    let line = r#"{
        "kind": "return",
        "receipt_id": "rc_run9_000002",
        "ts": "2026-07-06T00:05:00Z",
        "spawn_ref": "rc_run9_000001",
        "stop_reason": "done",
        "consumed": {"tokens": 500, "custom_meter": 3.5},
        "distilled": {"tokens": 40, "ref": "inline://x", "confidence": 0.9},
        "gate": {"admitted": true, "reason": "ok", "reviewer": "policy-v2"},
        "artifacts": [
            {
                "name": "diff",
                "content_type": "text/x-diff",
                "content": "--- a\n+++ b\n",
                "metadata": {"lines_changed": 12}
            }
        ],
        "custom_trace_id": "abc-123"
    }"#;

    let (first, second, serialized) = roundtrip(line);
    assert_eq!(
        first, second,
        "parse -> serialize -> parse must be a fixed point"
    );

    let Receipt::Return(ret) = &first else {
        panic!("expected a return receipt");
    };
    assert_eq!(
        ret.extra.get("custom_trace_id").and_then(|v| v.as_str()),
        Some("abc-123")
    );
    assert_eq!(
        ret.consumed
            .extra
            .get("custom_meter")
            .and_then(|v| v.as_f64()),
        Some(3.5)
    );
    assert_eq!(
        ret.distilled
            .as_ref()
            .and_then(|d| d.extra.get("confidence"))
            .and_then(|v| v.as_f64()),
        Some(0.9)
    );
    assert_eq!(
        ret.gate
            .as_ref()
            .and_then(|g| g.extra.get("reviewer"))
            .and_then(|v| v.as_str()),
        Some("policy-v2")
    );
    let artifact = &ret.artifacts.as_ref().unwrap()[0];
    assert_eq!(
        artifact
            .metadata
            .as_ref()
            .and_then(|m| m.get("lines_changed"))
            .and_then(|v| v.as_i64()),
        Some(12)
    );

    let reparsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(reparsed["custom_trace_id"], "abc-123");
    assert_eq!(reparsed["consumed"]["custom_meter"], 3.5);
    assert_eq!(reparsed["distilled"]["confidence"], 0.9);
    assert_eq!(reparsed["gate"]["reviewer"], "policy-v2");
    assert_eq!(reparsed["artifacts"][0]["metadata"]["lines_changed"], 12);
}
