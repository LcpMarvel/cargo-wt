//! post-checkout hook 的安装与处理。

use crate::{git, seed};
use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const MARKER: &str = "# installed by cargo-wt";
/// `cargo wt add` 自己负责 seed，通过该变量让 hook 跳过。
pub const SKIP_ENV: &str = "CARGO_WT_SKIP_HOOK";

fn script() -> String {
    format!("#!/bin/sh\n{MARKER}\nexec cargo-wt hook post-checkout \"$@\"\n")
}

pub fn install(dir: &Path) -> Result<PathBuf> {
    if let Some(p) = git::config(dir, "core.hooksPath")? {
        bail!("本仓库设置了 core.hooksPath={p}，请在该目录的 post-checkout 中调用: cargo-wt hook post-checkout \"$@\"");
    }
    let hooks = git::common_dir(dir)?.join("hooks");
    let path = hooks.join("post-checkout");
    if path.exists() {
        let existing = std::fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()))?;
        if existing.contains(MARKER) {
            return Ok(path);
        }
        bail!(
            "{} 已存在，拒绝覆盖；请在其中追加一行: cargo-wt hook post-checkout \"$@\"",
            path.display()
        );
    }
    std::fs::create_dir_all(&hooks).with_context(|| format!("创建 {}", hooks.display()))?;
    std::fs::write(&path, script()).with_context(|| format!("写入 {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// `git worktree add` 触发 post-checkout 时，前一个 HEAD 是全零 SHA（SHA-1 或 SHA-256）。
pub fn is_null_sha(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

pub fn post_checkout(prev: &str) -> Result<()> {
    if std::env::var_os(SKIP_ENV).is_some() || !is_null_sha(prev) {
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    let top = git::toplevel(&cwd)?;
    if !top.join("Cargo.toml").is_file() {
        return Ok(());
    }
    crate::report(seed::seed(&top, None)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_sha() {
        assert!(is_null_sha(&"0".repeat(40)));
        assert!(is_null_sha(&"0".repeat(64)));
        assert!(!is_null_sha(""));
        assert!(!is_null_sha("0000000000000000000000000000000000000001"));
    }
}
