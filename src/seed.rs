//! seed: let a freshly created worktree inherit another worktree's build artifacts.

use crate::{apfs, git};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Outcome {
    pub source: PathBuf,
    pub aligned: usize,
    pub skipped: usize,
}

/// Cargo's target directory relative to the worktree root. Errors if it lies
/// outside the worktree (e.g. `CARGO_TARGET_DIR` is set), since cargo-wt only
/// manages artifacts inside worktrees.
pub fn target_rel(top: &Path) -> Result<PathBuf> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path"])
        .arg(top.join("Cargo.toml"))
        .output()
        .context("failed to run cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("failed to parse cargo metadata output")?;
    let dir = v["target_directory"].as_str().context("cargo metadata has no target_directory")?;
    let dir = PathBuf::from(dir);
    match dir.strip_prefix(top) {
        Ok(rel) if !rel.as_os_str().is_empty() => Ok(rel.to_owned()),
        _ => bail!(
            "target directory {} is outside worktree {}; unset CARGO_TARGET_DIR / build.target-dir",
            dir.display(),
            top.display()
        ),
    }
}

pub fn seed(new_wt: &Path, from: Option<&Path>) -> Result<Option<Outcome>> {
    let top = git::toplevel(new_wt)?;
    if !top.join("Cargo.toml").is_file() {
        bail!("no Cargo.toml at the root of {}", top.display());
    }
    let rel = target_rel(&top)?;
    let new_target = top.join(&rel);
    if new_target.exists() {
        bail!("{} already exists; refusing to overwrite", new_target.display());
    }
    // Tracked files of the new worktree must equal HEAD, otherwise "same as source" does not hold
    if !git::tracked_clean(&top)? {
        bail!("{} has uncommitted changes; cannot safely inherit artifacts", top.display());
    }
    let new_head = git::head(&top)?;

    let Some((src, diff)) = pick_source(&top, &rel, &new_head, from)? else {
        return Ok(None);
    };
    let src_target = src.join(&rel);
    apfs::ensure_apfs(&src_target)?;
    apfs::ensure_same_volume(&src_target, &top)?;

    apfs::clone_tree(&src_target, &new_target)?;
    let finish = || -> Result<usize> {
        restore_hardlinks(&src_target, &new_target)?;
        align_mtimes(&src, &top, &diff)
    };
    match finish() {
        Ok(aligned) => Ok(Some(Outcome { source: src, aligned, skipped: diff.len() })),
        Err(e) => {
            // Never leave artifacts behind with only part of the mtimes aligned
            if let Err(rm) = std::fs::remove_dir_all(&new_target) {
                return Err(e.context(format!("and failed to clean up {}: {rm}", new_target.display())));
            }
            Err(e)
        }
    }
}

type Pick = Option<(PathBuf, HashSet<PathBuf>)>;

fn pick_source(top: &Path, rel: &Path, new_head: &str, from: Option<&Path>) -> Result<Pick> {
    let all = git::worktrees(top)?;
    if let Some(from) = from {
        let from = std::fs::canonicalize(from).with_context(|| format!("failed to resolve {}", from.display()))?;
        if from == top || !all.contains(&from) {
            bail!("{} is not another worktree of this repository", from.display());
        }
        if !from.join(rel).is_dir() {
            bail!("{} has no target directory {}", from.display(), rel.display());
        }
        let diff = git::diff_names(&from, new_head)?;
        return Ok(Some((from, diff)));
    }
    let mut best: Pick = None;
    for wt in all.into_iter().filter(|w| w != top && w.join(rel).is_dir()) {
        let diff = git::diff_names(&wt, new_head)?;
        if best.as_ref().is_none_or(|(_, d)| diff.len() < d.len()) {
            best = Some((wt, diff));
        }
    }
    Ok(best)
}

/// Cloning a directory with clonefile turns hardlinks into independent clones.
/// rustc's incremental compilation hardlinks `deps/*.rcgu.o` with files under
/// `incremental/`; recreate the source's hardlinks in the clone so its layout
/// matches.
fn restore_hardlinks(src_target: &Path, new_target: &Path) -> Result<()> {
    let mut groups: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    collect_hardlinks(src_target, src_target, &mut groups)?;
    for paths in groups.values().filter(|p| p.len() > 1) {
        let first = new_target.join(&paths[0]);
        for rel in &paths[1..] {
            let p = new_target.join(rel);
            std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
            std::fs::hard_link(&first, &p).with_context(|| format!("hardlink {} -> {}", p.display(), first.display()))?;
        }
    }
    Ok(())
}

fn collect_hardlinks(root: &Path, dir: &Path, groups: &mut HashMap<u64, Vec<PathBuf>>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            collect_hardlinks(root, &path, groups)?;
        } else if meta.is_file() && meta.nlink() > 1 {
            groups.entry(meta.ino()).or_default().push(path.strip_prefix(root)?.to_owned());
        }
    }
    Ok(())
}

/// Copy the source file's mtime onto every tracked file whose content matches
/// the source. Returns the number of files aligned.
fn align_mtimes(src: &Path, top: &Path, diff: &HashSet<PathBuf>) -> Result<usize> {
    let mut aligned = 0;
    for f in git::tracked_files(top)? {
        if diff.contains(&f) {
            continue;
        }
        let dst = top.join(&f);
        // Skip symlinks and submodule directories: cargo does not read their mtime directly
        if !std::fs::symlink_metadata(&dst)
            .with_context(|| format!("stat {}", dst.display()))?
            .is_file()
        {
            continue;
        }
        let src_file = src.join(&f);
        let src_meta =
            std::fs::symlink_metadata(&src_file).with_context(|| format!("stat {}", src_file.display()))?;
        if !src_meta.is_file() {
            bail!("{} matches the new worktree according to git but is not a regular file", src_file.display());
        }
        apfs::copy_mtime(&src_meta, &dst)?;
        aligned += 1;
    }
    Ok(aligned)
}
