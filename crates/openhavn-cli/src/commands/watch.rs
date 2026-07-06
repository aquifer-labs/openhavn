// SPDX-License-Identifier: Apache-2.0

//! `openhavn watch <path>` — tail a receipts.jsonl stream (or a directory of them) and print new
//! records and violations as they're appended. No filesystem-notification dependency: a plain
//! 500ms poll loop, matching this project's offline, dependency-light design.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use openhavn_receipts::{Consumed, Receipt, SpawnReceipt};

use crate::render::{fmt_num, role_at_harness, truncate};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_DEPTH: usize = 3;

/// Per-file poll state, so repeated passes only print what changed.
struct FileState {
    /// Number of records already printed from this file.
    record_count: usize,
    /// `Violation::to_string()` of every violation already printed for this file — the dedupe
    /// key the spec calls for ("dedupe already-printed ones by violation Display string").
    printed_violations: HashSet<String>,
    /// Last-printed open-spawn count, so it's only reprinted on change.
    last_open_spawns: Option<usize>,
}

impl FileState {
    fn new() -> Self {
        Self {
            record_count: 0,
            printed_violations: HashSet::new(),
            last_open_spawns: None,
        }
    }
}

/// `openhavn watch <path> [--once]`.
///
/// With `--once`: single pass, then returns 0 if no violations were found across any watched
/// file, 1 otherwise (CI mode). Without it: polls every 500ms until interrupted (default Ctrl-C
/// behavior is fine — nothing to override here), so the function never returns.
pub fn watch(path: &Path, once: bool) -> Result<i32> {
    let mut states: BTreeMap<PathBuf, FileState> = BTreeMap::new();

    loop {
        for file in discover_files(path)? {
            states.entry(file).or_insert_with(FileState::new);
        }

        let mut any_violations = false;
        for (file_path, state) in states.iter_mut() {
            if !file_path.is_file() {
                // Not there yet (or no longer there) — a later rescan may pick it back up.
                continue;
            }
            match process_file(file_path, state) {
                Ok(has_violations) => any_violations |= has_violations,
                Err(err) => eprintln!("warning: {}: {err:#}", file_path.display()),
            }
        }

        if once {
            return Ok(i32::from(any_violations));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Read, parse, and report on one file's current contents against `state`. Returns whether the
/// current full record set has any violations (used for `--once`'s exit code).
fn process_file(path: &Path, state: &mut FileState) -> Result<bool> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let records = openhavn_receipts::parse_jsonl(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    let label = path.display().to_string();

    if records.len() > state.record_count {
        let spawns_by_id: HashMap<&str, &SpawnReceipt> = records
            .iter()
            .filter_map(|r| match r {
                Receipt::Spawn(s) => Some((s.receipt_id.as_str(), s)),
                Receipt::Return(_) => None,
            })
            .collect();
        for record in &records[state.record_count..] {
            println!("{}", format_record(&label, record, &spawns_by_id));
        }
        state.record_count = records.len();
    }

    let violations = openhavn_receipts::validate(&records);
    for violation in &violations {
        let repr = violation.to_string();
        if state.printed_violations.insert(repr) {
            println!("{label}: {} — {violation}", violation.code());
        }
    }

    let open = open_spawn_count(&records);
    if state.last_open_spawns != Some(open) {
        println!("{label}: open spawns: {open}");
        state.last_open_spawns = Some(open);
    }

    Ok(!violations.is_empty())
}

/// One human line for a single record: `file  ts  receipt_id  kind  role@harness  detail`, where
/// `detail` is the task boundary for a spawn, or the stop reason + consumed summary for a return
/// (looking its spawn up in `spawns_by_id` for the `role@harness` column, when known).
fn format_record(
    label: &str,
    record: &Receipt,
    spawns_by_id: &HashMap<&str, &SpawnReceipt>,
) -> String {
    match record {
        Receipt::Spawn(s) => format!(
            "{label}  {}  {}  spawn   {}  \"{}\"",
            s.ts,
            s.receipt_id,
            role_at_harness(s),
            truncate(&s.task_boundary, 80),
        ),
        Receipt::Return(r) => {
            let role_harness = spawns_by_id
                .get(r.spawn_ref.as_str())
                .map(|s| role_at_harness(s))
                .unwrap_or_else(|| "?@?".to_string());
            let consumed = consumed_summary(&r.consumed);
            let detail = if consumed.is_empty() {
                r.stop_reason.to_string()
            } else {
                format!("{}  {consumed}", r.stop_reason)
            };
            format!(
                "{label}  {}  {}  return  {role_harness}  {detail}",
                r.ts, r.receipt_id,
            )
        }
    }
}

fn consumed_summary(consumed: &Consumed) -> String {
    let mut parts = Vec::new();
    if let Some(v) = consumed.tokens {
        parts.push(format!("tokens={v}"));
    }
    if let Some(v) = consumed.tool_calls {
        parts.push(format!("tool_calls={v}"));
    }
    if let Some(v) = consumed.wall_time_ms {
        parts.push(format!("wall_time_ms={v}"));
    }
    if let Some(v) = consumed.cost_usd {
        parts.push(format!("cost_usd={}", fmt_num(v)));
    }
    parts.join(" ")
}

/// Count spawns (deduplicated by `receipt_id`, first occurrence wins — same rule
/// [`openhavn_receipts::validate`] uses) that have no matching return yet.
fn open_spawn_count(records: &[Receipt]) -> usize {
    let mut spawn_ids: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for record in records {
        if let Receipt::Spawn(s) = record {
            if seen.insert(s.receipt_id.as_str()) {
                spawn_ids.push(s.receipt_id.as_str());
            }
        }
    }
    let returned: HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Receipt::Return(ret) => Some(ret.spawn_ref.as_str()),
            Receipt::Spawn(_) => None,
        })
        .collect();
    spawn_ids
        .into_iter()
        .filter(|id| !returned.contains(id))
        .count()
}

/// `path` itself if it's a file; otherwise every `receipts*.jsonl` file found by recursing into
/// `path`, up to [`MAX_DEPTH`] levels of subdirectories.
fn discover_files(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut found = Vec::new();
    walk_dir(path, 0, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk_dir(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading directory {}", dir.display()))?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            walk_dir(&entry_path, depth + 1, out)?;
        } else if is_receipts_file(&entry_path) {
            out.push(entry_path);
        }
    }
    Ok(())
}

fn is_receipts_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("receipts") && name.ends_with(".jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_receipts_file_matches_prefix_and_suffix_only() {
        assert!(is_receipts_file(Path::new("receipts.jsonl")));
        assert!(is_receipts_file(Path::new("receipts-2.jsonl")));
        assert!(!is_receipts_file(Path::new("other.jsonl")));
        assert!(!is_receipts_file(Path::new("receipts.json")));
    }

    #[test]
    fn open_spawn_count_counts_spawns_without_a_return() {
        let text = concat!(
            "{\"kind\":\"spawn\",\"receipt_id\":\"rc_x_000001\",\"ts\":\"2026-01-01T00:00:00Z\",",
            "\"parent\":\"root\",\"task_boundary\":\"t\",\"budget\":{\"max_tokens\":1}}\n",
            "{\"kind\":\"spawn\",\"receipt_id\":\"rc_x_000002\",\"ts\":\"2026-01-01T00:00:01Z\",",
            "\"parent\":\"root\",\"task_boundary\":\"t2\",\"budget\":{\"max_tokens\":1}}\n",
            "{\"kind\":\"return\",\"receipt_id\":\"rc_x_000003\",\"ts\":\"2026-01-01T00:00:02Z\",",
            "\"spawn_ref\":\"rc_x_000002\",\"stop_reason\":\"done\",\"consumed\":{}}\n",
        );
        let records = openhavn_receipts::parse_jsonl(text).unwrap();
        assert_eq!(open_spawn_count(&records), 1);
    }

    #[test]
    fn discover_files_returns_the_file_itself_when_given_a_direct_path() {
        let path = Path::new("/tmp/does-not-need-to-exist/receipts.jsonl");
        assert_eq!(discover_files(path).unwrap(), vec![path.to_path_buf()]);
    }
}
