#[cfg(not(target_os = "macos"))]
compile_error!("cargo-wt only supports macOS (APFS clonefile)");

mod apfs;
mod du;
mod git;
mod hook;
mod seed;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// 在 macOS APFS 上让新 git worktree 通过 clonefile 继承已有的 Rust 构建产物
#[derive(Parser)]
#[command(name = "cargo wt", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// git worktree add，然后继承产物；其余参数原样传给 git
    Add {
        /// 指定产物来源 worktree，默认自动选差异最小的
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        git_args: Vec<OsString>,
    },
    /// 让已建好的 worktree 继承产物
    Seed {
        path: PathBuf,
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// 在本仓库安装 post-checkout hook，使任何方式新建的 worktree 都自动继承产物
    InstallHook,
    /// 统计各 worktree 产物的逻辑与独占大小
    Du {
        #[arg(long)]
        json: bool,
    },
    /// 由 git hook 调用
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        kind: HookKind,
    },
}

#[derive(Subcommand)]
enum HookKind {
    PostCheckout { prev: String, new: String, flag: String },
}

pub fn report(outcome: Option<seed::Outcome>) {
    match outcome {
        Some(o) => eprintln!(
            "cargo-wt: 已从 {} 继承产物（对齐 {} 个文件，{} 个差异文件保留新 mtime）",
            o.source.display(),
            o.aligned,
            o.skipped
        ),
        None => eprintln!("cargo-wt: 没有可继承产物的 worktree，跳过"),
    }
}

fn add(from: Option<PathBuf>, git_args: Vec<OsString>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let before = git::worktrees(&cwd)?;
    let status = Command::new("git")
        .args(["worktree", "add"])
        .args(&git_args)
        .env(hook::SKIP_ENV, "1")
        .status()
        .context("无法执行 git worktree add")?;
    if !status.success() {
        bail!("git worktree add 失败");
    }
    let mut created: Vec<_> = git::worktrees(&cwd)?.into_iter().filter(|w| !before.contains(w)).collect();
    if created.len() != 1 {
        bail!("无法确定新建的 worktree（新增 {} 个）", created.len());
    }
    report(seed::seed(&created.remove(0), from.as_deref())?);
    Ok(())
}

fn main() -> Result<()> {
    // 作为 `cargo wt ...` 调用时，cargo 会把 "wt" 作为第一个参数传入
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == "wt") {
        args.remove(1);
    }
    match Cli::parse_from(args).cmd {
        Cmd::Add { from, git_args } => add(from, git_args),
        Cmd::Seed { path, from } => {
            report(seed::seed(&path, from.as_deref())?);
            Ok(())
        }
        Cmd::InstallHook => {
            let path = hook::install(&std::env::current_dir()?)?;
            eprintln!("cargo-wt: hook 已安装在 {}", path.display());
            Ok(())
        }
        Cmd::Du { json } => {
            let usages = du::collect(&std::env::current_dir()?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&du::to_json(&usages))?);
            } else {
                du::print_table(&usages);
            }
            Ok(())
        }
        Cmd::Hook { kind: HookKind::PostCheckout { prev, .. } } => hook::post_checkout(&prev),
    }
}
