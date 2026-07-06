// SPDX-License-Identifier: Apache-2.0

//! Shared DATA (not text) representation of the fleet spawn tree, built on top of the same
//! `render::Node` forest the CLI's text renderers (`commands::receipts::show`,
//! `commands::budget::tree`) walk. Consumed by the `receipts.show` / `budget.tree` MCP tools in
//! `commands::mcp`.
//!
//! Render-vs-data separation: this module only ever produces `serde_json::Value` trees; it never
//! prints. The CLI's text output continues to be rendered directly from `render::Node` in
//! `commands/receipts.rs` / `commands/budget.rs`, independently of this module, so the text path
//! never pays for a JSON round trip.

use serde_json::{json, Map, Value};

use openhavn_receipts::BudgetDimension;

use crate::render::{all_nodes, Node};

/// One spawn node as DATA: `{receipt_id, role, harness, task_boundary, stop_reason, budget,
/// consumed, children}` (plus `orphan_parent` when the node's declared parent doesn't resolve).
/// Used by the `receipts.show` MCP tool.
pub fn node_to_json(node: &Node<'_>) -> Value {
    build_node_json(node, false)
}

/// [`node_to_json`] plus a per-node `context_efficiency` (`distilled.tokens / consumed.tokens`,
/// when both are known and `consumed.tokens > 0`). Used by the `budget.tree` MCP tool.
pub fn node_to_budget_json(node: &Node<'_>) -> Value {
    build_node_json(node, true)
}

fn build_node_json(node: &Node<'_>, with_efficiency: bool) -> Value {
    let mut map = Map::new();
    map.insert("receipt_id".to_string(), json!(node.spawn.receipt_id));
    map.insert("role".to_string(), json!(node.spawn.role));
    map.insert("harness".to_string(), json!(node.spawn.harness));
    map.insert("task_boundary".to_string(), json!(node.spawn.task_boundary));
    map.insert(
        "stop_reason".to_string(),
        match node.ret {
            Some(ret) => json!(ret.stop_reason.to_string()),
            None => json!("running"),
        },
    );
    map.insert(
        "budget".to_string(),
        dimension_map(|dim| dim.of_budget(&node.spawn.budget)),
    );
    map.insert(
        "consumed".to_string(),
        match node.ret {
            Some(ret) => dimension_map(|dim| dim.of_consumed(&ret.consumed)),
            None => Value::Null,
        },
    );
    if let Some(parent) = node.orphan_parent {
        map.insert("orphan_parent".to_string(), json!(parent));
    }
    if with_efficiency {
        if let Some(efficiency) = context_efficiency(node) {
            map.insert("context_efficiency".to_string(), json!(efficiency));
        }
    }
    let children: Vec<Value> = node
        .children
        .iter()
        .map(|child| build_node_json(child, with_efficiency))
        .collect();
    map.insert("children".to_string(), Value::Array(children));
    Value::Object(map)
}

/// Build a `{dimension_name: value}` object from a per-dimension accessor, skipping dimensions
/// the accessor returns `None` for — shared by budget/consumed node fields and the fleet totals
/// below so all four call sites stay in lockstep with `BudgetDimension::ALL`.
fn dimension_map(get: impl Fn(BudgetDimension) -> Option<f64>) -> Value {
    let mut map = Map::new();
    for dim in BudgetDimension::ALL {
        if let Some(value) = get(dim) {
            map.insert(dim.to_string(), json!(value));
        }
    }
    Value::Object(map)
}

fn context_efficiency(node: &Node<'_>) -> Option<f64> {
    let ret = node.ret?;
    let distilled_tokens = ret.distilled.as_ref()?.tokens?;
    let consumed_tokens = ret.consumed.tokens?;
    if consumed_tokens == 0 {
        return None;
    }
    Some(distilled_tokens as f64 / consumed_tokens as f64)
}

/// Fleet-wide totals for `budget.tree`: `{granted_top_level: {...}, consumed_fleet: {...}}`,
/// mirroring `commands::budget::print_totals`'s text rendering exactly (granted is summed over
/// top-level roots only; consumed is summed over every node in the forest).
pub fn fleet_totals_json(forest: &[Node<'_>]) -> Value {
    let granted = dimension_map(|dim| {
        let sum: f64 = forest
            .iter()
            .filter_map(|root| dim.of_budget(&root.spawn.budget))
            .sum();
        (sum > 0.0).then_some(sum)
    });
    let nodes = all_nodes(forest);
    let consumed = dimension_map(|dim| {
        let sum: f64 = nodes
            .iter()
            .filter_map(|node| node.ret.and_then(|ret| dim.of_consumed(&ret.consumed)))
            .sum();
        (sum > 0.0).then_some(sum)
    });
    json!({ "granted_top_level": granted, "consumed_fleet": consumed })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::render::build_forest;

    fn load_forest(name: &str) -> Vec<Node<'static>> {
        // Leak the parsed records for the lifetime of the test process — simplest way to hand
        // back a `Node<'static>` forest from a helper without fighting borrow lifetimes in tests.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../openhavn-receipts/tests/fixtures")
            .join(name);
        let text = std::fs::read_to_string(path).unwrap();
        let records = openhavn_receipts::parse_jsonl(&text).unwrap();
        let records: &'static [openhavn_receipts::Receipt] = Box::leak(records.into_boxed_slice());
        build_forest(records)
    }

    #[test]
    fn node_to_json_marks_return_less_root_as_running_with_two_children() {
        let forest = load_forest("valid.jsonl");
        assert_eq!(forest.len(), 1);
        let value = node_to_json(&forest[0]);
        assert_eq!(value["receipt_id"], "rc_run1_000001");
        assert_eq!(value["stop_reason"], "running");
        assert_eq!(value["consumed"], Value::Null);
        let children = value["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["stop_reason"], "done");
        assert_eq!(children[1]["stop_reason"], "budget_tokens");
        assert_eq!(children[0]["budget"]["tokens"], 80000.0);
    }

    #[test]
    fn node_to_budget_json_adds_context_efficiency_on_returned_nodes_only() {
        let forest = load_forest("valid.jsonl");
        let value = node_to_budget_json(&forest[0]);
        assert!(
            value.get("context_efficiency").is_none(),
            "root has no return yet"
        );
        let child = &value["children"][0];
        let efficiency = child["context_efficiency"].as_f64().unwrap();
        assert!((efficiency - (410.0 / 61212.0)).abs() < 1e-9);
    }

    #[test]
    fn fleet_totals_json_sums_top_level_granted_and_fleet_wide_consumed() {
        let forest = load_forest("valid.jsonl");
        let totals = fleet_totals_json(&forest);
        assert_eq!(totals["granted_top_level"]["tokens"], 200000.0);
        assert_eq!(totals["granted_top_level"]["tool_calls"], 40.0);
        assert_eq!(totals["consumed_fleet"]["tokens"], 121756.0);
        assert_eq!(totals["consumed_fleet"]["tool_calls"], 19.0);
    }
}
