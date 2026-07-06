// SPDX-License-Identifier: Apache-2.0

//! The provenance lockfile (`openhavn.lock` project-scope, `~/.openhavn/skills.lock`
//! global-scope): what was installed, from where, at what content hash, into which target
//! paths.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::targets::Roots;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockTarget {
    pub harness: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub name: String,
    /// The original source string the skill was installed from (a local path or a git URL).
    pub source: String,
    /// `"local"` or `"git"`.
    pub kind: String,
    /// The resolved git commit, present only when `kind == "git"`.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    pub content_sha256: String,
    /// RFC3339 UTC timestamp of the (re)install that produced this entry.
    pub installed_at: String,
    pub targets: Vec<LockTarget>,
}

/// The lockfile path for the given scope.
pub fn lock_path(roots: &Roots, global: bool) -> PathBuf {
    if global {
        roots.home.join(".openhavn").join("skills.lock")
    } else {
        roots.project_root.join("openhavn.lock")
    }
}

/// A loaded lockfile. Merge-preserving: any top-level TOML table/key this module does not know
/// about survives a [`Lockfile::load`] -> mutate -> [`Lockfile::save`] round trip untouched,
/// since only the `skills` array-of-tables is ever read or replaced (same idiom as
/// `commands::init`'s JSON/TOML mergers).
pub struct Lockfile {
    root: toml::Value,
}

impl Lockfile {
    /// Load `path`. A missing file loads as empty (no skills, no other tables) rather than
    /// erroring — first `skill install` in a project has nothing to read yet.
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

    /// The currently-locked skills, in file order.
    pub fn entries(&self) -> Result<Vec<LockEntry>> {
        match self.root.get("skills") {
            None => Ok(Vec::new()),
            Some(value) => value
                .clone()
                .try_into()
                .context("parsing [[skills]] entries"),
        }
    }

    /// Replace the `skills` array-of-tables wholesale; every other top-level key is untouched.
    pub fn set_entries(&mut self, entries: &[LockEntry]) -> Result<()> {
        let value = toml::Value::try_from(entries).context("serializing skills entries")?;
        let table = self
            .root
            .as_table_mut()
            .context("lockfile root is not a TOML table")?;
        table.insert("skills".to_string(), value);
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
            std::env::temp_dir().join(format!("openhavn-skill-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_entry(name: &str) -> LockEntry {
        LockEntry {
            name: name.to_string(),
            source: "/tmp/some-skill".to_string(),
            kind: "local".to_string(),
            git_ref: None,
            content_sha256: "abc123".to_string(),
            installed_at: "2026-07-06T00:00:00Z".to_string(),
            targets: vec![LockTarget {
                harness: "claude".to_string(),
                path: "/tmp/proj/.claude/skills/foo".to_string(),
            }],
        }
    }

    #[test]
    fn missing_lockfile_loads_empty() {
        let dir = scratch("missing");
        let path = dir.join("openhavn.lock");
        let lock = Lockfile::load(&path).unwrap();
        assert_eq!(lock.entries().unwrap(), Vec::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trip_preserves_unknown_top_level_tables() {
        let dir = scratch("roundtrip");
        let path = dir.join("openhavn.lock");
        std::fs::write(&path, "[other]\nkeep = \"me\"\n").unwrap();

        let mut lock = Lockfile::load(&path).unwrap();
        lock.set_entries(&[sample_entry("foo")]).unwrap();
        lock.save(&path).unwrap();

        let reloaded = Lockfile::load(&path).unwrap();
        let entries = reloaded.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "foo");
        assert_eq!(
            reloaded.root.get("other").and_then(|t| t.get("keep")),
            Some(&toml::Value::String("me".to_string()))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_ref_round_trips_under_the_ref_key() {
        let dir = scratch("gitref");
        let path = dir.join("openhavn.lock");
        let mut entry = sample_entry("foo");
        entry.kind = "git".to_string();
        entry.git_ref = Some("deadbeef".to_string());

        let mut lock = Lockfile::load(&path).unwrap();
        lock.set_entries(&[entry]).unwrap();
        lock.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("ref = \"deadbeef\""), "{text}");

        let reloaded = Lockfile::load(&path).unwrap();
        let entries = reloaded.entries().unwrap();
        assert_eq!(entries[0].git_ref.as_deref(), Some("deadbeef"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_path_switches_on_global() {
        let roots = Roots {
            home: PathBuf::from("/home/u"),
            project_root: PathBuf::from("/work/proj"),
        };
        assert_eq!(
            lock_path(&roots, false),
            PathBuf::from("/work/proj/openhavn.lock")
        );
        assert_eq!(
            lock_path(&roots, true),
            PathBuf::from("/home/u/.openhavn/skills.lock")
        );
    }
}
