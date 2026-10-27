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

/// Let new git worktrees inherit Rust build artifacts via APFS clonefile on macOS
#[derive(Parser)]
#[command(name = "cargo wt", bin_name = "cargo wt", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run `git worktree add`, then inherit artifacts; remaining args go to git unchanged
    Add {
        /// Worktree to inherit from; defaults to the one that differs least
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        git_args: Vec<OsString>,
    },
    /// Inherit artifacts into an existing worktree
    Seed {
        path: PathBuf,
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Install a post-checkout hook so every new worktree of this repository inherits artifacts
    InstallHook,
    /// Show logical and private (unshared) artifact size per worktree
    Du {
        #[arg(long)]
        json: bool,
    },
    /// Called by the git hook
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
            "cargo-wt: inherited artifacts from {} ({} files aligned, {} differing files keep a fresh mtime)",
            o.source.display(),
            o.aligned,
            o.skipped
        ),
        None => eprintln!("cargo-wt: no worktree with artifacts to inherit from; skipped"),
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
        .context("failed to run git worktree add")?;
    if !status.success() {
        bail!("git worktree add failed");
    }
    let mut created: Vec<_> = git::worktrees(&cwd)?.into_iter().filter(|w| !before.contains(w)).collect();
    if created.len() != 1 {
        bail!("cannot tell which worktree was created ({} new)", created.len());
    }
    report(seed::seed(&created.remove(0), from.as_deref())?);
    Ok(())
}

fn main() -> Result<()> {
    // When invoked as `cargo wt ...`, cargo passes "wt" as the first argument
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
            eprintln!("cargo-wt: hook installed at {}", path.display());
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
