// SPDX-License-Identifier: Apache-2.0

//! `openhavn init` — detect installed agent harnesses (claude, codex, opencode, zed) and, with
//! `--register-mcp`, register `openhavn mcp serve` as an MCP server for each detected harness
//! that has a well-known config format to merge into.
//!
//! Every detection and registration function takes its filesystem anchors explicitly
//! ([`DetectRoots`], `home: &Path`) rather than reading `$HOME` itself, so the whole module is
//! testable against a fake HOME without touching the real one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

/// Filesystem (and `PATH`) anchors used for harness detection, overridable for tests. Without
/// this, detection would depend on whatever happens to be installed/on `PATH` on the machine
/// running the tests rather than the fake HOME the test constructs.
pub struct DetectRoots<'a> {
    pub home: &'a Path,
    /// macOS app-bundle path checked for Zed (`/Applications/Zed.app` in production).
    pub zed_app_bundle: &'a Path,
    /// `PATH`-style search list used for `which`-style binary detection
    /// (`std::env::var_os("PATH")` in production; `None` in tests disables PATH search
    /// entirely, so detection only ever sees the injected `home`/`zed_app_bundle`).
    pub path: Option<&'a std::ffi::OsStr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Harness {
    Claude,
    Codex,
    Opencode,
    Zed,
}

impl Harness {
    const ALL: [Harness; 4] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Opencode,
        Harness::Zed,
    ];

    fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Zed => "zed",
        }
    }

    fn detect(self, roots: &DetectRoots<'_>) -> bool {
        match self {
            Harness::Claude => {
                command_on_path("claude", roots.path) || roots.home.join(".claude").is_dir()
            }
            Harness::Codex => {
                command_on_path("codex", roots.path) || roots.home.join(".codex").is_dir()
            }
            Harness::Opencode => {
                command_on_path("opencode", roots.path)
                    || roots.home.join(".opencode").is_dir()
                    || roots.home.join(".config").join("opencode").is_dir()
            }
            Harness::Zed => {
                roots.zed_app_bundle.exists() || roots.home.join(".config").join("zed").is_dir()
            }
        }
    }

    /// Registers (`write = true`) or previews (`write = false`) this harness's MCP entry.
    /// `Opencode` has no well-known MCP config format to merge into (see module docs), so it
    /// always reports [`RegOutcome::NotApplicable`].
    fn apply(self, home: &Path, exe: &Path, write: bool) -> Result<RegOutcome> {
        match self {
            Harness::Claude => register_claude(home, exe, write),
            Harness::Codex => register_codex(home, exe, write),
            Harness::Zed => register_zed(home, exe, write),
            Harness::Opencode => Ok(RegOutcome::NotApplicable),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum RegOutcome {
    Registered,
    AlreadyRegistered,
    WouldRegister,
    NotApplicable,
    Failed(String),
}

/// One row of the `openhavn init` status table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub harness: &'static str,
    pub found: bool,
    pub mcp: String,
}

/// Build the status rows for every known harness. Pure and hermetic — see module docs.
/// `register_requested` is `--register-mcp`; `write` is `--register-mcp && !--dry-run`
/// (plain `init`, and `--dry-run` alone, always pass `write = false`, so nothing is ever
/// mutated unless registration was both requested and not a dry run).
fn build_rows(
    roots: &DetectRoots<'_>,
    exe: &Path,
    register_requested: bool,
    write: bool,
) -> Vec<Row> {
    Harness::ALL
        .into_iter()
        .map(|harness| {
            let found = harness.detect(roots);
            let mcp = if !found {
                "-".to_string()
            } else if !register_requested {
                match harness.apply(roots.home, exe, false) {
                    Ok(RegOutcome::AlreadyRegistered) => "registered".to_string(),
                    Ok(RegOutcome::NotApplicable) => "-".to_string(),
                    Ok(_) => "not registered".to_string(),
                    Err(err) => format!("error: {err:#}"),
                }
            } else {
                match harness.apply(roots.home, exe, write) {
                    Ok(RegOutcome::Registered | RegOutcome::AlreadyRegistered) => {
                        "registered".to_string()
                    }
                    Ok(RegOutcome::WouldRegister) => {
                        "not registered (dry-run: would register)".to_string()
                    }
                    Ok(RegOutcome::NotApplicable) => "-".to_string(),
                    Ok(RegOutcome::Failed(msg)) => format!("not registered ({msg})"),
                    Err(err) => format!("error: {err:#}"),
                }
            };
            Row {
                harness: harness.name(),
                found,
                mcp,
            }
        })
        .collect()
}

fn render_table(rows: &[Row]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{:<10} {:<11} MCP", "HARNESS", "FOUND");
    for row in rows {
        let _ = writeln!(
            out,
            "{:<10} {:<11} {}",
            row.harness,
            if row.found { "found" } else { "not found" },
            row.mcp
        );
    }
    out
}

/// `openhavn init [--register-mcp] [--dry-run]`.
pub fn run(register_mcp: bool, dry_run: bool) -> Result<i32> {
    let home = home_dir()?;
    let exe = std::env::current_exe().context("resolving current executable path")?;
    let zed_app_bundle = PathBuf::from("/Applications/Zed.app");
    let path_env = std::env::var_os("PATH");
    let roots = DetectRoots {
        home: &home,
        zed_app_bundle: &zed_app_bundle,
        path: path_env.as_deref(),
    };
    let write = register_mcp && !dry_run;

    let rows = build_rows(&roots, &exe, register_mcp, write);
    print!("{}", render_table(&rows));

    if register_mcp && Harness::Opencode.detect(&roots) {
        println!(
            "opencode: manual — add {{\"command\": \"{}\", \"args\": [\"mcp\", \"serve\"]}} to your opencode MCP config (no well-known opencode MCP config path to merge into automatically)",
            exe.display()
        );
    }
    Ok(0)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn command_on_path(command: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let Some(paths) = path else {
        return false;
    };
    std::env::split_paths(paths).any(|dir| dir.join(command).is_file())
}

// ---------------------------------------------------------------------------------------------
// Generic read / merge / atomic-write helpers, shared by the three JSON- or TOML-backed
// registration targets below.
// ---------------------------------------------------------------------------------------------

/// Parse a JSON object file. A missing file reads as an empty object. Malformed JSON, or JSON
/// whose root isn't an object, yields `Ok(None)` — callers must warn and leave the file
/// untouched rather than write through a parse failure.
fn read_json_object(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(Some(json!({})));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Some(json!({})));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        _ => Ok(None),
    }
}

fn ensure_object<'a>(root: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    let object = root.as_object_mut().expect("root is a JSON object");
    object.entry(key).or_insert_with(|| json!({}));
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("just ensured an object at this key")
}

/// Write `contents` to `path` via a same-directory tmp file + rename, so a crash or concurrent
/// reader never observes a partially written config.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let tmp_name = format!(
        "{}.openhavn-tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        std::process::id()
    );
    let tmp_path = path.with_file_name(tmp_name);
    std::fs::write(&tmp_path, contents)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming into place: {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// claude: user-scope registration. Prefer the `claude` CLI; fall back to editing
// `~/.claude.json` directly. Idempotent either way — an existing "openhavn" key is left alone.
// ---------------------------------------------------------------------------------------------

fn claude_config_path(home: &Path) -> PathBuf {
    home.join(".claude.json")
}

fn register_claude(home: &Path, exe: &Path, write: bool) -> Result<RegOutcome> {
    let path = claude_config_path(home);
    let Some(mut root) = read_json_object(&path)? else {
        eprintln!(
            "warning: {} is not valid JSON; leaving it untouched",
            path.display()
        );
        return Ok(RegOutcome::Failed("invalid JSON".to_string()));
    };
    if ensure_object(&mut root, "mcpServers").contains_key("openhavn") {
        return Ok(RegOutcome::AlreadyRegistered);
    }
    if !write {
        return Ok(RegOutcome::WouldRegister);
    }
    if should_use_external_claude_cli(home) && try_claude_cli_add(exe) {
        return Ok(RegOutcome::Registered);
    }
    ensure_object(&mut root, "mcpServers").insert(
        "openhavn".to_string(),
        json!({"command": exe.display().to_string(), "args": ["mcp", "serve"]}),
    );
    write_atomic(&path, &(serde_json::to_string_pretty(&root)? + "\n"))?;
    Ok(RegOutcome::Registered)
}

/// The external `claude` CLI ignores any `home` override and always targets the *real* `$HOME`,
/// so this path must only run when `home` actually is the real `$HOME` (never under a
/// sandboxed/test home) and the binary is present.
fn should_use_external_claude_cli(home: &Path) -> bool {
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    real_home.as_deref() == Some(home)
        && command_on_path("claude", std::env::var_os("PATH").as_deref())
}

fn try_claude_cli_add(exe: &Path) -> bool {
    // Idempotent: drop any prior entry, then add fresh. Best-effort — a failure here just falls
    // back to the direct-file writer in `register_claude`.
    let _ = std::process::Command::new("claude")
        .args(["mcp", "remove", "--scope", "user", "openhavn"])
        .output();
    std::process::Command::new("claude")
        .args([
            "mcp",
            "add",
            "--scope",
            "user",
            "openhavn",
            "--",
            &exe.display().to_string(),
            "mcp",
            "serve",
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// codex: ~/.codex/config.toml `[mcp_servers.openhavn]`.
// ---------------------------------------------------------------------------------------------

fn register_codex(home: &Path, exe: &Path, write: bool) -> Result<RegOutcome> {
    let path = home.join(".codex").join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut value: toml::Value = if text.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        match toml::from_str(&text) {
            Ok(value) => value,
            Err(_) => {
                eprintln!(
                    "warning: {} is not valid TOML; leaving it untouched",
                    path.display()
                );
                return Ok(RegOutcome::Failed("invalid TOML".to_string()));
            }
        }
    };
    let Some(table) = value.as_table_mut() else {
        eprintln!(
            "warning: {} root is not a table; leaving it untouched",
            path.display()
        );
        return Ok(RegOutcome::Failed("root is not a table".to_string()));
    };
    let mcp_servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let Some(mcp_table) = mcp_servers.as_table_mut() else {
        return Ok(RegOutcome::Failed("mcp_servers is not a table".to_string()));
    };
    if mcp_table.contains_key("openhavn") {
        return Ok(RegOutcome::AlreadyRegistered);
    }
    if !write {
        return Ok(RegOutcome::WouldRegister);
    }
    let mut entry = toml::value::Table::new();
    entry.insert(
        "command".to_string(),
        toml::Value::String(exe.display().to_string()),
    );
    entry.insert(
        "args".to_string(),
        toml::Value::Array(vec![
            toml::Value::String("mcp".to_string()),
            toml::Value::String("serve".to_string()),
        ]),
    );
    mcp_table.insert("openhavn".to_string(), toml::Value::Table(entry));
    write_atomic(&path, &toml::to_string_pretty(&value)?)?;
    Ok(RegOutcome::Registered)
}

// ---------------------------------------------------------------------------------------------
// zed: ~/.config/zed/settings.json "context_servers".
// ---------------------------------------------------------------------------------------------

fn zed_settings_path(home: &Path) -> PathBuf {
    home.join(".config").join("zed").join("settings.json")
}

fn register_zed(home: &Path, exe: &Path, write: bool) -> Result<RegOutcome> {
    let path = zed_settings_path(home);
    let Some(mut root) = read_json_object(&path)? else {
        eprintln!(
            "warning: {} is not valid JSON; leaving it untouched",
            path.display()
        );
        return Ok(RegOutcome::Failed("invalid JSON".to_string()));
    };
    if ensure_object(&mut root, "context_servers").contains_key("openhavn") {
        return Ok(RegOutcome::AlreadyRegistered);
    }
    if !write {
        return Ok(RegOutcome::WouldRegister);
    }
    ensure_object(&mut root, "context_servers").insert(
        "openhavn".to_string(),
        json!({"command": {"path": exe.display().to_string(), "args": ["mcp", "serve"]}}),
    );
    write_atomic(&path, &(serde_json::to_string_pretty(&root)? + "\n"))?;
    Ok(RegOutcome::Registered)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openhavn-init-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn roots<'a>(home: &'a Path, zed_app_bundle: &'a Path) -> DetectRoots<'a> {
        // `path: None` disables PATH search entirely so detection only ever sees the injected
        // fake `home`/`zed_app_bundle` — otherwise these tests would depend on whatever agent
        // CLIs happen to be installed on the machine running them.
        DetectRoots {
            home,
            zed_app_bundle,
            path: None,
        }
    }

    fn fake_exe() -> PathBuf {
        PathBuf::from("/usr/local/bin/openhavn")
    }

    #[test]
    fn detect_finds_claude_via_config_dir_and_not_codex() {
        let home = temp_home("detect-claude");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let missing_app = home.join("NoZed.app");
        let roots = roots(&home, &missing_app);
        assert!(Harness::Claude.detect(&roots));
        assert!(!Harness::Codex.detect(&roots));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn detect_zed_via_injected_app_bundle_path_not_the_real_one() {
        let home = temp_home("detect-zed");
        let fake_app = home.join("Zed.app");
        fs::create_dir_all(&fake_app).unwrap();
        assert!(Harness::Zed.detect(&roots(&home, &fake_app)));

        let absent_home = temp_home("detect-zed-absent");
        let absent_app = absent_home.join("NoZed.app");
        assert!(!Harness::Zed.detect(&roots(&absent_home, &absent_app)));

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&absent_home).ok();
    }

    #[test]
    fn build_rows_reports_not_found_and_dash_when_nothing_is_installed() {
        let home = temp_home("rows-empty");
        let zed_app_bundle = home.join("NoZed.app");
        let rows = build_rows(&roots(&home, &zed_app_bundle), &fake_exe(), false, false);
        assert_eq!(rows.len(), 4);
        for row in &rows {
            assert!(!row.found);
            assert_eq!(row.mcp, "-");
        }
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn register_claude_direct_file_is_idempotent_and_preserves_unrelated_keys() {
        let home = temp_home("claude-reg");
        let path = claude_config_path(&home);
        fs::write(
            &path,
            r#"{"otherKey": "keepme", "mcpServers": {"unrelated": {"command": "x"}}}"#,
        )
        .unwrap();
        let exe = fake_exe();

        let first = register_claude(&home, &exe, true).unwrap();
        assert_eq!(first, RegOutcome::Registered);
        let after_first = fs::read_to_string(&path).unwrap();

        let second = register_claude(&home, &exe, true).unwrap();
        assert_eq!(second, RegOutcome::AlreadyRegistered);
        let after_second = fs::read_to_string(&path).unwrap();
        assert_eq!(
            after_first, after_second,
            "second run must not change the file"
        );

        let value: Value = serde_json::from_str(&after_second).unwrap();
        assert_eq!(value["otherKey"], "keepme");
        assert_eq!(value["mcpServers"]["unrelated"]["command"], "x");
        assert_eq!(value["mcpServers"]["openhavn"]["args"][0], "mcp");
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn register_codex_merges_into_existing_table_without_clobbering_other_servers() {
        let home = temp_home("codex-reg");
        let path = home.join(".codex").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[mcp_servers.other]\ncommand = \"y\"\n").unwrap();
        let exe = fake_exe();

        let first = register_codex(&home, &exe, true).unwrap();
        assert_eq!(first, RegOutcome::Registered);
        let second = register_codex(&home, &exe, true).unwrap();
        assert_eq!(second, RegOutcome::AlreadyRegistered);

        let text = fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["other"]["command"].as_str(),
            Some("y")
        );
        assert_eq!(
            parsed["mcp_servers"]["openhavn"]["command"].as_str(),
            exe.to_str()
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn register_zed_merges_into_existing_context_servers() {
        let home = temp_home("zed-reg");
        let path = zed_settings_path(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"context_servers": {"other": {"command": {"path": "y"}}}}"#,
        )
        .unwrap();
        let exe = fake_exe();

        let first = register_zed(&home, &exe, true).unwrap();
        assert_eq!(first, RegOutcome::Registered);
        let second = register_zed(&home, &exe, true).unwrap();
        assert_eq!(second, RegOutcome::AlreadyRegistered);

        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["context_servers"]["other"]["command"]["path"], "y");
        assert_eq!(
            value["context_servers"]["openhavn"]["command"]["args"][1],
            "serve"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn dry_run_preview_touches_nothing() {
        let home = temp_home("dry-run");
        let exe = fake_exe();
        let outcome = register_claude(&home, &exe, false).unwrap();
        assert_eq!(outcome, RegOutcome::WouldRegister);
        assert!(
            !claude_config_path(&home).exists(),
            "dry-run must not create the file"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn malformed_json_is_left_untouched() {
        let home = temp_home("malformed");
        let path = claude_config_path(&home);
        fs::write(&path, "{ not valid json").unwrap();
        let exe = fake_exe();

        let outcome = register_claude(&home, &exe, true).unwrap();
        assert!(matches!(outcome, RegOutcome::Failed(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not valid json");
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn opencode_never_registers_even_when_found() {
        let home = temp_home("opencode");
        fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
        let outcome = Harness::Opencode.apply(&home, &fake_exe(), true).unwrap();
        assert_eq!(outcome, RegOutcome::NotApplicable);
        fs::remove_dir_all(&home).ok();
    }
}
