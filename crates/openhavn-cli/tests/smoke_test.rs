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
