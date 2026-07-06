// SPDX-License-Identifier: Apache-2.0

//! `StopReason` serde strings must match the OCF schema enum in
//! `schema/receipts.schema.json` (`$defs` is inline on the `return` branch) exactly.

use openhavn_receipts::StopReason;

const EXPECTED: &[(StopReason, &str)] = &[
    (StopReason::Done, "done"),
    (StopReason::BudgetTokens, "budget_tokens"),
    (StopReason::BudgetToolCalls, "budget_tool_calls"),
    (StopReason::BudgetTime, "budget_time"),
    (StopReason::BudgetCost, "budget_cost"),
    (StopReason::GateRejected, "gate_rejected"),
    (StopReason::Error, "error"),
    (StopReason::Killed, "killed"),
];

#[test]
fn stop_reason_serializes_to_the_ocf_schema_strings() {
    for (variant, expected) in EXPECTED {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, serde_json::json!(expected), "variant {variant:?}");
    }
}

#[test]
fn stop_reason_deserializes_from_the_ocf_schema_strings() {
    for (variant, s) in EXPECTED {
        let parsed: StopReason = serde_json::from_value(serde_json::json!(s)).unwrap();
        assert_eq!(parsed, *variant);
    }
}

#[test]
fn stop_reason_display_matches_serde_strings() {
    for (variant, expected) in EXPECTED {
        assert_eq!(&variant.to_string(), expected);
    }
}

#[test]
fn stop_reason_rejects_unknown_strings() {
    let err = serde_json::from_value::<StopReason>(serde_json::json!("not_a_reason"));
    assert!(err.is_err());
}

#[test]
fn is_budget_matches_the_four_budget_variants() {
    for (variant, s) in EXPECTED {
        let expected = s.starts_with("budget_");
        assert_eq!(variant.is_budget(), expected, "variant {variant:?}");
    }
}
