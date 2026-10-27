//! Thin wrapper over the git CLI. Every failure carries git's stderr.

use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn run_line(dir: &Path, args: &[&str]) -> Result<String> {
    let out = run(dir, args)?;
    Ok(String::from_utf8(out)
        .context("git output is not UTF-8")?
        .trim_end_matches('\n')
        .to_owned())
}

fn split_nul(bytes: &[u8]) -> impl Iterator<Item = PathBuf> + '_ {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(OsStr::from_bytes(s)))
}

pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    let top = run_line(dir, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(&top).with_context(|| format!("failed to resolve {top}"))
}

pub fn head(dir: &Path) -> Result<String> {
    run_line(dir, &["rev-parse", "HEAD"])
}

pub fn common_dir(dir: &Path) -> Result<PathBuf> {
    let p = run_line(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Ok(PathBuf::from(p))
}

pub fn config(dir: &Path, key: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", key])
        .output()
        .context("failed to run git config")?;
    // git config --get exits with 1 when the key is not set
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => bail!("git config --get {key} failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
    }
}

/// All non-bare worktrees whose directory still exists, main worktree first, canonicalized.
pub fn worktrees(dir: &Path) -> Result<Vec<PathBuf>> {
    let out = run_line(dir, &["worktree", "list", "--porcelain"])?;
    parse_worktree_list(&out)
        .into_iter()
        .filter(|p| p.is_dir())
        .map(|p| std::fs::canonicalize(&p).with_context(|| format!("failed to resolve {}", p.display())))
        .collect()
}

pub fn parse_worktree_list(porcelain: &str) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for block in porcelain.split("\n\n") {
        let mut path = None;
        let mut bare = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if line == "bare" {
                bare = true;
            }
        }
        if let (Some(p), false) = (path, bare) {
            result.push(p);
        }
    }
    result
}

/// Files whose content in `dir`'s working tree differs from `commit`
/// (uncommitted changes included, untracked files excluded).
pub fn diff_names(dir: &Path, commit: &str) -> Result<HashSet<PathBuf>> {
    let out = run(dir, &["diff", "--name-only", "--no-renames", "-z", commit, "--"])?;
    Ok(split_nul(&out).collect())
}

pub fn tracked_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let out = run(dir, &["ls-files", "-z"])?;
    Ok(split_nul(&out).collect())
}

/// Whether every tracked file matches HEAD (untracked files are ignored).
pub fn tracked_clean(dir: &Path) -> Result<bool> {
    let out = run(dir, &["status", "--porcelain", "--untracked-files=no", "-z"])?;
    Ok(out.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_list_and_skips_bare() {
        let s = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                 worktree /bare\nbare\n\n\
                 worktree /wt one\nHEAD def\ndetached\n";
        assert_eq!(
            parse_worktree_list(s),
            vec![PathBuf::from("/repo"), PathBuf::from("/wt one")]
        );
    }
}
