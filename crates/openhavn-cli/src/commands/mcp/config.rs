// SPDX-License-Identifier: Apache-2.0

//! Reading and merge-preserving writing of one MCP server entry, per harness config shape.
//!
//! Every harness's config is read into the same [`ServerEntry`] shape regardless of its on-disk
//! form (flat JSON, `type: stdio` JSON, nested-object JSON, or TOML), so `list`/`sync`/the
//! collision gate in `add` never need to know which harness they're looking at beyond routing to
//! the right file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use super::targets::{Harness, Roots};

/// One MCP server registration, harness-shape-agnostic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerEntry {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// The outcome of reading every entry out of one harness's config file.
#[derive(Debug)]
pub enum ReadResult {
    /// The config file does not exist at all.
    Missing,
    /// The file exists but isn't valid (JSON/TOML, or its root isn't an object/table) — callers
    /// must warn and skip this target rather than risk clobbering it.
    ParseFailed,
    Present(BTreeMap<String, ServerEntry>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    ParseFailed,
}

/// Read every MCP server entry currently registered with `harness`.
pub fn read_all(harness: Harness, roots: &Roots) -> Result<ReadResult> {
    let path = harness.config_path(roots);
    if !path.exists() {
        return Ok(ReadResult::Missing);
    }
    match harness {
        Harness::ClaudeProject | Harness::ClaudeUser => read_json(&path, "mcpServers", false),
        Harness::Zed => read_json(&path, "context_servers", true),
        Harness::Codex => read_toml(&path),
    }
}

/// Insert or overwrite the `name` entry in `harness`'s config, atomically and merge-preserving
/// (every other entry, and every other top-level key, is left untouched).
pub fn upsert_entry(
    harness: Harness,
    roots: &Roots,
    name: &str,
    entry: &ServerEntry,
) -> Result<WriteOutcome> {
    let path = harness.config_path(roots);
    match harness {
        Harness::ClaudeProject => write_json(&path, "mcpServers", name, Some(entry), false, false),
        Harness::ClaudeUser => write_json(&path, "mcpServers", name, Some(entry), true, false),
        Harness::Zed => write_json(&path, "context_servers", name, Some(entry), false, true),
        Harness::Codex => write_toml(&path, name, Some(entry)),
    }
}

/// Remove the `name` entry from `harness`'s config, if present. A missing config file is a
/// no-op `Written` (nothing to remove, nothing to create).
pub fn remove_entry_from_config(
    harness: Harness,
    roots: &Roots,
    name: &str,
) -> Result<WriteOutcome> {
    let path = harness.config_path(roots);
    if !path.exists() {
        return Ok(WriteOutcome::Written);
    }
    match harness {
        Harness::ClaudeProject => write_json(&path, "mcpServers", name, None, false, false),
        Harness::ClaudeUser => write_json(&path, "mcpServers", name, None, true, false),
        Harness::Zed => write_json(&path, "context_servers", name, None, false, true),
        Harness::Codex => write_toml(&path, name, None),
    }
}

// ---------------------------------------------------------------------------------------------
// JSON (claude-project, claude-user, zed)
// ---------------------------------------------------------------------------------------------

fn read_json(path: &Path, top_key: &str, nested_command: bool) -> Result<ReadResult> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(ReadResult::Present(BTreeMap::new()));
    }
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return Ok(ReadResult::ParseFailed);
    };
    if !root.is_object() {
        return Ok(ReadResult::ParseFailed);
    }
    let Some(map) = root.get(top_key).and_then(Value::as_object) else {
        return Ok(ReadResult::Present(BTreeMap::new()));
    };
    let mut out = BTreeMap::new();
    for (name, value) in map {
        let holder = if nested_command {
            value.get("command")
        } else {
            Some(value)
        };
        let Some(holder) = holder else { continue };
        let command_key = if nested_command { "path" } else { "command" };
        let Some(command) = holder.get(command_key).and_then(Value::as_str) else {
            continue;
        };
        out.insert(
            name.clone(),
            ServerEntry {
                command: command.to_string(),
                args: str_array(holder.get("args")),
                env: str_map(holder.get("env")),
            },
        );
    }
    Ok(ReadResult::Present(out))
}

fn str_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn str_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a JSON object file for writing. A missing file reads as an empty object. Malformed
/// JSON, or JSON whose root isn't an object, yields `Ok(None)` — callers must warn and leave the
/// file untouched rather than write through a parse failure (same idiom as `commands::init`).
fn read_json_root_for_write(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(Some(json!({})));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Some(json!({})));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        _ => Ok(None),
    }
}

fn write_json(
    path: &Path,
    top_key: &str,
    name: &str,
    entry: Option<&ServerEntry>,
    with_type_stdio: bool,
    nested_command: bool,
) -> Result<WriteOutcome> {
    let Some(mut root) = read_json_root_for_write(path)? else {
        eprintln!(
            "warning: {} is not valid JSON; leaving it untouched",
            path.display()
        );
        return Ok(WriteOutcome::ParseFailed);
    };
    let Some(object) = root.as_object_mut() else {
        eprintln!(
            "warning: {} root is not a JSON object; leaving it untouched",
            path.display()
        );
        return Ok(WriteOutcome::ParseFailed);
    };
    let section = object.entry(top_key).or_insert_with(|| json!({}));
    let Some(section_map) = section.as_object_mut() else {
        eprintln!(
            "warning: {} .{top_key} is not an object; leaving it untouched",
            path.display()
        );
        return Ok(WriteOutcome::ParseFailed);
    };
    match entry {
        Some(entry) => {
            section_map.insert(
                name.to_string(),
                build_json_entry(entry, with_type_stdio, nested_command),
            );
        }
        None => {
            section_map.remove(name);
        }
    }
    write_atomic(path, &(serde_json::to_string_pretty(&root)? + "\n"))?;
    Ok(WriteOutcome::Written)
}

fn build_json_entry(entry: &ServerEntry, with_type_stdio: bool, nested_command: bool) -> Value {
    let args: Vec<Value> = entry.args.iter().map(|a| json!(a)).collect();
    if nested_command {
        let mut command_obj = Map::new();
        command_obj.insert("path".to_string(), json!(entry.command));
        command_obj.insert("args".to_string(), Value::Array(args));
        if !entry.env.is_empty() {
            command_obj.insert("env".to_string(), json!(entry.env));
        }
        json!({ "command": Value::Object(command_obj) })
    } else {
        let mut obj = Map::new();
        if with_type_stdio {
            obj.insert("type".to_string(), json!("stdio"));
        }
        obj.insert("command".to_string(), json!(entry.command));
        obj.insert("args".to_string(), Value::Array(args));
        if !entry.env.is_empty() {
            obj.insert("env".to_string(), json!(entry.env));
        }
        Value::Object(obj)
    }
}

// ---------------------------------------------------------------------------------------------
// TOML (codex)
// ---------------------------------------------------------------------------------------------

fn read_toml(path: &Path) -> Result<ReadResult> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(ReadResult::Present(BTreeMap::new()));
    }
    let Ok(root) = toml::from_str::<toml::Value>(&text) else {
        return Ok(ReadResult::ParseFailed);
    };
    let Some(table) = root.get("mcp_servers").and_then(|v| v.as_table()) else {
        return Ok(ReadResult::Present(BTreeMap::new()));
    };
    let mut out = BTreeMap::new();
    for (name, value) in table {
        let Some(command) = value.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        let args = value
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let env = value
            .get("env")
            .and_then(|e| e.as_table())
            .map(|e| {
                e.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.insert(
            name.clone(),
            ServerEntry {
                command: command.to_string(),
                args,
                env,
            },
        );
    }
    Ok(ReadResult::Present(out))
}

fn write_toml(path: &Path, name: &str, entry: Option<&ServerEntry>) -> Result<WriteOutcome> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut value: toml::Value = if text.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        match toml::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "warning: {} is not valid TOML; leaving it untouched",
                    path.display()
                );
                return Ok(WriteOutcome::ParseFailed);
            }
        }
    };
    let Some(table) = value.as_table_mut() else {
        eprintln!(
            "warning: {} root is not a table; leaving it untouched",
            path.display()
        );
        return Ok(WriteOutcome::ParseFailed);
    };
    let mcp_servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let Some(mcp_table) = mcp_servers.as_table_mut() else {
        eprintln!(
            "warning: {} mcp_servers is not a table; leaving it untouched",
            path.display()
        );
        return Ok(WriteOutcome::ParseFailed);
    };
    match entry {
        Some(entry) => {
            let mut t = toml::value::Table::new();
            t.insert(
                "command".to_string(),
                toml::Value::String(entry.command.clone()),
            );
            t.insert(
                "args".to_string(),
                toml::Value::Array(
                    entry
                        .args
                        .iter()
                        .map(|a| toml::Value::String(a.clone()))
                        .collect(),
                ),
            );
            if !entry.env.is_empty() {
                let mut env_table = toml::value::Table::new();
                for (k, v) in &entry.env {
                    env_table.insert(k.clone(), toml::Value::String(v.clone()));
                }
                t.insert("env".to_string(), toml::Value::Table(env_table));
            }
            mcp_table.insert(name.to_string(), toml::Value::Table(t));
        }
        None => {
            mcp_table.remove(name);
        }
    }
    write_atomic(path, &toml::to_string_pretty(&value)?)?;
    Ok(WriteOutcome::Written)
}

// ---------------------------------------------------------------------------------------------

/// Write `contents` to `path` via a same-directory tmp file + rename, so a crash or concurrent
/// reader never observes a partially written config (same idiom as `commands::init`).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Roots {
        let dir =
            std::env::temp_dir().join(format!("openhavn-mcp-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Roots {
            home: dir.clone(),
            project_root: dir,
        }
    }

    fn entry(command: &str) -> ServerEntry {
        ServerEntry {
            command: command.to_string(),
            args: vec!["mcp".to_string(), "serve".to_string()],
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn claude_project_write_read_round_trip_has_no_type_field() {
        let roots = scratch("claude-project");
        let outcome =
            upsert_entry(Harness::ClaudeProject, &roots, "demo", &entry("/bin/echo")).unwrap();
        assert_eq!(outcome, WriteOutcome::Written);

        let path = Harness::ClaudeProject.config_path(&roots);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw["mcpServers"]["demo"].get("type").is_none());
        assert_eq!(raw["mcpServers"]["demo"]["command"], "/bin/echo");

        let ReadResult::Present(map) = read_all(Harness::ClaudeProject, &roots).unwrap() else {
            panic!("expected Present");
        };
        assert_eq!(map["demo"].command, "/bin/echo");
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn claude_user_write_includes_type_stdio() {
        let roots = scratch("claude-user");
        upsert_entry(Harness::ClaudeUser, &roots, "demo", &entry("/bin/echo")).unwrap();
        let path = Harness::ClaudeUser.config_path(&roots);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["mcpServers"]["demo"]["type"], "stdio");
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn zed_write_uses_nested_command_object_with_path() {
        let roots = scratch("zed");
        upsert_entry(Harness::Zed, &roots, "demo", &entry("/bin/echo")).unwrap();
        let path = Harness::Zed.config_path(&roots);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw["context_servers"]["demo"]["command"]["path"],
            "/bin/echo"
        );
        assert_eq!(raw["context_servers"]["demo"]["command"]["args"][0], "mcp");

        let ReadResult::Present(map) = read_all(Harness::Zed, &roots).unwrap() else {
            panic!("expected Present");
        };
        assert_eq!(map["demo"].command, "/bin/echo");
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn codex_write_uses_mcp_servers_table() {
        let roots = scratch("codex");
        upsert_entry(Harness::Codex, &roots, "demo", &entry("/bin/echo")).unwrap();
        let path = Harness::Codex.config_path(&roots);
        let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw["mcp_servers"]["demo"]["command"].as_str(),
            Some("/bin/echo")
        );

        let ReadResult::Present(map) = read_all(Harness::Codex, &roots).unwrap() else {
            panic!("expected Present");
        };
        assert_eq!(map["demo"].command, "/bin/echo");
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn json_write_preserves_unrelated_top_level_keys_and_other_servers() {
        let roots = scratch("json-merge");
        let path = Harness::ClaudeProject.config_path(&roots);
        std::fs::write(
            &path,
            r#"{"otherKey": "keepme", "mcpServers": {"unrelated": {"command": "x"}}}"#,
        )
        .unwrap();

        upsert_entry(Harness::ClaudeProject, &roots, "demo", &entry("/bin/echo")).unwrap();

        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["otherKey"], "keepme");
        assert_eq!(raw["mcpServers"]["unrelated"]["command"], "x");
        assert_eq!(raw["mcpServers"]["demo"]["command"], "/bin/echo");
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn toml_write_preserves_unrelated_tables_and_other_servers() {
        let roots = scratch("toml-merge");
        let path = Harness::Codex.config_path(&roots);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[other]\nkeep = \"me\"\n\n[mcp_servers.other]\ncommand = \"y\"\n",
        )
        .unwrap();

        upsert_entry(Harness::Codex, &roots, "demo", &entry("/bin/echo")).unwrap();

        let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["other"]["keep"].as_str(), Some("me"));
        assert_eq!(raw["mcp_servers"]["other"]["command"].as_str(), Some("y"));
        assert_eq!(
            raw["mcp_servers"]["demo"]["command"].as_str(),
            Some("/bin/echo")
        );
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn malformed_json_is_left_untouched_and_reported_as_parse_failed() {
        let roots = scratch("malformed-json");
        let path = Harness::ClaudeProject.config_path(&roots);
        std::fs::write(&path, "{ not valid json").unwrap();

        let outcome =
            upsert_entry(Harness::ClaudeProject, &roots, "demo", &entry("/bin/echo")).unwrap();
        assert_eq!(outcome, WriteOutcome::ParseFailed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not valid json");

        assert!(matches!(
            read_all(Harness::ClaudeProject, &roots).unwrap(),
            ReadResult::ParseFailed
        ));
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn remove_drops_only_the_named_entry() {
        let roots = scratch("remove");
        upsert_entry(Harness::ClaudeProject, &roots, "keep", &entry("/bin/sh")).unwrap();
        upsert_entry(Harness::ClaudeProject, &roots, "demo", &entry("/bin/echo")).unwrap();

        remove_entry_from_config(Harness::ClaudeProject, &roots, "demo").unwrap();

        let ReadResult::Present(map) = read_all(Harness::ClaudeProject, &roots).unwrap() else {
            panic!("expected Present");
        };
        assert!(!map.contains_key("demo"));
        assert!(map.contains_key("keep"));
        std::fs::remove_dir_all(&roots.home).ok();
    }

    #[test]
    fn missing_config_reads_as_missing() {
        let roots = scratch("missing");
        assert!(matches!(
            read_all(Harness::ClaudeProject, &roots).unwrap(),
            ReadResult::Missing
        ));
        std::fs::remove_dir_all(&roots.home).ok();
    }
}
