// SPDX-License-Identifier: Apache-2.0

//! Validation against the OCF conformance fixtures (aquifer-labs/ocf `conformance/`).

use openhavn_receipts::{parse_jsonl, validate, Receipt, Violation};

const VALID: &str = include_str!("fixtures/valid.jsonl");
const OVER_BUDGET: &str = include_str!("fixtures/over-budget.jsonl");

#[test]
fn valid_fixture_has_zero_violations_and_correct_counts() {
    let records = parse_jsonl(VALID).expect("valid fixture must parse");
    assert_eq!(records.len(), 5, "5 lines in the fixture");

    let spawns = records
        .iter()
        .filter(|r| matches!(r, Receipt::Spawn(_)))
        .count();
    let returns = records
        .iter()
        .filter(|r| matches!(r, Receipt::Return(_)))
        .count();
    assert_eq!(spawns, 3);
    assert_eq!(returns, 2);

    let violations = validate(&records);
    assert!(
        violations.is_empty(),
        "expected zero violations, got: {violations:#?}"
    );
}

#[test]
fn over_budget_fixture_has_exactly_one_over_budget_violation() {
    let records = parse_jsonl(OVER_BUDGET).expect("over-budget fixture must parse");
    assert_eq!(records.len(), 2);

    let violations = validate(&records);
    assert_eq!(
        violations.len(),
        1,
        "expected exactly one violation, got: {violations:#?}"
    );
    match &violations[0] {
        Violation::OverBudgetWithoutBudgetStop {
            receipt_id,
            dimension,
            used,
            limit,
        } => {
            assert_eq!(receipt_id, "rc_run2_000002");
            assert_eq!(dimension.to_string(), "tokens");
            assert_eq!(*used, 15000.0);
            assert_eq!(*limit, 10000.0);
        }
        other => panic!("expected OverBudgetWithoutBudgetStop, got {other:?}"),
    }
    assert_eq!(violations[0].code(), "OVER_BUDGET_WITHOUT_BUDGET_STOP");
}
