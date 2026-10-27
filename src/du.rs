//! du：按 APFS 独占大小统计各 worktree 的产物占用。

use crate::{apfs, git, seed};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct Usage {
    pub worktree: PathBuf,
    pub present: bool,
    /// 已分配字节数
    pub logical: u64,
    /// 不与任何 clone 共享的字节数，即删掉该产物可释放的空间
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
    for entry in std::fs::read_dir(dir).with_context(|| format!("读取 {}", dir.display()))? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            walk(&path, seen, u)?;
        } else if meta.is_file() && seen.insert(meta.ino()) {
            // cargo 会把 deps/ 下的产物硬链接到上层，同一 inode 只算一次
            u.logical += meta.blocks() * 512;
            u.private += apfs::private_size(&path)?;
        }
    }
    Ok(())
}

/// 只给出必然成立的数字。共享块被多少个文件共享（包括同一 worktree 内
/// cargo 自己做的 clone，如 `deps/x-<hash>` 与 `debug/x`）无法从独占大小推出，
/// 所以物理占用只有上界（逻辑合计），没有下界。
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
    println!("{:<width$}  {:>10}  {:>10}", "worktree", "逻辑", "独占");
    for u in usages {
        if u.present {
            println!("{:<width$}  {:>10}  {:>10}", u.worktree.display(), human(u.logical), human(u.private));
        } else {
            println!("{:<width$}  {:>10}  {:>10}", u.worktree.display(), "-", "-");
        }
    }
    let t = totals(usages);
    println!(
        "合计：逻辑 {}（物理占用上限）· 独占 {}（删掉对应产物可释放）",
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
