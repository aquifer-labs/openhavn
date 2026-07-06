// SPDX-License-Identifier: Apache-2.0

//! `openhavn budget tree`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use openhavn_receipts::{BudgetDimension, Violation};

use crate::render::{all_nodes, fmt_num, role_at_harness, truncate, Node};

use super::load;

/// `openhavn budget tree <path>`. Budget composition + fleet observability: per node,
/// granted -> consumed per dimension and context-efficiency; rolled-up totals; over-allocation
/// flags (reusing the exact same check `receipts validate` runs, so the two verbs never
/// disagree).
pub fn tree(path: &Path) -> Result<i32> {
    let records = load(path)?;
    let forest = crate::render::build_forest(&records);

    let violations = openhavn_receipts::validate(&records);
    let mut over_allocated: HashMap<(&str, BudgetDimension), (f64, f64)> = HashMap::new();
    for violation in &violations {
        if let Violation::ChildrenExceedParent {
            parent,
            dimension,
            sum,
            limit,
        } = violation
        {
            over_allocated.insert((parent.as_str(), *dimension), (*sum, *limit));
        }
    }

    for root in &forest {
        print_node(root, 0, &over_allocated);
    }

    print_totals(&forest);
    Ok(0)
}

fn print_node(
    node: &Node<'_>,
    depth: usize,
    over_allocated: &HashMap<(&str, BudgetDimension), (f64, f64)>,
) {
    let indent = "  ".repeat(depth);
    println!(
        "{indent}{}  {}  \"{}\"",
        node.spawn.receipt_id,
        role_at_harness(node.spawn),
        truncate(&node.spawn.task_boundary, 60),
    );

    for dim in BudgetDimension::ALL {
        let Some(granted) = dim.of_budget(&node.spawn.budget) else {
            continue;
        };
        let consumed = node.ret.and_then(|ret| dim.of_consumed(&ret.consumed));
        let consumed_str = consumed.map(fmt_num).unwrap_or_else(|| "-".to_string());
        let flag = match over_allocated.get(&(node.spawn.receipt_id.as_str(), dim)) {
            Some((sum, limit)) => format!(
                "  [OVER-ALLOCATED: children sum {} > limit {}]",
                fmt_num(*sum),
                fmt_num(*limit)
            ),
            None => String::new(),
        };
        println!(
            "{indent}  {dim}: granted={} -> consumed={consumed_str}{flag}",
            fmt_num(granted)
        );
    }

    if let Some(ret) = node.ret {
        if let (Some(distilled_tokens), Some(consumed_tokens)) = (
            ret.distilled.as_ref().and_then(|d| d.tokens),
            ret.consumed.tokens,
        ) {
            if consumed_tokens > 0 {
                let efficiency = distilled_tokens as f64 / consumed_tokens as f64;
                println!(
                    "{indent}  context-efficiency: {:.2}% ({distilled_tokens}/{consumed_tokens})",
                    efficiency * 100.0
                );
            }
        }
    }

    for child in &node.children {
        print_node(child, depth + 1, over_allocated);
    }
}

fn print_totals(forest: &[Node<'_>]) {
    println!("TOTAL");
    for dim in BudgetDimension::ALL {
        let granted: f64 = forest
            .iter()
            .filter_map(|root| dim.of_budget(&root.spawn.budget))
            .sum();
        if granted > 0.0 {
            println!("  {dim}: granted(top-level)={}", fmt_num(granted));
        }
    }
    let nodes = all_nodes(forest);
    for dim in BudgetDimension::ALL {
        let consumed: f64 = nodes
            .iter()
            .filter_map(|node| node.ret.and_then(|ret| dim.of_consumed(&ret.consumed)))
            .sum();
        if consumed > 0.0 {
            println!("  {dim}: consumed(fleet)={}", fmt_num(consumed));
        }
    }
}
