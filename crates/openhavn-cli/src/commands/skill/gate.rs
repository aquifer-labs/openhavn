// SPDX-License-Identifier: Apache-2.0

//! The admission gate: deterministic, typed checks run on a fetched skill *before* any install —
//! never prompt-based, so every decision (admit or one specific [`Rejection`]) is mechanically
//! loggable to the equipment log.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::frontmatter;

/// Total on-disk size, after excluding `.git`, a fetched skill may not exceed.
pub const MAX_SIZE_BYTES: u64 = 2 * 1024 * 1024;
/// File count, after excluding `.git`, a fetched skill may not exceed.
pub const MAX_FILE_COUNT: usize = 200;

/// A skill that cleared every gate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedSkill {
    pub name: String,
    pub description: String,
    /// sha256 over the canonical walk: sorted relative paths, each contributing
    /// `path bytes + 0x00 + file contents` to the hash.
    pub content_sha256: String,
    /// Sorted relative file paths making up the skill (`.git` excluded), used to copy the skill
    /// into each install target.
    pub files: Vec<PathBuf>,
}

/// The gate's verdict: either the skill is admitted, or it is rejected for exactly one typed
/// reason. I/O failures that prevent evaluating the gate at all (e.g. an unreadable file) are
/// not part of this type — they bubble out of [`evaluate`] as a hard `Err` instead, since they
/// are not a considered policy decision the equipment log should record as a `reject`.
#[derive(Debug)]
pub enum GateOutcome {
    Admitted(AdmittedSkill),
    Rejected(Rejection),
}

/// Every way the admission gate can refuse a fetched skill — typed, never a free-form message
/// only, so callers (and the equipment log) can match on *why* deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    MissingSkillMd,
    MissingName,
    MissingDescription,
    UnsafeName { name: String },
    PathEscape { rel_path: String },
    TooManyFiles { count: usize, max: usize },
    TooLarge { size: u64, max: u64 },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::MissingSkillMd => write!(f, "no SKILL.md found in the fetched skill"),
            Rejection::MissingName => {
                write!(f, "SKILL.md frontmatter is missing a non-empty \"name\"")
            }
            Rejection::MissingDescription => write!(
                f,
                "SKILL.md frontmatter is missing a non-empty \"description\""
            ),
            Rejection::UnsafeName { name } => write!(
                f,
                "skill name {name:?} is not a safe slug (expected [a-z0-9-_]+)"
            ),
            Rejection::PathEscape { rel_path } => {
                write!(f, "path escapes the skill directory: {rel_path}")
            }
            Rejection::TooManyFiles { count, max } => {
                write!(f, "skill has {count} files, exceeds the {max}-file limit")
            }
            Rejection::TooLarge { size, max } => {
                write!(f, "skill is {size} bytes, exceeds the {max}-byte limit")
            }
        }
    }
}

impl std::error::Error for Rejection {}

/// Run every admission check on `skill_dir`, in order: `SKILL.md` exists, frontmatter has a
/// non-empty name and description, the name is a safe slug, no entry escapes the directory, the
/// file count is within limits, and the total size is within limits. Returns the first
/// [`Rejection`] hit, or an [`AdmittedSkill`] once all checks pass.
pub fn evaluate(skill_dir: &Path) -> Result<GateOutcome> {
    let skill_md = skill_dir.join("SKILL.md");
    if !skill_md.is_file() {
        return Ok(GateOutcome::Rejected(Rejection::MissingSkillMd));
    }
    let content = std::fs::read_to_string(&skill_md)
        .with_context(|| format!("reading {}", skill_md.display()))?;
    let fm = frontmatter::parse(&content);

    let Some(name) = non_empty(fm.name) else {
        return Ok(GateOutcome::Rejected(Rejection::MissingName));
    };
    let Some(description) = non_empty(fm.description) else {
        return Ok(GateOutcome::Rejected(Rejection::MissingDescription));
    };
    if !is_safe_slug(&name) {
        return Ok(GateOutcome::Rejected(Rejection::UnsafeName { name }));
    }

    let files = walk_files(skill_dir)?;
    if let Some(rejection) = find_escape(skill_dir, &files) {
        return Ok(GateOutcome::Rejected(rejection));
    }
    if files.len() > MAX_FILE_COUNT {
        return Ok(GateOutcome::Rejected(Rejection::TooManyFiles {
            count: files.len(),
            max: MAX_FILE_COUNT,
        }));
    }
    let total_size: u64 = files
        .iter()
        .map(|rel| {
            std::fs::metadata(skill_dir.join(rel))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
    if total_size > MAX_SIZE_BYTES {
        return Ok(GateOutcome::Rejected(Rejection::TooLarge {
            size: total_size,
            max: MAX_SIZE_BYTES,
        }));
    }

    let content_sha256 = hash_files(skill_dir, &files)?;
    Ok(GateOutcome::Admitted(AdmittedSkill {
        name,
        description,
        content_sha256,
        files,
    }))
}

/// sha256 of an already-installed (or already-fetched) skill directory, recomputed the same way
/// [`evaluate`] computes [`AdmittedSkill::content_sha256`] — used for drift detection (`skill
/// list`) and change detection (`skill update`), neither of which re-runs the size/name policy
/// checks (those only ever gate a *new* install).
pub fn hash_dir(root: &Path) -> Result<String> {
    let files = walk_files(root)?;
    hash_files(root, &files)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn is_safe_slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Sorted relative file paths under `root`, recursing into subdirectories but excluding any
/// `.git` directory (version-control plumbing, never part of a skill's content).
fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_into(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_into(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading directory {}", dir.display()))?;
        let path = entry.path();
        if entry.file_name() == ".git" && path.is_dir() {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            walk_into(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("path was built from root")
                .to_path_buf();
            out.push(rel);
        }
    }
    Ok(())
}

/// A symlink whose target is absolute, or contains a `..` component, escapes the skill
/// directory. Only [`walk_files`] entries are inspected (via `read_link`, which only succeeds
/// for an actual symlink), so regular files can never trigger this.
fn find_escape(root: &Path, files: &[PathBuf]) -> Option<Rejection> {
    for rel in files {
        if let Ok(target) = std::fs::read_link(root.join(rel)) {
            let escapes = target.is_absolute()
                || target
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir));
            if escapes {
                return Some(Rejection::PathEscape {
                    rel_path: rel.display().to_string(),
                });
            }
        }
    }
    None
}

fn hash_files(root: &Path, files: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for rel in files {
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        let bytes =
            std::fs::read(root.join(rel)).with_context(|| format!("reading {}", rel.display()))?;
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openhavn-skill-gate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill_md(dir: &Path, frontmatter: &str) {
        std::fs::write(dir.join("SKILL.md"), frontmatter).unwrap();
    }

    #[test]
    fn valid_skill_admits_with_stable_hash() {
        let dir = scratch("valid");
        write_skill_md(
            &dir,
            "---\nname: demo-skill\ndescription: A demo\n---\nBody.\n",
        );
        std::fs::write(dir.join("helper.txt"), b"hello").unwrap();

        let outcome = evaluate(&dir).unwrap();
        let GateOutcome::Admitted(admitted) = outcome else {
            panic!("expected admission");
        };
        assert_eq!(admitted.name, "demo-skill");
        assert_eq!(admitted.description, "A demo");
        assert_eq!(
            admitted.files,
            vec![PathBuf::from("SKILL.md"), PathBuf::from("helper.txt")]
        );

        // Re-evaluating the same content is stable and matches `hash_dir`.
        let again = evaluate(&dir).unwrap();
        let GateOutcome::Admitted(admitted_again) = again else {
            panic!("expected admission");
        };
        assert_eq!(admitted.content_sha256, admitted_again.content_sha256);
        assert_eq!(hash_dir(&dir).unwrap(), admitted.content_sha256);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_skill_md_is_rejected() {
        let dir = scratch("missing-skill-md");
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::MissingSkillMd)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_name_is_rejected() {
        let dir = scratch("missing-name");
        write_skill_md(&dir, "---\ndescription: A demo\n---\n");
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::MissingName)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_description_is_rejected() {
        let dir = scratch("missing-description");
        write_skill_md(&dir, "---\nname: demo\n---\n");
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::MissingDescription)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsafe_name_is_rejected() {
        let dir = scratch("unsafe-name");
        write_skill_md(&dir, "---\nname: Not A Slug!\ndescription: A demo\n---\n");
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::UnsafeName { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn too_many_files_is_rejected() {
        let dir = scratch("too-many-files");
        write_skill_md(&dir, "---\nname: demo\ndescription: A demo\n---\n");
        for i in 0..MAX_FILE_COUNT {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::TooManyFiles { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn too_large_is_rejected() {
        let dir = scratch("too-large");
        write_skill_md(&dir, "---\nname: demo\ndescription: A demo\n---\n");
        let big = vec![0u8; (MAX_SIZE_BYTES + 1) as usize];
        std::fs::write(dir.join("big.bin"), big).unwrap();
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::TooLarge { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn absolute_symlink_target_is_rejected() {
        let dir = scratch("symlink-escape");
        write_skill_md(&dir, "---\nname: demo\ndescription: A demo\n---\n");
        std::os::unix::fs::symlink("/etc/passwd", dir.join("evil")).unwrap();
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::PathEscape { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn relative_dotdot_symlink_target_is_rejected() {
        let dir = scratch("symlink-dotdot");
        write_skill_md(&dir, "---\nname: demo\ndescription: A demo\n---\n");
        std::os::unix::fs::symlink("../../outside", dir.join("evil")).unwrap();
        let outcome = evaluate(&dir).unwrap();
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::PathEscape { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_directory_is_excluded_from_every_check() {
        let dir = scratch("dotgit-excluded");
        write_skill_md(&dir, "---\nname: demo\ndescription: A demo\n---\n");
        std::fs::create_dir_all(dir.join(".git").join("objects")).unwrap();
        std::fs::write(dir.join(".git").join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        let outcome = evaluate(&dir).unwrap();
        let GateOutcome::Admitted(admitted) = outcome else {
            panic!("expected admission");
        };
        assert_eq!(admitted.files, vec![PathBuf::from("SKILL.md")]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
