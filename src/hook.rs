//! Installing and handling the post-checkout hook.

use crate::{git, seed};
use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const MARKER: &str = "# installed by cargo-wt";
/// `cargo wt add` seeds on its own and uses this variable to make the hook skip.
pub const SKIP_ENV: &str = "CARGO_WT_SKIP_HOOK";

fn script() -> String {
    format!("#!/bin/sh\n{MARKER}\nexec cargo-wt hook post-checkout \"$@\"\n")
}

pub fn install(dir: &Path) -> Result<PathBuf> {
    if let Some(p) = git::config(dir, "core.hooksPath")? {
        bail!("core.hooksPath is set to {p}; call this from the post-checkout hook there: cargo-wt hook post-checkout \"$@\"");
    }
    let hooks = git::common_dir(dir)?.join("hooks");
    let path = hooks.join("post-checkout");
    if path.exists() {
        let existing = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if existing.contains(MARKER) {
            return Ok(path);
        }
        bail!(
            "{} already exists; refusing to overwrite. Append this line to it: cargo-wt hook post-checkout \"$@\"",
            path.display()
        );
    }
    std::fs::create_dir_all(&hooks).with_context(|| format!("create {}", hooks.display()))?;
    std::fs::write(&path, script()).with_context(|| format!("write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// When `git worktree add` triggers post-checkout, the previous HEAD is the
/// all-zero SHA (SHA-1 or SHA-256).
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
