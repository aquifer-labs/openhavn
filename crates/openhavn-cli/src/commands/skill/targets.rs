// SPDX-License-Identifier: Apache-2.0

//! The skill path table: which directory a skill installs into, per harness and scope.
//!
//! Data-driven and easy to extend — adding a harness means adding one [`Harness`] variant and
//! one row to [`TARGETS`], nothing else in this module changes.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Filesystem anchors used for skill path resolution, overridable for tests — the same pattern
/// as `commands::init::DetectRoots`: nothing in this module (or its siblings) reads `$HOME` or
/// the current directory itself, so it is testable against a fake home/project root without
/// touching the real ones.
#[derive(Debug, Clone)]
pub struct Roots {
    pub home: PathBuf,
    pub project_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Opencode,
}

struct TargetDef {
    harness: Harness,
    /// Path segments under the project root for a project-scope install.
    project_segments: &'static [&'static str],
    /// Path segments under `$HOME` for a global-scope install.
    global_segments: &'static [&'static str],
}

/// One row per harness: `{harness, project_dir, global_dir}` as path segments joined onto the
/// project root / home respectively. Note `opencode` uses the singular `skill` directory name in
/// both scopes, unlike `claude`/`codex`'s plural `skills`.
const TARGETS: &[TargetDef] = &[
    TargetDef {
        harness: Harness::Claude,
        project_segments: &[".claude", "skills"],
        global_segments: &[".claude", "skills"],
    },
    TargetDef {
        harness: Harness::Codex,
        project_segments: &[".codex", "skills"],
        global_segments: &[".codex", "skills"],
    },
    TargetDef {
        harness: Harness::Opencode,
        project_segments: &[".opencode", "skill"],
        global_segments: &[".config", "opencode", "skill"],
    },
];

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Opencode];

    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
        }
    }

    pub fn parse(name: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.name() == name)
    }

    fn def(self) -> &'static TargetDef {
        TARGETS
            .iter()
            .find(|def| def.harness == self)
            .expect("every Harness variant has a TARGETS row")
    }

    /// The directory a skill named `name` installs to, in the given scope.
    pub fn dir(self, roots: &Roots, name: &str, global: bool) -> PathBuf {
        let def = self.def();
        let (base, segments) = if global {
            (&roots.home, def.global_segments)
        } else {
            (&roots.project_root, def.project_segments)
        };
        let mut path = base.clone();
        for segment in segments {
            path.push(segment);
        }
        path.push(name);
        path
    }

    /// Whether this harness looks installed on this machine — used to pick the default
    /// `--target` set, independent of install scope. Mirrors `commands::init`'s directory-based
    /// detection (e.g. `claude` is "present" when `~/.claude` exists).
    pub fn present(self, home: &Path) -> bool {
        match self {
            Harness::Claude => home.join(".claude").is_dir(),
            Harness::Codex => home.join(".codex").is_dir(),
            Harness::Opencode => {
                home.join(".opencode").is_dir() || home.join(".config").join("opencode").is_dir()
            }
        }
    }
}

/// Resolve `--target` into the harness list to install into: the explicit names if given
/// (erroring on an unrecognized one), else every harness [`Harness::present`] on this machine.
pub fn resolve_targets(roots: &Roots, target: Option<&[String]>) -> Result<Vec<Harness>> {
    match target {
        Some(names) => names
            .iter()
            .map(|name| {
                Harness::parse(name).ok_or_else(|| {
                    anyhow!("unknown harness {name:?} (expected one of: claude, codex, opencode)")
                })
            })
            .collect(),
        None => Ok(Harness::ALL
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
    fn claude_and_codex_use_plural_skills_dir_in_both_scopes() {
        let roots = roots();
        assert_eq!(
            Harness::Claude.dir(&roots, "foo", false),
            PathBuf::from("/work/proj/.claude/skills/foo")
        );
        assert_eq!(
            Harness::Claude.dir(&roots, "foo", true),
            PathBuf::from("/home/u/.claude/skills/foo")
        );
        assert_eq!(
            Harness::Codex.dir(&roots, "foo", false),
            PathBuf::from("/work/proj/.codex/skills/foo")
        );
        assert_eq!(
            Harness::Codex.dir(&roots, "foo", true),
            PathBuf::from("/home/u/.codex/skills/foo")
        );
    }

    #[test]
    fn opencode_uses_singular_skill_dir_and_config_subdir_when_global() {
        let roots = roots();
        assert_eq!(
            Harness::Opencode.dir(&roots, "foo", false),
            PathBuf::from("/work/proj/.opencode/skill/foo")
        );
        assert_eq!(
            Harness::Opencode.dir(&roots, "foo", true),
            PathBuf::from("/home/u/.config/opencode/skill/foo")
        );
    }

    #[test]
    fn resolve_targets_explicit_list_parses_and_rejects_unknown() {
        let roots = roots();
        let names = vec!["claude".to_string(), "opencode".to_string()];
        let resolved = resolve_targets(&roots, Some(&names)).unwrap();
        assert_eq!(resolved, vec![Harness::Claude, Harness::Opencode]);

        let bad = vec!["nope".to_string()];
        assert!(resolve_targets(&roots, Some(&bad)).is_err());
    }

    #[test]
    fn resolve_targets_default_picks_only_present_harnesses() {
        let dir = std::env::temp_dir().join(format!(
            "openhavn-skill-targets-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let roots = Roots {
            home: dir.clone(),
            project_root: dir.clone(),
        };
        let resolved = resolve_targets(&roots, None).unwrap();
        assert_eq!(resolved, vec![Harness::Claude]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
