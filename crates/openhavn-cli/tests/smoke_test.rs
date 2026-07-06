// SPDX-License-Identifier: Apache-2.0

//! Hermetic CLI smoke tests: run the built `openhavn` binary against the shared conformance
//! fixtures (`openhavn-receipts/tests/fixtures/`) and check exit codes + key output substrings.

use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../openhavn-receipts/tests/fixtures")
        .join(name)
}

/// Run the built `openhavn` binary and capture (exit_code, stdout, stderr).
fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_openhavn"))
        .args(args)
        .output()
        .expect("openhavn binary should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

#[test]
fn receipts_validate_valid_fixture_prints_ok_and_exits_zero() {
    let path = fixture("valid.jsonl");
    let (code, stdout, stderr) = run(&["receipts", "validate", path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "ok — 5 records, 3 spawns, 2 returns");
}

#[test]
fn receipts_validate_over_budget_fixture_exits_one_with_typed_violation() {
    let path = fixture("over-budget.jsonl");
    let (code, stdout, stderr) = run(&["receipts", "validate", path.to_str().unwrap()]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stdout.contains("OVER_BUDGET_WITHOUT_BUDGET_STOP"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("rc_run2_000002"), "stdout: {stdout}");
}

#[test]
fn receipts_show_renders_indented_spawn_tree() {
    let path = fixture("valid.jsonl");
    let (code, stdout, stderr) = run(&["receipts", "show", path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "stdout: {stdout}");
    assert!(lines[0].starts_with("rc_run1_000001"));
    assert!(lines[0].contains("RUNNING"), "root has no return yet");
    assert!(
        lines[1].starts_with("  rc_run1_000002"),
        "child is indented"
    );
    assert!(lines[1].contains("done"));
    assert!(
        lines[2].starts_with("  rc_run1_000003"),
        "child is indented"
    );
    assert!(lines[2].contains("budget_tokens"));
}

#[test]
fn budget_tree_shows_granted_consumed_efficiency_and_totals() {
    let path = fixture("valid.jsonl");
    let (code, stdout, stderr) = run(&["budget", "tree", path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("granted=200000"), "stdout: {stdout}");
    assert!(stdout.contains("context-efficiency"), "stdout: {stdout}");
    assert!(stdout.contains("TOTAL"), "stdout: {stdout}");
}

#[test]
fn receipts_validate_missing_file_fails_with_nonzero_exit() {
    let (code, stdout, stderr) = run(&[
        "receipts",
        "validate",
        "/tmp/openhavn-smoke-does-not-exist.jsonl",
    ]);
    assert_ne!(code, 0);
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("error"), "stderr: {stderr}");
}

#[test]
fn receipts_validate_accepts_an_ocf_bundle_directory() {
    let dir = std::env::temp_dir().join(format!(
        "openhavn-cli-smoke-{}-{}.ocf",
        std::process::id(),
        "bundle"
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture("valid.jsonl"), dir.join("receipts.jsonl")).unwrap();

    let (code, stdout, stderr) = run(&["receipts", "validate", dir.to_str().unwrap()]);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "ok — 5 records, 3 spawns, 2 returns");
}

// ---------------------------------------------------------------------------------------------
// `openhavn run` — govern an arbitrary command with a spawn/return receipt pair.
// ---------------------------------------------------------------------------------------------

/// Like [`run`], but sets the child's working directory — needed to exercise the default
/// (relative, run-id-scoped) receipts path without littering the real repo checkout.
fn run_in(dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_openhavn"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("openhavn binary should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "openhavn-cli-smoke-run-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn run_true_writes_a_clean_done_receipt_pair() {
    let dir = scratch_dir("true");
    let receipts = dir.join("receipts.jsonl");
    let (code, stdout, stderr) = run(&[
        "run",
        "--task",
        "demo true",
        "--budget-time-ms",
        "60000",
        "--receipts",
        receipts.to_str().unwrap(),
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("receipts:"), "stderr: {stderr}");

    let (vcode, vstdout, vstderr) = run(&["receipts", "validate", receipts.to_str().unwrap()]);
    assert_eq!(vcode, 0, "stderr: {vstderr}");
    assert_eq!(vstdout.trim(), "ok — 2 records, 1 spawns, 1 returns");

    let contents = std::fs::read_to_string(&receipts).unwrap();
    assert!(contents.contains("\"stop_reason\":\"done\""), "{contents}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_false_mirrors_nonzero_exit_code() {
    let dir = scratch_dir("false");
    let receipts = dir.join("receipts.jsonl");
    let (code, _stdout, stderr) = run(&[
        "run",
        "--budget-time-ms",
        "60000",
        "--receipts",
        receipts.to_str().unwrap(),
        "--",
        "false",
    ]);
    assert_eq!(code, 1, "stderr: {stderr}");

    let (vcode, _vstdout, vstderr) = run(&["receipts", "validate", receipts.to_str().unwrap()]);
    assert_eq!(vcode, 0, "stderr: {vstderr}");
    let contents = std::fs::read_to_string(&receipts).unwrap();
    assert!(contents.contains("\"stop_reason\":\"error\""), "{contents}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_sh_exit_7_mirrors_that_exit_code() {
    let dir = scratch_dir("sh7");
    let receipts = dir.join("receipts.jsonl");
    let (code, _stdout, stderr) = run(&[
        "run",
        "--budget-time-ms",
        "60000",
        "--receipts",
        receipts.to_str().unwrap(),
        "--",
        "sh",
        "-c",
        "exit 7",
    ]);
    assert_eq!(code, 7, "stderr: {stderr}");

    let contents = std::fs::read_to_string(&receipts).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected exactly 1 spawn + 1 return: {contents}"
    );
    assert!(contents.contains("\"stop_reason\":\"error\""), "{contents}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_fail_closed_without_budget_refuses_and_writes_nothing() {
    let dir = scratch_dir("fail-closed");
    let receipts = dir.join("nested").join("receipts.jsonl");
    let (code, stdout, stderr) = run(&[
        "run",
        "--fail-closed",
        "--receipts",
        receipts.to_str().unwrap(),
        "--",
        "true",
    ]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("error"), "stderr: {stderr}");
    assert!(!receipts.exists(), "must not write a receipts file");
    assert!(
        !receipts.parent().unwrap().exists(),
        "must not create the receipts directory either"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_without_budget_flags_defaults_to_24h_wall_time_and_warns() {
    let dir = scratch_dir("default-budget");
    let receipts = dir.join("receipts.jsonl");
    let (code, _stdout, stderr) = run(&[
        "run",
        "--receipts",
        receipts.to_str().unwrap(),
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stderr.contains("no budget declared — defaulting to max_wall_time_ms=86400000"),
        "stderr: {stderr}"
    );

    let contents = std::fs::read_to_string(&receipts).unwrap();
    assert!(
        contents.contains("\"max_wall_time_ms\":86400000"),
        "{contents}"
    );

    let (vcode, _vstdout, vstderr) = run(&["receipts", "validate", receipts.to_str().unwrap()]);
    assert_eq!(vcode, 0, "stderr: {vstderr}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_default_receipts_path_smoke_matches_acceptance_example() {
    let dir = scratch_dir("acceptance");
    let (code, stdout, stderr) = run_in(
        &dir,
        &[
            "run",
            "--task",
            "demo",
            "--budget-time-ms",
            "60000",
            "--",
            "echo",
            "hi",
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "hi\n");

    let receipts_line = stderr
        .lines()
        .find(|l| l.starts_with("receipts:"))
        .unwrap_or_else(|| panic!("expected a 'receipts:' line, stderr: {stderr}"));
    assert!(
        receipts_line.contains(".openhavn/runs/"),
        "expected the default run-scoped path, got: {receipts_line}"
    );
    let receipts_path = receipts_line.trim_start_matches("receipts:").trim();
    let resolved = dir.join(receipts_path.trim_start_matches("./"));
    assert!(
        resolved.is_file(),
        "expected {} to exist",
        resolved.display()
    );

    let (vcode, vstdout, vstderr) = run_in(&dir, &["receipts", "validate", receipts_path]);
    assert_eq!(vcode, 0, "stderr: {vstderr}");
    assert_eq!(vstdout.trim(), "ok — 2 records, 1 spawns, 1 returns");

    let (wcode, wstdout, wstderr) = run_in(&dir, &["watch", "--once", receipts_path]);
    assert_eq!(wcode, 0, "stderr: {wstderr}");
    assert!(wstdout.contains("open spawns: 0"), "stdout: {wstdout}");

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------------------------
// `openhavn watch --once` — single-pass CI mode over the shared conformance fixtures.
// ---------------------------------------------------------------------------------------------

#[test]
fn watch_once_valid_fixture_prints_all_records_and_exits_zero() {
    let path = fixture("valid.jsonl");
    let (code, stdout, stderr) = run(&["watch", "--once", path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    for id in [
        "rc_run1_000001",
        "rc_run1_000002",
        "rc_run1_000003",
        "rc_run1_000004",
        "rc_run1_000005",
    ] {
        assert!(stdout.contains(id), "stdout missing {id}: {stdout}");
    }
    assert!(stdout.contains("open spawns: 1"), "stdout: {stdout}");
}

#[test]
fn watch_once_over_budget_fixture_exits_one() {
    let path = fixture("over-budget.jsonl");
    let (code, stdout, stderr) = run(&["watch", "--once", path.to_str().unwrap()]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stdout.contains("OVER_BUDGET_WITHOUT_BUDGET_STOP"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("open spawns: 0"), "stdout: {stdout}");
}

// ---------------------------------------------------------------------------------------------
// `openhavn skill install|list|rm` — governed cross-harness skill logistics.
// ---------------------------------------------------------------------------------------------

/// Like [`run`], but sets a fake `HOME` (so the equipment log / global lock never touch the
/// real developer's `~/.openhavn`) and a working directory (the project scope root).
fn run_with_home(
    home: &std::path::Path,
    dir: &std::path::Path,
    args: &[&str],
) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_openhavn"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .output()
        .expect("openhavn binary should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

#[test]
fn skill_install_list_rm_round_trip_project_scope() {
    let tag = std::process::id();
    let home = std::env::temp_dir().join(format!("openhavn-cli-smoke-skill-home-{tag}"));
    let project = std::env::temp_dir().join(format!("openhavn-cli-smoke-skill-project-{tag}"));
    let skill_src = std::env::temp_dir().join(format!("openhavn-cli-smoke-skill-src-{tag}"));
    for dir in [&home, &project, &skill_src] {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        skill_src.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: A demo skill for the smoke test\n---\nBody.\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_with_home(
        &home,
        &project,
        &[
            "skill",
            "install",
            skill_src.to_str().unwrap(),
            "--target",
            "claude",
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("installed 'demo-skill'"),
        "stdout: {stdout}"
    );
    assert!(project.join(".claude/skills/demo-skill/SKILL.md").is_file());

    let (code, stdout, stderr) = run_with_home(&home, &project, &["skill", "list"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("demo-skill"), "stdout: {stdout}");
    assert!(stdout.contains("OK"), "stdout: {stdout}");

    let (code, stdout, stderr) = run_with_home(&home, &project, &["skill", "rm", "demo-skill"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("removed 'demo-skill'"), "stdout: {stdout}");
    assert!(!project.join(".claude/skills/demo-skill").exists());

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&project).ok();
    std::fs::remove_dir_all(&skill_src).ok();
}
