// SPDX-License-Identifier: Apache-2.0

//! `openhavn run` — govern an arbitrary command with a spawn/return receipt pair. The
//! harness-agnostic entry point: any command becomes visible to `receipts validate` / `show` /
//! `budget tree` without a bespoke per-harness adapter.

use std::path::PathBuf;
use std::process::Command as ChildCommand;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use openhavn_receipts::{
    BudgetEnvelope, Consumed, Receipt, ReceiptIdGen, ReceiptLog, ReturnReceipt, SpawnReceipt,
    StopReason,
};

use crate::cli::RunArgs;
use crate::render::truncate;

/// Applied when no `--budget-*` flag is given and `--fail-closed` was not requested: 24h of wall
/// time. Keeps every emitted spawn schema-valid (the OCF schema requires `budget` to declare at
/// least one dimension) without fabricating a token/tool-call/cost figure nobody measured.
const DEFAULT_MAX_WALL_TIME_MS: u64 = 86_400_000;

/// `openhavn run [flags] -- <command> [args...]`.
///
/// Returns the child's own exit code, mirrored (or 130 if it was killed by a signal). Refusing to
/// launch under `--fail-closed` is reported as an `Err` — `main()` turns that into exit code 2 —
/// and touches no filesystem state at all: no directory is created, no receipts file is written.
pub fn run(opts: RunArgs) -> Result<i32> {
    let RunArgs {
        role,
        harness,
        model,
        task,
        budget_tokens,
        budget_tool_calls,
        budget_time_ms,
        budget_cost,
        parent,
        receipts,
        run_id,
        fail_closed,
        command,
    } = opts;

    let Some((program, args)) = command.split_first() else {
        bail!("openhavn run: no command given after `--`");
    };

    let has_budget_dimension = budget_tokens.is_some()
        || budget_tool_calls.is_some()
        || budget_time_ms.is_some()
        || budget_cost.is_some();
    if fail_closed && !has_budget_dimension {
        bail!(
            "no budget dimension declared (--budget-tokens / --budget-tool-calls / \
             --budget-time-ms / --budget-cost) and --fail-closed was given; refusing to launch"
        );
    }

    let budget = if has_budget_dimension {
        BudgetEnvelope {
            max_tokens: budget_tokens,
            max_tool_calls: budget_tool_calls,
            max_wall_time_ms: budget_time_ms,
            max_cost_usd: budget_cost,
            ..Default::default()
        }
    } else {
        eprintln!(
            "warning: no budget declared — defaulting to max_wall_time_ms={DEFAULT_MAX_WALL_TIME_MS}"
        );
        BudgetEnvelope {
            max_wall_time_ms: Some(DEFAULT_MAX_WALL_TIME_MS),
            ..Default::default()
        }
    };

    // Everything above is pure validation/derivation; nothing on disk has been touched yet, so a
    // `--fail-closed` refusal (checked before this point) never creates a directory or a file.
    let run_id = run_id.unwrap_or_else(default_run_id);
    let receipts_path = receipts.unwrap_or_else(|| {
        PathBuf::from("./.openhavn/runs")
            .join(&run_id)
            .join("receipts.jsonl")
    });
    let log = ReceiptLog::new(receipts_path);
    if let Some(dir) = log.path().parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating receipts directory {}", dir.display()))?;
        }
    }

    let id_gen = ReceiptIdGen::new(&run_id);
    let harness = harness.unwrap_or_else(|| basename(program));
    let task_boundary = task.unwrap_or_else(|| truncate(&command.join(" "), 200));

    let spawn_id = id_gen.next_id();
    let spawn = SpawnReceipt {
        receipt_id: spawn_id.clone(),
        ts: now_ts(),
        parent: parent.unwrap_or_else(|| "root".to_string()),
        role,
        harness: Some(harness),
        model,
        task_boundary,
        budget,
        tool_allowlist: None,
        schema_hash: None,
        extra: Default::default(),
    };
    let max_wall_time_ms = spawn.budget.max_wall_time_ms;
    log.append(&Receipt::Spawn(spawn))
        .context("writing spawn receipt")?;

    let start = Instant::now();
    let status = ChildCommand::new(program)
        .args(args)
        .status()
        .with_context(|| format!("launching {program}"))?;
    let wall_time_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let exit_code = status.code().unwrap_or(130);
    let mut stop_reason = match status.code() {
        Some(0) => StopReason::Done,
        Some(_) => StopReason::Error,
        None => StopReason::Killed,
    };
    // Checked after completion, never preemptively enforced in this slice: a return that is over
    // budget on a dimension its spawn declared must carry a budget_* stop_reason, or
    // `openhavn_receipts::validate` flags it as `OverBudgetWithoutBudgetStop`. This keeps every
    // receipts stream `openhavn run` writes self-consistent with that invariant unconditionally.
    if max_wall_time_ms.is_some_and(|max| wall_time_ms > max) {
        stop_reason = StopReason::BudgetTime;
    }

    let ret = ReturnReceipt {
        receipt_id: id_gen.next_id(),
        ts: now_ts(),
        spawn_ref: spawn_id,
        stop_reason,
        consumed: Consumed {
            wall_time_ms: Some(wall_time_ms),
            ..Default::default()
        },
        distilled: None,
        artifacts: None,
        trace_ref: None,
        gate: None,
        extra: Default::default(),
    };
    log.append(&Receipt::Return(ret))
        .context("writing return receipt")?;

    eprintln!("receipts: {}", log.path().display());
    Ok(exit_code)
}

/// RFC3339, second precision, `Z` suffix — matches the `ts` style used throughout the shared
/// conformance fixtures (e.g. `"2026-07-05T21:00:00Z"`).
fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `run-<current UTC timestamp, compact>`, e.g. `run-20260705T210000Z`.
fn default_run_id() -> String {
    format!("run-{}", Utc::now().format("%Y%m%dT%H%M%SZ"))
}

fn basename(program: &str) -> String {
    std::path::Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_strips_directory_components() {
        assert_eq!(basename("/usr/bin/echo"), "echo");
        assert_eq!(basename("echo"), "echo");
        assert_eq!(basename("sh"), "sh");
    }

    #[test]
    fn default_run_id_has_expected_shape() {
        let id = default_run_id();
        assert!(id.starts_with("run-"), "{id}");
        assert!(id.ends_with('Z'), "{id}");
        assert_eq!(id.len(), "run-20260705T210000Z".len());
    }

    #[test]
    fn now_ts_matches_fixture_style_rfc3339_seconds_with_z() {
        let ts = now_ts();
        assert!(ts.ends_with('Z'), "{ts}");
        assert_eq!(ts.len(), "2026-07-05T21:00:00Z".len());
    }
}
