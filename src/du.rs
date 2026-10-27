//! du: per-worktree artifact usage, based on APFS private size.

use crate::{apfs, git, seed};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct Usage {
    pub worktree: PathBuf,
    pub present: bool,
    /// Allocated bytes
    pub logical: u64,
    /// Bytes not shared with any clone, i.e. what deleting these artifacts frees
    pub private: u64,
}

pub fn collect(dir: &Path) -> Result<Vec<Usage>> {
    let top = git::toplevel(dir)?;
    let rel = seed::target_rel(&top)?;
    git::worktrees(&top)?
        .into_iter()
        .map(|wt| {
            let target = wt.join(&rel);
            let mut u = Usage { worktree: wt, present: target.is_dir(), logical: 0, private: 0 };
            if u.present {
                apfs::ensure_apfs(&target)?;
                walk(&target, &mut HashSet::new(), &mut u)?;
            }
            Ok(u)
        })
        .collect()
}

fn walk(dir: &Path, seen: &mut HashSet<u64>, u: &mut Usage) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            walk(&path, seen, u)?;
        } else if meta.is_file() && seen.insert(meta.ino()) {
            // rustc hardlinks incremental outputs; count each inode once
            u.logical += meta.blocks() * 512;
            u.private += apfs::private_size(&path)?;
        }
    }
    Ok(())
}

/// Only numbers that always hold. How many files share a block (including
/// clones cargo makes inside one worktree, such as `deps/x-<hash>` and
/// `debug/x`) cannot be derived from private sizes, so physical usage has an
/// upper bound (total logical) but no lower bound.
pub struct Totals {
    pub logical: u64,
    pub private: u64,
}

pub fn totals(usages: &[Usage]) -> Totals {
    Totals {
        logical: usages.iter().map(|u| u.logical).sum(),
        private: usages.iter().map(|u| u.private).sum(),
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

pub fn print_table(usages: &[Usage]) {
    let width = usages.iter().map(|u| u.worktree.as_os_str().len()).max().unwrap_or(8).max(8);
    println!("{:<width$}  {:>10}  {:>10}", "worktree", "logical", "private");
    for u in usages {
        if u.present {
            println!("{:<width$}  {:>10}  {:>10}", u.worktree.display(), human(u.logical), human(u.private));
        } else {
            println!("{:<width$}  {:>10}  {:>10}", u.worktree.display(), "-", "-");
        }
    }
    let t = totals(usages);
    println!(
        "total: logical {} (upper bound of physical usage) · private {} (freed by deleting the artifacts)",
        human(t.logical),
        human(t.private)
    );
}

pub fn to_json(usages: &[Usage]) -> serde_json::Value {
    let t = totals(usages);
    serde_json::json!({
        "worktrees": usages.iter().map(|u| serde_json::json!({
            "path": u.worktree,
            "target_present": u.present,
            "logical_bytes": u.logical,
            "private_bytes": u.private,
        })).collect::<Vec<_>>(),
        "total_logical_bytes": t.logical,
        "total_private_bytes": t.private,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(logical: u64, private: u64) -> Usage {
        Usage { worktree: PathBuf::new(), present: true, logical, private }
    }

    #[test]
    fn totals_sum() {
        let t = totals(&[u(100, 0), u(100, 0), u(100, 10)]);
        assert_eq!(t.logical, 300);
        assert_eq!(t.private, 10);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(300 << 20), "300.0 MB");
    }
}
