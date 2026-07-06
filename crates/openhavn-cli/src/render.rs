// SPDX-License-Identifier: Apache-2.0

//! Shared rendering helpers for `receipts show` and `budget tree`: turning a flat
//! `Vec<Receipt>` into a spawn tree, and small text-formatting utilities.

use std::collections::{HashMap, HashSet};

use openhavn_receipts::{Receipt, ReturnReceipt, SpawnReceipt};

/// One spawn node in the rendered fleet tree, with its (at most one) return and its children,
/// in the order they appeared in the receipts log.
pub struct Node<'a> {
    pub spawn: &'a SpawnReceipt,
    pub ret: Option<&'a ReturnReceipt>,
    /// `Some(parent_id)` only when this node is a root because its declared parent does not
    /// resolve to any known spawn (and is not the literal `"root"` sentinel) — an orphan.
    pub orphan_parent: Option<&'a str>,
    pub children: Vec<Node<'a>>,
}

/// Build the fleet forest: one tree per top-level spawn (`parent == "root"`, or an unresolvable
/// `parent` — rendered as an orphan root rather than silently dropped).
///
/// This is a rendering helper, not a validator: it never rejects malformed input. Duplicate
/// spawn ids and multiple returns for one spawn are handled the same "first one wins" way
/// [`openhavn_receipts::validate`] resolves them for its own checks, so `show` / `budget tree`
/// stay consistent with what `receipts validate` reports.
pub fn build_forest(records: &[Receipt]) -> Vec<Node<'_>> {
    let mut spawns_by_id: HashMap<&str, &SpawnReceipt> = HashMap::new();
    let mut spawn_order: Vec<&str> = Vec::new();
    for record in records {
        if let Receipt::Spawn(spawn) = record {
            if !spawns_by_id.contains_key(spawn.receipt_id.as_str()) {
                spawns_by_id.insert(spawn.receipt_id.as_str(), spawn);
                spawn_order.push(spawn.receipt_id.as_str());
            }
        }
    }

    let mut returns_by_spawn_ref: HashMap<&str, &ReturnReceipt> = HashMap::new();
    for record in records {
        if let Receipt::Return(ret) = record {
            returns_by_spawn_ref
                .entry(ret.spawn_ref.as_str())
                .or_insert(ret);
        }
    }

    let mut children_by_parent: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut root_ids: Vec<&str> = Vec::new();
    for id in &spawn_order {
        let parent = spawns_by_id[id].parent.as_str();
        if parent == "root" || !spawns_by_id.contains_key(parent) {
            root_ids.push(id);
        } else {
            children_by_parent.entry(parent).or_default().push(id);
        }
    }

    let mut visited: HashSet<&str> = HashSet::new();
    root_ids
        .into_iter()
        .filter_map(|id| {
            build_node(
                id,
                &spawns_by_id,
                &returns_by_spawn_ref,
                &children_by_parent,
                &mut visited,
            )
        })
        .collect()
}

fn build_node<'a>(
    id: &'a str,
    spawns_by_id: &HashMap<&'a str, &'a SpawnReceipt>,
    returns_by_spawn_ref: &HashMap<&'a str, &'a ReturnReceipt>,
    children_by_parent: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
) -> Option<Node<'a>> {
    if !visited.insert(id) {
        // Defense in depth: a spawn's `parent` is single-valued, so a node can only ever be
        // queued under one parent's children — this should be unreachable, not a real cycle.
        return None;
    }
    let spawn = *spawns_by_id.get(id)?;
    let ret = returns_by_spawn_ref.get(id).copied();
    let orphan_parent =
        if spawn.parent != "root" && !spawns_by_id.contains_key(spawn.parent.as_str()) {
            Some(spawn.parent.as_str())
        } else {
            None
        };
    let children = children_by_parent
        .get(id)
        .into_iter()
        .flatten()
        .filter_map(|child_id| {
            build_node(
                child_id,
                spawns_by_id,
                returns_by_spawn_ref,
                children_by_parent,
                visited,
            )
        })
        .collect();
    Some(Node {
        spawn,
        ret,
        orphan_parent,
        children,
    })
}

/// Flatten a forest into every node, parents before their children, depth-first — used to roll
/// up fleet-wide totals in `budget tree`.
pub fn all_nodes<'a>(forest: &'a [Node<'a>]) -> Vec<&'a Node<'a>> {
    let mut out = Vec::new();
    fn walk<'a>(node: &'a Node<'a>, out: &mut Vec<&'a Node<'a>>) {
        out.push(node);
        for child in &node.children {
            walk(child, out);
        }
    }
    for root in forest {
        walk(root, &mut out);
    }
    out
}

/// Truncate `s` to at most `max_chars` characters (UTF-8 safe), appending `…` when truncated.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

/// `role@harness`, defaulting either side to `?` when the (optional) field is absent.
pub fn role_at_harness(spawn: &SpawnReceipt) -> String {
    format!(
        "{}@{}",
        spawn.role.as_deref().unwrap_or("?"),
        spawn.harness.as_deref().unwrap_or("?")
    )
}

/// Render a number with no trailing `.0` for whole values, so token/tool-call counts don't print
/// as `61212.0` even though the underlying comparison is done in `f64` (see
/// `openhavn_receipts::BudgetDimension`).
pub fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_is_unchanged() {
        assert_eq!(truncate("short", 60), "short");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis_and_60_char_budget() {
        let long = "x".repeat(100);
        let truncated = truncate(&long, 60);
        assert_eq!(truncated.chars().count(), 60);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn fmt_num_drops_trailing_zero() {
        assert_eq!(fmt_num(61212.0), "61212");
        assert_eq!(fmt_num(0.67), "0.67");
    }
}
