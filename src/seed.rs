//! seed：让一个刚建好的 worktree 继承另一个 worktree 的构建产物。

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

/// cargo 产物目录相对 worktree 根的路径。产物不在 worktree 内（如设置了
/// `CARGO_TARGET_DIR`）时报错，因为 cargo-wt 只管理 worktree 内的产物。
pub fn target_rel(top: &Path) -> Result<PathBuf> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path"])
        .arg(top.join("Cargo.toml"))
        .output()
        .context("无法执行 cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata 失败: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("解析 cargo metadata 输出")?;
    let dir = v["target_directory"].as_str().context("cargo metadata 缺少 target_directory")?;
    let dir = PathBuf::from(dir);
    match dir.strip_prefix(top) {
        Ok(rel) if !rel.as_os_str().is_empty() => Ok(rel.to_owned()),
        _ => bail!(
            "产物目录 {} 不在 worktree {} 内；请取消 CARGO_TARGET_DIR / build.target-dir 设置",
            dir.display(),
            top.display()
        ),
    }
}

pub fn seed(new_wt: &Path, from: Option<&Path>) -> Result<Option<Outcome>> {
    let top = git::toplevel(new_wt)?;
    if !top.join("Cargo.toml").is_file() {
        bail!("{} 根目录没有 Cargo.toml", top.display());
    }
    let rel = target_rel(&top)?;
    let new_target = top.join(&rel);
    if new_target.exists() {
        bail!("{} 已存在，拒绝覆盖", new_target.display());
    }
    // 新 worktree 的已跟踪文件必须等于 HEAD，否则"与源相同"的判断不成立
    if !git::tracked_clean(&top)? {
        bail!("{} 有未提交的修改，无法安全地继承产物", top.display());
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
            // 不留下 mtime 只对齐了一部分的产物
            if let Err(rm) = std::fs::remove_dir_all(&new_target) {
                return Err(e.context(format!("且清理 {} 失败: {rm}", new_target.display())));
            }
            Err(e)
        }
    }
}

type Pick = Option<(PathBuf, HashSet<PathBuf>)>;

fn pick_source(top: &Path, rel: &Path, new_head: &str, from: Option<&Path>) -> Result<Pick> {
    let all = git::worktrees(top)?;
    if let Some(from) = from {
        let from = std::fs::canonicalize(from).with_context(|| format!("无法解析 {}", from.display()))?;
        if from == top || !all.contains(&from) {
            bail!("{} 不是本仓库的另一个 worktree", from.display());
        }
        if !from.join(rel).is_dir() {
            bail!("{} 没有产物目录 {}", from.display(), rel.display());
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

/// clonefile 克隆目录时会把硬链接拆成独立的 clone。rustc 增量编译把
/// `deps/*.rcgu.o` 与 `incremental/` 下的文件硬链接，这里按源目录的硬链接关系
/// 在克隆结果中重建，保持结构一致。
fn restore_hardlinks(src_target: &Path, new_target: &Path) -> Result<()> {
    let mut groups: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    collect_hardlinks(src_target, src_target, &mut groups)?;
    for paths in groups.values().filter(|p| p.len() > 1) {
        let first = new_target.join(&paths[0]);
        for rel in &paths[1..] {
            let p = new_target.join(rel);
            std::fs::remove_file(&p).with_context(|| format!("删除 {}", p.display()))?;
            std::fs::hard_link(&first, &p).with_context(|| format!("硬链接 {} -> {}", p.display(), first.display()))?;
        }
    }
    Ok(())
}

fn collect_hardlinks(root: &Path, dir: &Path, groups: &mut HashMap<u64, Vec<PathBuf>>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("读取 {}", dir.display()))? {
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

/// 对内容与源相同的已跟踪文件，复制源文件的 mtime。返回对齐的文件数。
fn align_mtimes(src: &Path, top: &Path, diff: &HashSet<PathBuf>) -> Result<usize> {
    let mut aligned = 0;
    for f in git::tracked_files(top)? {
        if diff.contains(&f) {
            continue;
        }
        let dst = top.join(&f);
        // 符号链接和子模块目录不对齐：它们不是 cargo 直接读取 mtime 的普通文件
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
            bail!("{} 在 git 看来与新 worktree 相同，但不是普通文件", src_file.display());
        }
        apfs::copy_mtime(&src_meta, &dst)?;
        aligned += 1;
    }
    Ok(aligned)
}
