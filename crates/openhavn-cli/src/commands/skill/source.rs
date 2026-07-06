// SPDX-License-Identifier: Apache-2.0

//! Fetching a skill from its source: a local directory, or a git repository.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};

/// A fetched skill, ready for the admission gate.
#[derive(Debug)]
pub struct Fetched {
    /// The resolved skill directory (containing, if valid, a `SKILL.md`).
    pub dir: PathBuf,
    pub kind: SourceKind,
    /// The resolved commit for a git source (`git rev-parse HEAD` in the shallow clone).
    pub git_ref: Option<String>,
    /// Owns the temp clone directory for a git source, removed on drop; `None` for a local
    /// source (nothing to clean up — `dir` is the caller's own directory).
    _clone_root: Option<TempDir>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Local,
    Git,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Local => "local",
            SourceKind::Git => "git",
        }
    }
}

#[derive(Debug)]
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fetch `source`: a local directory path, or a git source (any URL ending `.git`, or one
/// starting `https://github.com/`).
pub fn fetch(source: &str) -> Result<Fetched> {
    if is_git_source(source) {
        fetch_git(source, None)
    } else {
        let dir = PathBuf::from(source);
        if !dir.is_dir() {
            bail!(
                "local skill source not found or not a directory: {}",
                dir.display()
            );
        }
        Ok(Fetched {
            dir,
            kind: SourceKind::Local,
            git_ref: None,
            _clone_root: None,
        })
    }
}

fn is_git_source(source: &str) -> bool {
    source.starts_with("https://github.com/") || source.contains(".git")
}

/// Splits a git source string into `(repo_url_to_clone, optional_subdir)`. A `.git` boundary
/// (bare-repo convention, e.g. `.../name.git` or `.../name.git/sub/dir`) takes priority; failing
/// that, a `https://github.com/<owner>/<repo>[/<subdir>]` URL's first two path segments are the
/// repo, anything after is the subdir.
fn split_git_source(source: &str) -> (String, Option<String>) {
    if let Some(idx) = source.find(".git") {
        let boundary = idx + ".git".len();
        let repo = source[..boundary].to_string();
        let rest = source[boundary..].trim_start_matches('/');
        return (repo, (!rest.is_empty()).then(|| rest.to_string()));
    }
    if let Some(rest) = source.strip_prefix("https://github.com/") {
        let mut segments = rest.splitn(3, '/');
        let owner = segments.next().unwrap_or_default();
        let repo = segments.next().unwrap_or_default();
        let subdir = segments.next();
        return (
            format!("https://github.com/{owner}/{repo}"),
            subdir.map(str::to_string),
        );
    }
    (source.to_string(), None)
}

static FETCH_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fetch_git(source: &str, branch: Option<&str>) -> Result<Fetched> {
    let (repo_url, subdir) = split_git_source(source);

    let unique = FETCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let clone_root = std::env::temp_dir().join(format!(
        "openhavn-skill-fetch-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&clone_root)
        .with_context(|| format!("creating {}", clone_root.display()))?;
    let guard = TempDir(clone_root.clone());
    let clone_dir = clone_root.join("repo");

    let mut cmd = std::process::Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(branch) = branch {
        cmd.arg("--branch").arg(branch);
    }
    cmd.arg(&repo_url)
        .arg(&clone_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = cmd
        .status()
        .with_context(|| format!("running git clone {repo_url}"))?;
    if !status.success() {
        bail!("git clone failed for {repo_url}");
    }

    let rev = std::process::Command::new("git")
        .arg("-C")
        .arg(&clone_dir)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .context("running git rev-parse HEAD")?;
    if !rev.status.success() {
        bail!("git rev-parse HEAD failed for {repo_url}");
    }
    let git_ref = String::from_utf8_lossy(&rev.stdout).trim().to_string();

    let skill_dir = match &subdir {
        Some(sub) => clone_dir.join(sub),
        None => clone_dir,
    };
    if !skill_dir.is_dir() {
        bail!(
            "resolved skill directory {} does not exist in {repo_url}",
            skill_dir.display()
        );
    }

    Ok(Fetched {
        dir: skill_dir,
        kind: SourceKind::Git,
        git_ref: Some(git_ref),
        _clone_root: Some(guard),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_git_source_matches_github_and_dot_git() {
        assert!(is_git_source("https://github.com/owner/repo"));
        assert!(is_git_source("https://example.com/owner/repo.git"));
        assert!(is_git_source("/tmp/fixtures/repo.git"));
        assert!(!is_git_source("/tmp/local/skill-dir"));
    }

    #[test]
    fn split_git_source_dot_git_boundary_with_and_without_subdir() {
        let (repo, subdir) = split_git_source("/tmp/fixtures/repo.git");
        assert_eq!(repo, "/tmp/fixtures/repo.git");
        assert_eq!(subdir, None);

        let (repo, subdir) = split_git_source("/tmp/fixtures/repo.git/skills/demo");
        assert_eq!(repo, "/tmp/fixtures/repo.git");
        assert_eq!(subdir.as_deref(), Some("skills/demo"));
    }

    #[test]
    fn split_git_source_github_url_with_and_without_subdir() {
        let (repo, subdir) = split_git_source("https://github.com/owner/repo");
        assert_eq!(repo, "https://github.com/owner/repo");
        assert_eq!(subdir, None);

        let (repo, subdir) = split_git_source("https://github.com/owner/repo/skills/demo");
        assert_eq!(repo, "https://github.com/owner/repo");
        assert_eq!(subdir.as_deref(), Some("skills/demo"));
    }

    #[test]
    fn fetch_local_rejects_missing_directory() {
        let missing = std::env::temp_dir().join("openhavn-skill-source-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(fetch(missing.to_str().unwrap()).is_err());
    }

    #[test]
    fn fetch_local_accepts_existing_directory() {
        let dir = std::env::temp_dir().join(format!(
            "openhavn-skill-source-local-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fetched = fetch(dir.to_str().unwrap()).unwrap();
        assert_eq!(fetched.kind, SourceKind::Local);
        assert_eq!(fetched.dir, dir);
        assert_eq!(fetched.git_ref, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Exercises the real `fetch` entry point (which only ever passes `branch: None`) against a
    /// local `git init` fixture named with a `.git` suffix, so `is_git_source` routes it through
    /// the git path exactly like a real bare-repo URL would.
    #[test]
    fn fetch_git_clones_local_fixture_and_resolves_head() {
        let fixture = std::env::temp_dir().join(format!(
            "openhavn-skill-source-fixture-{}.git",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&fixture);
        std::fs::create_dir_all(&fixture).unwrap();
        run_git(&fixture, &["init", "-q", "-b", "main"]);
        run_git(&fixture, &["config", "user.email", "test@example.com"]);
        run_git(&fixture, &["config", "user.name", "Test"]);
        std::fs::write(
            fixture.join("SKILL.md"),
            "---\nname: demo\ndescription: A demo\n---\n",
        )
        .unwrap();
        run_git(&fixture, &["add", "."]);
        run_git(&fixture, &["commit", "-q", "-m", "init"]);

        let expected_head = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&fixture)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let fetched = fetch(fixture.to_str().unwrap()).unwrap();
        assert_eq!(fetched.kind, SourceKind::Git);
        assert_eq!(fetched.git_ref.as_deref(), Some(expected_head.as_str()));
        assert!(fetched.dir.join("SKILL.md").is_file());

        std::fs::remove_dir_all(&fixture).ok();
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }
}
