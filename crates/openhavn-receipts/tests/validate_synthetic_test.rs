// SPDX-License-Identifier: Apache-2.0

//! Synthetic (hand-constructed) cases for violations the two fixture files don't exercise:
//! artifact content XOR and children-exceed-parent budget composition.

use openhavn_receipts::{
    Artifact, BudgetDimension, BudgetEnvelope, Consumed, Receipt, ReturnReceipt, SpawnReceipt,
    StopReason, Violation, XorProblem,
};

fn spawn(receipt_id: &str, parent: &str, max_tokens: Option<u64>) -> Receipt {
    Receipt::Spawn(SpawnReceipt {
        receipt_id: receipt_id.to_string(),
        ts: "2026-07-06T00:00:00Z".to_string(),
        parent: parent.to_string(),
        role: None,
        harness: None,
        model: None,
        task_boundary: "test boundary".to_string(),
        budget: BudgetEnvelope {
            max_tokens,
            ..Default::default()
        },
        tool_allowlist: None,
        schema_hash: None,
        extra: Default::default(),
    })
}

#[test]
fn children_exceed_parent_budget_is_detected() {
    let parent = spawn("p1", "root", Some(100));
    let child_a = spawn("c1", "p1", Some(60));
    let child_b = spawn("c2", "p1", Some(70));
    let records = vec![parent, child_a, child_b];

    let violations = openhavn_receipts::validate(&records);
    assert_eq!(
        violations.len(),
        1,
        "expected exactly one violation, got: {violations:#?}"
    );
    match &violations[0] {
        Violation::ChildrenExceedParent {
            parent,
            dimension,
            sum,
            limit,
        } => {
            assert_eq!(parent, "p1");
            assert_eq!(*dimension, BudgetDimension::Tokens);
            assert_eq!(*sum, 130.0);
            assert_eq!(*limit, 100.0);
        }
        other => panic!("expected ChildrenExceedParent, got {other:?}"),
    }
    assert_eq!(violations[0].code(), "CHILDREN_EXCEED_PARENT");
}

#[test]
fn children_within_parent_budget_is_not_flagged() {
    let parent = spawn("p1", "root", Some(100));
    let child_a = spawn("c1", "p1", Some(40));
    let child_b = spawn("c2", "p1", Some(40));
    let records = vec![parent, child_a, child_b];

    let violations = openhavn_receipts::validate(&records);
    assert!(violations.is_empty(), "got: {violations:#?}");
}

#[test]
fn artifact_content_xor_is_detected_for_both_and_neither() {
    let sp = spawn("p1", "root", Some(1000));
    let ret = Receipt::Return(ReturnReceipt {
        receipt_id: "r1".to_string(),
        ts: "2026-07-06T00:01:00Z".to_string(),
        spawn_ref: "p1".to_string(),
        stop_reason: StopReason::Done,
        consumed: Consumed {
            tokens: Some(10),
            ..Default::default()
        },
        distilled: None,
        artifacts: Some(vec![
            Artifact {
                name: "both-set".to_string(),
                content_type: "text/plain".to_string(),
                content: Some("x".to_string()),
                content_url: Some("http://example/y".to_string()),
                content_encoding: None,
                metadata: None,
                extra: Default::default(),
            },
            Artifact {
                name: "neither-set".to_string(),
                content_type: "text/plain".to_string(),
                content: None,
                content_url: None,
                content_encoding: None,
                metadata: None,
                extra: Default::default(),
            },
            Artifact {
                name: "valid".to_string(),
                content_type: "text/plain".to_string(),
                content: Some("ok".to_string()),
                content_url: None,
                content_encoding: None,
                metadata: None,
                extra: Default::default(),
            },
        ]),
        trace_ref: None,
        gate: None,
        extra: Default::default(),
    });
    let records = vec![sp, ret];

    let mut violations = openhavn_receipts::validate(&records);
    assert_eq!(
        violations.len(),
        2,
        "expected exactly two violations, got: {violations:#?}"
    );
    violations.sort_by_key(|v| format!("{v:?}"));

    let mut saw_both = false;
    let mut saw_neither = false;
    for v in &violations {
        match v {
            Violation::ArtifactContentXor {
                receipt_id,
                artifact,
                problem,
            } => {
                assert_eq!(receipt_id, "r1");
                match problem {
                    XorProblem::Both => {
                        assert_eq!(artifact, "both-set");
                        saw_both = true;
                    }
                    XorProblem::Neither => {
                        assert_eq!(artifact, "neither-set");
                        saw_neither = true;
                    }
                }
            }
            other => panic!("expected ArtifactContentXor, got {other:?}"),
        }
        assert_eq!(v.code(), "ARTIFACT_CONTENT_XOR");
    }
    assert!(saw_both && saw_neither);
}

#[test]
fn missing_budget_dimension_is_detected() {
    let sp = Receipt::Spawn(SpawnReceipt {
        receipt_id: "p1".to_string(),
        ts: "2026-07-06T00:00:00Z".to_string(),
        parent: "root".to_string(),
        role: None,
        harness: None,
        model: None,
        task_boundary: "test boundary".to_string(),
        budget: BudgetEnvelope::default(),
        tool_allowlist: None,
        schema_hash: None,
        extra: Default::default(),
    });
    let violations = openhavn_receipts::validate(&[sp]);
    assert_eq!(violations.len(), 1);
    assert!(matches!(
        violations[0],
        Violation::MissingBudgetDimension { .. }
    ));
    assert_eq!(violations[0].code(), "MISSING_BUDGET_DIMENSION");
}

#[test]
fn duplicate_spawn_id_unknown_spawn_ref_and_multiple_returns_are_detected() {
    let dup_a = spawn("dup", "root", Some(10));
    let dup_b = spawn("dup", "root", Some(20));
    let ret_unknown = Receipt::Return(ReturnReceipt {
        receipt_id: "r-unknown".to_string(),
        ts: "2026-07-06T00:02:00Z".to_string(),
        spawn_ref: "does-not-exist".to_string(),
        stop_reason: StopReason::Done,
        consumed: Consumed::default(),
        distilled: None,
        artifacts: None,
        trace_ref: None,
        gate: None,
        extra: Default::default(),
    });
    let ret_1 = Receipt::Return(ReturnReceipt {
        receipt_id: "r-1".to_string(),
        ts: "2026-07-06T00:03:00Z".to_string(),
        spawn_ref: "dup".to_string(),
        stop_reason: StopReason::Done,
        consumed: Consumed::default(),
        distilled: None,
        artifacts: None,
        trace_ref: None,
        gate: None,
        extra: Default::default(),
    });
    let ret_2 = Receipt::Return(ReturnReceipt {
        receipt_id: "r-2".to_string(),
        ts: "2026-07-06T00:04:00Z".to_string(),
        spawn_ref: "dup".to_string(),
        stop_reason: StopReason::Done,
        consumed: Consumed::default(),
        distilled: None,
        artifacts: None,
        trace_ref: None,
        gate: None,
        extra: Default::default(),
    });
    let records = vec![dup_a, dup_b, ret_unknown, ret_1, ret_2];

    let violations = openhavn_receipts::validate(&records);
    let codes: Vec<&str> = violations.iter().map(Violation::code).collect();
    assert!(codes.contains(&"DUPLICATE_SPAWN_ID"), "{codes:?}");
    assert!(codes.contains(&"UNKNOWN_SPAWN_REF"), "{codes:?}");
    assert!(codes.contains(&"MULTIPLE_RETURNS"), "{codes:?}");
}
