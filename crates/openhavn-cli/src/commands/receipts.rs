// SPDX-License-Identifier: Apache-2.0

//! `openhavn receipts validate|show`.

use std::path::Path;

use anyhow::Result;
use openhavn_receipts::{BudgetDimension, Receipt};

use crate::render::{fmt_num, role_at_harness, truncate, Node};

use super::load;

/// `openhavn receipts validate <path>`. Returns the process exit code (0 = clean, 1 = violations
/// found) — never fails the process on a *semantic* violation, only on I/O / parse errors.
pub fn validate(path: &Path) -> Result<i32> {
    let records = load(path)?;
    let violations = openhavn_receipts::validate(&records);

    if violations.is_empty() {
        let spawns = records
            .iter()
            .filter(|r| matches!(r, Receipt::Spawn(_)))
            .count();
        let returns = records
            .iter()
            .filter(|r| matches!(r, Receipt::Return(_)))
            .count();
        println!(
            "ok — {} records, {} spawns, {} returns",
            records.len(),
            spawns,
            returns
        );
        Ok(0)
    } else {
        for violation in &violations {
            println!("{} — {}", violation.code(), violation);
        }
        Ok(1)
    }
}

/// `openhavn receipts show <path>`. Renders the spawn tree, indented by parent chain.
pub fn show(path: &Path) -> Result<i32> {
    let records = load(path)?;
    let forest = crate::render::build_forest(&records);
    for root in &forest {
        print_node(root, 0);
    }
    Ok(0)
}

fn print_node(node: &Node<'_>, depth: usize) {
    let indent = "  ".repeat(depth);
    let stop = match node.ret {
        Some(ret) => ret.stop_reason.to_string(),
        None => "RUNNING".to_string(),
    };

    let mut dims = Vec::new();
    for dim in BudgetDimension::ALL {
        let Some(limit) = dim.of_budget(&node.spawn.budget) else {
            continue;
        };
        let used = node.ret.and_then(|ret| dim.of_consumed(&ret.consumed));
        let used_str = used.map(fmt_num).unwrap_or_else(|| "-".to_string());
        dims.push(format!("{dim}={used_str}/{}", fmt_num(limit)));
    }
    let dims_str = if dims.is_empty() {
        String::new()
    } else {
        format!("  {}", dims.join(" "))
    };
    let orphan_note = node
        .orphan_parent
        .map(|parent| format!("  (orphan, unknown parent {parent:?})"))
        .unwrap_or_default();

    println!(
        "{indent}{}  {}  \"{}\"  {stop}{dims_str}{orphan_note}",
        node.spawn.receipt_id,
        role_at_harness(node.spawn),
        truncate(&node.spawn.task_boundary, 60),
    );

    for child in &node.children {
        print_node(child, depth + 1);
    }
}
