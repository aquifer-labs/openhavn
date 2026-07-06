// SPDX-License-Identifier: Apache-2.0

//! The MCP provenance lock: `openhavn.lock`'s `[mcp.<name>]` table — what was registered, its
//! command/args/env *keys* (never env values, so a secret never lands in the lock), which
//! targets it was written to, and when. Mirrors `commands::skill::lock`, but keyed by name as
//! nested tables (`[mcp.<name>]`) rather than an array-of-tables, since MCP server names are a
//! natural unique key.
//!
//! Shares the same physical `openhavn.lock` file as `commands::skill::lock`'s `[[skills]]`
//! array: each module only ever reads/replaces its own top-level key, so both round-trip through
//! the same file without clobbering one another.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::targets::Roots;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpLockTarget {
    pub harness: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpLockEntry {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Env var *names* only — values are never persisted to the lock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
    pub targets: Vec<McpLockTarget>,
    /// RFC3339 UTC timestamp of the (re)registration that produced this entry.
    pub added_at: String,
}

/// The lockfile path — always project-scoped (`<project>/openhavn.lock`); MCP registrations have
/// no global/user scope of their own (a `claude-user`/`codex`/`zed` target is still *recorded*
/// against the project that ran `mcp add`, even though the config file it writes to is
/// machine-wide).
pub fn lock_path(roots: &Roots) -> PathBuf {
    roots.project_root.join("openhavn.lock")
}

/// A loaded lockfile. Merge-preserving: any top-level TOML table/key this module does not know
/// about (including `commands::skill::lock`'s `skills` array) survives a [`Lockfile::load`] ->
/// mutate -> [`Lockfile::save`] round trip untouched, since only the `mcp` table is ever read or
/// replaced.
pub struct Lockfile {
    root: toml::Value,
}

impl Lockfile {
    /// Load `path`. A missing file loads as empty (no mcp entries, no other tables).
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(e).with_context(|| format!("reading lockfile {}", path.display()))
            }
        };
        let root = if text.trim().is_empty() {
            toml::Value::Table(Default::default())
        } else {
            toml::from_str(&text).with_context(|| format!("parsing lockfile {}", path.display()))?
        };
        Ok(Self { root })
    }

    /// The currently-locked MCP servers, keyed by name.
    pub fn entries(&self) -> Result<BTreeMap<String, McpLockEntry>> {
        match self.root.get("mcp") {
            None => Ok(BTreeMap::new()),
            Some(value) => value.clone().try_into().context("parsing [mcp.*] entries"),
        }
    }

    /// Replace the `mcp` table wholesale; every other top-level key (including `skills`) is
    /// untouched.
    pub fn set_entries(&mut self, entries: &BTreeMap<String, McpLockEntry>) -> Result<()> {
        let value = toml::Value::try_from(entries).context("serializing mcp entries")?;
        let table = self
            .root
            .as_table_mut()
            .context("lockfile root is not a TOML table")?;
        table.insert("mcp".to_string(), value);
        Ok(())
    }

    /// Atomic write (tmp file in the same directory, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(&self.root).context("serializing lockfile")?;
        write_atomic(path, &text)
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let tmp_name = format!(
        "{}.openhavn-tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("lock"),
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

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openhavn-mcp-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_entry() -> McpLockEntry {
        McpLockEntry {
            command: "/bin/echo".to_string(),
            args: vec!["mcp".to_string(), "serve".to_string()],
            env_keys: vec!["API_KEY".to_string()],
            targets: vec![McpLockTarget {
                harness: "claude-project".to_string(),
                path: "/tmp/proj/.mcp.json".to_string(),
            }],
            added_at: "2026-07-06T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn missing_lockfile_loads_empty() {
        let dir = scratch("missing");
        let path = dir.join("openhavn.lock");
        let lock = Lockfile::load(&path).unwrap();
        assert!(lock.entries().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trip_preserves_unrelated_top_level_tables() {
        let dir = scratch("roundtrip");
        let path = dir.join("openhavn.lock");
        std::fs::write(
            &path,
            "[other]\nkeep = \"me\"\n\n[[skills]]\nname = \"demo-skill\"\n",
        )
        .unwrap();

        let mut lock = Lockfile::load(&path).unwrap();
        let mut entries = BTreeMap::new();
        entries.insert("my-server".to_string(), sample_entry());
        lock.set_entries(&entries).unwrap();
        lock.save(&path).unwrap();

        let reloaded = Lockfile::load(&path).unwrap();
        let entries = reloaded.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries["my-server"].command, "/bin/echo");
        assert_eq!(
            reloaded.root.get("other").and_then(|t| t.get("keep")),
            Some(&toml::Value::String("me".to_string()))
        );
        assert!(
            reloaded.root.get("skills").is_some(),
            "skills array must survive"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_keys_never_carry_values() {
        let dir = scratch("env-keys");
        let path = dir.join("openhavn.lock");
        let mut lock = Lockfile::load(&path).unwrap();
        let mut entries = BTreeMap::new();
        entries.insert("my-server".to_string(), sample_entry());
        lock.set_entries(&entries).unwrap();
        lock.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("API_KEY"));
        assert!(!text.to_lowercase().contains("secret-value"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_path_is_always_project_scoped() {
        let roots = Roots {
            home: PathBuf::from("/home/u"),
            project_root: PathBuf::from("/work/proj"),
        };
        assert_eq!(lock_path(&roots), PathBuf::from("/work/proj/openhavn.lock"));
    }
}
