// SPDX-License-Identifier: Apache-2.0

//! `openhavn skill install|list|update|rm` — governed cross-harness skill logistics (the
//! "Equipment" pillar): a provenance lockfile, drift detection, and a deterministic admission
//! gate whose decisions are appended to `~/.openhavn/equipment.jsonl`, in place of today's
//! ungoverned "copy files into `~/.claude/skills`" logistics.
//!
//! Every command has a thin, real-environment-resolving entry point (this module's `pub fn`s,
//! called directly from `main.rs`) and a `_core` function taking an explicit [`targets::Roots`]
//! — the same split `commands::init` uses for its `DetectRoots`, so the actual logic is
//! hermetically testable against a fake home/project root.

mod equipment;
mod frontmatter;
mod gate;
mod lock;
mod source;
mod targets;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};

use equipment::EquipmentRecord;
use gate::GateOutcome;
use lock::{LockEntry, LockTarget, Lockfile};
use targets::{resolve_targets, Harness, Roots};

/// `openhavn skill install <source> [--name] [--global] [--target a,b] [--dry-run] [--force]`.
pub fn install(
    source: &str,
    name: Option<&str>,
    global: bool,
    target: Option<&[String]>,
    dry_run: bool,
    force: bool,
) -> Result<i32> {
    install_core(&real_roots()?, source, name, global, target, dry_run, force)
}

/// `openhavn skill list [--global]`.
pub fn list(global: bool) -> Result<i32> {
    list_core(&real_roots()?, global)
}

/// `openhavn skill update [<name> | --all] [--global] [--dry-run]`.
pub fn update(name: Option<&str>, all: bool, global: bool, dry_run: bool) -> Result<i32> {
    update_core(&real_roots()?, name, all, global, dry_run)
}

/// `openhavn skill rm <name> [--global]`.
pub fn rm(name: &str, global: bool) -> Result<i32> {
    rm_core(&real_roots()?, name, global)
}

fn real_roots() -> Result<Roots> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let project_root = std::env::current_dir().context("resolving current directory")?;
    Ok(Roots { home, project_root })
}

/// RFC3339, second precision, `Z` suffix — matches `commands::run`'s `now_ts` style.
fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

// -------------------------------------------------------------------------------------------
// install
// -------------------------------------------------------------------------------------------

fn install_core(
    roots: &Roots,
    source: &str,
    name: Option<&str>,
    global: bool,
    target: Option<&[String]>,
    dry_run: bool,
    force: bool,
) -> Result<i32> {
    let harnesses = resolve_targets(roots, target)?;
    if harnesses.is_empty() {
        bail!(
            "no target harnesses selected: none detected on this machine \
             (pass --target explicitly)"
        );
    }

    let fetched = source::fetch(source)?;
    let admitted = match gate::evaluate(&fetched.dir)? {
        GateOutcome::Rejected(rejection) => {
            equipment::append(roots, &EquipmentRecord::reject(source, &rejection))?;
            println!("rejected: {rejection}");
            return Ok(1);
        }
        GateOutcome::Admitted(admitted) => admitted,
    };
    let name = name.unwrap_or(&admitted.name).to_string();

    let lock_path = lock::lock_path(roots, global);
    let mut lockfile = Lockfile::load(&lock_path)?;
    let mut entries = lockfile.entries()?;
    let existing = entries.iter().find(|e| e.name == name).cloned();

    let mut target_dirs: Vec<(Harness, PathBuf)> = harnesses
        .iter()
        .map(|&h| (h, h.dir(roots, &name, global)))
        .collect();
    // Reinstalling an already-locked name must never *shrink* its tracked target set: union in
    // whatever this entry was already installed to, so a narrower `--target` this time around
    // can't silently orphan a previously-installed directory the lock stops knowing about (that
    // would be exactly the ungoverned drift this whole module exists to prevent).
    if let Some(existing) = &existing {
        for target in &existing.targets {
            let already_covered = target_dirs
                .iter()
                .any(|(_, dir)| dir.as_path() == Path::new(&target.path));
            if !already_covered {
                if let Some(harness) = Harness::parse(&target.harness) {
                    target_dirs.push((harness, PathBuf::from(&target.path)));
                }
            }
        }
    }

    let already_up_to_date = existing.as_ref().is_some_and(|e| {
        e.content_sha256 == admitted.content_sha256
            && target_dirs.iter().all(|(_, dir)| dir.exists())
    });
    if already_up_to_date {
        println!(
            "up to date: {name} ({}) — {}",
            short_hash(&admitted.content_sha256),
            admitted.description
        );
        return Ok(0);
    }

    if dry_run {
        println!(
            "dry-run: would install '{name}' ({} files, hash {}) — {}",
            admitted.files.len(),
            short_hash(&admitted.content_sha256),
            admitted.description
        );
        for (harness, dir) in &target_dirs {
            println!("  {} -> {}", harness.name(), dir.display());
        }
        return Ok(0);
    }

    for (harness, dir) in &target_dirs {
        let owned = existing.as_ref().is_some_and(|e| {
            e.targets
                .iter()
                .any(|t| t.harness == harness.name() && Path::new(&t.path) == dir.as_path())
        });
        if dir.exists() && !owned && !force {
            bail!("unmanaged skill exists at {}, use --force", dir.display());
        }
    }

    for (_, dir) in &target_dirs {
        install_dir(&fetched.dir, &admitted.files, dir)?;
    }

    let target_names: Vec<String> = target_dirs
        .iter()
        .map(|(h, _)| h.name().to_string())
        .collect();

    let entry = LockEntry {
        name: name.clone(),
        source: source.to_string(),
        kind: fetched.kind.as_str().to_string(),
        git_ref: fetched.git_ref.clone(),
        content_sha256: admitted.content_sha256.clone(),
        installed_at: now_ts(),
        targets: target_dirs
            .iter()
            .map(|(h, dir)| LockTarget {
                harness: h.name().to_string(),
                path: dir.display().to_string(),
            })
            .collect(),
    };
    entries.retain(|e| e.name != name);
    entries.push(entry);
    lockfile.set_entries(&entries)?;
    lockfile.save(&lock_path)?;

    equipment::append(
        roots,
        &EquipmentRecord::admit(source, &name, &admitted.content_sha256, &target_names),
    )?;

    println!(
        "installed '{name}' ({}) to {} target(s):",
        short_hash(&admitted.content_sha256),
        target_dirs.len()
    );
    for (harness, dir) in &target_dirs {
        println!("  {} -> {}", harness.name(), dir.display());
    }
    Ok(0)
}

/// Copies `files` (relative to `src_root`) into `dest`, staging into a sibling temp directory
/// first and renaming into place, so a crash mid-copy never leaves `dest` half-written. Only
/// removes a pre-existing `dest` once the staged copy is complete and ready to take its place.
fn install_dir(src_root: &Path, files: &[PathBuf], dest: &Path) -> Result<()> {
    let parent = dest
        .parent()
        .with_context(|| format!("{} has no parent directory", dest.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating directory {}", parent.display()))?;

    let staging = parent.join(format!(
        ".{}.openhavn-staging-{}",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("skill"),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("creating staging directory {}", staging.display()))?;

    for rel in files {
        let from = src_root.join(rel);
        let to = staging.join(rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)
                .with_context(|| format!("creating directory {}", p.display()))?;
        }
        std::fs::copy(&from, &to)
            .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
    }

    if dest.exists() {
        std::fs::remove_dir_all(dest)
            .with_context(|| format!("removing previous {}", dest.display()))?;
    }
    std::fs::rename(&staging, dest)
        .with_context(|| format!("installing into {}", dest.display()))?;
    Ok(())
}

// -------------------------------------------------------------------------------------------
// list — locked skills + live drift per target
// -------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drift {
    Ok,
    Modified,
    Missing,
}

impl Drift {
    fn as_str(self) -> &'static str {
        match self {
            Drift::Ok => "OK",
            Drift::Modified => "MODIFIED",
            Drift::Missing => "MISSING",
        }
    }
}

/// `OK` when `path` exists and its recomputed content hash matches `recorded_sha256`,
/// `MODIFIED` when it exists but the hash differs, `MISSING` when it doesn't exist (or can no
/// longer be hashed at all, e.g. permissions).
fn drift_status(path: &Path, recorded_sha256: &str) -> Drift {
    if !path.exists() {
        return Drift::Missing;
    }
    match gate::hash_dir(path) {
        Ok(sha) if sha == recorded_sha256 => Drift::Ok,
        Ok(_) => Drift::Modified,
        Err(_) => Drift::Missing,
    }
}

fn list_core(roots: &Roots, global: bool) -> Result<i32> {
    let lock_path = lock::lock_path(roots, global);
    let entries = Lockfile::load(&lock_path)?.entries()?;
    if entries.is_empty() {
        println!("no skills locked ({})", lock_path.display());
        return Ok(0);
    }
    for entry in &entries {
        println!(
            "{}  {}  {}",
            entry.name,
            short_hash(&entry.content_sha256),
            entry.source
        );
        for target in &entry.targets {
            let path = PathBuf::from(&target.path);
            let status = drift_status(&path, &entry.content_sha256).as_str();
            println!("  {:<9} {:<9} {}", target.harness, status, path.display());
        }
    }
    Ok(0)
}

// -------------------------------------------------------------------------------------------
// update — refetch + re-gate + reinstall only on explicit request (pin+notify)
// -------------------------------------------------------------------------------------------

fn update_core(
    roots: &Roots,
    name: Option<&str>,
    all: bool,
    global: bool,
    dry_run: bool,
) -> Result<i32> {
    let lock_path = lock::lock_path(roots, global);
    let mut lockfile = Lockfile::load(&lock_path)?;
    let mut entries = lockfile.entries()?;

    let names: Vec<String> = if all {
        entries.iter().map(|e| e.name.clone()).collect()
    } else {
        let requested = name.context("provide a skill name, or pass --all")?;
        if !entries.iter().any(|e| e.name == requested) {
            bail!(
                "no locked skill named {requested:?} ({})",
                lock_path.display()
            );
        }
        vec![requested.to_string()]
    };
    if names.is_empty() {
        println!("no locked skills to update ({})", lock_path.display());
        return Ok(0);
    }

    let mut exit_code = 0;
    let mut changed = false;
    for name in &names {
        let idx = entries
            .iter()
            .position(|e| &e.name == name)
            .expect("name was drawn from entries above");
        let old = entries[idx].clone();

        let fetched = source::fetch(&old.source)?;
        let admitted = match gate::evaluate(&fetched.dir)? {
            GateOutcome::Rejected(rejection) => {
                equipment::append(roots, &EquipmentRecord::reject(&old.source, &rejection))?;
                println!("{name}: rejected on refetch: {rejection}");
                exit_code = 1;
                continue;
            }
            GateOutcome::Admitted(admitted) => admitted,
        };

        if admitted.content_sha256 == old.content_sha256 {
            println!("{name}: up to date ({})", short_hash(&old.content_sha256));
            continue;
        }

        let ref_note = match (&old.git_ref, &fetched.git_ref) {
            (Some(old_ref), Some(new_ref)) if old_ref != new_ref => {
                format!(" ref {}->{}", short_hash(old_ref), short_hash(new_ref))
            }
            _ => String::new(),
        };
        println!(
            "{name}: update available {}->{}{ref_note}",
            short_hash(&old.content_sha256),
            short_hash(&admitted.content_sha256)
        );
        if dry_run {
            continue;
        }

        for target in &old.targets {
            install_dir(&fetched.dir, &admitted.files, Path::new(&target.path))?;
        }

        let target_names: Vec<String> = old.targets.iter().map(|t| t.harness.clone()).collect();
        entries[idx] = LockEntry {
            name: name.clone(),
            source: old.source.clone(),
            kind: old.kind.clone(),
            git_ref: fetched.git_ref.clone().or(old.git_ref.clone()),
            content_sha256: admitted.content_sha256.clone(),
            installed_at: now_ts(),
            targets: old.targets.clone(),
        };
        changed = true;

        equipment::append(
            roots,
            &EquipmentRecord::update(&old.source, name, &admitted.content_sha256, &target_names),
        )?;
    }

    if changed {
        lockfile.set_entries(&entries)?;
        lockfile.save(&lock_path)?;
    }
    Ok(exit_code)
}

// -------------------------------------------------------------------------------------------
// rm — remove only lock-owned paths
// -------------------------------------------------------------------------------------------

fn rm_core(roots: &Roots, name: &str, global: bool) -> Result<i32> {
    let lock_path = lock::lock_path(roots, global);
    let mut lockfile = Lockfile::load(&lock_path)?;
    let mut entries = lockfile.entries()?;
    let Some(idx) = entries.iter().position(|e| e.name == name) else {
        bail!("no locked skill named {name:?} ({})", lock_path.display());
    };
    let entry = entries.remove(idx);

    for target in &entry.targets {
        let path = PathBuf::from(&target.path);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("removing {}", path.display()))?;
        }
    }

    lockfile.set_entries(&entries)?;
    lockfile.save(&lock_path)?;

    let target_names: Vec<String> = entry.targets.iter().map(|t| t.harness.clone()).collect();
    equipment::append(
        roots,
        &EquipmentRecord::remove(&entry.source, name, &target_names),
    )?;

    println!("removed '{name}' from {} target(s)", entry.targets.len());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_roots(tag: &str) -> (PathBuf, Roots) {
        let base =
            std::env::temp_dir().join(format!("openhavn-skill-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let project_root = base.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        (base, Roots { home, project_root })
    }

    fn write_demo_skill_source(dir: &Path, name: &str, description: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n"),
        )
        .unwrap();
        std::fs::write(dir.join("body.txt"), body).unwrap();
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    #[test]
    fn install_project_scope_writes_all_three_targets_lock_and_equipment_log() {
        let (base, roots) = scratch_roots("install-project");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec![
            "claude".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
        ];

        let code = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        assert!(roots
            .project_root
            .join(".claude/skills/demo-skill/SKILL.md")
            .is_file());
        assert!(roots
            .project_root
            .join(".codex/skills/demo-skill/SKILL.md")
            .is_file());
        assert!(roots
            .project_root
            .join(".opencode/skill/demo-skill/SKILL.md")
            .is_file());
        assert_eq!(
            std::fs::read_to_string(
                roots
                    .project_root
                    .join(".claude/skills/demo-skill/body.txt")
            )
            .unwrap(),
            "v1"
        );

        let lock_path = lock::lock_path(&roots, false);
        let entries = Lockfile::load(&lock_path).unwrap().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "demo-skill");
        assert_eq!(entries[0].kind, "local");
        assert_eq!(entries[0].targets.len(), 3);

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert_eq!(equipment_log.lines().count(), 1);
        assert!(equipment_log.contains("\"decision\":\"admit\""));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_global_scope_writes_home_anchored_targets() {
        let (base, roots) = scratch_roots("install-global");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec![
            "claude".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
        ];

        let code = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            true,
            Some(&targets),
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        assert!(roots
            .home
            .join(".claude/skills/demo-skill/SKILL.md")
            .is_file());
        assert!(roots
            .home
            .join(".codex/skills/demo-skill/SKILL.md")
            .is_file());
        assert!(roots
            .home
            .join(".config/opencode/skill/demo-skill/SKILL.md")
            .is_file());

        let lock_path = lock::lock_path(&roots, true);
        assert_eq!(lock_path, roots.home.join(".openhavn").join("skills.lock"));
        let entries = Lockfile::load(&lock_path).unwrap().entries().unwrap();
        assert_eq!(entries[0].targets.len(), 3);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_is_idempotent_when_content_unchanged() {
        let (base, roots) = scratch_roots("install-idempotent");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec!["claude".to_string()];

        let first = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();
        assert_eq!(first, 0);
        let second = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();
        assert_eq!(second, 0);

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert_eq!(
            equipment_log.lines().count(),
            1,
            "second install must not append another equipment record"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn reinstall_with_a_narrower_target_set_still_keeps_previously_tracked_targets() {
        let (base, roots) = scratch_roots("install-narrower-target");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");

        let both = vec!["claude".to_string(), "codex".to_string()];
        install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&both),
            false,
            false,
        )
        .unwrap();

        // Reinstall requesting only `claude` this time.
        let claude_only = vec!["claude".to_string()];
        let code = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&claude_only),
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        // The codex directory must still exist on disk...
        let codex_dir = Harness::Codex.dir(&roots, "demo-skill", false);
        assert!(
            codex_dir.is_dir(),
            "a narrower --target must not delete a previously-installed directory"
        );
        // ...and the lock must still know about it (not orphan it).
        let lock_path = lock::lock_path(&roots, false);
        let entries = Lockfile::load(&lock_path).unwrap().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].targets.len(), 2);
        assert!(entries[0].targets.iter().any(|t| t.harness == "codex"));
        assert!(entries[0].targets.iter().any(|t| t.harness == "claude"));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_refuses_unmanaged_collision_unless_forced() {
        let (base, roots) = scratch_roots("install-collision");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec!["claude".to_string()];

        let dest = Harness::Claude.dir(&roots, "demo-skill", false);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("unmanaged.txt"), "pre-existing").unwrap();

        let err = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--force"), "{err}");
        assert!(
            dest.join("unmanaged.txt").is_file(),
            "must not touch the unmanaged dir without --force"
        );

        let code = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            true,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(dest.join("SKILL.md").is_file());
        assert!(
            !dest.join("unmanaged.txt").exists(),
            "force replaces the whole directory"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn drift_status_reports_ok_modified_and_missing() {
        let (base, roots) = scratch_roots("drift");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec!["claude".to_string()];
        install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();

        let lock_path = lock::lock_path(&roots, false);
        let entry = Lockfile::load(&lock_path)
            .unwrap()
            .entries()
            .unwrap()
            .remove(0);
        let dest = PathBuf::from(&entry.targets[0].path);

        assert_eq!(drift_status(&dest, &entry.content_sha256), Drift::Ok);

        std::fs::write(dest.join("body.txt"), "tampered").unwrap();
        assert_eq!(drift_status(&dest, &entry.content_sha256), Drift::Modified);

        std::fs::remove_dir_all(&dest).unwrap();
        assert_eq!(drift_status(&dest, &entry.content_sha256), Drift::Missing);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn update_local_source_change_bumps_hash_and_logs_update() {
        let (base, roots) = scratch_roots("update-local");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec!["claude".to_string()];
        install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();

        let lock_path = lock::lock_path(&roots, false);
        let old_sha = Lockfile::load(&lock_path).unwrap().entries().unwrap()[0]
            .content_sha256
            .clone();

        std::fs::write(source_dir.join("body.txt"), "v2").unwrap();

        let code = update_core(&roots, Some("demo-skill"), false, false, false).unwrap();
        assert_eq!(code, 0);

        let entries = Lockfile::load(&lock_path).unwrap().entries().unwrap();
        assert_ne!(entries[0].content_sha256, old_sha);

        let dest = PathBuf::from(&entries[0].targets[0].path);
        assert_eq!(
            std::fs::read_to_string(dest.join("body.txt")).unwrap(),
            "v2"
        );

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert!(equipment_log
            .lines()
            .any(|l| l.contains("\"decision\":\"update\"")));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn update_git_fixture_new_commit_picks_new_sha() {
        let (base, roots) = scratch_roots("update-git");
        let fixture = base.join("fixture.git");
        std::fs::create_dir_all(&fixture).unwrap();
        run_git(&fixture, &["init", "-q", "-b", "main"]);
        run_git(&fixture, &["config", "user.email", "test@example.com"]);
        run_git(&fixture, &["config", "user.name", "Test"]);
        std::fs::write(
            fixture.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: A demo\n---\n",
        )
        .unwrap();
        std::fs::write(fixture.join("body.txt"), "v1").unwrap();
        run_git(&fixture, &["add", "."]);
        run_git(&fixture, &["commit", "-q", "-m", "v1"]);

        let targets = vec!["claude".to_string()];
        install_core(
            &roots,
            fixture.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();

        let lock_path = lock::lock_path(&roots, false);
        let old_entry = Lockfile::load(&lock_path)
            .unwrap()
            .entries()
            .unwrap()
            .remove(0);
        assert_eq!(old_entry.kind, "git");

        std::fs::write(fixture.join("body.txt"), "v2").unwrap();
        run_git(&fixture, &["add", "."]);
        run_git(&fixture, &["commit", "-q", "-m", "v2"]);

        let code = update_core(&roots, Some("demo-skill"), false, false, false).unwrap();
        assert_eq!(code, 0);

        let new_entry = Lockfile::load(&lock_path)
            .unwrap()
            .entries()
            .unwrap()
            .remove(0);
        assert_ne!(new_entry.content_sha256, old_entry.content_sha256);
        assert_ne!(new_entry.git_ref, old_entry.git_ref);

        let dest = PathBuf::from(&new_entry.targets[0].path);
        assert_eq!(
            std::fs::read_to_string(dest.join("body.txt")).unwrap(),
            "v2"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn rm_removes_only_owned_targets_drops_lock_entry_and_logs_remove() {
        let (base, roots) = scratch_roots("rm");
        let source_dir = base.join("source-skill");
        write_demo_skill_source(&source_dir, "demo-skill", "A demo skill", "v1");
        let targets = vec!["claude".to_string(), "codex".to_string()];
        install_core(
            &roots,
            source_dir.to_str().unwrap(),
            None,
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();

        let claude_dir = Harness::Claude.dir(&roots, "demo-skill", false);
        let codex_dir = Harness::Codex.dir(&roots, "demo-skill", false);
        assert!(claude_dir.is_dir());
        assert!(codex_dir.is_dir());

        let code = rm_core(&roots, "demo-skill", false).unwrap();
        assert_eq!(code, 0);
        assert!(!claude_dir.exists());
        assert!(!codex_dir.exists());

        let lock_path = lock::lock_path(&roots, false);
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

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_rejected_skill_logs_reject_and_returns_one() {
        let (base, roots) = scratch_roots("install-reject");
        let source_dir = base.join("bad-skill");
        std::fs::create_dir_all(&source_dir).unwrap(); // no SKILL.md at all
        let targets = vec!["claude".to_string()];

        let code = install_core(
            &roots,
            source_dir.to_str().unwrap(),
            Some("whatever"),
            false,
            Some(&targets),
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, 1);
        assert!(!Harness::Claude.dir(&roots, "whatever", false).exists());

        let equipment_log =
            std::fs::read_to_string(roots.home.join(".openhavn").join("equipment.jsonl")).unwrap();
        assert!(equipment_log
            .lines()
            .any(|l| l.contains("\"decision\":\"reject\"")));

        std::fs::remove_dir_all(&base).ok();
    }
}
