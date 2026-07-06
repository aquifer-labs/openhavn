// SPDX-License-Identifier: Apache-2.0

//! The MCP harness config table: which file, and which nesting key, a given harness's MCP
//! server registration lives under.
//!
//! Data-driven and easy to extend — adding a harness means adding one [`Harness`] variant, one
//! arm in [`Harness::config_path`], and one arm in [`Harness::present`]; everything else in
//! `commands::mcp` (gate, config read/write, lock, equipment) dispatches on the enum and needs no
//! changes. Mirrors `commands::skill::targets`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Filesystem anchors used for MCP config resolution, overridable for tests — the same pattern
/// as `commands::init::DetectRoots` / `commands::skill::targets::Roots`: nothing in this module
/// (or its siblings) reads `$HOME` or the current directory itself, so the whole `mcp` command
/// tree is testable against a fake home/project root without touching the real ones.
#[derive(Debug, Clone)]
pub struct Roots {
    pub home: PathBuf,
    pub project_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// `<project>/.mcp.json`, `mcpServers.<name>` = `{command, args, env}`.
    ClaudeProject,
    /// `~/.claude.json`, `mcpServers.<name>` = `{type: "stdio", command, args, env}`.
    ClaudeUser,
    /// `~/.codex/config.toml`, `[mcp_servers.<name>]` command/args/env.
    Codex,
    /// Zed `settings.json`, `context_servers.<name>` = `{command: {path, args, env}}`.
    Zed,
}

impl Harness {
    pub const ALL: [Harness; 4] = [
        Harness::ClaudeProject,
        Harness::ClaudeUser,
        Harness::Codex,
        Harness::Zed,
    ];

    /// Harnesses eligible for the *default* `--target` set. `ClaudeUser` is deliberately excluded
    /// — a machine/user-wide config affects every project, so it is only ever touched when named
    /// explicitly via `--target claude-user`.
    const DEFAULT_CANDIDATES: [Harness; 3] = [Harness::ClaudeProject, Harness::Codex, Harness::Zed];

    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeProject => "claude-project",
            Harness::ClaudeUser => "claude-user",
            Harness::Codex => "codex",
            Harness::Zed => "zed",
        }
    }

    pub fn parse(name: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.name() == name)
    }

    /// The config file this harness's MCP servers live in.
    pub fn config_path(self, roots: &Roots) -> PathBuf {
        match self {
            Harness::ClaudeProject => roots.project_root.join(".mcp.json"),
            Harness::ClaudeUser => roots.home.join(".claude.json"),
            Harness::Codex => roots.home.join(".codex").join("config.toml"),
            Harness::Zed => zed_settings_path(&roots.home),
        }
    }

    /// Whether this harness's parent config/dir already exists on this machine — used only to
    /// pick the default `--target` set (see [`resolve_targets`]); an explicit `--target` bypasses
    /// this entirely. `ClaudeProject`'s "parent" is the project root itself, always present.
    fn present(self, home: &Path) -> bool {
        match self {
            Harness::ClaudeProject => true,
            Harness::ClaudeUser => false,
            Harness::Codex => home.join(".codex").is_dir(),
            Harness::Zed => {
                app_support_dir(home).is_dir() || home.join(".config").join("zed").is_dir()
            }
        }
    }
}

fn app_support_dir(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support")
}

/// `~/Library/Application Support/Zed/settings.json`, falling back to
/// `~/.config/zed/settings.json` when the App Support parent dir (`~/Library/Application
/// Support`) itself is absent — e.g. under a hermetic test home, or on Linux.
fn zed_settings_path(home: &Path) -> PathBuf {
    if app_support_dir(home).is_dir() {
        app_support_dir(home).join("Zed").join("settings.json")
    } else {
        home.join(".config").join("zed").join("settings.json")
    }
}

/// Parse an explicit `--target` list, erroring on any unrecognized name.
pub fn parse_names(names: &[String]) -> Result<Vec<Harness>> {
    names
        .iter()
        .map(|name| {
            Harness::parse(name).ok_or_else(|| {
                anyhow!(
                    "unknown harness {name:?} (expected one of: claude-project, claude-user, codex, zed)"
                )
            })
        })
        .collect()
}

/// Resolve `--target` into the harness list to act on: the explicit names if given, else every
/// [`Harness::DEFAULT_CANDIDATES`] harness whose parent config/dir is already present.
pub fn resolve_targets(roots: &Roots, target: Option<&[String]>) -> Result<Vec<Harness>> {
    match target {
        Some(names) => parse_names(names),
        None => Ok(Harness::DEFAULT_CANDIDATES
            .into_iter()
            .filter(|h| h.present(&roots.home))
            .collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Roots {
        Roots {
            home: PathBuf::from("/home/u"),
            project_root: PathBuf::from("/work/proj"),
        }
    }

    #[test]
    fn config_paths_match_the_documented_shapes() {
        let roots = roots();
        assert_eq!(
            Harness::ClaudeProject.config_path(&roots),
            PathBuf::from("/work/proj/.mcp.json")
        );
        assert_eq!(
            Harness::ClaudeUser.config_path(&roots),
            PathBuf::from("/home/u/.claude.json")
        );
        assert_eq!(
            Harness::Codex.config_path(&roots),
            PathBuf::from("/home/u/.codex/config.toml")
        );
    }

    #[test]
    fn zed_path_falls_back_when_app_support_parent_is_absent() {
        let dir =
            std::env::temp_dir().join(format!("openhavn-mcp-targets-zed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let roots = Roots {
            home: dir.clone(),
            project_root: dir.clone(),
        };
        assert_eq!(
            Harness::Zed.config_path(&roots),
            dir.join(".config").join("zed").join("settings.json")
        );

        std::fs::create_dir_all(app_support_dir(&dir)).unwrap();
        assert_eq!(
            Harness::Zed.config_path(&roots),
            app_support_dir(&dir).join("Zed").join("settings.json")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_targets_explicit_list_parses_and_rejects_unknown() {
        let roots = roots();
        let names = vec!["codex".to_string(), "zed".to_string()];
        assert_eq!(
            resolve_targets(&roots, Some(&names)).unwrap(),
            vec![Harness::Codex, Harness::Zed]
        );
        let bad = vec!["nope".to_string()];
        assert!(resolve_targets(&roots, Some(&bad)).is_err());
    }

    #[test]
    fn resolve_targets_default_never_includes_claude_user() {
        let dir = std::env::temp_dir().join(format!(
            "openhavn-mcp-targets-default-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        let roots = Roots {
            home: dir.clone(),
            project_root: dir.clone(),
        };
        let resolved = resolve_targets(&roots, None).unwrap();
        assert!(resolved.contains(&Harness::ClaudeProject));
        assert!(resolved.contains(&Harness::Codex));
        assert!(!resolved.contains(&Harness::ClaudeUser));
        assert!(!resolved.contains(&Harness::Zed), "no Zed dir was created");
        std::fs::remove_dir_all(&dir).ok();
    }
}
