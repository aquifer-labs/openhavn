// SPDX-License-Identifier: Apache-2.0

//! `openhavn mcp add|list|rm|sync` — governed cross-harness MCP-server logistics (the
//! "Equipment" pillar's second slot): generalizes what `commands::init`'s `--register-mcp` does
//! for the OpenHavn MCP server itself to *any* MCP server, with a provenance lock, drift
//! detection, and a deterministic admission gate whose decisions are appended to
//! `~/.openhavn/equipment.jsonl` — same governed-logistics shape as `commands::skill`.
//!
//! `openhavn mcp serve` (the OpenHavn MCP server itself, unrelated to server *registration*)
//! lives in the sibling [`server`] module and is re-exported as [`serve`].
//!
//! Every command has a thin, real-environment-resolving entry point (this module's `pub fn`s,
//! called directly from `main.rs`) and a `_core` function taking an explicit [`targets::Roots`]
//! — the same split `commands::init` / `commands::skill` use, so the actual logic is hermetically
//! testable against a fake home/project root.

mod config;
mod equipment;
mod gate;
mod lock;
mod server;
mod targets;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};

pub use server::serve;

use config::{ReadResult, ServerEntry, WriteOutcome};
use equipment::EquipmentRecord;
use gate::GateOutcome;
use lock::{Lockfile, McpLockEntry, McpLockTarget};
use targets::{Harness, Roots};

/// `openhavn mcp add <name> [--target a,b] [--env K=V]... [--dry-run] [--force] -- <command>
/// [args...]`.
pub fn add(
    name: &str,
    target: Option<&[String]>,
    env: &[String],
    command: &[String],
    dry_run: bool,
    force: bool,
) -> Result<i32> {
    add_core(&real_roots()?, name, target, env, command, dry_run, force)
}

/// `openhavn mcp list`.
pub fn list() -> Result<i32> {
    list_core(&real_roots()?)
}

/// `openhavn mcp rm <name> [--target a,b] [--force]`.
pub fn rm(name: &str, target: Option<&[String]>, force: bool) -> Result<i32> {
    rm_core(&real_roots()?, name, target, force)
}

/// `openhavn mcp sync [--dry-run]`.
pub fn sync(dry_run: bool) -> Result<i32> {
    sync_core(&real_roots()?, dry_run)
}

fn real_roots() -> Result<Roots> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let project_root = std::env::current_dir().context("resolving current directory")?;
    Ok(Roots { home, project_root })
}

/// RFC3339, second precision, `Z` suffix — matches `commands::skill`'s `now_ts` style.
fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_env(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    pairs
        .iter()
        .map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .with_context(|| format!("invalid --env {pair:?}, expected KEY=VALUE"))
        })
        .collect()
}

// -------------------------------------------------------------------------------------------
// add
// -------------------------------------------------------------------------------------------

fn add_core(
    roots: &Roots,
    name: &str,
    target: Option<&[String]>,
    env_pairs: &[String],
    command_line: &[String],
    dry_run: bool,
    force: bool,
) -> Result<i32> {
    if command_line.is_empty() {
        bail!("missing command (pass `-- <command> [args...]`)");
    }
    let command = command_line[0].clone();
    let args = command_line[1..].to_vec();
    let env = parse_env(env_pairs)?;

    // Deterministic gate first — never touch a target until the registration itself is admitted.
    let path_env = std::env::var_os("PATH");
    let admitted = match gate::evaluate(name, &command, &env, path_env.as_deref()) {
        GateOutcome::Rejected(rejection) => {
            equipment::append(roots, &EquipmentRecord::reject(name, &rejection))?;
            println!("rejected: {rejection}");
            return Ok(1);
        }
        GateOutcome::Admitted(admitted) => admitted,
    };
    for warning in &admitted.warnings {
        eprintln!("warning: {warning}");
    }

    let requested = targets::resolve_targets(roots, target)?;
    if requested.is_empty() {
        bail!(
            "no target harnesses selected: none detected on this machine \
             (pass --target explicitly)"
        );
    }

    let lock_path = lock::lock_path(roots);
    let mut lockfile = Lockfile::load(&lock_path)?;
    let mut entries = lockfile.entries()?;
    let existing = entries.get(name).cloned();

    // Union in any previously-tracked target so a narrower `--target` this time can't silently
    // orphan a previously-registered harness (same reasoning as `skill::install_core`).
    let mut target_harnesses = requested.clone();
    if let Some(existing) = &existing {
        for t in &existing.targets {
            if let Some(h) = Harness::parse(&t.harness) {
                if !target_harnesses.contains(&h) {
                    target_harnesses.push(h);
                }
            }
        }
    }

    let env_keys: Vec<String> = env.keys().cloned().collect();
    let unchanged = existing.as_ref().is_some_and(|e| {
        e.command == command
            && e.args == args
            && e.env_keys == env_keys
            && target_harnesses
                .iter()
                .all(|h| e.targets.iter().any(|t| t.harness == h.name()))
    });
    if unchanged {
        println!("already registered: '{name}' ({command})");
        return Ok(0);
    }

    for h in &target_harnesses {
        if let ReadResult::Present(map) = config::read_all(*h, roots)? {
            if map.contains_key(name) {
                let owned = existing
                    .as_ref()
                    .is_some_and(|e| e.targets.iter().any(|t| t.harness == h.name()));
                if !owned && !force {
                    bail!(
                        "unmanaged server '{name}' exists in {} (use --force)",
                        h.name()
                    );
                }
            }
        }
    }

    if dry_run {
        println!(
            "dry-run: would add '{name}' ({command}) to {} target(s):",
            target_harnesses.len()
        );
        for h in &target_harnesses {
            println!("  {} -> {}", h.name(), h.config_path(roots).display());
        }
        return Ok(0);
    }

    let server_entry = ServerEntry {
        command: command.clone(),
        args: args.clone(),
        env,
    };
    let mut written = Vec::new();
    for h in &target_harnesses {
        match config::upsert_entry(*h, roots, name, &server_entry)? {
            WriteOutcome::Written => written.push(*h),
            WriteOutcome::ParseFailed => eprintln!(
                "warning: skipping {} — could not parse {}",
                h.name(),
                h.config_path(roots).display()
            ),
        }
    }
    if written.is_empty() {
        bail!("failed to write '{name}' to any target (every target config failed to parse)");
    }

    let entry = McpLockEntry {
        command: command.clone(),
        args,
        env_keys,
        targets: written
            .iter()
            .map(|h| McpLockTarget {
                harness: h.name().to_string(),
                path: h.config_path(roots).display().to_string(),
            })
            .collect(),
        added_at: now_ts(),
    };
    entries.insert(name.to_string(), entry);
    lockfile.set_entries(&entries)?;
    lockfile.save(&lock_path)?;

    let target_names: Vec<String> = written.iter().map(|h| h.name().to_string()).collect();
    equipment::append(
        roots,
        &EquipmentRecord::admit(name, &command, &target_names),
    )?;

    println!("added '{name}' ({command}) to {} target(s):", written.len());
    for h in &written {
        println!("  {} -> {}", h.name(), h.config_path(roots).display());
    }
    Ok(0)
}

// -------------------------------------------------------------------------------------------
// list — inventory across every harness config (not just the lock)
// -------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct InventoryRow {
    name: String,
    harness: &'static str,
    command: String,
    status: &'static str,
    /// Set only for `DRIFTED`: the lock's recorded command, shown alongside the live one.
    lock_command: Option<String>,
}

fn build_inventory(roots: &Roots) -> Result<Vec<InventoryRow>> {
    let locked = Lockfile::load(&lock::lock_path(roots))?.entries()?;
    let mut rows = Vec::new();
    for h in Harness::ALL {
        match config::read_all(h, roots)? {
            ReadResult::Missing => {}
            ReadResult::ParseFailed => rows.push(InventoryRow {
                name: String::new(),
                harness: h.name(),
                command: String::new(),
                status: "PARSE_ERROR",
                lock_command: None,
            }),
            ReadResult::Present(map) => {
                for (name, live) in map {
                    let (status, lock_command) = match locked.get(&name) {
                        Some(entry) if entry.targets.iter().any(|t| t.harness == h.name()) => {
                            if entry.command == live.command && entry.args == live.args {
                                ("MANAGED", None)
                            } else {
                                ("DRIFTED", Some(entry.command.clone()))
                            }
                        }
                        _ => ("UNMANAGED", None),
                    };
                    rows.push(InventoryRow {
                        name,
                        harness: h.name(),
                        command: live.command,
                        status,
                        lock_command,
                    });
                }
            }
        }
    }
    Ok(rows)
}

fn render_inventory(rows: &[InventoryRow]) -> String {
    if rows.is_empty() {
        return "no MCP servers found in any harness config\n".to_string();
    }
    let mut out = String::new();
    for row in rows {
        if row.status == "PARSE_ERROR" {
            let _ = writeln!(
                out,
                "{:<14} PARSE_ERROR (could not parse its config)",
                row.harness
            );
            continue;
        }
        match &row.lock_command {
            Some(lock_cmd) => {
                let _ = writeln!(
                    out,
                    "{:<24} {:<14} {:<10} config={} lock={}",
                    row.name, row.harness, row.status, row.command, lock_cmd
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "{:<24} {:<14} {:<10} {}",
                    row.name, row.harness, row.status, row.command
                );
            }
        }
    }
    out
}

/// Always exits 0 — `list` is a read-only inventory, never a failure signal.
fn list_core(roots: &Roots) -> Result<i32> {
    let rows = build_inventory(roots)?;
    print!("{}", render_inventory(&rows));
    Ok(0)
}

// -------------------------------------------------------------------------------------------
// rm — remove only lock-owned entries (or, with --force, an unmanaged one too)
// -------------------------------------------------------------------------------------------

fn rm_core(roots: &Roots, name: &str, target: Option<&[String]>, force: bool) -> Result<i32> {
    let lock_path = lock::lock_path(roots);
    let mut lockfile = Lockfile::load(&lock_path)?;
    let mut entries = lockfile.entries()?;

    let Some(entry) = entries.get(name).cloned() else {
        if !force {
            bail!(
                "no locked MCP server named {name:?} ({}); use --force to remove an unmanaged \
                 entry",
                lock_path.display()
            );
        }
        let harnesses = match target {
            Some(names) => targets::parse_names(names)?,
            None => Harness::ALL.to_vec(),
        };
        for h in &harnesses {
            config::remove_entry_from_config(*h, roots, name)?;
        }
        let target_names: Vec<String> = harnesses.iter().map(|h| h.name().to_string()).collect();
        equipment::append(roots, &EquipmentRecord::remove(name, &target_names))?;
        println!(
            "removed '{name}' (unmanaged) from {} target(s)",
            harnesses.len()
        );
        return Ok(0);
    };

    let selected: Vec<Harness> = match target {
        Some(names) => targets::parse_names(names)?,
        None => entry
            .targets
            .iter()
            .filter_map(|t| Harness::parse(&t.harness))
            .collect(),
    };

    for h in &selected {
        let owned = entry.targets.iter().any(|t| t.harness == h.name());
        if !owned && !force {
            bail!("'{name}' is not lock-owned in {} (use --force)", h.name());
        }
        config::remove_entry_from_config(*h, roots, name)?;
    }

    let remaining: Vec<McpLockTarget> = entry
        .targets
        .iter()
        .filter(|t| !selected.iter().any(|h| h.name() == t.harness))
        .cloned()
        .collect();
    if remaining.is_empty() {
        entries.remove(name);
    } else {
        let mut updated = entry;
        updated.targets = remaining;
        entries.insert(name.to_string(), updated);
    }
    lockfile.set_entries(&entries)?;
    lockfile.save(&lock_path)?;

    let target_names: Vec<String> = selected.iter().map(|h| h.name().to_string()).collect();
    equipment::append(roots, &EquipmentRecord::remove(name, &target_names))?;

    println!("removed '{name}' from {} target(s)", selected.len());
    Ok(0)
}

// -------------------------------------------------------------------------------------------
// sync — reconcile every locked entry into its targeted harness configs
// -------------------------------------------------------------------------------------------

fn sync_core(roots: &Roots, dry_run: bool) -> Result<i32> {
    let locked = Lockfile::load(&lock::lock_path(roots))?.entries()?;

    for (name, entry) in &locked {
        let env = find_existing_env(roots, name, &entry.targets)?;
        let server_entry = ServerEntry {
            command: entry.command.clone(),
            args: entry.args.clone(),
            env,
        };
        let mut changed_targets = Vec::new();

        for t in &entry.targets {
            let Some(h) = Harness::parse(&t.harness) else {
                continue;
            };
            let action = match config::read_all(h, roots)? {
                ReadResult::Missing => Some("add"),
                ReadResult::ParseFailed => {
                    eprintln!(
                        "warning: skipping {} — could not parse {}",
                        h.name(),
                        h.config_path(roots).display()
                    );
                    None
                }
                ReadResult::Present(map) => match map.get(name) {
                    None => Some("add"),
                    Some(live)
                        if live.command != server_entry.command
                            || live.args != server_entry.args =>
                    {
                        Some("repoint")
                    }
                    Some(_) => None,
                },
            };
            if let Some(action) = action {
                println!("{action}: '{name}' -> {}", h.name());
                changed_targets.push(h.name().to_string());
                if !dry_run {
                    config::upsert_entry(h, roots, name, &server_entry)?;
                }
            }
        }

        if !dry_run && !changed_targets.is_empty() {
            equipment::append(
                roots,
                &EquipmentRecord::update(name, &entry.command, &changed_targets),
            )?;
        }
    }
    Ok(0)
}

/// Best-effort env recovery for a `sync`-restored entry: the lock only ever stores env *keys*
/// (never values), so pull the real values from whichever of this entry's other targets still
/// has them on disk. Returns an empty map if none do (the restored entry simply has no env).
fn find_existing_env(
    roots: &Roots,
    name: &str,
    targets: &[McpLockTarget],
) -> Result<BTreeMap<String, String>> {
    for t in targets {
        if let Some(h) = Harness::parse(&t.harness) {
            if let ReadResult::Present(map) = config::read_all(h, roots)? {
                if let Some(entry) = map.get(name) {
                    if !entry.env.is_empty() {
                        return Ok(entry.env.clone());
                    }
                }
            }
        }
    }
    Ok(BTreeMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_roots(tag: &str) -> Roots {
        let base =
            std::env::temp_dir().join(format!("openhavn-mcp-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let project_root = base.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        Roots { home, project_root }
    }

    fn cleanup(roots: &Roots) {
        std::fs::remove_dir_all(roots.home.parent().unwrap()).ok();
    }

    fn all_targets() -> Vec<String> {
        vec![
            "claude-project".to_string(),
            "claude-user".to_string(),
            "codex".to_string(),
            "zed".to_string(),
        ]
    }

    #[test]
    fn add_writes_all_four_target_shapes_lock_and_equipment_admit() {
        let roots = scratch_roots("add-all-shapes");
        let code = add_core(
            &roots,
            "demo",
            Some(&all_targets()),
            &[],
            &["/bin/echo".to_string(), "hello".to_string()],
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        // claude-project: flat shape, no "type".
        let cp: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(Harness::ClaudeProject.config_path(&roots)).unwrap(),
        )
        .unwrap();
        assert_eq!(cp["mcpServers"]["demo"]["command"], "/bin/echo");
        assert!(cp["mcpServers"]["demo"].get("type").is_none());

        // claude-user: flat shape with type: stdio.
        let cu: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(Harness::ClaudeUser.config_path(&roots)).unwrap(),
        )
        .unwrap();
        assert_eq!(cu["mcpServers"]["demo"]["type"], "stdio");
        assert_eq!(cu["mcpServers"]["demo"]["command"], "/bin/echo");

        // codex: TOML table.
        let codex: toml::Value =
            toml::from_str(&std::fs::read_to_string(Harness::Codex.config_path(&roots)).unwrap())
                .unwrap();
        assert_eq!(
            codex["mcp_servers"]["demo"]["command"].as_str(),
            Some("/bin/echo")
        );

        // zed: nested command object.
        let zed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(Harness::Zed.config_path(&roots)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            zed["context_servers"]["demo"]["command"]["path"],
            "/bin/echo"
        );
        assert_eq!(
            zed["context_servers"]["demo"]["command"]["args"][0],
            "hello"
        );

        // Lock entry.
        let lock_path = lock::lock_path(&roots);
        let entries = Lockfile::load(&lock_path).unwrap().entries().unwrap();
        assert_eq!(entries["demo"].command, "/bin/echo");
        assert_eq!(entries["demo"].targets.len(), 4);

        // Equipment log.
        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert_eq!(equipment_log.lines().count(), 1);
        assert!(equipment_log.contains("\"decision\":\"admit\""));

        cleanup(&roots);
    }

    #[test]
    fn add_is_idempotent_noop_on_identical_re_add() {
        let roots = scratch_roots("add-idempotent");
        let targets = vec!["claude-project".to_string()];
        let code1 = add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();
        assert_eq!(code1, 0);
        let code2 = add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();
        assert_eq!(code2, 0);

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert_eq!(
            equipment_log.lines().count(),
            1,
            "an identical re-add must not append another equipment record"
        );
        cleanup(&roots);
    }

    #[test]
    fn add_refuses_unmanaged_collision_unless_forced() {
        let roots = scratch_roots("add-collision");
        let path = Harness::ClaudeProject.config_path(&roots);
        std::fs::write(
            &path,
            r#"{"mcpServers": {"demo": {"command": "/usr/bin/something-else"}}}"#,
        )
        .unwrap();
        let targets = vec!["claude-project".to_string()];

        let err = add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--force"), "{err}");

        let code = add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            true,
        )
        .unwrap();
        assert_eq!(code, 0);
        let cp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cp["mcpServers"]["demo"]["command"], "/bin/echo");
        cleanup(&roots);
    }

    #[test]
    fn add_with_bad_slug_is_rejected_and_logged_without_writing_any_target() {
        let roots = scratch_roots("add-reject");
        let targets = vec!["claude-project".to_string()];
        let code = add_core(
            &roots,
            "Not A Slug!",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, 1);
        assert!(!Harness::ClaudeProject.config_path(&roots).exists());

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert!(equipment_log.contains("\"decision\":\"reject\""));
        cleanup(&roots);
    }

    #[test]
    fn list_reports_managed_drifted_and_unmanaged() {
        let roots = scratch_roots("list");
        let targets = vec!["claude-project".to_string()];
        add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();

        // Add a second, wholly unmanaged entry directly (never went through `add`).
        let cp_path = Harness::ClaudeProject.config_path(&roots);
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cp_path).unwrap()).unwrap();
        raw["mcpServers"]["rogue"] = serde_json::json!({"command": "/bin/ls"});
        std::fs::write(&cp_path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let rows = build_inventory(&roots).unwrap();
        let demo = rows.iter().find(|r| r.name == "demo").unwrap();
        assert_eq!(demo.status, "MANAGED");
        let rogue = rows.iter().find(|r| r.name == "rogue").unwrap();
        assert_eq!(rogue.status, "UNMANAGED");

        // Now hand-edit the managed entry's command -> DRIFTED.
        raw["mcpServers"]["demo"]["command"] = serde_json::json!("/bin/sh");
        std::fs::write(&cp_path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
        let rows = build_inventory(&roots).unwrap();
        let demo = rows.iter().find(|r| r.name == "demo").unwrap();
        assert_eq!(demo.status, "DRIFTED");
        assert_eq!(demo.lock_command.as_deref(), Some("/bin/echo"));

        assert_eq!(list_core(&roots).unwrap(), 0, "list always exits 0");
        cleanup(&roots);
    }

    #[test]
    fn rm_removes_only_owned_targets_and_drops_lock_entry() {
        let roots = scratch_roots("rm");
        let targets = vec!["claude-project".to_string(), "codex".to_string()];
        add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();

        let code = rm_core(&roots, "demo", None, false).unwrap();
        assert_eq!(code, 0);

        let cp: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(Harness::ClaudeProject.config_path(&roots)).unwrap(),
        )
        .unwrap();
        assert!(cp["mcpServers"].get("demo").is_none());

        let lock_path = lock::lock_path(&roots);
        assert!(Lockfile::load(&lock_path)
            .unwrap()
            .entries()
            .unwrap()
            .is_empty());

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert!(equipment_log
            .lines()
            .any(|l| l.contains("\"decision\":\"remove\"")));
        cleanup(&roots);
    }

    #[test]
    fn rm_of_unmanaged_name_requires_force() {
        let roots = scratch_roots("rm-unmanaged");
        assert!(rm_core(&roots, "nope", None, false).is_err());
        assert_eq!(rm_core(&roots, "nope", None, true).unwrap(), 0);
        cleanup(&roots);
    }

    #[test]
    fn sync_restores_missing_entry_and_repoints_drifted_entry() {
        let roots = scratch_roots("sync");
        let targets = vec!["claude-project".to_string(), "codex".to_string()];
        add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();

        // Drift claude-project's command, and delete codex's entry entirely.
        let cp_path = Harness::ClaudeProject.config_path(&roots);
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cp_path).unwrap()).unwrap();
        raw["mcpServers"]["demo"]["command"] = serde_json::json!("/bin/sh");
        std::fs::write(&cp_path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let codex_path = Harness::Codex.config_path(&roots);
        std::fs::write(&codex_path, "").unwrap();

        let code = sync_core(&roots, false).unwrap();
        assert_eq!(code, 0);

        let cp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cp_path).unwrap()).unwrap();
        assert_eq!(cp["mcpServers"]["demo"]["command"], "/bin/echo");

        let codex: toml::Value =
            toml::from_str(&std::fs::read_to_string(&codex_path).unwrap()).unwrap();
        assert_eq!(
            codex["mcp_servers"]["demo"]["command"].as_str(),
            Some("/bin/echo")
        );

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert!(equipment_log
            .lines()
            .any(|l| l.contains("\"decision\":\"update\"")));
        cleanup(&roots);
    }

    #[test]
    fn sync_dry_run_touches_no_file() {
        let roots = scratch_roots("sync-dry-run");
        let targets = vec!["claude-project".to_string()];
        add_core(
            &roots,
            "demo",
            Some(&targets),
            &[],
            &["/bin/echo".to_string()],
            false,
            false,
        )
        .unwrap();

        let cp_path = Harness::ClaudeProject.config_path(&roots);
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cp_path).unwrap()).unwrap();
        raw["mcpServers"]["demo"]["command"] = serde_json::json!("/bin/sh");
        std::fs::write(&cp_path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let before_cp = std::fs::read(&cp_path).unwrap();
        let lock_path = lock::lock_path(&roots);
        let before_lock = std::fs::read(&lock_path).unwrap();

        let code = sync_core(&roots, true).unwrap();
        assert_eq!(code, 0);

        assert_eq!(
            std::fs::read(&cp_path).unwrap(),
            before_cp,
            "dry-run must not write the config"
        );
        assert_eq!(
            std::fs::read(&lock_path).unwrap(),
            before_lock,
            "dry-run must not write the lock"
        );

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert_eq!(
            equipment_log.lines().count(),
            1,
            "dry-run must not append an equipment record (only the earlier `add`'s)"
        );
        cleanup(&roots);
    }
}
