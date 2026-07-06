// SPDX-License-Identifier: Apache-2.0

//! Semantic validation of a parsed `receipts.jsonl` stream, per SPEC.md "Lifecycle Receipts" ->
//! "Budget composition" and the OCF conformance fixtures under `ocf/conformance/`.

use std::collections::HashMap;
use std::fmt;

use crate::model::{BudgetEnvelope, Consumed, Receipt, SpawnReceipt};

/// One of the four budget dimensions a receipt envelope may declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetDimension {
    Tokens,
    ToolCalls,
    WallTimeMs,
    CostUsd,
}

impl BudgetDimension {
    /// All four dimensions, in the canonical display order used by every CLI verb.
    pub const ALL: [BudgetDimension; 4] = [
        BudgetDimension::Tokens,
        BudgetDimension::ToolCalls,
        BudgetDimension::WallTimeMs,
        BudgetDimension::CostUsd,
    ];

    /// The value of this dimension on a spawn's `budget` (the `max_*` side), if declared.
    ///
    /// Public so the CLI's `show` / `budget tree` rendering shares the exact same
    /// budget<->dimension mapping [`validate`] checks against, rather than re-deriving it.
    pub fn of_budget(self, budget: &BudgetEnvelope) -> Option<f64> {
        match self {
            BudgetDimension::Tokens => budget.max_tokens.map(|v| v as f64),
            BudgetDimension::ToolCalls => budget.max_tool_calls.map(|v| v as f64),
            BudgetDimension::WallTimeMs => budget.max_wall_time_ms.map(|v| v as f64),
            BudgetDimension::CostUsd => budget.max_cost_usd,
        }
    }

    /// The value of this dimension on a return's `consumed`, if reported.
    pub fn of_consumed(self, consumed: &Consumed) -> Option<f64> {
        match self {
            BudgetDimension::Tokens => consumed.tokens.map(|v| v as f64),
            BudgetDimension::ToolCalls => consumed.tool_calls.map(|v| v as f64),
            BudgetDimension::WallTimeMs => consumed.wall_time_ms.map(|v| v as f64),
            BudgetDimension::CostUsd => consumed.cost_usd,
        }
    }
}

impl fmt::Display for BudgetDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BudgetDimension::Tokens => "tokens",
            BudgetDimension::ToolCalls => "tool_calls",
            BudgetDimension::WallTimeMs => "wall_time_ms",
            BudgetDimension::CostUsd => "cost_usd",
        };
        f.write_str(s)
    }
}

/// Why an artifact failed the content/content_url XOR rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XorProblem {
    /// Both `content` and `content_url` were set.
    Both,
    /// Neither `content` nor `content_url` was set.
    Neither,
}

impl fmt::Display for XorProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XorProblem::Both => write!(f, "both content and content_url are set"),
            XorProblem::Neither => write!(f, "neither content nor content_url is set"),
        }
    }
}

/// A single semantic-validation failure. Every variant renders a human-readable message via
/// `Display`, and [`Violation::code`] gives a stable machine-checkable identifier — never a bare
/// string, per `docs/design.md`'s "typed rejection" invariant.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Violation {
    #[error("duplicate spawn receipt_id {receipt_id:?}")]
    DuplicateSpawnId { receipt_id: String },

    #[error("return {receipt_id:?} references unknown spawn_ref {spawn_ref:?}")]
    UnknownSpawnRef {
        receipt_id: String,
        spawn_ref: String,
    },

    #[error("spawn_ref {spawn_ref:?} has {} returns (expected exactly one): {receipt_ids:?}", receipt_ids.len())]
    MultipleReturns {
        spawn_ref: String,
        receipt_ids: Vec<String>,
    },

    #[error(
        "return {receipt_id:?} is over budget on {dimension}: consumed {used}, budget {limit}, \
         without a budget_* stop_reason"
    )]
    OverBudgetWithoutBudgetStop {
        receipt_id: String,
        dimension: BudgetDimension,
        used: f64,
        limit: f64,
    },

    #[error(
        "children of parent {parent:?} exceed its budget on {dimension}: sum {sum}, limit {limit}"
    )]
    ChildrenExceedParent {
        parent: String,
        dimension: BudgetDimension,
        sum: f64,
        limit: f64,
    },

    #[error("artifact {artifact:?} on return {receipt_id:?}: {problem}")]
    ArtifactContentXor {
        receipt_id: String,
        artifact: String,
        problem: XorProblem,
    },

    #[error(
        "spawn {receipt_id:?} budget has no known dimensions \
         (max_tokens/max_tool_calls/max_wall_time_ms/max_cost_usd)"
    )]
    MissingBudgetDimension { receipt_id: String },
}

impl Violation {
    /// A stable, machine-checkable identifier for this violation kind (SCREAMING_SNAKE_CASE).
    pub fn code(&self) -> &'static str {
        match self {
            Violation::DuplicateSpawnId { .. } => "DUPLICATE_SPAWN_ID",
            Violation::UnknownSpawnRef { .. } => "UNKNOWN_SPAWN_REF",
            Violation::MultipleReturns { .. } => "MULTIPLE_RETURNS",
            Violation::OverBudgetWithoutBudgetStop { .. } => "OVER_BUDGET_WITHOUT_BUDGET_STOP",
            Violation::ChildrenExceedParent { .. } => "CHILDREN_EXCEED_PARENT",
            Violation::ArtifactContentXor { .. } => "ARTIFACT_CONTENT_XOR",
            Violation::MissingBudgetDimension { .. } => "MISSING_BUDGET_DIMENSION",
        }
    }
}

/// Validate a parsed receipt stream against the SPEC.md "Lifecycle Receipts" invariants.
///
/// Checks performed (semantics identical to the OCF conformance fixtures):
/// - `DuplicateSpawnId` — the same `receipt_id` used by more than one spawn record.
/// - `UnknownSpawnRef` — a return's `spawn_ref` does not match any spawn's `receipt_id`.
/// - `MultipleReturns` — more than one return references the same `spawn_ref` (spec: "exactly
///   one return per spawn").
/// - `OverBudgetWithoutBudgetStop` — for a return whose spawn is known, any dimension where
///   `consumed` exceeds the spawn's declared `budget` is only allowed when `stop_reason` is one
///   of the four `budget_*` values ("the receipt reports reality, not the plan" — SPEC.md
///   "Reconciliation"). Dimensions absent from the spawn's budget are not checked.
/// - `ChildrenExceedParent` — for every spawn that is itself referenced as a `parent` by other
///   spawns (i.e. every non-root parent), the sum of its children's declared budget must not
///   exceed its own declared budget, per dimension. Only dimensions the parent declares are
///   checked; a child silent on a dimension contributes zero to that dimension's sum (it is still
///   bounded by whatever it does declare).
/// - `ArtifactContentXor` — a return artifact must set exactly one of `content` / `content_url`.
/// - `MissingBudgetDimension` — a spawn's budget declares none of the four known dimensions.
pub fn validate(records: &[Receipt]) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Index spawns by receipt_id; flag duplicates. Later duplicates are recorded but not
    // re-inserted, so the "first writer wins" for every subsequent lookup (spawn_ref resolution,
    // parent/child composition).
    let mut spawns: HashMap<&str, &SpawnReceipt> = HashMap::new();
    for record in records {
        if let Receipt::Spawn(spawn) = record {
            if spawns.contains_key(spawn.receipt_id.as_str()) {
                violations.push(Violation::DuplicateSpawnId {
                    receipt_id: spawn.receipt_id.clone(),
                });
            } else {
                spawns.insert(spawn.receipt_id.as_str(), spawn);
            }
        }
    }

    // MissingBudgetDimension: a spawn budget with none of the four known dimensions set.
    for spawn in spawns.values() {
        let has_any_dimension = BudgetDimension::ALL
            .iter()
            .any(|dim| dim.of_budget(&spawn.budget).is_some());
        if !has_any_dimension {
            violations.push(Violation::MissingBudgetDimension {
                receipt_id: spawn.receipt_id.clone(),
            });
        }
    }

    // ChildrenExceedParent: group spawns by parent, sum declared budgets, compare to the
    // parent's own declared budget. Skip the synthetic "root" parent — it is a sentinel string,
    // not an actual receipt, so it has no budget to compare against.
    let mut children_by_parent: HashMap<&str, Vec<&SpawnReceipt>> = HashMap::new();
    for spawn in spawns.values() {
        children_by_parent
            .entry(spawn.parent.as_str())
            .or_default()
            .push(spawn);
    }
    for (parent_id, children) in &children_by_parent {
        if *parent_id == "root" {
            continue;
        }
        let Some(parent) = spawns.get(parent_id) else {
            // Dangling parent reference (points at no known spawn) is not one of the checks this
            // function performs; there is nothing to compose against.
            continue;
        };
        for dim in BudgetDimension::ALL {
            let Some(limit) = dim.of_budget(&parent.budget) else {
                continue;
            };
            let sum: f64 = children
                .iter()
                .filter_map(|child| dim.of_budget(&child.budget))
                .sum();
            if sum > limit {
                violations.push(Violation::ChildrenExceedParent {
                    parent: (*parent_id).to_string(),
                    dimension: dim,
                    sum,
                    limit,
                });
            }
        }
    }

    // Returns: unknown spawn_ref, multiple returns per spawn_ref, over-budget-without-budget-stop,
    // and artifact content XOR.
    let mut returns_by_spawn_ref: HashMap<&str, Vec<&str>> = HashMap::new();
    for record in records {
        let Receipt::Return(ret) = record else {
            continue;
        };

        if !spawns.contains_key(ret.spawn_ref.as_str()) {
            violations.push(Violation::UnknownSpawnRef {
                receipt_id: ret.receipt_id.clone(),
                spawn_ref: ret.spawn_ref.clone(),
            });
        }
        returns_by_spawn_ref
            .entry(ret.spawn_ref.as_str())
            .or_default()
            .push(ret.receipt_id.as_str());

        if let Some(spawn) = spawns.get(ret.spawn_ref.as_str()) {
            for dim in BudgetDimension::ALL {
                let (Some(limit), Some(used)) =
                    (dim.of_budget(&spawn.budget), dim.of_consumed(&ret.consumed))
                else {
                    continue;
                };
                if used > limit && !ret.stop_reason.is_budget() {
                    violations.push(Violation::OverBudgetWithoutBudgetStop {
                        receipt_id: ret.receipt_id.clone(),
                        dimension: dim,
                        used,
                        limit,
                    });
                }
            }
        }

        if let Some(artifacts) = &ret.artifacts {
            for artifact in artifacts {
                if !artifact.satisfies_content_xor() {
                    let problem = if artifact.content.is_some() && artifact.content_url.is_some() {
                        XorProblem::Both
                    } else {
                        XorProblem::Neither
                    };
                    violations.push(Violation::ArtifactContentXor {
                        receipt_id: ret.receipt_id.clone(),
                        artifact: artifact.name.clone(),
                        problem,
                    });
                }
            }
        }
    }
    for (spawn_ref, receipt_ids) in returns_by_spawn_ref {
        if receipt_ids.len() > 1 {
            violations.push(Violation::MultipleReturns {
                spawn_ref: spawn_ref.to_string(),
                receipt_ids: receipt_ids.into_iter().map(str::to_string).collect(),
            });
        }
    }

    violations
}
